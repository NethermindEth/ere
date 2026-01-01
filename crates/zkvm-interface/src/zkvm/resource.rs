use serde::{Deserialize, Serialize};

/// Configuration for network-based proving
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct NetworkProverConfig {
    #[cfg_attr(feature = "clap", arg(long))]
    /// The endpoint URL of the prover network service
    pub endpoint: String,

    #[cfg_attr(feature = "clap", arg(long))]
    /// Optional API key for authentication
    pub api_key: Option<String>,
}

#[cfg(feature = "clap")]
impl NetworkProverConfig {   
    pub fn to_args(&self) -> Vec<&str> {
        core::iter::once(["--endpoint", self.endpoint.as_str()])
            .chain(self.api_key.as_deref().map(|val| ["--api-key", val]))
            .flatten()
            .collect()
    }
}

/// Configuration for cluster-based proving (e.g., SP1 Cluster)
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct ClusterProverConfig {
    #[cfg_attr(feature = "clap", arg(long, env = "SP1_CLUSTER_ENDPOINT", default_value = "http://127.0.0.1:50051/"))]
    /// The gRPC endpoint URL of the cluster API service (e.g., http://localhost:50051)
    pub endpoint: String,

    #[cfg_attr(feature = "clap", arg(long, env = "SP1_CLUSTER_REDIS_URL", default_value = "redis://:redispassword@127.0.0.1:6379/0"))]
    /// Redis URL for artifact storage (e.g., redis://:password@localhost:6379/0)
    pub redis_url: String,
}

#[cfg(feature = "clap")]
impl ClusterProverConfig {
    pub fn to_args(&self) -> Vec<&str> {
        core::iter::once(["--endpoint", self.endpoint.as_str()])
            .chain(core::iter::once(["--redis-url", self.redis_url.as_str()]))
            .flatten()
            .collect()
    }
}

/// ResourceType specifies what resource will be used to create the proofs.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "clap", derive(clap::Subcommand))]
pub enum ProverResourceType {
    #[default]
    Cpu,
    Gpu,
    /// Use a remote prover network
    Network(NetworkProverConfig),
    /// Use a multi-GPU cluster (e.g., SP1 Cluster)
    Cluster(ClusterProverConfig),
}

#[cfg(feature = "clap")]
impl ProverResourceType {
    pub fn to_args(&self) -> Vec<&str> {
        match self {
            Self::Cpu => vec!["cpu"],
            Self::Gpu => vec!["gpu"],
            Self::Network(config) => core::iter::once("network").chain(config.to_args()).collect(),
            Self::Cluster(config) => core::iter::once("cluster").chain(config.to_args()).collect(),
        }
    }
}
