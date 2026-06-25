//! Compile the `unreliable-libc` LD_PRELOAD shim into a standalone shared
//! object, independent of the engine.
//!
//! We invoke the C compiler directly (rather than `cc::Build`, which only emits
//! static archives) so the artifact is a real `LD_PRELOAD`-able `.so`. The path
//! is exported as `UNRELIABLE_LIBC_SO` so the integration test can preload it.
//! No nested `cargo` is spawned, so the host's build guard never deadlocks.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let src = "csrc/unreliable_libc.c";
    println!("cargo:rerun-if-changed={src}");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let so_path = out_dir.join("libunreliable_libc.so");

    // Reuse cc's compiler discovery (honours CC/cross targets), then drive it
    // as a shared-object link ourselves.
    let compiler = cc::Build::new().get_compiler();
    let mut cmd = Command::new(compiler.path());
    cmd.args(compiler.args());
    cmd.arg("-shared")
        .arg("-fPIC")
        .arg("-O2")
        .arg("-o")
        .arg(&so_path)
        .arg(src)
        .arg("-ldl");

    let status = cmd
        .status()
        .expect("failed to spawn C compiler for unreliable-libc shim");
    assert!(
        status.success(),
        "C compiler failed to build the unreliable-libc shim ({status})"
    );

    println!("cargo:rustc-env=UNRELIABLE_LIBC_SO={}", so_path.display());
}
