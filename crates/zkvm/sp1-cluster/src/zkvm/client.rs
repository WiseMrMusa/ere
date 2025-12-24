//! gRPC client for SP1 Cluster API with Redis artifact storage.

use crate::zkvm::Error;
use bincode1;
use redis::AsyncCommands;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;
use tonic::transport::Channel;
use tracing::{debug, info};
use ulid::Ulid;

// Include the generated protobuf code
pub mod cluster {
    tonic::include_proto!("cluster");
}

use cluster::{
    cluster_service_client::ClusterServiceClient, ProofRequestCreateRequest,
    ProofRequestGetRequest, ProofRequestStatus,
};

/// Default timeout for proof generation (4 hours)
const DEFAULT_TIMEOUT_SECS: u64 = 4 * 60 * 60;

/// SP1 Cluster client that uses gRPC API and Redis for artifact storage
pub struct SP1ClusterClient {
    grpc_endpoint: String,
    redis_url: String,
    num_gpus: Option<u32>,
}

impl SP1ClusterClient {
    /// Creates a new SP1 Cluster client
    pub fn new(
        grpc_endpoint: &str,
        redis_url: &str,
        num_gpus: Option<u32>,
    ) -> Result<Self, Error> {
        if grpc_endpoint.is_empty() {
            return Err(Error::EndpointNotConfigured);
        }
        if redis_url.is_empty() {
            return Err(Error::RedisNotConfigured);
        }

        info!(
            "Created SP1 Cluster client: grpc={}, redis={}, num_gpus={:?}",
            grpc_endpoint, redis_url, num_gpus
        );

        Ok(Self {
            grpc_endpoint: grpc_endpoint.to_string(),
            redis_url: redis_url.to_string(),
            num_gpus,
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
        
        // #region agent log
        let log_path = "/root/sp1-cluster/.cursor/debug.log";
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
            use std::io::Write;
            let _ = writeln!(file, "{{\"location\":\"client.rs:169\",\"message\":\"Uploading program artifact\",\"data\":{{\"program_id\":\"{}\",\"elf_size\":{},\"serialized_size\":{}}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"A\"}}", program_id, elf.len(), program_serialized.len(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
        }
        // #endregion
        
        self.upload_artifact(&mut redis, &program_id, &program_serialized)
            .await?;

        // #region agent log
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
            use std::io::Write;
            let _ = writeln!(file, "{{\"location\":\"client.rs:173\",\"message\":\"Uploading stdin artifact\",\"data\":{{\"stdin_id\":\"{}\",\"stdin_size\":{}}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"A\"}}", stdin_id, stdin.len(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
        }
        // #endregion

        // Stdin is uploaded as-is (already serialized by caller)
        self.upload_artifact(&mut redis, &stdin_id, stdin).await?;

        // Create proof request with validated ID
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let request_id = format!("ere_{}", timestamp);
        
        // Validate request_id was generated correctly
        if !request_id.starts_with("ere_") || request_id.len() < 10 {
            return Err(Error::Prove(format!(
                "Failed to generate valid request_id: {} (timestamp: {})",
                request_id, timestamp
            )));
        }
        
        let deadline = SystemTime::now() + Duration::from_secs(DEFAULT_TIMEOUT_SECS);

        info!("Submitting proof request: {} (timestamp: {})", request_id, timestamp);

        // #region agent log
        let log_path = "/root/sp1-cluster/.cursor/debug.log";
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
            use std::io::Write;
            let _ = writeln!(file, "{{\"location\":\"client.rs:187\",\"message\":\"Creating proof request\",\"data\":{{\"request_id\":\"{}\",\"program_id\":\"{}\",\"stdin_id\":\"{}\",\"proof_id\":\"{}\",\"mode\":{},\"deadline\":{}}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"B\"}}", request_id, program_id, stdin_id, proof_id, mode, deadline.duration_since(UNIX_EPOCH).unwrap().as_secs(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
        }
        // #endregion

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
        .map_err(|e| {
            // #region agent log
            let log_path = "/root/sp1-cluster/.cursor/debug.log";
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                use std::io::Write;
                let _ = writeln!(file, "{{\"location\":\"client.rs:199\",\"message\":\"Proof request create failed\",\"data\":{{\"request_id\":\"{}\",\"error\":\"{}\"}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"C\"}}", request_id, e, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
            }
            // #endregion
            Error::GrpcRequest(e.to_string())
        })?;

        // #region agent log
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
            use std::io::Write;
            let _ = writeln!(file, "{{\"location\":\"client.rs:200\",\"message\":\"Proof request created successfully\",\"data\":{{\"request_id\":\"{}\"}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"B\"}}", request_id, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
        }
        // #endregion

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
                let status = ProofRequestStatus::try_from(proof_request.proof_status).unwrap_or_default();
                
                // #region agent log
                let log_path = "/root/sp1-cluster/.cursor/debug.log";
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                    use std::io::Write;
                    let exec_info = if let Some(exec) = &proof_request.execution_result {
                        format!("{{\"status\":{},\"failure_cause\":{},\"cycles\":{},\"gas\":{},\"public_values_hash_len\":{}}}", exec.status, exec.failure_cause, exec.cycles, exec.gas, exec.public_values_hash.len())
                    } else {
                        "\"none\"".to_string()
                    };
                    let _ = writeln!(file, "{{\"location\":\"client.rs:262\",\"message\":\"Polling proof request status\",\"data\":{{\"request_id\":\"{}\",\"proof_request_id\":\"{}\",\"proof_status\":{},\"elapsed_ms\":{},\"execution_result\":{},\"metadata\":\"{}\"}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"D\"}}", request_id, proof_request.id, proof_request.proof_status, start.elapsed().as_millis(), exec_info, proof_request.metadata, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                }
                // #endregion
                
                match status {
                    ProofRequestStatus::Completed => {
                        info!("Proof completed in {:?}", start.elapsed());

                        // Get the actual proof artifact ID from the response
                        let actual_proof_id = proof_request
                            .proof_artifact_id
                            .as_ref()
                            .ok_or_else(|| {
                                Error::Prove("No proof artifact ID in response".to_string())
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
                    ProofRequestStatus::Failed | ProofRequestStatus::Cancelled => {
                        // Log detailed failure information for debugging
                        let exec_info = if let Some(exec) = &proof_request.execution_result {
                            let exec_status_str = match exec.status {
                                0 => "UNSPECIFIED".to_string(),
                                1 => "UNEXECUTED".to_string(),
                                2 => "EXECUTED".to_string(),
                                3 => "FAILED".to_string(),
                                4 => "CANCELLED".to_string(),
                                _ => format!("UNKNOWN({})", exec.status),
                            };
                            let failure_cause_str = match exec.failure_cause {
                                0 => "UNSPECIFIED".to_string(),
                                1 => "HALT_WITH_NON_ZERO_EXIT_CODE".to_string(),
                                2 => "INVALID_MEMORY_ACCESS".to_string(),
                                3 => "UNSUPPORTED_SYSCALL".to_string(),
                                4 => "BREAKPOINT".to_string(),
                                5 => "EXCEEDED_CYCLE_LIMIT".to_string(),
                                6 => "INVALID_SYSCALL_USAGE".to_string(),
                                7 => "UNIMPLEMENTED".to_string(),
                                8 => "END_IN_UNCONSTRAINED".to_string(),
                                _ => format!("UNKNOWN({})", exec.failure_cause),
                            };
                            format!("status={} ({}), failure_cause={} ({}), cycles={}, gas={}, public_values_hash_len={}", exec.status, exec_status_str, exec.failure_cause, failure_cause_str, exec.cycles, exec.gas, exec.public_values_hash.len())
                        } else {
                            "no execution_result".to_string()
                        };
                        
                        eprintln!("[CLUSTER-ERROR] Proof request {} (id: {}) failed: proof_status={}, elapsed={:?}, execution_result: {}", request_id, proof_request.id, proof_request.proof_status, start.elapsed(), exec_info);
                        eprintln!("[CLUSTER-ERROR] Metadata: {}", proof_request.metadata);
                        eprintln!("[CLUSTER-ERROR] Proof artifact ID: {:?}", proof_request.proof_artifact_id);
                        
                        // #region agent log
                        let log_path = "/root/sp1-cluster/.cursor/debug.log";
                        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                            use std::io::Write;
                            let exec_info_json = if let Some(exec) = &proof_request.execution_result {
                                let exec_status_str = match exec.status {
                                    0 => "UNSPECIFIED",
                                    1 => "UNEXECUTED",
                                    2 => "EXECUTED",
                                    3 => "FAILED",
                                    4 => "CANCELLED",
                                    _ => &format!("UNKNOWN({})", exec.status),
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
                                    _ => &format!("UNKNOWN({})", exec.failure_cause),
                                };
                                format!("{{\"status\":{},\"status_str\":\"{}\",\"failure_cause\":{},\"failure_cause_str\":\"{}\",\"cycles\":{},\"gas\":{},\"public_values_hash_len\":{}}}", exec.status, exec_status_str, exec.failure_cause, failure_cause_str, exec.cycles, exec.gas, exec.public_values_hash.len())
                            } else {
                                "\"none\"".to_string()
                            };
                            let _ = writeln!(file, "{{\"location\":\"client.rs:309\",\"message\":\"Proof request failed - detailed\",\"data\":{{\"request_id\":\"{}\",\"proof_request_id\":\"{}\",\"proof_status\":{},\"elapsed_ms\":{},\"execution_result\":{},\"metadata\":\"{}\",\"proof_artifact_id\":\"{}\"}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"E\"}}", request_id, proof_request.id, proof_request.proof_status, start.elapsed().as_millis(), exec_info_json, proof_request.metadata, proof_request.proof_artifact_id.as_ref().unwrap_or(&"none".to_string()), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                        }
                        // #endregion
                        
                        // Validate request_id - it should start with "ere_" and be at least 10 characters
                        let request_id_valid = request_id.starts_with("ere_") && request_id.len() >= 10;
                        let proof_request_id_valid = !proof_request.id.is_empty() && proof_request.id.len() >= 5;
                        
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
                        
                        // Build the request ID display with validation
                        let request_id_display = if !request_id_valid {
                            // Invalid request_id - use proof_request.id if available, otherwise flag it
                            if proof_request_id_valid {
                                format!("[INVALID_REQUEST_ID:{}][USING_PROOF_REQUEST_ID:{}]", request_id, proof_request.id)
                            } else {
                                format!("[INVALID_REQUEST_ID:{}][INVALID_PROOF_REQUEST_ID:{}]", request_id, proof_request.id)
                            }
                        } else if proof_request_id_valid && proof_request.id != request_id {
                            // Both valid but different - show both
                            format!("{}[response_id:{}]", request_id, proof_request.id)
                        } else {
                            // Normal case - use request_id
                            request_id.clone()
                        };
                        
                        // Debug: Print all values before constructing error message
                        eprintln!("[DEBUG] request_id={}, proof_request.id={}, request_id_valid={}, proof_request_id_valid={}", request_id, proof_request.id, request_id_valid, proof_request_id_valid);
                        eprintln!("[DEBUG] request_id_display={}, status_str={}, elapsed={:?}", request_id_display, status_str, elapsed);
                        eprintln!("[DEBUG] execution_result.is_some()={}", proof_request.execution_result.is_some());
                        
                        // Build error message with explicit formatting to prevent truncation
                        // Use format! to ensure all parts are included
                        let mut error_msg = format!(
                            "Proof request {} (status={}) after {:?}",
                            request_id_display, status_str, elapsed
                        );
                        eprintln!("[DEBUG] error_msg after initial format: {}", error_msg);
                        
                        // Add execution details
                        if let Some(exec) = &proof_request.execution_result {
                            let exec_status_str = match exec.status {
                                0 => "UNSPECIFIED".to_string(),
                                1 => "UNEXECUTED".to_string(),
                                2 => "EXECUTED".to_string(),
                                3 => "FAILED".to_string(),
                                4 => "CANCELLED".to_string(),
                                _ => format!("UNKNOWN({})", exec.status),
                            };
                            let failure_cause_str = match exec.failure_cause {
                                0 => "UNSPECIFIED".to_string(),
                                1 => "HALT_WITH_NON_ZERO_EXIT_CODE".to_string(),
                                2 => "INVALID_MEMORY_ACCESS".to_string(),
                                3 => "UNSUPPORTED_SYSCALL".to_string(),
                                4 => "BREAKPOINT".to_string(),
                                5 => "EXCEEDED_CYCLE_LIMIT".to_string(),
                                6 => "INVALID_SYSCALL_USAGE".to_string(),
                                7 => "UNIMPLEMENTED".to_string(),
                                8 => "END_IN_UNCONSTRAINED".to_string(),
                                _ => format!("UNKNOWN({})", exec.failure_cause),
                            };
                            error_msg.push_str(" - Execution: status=");
                            error_msg.push_str(&exec_status_str);
                            error_msg.push_str(", failure_cause=");
                            error_msg.push_str(&failure_cause_str);
                            error_msg.push_str(", cycles=");
                            error_msg.push_str(&exec.cycles.to_string());
                            error_msg.push_str(", gas=");
                            error_msg.push_str(&exec.gas.to_string());
                            
                            // Add public values hash info if available
                            if !exec.public_values_hash.is_empty() {
                                error_msg.push_str(", public_values_hash_len=");
                                error_msg.push_str(&exec.public_values_hash.len().to_string());
                            }
                        } else {
                            error_msg.push_str(" - Execution: no execution_result (execution may not have started or failed before execution)");
                        }
                        
                        // Add metadata if available
                        if !proof_request.metadata.is_empty() {
                            error_msg.push_str(" - metadata: ");
                            error_msg.push_str(&proof_request.metadata);
                        }
                        
                        // Add context for quick failures
                        if elapsed.as_millis() < 1000 {
                            error_msg.push_str(" - Note: Failure occurred very quickly, may indicate execution failure or resource issue");
                        }
                        
                        // Validate error message is not empty or suspiciously short
                        if error_msg.is_empty() {
                            error_msg = format!(
                                "Proof request {} failed with empty error message (status={}, elapsed={:?})",
                                request_id_display, status_str, elapsed
                            );
                        } else if error_msg.len() < 20 {
                            // If error message is suspiciously short, add more context
                            error_msg = format!(
                                "{} [original_msg_len={}, request_id={}, status={}]",
                                error_msg, error_msg.len(), request_id_display, status_str
                            );
                        }
                        
                        // Log the complete error message before returning
                        eprintln!("[ERROR] Proof request failed with error ({} chars): {}", error_msg.len(), error_msg);
                        eprintln!("[ERROR] Full error message: {}", error_msg);
                        info!("Proof request failed with error ({} chars): {}", error_msg.len(), error_msg);
                        
                        // Log to debug file as well
                        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                            use std::io::Write;
                            let escaped_msg = error_msg.replace('"', "\\\"").replace('\n', "\\n");
                            let _ = writeln!(file, "{{\"location\":\"client.rs:428\",\"message\":\"Complete error message before returning\",\"data\":{{\"error_msg\":\"{}\",\"error_msg_len\":{},\"request_id\":\"{}\",\"status\":\"{}\"}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"E\"}}", escaped_msg, error_msg.len(), request_id, status_str, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                        }
                        
                        // #region agent log
                        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                            use std::io::Write;
                            let error_obj = format!("{{\"Prove\":\"{}\"}}", error_msg.replace('"', "\\\"").replace('\n', "\\n"));
                            let _ = writeln!(file, "{{\"location\":\"client.rs:438\",\"message\":\"Creating Error::Prove\",\"data\":{{\"error_obj\":{}}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"E\"}}", error_obj, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                        }
                        // #endregion
                        
                        return Err(Error::Prove(error_msg));
                    }
                    _ => {}
                }
            }

            sleep(Duration::from_millis(500)).await;
        }
    }

    /// Execute a program (without proving)
    pub async fn execute(&self, _elf: &[u8], _stdin: &[u8]) -> Result<ExecuteResult, Error> {
        // For execution-only, we use the SP1 SDK locally
        // The cluster is primarily for proving
        Err(Error::Execute(
            "Execute-only not supported via cluster. Use local SP1 SDK.".to_string(),
        ))
    }

    /// Returns the number of GPUs configured
    pub fn num_gpus(&self) -> Option<u32> {
        self.num_gpus
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

/// Result from an execute operation
#[derive(Debug)]
pub struct ExecuteResult {
    pub public_values: Vec<u8>,
    pub cycles: u64,
    pub execution_time: Duration,
}
