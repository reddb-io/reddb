//! Integration tests for issue #572 slice 3: TLS HTTP accept loop
//! shares the `HttpConnectionLimiter` instance with the clear-text
//! accept loop, and inherits the per-handler total-time deadline.
//!
//! Coverage:
//!   * Cross-transport saturation: clear-text holds `cap` connections;
//!     a TLS connect attempt is closed immediately without TLS
//!     handshake. After clear-text drains, a fresh TLS connection
//!     completes the handshake and serves `/health/live` normally.
//!   * Per-handler deadline applies to TLS handlers: slow injection
//!     exceeding `handler_timeout` yields a 503 emitted over the TLS
//!     stream; the permit drops; a follow-up TLS request succeeds.

#[allow(dead_code)]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use reddb::server::tls::{build_server_config, HttpTlsConfig};
use reddb::server::RedDBServer;
use rustls::pki_types::{CertificateDer, ServerName};

fn write_self_signed(dir: &std::path::Path) -> (PathBuf, PathBuf, Vec<u8>) {
    use rcgen::{CertificateParams, KeyPair};
    let mut params = CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    params
        .subject_alt_names
        .push(rcgen::SanType::IpAddress(std::net::IpAddr::V4(
            std::net::Ipv4Addr::LOCALHOST,
        )));
    let key_pair = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key_pair).unwrap();
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();
    let cert_der = cert.der().to_vec();

    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    std::fs::write(&cert_path, &cert_pem).unwrap();
    std::fs::write(&key_path, &key_pem).unwrap();
    (cert_path, key_path, cert_der)
}

fn build_tls_config(label: &str) -> (support::TempDataDir, Arc<rustls::ServerConfig>, Vec<u8>) {
    let dir = support::temp_data_dir(&format!("http-tls-limiter-{label}"));
    let (cert_path, key_path, der) = write_self_signed(&dir);
    let cfg = HttpTlsConfig {
        cert_path,
        key_path,
        client_ca_path: None,
    };
    let server_config = build_server_config(&cfg).expect("server config builds");
    (dir, server_config, der)
}

fn client_config_trusting(cert_der: &[u8]) -> Arc<rustls::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(cert_der.to_vec())).unwrap();
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(client_config)
}

/// Clear-text `GET /health/live` over a raw TCP socket. Returns the
/// raw response string, or a `<no-body ...>` marker if the limiter
/// closed the socket before/while writing (mirrors `tls_get_health`).
fn http_get_health(addr: &str) -> String {
    let mut tcp = match TcpStream::connect(addr) {
        Ok(s) => s,
        Err(e) => return format!("<connect-err {e:?}>"),
    };
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let req = b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let write_res = tcp.write_all(req).and_then(|_| tcp.flush());
    let mut buf = Vec::new();
    let read_res = tcp.read_to_end(&mut buf);
    let body = String::from_utf8_lossy(&buf).to_string();
    if body.is_empty() {
        format!(
            "<no-body write={:?} read={:?}>",
            write_res.is_err(),
            read_res.is_err()
        )
    } else {
        body
    }
}

fn spawn_http_get(addr: String) -> thread::JoinHandle<String> {
    thread::spawn(move || http_get_health(&addr))
}

fn wait_for_limiter(server: &RedDBServer, target: usize) -> bool {
    for _ in 0..150 {
        if server.http_limiter().current() == target {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    server.http_limiter().current() == target
}

fn tls_get_health(addr: &str, client_cfg: Arc<rustls::ClientConfig>) -> String {
    let server_name: ServerName<'static> = ServerName::try_from("localhost").unwrap();
    let mut conn = rustls::ClientConnection::new(client_cfg, server_name).unwrap();
    let mut tcp = TcpStream::connect(addr).expect("tcp connect");
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut stream = rustls::Stream::new(&mut conn, &mut tcp);
    let req = b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let write_res = stream.write_all(req).and_then(|_| stream.flush());
    let mut buf = Vec::new();
    let read_res = stream.read_to_end(&mut buf);
    // Surface both results into the body string for the assertion in
    // the rejection test — a closed raw socket either errors on write
    // or returns an empty body.
    let body = String::from_utf8_lossy(&buf).to_string();
    if body.is_empty() {
        format!(
            "<no-body write={:?} read={:?}>",
            write_res.is_err(),
            read_res.is_err()
        )
    } else {
        body
    }
}

fn spawn_tls_get(
    addr: String,
    client_cfg: Arc<rustls::ClientConfig>,
) -> thread::JoinHandle<String> {
    thread::spawn(move || tls_get_health(&addr, client_cfg))
}

#[test]
fn tls_rejection_when_clear_text_saturates_then_recovers() {
    let cap = 2;
    let (_dir, tls_cfg, cert_der) = build_tls_config("cross");

    // Boot a single RedDBServer with one shared limiter, then bind two
    // listeners on it: clear-text and TLS.
    let (_db, runtime) = support::persistent_runtime("http-tls-limiter-cross");
    let server = RedDBServer::new(runtime).with_http_limiter_cap(cap);

    let clear_listener = TcpListener::bind("127.0.0.1:0").expect("bind clear");
    let clear_addr = clear_listener.local_addr().unwrap().to_string();
    let tls_listener = TcpListener::bind("127.0.0.1:0").expect("bind tls");
    let tls_addr = tls_listener.local_addr().unwrap().to_string();

    let s1 = server.clone();
    thread::spawn(move || {
        let _ = s1.serve_on(clear_listener);
    });
    let s2 = server.clone();
    let cfg_arc = Arc::clone(&tls_cfg);
    thread::spawn(move || {
        let _ = s2.serve_tls_on(tls_listener, cfg_arc);
    });
    thread::sleep(Duration::from_millis(120));

    // Saturate the shared cap via clear-text requests. The async edge
    // limits in-flight requests, not idle TCP connections, so the test
    // slow-inject hook keeps each request occupying a permit.
    server.set_test_slow_inject_ms(2_000);
    let mut held = Vec::new();
    for _ in 0..cap {
        held.push(spawn_http_get(clear_addr.clone()));
    }
    assert!(
        wait_for_limiter(&server, cap),
        "clear-text requests must saturate the shared cap, current={}",
        server.http_limiter().current()
    );

    // Now attempt a TLS request — it must be rejected by the shared
    // in-flight request cap.
    let client_cfg = client_config_trusting(&cert_der);
    let body = tls_get_health(&tls_addr, Arc::clone(&client_cfg));
    assert!(
        body.starts_with("HTTP/1.1 503"),
        "TLS request must be rejected while cap is saturated, got: {body:?}"
    );
    assert!(
        body.to_ascii_lowercase().contains("retry-after: 5"),
        "expected Retry-After header, got: {body:?}"
    );

    // Drain the held clear-text requests.
    server.set_test_slow_inject_ms(0);
    for handle in held {
        let body = handle.join().expect("held clear-text request");
        assert!(
            body.starts_with("HTTP/1.1 2"),
            "held clear-text request should complete normally, got: {body:?}"
        );
    }
    assert!(
        wait_for_limiter(&server, 0),
        "permits should drain back to zero after clear-text requests finish, current={}",
        server.http_limiter().current()
    );
    thread::sleep(Duration::from_millis(100));

    // Fresh TLS request now completes the handshake and serves.
    let body2 = tls_get_health(&tls_addr, client_cfg);
    assert!(
        body2.starts_with("HTTP/1.1 2"),
        "TLS request must succeed after cap drains, got: {body2:?}"
    );
}

#[test]
fn tls_handler_deadline_emits_503_then_recovers() {
    let handler_timeout = Duration::from_millis(200);
    let inject_ms: u64 = 500;
    let slack = Duration::from_millis(2_000);

    let (_dir, tls_cfg, cert_der) = build_tls_config("deadline");
    let (_db, runtime) = support::persistent_runtime("http-tls-limiter-deadline");
    let server = RedDBServer::new(runtime).with_handler_timeout(handler_timeout);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind tls");
    let addr = listener.local_addr().unwrap().to_string();
    let server_clone = server.clone();
    let cfg_arc = Arc::clone(&tls_cfg);
    thread::spawn(move || {
        let _ = server_clone.serve_tls_on(listener, cfg_arc);
    });
    thread::sleep(Duration::from_millis(100));

    server.set_test_slow_inject_ms(inject_ms);

    let client_cfg = client_config_trusting(&cert_der);
    let start = Instant::now();
    let body = tls_get_health(&addr, Arc::clone(&client_cfg));
    let elapsed = start.elapsed();

    assert!(
        body.starts_with("HTTP/1.1 503"),
        "expected 503 over TLS, got: {body:?}"
    );
    let body_lower = body.to_ascii_lowercase();
    assert!(
        body_lower.contains("connection: close"),
        "expected Connection: close, got: {body:?}"
    );
    assert!(
        !body_lower.contains("retry-after:"),
        "deadline-503 should not carry Retry-After, got: {body:?}"
    );
    assert!(
        elapsed <= Duration::from_millis(inject_ms) + slack,
        "TLS handler exit too slow: {elapsed:?}"
    );

    for _ in 0..100 {
        if server.http_limiter().current() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        server.http_limiter().current(),
        0,
        "permit should drop when TLS handler exits"
    );

    server.set_test_slow_inject_ms(0);
    let body2 = tls_get_health(&addr, client_cfg);
    assert!(
        body2.starts_with("HTTP/1.1 2"),
        "subsequent TLS request should succeed, got: {body2:?}"
    );
}

/// AC #4 (vice-versa + mix): one shared cap, saturated by a *mixture*
/// of clear-text and TLS connections. While saturated, a further
/// request on *either* transport is rejected — proving HTTP and HTTPS
/// draw against the single limiter regardless of which transport
/// filled it. After the holders drain, capacity is restored on *both*
/// transports.
#[test]
fn mixed_http_https_saturation_rejects_both_transports_then_recovers() {
    let cap = 2;
    let (_dir, tls_cfg, cert_der) = build_tls_config("mixed");

    let (_db, runtime) = support::persistent_runtime("http-tls-limiter-mixed");
    let server = RedDBServer::new(runtime).with_http_limiter_cap(cap);

    let clear_listener = TcpListener::bind("127.0.0.1:0").expect("bind clear");
    let clear_addr = clear_listener.local_addr().unwrap().to_string();
    let tls_listener = TcpListener::bind("127.0.0.1:0").expect("bind tls");
    let tls_addr = tls_listener.local_addr().unwrap().to_string();

    let s1 = server.clone();
    thread::spawn(move || {
        let _ = s1.serve_on(clear_listener);
    });
    let s2 = server.clone();
    let cfg_arc = Arc::clone(&tls_cfg);
    thread::spawn(move || {
        let _ = s2.serve_tls_on(tls_listener, cfg_arc);
    });
    thread::sleep(Duration::from_millis(120));

    let client_cfg = client_config_trusting(&cert_der);

    // Saturate the shared cap with a MIX: one clear-text request + one
    // TLS request. Both occupy the shared in-flight request limiter
    // while the test slow-inject hook sleeps.
    server.set_test_slow_inject_ms(2_000);
    let hold_http = spawn_http_get(clear_addr.clone());
    let hold_https = spawn_tls_get(tls_addr.clone(), Arc::clone(&client_cfg));
    assert!(
        wait_for_limiter(&server, cap),
        "a mix of one HTTP + one HTTPS request must saturate the shared cap, current={}",
        server.http_limiter().current()
    );

    // Cap is full (filled by both transports). A new clear-text request
    // is rejected with the limiter 503...
    let http_body = http_get_health(&clear_addr);
    assert!(
        http_body.starts_with("HTTP/1.1 503"),
        "clear-text request must be rejected while shared cap is saturated, got: {http_body:?}"
    );
    assert!(
        http_body.to_ascii_lowercase().contains("retry-after: 5"),
        "expected Retry-After header, got: {http_body:?}"
    );

    // ...and a new TLS request is rejected by the same shared cap.
    let https_body = tls_get_health(&tls_addr, Arc::clone(&client_cfg));
    assert!(
        https_body.starts_with("HTTP/1.1 503"),
        "TLS request must be rejected while shared cap is saturated, got: {https_body:?}"
    );
    assert!(
        https_body.to_ascii_lowercase().contains("retry-after: 5"),
        "expected Retry-After header, got: {https_body:?}"
    );

    // Drain both holders; permits return to zero.
    server.set_test_slow_inject_ms(0);
    let http_done = hold_http.join().expect("held HTTP request");
    assert!(
        http_done.starts_with("HTTP/1.1 2"),
        "held HTTP request should complete normally, got: {http_done:?}"
    );
    let https_done = hold_https.join().expect("held HTTPS request");
    assert!(
        https_done.starts_with("HTTP/1.1 2"),
        "held HTTPS request should complete normally, got: {https_done:?}"
    );
    assert!(
        wait_for_limiter(&server, 0),
        "permits should drain to zero after both holders close, current={}",
        server.http_limiter().current()
    );
    thread::sleep(Duration::from_millis(100));

    // Capacity restored on BOTH transports.
    let http_ok = http_get_health(&clear_addr);
    assert!(
        http_ok.starts_with("HTTP/1.1 2"),
        "clear-text must recover after the shared cap drains, got: {http_ok:?}"
    );
    let https_ok = tls_get_health(&tls_addr, client_cfg);
    assert!(
        https_ok.starts_with("HTTP/1.1 2"),
        "TLS must recover after the shared cap drains, got: {https_ok:?}"
    );
}
