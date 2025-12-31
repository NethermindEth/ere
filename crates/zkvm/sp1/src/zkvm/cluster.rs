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
    cluster_service_client::ClusterServiceClient, ProofRequestCreateRequest,
    ProofRequestGetRequest, ProofRequestStatus,
};

/// Default timeout for proof generation (4 hours)
const DEFAULT_TIMEOUT_SECS: u64 = 4 * 60 * 60;

/// Default Redis URL for local development
pub const DEFAULT_REDIS_URL: &str = "redis://:redispassword@127.0.0.1:6379/0";

/// SP1 Cluster client that uses gRPC API and Redis for artifact storage
pub struct SP1ClusterClient {
    grpc_endpoint: String,
    redis_url: String,
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

        info!(
            "Created SP1 Cluster client: grpc={}, redis={}",
            grpc_endpoint, redis_url
        );

        Ok(Self {
            grpc_endpoint: grpc_endpoint.to_string(),
            redis_url: redis_url.to_string(),
        })
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
            program_artifact_id: program_id,
            stdin_artifact_id: stdin_id,
            options_artifact_id: Some(mode.to_string()),
            proof_artifact_id: Some(proof_id.clone()),
            requester: vec![],
            deadline: deadline.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            cycle_limit: 0,
            gas_limit: 0,
        })
        .await
        .map_err(|e| Error::GrpcRequest(e.to_string()))?;

        // Poll for completion
        let start = std::time::Instant::now();
        loop {
            if SystemTime::now() > deadline {
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
                    ProofRequestStatus::ProofRequestStatusCompleted => {
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
                    ProofRequestStatus::ProofRequestStatusFailed
                    | ProofRequestStatus::ProofRequestStatusCancelled => {
                        // Build status string
                        let status_str = match proof_request.proof_status {
                            0 => "UNSPECIFIED".to_string(),
                            1 => "PENDING".to_string(),
                            2 => "COMPLETED".to_string(),
                            3 => "FAILED".to_string(),
                            4 => "CANCELLED".to_string(),
                            _ => format!("UNKNOWN({})", proof_request.proof_status),
                        };

                        let elapsed = start.elapsed();

                        // Build error message
                        let mut error_msg = format!(
                            "Proof request {} (status={}) after {:?}",
                            request_id, status_str, elapsed
                        );

                        // Add execution details
                        if let Some(exec) = &proof_request.execution_result {
                            let exec_status_str = match exec.status {
                                0 => "UNSPECIFIED",
                                1 => "UNEXECUTED",
                                2 => "EXECUTED",
                                3 => "FAILED",
                                4 => "CANCELLED",
                                _ => "UNKNOWN",
                            };
                            let failure_cause_str = match exec.failure_cause {
                                0 => "UNSPECIFIED",
                                1 => "HALT_WITH_NON_ZERO_EXIT_CODE",
                                2 => "INVALID_MEMORY_ACCESS",
                                3 => "UNSUPPORTED_SYSCALL",
                                4 => "BREAKPOINT",
                                5 => "EXCEEDED_CYCLE_LIMIT",
                                6 => "INVALID_SYSCALL_USAGE",
                                7 => "UNIMPLEMENTED",
                                8 => "END_IN_UNCONSTRAINED",
                                _ => "UNKNOWN",
                            };
                            error_msg.push_str(&format!(
                                " - Execution: status={}, failure_cause={}, cycles={}, gas={}",
                                exec_status_str, failure_cause_str, exec.cycles, exec.gas
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

                        return Err(Error::ClusterProve(error_msg));
                    }
                    _ => {}
                }
            }

            sleep(Duration::from_millis(500)).await;
        }
    }

    /// Returns the gRPC endpoint
    pub fn endpoint(&self) -> &str {
        &self.grpc_endpoint
    }

    /// Returns the Redis URL
    pub fn redis_url(&self) -> &str {
        &self.redis_url
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

