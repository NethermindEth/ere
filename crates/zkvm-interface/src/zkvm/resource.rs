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
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if !self.endpoint.is_empty() {
            args.push("--endpoint".to_string());
            args.push(self.endpoint.clone());
        }
        if let Some(api_key) = &self.api_key {
            args.push("--api-key".to_string());
            args.push(api_key.clone());
        }
        args
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
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        // Only add endpoint if it's not empty
        if !self.endpoint.is_empty() {
            args.push("--endpoint".to_string());
            args.push(self.endpoint.clone());
        }
        if !self.redis_url.is_empty() {
            args.push("--redis-url".to_string());
            args.push(self.redis_url.clone());
        }
        args
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
    pub fn to_args(&self) -> Vec<String> {
        match self {
            Self::Cpu => vec!["cpu".to_string()],
            Self::Gpu => vec!["gpu".to_string()],
            Self::Network(config) => core::iter::once("network".to_string())
                .chain(config.to_args())
                .collect(),
            Self::Cluster(config) => core::iter::once("cluster".to_string())
                .chain(config.to_args())
                .collect(),
        }
    }
}
