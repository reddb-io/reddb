fn main() {
    println!("cargo:rerun-if-changed=proto/reddb.proto");
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/reddb.proto"], &["proto"])
        .expect("failed to compile reddb gRPC protobufs");
}
