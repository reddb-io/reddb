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

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use reddb::server::tls::{build_server_config, HttpTlsConfig};
use reddb::server::RedDBServer;
use reddb::{RedDBOptions, RedDBRuntime};
use rustls::pki_types::{CertificateDer, ServerName};

fn tmpdir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "reddb-http-tls-limiter-test-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

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

fn build_tls_config(label: &str) -> (Arc<rustls::ServerConfig>, Vec<u8>) {
    let dir = tmpdir(label);
    let (cert_path, key_path, der) = write_self_signed(&dir);
    let cfg = HttpTlsConfig {
        cert_path,
        key_path,
        client_ca_path: None,
    };
    let server_config = build_server_config(&cfg).expect("server config builds");
    (server_config, der)
}

fn client_config_trusting(cert_der: &[u8]) -> Arc<rustls::ClientConfig> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(cert_der.to_vec()))
        .unwrap();
    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(client_config)
}

fn tls_get_health(addr: &str, client_cfg: Arc<rustls::ClientConfig>) -> String {
    let server_name: ServerName<'static> = ServerName::try_from("localhost").unwrap();
    let mut conn = rustls::ClientConnection::new(client_cfg, server_name).unwrap();
    let mut tcp = TcpStream::connect(addr).expect("tcp connect");
    tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut stream = rustls::Stream::new(&mut conn, &mut tcp);
    let req =
        b"GET /health/live HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
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

#[test]
fn tls_rejection_when_clear_text_saturates_then_recovers() {
    let cap = 2;
    let (tls_cfg, cert_der) = build_tls_config("cross");

    // Boot a single RedDBServer with one shared limiter, then bind two
    // listeners on it: clear-text and TLS.
    let opts = RedDBOptions::in_memory();
    let runtime = RedDBRuntime::with_options(opts).expect("runtime");
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

    // Saturate the cap via clear-text — `cap` open connections that
    // never send a request; handler threads park on the read timeout.
    let mut held: Vec<TcpStream> = Vec::new();
    for _ in 0..cap {
        held.push(TcpStream::connect(&clear_addr).expect("hold"));
    }
    for _ in 0..50 {
        if server.http_limiter().current() == cap {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        server.http_limiter().current(),
        cap,
        "clear-text must saturate the shared cap"
    );

    // Now attempt a TLS connection — the accept loop must close the
    // raw socket before any handshake. The rustls client either fails
    // the handshake or sees EOF before bytes arrive. Either way: no
    // HTTP 200, no clean response body.
    let client_cfg = client_config_trusting(&cert_der);
    let body = tls_get_health(&tls_addr, Arc::clone(&client_cfg));
    assert!(
        !body.starts_with("HTTP/1.1 200"),
        "TLS request must be rejected while cap is saturated, got: {body:?}"
    );

    // Drain the held clear-text connections.
    for s in held.drain(..) {
        let _ = s.shutdown(std::net::Shutdown::Both);
        drop(s);
    }
    for _ in 0..100 {
        if server.http_limiter().current() == 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        server.http_limiter().current(),
        0,
        "permits should drain back to zero after clear-text close"
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

    let (tls_cfg, cert_der) = build_tls_config("deadline");
    let opts = RedDBOptions::in_memory();
    let runtime = RedDBRuntime::with_options(opts).expect("runtime");
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
    assert!(
        body.contains("Connection: close"),
        "expected Connection: close, got: {body:?}"
    );
    assert!(
        !body.contains("Retry-After:"),
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
