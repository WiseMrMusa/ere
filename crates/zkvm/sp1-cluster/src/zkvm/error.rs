use ere_zkvm_interface::zkvm::{CommonError, ProofKind};
use sp1_sdk::{SP1ProofMode, SP1VerificationError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    CommonError(#[from] CommonError),

    // Configuration errors
    #[error("SP1 Cluster endpoint not configured. Set SP1_CLUSTER_ENDPOINT environment variable or provide endpoint in ClusterProverConfig")]
    EndpointNotConfigured,

    #[error("Redis URL not configured. Set SP1_CLUSTER_REDIS_URL environment variable")]
    RedisNotConfigured,

    #[error("Invalid cluster endpoint URL: {0}")]
    InvalidEndpoint(String),

    // gRPC errors
    #[error("Failed to connect to gRPC service: {0}")]
    GrpcConnect(String),

    #[error("gRPC request failed: {0}")]
    GrpcRequest(String),

    // Redis errors
    #[error("Redis error: {0}")]
    Redis(String),

    // Execution errors
    #[error("SP1 Cluster execution failed: {0}")]
    Execute(String),

    // Proving errors
    #[error("SP1 Cluster proving failed: {0}")]
    Prove(String),

    #[error("SP1 Cluster proving timed out after {0} seconds")]
    ProveTimeout(u64),

    // Verification errors
    #[error("Invalid proof kind, expected: {0:?}, got: {1:?}")]
    InvalidProofKind(ProofKind, SP1ProofMode),

    #[error("SP1 SDK verification failed: {0}")]
    Verify(#[source] SP1VerificationError),

    // Setup errors
    #[error("Failed to setup proving/verifying keys: {0}")]
    Setup(String),
}
