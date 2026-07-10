//! Integration tests for #1587 — first-boot self-bootstrap.
//!
//! These spawn the real `red` binary via `CARGO_BIN_EXE_red` so they
//! exercise the full `main()` boot path: a single `red server` on a
//! fresh volume with a bootstrap intent must create the paged vault in
//! place, apply the preset, and serve — no separate `red bootstrap` and
//! no "vault requires a paged database" abort.

#[allow(dead_code)]
mod support;

use std::fs::File;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

/// Grab an OS-assigned free TCP port, then release it so the child can
/// bind it. Small TOCTOU window, acceptable for a test.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    listener.local_addr().expect("local_addr").port()
}

/// A spawned `red server` that is killed + reaped on drop, with its
/// stderr captured to a file for post-mortem assertions.
struct ServerChild {
    child: Child,
    stderr_path: PathBuf,
}

impl ServerChild {
    fn stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `red server` with a hermetic environment. `stderr_path` receives
/// the child's stderr so the caller can grep it after the child exits.
fn spawn_server(args: &[&str], stderr_path: &Path) -> ServerChild {
    let stderr_file = File::create(stderr_path).expect("create stderr file");
    let child = Command::new(red_binary())
        .args(args)
        .env_remove("REDDB_CERTIFICATE")
        .env_remove("REDDB_USERNAME")
        .env_remove("REDDB_PASSWORD")
        .env_remove("REDDB_USERNAME_FILE")
        .env_remove("REDDB_PASSWORD_FILE")
        .env_remove("REDDB_CERTIFICATE_FILE")
        .env_remove("REDDB_BOOTSTRAP_PRESET")
        .env_remove("REDDB_PRESET")
        .env_remove("REDDB_BOOTSTRAP_MANIFEST")
        .env_remove("REDDB_BOOTSTRAP_CERT_OUT")
        .env_remove("REDDB_VAULT")
        .env_remove("REDDB_AUTH")
        .env_remove("REDDB_REQUIRE_AUTH")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .expect("spawn red server");
    ServerChild {
        child,
        stderr_path: stderr_path.to_path_buf(),
    }
}

/// Spawn `red server` like [`spawn_server`], but with `REDDB_CERTIFICATE_FILE`
/// pointed at `cert_file` so a reboot can unseal the existing vault from the
/// certificate written by an earlier `--bootstrap-cert-out` boot (issue #1589).
fn spawn_server_with_cert_file(args: &[&str], stderr_path: &Path, cert_file: &Path) -> ServerChild {
    let stderr_file = File::create(stderr_path).expect("create stderr file");
    let child = Command::new(red_binary())
        .args(args)
        .env_remove("REDDB_CERTIFICATE")
        .env_remove("REDDB_USERNAME")
        .env_remove("REDDB_PASSWORD")
        .env_remove("REDDB_USERNAME_FILE")
        .env_remove("REDDB_PASSWORD_FILE")
        .env("REDDB_CERTIFICATE_FILE", cert_file)
        .env_remove("REDDB_BOOTSTRAP_PRESET")
        .env_remove("REDDB_PRESET")
        .env_remove("REDDB_BOOTSTRAP_MANIFEST")
        .env_remove("REDDB_BOOTSTRAP_CERT_OUT")
        .env_remove("REDDB_VAULT")
        .env_remove("REDDB_AUTH")
        .env_remove("REDDB_REQUIRE_AUTH")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .expect("spawn red server");
    ServerChild {
        child,
        stderr_path: stderr_path.to_path_buf(),
    }
}

/// Wait until the spawned server is serving: it is still alive AND a TCP
/// connection to its listener succeeds (listeners bind only after the
/// runtime + vault + preset are built, so a live connect proves the boot
/// got past the vault gate). Returns `false` if the process exits first.
fn wait_until_serving(server: &mut ServerChild, addr: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(Some(_status)) = server.child.try_wait() {
            return false; // exited before serving — boot failed
        }
        if TcpStream::connect(addr).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}

/// Poll until `path` exists (the spawned server is still alive), or the
/// process exits / the timeout elapses. Returns `true` only when the file
/// appeared while the server was running. More robust than a bare TCP
/// probe: it ties the wait to the artifact under test and cannot be fooled
/// by a stale listener on the same ephemeral port.
fn wait_for_file(server: &mut ServerChild, path: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        if let Ok(Some(_status)) = server.child.try_wait() {
            return path.exists(); // exited — accept only if the file landed
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    path.exists()
}

/// Poll the server's stderr until it contains `needle` (proof the boot
/// reached that log line), the process exits, or the timeout elapses.
fn wait_for_stderr_contains(server: &mut ServerChild, needle: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if server.stderr().contains(needle) {
            return true;
        }
        if let Ok(Some(_status)) = server.child.try_wait() {
            return server.stderr().contains(needle);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    server.stderr().contains(needle)
}

/// Wait for the process to exit, returning its exit code (or `None` on
/// timeout). Used by the negative test where boot must fail fast.
fn wait_for_exit(server: &mut ServerChild, timeout: Duration) -> Option<i32> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(Some(status)) = server.child.try_wait() {
            return Some(status.code().unwrap_or(-1));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// A paged store carries either the `RDDB` magic at the start of page 0's
/// payload (after the 32-byte page header) or, in the zoned single-file
/// layout, the `RDBSBLK1` superblock magic at offset 0 (ADR 0038 §2). Either
/// in-file magic is the marker that the paged vault was created in place —
/// the `-hdr` sidecar that used to serve this role is retired.
fn paged_vault_created(db_path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(db_path) else {
        return false;
    };
    bytes.get(32..36) == Some(b"RDDB".as_slice()) || bytes.get(0..8) == Some(b"RDBSBLK1".as_slice())
}

/// Page 0 (or the superblock zone) is flushed by the engine's own cadence,
/// not synchronously with "serving" — poll instead of racing it.
fn wait_for_paged_vault(db_path: &Path) -> bool {
    for _ in 0..100 {
        if paged_vault_created(db_path) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn cloud_server_args<'a>(
    db_path: &'a str,
    http_addr: &'a str,
    head_pw: &'a str,
    customer_pw: &'a str,
) -> Vec<&'a str> {
    vec![
        "server",
        "--path",
        db_path,
        "--vault",
        "true",
        "--http",
        "--http-bind",
        http_addr,
        "--bootstrap-preset",
        "cloud",
        "--cloud-head-admin",
        "red_admin",
        "--cloud-head-admin-password-file",
        head_pw,
        "--customer-admin",
        "admin",
        "--customer-admin-password-file",
        customer_pw,
    ]
}

/// First boot on a fresh volume with a `cloud` bootstrap intent creates
/// the paged vault in place, applies the preset, and serves — and a
/// re-boot against the now-existing vault serves again without
/// re-bootstrapping (idempotent).
#[test]
fn first_boot_cloud_intent_creates_vault_then_idempotent_reboot() {
    let dir = support::temp_data_dir("first-boot-cloud");
    let db_path = dir.join("data.rdb");
    let db_path_str = db_path.display().to_string();
    let head_pw = dir.join("head.pw");
    let customer_pw = dir.join("customer.pw");
    std::fs::write(&head_pw, "head-secret\n").unwrap();
    std::fs::write(&customer_pw, "customer-secret\n").unwrap();
    let head_pw_s = head_pw.to_str().unwrap();
    let customer_pw_s = customer_pw.to_str().unwrap();
    let port = free_port();
    let http_addr = format!("127.0.0.1:{port}");
    let stderr_path = dir.join("server.stderr");

    // Sanity: the database does not exist yet.
    assert!(!db_path.exists(), "fresh volume precondition");

    let mut server = spawn_server(
        &cloud_server_args(&db_path_str, &http_addr, head_pw_s, customer_pw_s),
        &stderr_path,
    );

    let serving = wait_until_serving(&mut server, &http_addr, Duration::from_secs(30));
    let stderr = server.stderr();
    assert!(
        serving,
        "server never came up — first boot did not serve.\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("vault requires a paged database"),
        "first boot must NOT abort at the vault gate.\nstderr:\n{stderr}"
    );
    // The paged vault was created in place (operational-directory layout).
    assert!(
        wait_for_paged_vault(&db_path),
        "expected paged vault created in place ({} lacks the page-0/superblock magic).\nstderr:\n{stderr}",
        db_path.display()
    );

    drop(server); // kill + reap

    // ---- Idempotent re-boot against the now-existing paged vault. ----
    // If the preset were re-applied, re-creating the head/customer admins
    // would fail ("already exists") and boot would abort — so a clean
    // second serve is itself proof that the bootstrap-completed marker
    // short-circuited preset re-application (no re-bootstrap, no new cert).
    let stderr_path2 = dir.join("server2.stderr");
    let mut server2 = spawn_server(
        &cloud_server_args(&db_path_str, &http_addr, head_pw_s, customer_pw_s),
        &stderr_path2,
    );

    let serving2 = wait_until_serving(&mut server2, &http_addr, Duration::from_secs(30));
    let stderr2 = server2.stderr();
    assert!(
        serving2,
        "re-boot against the existing vault must serve without re-bootstrapping.\nstderr:\n{stderr2}"
    );
    assert!(
        !stderr2.contains("vault requires a paged database"),
        "re-boot must not abort at the vault gate.\nstderr:\n{stderr2}"
    );
}

/// No bootstrap intent + non-existent path keeps today's behaviour: the
/// vault gate aborts with a clear error rather than silently creating an
/// empty vault. (`--vault true` with no preset/credentials = no intent.)
#[test]
fn no_intent_fresh_path_keeps_vault_gate_error() {
    let dir = support::temp_data_dir("first-boot-no-intent");
    let db_path = dir.join("data.rdb");
    let db_path_str = db_path.display().to_string();
    let port = free_port();
    let http_addr = format!("127.0.0.1:{port}");
    let stderr_path = dir.join("server.stderr");

    let mut server = spawn_server(
        &[
            "server",
            "--path",
            &db_path_str,
            "--vault",
            "true",
            "--http",
            "--http-bind",
            &http_addr,
        ],
        &stderr_path,
    );

    // Without an intent the boot must fail fast at the vault gate — it
    // must exit non-zero and never come up as a listener.
    let code = wait_for_exit(&mut server, Duration::from_secs(15));
    let stderr = server.stderr();
    assert_eq!(
        code,
        Some(1),
        "no-intent fresh-path boot must exit 1 at the vault gate; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains("vault requires a paged database"),
        "expected the clear vault-gate error; stderr:\n{stderr}"
    );
    // No vault was silently created: the gate error above fires BEFORE any
    // bootstrap provisioning runs, and exit 1 proves the boot stopped there.
    // (A file-shape probe can no longer carry this property — the zoned
    // single-file layout writes its superblock zone on mere store creation,
    // vault or not.)
}

/// Issue #1589 — first boot with `--bootstrap-cert-out <path>` writes the
/// freshly minted unseal certificate to `<path>` (in addition to the
/// stdout/stderr emission). The written file is consumable via
/// `REDDB_CERTIFICATE_FILE` to unseal on a subsequent boot, and a re-boot
/// against the existing vault does not rewrite/churn the file.
#[test]
fn first_boot_cert_out_writes_cert_then_unseal_roundtrip_no_churn() {
    let dir = support::temp_data_dir("first-boot-cert-out");
    let db_path = dir.join("data.rdb");
    let db_path_str = db_path.display().to_string();
    let head_pw = dir.join("head.pw");
    let customer_pw = dir.join("customer.pw");
    std::fs::write(&head_pw, "head-secret\n").unwrap();
    std::fs::write(&customer_pw, "customer-secret\n").unwrap();
    let head_pw_s = head_pw.to_str().unwrap();
    let customer_pw_s = customer_pw.to_str().unwrap();
    let cert_out = dir.join("cert.pem");
    let cert_out_s = cert_out.to_str().unwrap();
    let port = free_port();
    let http_addr = format!("127.0.0.1:{port}");

    assert!(!db_path.exists(), "fresh volume precondition");
    assert!(
        !cert_out.exists(),
        "cert-out must not exist before first boot"
    );

    let mut cloud_with_cert_out =
        cloud_server_args(&db_path_str, &http_addr, head_pw_s, customer_pw_s);
    cloud_with_cert_out.push("--bootstrap-cert-out");
    cloud_with_cert_out.push(cert_out_s);

    // ---- First boot: mint the vault + write the certificate file. ----
    let stderr_path = dir.join("server.stderr");
    let mut server = spawn_server(&cloud_with_cert_out, &stderr_path);
    // The cert file is written during preset application, before listeners
    // come up; poll for it directly rather than probing the port.
    let wrote_cert = wait_for_file(&mut server, &cert_out, Duration::from_secs(30));
    let stderr = server.stderr();
    assert!(
        wrote_cert,
        "--bootstrap-cert-out file was not written.\nstderr:\n{stderr}"
    );
    assert!(
        wait_for_paged_vault(&db_path),
        "first boot must create the paged vault in place.\nstderr:\n{stderr}"
    );
    drop(server); // kill + reap

    // The certificate holds a usable 32-byte (64 hex chars) unseal cert.
    let cert_first = std::fs::read_to_string(&cert_out).expect("read cert-out");
    let cert_trimmed = cert_first.trim();
    assert_eq!(
        cert_trimmed.len(),
        64,
        "expected a 64-hex-char certificate, got {:?}",
        cert_trimmed
    );
    assert!(
        cert_trimmed.bytes().all(|b| b.is_ascii_hexdigit()),
        "certificate must be hex, got {:?}",
        cert_trimmed
    );
    let mtime_first = std::fs::metadata(&cert_out)
        .and_then(|m| m.modified())
        .expect("cert-out mtime");

    // ---- Round-trip: a fresh boot with NO bootstrap intent unseals the
    // existing vault from the written file via REDDB_CERTIFICATE_FILE. The
    // bootstrapped operational-directory vault requires the certificate on
    // every subsequent boot, so a clean serve here proves the file we wrote
    // is a usable unseal certificate. ----
    let stderr_path2 = dir.join("server2.stderr");
    let mut server2 = spawn_server_with_cert_file(
        &[
            "server",
            "--path",
            &db_path_str,
            "--vault",
            "true",
            "--http",
            "--http-bind",
            &http_addr,
        ],
        &stderr_path2,
        &cert_out,
    );
    let serving2 =
        wait_for_stderr_contains(&mut server2, "listener online", Duration::from_secs(30));
    let stderr2 = server2.stderr();
    assert!(
        serving2,
        "the written certificate must unseal via REDDB_CERTIFICATE_FILE on a subsequent boot.\nstderr:\n{stderr2}"
    );
    drop(server2);

    // ---- No churn: a re-boot against the existing vault — even with the
    // bootstrap intent and `--bootstrap-cert-out` re-passed — must NOT
    // rewrite the certificate file. The completion marker short-circuits
    // preset re-application before any cert is minted or written. The cert
    // env unseals the existing vault so the boot reaches that point. ----
    let stderr_path3 = dir.join("server3.stderr");
    let mut server3 = spawn_server_with_cert_file(&cloud_with_cert_out, &stderr_path3, &cert_out);
    let rebooted =
        wait_for_stderr_contains(&mut server3, "listener online", Duration::from_secs(30));
    let stderr3 = server3.stderr();
    assert!(
        rebooted,
        "re-boot against the existing vault must serve.\nstderr:\n{stderr3}"
    );
    drop(server3);

    let cert_second = std::fs::read_to_string(&cert_out).expect("read cert-out after reboot");
    assert_eq!(
        cert_first, cert_second,
        "re-boot must not rewrite the cert-out file (content churned)"
    );
    let mtime_second = std::fs::metadata(&cert_out)
        .and_then(|m| m.modified())
        .expect("cert-out mtime after reboot");
    assert_eq!(
        mtime_first, mtime_second,
        "re-boot must not rewrite the cert-out file (mtime churned)"
    );
}

/// Issue #1592 — first-boot flags and idempotency are documented in
/// `red server --help`.
#[test]
fn server_help_documents_first_boot_flags_and_idempotency() {
    let output = Command::new(red_binary())
        .args(["server", "--help"])
        .stdin(Stdio::null())
        .output()
        .expect("run red server --help");
    let help = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        help.contains("--bootstrap-cert-out"),
        "red server --help must document --bootstrap-cert-out.\nhelp:\n{help}"
    );
    assert!(
        help.contains("manifest-driven first boot"),
        "red server --help must explain manifest-driven first boot.\nhelp:\n{help}"
    );
    assert!(
        help.contains("idempotent"),
        "red server --help must document idempotent re-boot behavior.\nhelp:\n{help}"
    );
    assert!(
        help.contains("REDDB_CERTIFICATE_FILE"),
        "red server --help must connect cert capture to REDDB_CERTIFICATE_FILE.\nhelp:\n{help}"
    );
}
