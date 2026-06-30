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

/// The paged operational-directory layout writes a `<path>-hdr` sidecar
/// next to the main file; the embedded single-file layout does not. Its
/// presence is therefore a reliable marker that the paged vault was
/// created in place.
fn paged_layout_marker(db_path: &Path) -> PathBuf {
    let mut name = db_path.file_name().unwrap().to_os_string();
    name.push("-hdr");
    db_path.with_file_name(name)
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
        paged_layout_marker(&db_path).exists(),
        "expected paged vault created in place ({} missing).\nstderr:\n{stderr}",
        paged_layout_marker(&db_path).display()
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
    // It must NOT have silently created a paged vault.
    assert!(
        !paged_layout_marker(&db_path).exists(),
        "no-intent boot must not self-create a paged vault"
    );
}
