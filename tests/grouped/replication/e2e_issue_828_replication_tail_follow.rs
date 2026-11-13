//! Issue #828 — replica WAL tail-follow stream with await-data semantics.

#[path = "../../support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::{JsonPayloadRequest, WalStreamChunk, WalStreamRequest};
use reddb::replication::cdc::ChangeRecord;
use reddb::replication::logical::{ApplyMode, ApplyOutcome, LogicalChangeApplier};
use reddb::replication::ReplicationConfig;
use reddb::storage::RedDB;
use reddb::{GrpcServerOptions, RedDBGrpcServer, RedDBOptions, RedDBRuntime};
use tonic::metadata::MetadataValue;
use tonic::transport::Endpoint;

use support::prometheus::get;

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
        r#"{{"id":"p_{username}_replication","version":1,"statements":[{{"effect":"allow","actions":["cluster:replication:stream","cluster:replication:ack"],"resources":["cluster:replication"]}}]}}"#
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

fn bearer_request(message: JsonPayloadRequest, token: &str) -> tonic::Request<JsonPayloadRequest> {
    let mut request = tonic::Request::new(message);
    let value: MetadataValue<_> = format!("Bearer {token}").parse().unwrap();
    request.metadata_mut().insert("authorization", value);
    request
}

fn pull_request(
    replica_id: &str,
    token: &str,
    since_lsn: u64,
    await_data: bool,
) -> tonic::Request<JsonPayloadRequest> {
    bearer_request(
        JsonPayloadRequest {
            payload_json: format!(
                r#"{{"replica_id":"{replica_id}","since_lsn":{since_lsn},"max_count":10,"await_data":{await_data},"await_timeout_ms":5000}}"#
            ),
        },
        token,
    )
}

fn ack_request(
    replica_id: &str,
    token: &str,
    applied_lsn: u64,
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

fn binary_pull_request(
    replica_id: &str,
    token: &str,
    since_lsn: u64,
    await_data: bool,
) -> tonic::Request<WalStreamRequest> {
    let mut request = tonic::Request::new(WalStreamRequest {
        version: 1,
        since_lsn,
        max_count: 10,
        replica_id: Some(replica_id.to_string()),
        term: 1,
        await_data,
        await_timeout_ms: 5_000,
        routing_endpoints: Default::default(),
        max_chunk_bytes: 1024 * 1024,
        installed_snapshot_token: None,
    });
    let value: MetadataValue<_> = format!("Bearer {token}")
        .parse()
        .expect("bearer token is valid metadata");
    request.metadata_mut().insert("authorization", value);
    request
}

async fn first_binary_chunk(
    client: &mut RedDbClient<tonic::transport::Channel>,
    request: tonic::Request<WalStreamRequest>,
) -> WalStreamChunk {
    client
        .stream_wal_records(request)
        .await
        .expect("binary WAL stream opens")
        .into_inner()
        .message()
        .await
        .expect("binary WAL stream remains valid")
        .expect("binary WAL stream returns a metadata chunk")
}

fn decode_binary_records(chunk: &WalStreamChunk) -> Vec<ChangeRecord> {
    chunk
        .records
        .iter()
        .map(|record| ChangeRecord::decode(&record.data).expect("change record decodes"))
        .collect()
}

fn parse_records(body: &serde_json::Value) -> Vec<ChangeRecord> {
    body["records"]
        .as_array()
        .expect("records array")
        .iter()
        .map(|entry| {
            let data = entry["data"].as_str().expect("record data hex");
            let bytes = decode_hex(data).expect("record data decodes");
            ChangeRecord::decode(&bytes).expect("change record decodes")
        })
        .collect()
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("hex input has odd length".to_string());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("invalid hex byte {b:#x}")),
    }
}

fn metric_value(body: &str, prefix: &str) -> u64 {
    for line in body.lines() {
        if let Some(value) = line.strip_prefix(prefix) {
            return value
                .trim()
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("invalid metric value for {prefix:?}: {line}"));
        }
    }
    panic!("metric line not found: {prefix:?}\n{body}");
}

#[tokio::test]
async fn tail_follow_pull_waits_applies_and_resumes_from_slot() {
    let primary = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .expect("primary runtime");
    primary
        .execute_query("CREATE TABLE issue_828_tail (id INTEGER, name TEXT)")
        .expect("create table");

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
    let registered = client
        .pull_wal_records(pull_request("replica_a", &replica_key.key, 0, false))
        .await
        .expect("register replica")
        .into_inner();
    let registered_body: serde_json::Value =
        serde_json::from_str(&registered.payload).expect("registration body");
    let slot_lsn = registered_body["current_lsn"]
        .as_u64()
        .expect("current_lsn");

    let token = replica_key.key.clone();
    let mut tail_client = connect_client(port).await;
    let pending = tokio::spawn(async move {
        tail_client
            .pull_wal_records(pull_request("replica_a", &token, slot_lsn, true))
            .await
            .expect("await-data pull")
            .into_inner()
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !pending.is_finished(),
        "await-data pull must wait for the next WAL record instead of returning an empty poll"
    );

    primary
        .execute_query("INSERT INTO issue_828_tail (id, name) VALUES (1, 'one')")
        .expect("insert first row");
    let pushed = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .expect("tail-follow pull returns after primary write")
        .expect("tail task joins");
    let pushed_body: serde_json::Value =
        serde_json::from_str(&pushed.payload).expect("pushed body");
    let records = parse_records(&pushed_body);
    assert_eq!(records.len(), 1, "tail-follow pull returns the live record");
    assert_eq!(records[0].lsn, slot_lsn + 1);

    let replica = RedDB::new();
    let applier = LogicalChangeApplier::new(slot_lsn);
    applier
        .apply(&replica, &records[0], ApplyMode::Replica)
        .expect("replica applies pushed record");

    client
        .ack_replica_lsn(ack_request("replica_a", &replica_key.key, records[0].lsn))
        .await
        .expect("ack pushed record");
    let (status, metrics) = get(primary.clone(), "/metrics");
    assert_eq!(status, 200, "unexpected metrics response: {metrics}");
    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_lag_records{replica_id=\"replica_a\"} "
        ),
        0,
        "acked streamed apply should drive replica lag to zero without waiting for a poll tick"
    );

    primary
        .execute_query("INSERT INTO issue_828_tail (id, name) VALUES (2, 'two')")
        .expect("insert second row");
    let resumed = client
        .pull_wal_records(pull_request("replica_a", &replica_key.key, 0, false))
        .await
        .expect("resume from slot")
        .into_inner();
    let resumed_body: serde_json::Value =
        serde_json::from_str(&resumed.payload).expect("resumed body");
    let resumed_records = parse_records(&resumed_body);
    assert_eq!(
        resumed_records.len(),
        1,
        "primary must resume from the slot LSN, not the stale caller since_lsn"
    );
    assert_eq!(resumed_records[0].lsn, records[0].lsn + 1);
    applier
        .apply(&replica, &resumed_records[0], ApplyMode::Replica)
        .expect("replica applies resumed record");

    handle.abort();
}

#[tokio::test]
async fn binary_tail_follow_replays_idempotently_when_connection_drops_before_ack() {
    let primary = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .expect("primary runtime");
    primary
        .execute_query("CREATE TABLE issue_828_binary_tail (id INTEGER, name TEXT)")
        .expect("create table");

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store
        .create_user("replica_binary", "p", Role::Read)
        .expect("create replica user");
    let replica_key = store
        .create_api_key("replica_binary", "replication-binary", Role::Read)
        .expect("create replica API key");
    install_replication_policy(&store, "replica_binary");

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
    let registered = first_binary_chunk(
        &mut client,
        binary_pull_request("replica_binary", &replica_key.key, 0, false),
    )
    .await;
    let slot_lsn = registered.current_lsn;

    let token = replica_key.key.clone();
    let mut tail_client = connect_client(port).await;
    let pending = tokio::spawn(async move {
        first_binary_chunk(
            &mut tail_client,
            binary_pull_request("replica_binary", &token, slot_lsn, true),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !pending.is_finished(),
        "binary await-data stream must wait for the next WAL record"
    );

    primary
        .execute_query("INSERT INTO issue_828_binary_tail (id, name) VALUES (1, 'one')")
        .expect("insert row");
    let pushed = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .expect("binary tail-follow returns after primary write")
        .expect("binary tail task joins");
    let records = decode_binary_records(&pushed);
    assert_eq!(records.len(), 1, "binary stream returns the live record");
    assert_eq!(records[0].lsn, slot_lsn + 1);

    let replica = RedDB::new();
    let applier = LogicalChangeApplier::new(slot_lsn);
    assert_eq!(
        applier
            .apply(&replica, &records[0], ApplyMode::Replica)
            .expect("replica applies binary record"),
        ApplyOutcome::Applied
    );

    // Simulate a process/connection loss after apply but before relay/cursor/ACK.
    // The primary slot has not advanced, so reconnecting must replay the same
    // record and the stateful applier must reject it as a duplicate no-op.
    let retried = first_binary_chunk(
        &mut client,
        binary_pull_request("replica_binary", &replica_key.key, slot_lsn, false),
    )
    .await;
    let retried_records = decode_binary_records(&retried);
    assert_eq!(retried_records.len(), 1, "unacked record must be replayed");
    assert_eq!(retried_records[0].lsn, records[0].lsn);
    assert_eq!(
        applier
            .apply(&replica, &retried_records[0], ApplyMode::Replica)
            .expect("duplicate replay remains valid"),
        ApplyOutcome::Idempotent
    );

    client
        .ack_replica_lsn(ack_request(
            "replica_binary",
            &replica_key.key,
            records[0].lsn,
        ))
        .await
        .expect("ack replayed binary record");
    let (status, metrics) = get(primary.clone(), "/metrics");
    assert_eq!(status, 200, "unexpected metrics response: {metrics}");
    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_lag_records{replica_id=\"replica_binary\"} "
        ),
        0,
        "ACK after idempotent retry must advance the primary slot"
    );

    handle.abort();
}
