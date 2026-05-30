//! Issue #830 — slot-pinned replica bootstrap.

mod support;

use std::sync::Arc;
use std::time::Duration;

use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::Empty;
use reddb::replication::ReplicationConfig;
use reddb::{GrpcServerOptions, RedDBGrpcServer, RedDBOptions, RedDBRuntime};
use tonic::metadata::MetadataValue;
use tonic::transport::Endpoint;

fn pick_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn wait_for_port(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
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
    RedDbClient::new(endpoint.connect().await.expect("client connect"))
}

fn install_replication_policy(store: &AuthStore, username: &str) {
    let policy_json = format!(
        r#"{{"id":"p_{username}_replication","version":1,"statements":[{{"effect":"allow","actions":["cluster:replication:stream"],"resources":["cluster:replication"]}}]}}"#
    );
    store
        .put_policy(Policy::from_json_str(&policy_json).expect("policy parses"))
        .expect("put policy");
    store
        .attach_policy(
            PrincipalRef::User(UserId::platform(username)),
            &format!("p_{username}_replication"),
        )
        .expect("attach policy");
}

fn bearer_empty(token: &str) -> tonic::Request<Empty> {
    let mut request = tonic::Request::new(Empty {});
    let value: MetadataValue<_> = format!("Bearer {token}").parse().unwrap();
    request.metadata_mut().insert("authorization", value);
    request
}

fn bearer_snapshot_chunk(
    token: &str,
    max_bytes: usize,
    offset: usize,
    snapshot_token: Option<&str>,
) -> tonic::Request<Empty> {
    let mut request = bearer_empty(token);
    request.metadata_mut().insert(
        "x-reddb-snapshot-max-bytes",
        max_bytes.to_string().parse().unwrap(),
    );
    request.metadata_mut().insert(
        "x-reddb-snapshot-offset",
        offset.to_string().parse().unwrap(),
    );
    if let Some(snapshot_token) = snapshot_token {
        request
            .metadata_mut()
            .insert("x-reddb-snapshot-token", snapshot_token.parse().unwrap());
    }
    request
}

#[tokio::test]
async fn replication_snapshot_pins_authenticated_replica_slot_at_bootstrap_start() {
    let primary = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .expect("primary runtime");
    primary
        .execute_query("CREATE TABLE issue_830_bootstrap (id INTEGER, name TEXT)")
        .expect("create table");
    primary
        .execute_query("INSERT INTO issue_830_bootstrap (id, name) VALUES (1, 'one')")
        .expect("insert bootstrap row");

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store.create_user("replica_a", "p", Role::Read).unwrap();
    let replica_key = store
        .create_api_key("replica_a", "replication-a", Role::Read)
        .unwrap();
    install_replication_policy(&store, "replica_a");

    let port = pick_port();
    let server = RedDBGrpcServer::with_options(
        primary.clone(),
        GrpcServerOptions {
            bind_addr: format!("127.0.0.1:{port}"),
            tls: None,
        },
        store,
    );
    let handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port).await;

    let mut client = connect_client(port).await;
    let snapshot = client
        .replication_snapshot(bearer_empty(&replica_key.key))
        .await
        .expect("replication snapshot starts bootstrap")
        .into_inner();
    let snapshot_body: serde_json::Value =
        serde_json::from_str(&snapshot.payload).expect("snapshot body");
    let snapshot_lsn = snapshot_body["snapshot_lsn"]
        .as_u64()
        .expect("snapshot_lsn");
    assert_eq!(snapshot_body["replica_id"], "replica_a");
    assert_eq!(
        snapshot_body["slot_restart_lsn"].as_u64(),
        Some(snapshot_lsn)
    );

    let status = client
        .replication_status(bearer_empty(&replica_key.key))
        .await
        .expect("replication status")
        .into_inner();
    let status_body: serde_json::Value = serde_json::from_str(&status.payload).expect("status");
    let slots = status_body["slots"].as_array().expect("slots array");
    let slot = slots
        .iter()
        .find(|slot| slot["id"] == "replica_a")
        .expect("bootstrap slot registered");
    assert_eq!(slot["restart_lsn"].as_u64(), Some(snapshot_lsn));
    assert_eq!(slot["invalidated"].as_bool(), Some(false));

    handle.abort();
}

#[tokio::test]
async fn replication_snapshot_resumes_from_checkpoint_token_offset() {
    let primary = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .expect("primary runtime");
    primary
        .execute_query("CREATE TABLE issue_830_snapshot_resume (id INTEGER, name TEXT)")
        .expect("create table");
    for id in 0..8 {
        primary
            .execute_query(&format!(
                "INSERT INTO issue_830_snapshot_resume (id, name) VALUES ({id}, 'row-{id}')"
            ))
            .expect("insert row");
    }

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store.create_user("replica_b", "p", Role::Read).unwrap();
    let replica_key = store
        .create_api_key("replica_b", "replication-b", Role::Read)
        .unwrap();
    install_replication_policy(&store, "replica_b");

    let port = pick_port();
    let server = RedDBGrpcServer::with_options(
        primary,
        GrpcServerOptions {
            bind_addr: format!("127.0.0.1:{port}"),
            tls: None,
        },
        store,
    );
    let handle = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port).await;

    let mut client = connect_client(port).await;
    let first = client
        .replication_snapshot(bearer_snapshot_chunk(&replica_key.key, 64, 0, None))
        .await
        .expect("first snapshot chunk")
        .into_inner();
    let first_body: serde_json::Value = serde_json::from_str(&first.payload).expect("first body");
    let snapshot_token = first_body["snapshot_token"]
        .as_str()
        .expect("snapshot token")
        .to_string();
    let next_offset = first_body["next_snapshot_offset"]
        .as_u64()
        .expect("next offset");
    assert_eq!(first_body["snapshot_offset"].as_u64(), Some(0));
    assert!(
        next_offset > 0,
        "first chunk must advance the resumable offset"
    );
    assert_eq!(first_body["snapshot_complete"].as_bool(), Some(false));

    let resumed = client
        .replication_snapshot(bearer_snapshot_chunk(
            &replica_key.key,
            64,
            next_offset as usize,
            Some(&snapshot_token),
        ))
        .await
        .expect("resumed snapshot chunk")
        .into_inner();
    let resumed_body: serde_json::Value =
        serde_json::from_str(&resumed.payload).expect("resumed body");
    assert_eq!(
        resumed_body["snapshot_token"].as_str(),
        Some(snapshot_token.as_str())
    );
    assert_eq!(resumed_body["snapshot_offset"].as_u64(), Some(next_offset));
    assert!(
        resumed_body["snapshot_chunk_hex"]
            .as_str()
            .map(|chunk| !chunk.is_empty())
            .unwrap_or(false),
        "resumed chunk must carry snapshot bytes from the checkpoint"
    );

    handle.abort();
}
