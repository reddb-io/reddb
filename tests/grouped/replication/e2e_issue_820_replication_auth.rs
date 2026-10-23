//! Issue #820 — replication stream/ack capabilities and ack identity binding.

use std::sync::Arc;
use std::time::Duration;

use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::{Empty, JsonPayloadRequest};
use reddb::replication::ReplicationConfig;
use reddb::runtime::RedDBRuntime;
use reddb::{GrpcServerOptions, RedDBGrpcServer, RedDBOptions};

use tonic::metadata::MetadataValue;
use tonic::transport::Endpoint;
use tonic::Code;

fn pick_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

async fn wait_for_port(port: u16, max_ms: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(max_ms);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("gRPC server never came up on port {port}");
}

async fn connect_client(port: u16) -> RedDbClient<tonic::transport::Channel> {
    let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
        .unwrap()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5));
    let channel = endpoint.connect().await.expect("client connect");
    RedDbClient::new(channel)
}

fn install_policy(store: &AuthStore, username: &str, id: &str, actions: &[&str], resource: &str) {
    let actions_json = actions
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect::<Vec<_>>()
        .join(",");
    let policy_json = format!(
        r#"{{"id":"{id}","version":1,"statements":[{{"effect":"allow","actions":[{actions_json}],"resources":["{resource}"]}}]}}"#
    );
    store
        .put_policy(Policy::from_json_str(&policy_json).expect("policy parses"))
        .expect("put policy");
    store
        .attach_policy(PrincipalRef::User(UserId::platform(username)), id)
        .expect("attach policy");
}

fn bearer_request<T>(message: T, token: &str) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    let value: MetadataValue<_> = format!("Bearer {token}").parse().unwrap();
    request.metadata_mut().insert("authorization", value);
    request
}

fn pull_request(replica_id: &str, token: &str) -> tonic::Request<JsonPayloadRequest> {
    bearer_request(
        JsonPayloadRequest {
            payload_json: format!(
                r#"{{"replica_id":"{replica_id}","since_lsn":0,"max_count":10}}"#
            ),
        },
        token,
    )
}

fn ack_request(
    replica_id: &str,
    applied_lsn: u64,
    token: &str,
) -> tonic::Request<JsonPayloadRequest> {
    bearer_request(
        JsonPayloadRequest {
            payload_json: format!(
                r#"{{"replica_id":"{replica_id}","applied_lsn":{applied_lsn},"durable_lsn":{applied_lsn}}}"#
            ),
        },
        token,
    )
}

#[tokio::test]
async fn replication_stream_and_ack_require_capabilities_and_bind_ack_identity() {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime");

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store.create_user("reader", "p", Role::Read).unwrap();
    store.create_user("replica_a", "p", Role::Read).unwrap();
    store.create_user("replica_b", "p", Role::Read).unwrap();
    let reader_key = store
        .create_api_key("reader", "read-only", Role::Read)
        .unwrap();
    let replica_a_key = store
        .create_api_key("replica_a", "replication-a", Role::Read)
        .unwrap();
    let replica_b_key = store
        .create_api_key("replica_b", "replication-b", Role::Read)
        .unwrap();

    install_policy(
        &store,
        "reader",
        "p_reader_data_read",
        &["select"],
        "table:*",
    );
    install_policy(
        &store,
        "replica_a",
        "p_replica_a",
        &["cluster:replication:stream", "cluster:replication:ack"],
        "cluster:replication",
    );
    install_policy(
        &store,
        "replica_b",
        "p_replica_b",
        &["cluster:replication:stream", "cluster:replication:ack"],
        "cluster:replication",
    );

    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let server = RedDBGrpcServer::with_options(
        runtime.clone(),
        GrpcServerOptions {
            bind_addr: bind,
            tls: None,
        },
        store,
    );
    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;
    let mut client = connect_client(port).await;

    let read_only_pull = client
        .pull_wal_records(pull_request("reader", &reader_key.key))
        .await
        .expect_err("read-only token must not stream WAL records");
    assert_eq!(read_only_pull.code(), Code::PermissionDenied);

    let read_only_snapshot = client
        .replication_snapshot(bearer_request(Empty {}, &reader_key.key))
        .await
        .expect_err("read-only token must not fetch replication snapshot");
    assert_eq!(read_only_snapshot.code(), Code::PermissionDenied);

    let read_only_ack = client
        .ack_replica_lsn(ack_request("reader", 10, &reader_key.key))
        .await
        .expect_err("read-only token must not ack replica LSN");
    assert_eq!(read_only_ack.code(), Code::PermissionDenied);

    client
        .pull_wal_records(pull_request("replica_a", &replica_a_key.key))
        .await
        .expect("replication-capable token can stream WAL records");
    client
        .replication_snapshot(bearer_request(Empty {}, &replica_a_key.key))
        .await
        .expect("replication-capable token can fetch snapshot");

    client
        .pull_wal_records(pull_request("replica_b", &replica_b_key.key))
        .await
        .expect("second replica self-registers before forged ack attempt");

    let forged = client
        .ack_replica_lsn(ack_request("replica_b", 100, &replica_a_key.key))
        .await
        .expect_err("replica_a token must not ack as replica_b");
    assert_eq!(forged.code(), Code::PermissionDenied);
    let replica_b = runtime
        .primary_replica_snapshots()
        .into_iter()
        .find(|replica| replica.id == "replica_b")
        .expect("replica_b registered");
    assert_eq!(
        replica_b.last_acked_lsn, 0,
        "forged ack must not advance replica_b commit watermark"
    );

    let ack = client
        .ack_replica_lsn(ack_request("replica_a", 10, &replica_a_key.key))
        .await
        .expect("replica_a can ack as itself")
        .into_inner();
    let body: serde_json::Value = serde_json::from_str(&ack.payload).expect("ack reply is JSON");
    assert_eq!(body["replica_id"], "replica_a");
    let replica_a = runtime
        .primary_replica_snapshots()
        .into_iter()
        .find(|replica| replica.id == "replica_a")
        .expect("replica_a registered");
    assert_eq!(replica_a.last_acked_lsn, 10);

    h.abort();
}

/// Exercise the engine's own bootstrap and background follower, rather than
/// hand-built requests that accidentally supply the headers the follower omits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_replica_bootstraps_and_follows_authenticated_primary() {
    let directory = tempfile::tempdir().expect("test directory");
    let primary = RedDBRuntime::with_options(
        RedDBOptions::persistent(directory.path().join("primary.rdb"))
            .with_replication(ReplicationConfig::primary()),
    )
    .expect("primary runtime");
    primary
        .execute_query("CREATE TABLE replication_probe (id TEXT, value INTEGER)")
        .expect("schema");
    primary
        .execute_query("INSERT INTO replication_probe (id, value) VALUES ('before', 1)")
        .expect("seed");
    // Hex-encoded physical snapshots exceed tonic's 4 MiB client default
    // even for a small control-plane store. Cover that real bootstrap size.
    primary
        .execute_query("CREATE TABLE snapshot_payload (id TEXT, body TEXT)")
        .expect("payload schema");
    for index in 0..5 {
        primary
            .execute_query(&format!(
                "INSERT INTO snapshot_payload (id, body) VALUES ('large-{index}', '{}')",
                "x".repeat(450_000)
            ))
            .expect("payload seed");
    }

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store
        .create_user("follower", "test-password", Role::Read)
        .expect("replica principal");
    let key = store
        .create_api_key("follower", "replication", Role::Read)
        .expect("replica key");
    install_policy(
        &store,
        "follower",
        "follower_replication",
        &["cluster:replication:stream", "cluster:replication:ack"],
        "cluster:replication",
    );
    let port = pick_port();
    let server = RedDBGrpcServer::with_options(
        primary.clone(),
        GrpcServerOptions {
            bind_addr: format!("127.0.0.1:{port}"),
            tls: None,
        },
        store,
    );
    let server_task = tokio::spawn(async move {
        server.serve().await.expect("gRPC server");
    });
    wait_for_port(port, 5000).await;

    struct EnvironmentGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for EnvironmentGuard {
        fn drop(&mut self) {
            for (name, value) in &self.0 {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
    let names = [
        "REDDB_REPLICATION_USERNAME",
        "REDDB_REPLICATION_API_KEY",
        "REDDB_REPLICATION_USERNAME_FILE",
        "REDDB_REPLICATION_API_KEY_FILE",
    ];
    let _environment = EnvironmentGuard(
        names
            .into_iter()
            .map(|name| (name, std::env::var_os(name)))
            .collect(),
    );
    std::env::set_var("REDDB_REPLICATION_USERNAME", "follower");
    std::env::set_var("REDDB_REPLICATION_API_KEY", &key.key);
    std::env::remove_var("REDDB_REPLICATION_USERNAME_FILE");
    std::env::remove_var("REDDB_REPLICATION_API_KEY_FILE");

    // A rejected bootstrap must not silently create an empty local database.
    std::env::set_var("REDDB_REPLICATION_API_KEY", "rejected-test-key");
    let rejected_path = directory.path().join("rejected.rdb");
    let rejected_bootstrap_path = rejected_path.clone();
    let rejected = tokio::task::spawn_blocking(move || {
        RedDBRuntime::with_options(
            RedDBOptions::persistent(rejected_bootstrap_path).with_replication(
                ReplicationConfig::replica(format!("http://127.0.0.1:{port}")),
            ),
        )
        .err()
        .map(|error| error.to_string())
    })
    .await
    .expect("rejected bootstrap task")
    .expect("bad credentials must fail startup");
    assert!(rejected.contains("authenticated replica snapshot bootstrap failed"));
    assert!(!rejected.contains("rejected-test-key"));
    assert!(!rejected_path.exists());
    std::env::set_var("REDDB_REPLICATION_API_KEY", &key.key);

    let replica_path = directory.path().join("replica.rdb");
    let replica = tokio::task::spawn_blocking(move || {
        RedDBRuntime::with_options(RedDBOptions::persistent(replica_path).with_replication(
            ReplicationConfig::replica(format!("http://127.0.0.1:{port}")),
        ))
        .expect("authenticated snapshot bootstrap")
    })
    .await
    .expect("bootstrap task");
    assert_eq!(
        replica
            .execute_query("SELECT id FROM replication_probe")
            .expect("snapshot rows")
            .result
            .records
            .len(),
        1
    );
    assert_eq!(
        replica
            .execute_query("SELECT id FROM snapshot_payload")
            .expect("large snapshot imported")
            .result
            .records
            .len(),
        5
    );
    assert!(replica
        .execute_query("INSERT INTO replication_probe (id, value) VALUES ('forbidden', 9)")
        .is_err());

    primary
        .execute_query("INSERT INTO replication_probe (id, value) VALUES ('after', 2)")
        .expect("insert");
    primary
        .execute_query("UPDATE replication_probe SET value = 3 WHERE id = 'after'")
        .expect("update");
    primary
        .execute_query("DELETE FROM replication_probe WHERE id = 'before'")
        .expect("delete");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    loop {
        let found = replica
            .execute_query("SELECT id FROM replication_probe WHERE id = 'after' AND value = 3")
            .expect("replica query")
            .result
            .records
            .len();
        let total = replica
            .execute_query("SELECT id FROM replication_probe")
            .expect("replica total")
            .result
            .records
            .len();
        let acknowledged = primary
            .primary_replica_snapshots()
            .into_iter()
            .any(|slot| slot.id == "follower" && slot.last_acked_lsn > 0);
        if found == 1 && total == 1 && acknowledged {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline,
            "native replica must apply insert/update/delete and acknowledge with its credential identity");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    server_task.abort();
}
