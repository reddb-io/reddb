//! Real CLI composition check for the documented no-auth primary/replica path.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

fn free_ports(count: usize) -> Vec<u16> {
    let mut listeners = Vec::with_capacity(count);
    for _ in 0..count {
        listeners.push(TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port"));
    }
    let ports = listeners
        .iter()
        .map(|listener| listener.local_addr().expect("local address").port())
        .collect();
    drop(listeners);
    ports
}

struct ServerChild(Child);

impl ServerChild {
    fn has_exited(&mut self) -> bool {
        self.0.try_wait().expect("poll child").is_some()
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_primary(path: &Path, http_port: u16, grpc_port: u16) -> ServerChild {
    ServerChild(
        Command::new(red_binary())
            .args([
                "server",
                "--path",
                path.to_str().expect("utf-8 primary path"),
                "--grpc",
                "--http",
                "--grpc-bind",
                &format!("127.0.0.1:{grpc_port}"),
                "--http-bind",
                &format!("127.0.0.1:{http_port}"),
                "--role",
                "primary",
                "--storage-preset",
                "primary-replica-dev",
                "--no-auth",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn primary"),
    )
}

fn spawn_replica(path: &Path, http_port: u16, primary_grpc_port: u16) -> ServerChild {
    ServerChild(
        Command::new(red_binary())
            .args([
                "replica",
                "--path",
                path.to_str().expect("utf-8 replica path"),
                "--primary-addr",
                &format!("http://127.0.0.1:{primary_grpc_port}"),
                "--http",
                "--http-bind",
                &format!("127.0.0.1:{http_port}"),
                "--storage-preset",
                "primary-replica-dev",
                "--no-auth",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn replica"),
    )
}

fn http_request(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect HTTP server");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("write timeout");
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(body) = body {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ));
    } else {
        request.push_str("\r\n");
    }
    stream.write_all(request.as_bytes()).expect("write request");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).expect("read response");
    let response = String::from_utf8_lossy(&response);
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .unwrap_or_default()
        .to_string();
    (status, body)
}

fn wait_for_health(child: &mut ServerChild, port: u16, name: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        assert!(!child.has_exited(), "{name} exited before becoming healthy");
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = stream
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            if response.starts_with("HTTP/1.1 200") {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{name} did not become healthy on port {port}");
}

#[test]
fn cli_no_auth_primary_replicates_to_read_replica() {
    let temp = support::temp_data_dir("cli-primary-replica");
    let ports = free_ports(3);
    let (primary_http, primary_grpc, replica_http) = (ports[0], ports[1], ports[2]);
    let mut primary = spawn_primary(&temp.join("primary.rdb"), primary_http, primary_grpc);
    wait_for_health(&mut primary, primary_http, "primary");

    for query in [
        "CREATE TABLE repl_probe (id INTEGER, name TEXT)",
        "INSERT INTO repl_probe (id, name) VALUES (1, 'backlog')",
    ] {
        let body = format!(r#"{{"query":"{query}"}}"#);
        let (status, response) = http_request(primary_http, "POST", "/query", Some(&body));
        assert_eq!(status, 200, "primary query failed: {response}");
    }

    let mut replica = spawn_replica(&temp.join("replica.rdb"), replica_http, primary_grpc);
    wait_for_health(&mut replica, replica_http, "replica");

    let healthy_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(!replica.has_exited(), "replica exited while connecting");
        let (status, body) = http_request(replica_http, "GET", "/replication/status", None);
        if status == 200 && body.contains("\"state\":\"healthy\"") {
            break;
        }
        assert!(
            Instant::now() < healthy_deadline,
            "replica did not become healthy: status={status} body={body}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let visible_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(!replica.has_exited(), "replica exited while applying WAL");
        let (status, body) = http_request(
            replica_http,
            "POST",
            "/query",
            Some(r#"{"query":"SELECT * FROM repl_probe WHERE id = 1"}"#),
        );
        if status == 200 && body.contains("backlog") {
            break;
        }
        assert!(
            Instant::now() < visible_deadline,
            "replicated row did not become visible: status={status} body={body}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let (status, response) = http_request(
        primary_http,
        "POST",
        "/query",
        Some(r#"{"query":"INSERT INTO repl_probe (id, name) VALUES (2, 'live')"}"#),
    );
    assert_eq!(status, 200, "primary query failed: {response}");

    let live_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(!replica.has_exited(), "replica exited while tailing WAL");
        let (status, body) = http_request(
            replica_http,
            "POST",
            "/query",
            Some(r#"{"query":"SELECT * FROM repl_probe WHERE id = 2"}"#),
        );
        if status == 200 && body.contains("live") {
            break;
        }
        assert!(
            Instant::now() < live_deadline,
            "live row did not become visible: status={status} body={body}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let (status, body) = http_request(primary_http, "GET", "/replication/status", None);
    assert_eq!(status, 200, "primary replication status failed: {body}");
    assert!(
        body.contains("\"replica_count\":1"),
        "primary did not retain the replica registration: {body}"
    );
}
