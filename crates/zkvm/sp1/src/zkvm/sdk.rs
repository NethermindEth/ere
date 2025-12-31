use crate::zkvm::Error;
use crate::zkvm::cluster::{ProveResult as ClusterProveResult, SP1ClusterClient};
use ere_zkvm_interface::zkvm::{NetworkProverConfig, ProverResourceType};
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

impl Prover {
    pub fn new(resource: &ProverResourceType) -> Result<Self, Error> {
        Ok(match resource {
            ProverResourceType::Cpu => Self::Cpu(ProverClient::builder().cpu().build()),
            ProverResourceType::Gpu => Self::Gpu(ProverClient::builder().cuda().build()),
            ProverResourceType::Network(config) => Self::Network(build_network_prover(config)?),
            ProverResourceType::Cluster(config) => {
                let client = SP1ClusterClient::new(&config.endpoint, &config.redis_url)?;
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
            Self::Cpu(cpu_prover) => cpu_prover.prove(pk, input).mode(mode).run().map_err(Error::Prove),
            Self::Gpu(cuda_prover) => cuda_prover.prove(pk, input).mode(mode).run().map_err(Error::Prove),
            Self::Network(network_prover) => network_prover.prove(pk, input).mode(mode).run().map_err(Error::Prove),
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

fn build_network_prover(config: &NetworkProverConfig) -> Result<NetworkProver, Error> {
    let mut builder = ProverClient::builder().network();
    // Check if we have a private key in the config or environment
    if let Some(api_key) = &config.api_key {
        builder = builder.private_key(api_key);
    } else if let Ok(private_key) = std::env::var("NETWORK_PRIVATE_KEY") {
        builder = builder.private_key(&private_key);
    } else {
        return Err(Error::NetworkPrivateKeyNotConfigured);
    }
    // Set the RPC URL if provided
    if !config.endpoint.is_empty() {
        builder = builder.rpc_url(&config.endpoint);
    } else if let Ok(rpc_url) = std::env::var("NETWORK_RPC_URL") {
        builder = builder.rpc_url(&rpc_url);
    }
    // Otherwise SP1 SDK will use its default RPC URL
    Ok(builder.build())
}
