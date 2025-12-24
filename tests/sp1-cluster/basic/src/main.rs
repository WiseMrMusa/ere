//! SP1 Cluster test guest program
//!
//! This is identical to the SP1 test guest since SP1 Cluster uses the same
//! program format and runtime.

#![no_main]

use ere_platform_sp1_cluster::{sp1_zkvm, SP1Platform};
use ere_test_utils::program::{basic::BasicProgram, Program};

sp1_zkvm::entrypoint!(main);

pub fn main() {
    BasicProgram::run::<SP1Platform>();
}
