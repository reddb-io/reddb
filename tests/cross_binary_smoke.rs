//! Cross-binary smoke: boot the full `red` binary and drive
//! `red_client` against it. Pinned to gRPC plain for now — the
//! TLS variants and the HTTP/RedWire transports land when the
//! red_client connector grows past gRPC-only.
//!
//! Follows the prior-art subprocess pattern in `tests/cli_bootstrap.rs`.
//! Both binaries are reached through Cargo's `CARGO_BIN_EXE_*`
//! env vars so the test never duplicates path knowledge.

use std::io::{ErrorKind, Read};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

// `pick_port()` returns a port that is briefly free, but the OS may
// hand the same port to a sibling test thread (or another process)
// before the spawned `red` child has bind()'d to it. On fast runners
// where multiple smoke tests boot servers concurrently, that
// TOCTOU race makes `red_client` see "Connection refused" / "transport
// error" against a port owned by a different test's listener (or no
// listener at all). Serialise the boot-and-handshake critical section
// across the tests in this file so each pick/spawn/wait cycle runs to
// completion before the next one starts.
static BOOT_LOCK: Mutex<()> = Mutex::new(());

fn lock_boot() -> MutexGuard<'static, ()> {
    // A panicking sibling test poisons the mutex; recover the guard
    // so the remaining tests still run and surface their own outcome.
    BOOT_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

fn red_client_binary() -> PathBuf {
    // CARGO_BIN_EXE_<name> is only set for binaries in the *same*
    // package as the test. red_client lives in the sibling
    // `reddb-client` crate, so we derive its path from the
    // `red` binary's location: both land in the same workspace
    // target/<profile>/ directory.
    let red = PathBuf::from(env!("CARGO_BIN_EXE_red"));
    let dir = red.parent().expect("red bin has no parent dir");
    let mut path = dir.join("red_client");
    if cfg!(windows) {
        path.set_extension("exe");
    }
    path
}

fn pick_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn scratch_path(label: &str) -> PathBuf {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "reddb-cross-smoke-{}-{}-{}",
        label,
        std::process::id(),
        now_ns
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("data.rdb")
}

/// Block until `host:port` accepts a TCP connection or the deadline
/// elapses. Returns true when the listener is up.
fn wait_for_listener(host: &str, port: u16, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        match TcpStream::connect_timeout(
            &format!("{host}:{port}").parse().unwrap(),
            Duration::from_millis(200),
        ) {
            Ok(_) => return true,
            Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    false
}

/// Drains stdout/stderr from a server child process so its pipes
/// don't fill and stall the boot sequence.
fn drain_output(child: &mut Child) -> (String, String) {
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut out);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut err);
    }
    (out, err)
}

struct ServerHandle {
    child: Child,
}

impl ServerHandle {
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.kill();
    }
}

fn boot_red_grpc(port: u16) -> Result<ServerHandle, String> {
    let path = scratch_path("grpc");
    let bind = format!("127.0.0.1:{port}");
    let mut cmd = Command::new(red_binary());
    cmd.args([
        "server",
        "--grpc",
        "--grpc-bind",
        &bind,
        "--path",
        path.to_str().unwrap(),
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn red server: {e}"))?;
    Ok(ServerHandle { child })
}

fn boot_red_http(port: u16) -> Result<ServerHandle, String> {
    let path = scratch_path("http");
    let bind = format!("127.0.0.1:{port}");
    let grpc_port = pick_port();
    let grpc_bind = format!("127.0.0.1:{grpc_port}");
    let mut cmd = Command::new(red_binary());
    cmd.args([
        "server",
        "--http",
        "--http-bind",
        &bind,
        "--grpc-bind",
        &grpc_bind,
        "--path",
        path.to_str().unwrap(),
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn red server: {e}"))?;
    Ok(ServerHandle { child })
}

fn boot_red_wire(port: u16) -> Result<ServerHandle, String> {
    let path = scratch_path("wire");
    let bind = format!("127.0.0.1:{port}");
    // The server boots a default-port gRPC listener even when only
    // --wire-bind is requested. Pin it to a free port so parallel
    // smoke tests don't collide on 5055.
    let grpc_port = pick_port();
    let grpc_bind = format!("127.0.0.1:{grpc_port}");
    let mut cmd = Command::new(red_binary());
    cmd.args([
        "server",
        "--wire-bind",
        &bind,
        "--grpc-bind",
        &grpc_bind,
        "--path",
        path.to_str().unwrap(),
    ])
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn red server: {e}"))?;
    Ok(ServerHandle { child })
}

/// Skip helper: emit a message and return early when red_client
/// hasn't been built yet (workspace test runs without
/// `--bin red_client` won't build it).
fn skip_if_no_red_client() -> Option<PathBuf> {
    let path = red_client_binary();
    if !path.exists() {
        eprintln!(
            "red_client binary not found at {}; skipping cross-binary smoke test. \
             Build with `cargo build --bin red_client -p reddb-client` first.",
            path.display()
        );
        return None;
    }
    Some(path)
}

#[test]
fn red_client_round_trips_against_red_over_grpc() {
    let Some(red_client) = skip_if_no_red_client() else {
        return;
    };
    let _guard = lock_boot();
    let port = pick_port();
    let mut server = match boot_red_grpc(port) {
        Ok(h) => h,
        Err(e) => panic!("could not start `red server`: {e}"),
    };

    if !wait_for_listener("127.0.0.1", port, Duration::from_secs(15)) {
        let (out, err) = drain_output(&mut server.child);
        panic!(
            "`red server` did not listen on {port} within 15s\nstdout:\n{out}\nstderr:\n{err}"
        );
    }

    let uri = format!("grpc://127.0.0.1:{port}");
    let out = Command::new(&red_client)
        .args([&uri, "-c", "SELECT 1"])
        .output()
        .expect("spawn red_client");

    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    server.kill();

    assert_eq!(
        code, 0,
        "red_client should exit 0 on a successful round-trip; \
         got {code}\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "red_client should print a result for `SELECT 1`; stdout was empty.\
         \nstderr: {stderr}"
    );
}

#[test]
fn red_client_round_trips_against_red_over_redwire() {
    let Some(red_client) = skip_if_no_red_client() else {
        return;
    };
    let _guard = lock_boot();
    let port = pick_port();
    let mut server = match boot_red_wire(port) {
        Ok(h) => h,
        Err(e) => panic!("could not start `red server` (wire): {e}"),
    };

    if !wait_for_listener("127.0.0.1", port, Duration::from_secs(15)) {
        let (out, err) = drain_output(&mut server.child);
        panic!(
            "`red server --wire-bind` did not listen on {port} within 15s\nstdout:\n{out}\nstderr:\n{err}"
        );
    }

    let uri = format!("red://127.0.0.1:{port}");
    let out = Command::new(&red_client)
        .args([&uri, "-c", "SELECT 1"])
        .output()
        .expect("spawn red_client");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    server.kill();

    assert_eq!(
        code, 0,
        "red_client over RedWire should exit 0; got {code}\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "red_client should print a result; stdout empty.\nstderr: {stderr}"
    );
}

#[test]
fn red_client_round_trips_against_red_over_http() {
    let Some(red_client) = skip_if_no_red_client() else {
        return;
    };
    let _guard = lock_boot();
    let port = pick_port();
    let mut server = match boot_red_http(port) {
        Ok(h) => h,
        Err(e) => panic!("could not start `red server` (http): {e}"),
    };

    if !wait_for_listener("127.0.0.1", port, Duration::from_secs(15)) {
        let (out, err) = drain_output(&mut server.child);
        panic!(
            "`red server --http` did not listen on {port} within 15s\nstdout:\n{out}\nstderr:\n{err}"
        );
    }

    let uri = format!("http://127.0.0.1:{port}");
    let out = Command::new(&red_client)
        .args([&uri, "-c", "SELECT 1"])
        .output()
        .expect("spawn red_client");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    server.kill();

    assert_eq!(
        code, 0,
        "red_client over HTTP should exit 0; got {code}\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.trim().is_empty(),
        "red_client should print a result; stdout empty.\nstderr: {stderr}"
    );
}

#[test]
fn red_client_reds_scheme_returns_tls_not_implemented() {
    // `reds://` is parsed correctly but the connector does not yet
    // wire TLS — surface the documented "not yet implemented" exit
    // code (5) instead of crashing or hanging on a TCP connect.
    let Some(red_client) = skip_if_no_red_client() else {
        return;
    };
    let out = Command::new(&red_client)
        .args(["reds://127.0.0.1:1", "-c", "SELECT 1"])
        .output()
        .expect("spawn red_client");
    let code = out.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 5,
        "reds:// should hit transport-not-implemented (5); got {code}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("TLS") || stderr.contains("reds"),
        "stderr should mention TLS / reds; got: {stderr}"
    );
}

#[test]
fn red_client_red_scheme_routes_to_default_port() {
    let Some(red_client) = skip_if_no_red_client() else {
        return;
    };
    // `red://host` (no explicit port) must inherit DEFAULT_PORT_RED
    // (5050). Boot a RedWire listener on 5050 and confirm.
    // Skipped when 5050 is already taken on this host.
    let port = 5050u16;
    let probe = std::net::TcpListener::bind(("127.0.0.1", port));
    if probe.is_err() {
        eprintln!("port 5050 already bound — skipping default-port smoke");
        return;
    }
    drop(probe);

    let _guard = lock_boot();
    let mut server = match boot_red_wire(port) {
        Ok(h) => h,
        Err(e) => panic!("could not start `red server` (wire): {e}"),
    };

    if !wait_for_listener("127.0.0.1", port, Duration::from_secs(15)) {
        let (out, err) = drain_output(&mut server.child);
        panic!(
            "`red server` did not listen on {port} within 15s\nstdout:\n{out}\nstderr:\n{err}"
        );
    }

    let out = Command::new(&red_client)
        .args(["red://127.0.0.1", "-c", "SELECT 1"])
        .output()
        .expect("spawn red_client");

    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);

    server.kill();

    assert_eq!(
        code, 0,
        "red_client red:// (default port 5050) should exit 0; \
         got {code}\nstdout: {stdout}\nstderr: {stderr}"
    );
}
