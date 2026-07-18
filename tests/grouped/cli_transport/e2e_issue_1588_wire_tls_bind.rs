//! Regression coverage for issue #1588.
//!
//! `red server --wire-tls-bind <addr>` with no explicit plaintext
//! `--wire-bind` (and no gRPC/HTTP bind) must bring the RedWire-over-TLS
//! listener online. Before the fix the default router still claimed
//! port 5050 / the wire-TLS-only config had no runner at all, so the
//! TLS listener never bound — the process either collided with the
//! router (`Address already in use`) or aborted with
//! "at least one server bind address must be configured".
//!
//! These spawn the real `red` binary via `CARGO_BIN_EXE_red` so the
//! whole `build_server_config` → `run_configured_servers` boot path is
//! exercised end-to-end, and assert against process liveness (not a
//! bare port probe) so a stray listener cannot mask a failed boot.

#[path = "../../support/mod.rs"]
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

fn port_is_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
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

fn spawn_server(args: &[&str], stderr_path: &Path) -> ServerChild {
    let stderr_file = File::create(stderr_path).expect("create stderr file");
    let child = Command::new(red_binary())
        .args(args)
        // Opt into self-signed wire-TLS auto-generation (gated by
        // RED_WIRE_TLS_DEV, mirroring RED_HTTP_TLS_DEV). Inert unless
        // `--wire-tls-bind` is used without an explicit cert/key.
        .env("RED_WIRE_TLS_DEV", "1")
        .env_remove("REDDB_USERNAME")
        .env_remove("REDDB_PASSWORD")
        .env_remove("REDDB_VAULT")
        .env_remove("REDDB_AUTH")
        .env_remove("REDDB_REQUIRE_AUTH")
        .env_remove("REDDB_WIRE_BIND_ADDR")
        .env_remove("REDDB_BIND_ADDR")
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
/// connection to its listener succeeds. Returns `false` if the process
/// exits first (boot failed) — process liveness guards against a stray
/// listener masking a failed boot.
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

/// `--wire-tls-bind` with no explicit `--wire-bind` brings the
/// RedWire-over-TLS listener online — no router collision, no
/// "at least one server bind address must be configured" abort.
/// The TLS cert/key are auto-generated next to the data dir (the
/// listener opts into self-signed dev certs via `RED_WIRE_TLS_DEV=1`,
/// set by `spawn_server`).
#[test]
fn wire_tls_only_serves_without_router_collision() {
    let dir = support::temp_data_dir("issue-1588-wire-tls-only");
    let db_path = dir.join("data.rdb");
    let db_path_str = db_path.display().to_string();
    let port = free_port();
    let wire_tls_addr = format!("127.0.0.1:{port}");
    let stderr_path = dir.join("server.stderr");

    let mut server = spawn_server(
        &[
            "server",
            "--path",
            &db_path_str,
            "--wire-tls-bind",
            &wire_tls_addr,
            "--no-auth",
        ],
        &stderr_path,
    );

    let serving = wait_until_serving(&mut server, &wire_tls_addr, Duration::from_secs(30));
    let stderr = server.stderr();
    assert!(
        serving,
        "wire-tls-only server never came up on {wire_tls_addr}.\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("Address already in use"),
        "wire-tls listener must not collide with the default router.\nstderr:\n{stderr}"
    );
    assert!(
        !stderr.contains("at least one server bind address must be configured"),
        "wire-tls-bind alone must be a valid server configuration.\nstderr:\n{stderr}"
    );
}

/// Default-router behaviour with NO `--wire-tls-bind` is unchanged: a
/// flagless `red server` still claims the default router port (5050).
/// Skipped when 5050 is already occupied on the host so environmental
/// contention can never produce a false failure.
#[test]
fn default_router_unchanged_without_wire_tls() {
    if !port_is_free(5050) {
        eprintln!("skipping: default router port 5050 is busy on this host");
        return;
    }
    let dir = support::temp_data_dir("issue-1588-default-router");
    let db_path = dir.join("data.rdb");
    let db_path_str = db_path.display().to_string();
    let stderr_path = dir.join("server.stderr");

    let mut server = spawn_server(
        &["server", "--path", &db_path_str, "--no-auth"],
        &stderr_path,
    );

    let serving = wait_until_serving(&mut server, "127.0.0.1:5050", Duration::from_secs(30));
    let stderr = server.stderr();
    assert!(
        serving,
        "default-router server never came up on 127.0.0.1:5050.\nstderr:\n{stderr}"
    );
}
