//! Issue #826 — replication flow control throttles the primary when an
//! in-quorum replica lags past a soft target, and releases as it recovers.
//!
//! Drives the real write-admission path: a registered in-quorum replica
//! that falls behind the primary's LSN engages the `WriteGate` throttle
//! (observable via `/metrics` and a rejected DML write); acking the
//! replica forward releases it.

mod support;

use std::sync::Arc;
use std::time::Duration;

use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::JsonPayloadRequest;
use reddb::replication::{QuorumConfig, ReplicationConfig};
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
    let policy_id = format!("p_{username}_replication");
    let policy_json = format!(
        r#"{{"id":"{policy_id}","version":1,"statements":[{{"effect":"allow","actions":["cluster:replication:stream","cluster:replication:ack"],"resources":["cluster:replication"]}}]}}"#
    );
    store
        .put_policy(Policy::from_json_str(&policy_json).expect("policy parses"))
        .expect("put policy");
    store
        .attach_policy(PrincipalRef::User(UserId::platform(username)), &policy_id)
        .expect("attach policy");
}

fn bearer_request(message: JsonPayloadRequest, token: &str) -> tonic::Request<JsonPayloadRequest> {
    let mut request = tonic::Request::new(message);
    let value: MetadataValue<_> = format!("Bearer {token}").parse().unwrap();
    request.metadata_mut().insert("authorization", value);
    request
}

fn pull_request(replica_id: &str, token: &str) -> tonic::Request<JsonPayloadRequest> {
    bearer_request(
        JsonPayloadRequest {
            payload_json: format!(
                r#"{{"replica_id":"{replica_id}","since_lsn":0,"max_count":100}}"#
            ),
        },
        token,
    )
}

fn ack_request(
    replica_id: &str,
    applied_lsn: u64,
    durable_lsn: u64,
    token: &str,
) -> tonic::Request<JsonPayloadRequest> {
    bearer_request(
        JsonPayloadRequest {
            payload_json: format!(
                r#"{{"replica_id":"{replica_id}","applied_lsn":{applied_lsn},"durable_lsn":{durable_lsn}}}"#
            ),
        },
        token,
    )
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
async fn flow_control_throttles_on_in_quorum_lag_and_releases_on_recovery() {
    // Sync(1) quorum → a registered replica is an in-quorum member, so
    // its lag drives flow control.
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory()
            .with_replication(ReplicationConfig::primary().with_quorum(QuorumConfig::sync(1))),
    )
    .expect("runtime");

    // Small soft target so a few inserts push the lagging replica past it.
    runtime.write_gate().flow_control().configure_soft_target(2);

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store.create_user("replica_a", "p", Role::Read).unwrap();
    let replica_a_key = store
        .create_api_key("replica_a", "replication-a", Role::Read)
        .unwrap();
    install_replication_policy(&store, "replica_a");

    let port = pick_port();
    let server = RedDBGrpcServer::with_options(
        runtime.clone(),
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

    // Register replica_a (in-quorum) at LSN 0.
    client
        .pull_wal_records(pull_request("replica_a", &replica_a_key.key))
        .await
        .expect("replica_a registers");

    // Initially caught up → no throttle, writes admitted.
    runtime
        .execute_query("CREATE TABLE issue_826_flow (id INTEGER, name TEXT)")
        .expect("create table");
    let (_, metrics) = get(runtime.clone(), "/metrics");
    assert_eq!(
        metric_value(&metrics, "reddb_replication_flow_control_soft_target_lsn "),
        2
    );

    // Advance the primary LSN well past the soft target while replica_a
    // stays behind (it has not acked the new LSNs).
    for id in 1..=8 {
        runtime
            .execute_query(&format!(
                "INSERT INTO issue_826_flow (id, name) VALUES ({id}, 'r{id}')"
            ))
            .expect("insert row");
    }

    // Scrape refreshes flow control from the live registry → throttled.
    let (status, metrics) = get(runtime.clone(), "/metrics");
    assert_eq!(status, 200, "unexpected /metrics response: {metrics}");
    assert_eq!(
        metric_value(&metrics, "reddb_replication_flow_control_throttled "),
        1,
        "in-quorum replica lag should engage throttle:\n{metrics}"
    );
    assert!(
        metric_value(
            &metrics,
            "reddb_replication_flow_control_in_quorum_lag_lsn "
        ) > 2,
        "observed in-quorum lag should exceed soft target:\n{metrics}"
    );

    // Write admission is throttled: a DML write is now rejected.
    let throttled = runtime.execute_query("INSERT INTO issue_826_flow (id, name) VALUES (99, 'x')");
    assert!(
        throttled.is_err(),
        "write must be throttled while in-quorum replica lags: {throttled:?}"
    );

    // Replica catches up: pull then ack the current LSN.
    let pull = client
        .pull_wal_records(pull_request("replica_a", &replica_a_key.key))
        .await
        .expect("replica_a pulls WAL")
        .into_inner();
    let pull_body: serde_json::Value = serde_json::from_str(&pull.payload).expect("pull JSON");
    let current_lsn = pull_body["current_lsn"].as_u64().expect("current_lsn");
    client
        .ack_replica_lsn(ack_request(
            "replica_a",
            current_lsn,
            current_lsn,
            &replica_a_key.key,
        ))
        .await
        .expect("replica_a acks current LSN");

    // Ack-path refresh releases the throttle.
    assert!(
        !runtime.write_gate().is_flow_throttled(),
        "throttle must release once the in-quorum replica catches up"
    );
    let (_, metrics) = get(runtime.clone(), "/metrics");
    assert_eq!(
        metric_value(&metrics, "reddb_replication_flow_control_throttled "),
        0,
        "throttle metric should clear after recovery:\n{metrics}"
    );

    // Writes are admitted again.
    runtime
        .execute_query("INSERT INTO issue_826_flow (id, name) VALUES (100, 'ok')")
        .expect("write admitted after recovery");

    handle.abort();
}
