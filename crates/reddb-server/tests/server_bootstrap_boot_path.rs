//! Issue #2133 — the real boot path, not a re-drive of its descriptors.
//!
//! The per-shape descriptor tests in `tests/grouped/cli_transport/` assert
//! what `transport_set` produces and that each descriptor binds. Nothing there
//! calls `bootstrap_server` / `BootedNode::run`, so the boot path itself could
//! regress — losing the readiness log line orchestrators grep for — without a
//! single test turning red. This drives the real path for the HTTP shape and
//! waits for that line.
//!
//! It owns its own test binary so the global tracing subscriber it installs to
//! capture the line cannot collide with another test's.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use reddb_server::service_cli::{bootstrap_server, BootstrapConfig, ServerCommandConfig};
use reddb_server::storage::StorageProfileSelection;

/// `tracing_subscriber` writer that appends every formatted event to a buffer
/// the test thread can read.
#[derive(Clone)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_global_logs() -> Arc<Mutex<Vec<u8>>> {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let writer = CapturedLog(buffer.clone());
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("this test binary installs the only global subscriber");
    buffer
}

fn http_only_config(data_path: PathBuf) -> ServerCommandConfig {
    ServerCommandConfig {
        path: Some(data_path),
        router_bind_addr: None,
        router_bind_explicit: false,
        grpc_bind_addr: None,
        grpc_bind_explicit: false,
        grpc_tls_bind_addr: None,
        grpc_tls_cert: None,
        grpc_tls_key: None,
        grpc_tls_client_ca: None,
        // Port 0: the OS picks a free port, so the test never races another
        // listener for a fixed one.
        http_bind_addr: Some("127.0.0.1:0".to_string()),
        http_bind_explicit: true,
        http_tls_bind_addr: None,
        http_tls_cert: None,
        http_tls_key: None,
        http_tls_client_ca: None,
        wire_bind_addr: None,
        wire_bind_explicit: false,
        wire_tls_bind_addr: None,
        wire_tls_cert: None,
        wire_tls_key: None,
        pg_bind_addr: None,
        create_if_missing: true,
        read_only: false,
        role: "standalone".to_string(),
        primary_addr: None,
        storage_profile: StorageProfileSelection::embedded_single_file(),
        auth: false,
        require_auth: false,
        vault: false,
        no_auth: false,
        workers: Some(1),
        telemetry: None,
        http_limits_cli: reddb_server::server::HttpLimitsCliInput::default(),
        ui: false,
        ui_dir: None,
        bootstrap: BootstrapConfig::default(),
    }
}

/// The HTTP shape must reach `transport="http" … listener online` through
/// `bootstrap_server` + `BootedNode::run`. `tests/cli_first_boot.rs` greps that
/// message as a readiness probe, so it is an operator contract, not a detail.
#[test]
fn http_shape_boot_path_logs_listener_online() {
    let logs = capture_global_logs();
    let dir = std::env::temp_dir().join(format!("reddb-boot-path-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp data dir");
    let config = http_only_config(dir.join("data.rdb"));

    // `run` only returns when the node is done, so the server owns a thread and
    // the test polls the captured log.
    let node = thread::spawn(move || bootstrap_server(config).and_then(|node| node.run()));

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut online = false;
    while Instant::now() < deadline {
        let captured = String::from_utf8_lossy(&logs.lock().expect("log buffer")).to_string();
        if captured.contains("listener online") && captured.contains("transport=\"http\"") {
            online = true;
            break;
        }
        if node.is_finished() {
            let outcome = node.join().expect("server thread");
            panic!("the HTTP boot path exited before logging readiness: {outcome:?}\n{captured}");
        }
        thread::sleep(Duration::from_millis(50));
    }

    let captured = String::from_utf8_lossy(&logs.lock().expect("log buffer")).to_string();
    assert!(
        online,
        "the HTTP shape's boot path must log its readiness line.\ncaptured:\n{captured}"
    );

    // The node has no shutdown hook short of a signal; it dies with the test
    // process. Clean up what we can.
    let _ = std::fs::remove_dir_all(&dir);
}
