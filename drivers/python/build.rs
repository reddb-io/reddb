fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    println!("cargo:rerun-if-changed=proto/reddb.proto");
    tonic_build::configure()
        .build_server(false)
        .out_dir(&out_dir)
        .compile_protos(&["proto/reddb.proto"], &["proto/"])?;
    Ok(())
}
