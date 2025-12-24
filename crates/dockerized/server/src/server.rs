use crate::api::{
    self, ExecuteOk, ExecuteRequest, ExecuteResponse, ProveOk, ProveRequest, ProveResponse,
    VerifyOk, VerifyRequest, VerifyResponse, ZkvmService,
    execute_response::Result as ExecuteResult, prove_response::Result as ProveResult,
    verify_response::Result as VerifyResult,
};
use anyhow::Context;
use ere_zkvm_interface::zkvm::{
    ProgramExecutionReport, ProgramProvingReport, Proof, ProofKind, PublicValues, zkVM,
};
use std::sync::Arc;
use twirp::{
    Request, Response, TwirpErrorResponse, async_trait::async_trait, internal, invalid_argument,
};

pub use api::router;

/// zkVM server that handles the request by forwarding to the underlying
/// [`zkVM`] implementation methods.
#[allow(non_camel_case_types)]
pub struct zkVMServer<T> {
    zkvm: Arc<T>,
}

impl<T: 'static + zkVM + Send + Sync> zkVMServer<T> {
    pub fn new(zkvm: T) -> Self {
        Self {
            zkvm: Arc::new(zkvm),
        }
    }

    async fn execute(
        &self,
        input: Vec<u8>,
    ) -> anyhow::Result<(PublicValues, ProgramExecutionReport)> {
        let zkvm = Arc::clone(&self.zkvm);
        tokio::task::spawn_blocking(move || zkvm.execute(&input))
            .await
            .context("execute panicked")?
    }

    async fn prove(
        &self,
        input: Vec<u8>,
        proof_kind: ProofKind,
    ) -> anyhow::Result<(PublicValues, Proof, ProgramProvingReport)> {
        let zkvm = Arc::clone(&self.zkvm);
        tokio::task::spawn_blocking(move || zkvm.prove(&input, proof_kind))
            .await
            .context("prove panicked")?
    }

    async fn verify(&self, proof: Proof) -> anyhow::Result<PublicValues> {
        let zkvm = Arc::clone(&self.zkvm);
        tokio::task::spawn_blocking(move || zkvm.verify(&proof))
            .await
            .context("verify panicked")?
    }
}

#[async_trait]
impl<T: 'static + zkVM + Send + Sync> ZkvmService for zkVMServer<T> {
    async fn execute(
        &self,
        request: Request<ExecuteRequest>,
    ) -> twirp::Result<Response<ExecuteResponse>> {
        let request = request.into_body();

        let input = request.input;

        let result = match self.execute(input).await {
            Ok((public_values, report)) => ExecuteResult::Ok(ExecuteOk {
                public_values,
                report: bincode::serde::encode_to_vec(&report, bincode::config::legacy())
                    .map_err(serialize_report_err)?,
            }),
            Err(err) => ExecuteResult::Err(err.to_string()),
        };

        Ok(Response::new(ExecuteResponse {
            result: Some(result),
        }))
    }

    async fn prove(
        &self,
        request: Request<ProveRequest>,
    ) -> twirp::Result<Response<ProveResponse>> {
        let request = request.into_body();

        let input = request.input;
        let proof_kind = ProofKind::from_repr(request.proof_kind as usize)
            .ok_or_else(|| invalid_proof_kind_err(request.proof_kind))?;

        let result = match self.prove(input, proof_kind).await {
            Ok((public_values, proof, report)) => ProveResult::Ok(ProveOk {
                public_values,
                proof: proof.as_bytes().to_vec(),
                report: bincode::serde::encode_to_vec(&report, bincode::config::legacy())
                    .map_err(serialize_report_err)?,
            }),
            Err(err) => {
                // #region agent log
                let log_path = "/root/sp1-cluster/.cursor/debug.log";
                let error_str = err.to_string();
                let error_debug = format!("{:?}", err);
                let error_chain: Vec<String> = err.chain().map(|e| e.to_string()).collect();
                eprintln!("[SERVER-ERROR] Prove error: {}", error_str);
                eprintln!("[SERVER-ERROR] Error debug: {}", error_debug);
                eprintln!("[SERVER-ERROR] Error chain: {:?}", error_chain);
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(log_path) {
                    use std::io::Write;
                    let _ = writeln!(file, "{{\"location\":\"server.rs:102\",\"message\":\"Prove error in server\",\"data\":{{\"error_str\":\"{}\",\"error_debug\":\"{}\",\"error_chain\":{:?},\"error_str_len\":{}}},\"timestamp\":{},\"sessionId\":\"debug-session\",\"runId\":\"run1\",\"hypothesisId\":\"F\"}}", error_str.replace("\"", "\\\"").replace("\n", "\\n"), error_debug.replace("\"", "\\\"").replace("\n", "\\n"), error_chain, error_str.len(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                }
                // #endregion
                // Use the full error chain to preserve all error details
                let full_error = if error_chain.len() > 1 {
                    error_chain.join(": ")
                } else {
                    error_str
                };
                ProveResult::Err(full_error)
            }
        };

        Ok(Response::new(ProveResponse {
            result: Some(result),
        }))
    }

    async fn verify(
        &self,
        request: Request<VerifyRequest>,
    ) -> twirp::Result<Response<VerifyResponse>> {
        let request = request.into_body();

        let proof_kind = ProofKind::from_repr(request.proof_kind as usize)
            .ok_or_else(|| invalid_proof_kind_err(request.proof_kind))?;

        let result = match self.verify(Proof::new(proof_kind, request.proof)).await {
            Ok(public_values) => VerifyResult::Ok(VerifyOk { public_values }),
            Err(err) => VerifyResult::Err(err.to_string()),
        };

        Ok(Response::new(VerifyResponse {
            result: Some(result),
        }))
    }
}

fn invalid_proof_kind_err(proof_kind: i32) -> TwirpErrorResponse {
    invalid_argument(format!("invalid proof kind: {proof_kind}"))
}

fn serialize_report_err(err: bincode::error::EncodeError) -> TwirpErrorResponse {
    internal(format!("failed to serialize report: {err}"))
}
