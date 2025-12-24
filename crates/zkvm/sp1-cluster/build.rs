use ere_build_utils::detect_and_generate_name_and_sdk_version;

fn main() {
    detect_and_generate_name_and_sdk_version("sp1-cluster", "sp1-sdk");

    // Compile the cluster gRPC proto
    // Include well-known types from proto/google/protobuf/ directory
    tonic_build::configure()
        .build_server(false)
        .protoc_arg("--experimental_allow_proto3_optional")
        .compile_protos(&["proto/cluster.proto"], &["proto"])
        .expect("Failed to compile cluster.proto");

    println!("cargo:rerun-if-changed=proto/cluster.proto");
    println!("cargo:rerun-if-changed=proto/google/protobuf/empty.proto");
}
