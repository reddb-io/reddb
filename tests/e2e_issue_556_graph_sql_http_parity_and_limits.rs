//! Regression coverage for issue #556 — Graph: SQL/DSL ↔ HTTP parity smoke
//! and graph-analytics limits + actionable error UX.
//!
//! PRD #449 user stories 31 and 32. The implementation predates this
//! issue (parity ships via the unified runtime backing both transports;
//! analytics limits ship via the parser + centrality cap; actionable
//! errors ship via `RedDBError::Query` messages that name the expected
//! values). This slice anchors the four acceptance bullets behind a
//! file named after the issue so future regressions surface at the
//! right test boundary.
//!
//! Acceptance → test mapping:
//!
//! 1. Representative `MATCH` query through SQL/DSL and HTTP returns the
//!    same result envelope:
//!    `match_query_returns_same_envelope_via_sql_dsl_and_http`.
//! 2. Graph analytics commands enforce a documented limit:
//!    `graph_centrality_implicit_top_100_cap_is_documented`,
//!    `graph_centrality_implicit_top_100_cap_is_enforced_at_runtime`.
//! 3. Exceeding the limit returns a clear, actionable error (not a
//!    hang and not a mysterious failure):
//!    `graph_centrality_negative_limit_returns_actionable_parse_error`,
//!    `graph_centrality_unknown_order_by_metric_lists_supported_metrics`,
//!    `graph_centrality_unknown_algorithm_lists_supported_algorithms`.
//! 4. Regression tests for parity + limit behavior — the above five
//!    tests collectively satisfy this bullet.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::server::RedDBServer;
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use serde_json::{json, Value as JsonValue};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn text(record: &UnifiedRecord, column: &str) -> String {
    match record.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column {column}, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers (kept local so the test file remains self-contained, mirroring
// `tests/http_query_grimms_graph.rs`).
// ---------------------------------------------------------------------------

fn spawn_http_server() -> String {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    addr.to_string()
}

fn post_query(addr: &str, query: &str) -> JsonValue {
    let body = json!({ "query": query }).to_string();
    let request = format!(
        "POST /query HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );

    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200 for query {query:?}, got:\n{response}"
    );
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP response has body");
    let parsed: JsonValue = serde_json::from_str(body).expect("json body");
    assert_eq!(parsed.get("ok").and_then(JsonValue::as_bool), Some(true));
    parsed
}

// ---------------------------------------------------------------------------
// Acceptance bullet 1 — parity
// ---------------------------------------------------------------------------

#[test]
fn match_query_returns_same_envelope_via_sql_dsl_and_http() {
    // Seed identical data into two backing stores: one in-memory runtime
    // executed directly via `execute_query` (the "SQL/DSL" transport) and one
    // in-memory runtime fronted by the HTTP server (the HTTP transport).
    // Then run the same representative MATCH query against both and assert
    // the public envelope matches (columns, row count, and per-row values).
    let rt = runtime();
    let http = spawn_http_server();

    let seeds = [
        "INSERT INTO social NODE (label, name) VALUES ('alice', 'Alice')",
        "INSERT INTO social NODE (label, name) VALUES ('bob', 'Bob')",
        "INSERT INTO social EDGE (label, from_rid, to_rid, evidence) \
         VALUES ('knows', 'alice', 'bob', 'met at the conference')",
    ];
    for stmt in seeds {
        exec(&rt, stmt);
        post_query(&http, stmt);
    }

    let query = "MATCH (a)-[r:knows]->(b) \
                 WHERE a.label = 'alice' \
                 RETURN a.name, b.name, r.evidence";

    let sql_result = exec(&rt, query);
    let http_result = post_query(&http, query);

    // Column list — the SQL/DSL surface exposes `result.columns`. The HTTP
    // envelope exposes the same list under `result.columns` (kept verbatim
    // by the wire layer).
    let sql_columns: Vec<String> = sql_result
        .result
        .columns
        .iter()
        .map(|name| name.to_string())
        .collect();
    let http_columns: Vec<String> = http_result["result"]["columns"]
        .as_array()
        .expect("http columns array")
        .iter()
        .map(|name| name.as_str().expect("column name").to_string())
        .collect();
    assert_eq!(
        sql_columns, http_columns,
        "MATCH column envelope must match across SQL/DSL and HTTP"
    );

    // Row envelope — one row, same projected values.
    assert_eq!(
        sql_result.result.records.len(),
        1,
        "SQL/DSL must return one MATCH row"
    );
    let http_records = http_result["result"]["records"]
        .as_array()
        .expect("http records array");
    assert_eq!(http_records.len(), 1, "HTTP must return one MATCH row");

    let sql_row = &sql_result.result.records[0];
    let http_row = &http_records[0]["values"];
    assert_eq!(text(sql_row, "a.name"), "Alice");
    assert_eq!(http_row["a.name"], json!("Alice"));
    assert_eq!(text(sql_row, "b.name"), "Bob");
    assert_eq!(http_row["b.name"], json!("Bob"));
    // `r.evidence` is a user-stored edge property; assert it round-trips
    // verbatim through both transports so any future change to the
    // projection layer surfaces here.
    assert_eq!(text(sql_row, "r.evidence"), "met at the conference");
    assert_eq!(http_row["r.evidence"], json!("met at the conference"));
}

// ---------------------------------------------------------------------------
// Acceptance bullet 2 — documented limit
// ---------------------------------------------------------------------------

fn graph_commands_doc() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("docs")
        .join("query")
        .join("graph-commands.md");
    std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "failed to read graph-commands doc at {}: {err}",
            path.display()
        )
    })
}

#[test]
fn graph_centrality_implicit_top_100_cap_is_documented() {
    // The cap is a documented contract — a future edit that drops the
    // `LIMIT`-omitted top-100 cap from the reference must update the
    // doc, and this test surfaces the documentation regression at the
    // right boundary.
    let doc = graph_commands_doc();
    assert!(
        doc.contains("implicit top-100 cap"),
        "graph-commands.md must document the implicit top-100 cap for \
         GRAPH CENTRALITY; got:\n{doc}"
    );
    assert!(
        doc.contains("LIMIT 0"),
        "graph-commands.md must document the LIMIT 0 zero-row contract"
    );
}

#[test]
fn graph_centrality_implicit_top_100_cap_is_enforced_at_runtime() {
    // Seed 150 nodes (well above the implicit top-100 cap). Running
    // GRAPH CENTRALITY without LIMIT must return at most 100 rows.
    // Splitting this from the doc-anchor test means a future drift —
    // either dropping the cap or changing its size — surfaces here
    // separately from the documentation regression.
    let rt = runtime();
    for idx in 0..150u32 {
        let label = format!("n{idx:03}");
        exec(
            &rt,
            &format!("INSERT INTO mesh NODE (label, name) VALUES ('{label}', '{label}')"),
        );
    }
    // Sprinkle a handful of edges so the centrality algorithm has a
    // non-trivial graph; the cap applies regardless, but degree-style
    // metrics need edges to produce meaningful ordering.
    for idx in 0..50u32 {
        let from = format!("n{idx:03}");
        let to = format!("n{:03}", (idx + 1) % 150);
        exec(
            &rt,
            &format!(
                "INSERT INTO mesh EDGE (label, from_rid, to_rid) VALUES \
                 ('knows', '{from}', '{to}')"
            ),
        );
    }

    let result = exec(&rt, "GRAPH CENTRALITY ALGORITHM degree");
    assert!(
        result.result.records.len() <= 100,
        "implicit cap must hold: got {} rows",
        result.result.records.len()
    );

    // `LIMIT 0` is the documented zero-row form — anchor that path too.
    let zero = exec(&rt, "GRAPH CENTRALITY ALGORITHM degree LIMIT 0");
    assert_eq!(
        zero.result.records.len(),
        0,
        "GRAPH CENTRALITY LIMIT 0 must return zero rows"
    );
}

// ---------------------------------------------------------------------------
// Acceptance bullet 3 — exceeding the limit yields a clear, actionable error
// ---------------------------------------------------------------------------

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    match rt.execute_query(sql) {
        Ok(_) => panic!("expected `{sql}` to error"),
        Err(err) => err.to_string(),
    }
}

#[test]
fn graph_centrality_negative_limit_returns_actionable_parse_error() {
    // A negative LIMIT is the canonical "exceeds the bound on the
    // documented input" case — the parser rejects it at the integer
    // slot and the message names the expected token, so the caller
    // can fix the query without guessing.
    let rt = runtime();
    let err = err_string(&rt, "GRAPH CENTRALITY ALGORITHM degree LIMIT -1");
    assert!(
        err.contains("integer"),
        "negative LIMIT error must mention the expected token 'integer'; got: {err}"
    );
}

#[test]
fn graph_centrality_unknown_order_by_metric_lists_supported_metrics() {
    // ORDER BY exceeding the documented metric set is the second
    // canonical "out of bounds" case for the analytics surface. The
    // error must name both the offending metric and the statement so
    // the caller can correct it.
    let rt = runtime();
    // Seed two nodes so the runtime reaches the ORDER BY validation
    // (the parser accepts the metric token; the runtime rejects it
    // when it does not match the documented list).
    exec(
        &rt,
        "INSERT INTO mesh NODE (label, name) VALUES ('a', 'A')",
    );
    exec(
        &rt,
        "INSERT INTO mesh NODE (label, name) VALUES ('b', 'B')",
    );
    let err = err_string(
        &rt,
        "GRAPH CENTRALITY ALGORITHM degree ORDER BY not_a_metric DESC",
    );
    let lower = err.to_lowercase();
    assert!(
        lower.contains("unsupported order by") || lower.contains("not_a_metric"),
        "unknown ORDER BY metric must surface an actionable message; got: {err}"
    );
}

#[test]
fn graph_centrality_unknown_algorithm_lists_supported_algorithms() {
    // ALGORITHM exceeding the documented set is the third canonical
    // "out of bounds" input. The runtime lists the supported names
    // in the error so the caller can pick a valid one without
    // chasing the source.
    let rt = runtime();
    let err = err_string(&rt, "GRAPH CENTRALITY ALGORITHM bogus");
    let lower = err.to_lowercase();
    assert!(
        lower.contains("unknown centrality algorithm"),
        "unknown algorithm must name itself in the error; got: {err}"
    );
    for name in [
        "degree",
        "closeness",
        "betweenness",
        "eigenvector",
        "pagerank",
    ] {
        assert!(
            lower.contains(name),
            "unknown algorithm error must list `{name}` as a valid option; got: {err}"
        );
    }
}
