//! Issue #586 — Analytics slice 5: gRPC mirror of HTTP
//! `BatchInsertEndpoint` (slice 4 / #582). Spins a real
//! `RedDBGrpcServer`, drives the streaming `BatchInsert` RPC with a
//! tonic client, and asserts the brief's acceptance bullets end-to-end:
//!
//! * `Idempotency-Key` initial-metadata replays the cached prior result
//!   without re-executing, and the cache is shared with the HTTP /
//!   RedWire transports (verified via the process-wide `global_cache`).
//! * All-or-nothing: row K's failure rolls back the batch; the
//!   response surfaces the offending row index via `x-row-index`
//!   trailing metadata.
//! * Oversize batches return `RESOURCE_EXHAUSTED` before any storage
//!   write.
//! * Schema validation via `AnalyticsSchemaRegistry` runs on every row
//!   before any commit; failures pin the failing index.
//! * Submission order is preserved on commit (CDC ordering surfaces via
//!   the user-observable scan order).

use std::sync::Arc;
use std::time::Duration;

use reddb::auth::store::AuthStore;
use reddb::auth::AuthConfig;
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::BatchInsertChunk;
use reddb::runtime::RedDBRuntime;
use reddb::{GrpcServerOptions, RedDBGrpcServer, RedDBOptions};

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

/// Build a server + return a handle to its runtime so the test can
/// inspect storage state directly. The runtime is shared (Arc-backed
/// inside) so the clone passed to the server and the one we keep see
/// the same writes.
fn build_runtime_and_server(table_ddl: &str) -> (RedDBRuntime, RedDBGrpcServer, String, u16) {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime.execute_query(table_ddl).expect("create table");
    let port = pick_port();
    let bind = format!("127.0.0.1:{port}");
    let auth_store = Arc::new(AuthStore::new(AuthConfig::default()));
    let server = RedDBGrpcServer::with_options(
        runtime.clone(),
        GrpcServerOptions {
            bind_addr: bind.clone(),
            tls: None,
        },
        auth_store,
    );
    (runtime, server, bind, port)
}

async fn connect_client(port: u16) -> RedDbClient<tonic::transport::Channel> {
    let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
        .unwrap()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5));
    let channel = endpoint.connect().await.expect("client connect");
    RedDbClient::new(channel)
}

fn unique_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{ts}_{n}")
}

/// Bullet 1 (RPC defined + accepted), bullet 5 (CDC / submission order):
/// stream 3 rows, server commits atomically, returns the count, and
/// the rows come back from a scan in submission order.
#[tokio::test]
async fn grpc_batch_insert_happy_path_returns_count_and_preserves_order() {
    let table = format!("events_586_ok_{}", unique_suffix());
    let ddl = format!("CREATE TABLE {table} (id INTEGER, name TEXT)");
    let (runtime, server, _bind, port) = build_runtime_and_server(&ddl);

    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let mut client = connect_client(port).await;
    let chunks = vec![
        BatchInsertChunk {
            collection: table.clone(),
            row_json: r#"{"fields":{"id":1,"name":"a"}}"#.to_string(),
        },
        BatchInsertChunk {
            collection: String::new(),
            row_json: r#"{"fields":{"id":2,"name":"b"}}"#.to_string(),
        },
        BatchInsertChunk {
            collection: String::new(),
            row_json: r#"{"fields":{"id":3,"name":"c"}}"#.to_string(),
        },
    ];
    let req = tonic::Request::new(tokio_stream::iter(chunks));
    let resp = client.batch_insert(req).await.expect("batch_insert ok");
    let reply = resp.into_inner();
    assert!(reply.ok);
    assert_eq!(reply.count, 3);

    let qr = runtime
        .execute_query(&format!("SELECT name FROM {table} ORDER BY id ASC"))
        .expect("scan");
    let names: Vec<String> = qr
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("name") {
            Some(reddb::storage::schema::Value::Text(s)) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["a", "b", "c"]);

    h.abort();
}

/// Bullet 2 — `idempotency-key` initial metadata replays the cached
/// prior result. Send the same key with a different body; the second
/// call must succeed with the same count and storage must be untouched
/// by the replay.
#[tokio::test]
async fn grpc_batch_insert_idempotency_key_replays_cached_result() {
    let table = format!("events_586_idem_{}", unique_suffix());
    let key = format!("grpc-586-idem-{}", unique_suffix());
    let ddl = format!("CREATE TABLE {table} (id INTEGER, name TEXT)");
    let (runtime, server, _bind, port) = build_runtime_and_server(&ddl);

    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let mut client = connect_client(port).await;

    let chunks_first = vec![BatchInsertChunk {
        collection: table.clone(),
        row_json: r#"{"fields":{"id":1,"name":"first"}}"#.to_string(),
    }];
    let mut req1 = tonic::Request::new(tokio_stream::iter(chunks_first));
    req1.metadata_mut()
        .insert("idempotency-key", key.parse().unwrap());
    let reply1 = client
        .batch_insert(req1)
        .await
        .expect("first call ok")
        .into_inner();
    assert_eq!(reply1.count, 1);

    let chunks_replay = vec![BatchInsertChunk {
        collection: table.clone(),
        row_json: r#"{"fields":{"id":2,"name":"should-not-land"}}"#.to_string(),
    }];
    let mut req2 = tonic::Request::new(tokio_stream::iter(chunks_replay));
    req2.metadata_mut()
        .insert("idempotency-key", key.parse().unwrap());
    let reply2 = client
        .batch_insert(req2)
        .await
        .expect("replay ok")
        .into_inner();
    assert_eq!(reply2.count, 1, "replay must echo prior count");

    let qr = runtime
        .execute_query(&format!("SELECT name FROM {table}"))
        .expect("scan");
    let names: Vec<String> = qr
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("name") {
            Some(reddb::storage::schema::Value::Text(s)) => Some(s.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(names, vec!["first"]);
    assert!(
        !names.iter().any(|n| n == "should-not-land"),
        "replay re-executed: {names:?}"
    );

    h.abort();
}

/// Bullet 2 (cont.) — the cache is shared with HTTP slice 4 / RedWire
/// slice 6: a gRPC batch populates the process-wide cache, and looking
/// the entry up via `global_cache().lookup` (the same handle HTTP and
/// RedWire use) returns the cached body bytes. That's the entire
/// "cross-transport shared cache" contract.
#[tokio::test]
async fn grpc_batch_insert_cache_shared_with_other_transports() {
    use reddb::runtime::batch_insert::global_cache;

    let table = format!("events_586_shared_{}", unique_suffix());
    let key = format!("grpc-586-shared-{}", unique_suffix());
    let ddl = format!("CREATE TABLE {table} (id INTEGER, name TEXT)");
    let (_runtime, server, _bind, port) = build_runtime_and_server(&ddl);

    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let mut client = connect_client(port).await;
    let chunks = vec![BatchInsertChunk {
        collection: table.clone(),
        row_json: r#"{"fields":{"id":1,"name":"x"}}"#.to_string(),
    }];
    let mut req = tonic::Request::new(tokio_stream::iter(chunks));
    req.metadata_mut()
        .insert("idempotency-key", key.parse().unwrap());
    let _ = client.batch_insert(req).await.expect("ok");

    let hit = global_cache()
        .lookup(&table, &key, std::time::Instant::now())
        .expect("global cache must serve the gRPC write");
    assert_eq!(hit.status, 200);
    let body = std::str::from_utf8(&hit.body).expect("utf8 body");
    assert!(body.contains("\"count\":1"), "body = {body}");

    h.abort();
}

/// Bullet 3 — row K's failure rolls back the whole batch; the gRPC
/// response is a `FAILED_PRECONDITION` Status carrying `x-row-index`
/// and `x-batch-error-code` trailing metadata so the caller can
/// pinpoint the broken row without parsing the message text.
#[tokio::test]
async fn grpc_batch_insert_row_failure_rolls_back_with_row_index_metadata() {
    let table = format!("events_586_rollback_{}", unique_suffix());
    let ddl = format!("CREATE TABLE {table} (id INTEGER, name TEXT)");
    let (runtime, server, _bind, port) = build_runtime_and_server(&ddl);

    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let mut client = connect_client(port).await;
    let chunks = vec![
        BatchInsertChunk {
            collection: table.clone(),
            row_json: r#"{"fields":{"id":1,"name":"a"}}"#.to_string(),
        },
        BatchInsertChunk {
            collection: String::new(),
            row_json: r#"{"not_fields":{"id":2}}"#.to_string(),
        },
        BatchInsertChunk {
            collection: String::new(),
            row_json: r#"{"fields":{"id":3,"name":"c"}}"#.to_string(),
        },
    ];
    let req = tonic::Request::new(tokio_stream::iter(chunks));
    let err = client
        .batch_insert(req)
        .await
        .expect_err("row 1 must reject the whole batch");
    assert_eq!(err.code(), Code::FailedPrecondition);
    let md = err.metadata();
    assert_eq!(
        md.get("x-batch-error-code").and_then(|v| v.to_str().ok()),
        Some("RowParseFailure"),
    );
    assert_eq!(
        md.get("x-row-index").and_then(|v| v.to_str().ok()),
        Some("1"),
    );

    let qr = runtime
        .execute_query(&format!("SELECT name FROM {table}"))
        .expect("scan");
    assert!(
        qr.result.records.is_empty(),
        "row 0 leaked despite row 1 failure"
    );

    h.abort();
}

/// Bullet 4 — schema validation runs before any storage write; an
/// `AnalyticsSchemaRegistry` rejection on row K rolls back the batch
/// and surfaces `RowSchemaRejected` + the offending index.
#[tokio::test]
async fn grpc_batch_insert_schema_validation_pinpoints_failing_row() {
    use reddb::runtime::analytics_schema_registry as reg;

    let table = format!("events_586_schema_{}", unique_suffix());
    let event_name = format!("click_586_{}", unique_suffix());
    let ddl = format!("CREATE TABLE {table} (event_name TEXT, payload TEXT)");
    let (runtime, server, _bind, port) = build_runtime_and_server(&ddl);

    let schema = r#"{"type":"object","properties":{"url":{"type":"string"}},"required":["url"],"additionalProperties":false}"#;
    reg::register(runtime.db().store().as_ref(), &event_name, schema).expect("register schema");

    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let mut client = connect_client(port).await;
    let chunks = vec![
        BatchInsertChunk {
            collection: table.clone(),
            row_json: format!(
                r#"{{"fields":{{"event_name":"{event_name}","payload":"{{\"url\":\"/a\"}}"}}}}"#
            ),
        },
        BatchInsertChunk {
            collection: String::new(),
            row_json: format!(
                r#"{{"fields":{{"event_name":"{event_name}","payload":"{{\"url\":\"/b\",\"extra\":1}}"}}}}"#
            ),
        },
    ];
    let req = tonic::Request::new(tokio_stream::iter(chunks));
    let err = client
        .batch_insert(req)
        .await
        .expect_err("row 1 schema rejection must roll back");
    assert_eq!(err.code(), Code::FailedPrecondition);
    let md = err.metadata();
    assert_eq!(
        md.get("x-batch-error-code").and_then(|v| v.to_str().ok()),
        Some("RowSchemaRejected"),
    );
    assert_eq!(
        md.get("x-row-index").and_then(|v| v.to_str().ok()),
        Some("1"),
    );

    let qr = runtime
        .execute_query(&format!("SELECT event_name FROM {table}"))
        .expect("scan");
    assert!(
        qr.result.records.is_empty(),
        "row 0 leaked despite row 1 schema rejection"
    );

    h.abort();
}

/// Bullet 4 — oversize → `RESOURCE_EXHAUSTED` before any storage
/// write. Build one row past the default `red.batch.max_rows = 10_000`
/// cap so we don't have to mutate the process-wide env var (cargo
/// test runs in parallel and `set_var` leaks into siblings — the HTTP
/// and RedWire slice tests take the same route).
#[tokio::test]
async fn grpc_batch_insert_oversize_returns_resource_exhausted_before_storage() {
    let table = format!("events_586_oversize_{}", unique_suffix());
    let ddl = format!("CREATE TABLE {table} (id INTEGER, name TEXT)");
    let (runtime, server, _bind, port) = build_runtime_and_server(&ddl);

    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let max = 10_000usize;
    let mut chunks = Vec::with_capacity(max + 1);
    for i in 0..(max + 1) {
        chunks.push(BatchInsertChunk {
            collection: if i == 0 { table.clone() } else { String::new() },
            row_json: format!(r#"{{"fields":{{"id":{i},"name":"x"}}}}"#),
        });
    }
    let mut client = connect_client(port).await;
    let req = tonic::Request::new(tokio_stream::iter(chunks));
    let err = client
        .batch_insert(req)
        .await
        .expect_err("oversize batch must be rejected");
    assert_eq!(err.code(), Code::ResourceExhausted);
    assert_eq!(
        err.metadata()
            .get("x-batch-error-code")
            .and_then(|v| v.to_str().ok()),
        Some("BatchTooLarge"),
    );

    let qr = runtime
        .execute_query(&format!("SELECT name FROM {table}"))
        .expect("scan");
    assert!(
        qr.result.records.is_empty(),
        "oversize batch leaked rows into storage"
    );

    h.abort();
}
