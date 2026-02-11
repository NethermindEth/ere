use crate::zkvm::Error;
use crate::zkvm::cluster::{ProveResult as ClusterProveResult, SP1ClusterClient};
use ere_zkvm_interface::zkvm::{ClusterProverConfig, NetworkProverConfig, ProverResourceType};
use sp1_sdk::{
    CpuProver, CudaProver, NetworkProver, Prover as _, ProverClient, SP1ProofMode,
    SP1ProofWithPublicValues, SP1ProvingKey, SP1Stdin, SP1VerifyingKey,
};

#[allow(clippy::large_enum_variant)]
pub enum Prover {
    Cpu(CpuProver),
    Gpu(CudaProver),
    Network(NetworkProver),
    /// Cluster prover uses a separate SP1ClusterClient with a local CpuProver for verification
    Cluster {
        client: SP1ClusterClient,
        local_prover: CpuProver,
    },
}

impl Default for Prover {
    fn default() -> Self {
        Self::Cpu(ProverClient::builder().cpu().build())
    }
}

impl Prover {
    pub fn new(resource: &ProverResourceType) -> Result<Self, Error> {
        Ok(match resource {
            ProverResourceType::Cpu => Self::Cpu(ProverClient::builder().cpu().build()),
            ProverResourceType::Gpu => Self::Gpu(ProverClient::builder().cuda().build()),
            ProverResourceType::Network(config) => Self::Network(build_network_prover(config)?),
            ProverResourceType::Cluster(config) => {
                let client = build_cluster_client(config)?;
                Self::Cluster {
                    client,
                    local_prover: ProverClient::builder().cpu().build(),
                }
            }
        })
    }

    pub fn setup(&self, elf: &[u8]) -> (SP1ProvingKey, SP1VerifyingKey) {
        match self {
            Self::Cpu(cpu_prover) => cpu_prover.setup(elf),
            Self::Gpu(cuda_prover) => cuda_prover.setup(elf),
            Self::Network(network_prover) => network_prover.setup(elf),
            Self::Cluster { local_prover, .. } => local_prover.setup(elf),
        }
    }

    pub fn execute(
        &self,
        elf: &[u8],
        input: &SP1Stdin,
    ) -> Result<(sp1_sdk::SP1PublicValues, sp1_sdk::ExecutionReport), Error> {
        match self {
            Self::Cpu(cpu_prover) => cpu_prover.execute(elf, input).run(),
            Self::Gpu(cuda_prover) => cuda_prover.execute(elf, input).run(),
            Self::Network(network_prover) => network_prover.execute(elf, input).run(),
            Self::Cluster { local_prover, .. } => {
                // Execute locally - cluster is for proving only
                local_prover.execute(elf, input).run()
            }
        }
        .map_err(Error::Execute)
    }

    pub fn prove(
        &self,
        pk: &SP1ProvingKey,
        input: &SP1Stdin,
        mode: SP1ProofMode,
    ) -> Result<SP1ProofWithPublicValues, Error> {
        match self {
            Self::Cpu(cpu_prover) => cpu_prover
                .prove(pk, input)
                .mode(mode)
                .run()
                .map_err(Error::Prove),
            Self::Gpu(cuda_prover) => cuda_prover
                .prove(pk, input)
                .mode(mode)
                .run()
                .map_err(Error::Prove),
            Self::Network(network_prover) => network_prover
                .prove(pk, input)
                .mode(mode)
                .run()
                .map_err(Error::Prove),
            Self::Cluster { .. } => {
                // This method shouldn't be called for cluster - use prove_cluster instead
                Err(Error::ClusterProve(
                    "Use prove_cluster() for cluster proving".to_string(),
                ))
            }
        }
    }

    /// Prove using the cluster
    pub fn prove_cluster(
        &self,
        elf: &[u8],
        stdin_bytes: &[u8],
        mode: i32,
    ) -> Result<ClusterProveResult, Error> {
        match self {
            Self::Cluster { client, .. } => client.prove_sync(elf, stdin_bytes, mode),
            _ => Err(Error::ClusterProve(
                "prove_cluster is only available for Cluster prover".to_string(),
            )),
        }
    }

    /// Check if this is a cluster prover
    pub fn is_cluster(&self) -> bool {
        matches!(self, Self::Cluster { .. })
    }

    pub fn verify(
        &self,
        proof: &SP1ProofWithPublicValues,
        vk: &SP1VerifyingKey,
    ) -> Result<(), Error> {
        match self {
            Self::Cpu(cpu_prover) => cpu_prover.verify(proof, vk),
            Self::Gpu(cuda_prover) => cuda_prover.verify(proof, vk),
            Self::Network(network_prover) => network_prover.verify(proof, vk),
            Self::Cluster { local_prover, .. } => {
                // Verify locally
                local_prover.verify(proof, vk)
            }
        }
        .map_err(Error::Verify)
    }
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn cfg_or_envs_non_empty(config_value: &str, env_names: &[&str]) -> Option<String> {
    if !config_value.trim().is_empty() {
        return Some(config_value.to_string());
    }

    env_names.iter().find_map(|name| env_non_empty(name))
}

fn build_cluster_client(config: &ClusterProverConfig) -> Result<SP1ClusterClient, Error> {
    let endpoint = cfg_or_envs_non_empty(&config.endpoint, &["CLUSTER_ENDPOINT"])
        .ok_or(Error::EndpointNotConfigured)?;
    let redis_url = cfg_or_envs_non_empty(&config.redis_url, &["CLUSTER_REDIS_URL"])
        .ok_or(Error::RedisNotConfigured)?;
    SP1ClusterClient::new(&endpoint, &redis_url)
}

fn build_network_prover(config: &NetworkProverConfig) -> Result<NetworkProver, Error> {
    let mut builder = ProverClient::builder().network();
    let private_key = config
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
        .map(str::to_string)
        .or_else(|| env_non_empty("NETWORK_PRIVATE_KEY"))
        .ok_or(Error::NetworkPrivateKeyNotConfigured)?;
    builder = builder.private_key(&private_key);

    if let Some(rpc_url) = cfg_or_envs_non_empty(&config.endpoint, &["NETWORK_RPC_URL"]) {
        builder = builder.rpc_url(&rpc_url);
    }

    Ok(builder.build())
}
