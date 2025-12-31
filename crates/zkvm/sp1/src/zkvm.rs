use crate::{program::SP1Program, zkvm::sdk::Prover};
use anyhow::bail;
use ere_zkvm_interface::zkvm::{
    CommonError, Input, ProgramExecutionReport, ProgramProvingReport, Proof, ProofKind,
    ProverResourceType, PublicValues, zkVM, zkVMProgramDigest,
};
use sp1_sdk::{SP1ProofMode, SP1ProofWithPublicValues, SP1ProvingKey, SP1Stdin, SP1VerifyingKey};
use std::{
    mem::take,
    panic,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
    time::Instant,
};
use tracing::info;

#[cfg(feature = "cluster")]
pub mod cluster;
mod error;
mod sdk;

pub use error::Error;
#[cfg(feature = "cluster")]
pub use cluster::SP1ClusterClient;

include!(concat!(env!("OUT_DIR"), "/name_and_sdk_version.rs"));

pub struct EreSP1 {
    program: SP1Program,
    /// Prover resource configuration for creating clients
    resource: ProverResourceType,
    /// Proving key
    pk: SP1ProvingKey,
    /// Verification key
    vk: SP1VerifyingKey,
    // The current version of SP1 (v5.2.3) has a problem where if GPU proving
    // the program crashes in the Moongate container, it leaves an internal
    // mutex poisoned, which prevents further proving attempts.
    // This is a workaround to avoid the poisoned mutex issue by creating a new
    // prover if the proving panics.
    // Eventually, this should be fixed in the SP1 SDK.
    // For more context see: https://github.com/eth-act/zkevm-benchmark-workload/issues/54
    prover: RwLock<Prover>,
}

impl EreSP1 {
    pub fn new(program: SP1Program, resource: ProverResourceType) -> Result<Self, Error> {
        let prover = Prover::new(&resource);
        let (pk, vk) = prover.setup(&program.elf);
        Ok(Self {
            program,
            resource,
            pk,
            vk,
            prover: RwLock::new(prover),
        })
    }

    fn prover(&'_ self) -> Result<RwLockReadGuard<'_, Prover>, Error> {
        self.prover.read().map_err(|_| Error::RwLockPosioned)
    }

    fn prover_mut(&'_ self) -> Result<RwLockWriteGuard<'_, Prover>, Error> {
        self.prover.write().map_err(|_| Error::RwLockPosioned)
    }

    /// Prove via the cluster (only available with cluster feature)
    #[cfg(feature = "cluster")]
    fn prove_via_cluster(
        &self,
        input: &Input,
        proof_kind: ProofKind,
    ) -> anyhow::Result<(PublicValues, Proof, ProgramProvingReport)> {
        use sp1_sdk::proof::ProofFromNetwork;

        info!(
            "Generating {:?} proof via SP1 Cluster...",
            proof_kind,
        );

        // ProofMode values from sp1_sdk::network::proto::types::ProofMode
        // Core = 1, Compressed = 2, Plonk = 3, Groth16 = 4
        let mode = match proof_kind {
            ProofKind::Compressed => 2, // COMPRESSED mode
            ProofKind::Groth16 => 4,    // GROTH16 mode
        };

        // Serialize stdin in SP1 format using bincode 1.x (must match sp1-cluster's bincode version)
        let mut stdin = SP1Stdin::new();
        stdin.write_slice(input.stdin());
        let stdin_bytes = bincode1::serialize(&stdin)
            .map_err(|e| CommonError::serialize("stdin", "bincode1", e))?;

        // Use the prover's cluster proving method
        let prover = self.prover()?;
        let result = prover.prove_cluster(self.program.elf(), &stdin_bytes, mode)?;

        // The proof from cluster is serialized ProofFromNetwork using bincode 1.x
        let proof_from_network: ProofFromNetwork = bincode1::deserialize(&result.proof)
            .map_err(|err| CommonError::deserialize("proof", "bincode1", err))?;

        info!(
            "Received proof from cluster: sp1_version={}, proof_type={:?}",
            proof_from_network.sp1_version,
            SP1ProofMode::from(&proof_from_network.proof)
        );

        let public_values = proof_from_network.public_values.as_slice().to_vec();

        // Re-serialize as SP1ProofWithPublicValues for storage (using bincode 2.x for ere compatibility)
        let sp1_proof = SP1ProofWithPublicValues {
            proof: proof_from_network.proof,
            public_values: proof_from_network.public_values,
            sp1_version: proof_from_network.sp1_version,
            tee_proof: None,
        };
        let proof_bytes = bincode::serde::encode_to_vec(&sp1_proof, bincode::config::legacy())
            .map_err(|e| CommonError::serialize("proof", "bincode", e))?;
        let proof = Proof::new(proof_kind, proof_bytes);

        Ok((
            public_values,
            proof,
            ProgramProvingReport::new(result.proving_time),
        ))
    }
}

impl zkVM for EreSP1 {
    fn execute(&self, input: &Input) -> anyhow::Result<(PublicValues, ProgramExecutionReport)> {
        let stdin = input_to_stdin(input)?;

        let prover = self.prover()?;

        let start = Instant::now();
        let (public_values, exec_report) = prover.execute(self.program.elf(), &stdin)?;
        let execution_duration = start.elapsed();

        Ok((
            public_values.to_vec(),
            ProgramExecutionReport {
                total_num_cycles: exec_report.total_instruction_count(),
                region_cycles: exec_report.cycle_tracker.into_iter().collect(),
                execution_duration,
            },
        ))
    }

    fn prove(
        &self,
        input: &Input,
        proof_kind: ProofKind,
    ) -> anyhow::Result<(PublicValues, Proof, ProgramProvingReport)> {
        info!("Generating proof…");

        // Handle cluster proving separately
        #[cfg(feature = "cluster")]
        if self.prover()?.is_cluster() {
            return self.prove_via_cluster(input, proof_kind);
        }

        let stdin = input_to_stdin(input)?;

        let mode = match proof_kind {
            ProofKind::Compressed => SP1ProofMode::Compressed,
            ProofKind::Groth16 => SP1ProofMode::Groth16,
        };

        let mut prover = self.prover_mut()?;

        let start = Instant::now();
        let proof =
            panic::catch_unwind(|| prover.prove(&self.pk, &stdin, mode)).map_err(|err| {
                if matches!(self.resource, ProverResourceType::Gpu) {
                    // Drop the panicked prover and create a new one.
                    // Note that `take` has to be done explicitly first so the
                    // Moongate container could be removed properly.
                    take(&mut *prover);
                    *prover = Prover::new(&self.resource);
                }

                Error::Panic(panic_msg(err))
            })??;
        let proving_time = start.elapsed();

        let public_values = proof.public_values.to_vec();
        let proof = Proof::new(
            proof_kind,
            bincode::serde::encode_to_vec(&proof, bincode::config::legacy())
                .map_err(|err| CommonError::serialize("proof", "bincode", err))?,
        );

        Ok((
            public_values,
            proof,
            ProgramProvingReport::new(proving_time),
        ))
    }

    fn verify(&self, proof: &Proof) -> anyhow::Result<PublicValues> {
        info!("Verifying proof…");

        let proof_kind = proof.kind();

        let (proof, _): (SP1ProofWithPublicValues, _) =
            bincode::serde::decode_from_slice(proof.as_bytes(), bincode::config::legacy())
                .map_err(|err| CommonError::deserialize("proof", "bincode", err))?;
        let inner_proof_kind = SP1ProofMode::from(&proof.proof);

        if !matches!(
            (proof_kind, inner_proof_kind),
            (ProofKind::Compressed, SP1ProofMode::Compressed)
                | (ProofKind::Groth16, SP1ProofMode::Groth16)
        ) {
            bail!(Error::InvalidProofKind(proof_kind, inner_proof_kind));
        }

        self.prover()?.verify(&proof, &self.vk)?;

        let public_values_bytes = proof.public_values.as_slice().to_vec();

        Ok(public_values_bytes)
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn sdk_version(&self) -> &'static str {
        SDK_VERSION
    }
}

impl zkVMProgramDigest for EreSP1 {
    type ProgramDigest = SP1VerifyingKey;

    fn program_digest(&self) -> anyhow::Result<Self::ProgramDigest> {
        Ok(self.vk.clone())
    }
}

fn input_to_stdin(input: &Input) -> Result<SP1Stdin, Error> {
    let mut stdin = SP1Stdin::new();
    stdin.write_slice(input.stdin());
    if let Some(proofs) = input.proofs() {
        for (proof, vk) in proofs.map_err(Error::DeserializeInputProofs)? {
            stdin.write_proof(proof, vk);
        }
    }
    Ok(stdin)
}

fn panic_msg(err: Box<dyn std::any::Any + Send + 'static>) -> String {
    None.or_else(|| err.downcast_ref::<String>().cloned())
        .or_else(|| err.downcast_ref::<&'static str>().map(ToString::to_string))
        .unwrap_or_else(|| "unknown panic msg".to_string())
}

#[cfg(test)]
mod tests {
    use crate::{compiler::RustRv32imaCustomized, program::SP1Program, zkvm::EreSP1};
    use ere_test_utils::{
        host::{TestCase, run_zkvm_execute, run_zkvm_prove, testing_guest_directory},
        io::serde::bincode::BincodeLegacy,
        program::basic::BasicProgram,
    };
    use ere_zkvm_interface::{
        Input,
        compiler::Compiler,
        zkvm::{NetworkProverConfig, ProofKind, ProverResourceType, zkVM},
    };
    use std::sync::OnceLock;

    fn basic_program() -> SP1Program {
        static PROGRAM: OnceLock<SP1Program> = OnceLock::new();
        PROGRAM
            .get_or_init(|| {
                RustRv32imaCustomized
                    .compile(&testing_guest_directory("sp1", "basic"))
                    .unwrap()
            })
            .clone()
    }

    #[test]
    fn test_execute() {
        let program = basic_program();
        let zkvm = EreSP1::new(program, ProverResourceType::Cpu).unwrap();

        let test_case = BasicProgram::<BincodeLegacy>::valid_test_case();
        run_zkvm_execute(&zkvm, &test_case);
    }

    #[test]
    fn test_execute_invalid_test_case() {
        let program = basic_program();
        let zkvm = EreSP1::new(program, ProverResourceType::Cpu).unwrap();

        for input in [
            Input::new(),
            BasicProgram::<BincodeLegacy>::invalid_test_case().input(),
        ] {
            zkvm.execute(&input).unwrap_err();
        }
    }

    #[test]
    fn test_prove() {
        let program = basic_program();
        let zkvm = EreSP1::new(program, ProverResourceType::Cpu).unwrap();

        let test_case = BasicProgram::<BincodeLegacy>::valid_test_case();
        run_zkvm_prove(&zkvm, &test_case);
    }

    #[test]
    fn test_prove_invalid_test_case() {
        let program = basic_program();
        let zkvm = EreSP1::new(program, ProverResourceType::Cpu).unwrap();

        for input in [
            Input::new(),
            BasicProgram::<BincodeLegacy>::invalid_test_case().input(),
        ] {
            zkvm.prove(&input, ProofKind::default()).unwrap_err();
        }
    }

    #[test]
    #[ignore = "Requires NETWORK_PRIVATE_KEY environment variable to be set"]
    fn test_prove_sp1_network() {
        // Check if we have the required environment variable
        if std::env::var("NETWORK_PRIVATE_KEY").is_err() {
            eprintln!("Skipping network test: NETWORK_PRIVATE_KEY not set");
            return;
        }

        // Create a network prover configuration
        let network_config = NetworkProverConfig {
            endpoint: std::env::var("NETWORK_RPC_URL").unwrap_or_default(),
            api_key: std::env::var("NETWORK_PRIVATE_KEY").ok(),
        };
        let program = basic_program();
        let zkvm = EreSP1::new(program, ProverResourceType::Network(network_config)).unwrap();

        let test_case = BasicProgram::<BincodeLegacy>::valid_test_case();
        run_zkvm_prove(&zkvm, &test_case);
    }

    #[cfg(feature = "cluster")]
    #[test]
    #[ignore = "Requires SP1_CLUSTER_ENDPOINT environment variable to be set"]
    fn test_prove_sp1_cluster() {
        use ere_zkvm_interface::zkvm::ClusterProverConfig;

        // Check if we have the required environment variable
        if std::env::var("SP1_CLUSTER_ENDPOINT").is_err() {
            eprintln!("Skipping cluster test: SP1_CLUSTER_ENDPOINT not set");
            return;
        }

        // Create a cluster prover configuration
        let cluster_config = ClusterProverConfig::default();
        let program = basic_program();
        let zkvm = EreSP1::new(program, ProverResourceType::Cluster(cluster_config)).unwrap();

        let test_case = BasicProgram::<BincodeLegacy>::valid_test_case();
        run_zkvm_prove(&zkvm, &test_case);
    }
}
