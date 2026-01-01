//! gRPC client for SP1 Cluster API with Redis artifact storage.

use crate::zkvm::Error;
use redis::AsyncCommands;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;
use tonic::transport::Channel;
use tracing::{debug, info};
use ulid::Ulid;

// Include the generated protobuf code
pub mod cluster_proto {
    tonic::include_proto!("cluster");
}

use cluster_proto::{
    cluster_service_client::ClusterServiceClient, ExecutionFailureCause, ExecutionStatus,
    ProofRequestCreateRequest, ProofRequestGetRequest, ProofRequestStatus,
};

/// Default timeout for proof generation (4 hours)
const DEFAULT_TIMEOUT_SECS: u64 = 4 * 60 * 60;

/// Polling interval configuration for exponential backoff
const POLL_INITIAL_INTERVAL_MS: u64 = 500;
const POLL_MAX_INTERVAL_MS: u64 = 10_000;
const POLL_BACKOFF_MULTIPLIER: f64 = 1.5;

/// SP1 Cluster client that uses gRPC API and Redis for artifact storage
pub struct SP1ClusterClient {
    grpc_endpoint: String,
    redis_url: String,
    runtime: tokio::runtime::Runtime,
}

impl SP1ClusterClient {
    /// Creates a new SP1 Cluster client
    pub fn new(
        grpc_endpoint: &str,
        redis_url: &str,
    ) -> Result<Self, Error> {
        if grpc_endpoint.is_empty() {
            return Err(Error::EndpointNotConfigured);
        }
        if redis_url.is_empty() {
            return Err(Error::RedisNotConfigured);
        }

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| Error::ClusterProve(format!("Failed to create tokio runtime: {}", e)))?;

        info!(
            "Created SP1 Cluster client: grpc={}, redis={}",
            grpc_endpoint, redis_url
        );

        Ok(Self {
            grpc_endpoint: grpc_endpoint.to_string(),
            redis_url: redis_url.to_string(),
            runtime,
        })
    }

    /// Synchronous wrapper for prove that uses the internal runtime
    pub fn prove_sync(
        &self,
        elf: &[u8],
        stdin: &[u8],
        mode: i32,
    ) -> Result<ProveResult, Error> {
        self.runtime.block_on(self.prove(elf, stdin, mode))
    }

    /// Connect to the gRPC service
    async fn connect_grpc(&self) -> Result<ClusterServiceClient<Channel>, Error> {
        ClusterServiceClient::connect(self.grpc_endpoint.clone())
            .await
            .map_err(|e| Error::GrpcConnect(e.to_string()))
    }

    /// Connect to Redis
    async fn connect_redis(&self) -> Result<redis::aio::MultiplexedConnection, Error> {
        let client =
            redis::Client::open(self.redis_url.as_str()).map_err(|e| Error::Redis(e.to_string()))?;
        client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| Error::Redis(e.to_string()))
    }

    /// Generate a unique artifact ID
    fn create_artifact_id(&self) -> String {
        format!("artifact_{}", Ulid::new().to_string().to_lowercase())
    }

    /// Upload an artifact to Redis with zstd compression
    async fn upload_artifact(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        artifact_id: &str,
        data: &[u8],
    ) -> Result<(), Error> {
        // Compress with zstd (level 0 for fast compression)
        let compressed = zstd::encode_all(data, 0)
            .map_err(|e| Error::Redis(format!("Failed to compress artifact: {}", e)))?;

        // Store with just the artifact_id as key (as SP1 Cluster expects)
        conn.set::<_, _, ()>(artifact_id, &compressed)
            .await
            .map_err(|e| Error::Redis(e.to_string()))?;

        debug!(
            "Uploaded artifact {} ({} bytes -> {} bytes compressed)",
            artifact_id,
            data.len(),
            compressed.len()
        );
        Ok(())
    }

    /// Download an artifact from Redis with zstd decompression
    /// Handles both simple keys and chunked storage (for large artifacts)
    async fn download_artifact(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        artifact_id: &str,
    ) -> Result<Vec<u8>, Error> {
        // Check if artifact is stored in chunks
        let chunks_key = format!("{}:chunks", artifact_id);
        let total_chunks: usize = conn
            .hlen(&chunks_key)
            .await
            .map_err(|e| Error::Redis(e.to_string()))?;

        let compressed: Vec<u8> = if total_chunks == 0 {
            // Simple key storage
            conn.get(artifact_id)
                .await
                .map_err(|e| Error::Redis(e.to_string()))?
        } else {
            // Chunked storage - download all chunks and combine
            let mut chunks: Vec<Vec<u8>> = Vec::with_capacity(total_chunks);
            for i in 0..total_chunks {
                let chunk: Vec<u8> = conn
                    .hget(&chunks_key, i)
                    .await
                    .map_err(|e| Error::Redis(format!("Failed to get chunk {}: {}", i, e)))?;
                chunks.push(chunk);
            }
            chunks.into_iter().flatten().collect()
        };

        // Decompress with zstd
        let data = zstd::decode_all(compressed.as_slice())
            .map_err(|e| Error::Redis(format!("Failed to decompress artifact: {}", e)))?;

        debug!(
            "Downloaded artifact {} ({} bytes compressed -> {} bytes, chunks: {})",
            artifact_id,
            compressed.len(),
            data.len(),
            total_chunks
        );
        Ok(data)
    }

    /// Delete artifacts from Redis to clean up after proving
    async fn cleanup_artifacts(
        &self,
        conn: &mut redis::aio::MultiplexedConnection,
        artifact_ids: &[&str],
    ) {
        for artifact_id in artifact_ids {
            // Try to delete both the simple key and chunks key
            let chunks_key = format!("{}:chunks", artifact_id);
            let _: Result<(), _> = conn.del::<_, ()>(*artifact_id).await;
            let _: Result<(), _> = conn.del::<_, ()>(&chunks_key).await;
        }
        debug!("Cleaned up {} artifacts from Redis", artifact_ids.len());
    }

    /// Submit a proof request and wait for completion
    pub async fn prove(
        &self,
        elf: &[u8],
        stdin: &[u8],
        mode: i32,
    ) -> Result<ProveResult, Error> {
        let mut grpc = self.connect_grpc().await?;
        let mut redis = self.connect_redis().await?;

        // Upload artifacts
        let program_id = self.create_artifact_id();
        let stdin_id = self.create_artifact_id();
        let proof_id = self.create_artifact_id();

        // Program needs to be bincode serialized (wrapping the ELF bytes)
        // Use bincode 1.x to match sp1-cluster's bincode version
        let program_serialized = bincode1::serialize(&elf.to_vec())
            .map_err(|e| Error::Redis(format!("Failed to serialize program: {}", e)))?;

        self.upload_artifact(&mut redis, &program_id, &program_serialized)
            .await?;

        // Stdin is uploaded as-is (already serialized by caller)
        self.upload_artifact(&mut redis, &stdin_id, stdin).await?;

        // Create proof request with validated ID
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let request_id = format!("ere_{}", timestamp);

        let deadline = SystemTime::now() + Duration::from_secs(DEFAULT_TIMEOUT_SECS);

        info!("Submitting proof request: {}", request_id);

        grpc.proof_request_create(ProofRequestCreateRequest {
            proof_id: request_id.clone(),
            program_artifact_id: program_id.clone(),
            stdin_artifact_id: stdin_id.clone(),
            options_artifact_id: Some(mode.to_string()),
            proof_artifact_id: Some(proof_id.clone()),
            requester: vec![],
            deadline: deadline.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            cycle_limit: 0,
            gas_limit: 0,
        })
        .await
        .map_err(|e| Error::GrpcRequest(e.to_string()))?;

        // Poll for completion with exponential backoff
        let start = std::time::Instant::now();
        let mut poll_interval_ms = POLL_INITIAL_INTERVAL_MS;
        loop {
            if SystemTime::now() > deadline {
                // Cleanup uploaded artifacts before returning error
                self.cleanup_artifacts(&mut redis, &[&program_id, &stdin_id])
                    .await;
                return Err(Error::ProveTimeout(DEFAULT_TIMEOUT_SECS));
            }

            let resp = grpc
                .proof_request_get(ProofRequestGetRequest {
                    proof_id: request_id.clone(),
                })
                .await
                .map_err(|e| Error::GrpcRequest(e.to_string()))?;

            if let Some(proof_request) = resp.into_inner().proof_request {
                let status =
                    ProofRequestStatus::try_from(proof_request.proof_status).unwrap_or_default();

                match status {
                    ProofRequestStatus::Completed => {
                        info!("Proof completed in {:?}", start.elapsed());

                        // Get the actual proof artifact ID from the response
                        let actual_proof_id = proof_request
                            .proof_artifact_id
                            .as_ref()
                            .ok_or_else(|| {
                                Error::ClusterProve("No proof artifact ID in response".to_string())
                            })?;

                        // Download proof
                        let proof_data =
                            self.download_artifact(&mut redis, actual_proof_id).await?;

                        // Cleanup uploaded artifacts (program, stdin)
                        // Note: We don't clean up the proof artifact as it might be needed by the cluster
                        self.cleanup_artifacts(&mut redis, &[&program_id, &stdin_id])
                            .await;

                        // Get execution result
                        let (cycles, public_values_hash) =
                            if let Some(exec) = proof_request.execution_result {
                                (exec.cycles, exec.public_values_hash)
                            } else {
                                (0, vec![])
                            };

                        return Ok(ProveResult {
                            proof: proof_data,
                            cycles,
                            public_values_hash,
                            proving_time: start.elapsed(),
                        });
                    }
                    ProofRequestStatus::Failed | ProofRequestStatus::Cancelled => {
                        // Use proto enum for status display
                        let status_str = status.as_str_name();

                        let elapsed = start.elapsed();

                        // Build error message
                        let mut error_msg = format!(
                            "Proof request {} (status={}) after {:?}",
                            request_id, status_str, elapsed
                        );

                        // Add execution details
                        if let Some(exec) = &proof_request.execution_result {
                            let exec_status = ExecutionStatus::try_from(exec.status)
                                .unwrap_or(ExecutionStatus::Unspecified);
                            let failure_cause = ExecutionFailureCause::try_from(exec.failure_cause)
                                .unwrap_or(ExecutionFailureCause::Unspecified);
                            error_msg.push_str(&format!(
                                " - Execution: status={}, failure_cause={}, cycles={}, gas={}",
                                exec_status.as_str_name(),
                                failure_cause.as_str_name(),
                                exec.cycles,
                                exec.gas
                            ));
                        } else {
                            error_msg.push_str(
                                " - Execution: no execution_result (execution may not have started)",
                            );
                        }

                        // Add metadata if available
                        if !proof_request.metadata.is_empty() {
                            error_msg.push_str(&format!(" - metadata: {}", proof_request.metadata));
                        }

                        // Cleanup uploaded artifacts before returning error
                        self.cleanup_artifacts(&mut redis, &[&program_id, &stdin_id])
                            .await;

                        return Err(Error::ClusterProve(error_msg));
                    }
                    _ => {}
                }
            }

            // Exponential backoff: increase interval up to max
            sleep(Duration::from_millis(poll_interval_ms)).await;
            poll_interval_ms = ((poll_interval_ms as f64 * POLL_BACKOFF_MULTIPLIER) as u64)
                .min(POLL_MAX_INTERVAL_MS);
        }
    }
}

/// Result from a prove operation
#[derive(Debug)]
pub struct ProveResult {
    pub proof: Vec<u8>,
    pub cycles: u64,
    pub public_values_hash: Vec<u8>,
    pub proving_time: Duration,
}

