//! Issue #817 — primary /metrics exports per-replica replication progress.

#[path = "../../support/mod.rs"]
mod support;

use std::sync::Arc;
use std::time::Duration;

use reddb::auth::policies::Policy;
use reddb::auth::store::PrincipalRef;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::JsonPayloadRequest;
use reddb::replication::ReplicationConfig;
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
                r#"{{"replica_id":"{replica_id}","since_lsn":0,"max_count":10}}"#
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
                r#"{{"replica_id":"{replica_id}","applied_lsn":{applied_lsn},"durable_lsn":{durable_lsn},"apply_errors_total":4,"divergence_total":2}}"#
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
async fn metrics_endpoint_exports_registered_replica_progress_and_errors() {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime");

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store.create_user("replica_a", "p", Role::Read).unwrap();
    store.create_user("replica_b", "p", Role::Read).unwrap();
    let replica_a_key = store
        .create_api_key("replica_a", "replication-a", Role::Read)
        .unwrap();
    let replica_b_key = store
        .create_api_key("replica_b", "replication-b", Role::Read)
        .unwrap();
    install_replication_policy(&store, "replica_a");
    install_replication_policy(&store, "replica_b");

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

    client
        .pull_wal_records(pull_request("replica_a", &replica_a_key.key))
        .await
        .expect("replica_a registers");
    client
        .pull_wal_records(pull_request("replica_b", &replica_b_key.key))
        .await
        .expect("replica_b registers");

    runtime
        .execute_query("CREATE TABLE issue_817_replication_metrics (id INTEGER, name TEXT)")
        .expect("create table");
    for id in 1..=3 {
        runtime
            .execute_query(&format!(
                "INSERT INTO issue_817_replication_metrics (id, name) VALUES ({id}, 'r{id}')"
            ))
            .expect("insert row");
    }

    let pull = client
        .pull_wal_records(pull_request("replica_a", &replica_a_key.key))
        .await
        .expect("replica_a pulls WAL")
        .into_inner();
    let pull_body: serde_json::Value = serde_json::from_str(&pull.payload).expect("pull JSON");
    let current_lsn = pull_body["current_lsn"].as_u64().expect("current_lsn");
    assert!(current_lsn >= 2, "test needs nonzero lag; pull={pull_body}");
    let applied_lsn = current_lsn - 1;
    let durable_lsn = current_lsn - 2;

    client
        .ack_replica_lsn(ack_request(
            "replica_a",
            applied_lsn,
            durable_lsn,
            &replica_a_key.key,
        ))
        .await
        .expect("replica_a acks progress");

    let (status, metrics) = get(runtime.clone(), "/metrics");
    assert_eq!(status, 200, "unexpected /metrics response: {metrics}");

    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_lag_records{replica_id=\"replica_a\"} "
        ),
        1
    );
    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_lag_records{replica_id=\"replica_b\"} "
        ),
        0,
        "registered replicas that have not pulled the new LSNs should not use current_lsn lag"
    );
    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_applied_lsn{replica_id=\"replica_a\"} "
        ),
        applied_lsn
    );
    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_durable_lsn{replica_id=\"replica_a\"} "
        ),
        durable_lsn
    );
    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_apply_errors_total{replica_id=\"replica_a\"} "
        ),
        4
    );
    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_divergence_total{replica_id=\"replica_a\"} "
        ),
        2
    );
    assert_eq!(
        metric_value(
            &metrics,
            "reddb_replica_apply_errors_total{replica_id=\"replica_b\"} "
        ),
        0
    );
    assert!(
        !metrics.contains("replica_id=\"ghost\""),
        "metrics must be bounded to the registered-replica set:\n{metrics}"
    );

    handle.abort();
}
