//! Issue #547 — RedWire / gRPC / HTTP envelope conformance.
//!
//! Parent: #449. Pins the four bullets from #547's acceptance:
//!
//! 1. The same query is exercised through all three transports in one
//!    test run, against one shared `RedDBRuntime`.
//! 2. The envelope shape diff is empty for the documented fields —
//!    `statement` (engine-level statement label), `columns`, and the
//!    per-row scalar projection.
//! 3. The result content matches across transports.
//! 4. The test compiles and runs under the standard `cargo test`
//!    target, so it executes in CI alongside the rest of the e2e
//!    suite.
//!
//! Normalisation uses `reddb_client::QueryResult::from_envelope`,
//! which already accommodates the HTTP / RedWire shape (envelope
//! carries `result: {columns, records: [{values: {...}}]}`) and the
//! flat gRPC `result_json` shape (`{columns, records: [{col: val}]}`)
//! via its `values`-or-flat record fallback — we synthesise an
//! envelope around the gRPC `QueryReply` so the same normaliser is
//! the single source of truth for what "equal envelope" means at the
//! user-visible level.

#![cfg(all(feature = "redwire", feature = "embedded"))]

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use reddb::api::RedDBOptions;
use reddb::auth::store::AuthStore;
use reddb::auth::AuthConfig;
use reddb::grpc::proto::query_value::Kind as GrpcValueKind;
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::{QueryRequest, QueryValue};
use reddb::server::RedDBServer;
use reddb::wire::redwire::start_redwire_listener_on;
use reddb::{GrpcServerOptions, RedDBGrpcServer, RedDBRuntime};

use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use reddb_client::{QueryResult, Value};

use serde_json::json;
use tonic::transport::Endpoint;

const TABLE: &str = "cross_xport_envelope";
const SQL: &str = "SELECT id, name FROM cross_xport_envelope WHERE id = $1";

fn pick_free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("seed bind");
    let port = listener.local_addr().expect("seed local_addr").port();
    drop(listener);
    port
}

/// Issue an HTTP/1.1 POST against the running HTTP listener and parse
/// the JSON body. Kept inline so the test does not depend on the
/// support harness used by other e2e files (which would otherwise
/// have to be reopened just for one POST helper).
fn http_post_json(addr: SocketAddr, path: &str, body: &str) -> serde_json::Value {
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
        len = body.len(),
    );
    let mut stream = std::net::TcpStream::connect(addr).expect("http connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("http read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("http write timeout");
    stream
        .write_all(request.as_bytes())
        .expect("http write request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("http read response");
    let split = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b)
        .unwrap_or("");
    serde_json::from_str(split).unwrap_or_else(|err| {
        panic!("HTTP body was not valid JSON: {err}\nraw response:\n{response}")
    })
}

/// Wait for a TCP listener to start accepting on `port`. The two
/// network transports are spawned onto background tasks that need a
/// tick of the runtime before they bind — polling with a 5s deadline
/// is the conservative version of the `sleep(50ms)` pattern the
/// redwire smoke uses.
async fn await_tcp(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("listener on 127.0.0.1:{port} never came up within deadline");
}

async fn drive_grpc(port: u16) -> QueryResult {
    await_tcp(port).await;
    let endpoint = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
        .unwrap()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(5));
    let channel = endpoint.connect().await.expect("gRPC connect");
    let mut client = RedDbClient::new(channel);
    let request = QueryRequest {
        query: SQL.to_string(),
        entity_types: vec![],
        capabilities: vec![],
        params: vec![QueryValue {
            kind: Some(GrpcValueKind::IntValue(1)),
        }],
    };
    let reply = client
        .query(request)
        .await
        .expect("gRPC query rpc")
        .into_inner();
    // Synthesise an envelope around the gRPC reply so the same
    // `from_envelope` parser used for HTTP and RedWire is the sole
    // arbiter of "what counts as the documented envelope".
    let result_value: serde_json::Value = serde_json::from_str(&reply.result_json)
        .expect("gRPC result_json must be valid JSON");
    let envelope = json!({
        "statement": reply.statement,
        "affected_rows": reply.record_count,
        "result": result_value,
    });
    QueryResult::from_envelope(envelope)
}

#[tokio::test]
async fn cross_transport_select_envelope_matches() {
    // One runtime shared by all three transports. Seeding happens once
    // before any listener is online — every transport observes the
    // exact same row set, so any envelope diff is a true diff and not
    // a write-visibility race.
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query(&format!(
            "CREATE TABLE {TABLE} (id INTEGER, name TEXT)"
        ))
        .expect("create table");
    runtime
        .execute_query(&format!(
            "INSERT INTO {TABLE} (id, name) VALUES (1, 'alice')"
        ))
        .expect("insert row");

    // --- HTTP listener (background thread, owns its own bound socket).
    let http_runtime = runtime.clone();
    let http_listener = TcpListener::bind("127.0.0.1:0").expect("http bind");
    let http_addr = http_listener.local_addr().expect("http addr");
    RedDBServer::new(http_runtime).serve_in_background_on(http_listener);

    // --- gRPC listener (tokio task; tonic owns the listener).
    let grpc_port = pick_free_port();
    let grpc_bind = format!("127.0.0.1:{grpc_port}");
    let grpc_runtime = runtime.clone();
    let auth_store = Arc::new(AuthStore::new(AuthConfig::default()));
    let grpc_server = RedDBGrpcServer::with_options(
        grpc_runtime,
        GrpcServerOptions {
            bind_addr: grpc_bind.clone(),
            tls: None,
        },
        auth_store,
    );
    tokio::spawn(async move {
        let _ = grpc_server.serve().await;
    });

    // --- RedWire listener (tokio task; the listener is pre-bound so
    //     we don't race the port between pick_free_port and bind).
    //     `start_redwire_listener_on` pulls the auth store from the
    //     runtime — our runtime has none, so the listener accepts
    //     anonymous like the rest of the redwire smoke tests.
    let wire_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redwire bind");
    let wire_addr: SocketAddr = wire_listener.local_addr().expect("redwire addr");
    let wire_runtime = Arc::new(runtime.clone());
    tokio::spawn(async move {
        let _ = start_redwire_listener_on(wire_listener, wire_runtime).await;
    });

    // --- Drive HTTP.
    let http_body = http_post_json(
        http_addr,
        "/query",
        &json!({ "query": SQL, "params": [1] }).to_string(),
    );
    let http_result = QueryResult::from_envelope(http_body);

    // --- Drive gRPC.
    let grpc_result = drive_grpc(grpc_port).await;

    // --- Drive RedWire. `query_with` on a non-empty param list routes
    //     through `QueryWithParams` (which serialises the same
    //     `runtime_query_json` envelope HTTP returns); the trivial
    //     `Query` frame deliberately omits records, so passing a
    //     placeholder `$1` binding is what surfaces the same envelope
    //     the brief is pinning.
    let mut wire = RedWireClient::connect(
        ConnectOptions::new(wire_addr.ip().to_string(), wire_addr.port())
            .with_auth(Auth::Anonymous),
    )
    .await
    .expect("redwire connect");
    let wire_result = wire
        .query_with(SQL, &[Value::Int(1)])
        .await
        .expect("redwire query_with");
    let _ = wire.close().await;

    // --- Compare documented envelope fields across the three transports.
    let triples = [
        ("http", &http_result),
        ("grpc", &grpc_result),
        ("redwire", &wire_result),
    ];
    let (base_label, base) = triples[0];
    for (label, other) in &triples[1..] {
        assert_eq!(
            base.statement, other.statement,
            "statement diff: {base_label}={:?} vs {label}={:?}",
            base.statement, other.statement
        );
        assert_eq!(
            base.columns, other.columns,
            "columns diff: {base_label}={:?} vs {label}={:?}",
            base.columns, other.columns
        );
        assert_eq!(
            base.rows.len(),
            other.rows.len(),
            "row count diff: {base_label}={} vs {label}={}",
            base.rows.len(),
            other.rows.len(),
        );
        for (i, (br, or)) in base.rows.iter().zip(other.rows.iter()).enumerate() {
            assert_eq!(
                br, or,
                "row {i} diff: {base_label} vs {label}\n  {base_label}={br:?}\n  {label}={or:?}",
            );
        }
    }

    // Sanity floor: the row actually came through. Without this the
    // assertions above would pass for three identical empty results,
    // which would silently mask a transport-side regression.
    assert_eq!(
        http_result.columns,
        vec!["id".to_string(), "name".to_string()],
        "expected the SELECT projection columns to round-trip",
    );
    assert_eq!(
        http_result.rows.len(),
        1,
        "expected one row across all three transports, got {:?}",
        http_result.rows,
    );
}
