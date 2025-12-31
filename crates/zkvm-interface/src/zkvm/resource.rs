use serde::{Deserialize, Serialize};

/// Configuration for network-based proving
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
#[cfg_attr(feature = "clap", derive(clap::Args))]
pub struct ClusterProverConfig {
    #[cfg_attr(feature = "clap", arg(long, env = "SP1_CLUSTER_ENDPOINT", default_value = ""))]
    /// The gRPC endpoint URL of the cluster API service (e.g., http://localhost:50051)
    pub endpoint: String,

    #[cfg_attr(feature = "clap", arg(long, env = "SP1_CLUSTER_API_KEY"))]
    /// Optional API key for authentication
    pub api_key: Option<String>,

    #[cfg_attr(feature = "clap", arg(long, env = "SP1_CLUSTER_NUM_GPUS"))]
    /// Number of GPUs to use for proving. If not set, uses all available GPUs.
    pub num_gpus: Option<u32>,

    #[cfg_attr(feature = "clap", arg(long, env = "SP1_CLUSTER_REDIS_URL"))]
    /// Redis URL for artifact storage (e.g., redis://:password@localhost:6379/0)
    pub redis_url: Option<String>,
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
        if let Some(api_key) = &self.api_key {
            if !api_key.is_empty() {
                args.push("--api-key".to_string());
                args.push(api_key.clone());
            }
        }
        // Only add num_gpus if it's Some and > 0
        if let Some(num_gpus) = self.num_gpus {
            if num_gpus > 0 {
                args.push("--num-gpus".to_string());
                args.push(num_gpus.to_string());
            }
        }
        if let Some(redis_url) = &self.redis_url {
            if !redis_url.is_empty() {
                args.push("--redis-url".to_string());
                args.push(redis_url.clone());
            }
        }
        args
    }
}

/// ResourceType specifies what resource will be used to create the proofs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
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
                .chain(config.to_args().into_iter().map(|s| s.to_string()))
                .collect(),
            Self::Cluster(config) => core::iter::once("cluster".to_string())
                .chain(config.to_args())
                .collect(),
        }
    }
}
