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

struct FailoverConfig<'a> {
    backend_root: &'a Path,
    remote_key: &'a str,
    holder_id: &'a str,
    term: u64,
    lease_required: bool,
}

impl FailoverConfig<'_> {
    fn apply(&self, command: &mut Command) {
        command
            .env("RED_BACKEND", "fs")
            .env("RED_REMOTE_KEY", self.remote_key)
            .env("REDDB_LOCAL_BACKEND_ROOT", self.backend_root)
            .env("RED_LEASE_HOLDER_ID", self.holder_id)
            .env("RED_LEASE_TTL_SECS", "3")
            .env("RED_REPLICATION_TERM", self.term.to_string())
            .env(
                "RED_LEASE_REQUIRED",
                if self.lease_required { "true" } else { "false" },
            )
            .env("RED_BACKUP_ON_SHUTDOWN", "false");
    }
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
    spawn_primary_with_failover(path, http_port, grpc_port, None)
}

fn spawn_primary_with_failover(
    path: &Path,
    http_port: u16,
    grpc_port: u16,
    failover: Option<&FailoverConfig<'_>>,
) -> ServerChild {
    let output_path = path.with_extension("log");
    let output = File::create(&output_path).expect("create primary output log");
    let mut command = Command::new(red_binary());
    command
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
        .stderr(output);
    if let Some(failover) = failover {
        failover.apply(&mut command);
    }
    let child = command.spawn().expect("spawn primary");
    ServerChild { child, output_path }
}

fn spawn_replica(
    path: &Path,
    http_port: u16,
    grpc_port: u16,
    primary_grpc_port: u16,
) -> ServerChild {
    spawn_replica_with_failover(path, http_port, grpc_port, primary_grpc_port, None)
}

fn spawn_replica_with_failover(
    path: &Path,
    http_port: u16,
    grpc_port: u16,
    primary_grpc_port: u16,
    failover: Option<&FailoverConfig<'_>>,
) -> ServerChild {
    let output_path = path.with_extension("log");
    let output = File::create(&output_path).expect("create replica output log");
    let mut command = Command::new(red_binary());
    command
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
        .stderr(output);
    if let Some(failover) = failover {
        failover.apply(&mut command);
    }
    let child = command.spawn().expect("spawn replica");
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

fn wait_for_exit(child: &mut ServerChild, timeout: Duration, name: &str) -> String {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if child.has_exited() {
            return std::fs::read_to_string(&child.output_path).unwrap_or_default();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let output = child.stop_and_read_output();
    panic!("{name} did not exit within {timeout:?}; output:\n{output}");
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

#[cfg(feature = "grpc")]
#[test]
fn cli_replication_snapshots_preserve_independent_native_generations() {
    use reddb::grpc::proto::red_db_client::RedDbClient;
    use reddb::grpc::proto::Empty;
    use reddb_wire::replication::BaseBackupChunk;

    let temp = support::temp_data_dir("cli-native-replication-snapshot");
    let ports = free_ports(2);
    let mut primary = spawn_primary(&temp.join("primary.rdb"), ports[0], ports[1]);
    wait_for_health(&mut primary, ports[0], "snapshot source");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("snapshot client runtime");
    let endpoint =
        tonic::transport::Endpoint::from_shared(format!("http://127.0.0.1:{}", ports[1]))
            .expect("snapshot endpoint")
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15));
    let channel = runtime
        .block_on(endpoint.connect())
        .expect("connect snapshot client");
    let mut client = RedDbClient::new(channel).max_decoding_message_size(64 * 1024 * 1024);
    let mut snapshots = Vec::new();
    for (ordinal, query, expected) in [
        (
            1,
            "INSERT INTO snapshot_probe (id, name) VALUES (1, 'original')",
            "original",
        ),
        (
            2,
            "UPDATE snapshot_probe SET name = 'updated' WHERE id = 1",
            "updated",
        ),
    ] {
        let body = format!(r#"{{"query":"{query}"}}"#);
        let (status, response) = http_request(ports[0], "POST", "/query", Some(&body));
        assert_eq!(status, 200, "snapshot source mutation: {response}");
        let mut request = tonic::Request::new(Empty {});
        request.metadata_mut().insert(
            "x-reddb-replica-id",
            "snapshot-reader".parse().expect("replica identity"),
        );
        let reply = runtime
            .block_on(client.replication_snapshot(request))
            .unwrap_or_else(|error| {
                panic!(
                    "native replication snapshot RPC failed: {error:?}; source log:\n{}",
                    std::fs::read_to_string(&primary.output_path).unwrap_or_default()
                )
            })
            .into_inner();
        assert!(reply.ok);
        let chunk =
            BaseBackupChunk::decode_json(reply.payload.as_bytes()).expect("snapshot wire contract");
        assert!(chunk.snapshot_complete);
        let lsn = chunk.snapshot_lsn.expect("snapshot cursor");
        let bytes = chunk.snapshot_hex.expect("whole native snapshot");
        assert_eq!(chunk.snapshot_total_bytes, Some(bytes.len() as u64));
        let path = temp.join(format!("snapshot-{ordinal}.rdb"));
        std::fs::write(&path, bytes).expect("independent replica image");
        snapshots.push((path, expected, lsn));
    }
    assert!(snapshots[1].2 > snapshots[0].2);
    // Read the older image only after the live source has changed again.
    // Both images must have valid native roots and their own MVCC/index state.
    for (path, expected, _) in snapshots {
        let reader = reddb::RedDBRuntime::with_options(
            reddb::RedDBOptions::persistent(&path)
                .with_storage_profile(reddb::StorageDeployPreset::PrimaryReplicaDev.selection())
                .expect("native snapshot reader")
                .with_read_only(true),
        )
        .expect("cold open transferred native generation");
        let point = reader
            .execute_query("SELECT name FROM snapshot_probe WHERE id = 1")
            .expect("cold snapshot point before scan");
        assert_eq!(point.result.len(), 1);
        assert_eq!(
            point.result.records[0]
                .get("name")
                .and_then(|value| value.as_text()),
            Some(expected)
        );
        let scan = reader
            .execute_query("SELECT name FROM snapshot_probe")
            .expect("snapshot scan");
        assert_eq!(scan.result.len(), 1);
        assert_eq!(
            scan.result.records[0]
                .get("name")
                .and_then(|value| value.as_text()),
            Some(expected)
        );
    }
}

#[cfg(feature = "grpc")]
#[test]
fn cli_manual_promotion_preserves_data_and_fences_the_old_primary() {
    use reddb_client::{QueryOptions, Reddb, ValueOut};
    use reddb_wire::replication::{FailoverPromotionReply, FailoverPromotionRequest};

    fn contains_text(result: &reddb_client::QueryResult, expected: &str) -> bool {
        result
            .rows
            .iter()
            .flatten()
            .any(|(_, value)| matches!(value, ValueOut::String(value) if value == expected))
    }

    fn wait_for_follower_row(port: u16, id: u64, expected: &str) {
        let deadline = Instant::now() + REPLICATION_TIMEOUT;
        let query = format!(r#"{{"query":"SELECT name FROM failover_probe WHERE id = {id}"}}"#);
        loop {
            let (status, row) = http_request(port, "POST", "/query", Some(&query));
            let (_, health) = http_request(port, "GET", "/replication/status", None);
            if status == 200 && row.contains(expected) && health.contains(r#""state":"healthy""#) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "replica missed row {id}: row={row}; replication={health}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    let temp = support::temp_data_dir("cli-manual-promotion");
    let backend_root = temp.join("lease-backend");
    std::fs::create_dir_all(&backend_root).expect("create lease backend root");
    let primary_path = temp.join("primary.rdb");
    let replica_path = temp.join("replica.rdb");
    let follower_path = temp.join("follower.rdb");
    let ports = free_ports(8);
    let (primary_http, primary_grpc, replica_http, replica_grpc, stale_http, stale_grpc) =
        (ports[0], ports[1], ports[2], ports[3], ports[4], ports[5]);
    let (follower_http, follower_grpc) = (ports[6], ports[7]);
    let primary_failover = FailoverConfig {
        backend_root: &backend_root,
        remote_key: "manual-promotion",
        holder_id: "primary-a",
        term: 1,
        lease_required: true,
    };
    let replica_failover = FailoverConfig {
        backend_root: &backend_root,
        remote_key: "manual-promotion",
        holder_id: "replica-b",
        term: 1,
        lease_required: false,
    };

    let mut primary = spawn_primary_with_failover(
        &primary_path,
        primary_http,
        primary_grpc,
        Some(&primary_failover),
    );
    wait_for_health(&mut primary, primary_http, "leased primary");
    for query in [
        "CREATE TABLE failover_probe (id INTEGER, name TEXT)",
        "INSERT INTO failover_probe (id, name) VALUES (1, 'before-promotion')",
    ] {
        let body = format!(r#"{{"query":"{query}"}}"#);
        let (status, response) = http_request(primary_http, "POST", "/query", Some(&body));
        assert_eq!(status, 200, "primary query failed: {response}");
    }

    let mut replica = spawn_replica_with_failover(
        &replica_path,
        replica_http,
        replica_grpc,
        primary_grpc,
        Some(&replica_failover),
    );
    wait_for_health(&mut replica, replica_http, "promotion candidate");
    let replicated_deadline = Instant::now() + REPLICATION_TIMEOUT;
    loop {
        let (status, body) = http_request(
            replica_http,
            "POST",
            "/query",
            Some(r#"{"query":"SELECT * FROM failover_probe WHERE id = 1"}"#),
        );
        let (_, replication) = http_request(replica_http, "GET", "/replication/status", None);
        if status == 200
            && body.contains("before-promotion")
            && replication.contains("\"state\":\"healthy\"")
        {
            break;
        }
        assert!(
            Instant::now() < replicated_deadline,
            "promotion candidate did not catch up: query={body} replication={replication}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // Keep another durable replica at the old primary's applied frontier.
    // Reconnecting it after failover must deliver the next write via WAL.
    let follower_failover = FailoverConfig {
        holder_id: "follower-c",
        ..replica_failover
    };
    let mut follower = spawn_replica_with_failover(
        &follower_path,
        follower_http,
        follower_grpc,
        primary_grpc,
        Some(&follower_failover),
    );
    wait_for_health(&mut follower, follower_http, "second promotion-era replica");
    wait_for_follower_row(follower_http, 1, "before-promotion");
    let _ = follower.stop_and_read_output();

    // Seed the negative upload sentinel after both fresh replicas bootstrap.
    let legacy_payload = backend_root.join("manual-promotion");
    let legacy_payload_bytes = b"a replica must not publish over this backend object";
    std::fs::write(&legacy_payload, legacy_payload_bytes).expect("backend payload sentinel");
    let (status, checkpoint) = http_request(replica_http, "POST", "/checkpoint", None);
    assert_eq!(status, 200, "replica local checkpoint failed: {checkpoint}");
    assert_eq!(
        std::fs::read(&legacy_payload).expect("backend payload after replica checkpoint"),
        legacy_payload_bytes
    );

    let _ = primary.stop_and_read_output();
    // Older lease-only boots could leave serverless control objects behind.
    // This profile must recover its local replica without consulting them.
    let foreign_head = backend_root.join("manual-promotion.serverless/HEAD.json");
    std::fs::create_dir_all(foreign_head.parent().expect("head directory"))
        .expect("create unrelated serverless directory");
    let foreign_head_bytes = b"unrelated serverless publication must remain untouched";
    std::fs::write(&foreign_head, foreign_head_bytes).expect("old serverless control object");
    let request = FailoverPromotionRequest {
        holder_id: Some("replica-b".to_string()),
        ttl_ms: Some(30_000),
    };
    let request = String::from_utf8(request.encode_json()).expect("promotion request JSON");
    let promotion_deadline = Instant::now() + Duration::from_secs(10);
    let promotion = loop {
        let (status, body) = http_request(
            replica_http,
            "POST",
            "/admin/failover/promote",
            Some(&request),
        );
        if status == 200 {
            break FailoverPromotionReply::decode_json(body.as_bytes())
                .expect("promotion response contract");
        }
        assert_eq!(status, 409, "unexpected promotion failure: {body}");
        assert!(
            Instant::now() < promotion_deadline,
            "old writer lease did not expire: {body}"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(promotion.holder_id, "replica-b");
    assert_eq!(promotion.term, 2);
    assert!(promotion.applied_lsn > 0, "promotion={promotion:?}");

    let _ = replica.stop_and_read_output();
    let promoted_failover = FailoverConfig {
        backend_root: &backend_root,
        remote_key: "manual-promotion",
        holder_id: "replica-b",
        term: promotion.term,
        lease_required: true,
    };
    let mut promoted = spawn_primary_with_failover(
        &replica_path,
        replica_http,
        replica_grpc,
        Some(&promoted_failover),
    );
    wait_for_health(&mut promoted, replica_http, "promoted primary");
    let (status, retention) = http_request(replica_http, "GET", "/replication/status", None);
    assert_eq!(status, 200, "promoted replication status: {retention}");
    assert!(
        retention.contains(r#""wal_retention_error":false"#),
        "promoted primary must calculate WAL retention: {retention}"
    );
    let follower_failover = FailoverConfig {
        term: promotion.term,
        ..follower_failover
    };
    let mut follower = spawn_replica_with_failover(
        &follower_path,
        follower_http,
        follower_grpc,
        replica_grpc,
        Some(&follower_failover),
    );
    wait_for_health(
        &mut follower,
        follower_http,
        "replica following the new primary",
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("client runtime");
    let client = runtime
        .block_on(Reddb::connect(&format!("grpc://127.0.0.1:{replica_grpc}")))
        .expect("connect public facade to promoted primary");
    runtime
        .block_on(
            client.query("INSERT INTO failover_probe (id, name) VALUES (2, 'after-promotion')"),
        )
        .expect("post-promotion write");
    let before = runtime
        .block_on(client.query_with_options(
            "SELECT name FROM failover_probe WHERE id = 1",
            QueryOptions::strong(),
        ))
        .expect("strong read of pre-promotion data");
    let after = runtime
        .block_on(client.query_with_options(
            "SELECT name FROM failover_probe WHERE id = 2",
            QueryOptions::strong(),
        ))
        .expect("strong read of post-promotion data");
    assert!(
        contains_text(&before, "before-promotion"),
        "before={before:?}"
    );
    assert!(contains_text(&after, "after-promotion"), "after={after:?}");
    wait_for_follower_row(follower_http, 2, "after-promotion");
    assert_eq!(
        std::fs::read(&foreign_head).expect("unrelated head after promotion"),
        foreign_head_bytes
    );

    let mut stale_primary = spawn_primary_with_failover(
        &primary_path,
        stale_http,
        stale_grpc,
        Some(&primary_failover),
    );
    let stale_output = wait_for_exit(&mut stale_primary, Duration::from_secs(10), "stale primary");
    assert!(
        stale_output
            .to_ascii_lowercase()
            .contains("fenced lease op"),
        "stale primary did not report term fencing:\n{stale_output}"
    );

    let _ = promoted.stop_and_read_output();
    let mut reopened = spawn_primary_with_failover(
        &replica_path,
        replica_http,
        replica_grpc,
        Some(&promoted_failover),
    );
    wait_for_health(&mut reopened, replica_http, "restarted promoted primary");
    let client = runtime
        .block_on(Reddb::connect(&format!("grpc://127.0.0.1:{replica_grpc}")))
        .expect("reconnect after a second crash");
    for (id, expected) in [(1, "before-promotion"), (2, "after-promotion")] {
        let result = runtime
            .block_on(client.query_with_options(
                &format!("SELECT name FROM failover_probe WHERE id = {id}"),
                QueryOptions::strong(),
            ))
            .expect("cold acknowledged row");
        assert_eq!(result.rows.len(), 1, "row {id}: {result:?}");
        assert!(contains_text(&result, expected), "row {id}: {result:?}");
    }
    runtime
        .block_on(client.query("INSERT INTO failover_probe (id, name) VALUES (3, 'after-restart')"))
        .expect("writer makes progress after restart");
    wait_for_follower_row(follower_http, 3, "after-restart");
    let scan = runtime
        .block_on(client.query_with_options(
            "SELECT id, name FROM failover_probe ORDER BY id",
            QueryOptions::strong(),
        ))
        .expect("scan agrees with cold points and new write");
    assert_eq!(scan.rows.len(), 3, "scan={scan:?}");
    for expected in ["before-promotion", "after-promotion", "after-restart"] {
        assert!(contains_text(&scan, expected), "scan={scan:?}");
    }
    let (status, checkpoint) = http_request(replica_http, "POST", "/checkpoint", None);
    assert_eq!(status, 200, "promoted checkpoint failed: {checkpoint}");
    let backup = reddb::RedDBRuntime::with_options(
        reddb::RedDBOptions::persistent(backend_root.join("manual-promotion"))
            .with_storage_profile(reddb::StorageDeployPreset::PrimaryReplicaDev.selection())
            .expect("native backup reader profile")
            .with_read_only(true),
    )
    .expect("open the checkpoint's remote backup independently");
    let catalog = backup
        .primary_replica_slot_catalog()
        .expect("read persisted promoted slots")
        .expect("promoted slot catalog");
    assert_eq!(catalog.timeline.0, 2);
    assert!(
        !catalog.slots.is_empty(),
        "real follower must persist its slot"
    );
    assert!(catalog.slots.iter().all(|slot| slot.timeline.0 == 2));
    backup
        .primary_replica_wal_retention_plan()
        .expect("promoted backup retains a usable retention catalog")
        .expect("backup retention plan");
    let restored = backup
        .execute_query("SELECT id, name FROM failover_probe ORDER BY id")
        .expect("read all acknowledged rows from the backup");
    assert_eq!(restored.result.records.len(), 3);
    let names: Vec<_> = restored
        .result
        .records
        .iter()
        .map(|row| {
            row.get("name")
                .and_then(|value| value.as_text())
                .expect("backup name")
        })
        .collect();
    assert_eq!(
        names,
        ["before-promotion", "after-promotion", "after-restart"]
    );
    assert_eq!(
        std::fs::read(&foreign_head).expect("unrelated head after restart"),
        foreign_head_bytes
    );
}
