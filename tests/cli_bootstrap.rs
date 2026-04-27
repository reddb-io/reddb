//! Integration tests for `red bootstrap` and the `*_FILE` env-var
//! expansion. These spawn the real `red` binary via `CARGO_BIN_EXE_red`
//! so they exercise the full main() path including the boot-time
//! secret expansion.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

/// Per-test scratch dir under /tmp. Pid + nanos suffix dodges
/// collision when the test binary runs in parallel.
fn scratch_dir(label: &str) -> PathBuf {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "reddb-cli-bootstrap-{}-{}-{}",
        label,
        std::process::id(),
        now_ns
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run `red bootstrap ...` with a short-lived REDDB_VAULT_KEY env var
/// (the cheapest seal the vault accepts) and the password fed via
/// stdin. Returns the (status, stdout, stderr) triple.
fn run_bootstrap(args: &[&str], password: Option<&str>, vault_key: &str) -> (i32, String, String) {
    let mut cmd = Command::new(red_binary());
    cmd.args(args)
        .env("REDDB_VAULT_KEY", vault_key)
        // Strip any inherited values so the run is hermetic.
        .env_remove("REDDB_CERTIFICATE")
        .env_remove("REDDB_USERNAME")
        .env_remove("REDDB_PASSWORD")
        .env_remove("REDDB_USERNAME_FILE")
        .env_remove("REDDB_PASSWORD_FILE")
        .env_remove("REDDB_CERTIFICATE_FILE")
        .env_remove("REDDB_VAULT_KEY_FILE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn red bootstrap");
    if let Some(pw) = password {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, "{pw}").unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait red bootstrap");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn bootstrap_succeeds_and_prints_certificate() {
    let dir = scratch_dir("ok");
    let path = dir.join("x.rdb");
    let path_str = path.display().to_string();

    let (code, stdout, stderr) = run_bootstrap(
        &[
            "bootstrap",
            "--path",
            &path_str,
            "--vault",
            "--username",
            "admin",
            "--password-stdin",
            "--print-certificate",
        ],
        Some("hunter2"),
        "test-vault-key-1",
    );

    assert_eq!(code, 0, "exit code != 0; stderr: {stderr}");
    let cert = stdout.trim().to_string();
    assert_eq!(
        cert.len(),
        64,
        "expected 64-hex certificate, got {} bytes: {cert:?}",
        cert.len()
    );
    assert!(
        cert.chars().all(|c| c.is_ascii_hexdigit()),
        "certificate not hex: {cert:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bootstrap_fails_when_already_bootstrapped() {
    let dir = scratch_dir("rerun");
    let path = dir.join("x.rdb");
    let path_str = path.display().to_string();

    // First run: success.
    let (code1, _, _) = run_bootstrap(
        &[
            "bootstrap",
            "--path",
            &path_str,
            "--vault",
            "--username",
            "admin",
            "--password-stdin",
            "--print-certificate",
        ],
        Some("hunter2"),
        "test-vault-key-2",
    );
    assert_eq!(code1, 0);

    // Second run: must fail.
    let (code2, _, stderr2) = run_bootstrap(
        &[
            "bootstrap",
            "--path",
            &path_str,
            "--vault",
            "--username",
            "admin",
            "--password-stdin",
            "--print-certificate",
        ],
        Some("hunter2"),
        "test-vault-key-2",
    );
    assert_ne!(code2, 0, "expected non-zero exit on re-bootstrap");
    assert!(
        stderr2.to_lowercase().contains("bootstrap") || stderr2.to_lowercase().contains("already"),
        "stderr should mention re-bootstrap; got: {stderr2}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bootstrap_json_output_has_required_keys() {
    let dir = scratch_dir("json");
    let path = dir.join("x.rdb");
    let path_str = path.display().to_string();

    let (code, stdout, stderr) = run_bootstrap(
        &[
            "bootstrap",
            "--path",
            &path_str,
            "--vault",
            "--username",
            "admin",
            "--password-stdin",
            "--json",
        ],
        Some("p4ssw0rd"),
        "test-vault-key-3",
    );
    assert_eq!(code, 0, "exit != 0; stderr: {stderr}");
    let line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with('{'))
        .unwrap_or_else(|| panic!("no JSON line in stdout: {stdout}"));
    // Cheap structural check — we don't have serde_json in dev-deps,
    // so look for the three documented keys.
    assert!(line.contains("\"username\":\"admin\""), "got: {line}");
    assert!(line.contains("\"token\":\""), "got: {line}");
    assert!(line.contains("\"certificate\":\""), "got: {line}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bootstrap_requires_vault_flag() {
    let dir = scratch_dir("novault");
    let path = dir.join("x.rdb");
    let path_str = path.display().to_string();

    let (code, _, stderr) = run_bootstrap(
        &[
            "bootstrap",
            "--path",
            &path_str,
            "--username",
            "admin",
            "--password-stdin",
        ],
        Some("hunter2"),
        "test-vault-key-4",
    );
    assert_ne!(code, 0);
    assert!(
        stderr.contains("--vault") || stderr.contains("vault"),
        "stderr should mention vault: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn password_file_env_expanded_into_password_var() {
    // Verifies the *_FILE expansion pipeline end-to-end: write a
    // password into a tmpfile, point REDDB_PASSWORD_FILE at it,
    // and run bootstrap WITHOUT --password-stdin or --password.
    // Bootstrap should pull the password out of REDDB_PASSWORD,
    // which was filled by the boot-time expander.
    let dir = scratch_dir("file-expand");
    let path = dir.join("x.rdb");
    let path_str = path.display().to_string();
    let pwfile = dir.join("pw");
    std::fs::write(&pwfile, "from-file-pw\n").unwrap();

    let mut cmd = Command::new(red_binary());
    cmd.args([
        "bootstrap",
        "--path",
        &path_str,
        "--vault",
        "--username",
        "admin",
        "--print-certificate",
    ])
    .env("REDDB_VAULT_KEY", "test-vault-key-5")
    .env("REDDB_PASSWORD_FILE", &pwfile)
    .env_remove("REDDB_CERTIFICATE")
    .env_remove("REDDB_USERNAME")
    .env_remove("REDDB_PASSWORD")
    .env_remove("REDDB_USERNAME_FILE")
    .env_remove("REDDB_CERTIFICATE_FILE")
    .env_remove("REDDB_VAULT_KEY_FILE")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let out = cmd.output().expect("run red bootstrap");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "stdout: {stdout}\nstderr: {stderr}"
    );
    let cert = stdout.trim();
    assert_eq!(cert.len(), 64, "expected 64-hex cert, got {cert:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn password_file_and_password_var_conflict_fails_boot() {
    // When both REDDB_PASSWORD and REDDB_PASSWORD_FILE are set the
    // process must refuse to start (exit 2 from main).
    let dir = scratch_dir("conflict");
    let pwfile = dir.join("pw");
    std::fs::write(&pwfile, "from-file").unwrap();

    let mut cmd = Command::new(red_binary());
    cmd.args(["bootstrap", "--vault"])
        .env("REDDB_PASSWORD", "from-env")
        .env("REDDB_PASSWORD_FILE", &pwfile)
        .env_remove("REDDB_CERTIFICATE")
        .env_remove("REDDB_USERNAME")
        .env_remove("REDDB_USERNAME_FILE")
        .env_remove("REDDB_VAULT_KEY")
        .env_remove("REDDB_VAULT_KEY_FILE")
        .env_remove("REDDB_CERTIFICATE_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = cmd.output().expect("run red bootstrap");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code().unwrap_or(-1), 0, "stderr: {stderr}");
    assert!(
        stderr.contains("REDDB_PASSWORD") || stderr.contains("_FILE"),
        "expected conflict message; got: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
