//! Pins the `red_client` contract: embedded schemes are rejected
//! at parse time with exit code 2 and a message pointing the user
//! at the full `red` binary.
//!
//! The bin is exercised through its release binary so the test
//! covers the actual end-to-end exit code, not just the internal
//! `resolve_endpoint` helper.

use std::process::Command;

fn run_red_client(uri: &str) -> (i32, String, String) {
    let bin = env!("CARGO_BIN_EXE_red_client");
    let out = Command::new(bin)
        .arg(uri)
        .output()
        .expect("spawn red_client");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn rejects_memory_scheme() {
    let (code, _stdout, stderr) = run_red_client("memory://");
    assert_eq!(code, 2, "expected exit code 2 for memory://");
    assert!(
        stderr.contains("embedded") || stderr.contains("memory"),
        "stderr should mention embedded/memory, got: {stderr}"
    );
    assert!(
        stderr.contains("red"),
        "stderr should point user at the `red` binary, got: {stderr}"
    );
}

#[test]
fn rejects_memory_alias() {
    let (code, _stdout, _stderr) = run_red_client("memory:");
    assert_eq!(code, 2, "expected exit code 2 for memory: alias");
}

#[test]
fn rejects_file_scheme() {
    let (code, _stdout, stderr) = run_red_client("file:///var/lib/reddb/data.rdb");
    assert_eq!(code, 2, "expected exit code 2 for file://");
    assert!(
        stderr.contains("embedded") || stderr.contains("file"),
        "stderr should mention embedded/file, got: {stderr}"
    );
}

#[test]
fn rejects_red_no_host() {
    let (code, _stdout, _stderr) = run_red_client("red://");
    assert_eq!(code, 2, "expected exit code 2 for `red://` (embedded form)");
}

#[test]
fn rejects_red_memory_alias() {
    let (code, _stdout, _stderr) = run_red_client("red://:memory:");
    assert_eq!(code, 2, "expected exit code 2 for `red://:memory:`");
}

#[test]
fn rejects_red_file_path() {
    let (code, _stdout, _stderr) = run_red_client("red:///var/lib/data.rdb");
    assert_eq!(code, 2, "expected exit code 2 for `red:///path` (embedded form)");
}

#[test]
fn rejects_pg_scheme_with_clear_message() {
    // `red://host:5432?proto=pg` → server-only PostgreSQL wire.
    let (code, _stdout, stderr) = run_red_client("red://localhost:5432?proto=pg");
    assert_eq!(code, 5, "expected exit code 5 (transport unsupported)");
    assert!(
        stderr.contains("PostgreSQL") || stderr.contains("pg") || stderr.contains("psql"),
        "stderr should mention PG transport, got: {stderr}"
    );
}

#[test]
fn rejects_unknown_scheme() {
    let (code, _stdout, _stderr) = run_red_client("mongodb://localhost");
    assert_eq!(code, 1, "expected exit code 1 (usage / parse error)");
}

#[test]
fn missing_uri_is_usage_error() {
    let bin = env!("CARGO_BIN_EXE_red_client");
    let out = Command::new(bin).output().expect("spawn red_client");
    assert_eq!(
        out.status.code().unwrap_or(-1),
        1,
        "expected exit code 1 when no URI is passed"
    );
}
