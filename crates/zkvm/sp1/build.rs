use ere_build_utils::detect_and_generate_name_and_sdk_version;

fn main() {
    detect_and_generate_name_and_sdk_version("sp1", "sp1-sdk");

    // Compile cluster proto
    let proto_dir = std::path::Path::new("proto");
    tonic_build::configure()
        .build_server(false)
        .compile_protos(&[proto_dir.join("cluster.proto")], &[proto_dir])
        .expect("Failed to compile cluster proto");
}
