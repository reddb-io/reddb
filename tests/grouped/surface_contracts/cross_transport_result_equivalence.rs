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

// ---------------------------------------------------------------------------
// Transport drivers
// ---------------------------------------------------------------------------

/// Embedded: direct synchronous call into the shared runtime.
fn drive_embedded(runtime: &RedDBRuntime) -> NormResult {
    let qr = runtime
        .execute_query(SELECT_SQL)
        .expect("embedded execute_query");
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

/// HTTP: raw HTTP/1.1 POST to `/query`, parse JSON response envelope.
fn drive_http(addr: SocketAddr) -> NormResult {
    let body = json!({ "query": SELECT_SQL }).to_string();
    let envelope = http_post_json(addr, "/query", &body);
    let qr = QueryResult::from_envelope(envelope);
    norm_from_query_result(&qr)
}

/// gRPC: tonic client, synthesise an HTTP-style envelope from the reply.
async fn drive_grpc(port: u16) -> NormResult {
    await_tcp(port).await;
    let ep = Endpoint::from_shared(format!("http://127.0.0.1:{port}"))
        .expect("grpc endpoint")
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5));
    let ch = ep.connect().await.expect("grpc connect");
    let mut client = RedDbClient::new(ch);
    let reply = client
        .query(QueryRequest {
            query: SELECT_SQL.to_string(),
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
    let mut c = RedWireClient::connect(
        ConnectOptions::new(addr.ip().to_string(), addr.port()).with_auth(Auth::Anonymous),
    )
    .await
    .expect("redwire connect");
    let qr = c
        .query_with(SELECT_SQL_REDWIRE, &[ClientValue::Int(0)])
        .await
        .expect("redwire query_with");
    let _ = c.close().await;
    norm_from_query_result(&qr)
}

/// PG-wire: raw TCP with the PostgreSQL frontend/backend protocol.
/// Uses the simple-query flow (Q frame) and text-format row values.
async fn drive_pgwire(addr: SocketAddr) -> NormResult {
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
    let sql = format!("{SELECT_SQL}\0");
    let qmsg_len = (sql.len() + 4) as u32;
    s.write_all(&[b'Q']).await.expect("pg Q tag");
    s.write_all(&qmsg_len.to_be_bytes())
        .await
        .expect("pg Q len");
    s.write_all(sql.as_bytes()).await.expect("pg Q sql");

    // --- Parse response frames
    let mut columns: Vec<String> = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();

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
            b'Z' => break, // ReadyForQuery — response complete
            _ => {}        // CommandComplete, Notice, Error, etc.
        }
    }

    // Terminate cleanly
    let _ = s.write_all(&[b'X', 0, 0, 0, 4]).await;

    (columns, rows)
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
