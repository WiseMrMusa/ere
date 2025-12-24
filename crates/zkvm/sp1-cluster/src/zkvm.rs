use crate::program::SP1Program;
use anyhow::bail;
use bincode1;
use ere_zkvm_interface::zkvm::{
    ClusterProverConfig, CommonError, ProgramExecutionReport, ProgramProvingReport, Proof,
    ProofKind, PublicValues, zkVM, zkVMProgramDigest,
};
use sp1_sdk::{
    proof::ProofFromNetwork, CpuProver, Prover, SP1ProofMode, SP1ProofWithPublicValues, SP1Stdin,
    SP1VerifyingKey,
};
use std::env;
use tracing::info;

mod client;
mod error;

pub use client::SP1ClusterClient;
pub use error::Error;

include!(concat!(env!("OUT_DIR"), "/name_and_sdk_version.rs"));

/// Default Redis URL for local development
const DEFAULT_REDIS_URL: &str = "redis://:redispassword@127.0.0.1:6379/0";

/// SP1 Cluster zkVM implementation.
///
/// This zkVM connects to a self-hosted SP1 Cluster for distributed multi-GPU proving.
/// It uses the same program format as SP1 but offloads proving to the cluster.
pub struct EreSP1Cluster {
    program: SP1Program,
    config: ClusterProverConfig,
    /// Verifying key for local verification
    vk: SP1VerifyingKey,
    /// Local CPU prover for execution and verification
    local_prover: CpuProver,
}

impl EreSP1Cluster {
    /// Creates a new SP1 Cluster zkVM instance.
    ///
    /// # Arguments
    /// * `program` - The compiled SP1 program (ELF)
    /// * `config` - Cluster configuration (endpoint, api_key, num_gpus)
    ///
    /// # Environment Variables
    /// * `SP1_CLUSTER_ENDPOINT` - gRPC endpoint (e.g., http://localhost:50051)
    /// * `SP1_CLUSTER_REDIS_URL` - Redis URL for artifact storage
    /// * `SP1_CLUSTER_NUM_GPUS` - Number of GPUs to use
    pub fn new(program: SP1Program, config: ClusterProverConfig) -> Result<Self, Error> {
        // Get endpoint from config or environment
        let endpoint = if config.endpoint.is_empty() {
            env::var("SP1_CLUSTER_ENDPOINT").unwrap_or_default()
        } else {
            config.endpoint.clone()
        };

        if endpoint.is_empty() {
            return Err(Error::EndpointNotConfigured);
        }

        // Get num_gpus from config or environment
        let num_gpus = config.num_gpus.or_else(|| {
            env::var("SP1_CLUSTER_NUM_GPUS")
                .ok()
                .filter(|s| !s.is_empty())  // Filter out empty strings
                .and_then(|s| s.parse().ok())
        });

        info!(
            "Creating SP1 Cluster zkVM with endpoint: {}, num_gpus: {:?}",
            endpoint, num_gpus
        );

        // Create local prover for setup and verification
        let local_prover = sp1_sdk::ProverClient::builder().cpu().build();
        let (_, vk) = local_prover.setup(program.elf());

        // Get redis_url from config or environment
        let redis_url = config.redis_url.clone().or_else(|| {
            env::var("SP1_CLUSTER_REDIS_URL").ok()
        });

        Ok(Self {
            program,
            config: ClusterProverConfig {
                endpoint,
                api_key: config.api_key,
                num_gpus,
                redis_url,
            },
            vk,
            local_prover,
        })
    }

    /// Returns the cluster configuration
    pub fn config(&self) -> &ClusterProverConfig {
        &self.config
    }

    /// Returns the number of GPUs configured for proving
    pub fn num_gpus(&self) -> Option<u32> {
        self.config.num_gpus
    }

    /// Get the Redis URL from config or default
    fn redis_url(&self) -> String {
        self.config
            .redis_url
            .clone()
            .unwrap_or_else(|| DEFAULT_REDIS_URL.to_string())
    }

    /// Create the cluster client
    fn create_client(&self) -> Result<SP1ClusterClient, Error> {
        SP1ClusterClient::new(&self.config.endpoint, &self.redis_url(), self.config.num_gpus)
    }
}

impl zkVM for EreSP1Cluster {
    fn execute(&self, input: &[u8]) -> anyhow::Result<(PublicValues, ProgramExecutionReport)> {
        info!("Executing program locally (cluster is for proving only)...");

        // Execute locally using SP1 SDK
        let mut stdin = SP1Stdin::new();
        stdin.write_slice(input);

        let start = std::time::Instant::now();
        let (public_values, exec_report) = self
            .local_prover
            .execute(self.program.elf(), &stdin)
            .run()
            .map_err(|e| Error::Execute(e.to_string()))?;

        Ok((
            public_values.to_vec(),
            ProgramExecutionReport {
                total_num_cycles: exec_report.total_instruction_count(),
                region_cycles: exec_report.cycle_tracker.into_iter().collect(),
                execution_duration: start.elapsed(),
            },
        ))
    }

    fn prove(
        &self,
        input: &[u8],
        proof_kind: ProofKind,
    ) -> anyhow::Result<(PublicValues, Proof, ProgramProvingReport)> {
        info!(
            "Generating {:?} proof via SP1 Cluster (num_gpus: {:?})...",
            proof_kind,
            self.num_gpus()
        );

        // ProofMode values from sp1_sdk::network::proto::types::ProofMode
        // Core = 1, Compressed = 2, Plonk = 3, Groth16 = 4
        let mode = match proof_kind {
            ProofKind::Compressed => 2, // COMPRESSED mode
            ProofKind::Groth16 => 4,    // GROTH16 mode
        };

        // Serialize stdin in SP1 format using bincode 1.x (must match sp1-cluster's bincode version)
        let mut stdin = SP1Stdin::new();
        stdin.write_slice(input);
        let stdin_bytes = bincode1::serialize(&stdin)
            .map_err(|e| CommonError::serialize("stdin", "bincode1", e))?;

        // Create client and submit proof request
        let client = self.create_client()?;

        // Run async proof in blocking context
        let result = tokio::runtime::Runtime::new()
            .map_err(|e| Error::Prove(format!("Failed to create runtime: {}", e)))?
            .block_on(async {
                // #region agent log
                let log_path = "/root/sp1-cluster/.cursor/debug.log";
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                    use std::io::Write;
                    let _ = writeln!(file, "{{\"location\":\"zkvm.rs:175\",\"message\":\"Calling client.prove\",\"data\":{{\"program_elf_len\":{},\"stdin_bytes_len\":{},\"mode\":{}}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"B\"}}", self.program.elf().len(), stdin_bytes.len(), mode, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                }
                // #endregion
                client.prove(self.program.elf(), &stdin_bytes, mode).await
            });
        
        // #region agent log
        let log_path = "/root/sp1-cluster/.cursor/debug.log";
        match &result {
            Ok(_) => {
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                    use std::io::Write;
                    let _ = writeln!(file, "{{\"location\":\"zkvm.rs:175\",\"message\":\"client.prove succeeded\",\"data\":{{}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"B\"}}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                }
            }
            Err(e) => {
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                    use std::io::Write;
                    let error_str = format!("{:?}", e);
                    let _ = writeln!(file, "{{\"location\":\"zkvm.rs:175\",\"message\":\"client.prove failed\",\"data\":{{\"error\":\"{}\"}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"C\"}}", error_str.replace("\"", "\\\"").replace("\n", "\\n"), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                }
            }
        }
        // #endregion
        
        let result = result?;

        // Save raw proof for debugging if DEBUG_PROOF_PATH is set
        if let Ok(debug_path) = env::var("DEBUG_PROOF_PATH") {
            info!("Saving raw proof to {} ({} bytes)", debug_path, result.proof.len());
            if let Err(e) = std::fs::write(&debug_path, &result.proof) {
                info!("Failed to save debug proof: {}", e);
            }
        }

        // The proof from cluster is serialized ProofFromNetwork using bincode 1.x
        // ProofFromNetwork { proof: SP1Proof, public_values: SP1PublicValues, sp1_version: String }
        let proof_from_network: ProofFromNetwork = bincode1::deserialize(&result.proof)
            .map_err(|err| CommonError::deserialize("proof", "bincode", err))?;

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

    fn verify(&self, proof: &Proof) -> anyhow::Result<PublicValues> {
        info!("Verifying proof locally...");

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

        self.local_prover
            .verify(&proof, &self.vk)
            .map_err(Error::Verify)?;

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

impl zkVMProgramDigest for EreSP1Cluster {
    type ProgramDigest = SP1VerifyingKey;

    fn program_digest(&self) -> anyhow::Result<Self::ProgramDigest> {
        Ok(self.vk.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::RustRv32imaCustomized;
    use ere_test_utils::host::testing_guest_directory;
    use ere_zkvm_interface::compiler::Compiler;
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
    #[ignore = "Requires SP1_CLUSTER_ENDPOINT environment variable to be set"]
    fn test_prove_via_cluster() {
        use ere_test_utils::program::basic::BasicProgramInput;

        let config = ClusterProverConfig {
            num_gpus: Some(4),
            ..Default::default()
        };
        let program = basic_program();
        let zkvm = EreSP1Cluster::new(program, config).unwrap();

        let test_case = BasicProgramInput::valid();
        let (public_values, proof, report) = zkvm
            .prove(&test_case.serialized_input(), ProofKind::Compressed)
            .unwrap();

        assert!(!public_values.is_empty());
        assert!(!proof.as_bytes().is_empty());
        assert!(report.proving_time.as_millis() > 0);

        // Verify locally
        let verified_pv = zkvm.verify(&proof).unwrap();
        assert_eq!(public_values, verified_pv);
    }
}
