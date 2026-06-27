//! Issue #1354 — Cross-transport result equivalence harness.
//!
//! Runs the same operation set — CREATE TABLE, INSERT, SELECT with ORDER BY —
//! through all five transports (embedded, HTTP, gRPC, RedWire, PG-wire) wired
//! to a single shared in-memory [`RedDBRuntime`].  Asserts that each transport
//! produces identical observable results: same column projection, same row
//! count, same per-cell values.
//!
//! Normalization: every transport's output is reduced to
//! `(Vec<String> columns, Vec<Vec<String>> rows)` where each cell is a
//! display string using the rules that are consistent across the five
//! wire encodings:
//! - INTEGER → decimal digits (no decimal point)
//! - TEXT    → content as-is (no surrounding quotes)
//! - NULL    → literal `"null"`
//!
//! A divergence is reported by naming the offending transport alongside the
//! baseline (embedded) and the mismatching transport's actual values.

#![cfg(all(feature = "redwire", feature = "embedded"))]

#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use reddb::auth::store::AuthStore;
use reddb::auth::AuthConfig;
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::QueryRequest;
use reddb::server::RedDBServer;
use reddb::storage::schema::Value as StorageValue;
use reddb::wire::postgres::{start_pg_wire_listener, PgWireConfig};
use reddb::wire::redwire::start_redwire_listener_on;
use reddb::{GrpcServerOptions, RedDBGrpcServer, RedDBOptions, RedDBRuntime};

use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use reddb_client::{QueryResult, Value as ClientValue, ValueOut};

use serde_json::json;
use tonic::transport::Endpoint;

const TABLE: &str = "xport_equiv_1354";
const SELECT_SQL: &str = "SELECT id, label FROM xport_equiv_1354 ORDER BY id";
/// RedWire's plain `Query` frame returns a summary envelope only (no column
/// or row data) — this is documented protocol behaviour, not a bug.  For
/// result-row parity we use `QueryWithParams` (triggered by passing at least
/// one parameter), which serialises the full `runtime_query_json` envelope.
/// `WHERE id > $1` with `$1 = 0` is semantically equivalent to the
/// unfiltered SELECT since all inserted rows have id ∈ {1, 2, 3}.
const SELECT_SQL_REDWIRE: &str = "SELECT id, label FROM xport_equiv_1354 WHERE id > $1 ORDER BY id";

/// Transport-independent normalized result: (column names, rows-as-strings).
type NormResult = (Vec<String>, Vec<Vec<String>>);
type DocumentCrudTrace = Vec<(&'static str, NormResult)>;

fn pick_free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("port pick bind");
    let port = l.local_addr().expect("port pick addr").port();
    drop(l);
    port
}

/// POST a JSON body to an HTTP/1.1 server and return the parsed response body.
fn http_post_json(addr: SocketAddr, path: &str, body: &str) -> serde_json::Value {
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
        len = body.len(),
    );
    let mut stream = std::net::TcpStream::connect(addr).expect("http connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("write timeout");
    stream.write_all(req.as_bytes()).expect("http write");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("http read");
    let body_str = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
    serde_json::from_str(body_str)
        .unwrap_or_else(|e| panic!("HTTP response not valid JSON: {e}\nraw:\n{raw}"))
}

/// Wait until a TCP listener accepts connections on `port` (up to 10 s).
async fn await_tcp(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("127.0.0.1:{port} never came up within 10 s");
}

// ---------------------------------------------------------------------------
// Per-cell normalization helpers
// ---------------------------------------------------------------------------

fn norm_cell_value_out(v: &ValueOut) -> String {
    match v {
        ValueOut::Null => "null".to_string(),
        ValueOut::Bool(b) => b.to_string(),
        ValueOut::Integer(n) => n.to_string(),
        ValueOut::Float(f) => format!("{f}"),
        ValueOut::String(s) => s.clone(),
    }
}

fn norm_from_query_result(qr: &QueryResult) -> NormResult {
    let cols = qr.columns.clone();
    let rows = qr
        .rows
        .iter()
        .map(|row| row.iter().map(|(_, v)| norm_cell_value_out(v)).collect())
        .collect();
    (cols, rows)
}

fn norm_from_runtime_query_result(qr: reddb::runtime::RuntimeQueryResult) -> NormResult {
    let cols = qr.result.columns.clone();
    let rows = qr
        .result
        .records
        .iter()
        .map(|rec| {
            cols.iter()
                .map(|col| match rec.get(col) {
                    None | Some(StorageValue::Null) => "null".to_string(),
                    Some(StorageValue::Integer(n)) => n.to_string(),
                    Some(StorageValue::UnsignedInteger(n)) => n.to_string(),
                    Some(StorageValue::Text(s)) => s.to_string(),
                    Some(StorageValue::Boolean(b)) => b.to_string(),
                    Some(StorageValue::Float(f)) => format!("{f}"),
                    Some(other) => format!("{other}"),
                })
                .collect()
        })
        .collect();
    (cols, rows)
}

// ---------------------------------------------------------------------------
// Transport drivers
// ---------------------------------------------------------------------------

/// Embedded: direct synchronous call into the shared runtime.
fn drive_embedded(runtime: &RedDBRuntime) -> NormResult {
    drive_embedded_sql(runtime, SELECT_SQL)
}

fn drive_embedded_sql(runtime: &RedDBRuntime, sql: &str) -> NormResult {
    norm_from_runtime_query_result(runtime.execute_query(sql).expect("embedded execute_query"))
}

/// HTTP: raw HTTP/1.1 POST to `/query`, parse JSON response envelope.
fn drive_http(addr: SocketAddr) -> NormResult {
    drive_http_sql(addr, SELECT_SQL)
}

fn drive_http_sql(addr: SocketAddr, sql: &str) -> NormResult {
    let body = json!({ "query": sql }).to_string();
    let envelope = http_post_json(addr, "/query", &body);
    let qr = QueryResult::from_envelope(envelope);
    norm_from_query_result(&qr)
}

/// gRPC: tonic client, synthesise an HTTP-style envelope from the reply.
async fn drive_grpc(port: u16) -> NormResult {
    drive_grpc_sql(port, SELECT_SQL).await
}

async fn drive_grpc_sql(port: u16, sql: &str) -> NormResult {
    await_tcp(port).await;
    let ep = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
        .expect("grpc endpoint")
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5));
    let ch = ep.connect().await.expect("grpc connect");
    let mut client = RedDbClient::new(ch);
    let reply = client
        .query(QueryRequest {
            query: sql.to_string(),
            entity_types: vec![],
            capabilities: vec![],
            params: vec![],
        })
        .await
        .expect("grpc query rpc")
        .into_inner();
    let result_json: serde_json::Value =
        serde_json::from_str(&reply.result_json).expect("grpc result_json");
    let envelope = json!({
        "statement": reply.statement,
        "affected_rows": reply.record_count,
        "result": result_json,
    });
    let qr = QueryResult::from_envelope(envelope);
    norm_from_query_result(&qr)
}

/// RedWire: reference client over the wire protocol.
///
/// Must use `query_with` (which sends a `QueryWithParams` frame) to obtain
/// the full `runtime_query_json` envelope including columns and rows.
/// The plain `Query` frame only returns a summary (statement + affected count)
/// so we pass `WHERE id > $1` with `$1 = 0` — semantically identical to the
/// unfiltered SELECT since all rows have id ∈ {1, 2, 3}.
async fn drive_redwire(addr: SocketAddr) -> NormResult {
    drive_redwire_sql(addr, SELECT_SQL_REDWIRE).await
}

async fn drive_redwire_sql(addr: SocketAddr, sql: &str) -> NormResult {
    let mut c = RedWireClient::connect(
        ConnectOptions::new(addr.ip().to_string(), addr.port()).with_auth(Auth::Anonymous),
    )
    .await
    .expect("redwire connect");
    let qr = c
        .query_with(sql, &[ClientValue::Int(0)])
        .await
        .expect("redwire query_with");
    let _ = c.close().await;
    norm_from_query_result(&qr)
}

async fn drive_redwire_command_sql(addr: SocketAddr, sql: &str) {
    let mut c = RedWireClient::connect(
        ConnectOptions::new(addr.ip().to_string(), addr.port()).with_auth(Auth::Anonymous),
    )
    .await
    .expect("redwire connect");
    c.query(sql).await.expect("redwire query");
    let _ = c.close().await;
}

/// PG-wire: raw TCP with the PostgreSQL frontend/backend protocol.
/// Uses the simple-query flow (Q frame) and text-format row values.
async fn drive_pgwire(addr: SocketAddr) -> NormResult {
    drive_pgwire_sql(addr, SELECT_SQL).await
}

async fn drive_pgwire_sql(addr: SocketAddr, sql: &str) -> NormResult {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(addr)
        .await
        .expect("pgwire connect");

    // --- Startup (protocol 3.0, user "reddb")
    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(&(196_608u32).to_be_bytes()); // protocol 3.0
    payload.extend_from_slice(b"user\0reddb\0");
    payload.push(0); // parameter list terminator
    let total_len = (payload.len() + 4) as u32;
    s.write_all(&total_len.to_be_bytes())
        .await
        .expect("pg startup len");
    s.write_all(&payload).await.expect("pg startup payload");

    // Drain until the server sends ReadyForQuery ('Z')
    loop {
        let tag = s.read_u8().await.expect("pg startup tag");
        let body_len = (s.read_i32().await.expect("pg startup len") as usize).saturating_sub(4);
        let mut body = vec![0u8; body_len];
        s.read_exact(&mut body).await.expect("pg startup body");
        if tag == b'Z' {
            break;
        }
    }

    // --- Simple Query ('Q')
    let sql = format!("{sql}\0");
    let qmsg_len = (sql.len() + 4) as u32;
    s.write_all(&[b'Q']).await.expect("pg Q tag");
    s.write_all(&qmsg_len.to_be_bytes())
        .await
        .expect("pg Q len");
    s.write_all(sql.as_bytes()).await.expect("pg Q sql");

    // --- Parse response frames
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut error: Option<String> = None;

    loop {
        let tag = s.read_u8().await.expect("pg response tag");
        let body_len = (s.read_i32().await.expect("pg response len") as usize).saturating_sub(4);
        let mut body = vec![0u8; body_len];
        s.read_exact(&mut body).await.expect("pg response body");

        match tag {
            b'T' => {
                // RowDescription: decode column names
                let col_count = i16::from_be_bytes([body[0], body[1]]) as usize;
                let mut pos = 2;
                for _ in 0..col_count {
                    let end = body[pos..]
                        .iter()
                        .position(|&b| b == 0)
                        .map(|o| pos + o)
                        .expect("pg column name null terminator");
                    let name = std::str::from_utf8(&body[pos..end])
                        .expect("pg column name utf8")
                        .to_string();
                    columns.push(name);
                    // Skip null byte + 18 bytes of metadata
                    // (table_oid 4, col_attr 2, type_oid 4, type_sz 2, type_mod 4, format 2)
                    pos = end + 1 + 18;
                }
            }
            b'D' => {
                // DataRow: decode text-format cell values
                let col_count = i16::from_be_bytes([body[0], body[1]]) as usize;
                let mut pos = 2;
                let mut row = Vec::with_capacity(col_count);
                for _ in 0..col_count {
                    let cell_len = i32::from_be_bytes([
                        body[pos],
                        body[pos + 1],
                        body[pos + 2],
                        body[pos + 3],
                    ]);
                    pos += 4;
                    if cell_len < 0 {
                        row.push("null".to_string());
                    } else {
                        let n = cell_len as usize;
                        let s = std::str::from_utf8(&body[pos..pos + n])
                            .expect("pg cell utf8")
                            .to_string();
                        row.push(s);
                        pos += n;
                    }
                }
                rows.push(row);
            }
            b'E' => {
                error = Some(String::from_utf8_lossy(&body).to_string());
            }
            b'Z' => break, // ReadyForQuery — response complete
            _ => {}        // CommandComplete, Notice, Error, etc.
        }
    }

    // Terminate cleanly
    let _ = s.write_all(&[b'X', 0, 0, 0, 4]).await;

    if let Some(error) = error {
        panic!(
            "pgwire query failed for `{}`: {error}",
            sql.trim_end_matches('\0')
        );
    }

    (columns, rows)
}

fn create_document_sql(collection: &str) -> String {
    format!("CREATE DOCUMENT {collection}")
}

fn insert_document_sql(collection: &str) -> String {
    format!(
        "INSERT INTO {collection} DOCUMENT (body) VALUES \
         ('{{\"name\":\"alpha\",\"score\":10,\"keep\":\"sibling\",\"status\":\"draft\"}}')"
    )
}

fn partial_update_document_sql(collection: &str) -> String {
    format!("UPDATE {collection} DOCUMENTS SET score += 5 WHERE name = 'alpha'")
}

fn replace_document_sql(collection: &str) -> String {
    format!(
        "UPDATE {collection} DOCUMENTS \
         SET name = 'beta', score = 99, keep = 'replacement', status = 'done' \
         WHERE name = 'alpha'"
    )
}

fn delete_document_sql(collection: &str) -> String {
    format!("DELETE FROM {collection} WHERE name = 'beta'")
}

fn select_document_sql(collection: &str, name: &str) -> String {
    format!("SELECT name, score, keep, status FROM {collection} WHERE name = '{name}'")
}

fn select_document_sql_redwire(collection: &str, name: &str) -> String {
    format!("SELECT name, score, keep, status FROM {collection} WHERE name = '{name}' AND $1 = 0")
}

fn count_document_sql(collection: &str) -> String {
    format!("SELECT COUNT(*) AS remaining FROM {collection} WHERE name = 'beta'")
}

fn count_document_sql_redwire(collection: &str) -> String {
    format!("SELECT COUNT(*) AS remaining FROM {collection} WHERE name = 'beta' AND $1 = 0")
}

fn assert_document_crud_trace_shape(trace: &DocumentCrudTrace) {
    let expected_columns = vec![
        "name".to_string(),
        "score".to_string(),
        "keep".to_string(),
        "status".to_string(),
    ];
    let expected_insert = vec![vec![
        "alpha".to_string(),
        "10".to_string(),
        "sibling".to_string(),
        "draft".to_string(),
    ]];
    let expected_partial = vec![vec![
        "alpha".to_string(),
        "15".to_string(),
        "sibling".to_string(),
        "draft".to_string(),
    ]];
    let expected_replace = vec![vec![
        "beta".to_string(),
        "99".to_string(),
        "replacement".to_string(),
        "done".to_string(),
    ]];

    assert_eq!(trace[0].0, "after_insert");
    assert_eq!(trace[0].1 .0, expected_columns);
    assert_eq!(trace[0].1 .1, expected_insert);
    assert_eq!(trace[1].0, "after_partial_update");
    assert_eq!(trace[1].1 .0, expected_columns);
    assert_eq!(
        trace[1].1 .1, expected_partial,
        "partial document update must preserve sibling fields"
    );
    assert_eq!(trace[2].0, "after_replace");
    assert_eq!(trace[2].1 .0, expected_columns);
    assert_eq!(trace[2].1 .1, expected_replace);
    assert_eq!(trace[3].0, "after_delete");
    assert_eq!(trace[3].1 .0, vec!["remaining".to_string()]);
    assert_eq!(trace[3].1 .1, vec![vec!["0".to_string()]]);
}

fn run_embedded_document_crud(runtime: &RedDBRuntime, collection: &str) -> DocumentCrudTrace {
    drive_embedded_sql(runtime, &create_document_sql(collection));
    drive_embedded_sql(runtime, &insert_document_sql(collection));
    let after_insert = drive_embedded_sql(runtime, &select_document_sql(collection, "alpha"));
    drive_embedded_sql(runtime, &partial_update_document_sql(collection));
    let after_partial = drive_embedded_sql(runtime, &select_document_sql(collection, "alpha"));
    drive_embedded_sql(runtime, &replace_document_sql(collection));
    let after_replace = drive_embedded_sql(runtime, &select_document_sql(collection, "beta"));
    drive_embedded_sql(runtime, &delete_document_sql(collection));
    let after_delete = drive_embedded_sql(runtime, &count_document_sql(collection));
    vec![
        ("after_insert", after_insert),
        ("after_partial_update", after_partial),
        ("after_replace", after_replace),
        ("after_delete", after_delete),
    ]
}

fn run_http_document_crud(addr: SocketAddr, collection: &str) -> DocumentCrudTrace {
    drive_http_sql(addr, &create_document_sql(collection));
    drive_http_sql(addr, &insert_document_sql(collection));
    let after_insert = drive_http_sql(addr, &select_document_sql(collection, "alpha"));
    drive_http_sql(addr, &partial_update_document_sql(collection));
    let after_partial = drive_http_sql(addr, &select_document_sql(collection, "alpha"));
    drive_http_sql(addr, &replace_document_sql(collection));
    let after_replace = drive_http_sql(addr, &select_document_sql(collection, "beta"));
    drive_http_sql(addr, &delete_document_sql(collection));
    let after_delete = drive_http_sql(addr, &count_document_sql(collection));
    vec![
        ("after_insert", after_insert),
        ("after_partial_update", after_partial),
        ("after_replace", after_replace),
        ("after_delete", after_delete),
    ]
}

async fn run_grpc_document_crud(port: u16, collection: &str) -> DocumentCrudTrace {
    drive_grpc_sql(port, &create_document_sql(collection)).await;
    drive_grpc_sql(port, &insert_document_sql(collection)).await;
    let after_insert = drive_grpc_sql(port, &select_document_sql(collection, "alpha")).await;
    drive_grpc_sql(port, &partial_update_document_sql(collection)).await;
    let after_partial = drive_grpc_sql(port, &select_document_sql(collection, "alpha")).await;
    drive_grpc_sql(port, &replace_document_sql(collection)).await;
    let after_replace = drive_grpc_sql(port, &select_document_sql(collection, "beta")).await;
    drive_grpc_sql(port, &delete_document_sql(collection)).await;
    let after_delete = drive_grpc_sql(port, &count_document_sql(collection)).await;
    vec![
        ("after_insert", after_insert),
        ("after_partial_update", after_partial),
        ("after_replace", after_replace),
        ("after_delete", after_delete),
    ]
}

async fn run_redwire_document_crud(addr: SocketAddr, collection: &str) -> DocumentCrudTrace {
    drive_redwire_command_sql(addr, &create_document_sql(collection)).await;
    drive_redwire_command_sql(addr, &insert_document_sql(collection)).await;
    let after_insert =
        drive_redwire_sql(addr, &select_document_sql_redwire(collection, "alpha")).await;
    drive_redwire_command_sql(addr, &partial_update_document_sql(collection)).await;
    let after_partial =
        drive_redwire_sql(addr, &select_document_sql_redwire(collection, "alpha")).await;
    drive_redwire_command_sql(addr, &replace_document_sql(collection)).await;
    let after_replace =
        drive_redwire_sql(addr, &select_document_sql_redwire(collection, "beta")).await;
    drive_redwire_command_sql(addr, &delete_document_sql(collection)).await;
    let after_delete = drive_redwire_sql(addr, &count_document_sql_redwire(collection)).await;
    vec![
        ("after_insert", after_insert),
        ("after_partial_update", after_partial),
        ("after_replace", after_replace),
        ("after_delete", after_delete),
    ]
}

async fn run_pgwire_document_crud(addr: SocketAddr, collection: &str) -> DocumentCrudTrace {
    drive_pgwire_sql(addr, &create_document_sql(collection)).await;
    drive_pgwire_sql(addr, &insert_document_sql(collection)).await;
    let after_insert = drive_pgwire_sql(addr, &select_document_sql(collection, "alpha")).await;
    drive_pgwire_sql(addr, &partial_update_document_sql(collection)).await;
    let after_partial = drive_pgwire_sql(addr, &select_document_sql(collection, "alpha")).await;
    drive_pgwire_sql(addr, &replace_document_sql(collection)).await;
    let after_replace = drive_pgwire_sql(addr, &select_document_sql(collection, "beta")).await;
    drive_pgwire_sql(addr, &delete_document_sql(collection)).await;
    let after_delete = drive_pgwire_sql(addr, &count_document_sql(collection)).await;
    vec![
        ("after_insert", after_insert),
        ("after_partial_update", after_partial),
        ("after_replace", after_replace),
        ("after_delete", after_delete),
    ]
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cross_transport_select_results_are_equivalent() {
    // One in-memory runtime shared across all five transports.
    // Data is seeded once via the embedded path before any listener starts,
    // so transport start-up races cannot create a read-before-write window.
    let runtime =
        Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"));

    runtime
        .execute_query(&format!("CREATE TABLE {TABLE} (id INTEGER, label TEXT)"))
        .expect("create table");
    runtime
        .execute_query(&format!(
            "INSERT INTO {TABLE} (id, label) VALUES (1, 'alpha')"
        ))
        .expect("insert 1");
    runtime
        .execute_query(&format!(
            "INSERT INTO {TABLE} (id, label) VALUES (2, 'beta')"
        ))
        .expect("insert 2");
    runtime
        .execute_query(&format!(
            "INSERT INTO {TABLE} (id, label) VALUES (3, 'gamma')"
        ))
        .expect("insert 3");

    // --- Embedded baseline (no network hop)
    let embedded = drive_embedded(&runtime);

    // --- HTTP
    let http_listener = TcpListener::bind("127.0.0.1:0").expect("http bind");
    let http_addr = http_listener.local_addr().expect("http addr");
    RedDBServer::new(runtime.as_ref().clone()).serve_in_background_on(http_listener);
    let http = drive_http(http_addr);

    // --- gRPC
    let grpc_port = pick_free_port();
    let grpc_opts = GrpcServerOptions {
        bind_addr: format!("127.0.0.1:{grpc_port}"),
        tls: None,
    };
    let grpc_auth = Arc::new(AuthStore::new(AuthConfig::default()));
    let grpc_server = RedDBGrpcServer::with_options(runtime.as_ref().clone(), grpc_opts, grpc_auth);
    tokio::spawn(async move {
        let _ = grpc_server.serve().await;
    });
    let grpc = drive_grpc(grpc_port).await;

    // --- RedWire
    let wire_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redwire bind");
    let wire_addr = wire_listener.local_addr().expect("redwire addr");
    tokio::spawn({
        let rt = Arc::clone(&runtime);
        async move {
            let _ = start_redwire_listener_on(wire_listener, rt).await;
        }
    });
    let redwire = drive_redwire(wire_addr).await;

    // --- PG-wire
    let pg_port = pick_free_port();
    let pg_cfg = PgWireConfig {
        bind_addr: format!("127.0.0.1:{pg_port}"),
        ..PgWireConfig::default()
    };
    tokio::spawn({
        let rt = Arc::clone(&runtime);
        async move {
            let _ = start_pg_wire_listener(pg_cfg, rt).await;
        }
    });
    await_tcp(pg_port).await;
    let pg_addr: SocketAddr = format!("127.0.0.1:{pg_port}")
        .parse()
        .expect("pg addr parse");
    let pgwire = drive_pgwire(pg_addr).await;

    // --- Assert equivalence across all five transports
    let all: &[(&str, &NormResult)] = &[
        ("embedded", &embedded),
        ("http", &http),
        ("grpc", &grpc),
        ("redwire", &redwire),
        ("pgwire", &pgwire),
    ];

    let (base_name, (base_cols, base_rows)) = all[0];
    for (name, (cols, rows)) in &all[1..] {
        assert_eq!(
            base_cols, cols,
            "column mismatch: {base_name}={base_cols:?} vs {name}={cols:?}",
        );
        assert_eq!(
            base_rows.len(),
            rows.len(),
            "row count mismatch: {base_name}={} vs {name}={}",
            base_rows.len(),
            rows.len(),
        );
        for (i, (br, or)) in base_rows.iter().zip(rows.iter()).enumerate() {
            assert_eq!(
                br, or,
                "row {i} mismatch: {base_name} vs {name}\n  {base_name}={br:?}\n  {name}={or:?}",
            );
        }
    }

    // Sanity floor: verify the expected shape came through on the baseline.
    assert_eq!(base_cols, &["id".to_string(), "label".to_string()]);
    assert_eq!(base_rows.len(), 3, "expected 3 rows from all transports");
    assert_eq!(
        base_rows[0],
        vec!["1".to_string(), "alpha".to_string()],
        "row 0 sanity"
    );
    assert_eq!(
        base_rows[1],
        vec!["2".to_string(), "beta".to_string()],
        "row 1 sanity"
    );
    assert_eq!(
        base_rows[2],
        vec!["3".to_string(), "gamma".to_string()],
        "row 2 sanity"
    );
}

#[tokio::test]
async fn cross_transport_document_crud_results_are_equivalent() {
    let runtime =
        Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"));

    // --- HTTP
    let http_listener = TcpListener::bind("127.0.0.1:0").expect("http bind");
    let http_addr = http_listener.local_addr().expect("http addr");
    RedDBServer::new(runtime.as_ref().clone()).serve_in_background_on(http_listener);

    // --- gRPC
    let grpc_port = pick_free_port();
    let grpc_opts = GrpcServerOptions {
        bind_addr: format!("127.0.0.1:{grpc_port}"),
        tls: None,
    };
    let grpc_auth = Arc::new(AuthStore::new(AuthConfig::default()));
    let grpc_server = RedDBGrpcServer::with_options(runtime.as_ref().clone(), grpc_opts, grpc_auth);
    tokio::spawn(async move {
        let _ = grpc_server.serve().await;
    });
    await_tcp(grpc_port).await;

    // --- RedWire
    let wire_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redwire bind");
    let wire_addr = wire_listener.local_addr().expect("redwire addr");
    tokio::spawn({
        let rt = Arc::clone(&runtime);
        async move {
            let _ = start_redwire_listener_on(wire_listener, rt).await;
        }
    });
    await_tcp(wire_addr.port()).await;

    // --- PG-wire
    let pg_port = pick_free_port();
    let pg_cfg = PgWireConfig {
        bind_addr: format!("127.0.0.1:{pg_port}"),
        ..PgWireConfig::default()
    };
    tokio::spawn({
        let rt = Arc::clone(&runtime);
        async move {
            let _ = start_pg_wire_listener(pg_cfg, rt).await;
        }
    });
    await_tcp(pg_port).await;
    let pg_addr: SocketAddr = format!("127.0.0.1:{pg_port}")
        .parse()
        .expect("pg addr parse");

    let embedded = run_embedded_document_crud(&runtime, "doc_crud_embedded");
    let http = run_http_document_crud(http_addr, "doc_crud_http");
    let grpc = run_grpc_document_crud(grpc_port, "doc_crud_grpc").await;
    let redwire = run_redwire_document_crud(wire_addr, "doc_crud_redwire").await;
    let pgwire = run_pgwire_document_crud(pg_addr, "doc_crud_pgwire").await;

    for (name, trace) in [
        ("embedded", &embedded),
        ("http", &http),
        ("grpc", &grpc),
        ("redwire", &redwire),
        ("pgwire", &pgwire),
    ] {
        assert_document_crud_trace_shape(trace);
        assert_eq!(
            &embedded, trace,
            "document CRUD trace diverged for {name}; embedded={embedded:?}, {name}={trace:?}",
        );
    }
}
