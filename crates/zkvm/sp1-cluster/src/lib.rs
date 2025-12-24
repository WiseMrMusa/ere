#![cfg_attr(
    all(not(test), feature = "compiler", feature = "zkvm"),
    warn(unused_crate_dependencies)
)]

//! # ere-sp1-cluster
//!
//! SP1 Cluster integration for the Ere zkVM framework.
//!
//! This crate provides support for proving using a self-hosted SP1 Cluster,
//! which is a multi-GPU proving service. It reuses the SP1 compiler and program
//! format but connects to a cluster API for distributed GPU proving.
//!
//! ## Configuration
//!
//! The cluster can be configured via environment variables:
//! - `SP1_CLUSTER_ENDPOINT`: The cluster API endpoint URL (required)
//! - `SP1_CLUSTER_API_KEY`: Optional API key for authentication
//! - `SP1_CLUSTER_NUM_GPUS`: Number of GPUs to use (optional, defaults to all)

pub mod program;

#[cfg(feature = "compiler")]
pub mod compiler;

#[cfg(feature = "zkvm")]
pub mod zkvm;

#[cfg(feature = "zkvm")]
pub use zkvm::*;
