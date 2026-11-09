//! Real CLI composition check for the documented no-auth primary/replica path.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PROCESS_READY_TIMEOUT: Duration = Duration::from_secs(60);
const REPLICATION_TIMEOUT: Duration = Duration::from_secs(30);

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

struct ServerChild {
    child: Child,
    output_path: PathBuf,
}

impl ServerChild {
    fn has_exited(&mut self) -> bool {
        self.child.try_wait().expect("poll child").is_some()
    }

    fn stop_and_read_output(&mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::fs::read_to_string(&self.output_path).unwrap_or_default()
    }
}

impl Drop for ServerChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_primary(path: &Path, http_port: u16, grpc_port: u16) -> ServerChild {
    let output_path = path.with_extension("log");
    let output = File::create(&output_path).expect("create primary output log");
    let child = Command::new(red_binary())
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
        .stdout(output.try_clone().expect("clone primary output log"))
        .stderr(output)
        .spawn()
        .expect("spawn primary");
    ServerChild { child, output_path }
}

fn spawn_replica(
    path: &Path,
    http_port: u16,
    grpc_port: u16,
    primary_grpc_port: u16,
) -> ServerChild {
    let output_path = path.with_extension("log");
    let output = File::create(&output_path).expect("create replica output log");
    let child = Command::new(red_binary())
        .args([
            "replica",
            "--path",
            path.to_str().expect("utf-8 replica path"),
            "--primary-addr",
            &format!("http://127.0.0.1:{primary_grpc_port}"),
            "--grpc",
            "--grpc-bind",
            &format!("127.0.0.1:{grpc_port}"),
            "--http",
            "--http-bind",
            &format!("127.0.0.1:{http_port}"),
            "--storage-preset",
            "primary-replica-dev",
            "--no-auth",
        ])
        .stdin(Stdio::null())
        .stdout(output.try_clone().expect("clone replica output log"))
        .stderr(output)
        .spawn()
        .expect("spawn replica");
    ServerChild { child, output_path }
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
    let deadline = Instant::now() + PROCESS_READY_TIMEOUT;
    let mut last_status_line = String::new();
    while Instant::now() < deadline {
        if child.has_exited() {
            let output = child.stop_and_read_output();
            panic!("{name} exited before becoming healthy; output:\n{output}");
        }
        if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = stream
                .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            let mut status_line = String::new();
            let _ = BufReader::new(stream).read_line(&mut status_line);
            if status_line.starts_with("HTTP/1.1 200") {
                return;
            }
            last_status_line = status_line;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = child.stop_and_read_output();
    panic!(
        "{name} did not become healthy on port {port}; last status={last_status_line:?}; output:\n{output}"
    );
}

#[test]
fn cli_no_auth_primary_replicates_to_read_replica() {
    let temp = support::temp_data_dir("cli-primary-replica");
    let ports = free_ports(4);
    let (primary_http, primary_grpc, replica_http, replica_grpc) =
        (ports[0], ports[1], ports[2], ports[3]);
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

    let mut replica = spawn_replica(
        &temp.join("replica.rdb"),
        replica_http,
        replica_grpc,
        primary_grpc,
    );
    wait_for_health(&mut replica, replica_http, "replica");

    let healthy_deadline = Instant::now() + REPLICATION_TIMEOUT;
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

    let visible_deadline = Instant::now() + REPLICATION_TIMEOUT;
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

    let live_deadline = Instant::now() + REPLICATION_TIMEOUT;
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

    #[cfg(feature = "grpc")]
    {
        use reddb_client::{QueryOptions, Reddb, ValueOut};

        fn contains_text(result: &reddb_client::QueryResult, expected: &str) -> bool {
            result
                .rows
                .iter()
                .flatten()
                .any(|(_, value)| matches!(value, ValueOut::String(value) if value == expected))
        }

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("client runtime");
        let uri = format!("grpc://127.0.0.1:{primary_grpc},127.0.0.1:{replica_grpc}");
        let client = runtime
            .block_on(Reddb::connect(&uri))
            .expect("connect public primary/replica facade");

        runtime
            .block_on(client.query("INSERT INTO repl_probe (id, name) VALUES (3, 'public-facade')"))
            .expect("public facade writes to primary");

        let facade_deadline = Instant::now() + REPLICATION_TIMEOUT;
        loop {
            let result = runtime.block_on(client.query_with_options(
                "SELECT name FROM repl_probe WHERE id = 3",
                QueryOptions::local(),
            ));
            if result
                .as_ref()
                .is_ok_and(|result| contains_text(result, "public-facade"))
            {
                break;
            }
            if Instant::now() >= facade_deadline {
                let primary_row = http_request(
                    primary_http,
                    "POST",
                    "/query",
                    Some(r#"{"query":"SELECT * FROM repl_probe WHERE id = 3"}"#),
                );
                let primary_status = http_request(primary_http, "GET", "/replication/status", None);
                let replica_status = http_request(replica_http, "GET", "/replication/status", None);
                panic!(
                    "public facade never observed the replicated row: {result:?}; \
                     primary_row={primary_row:?}; primary_status={primary_status:?}; \
                     replica_status={replica_status:?}"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // A local read remains available from the real replica after the
        // primary process disappears. Strong reads and all ordinary queries
        // stay pinned to the primary and therefore fail closed instead of
        // silently serving stale data or mutating a replica.
        drop(primary);
        let local = runtime
            .block_on(client.query_with_options(
                "SELECT name FROM repl_probe WHERE id = 3",
                QueryOptions::local(),
            ))
            .expect("local read remains available on replica");
        assert!(contains_text(&local, "public-facade"), "local={local:?}");

        let strong = runtime.block_on(client.query_with_options(
            "SELECT name FROM repl_probe WHERE id = 3",
            QueryOptions::strong(),
        ));
        assert!(
            strong.is_err(),
            "strong read must fail closed while the primary is down: {strong:?}"
        );
        let write = runtime.block_on(
            client.query("INSERT INTO repl_probe (id, name) VALUES (4, 'must-not-reach-replica')"),
        );
        assert!(
            write.is_err(),
            "write must not fall back to the replica: {write:?}"
        );
    }
}
