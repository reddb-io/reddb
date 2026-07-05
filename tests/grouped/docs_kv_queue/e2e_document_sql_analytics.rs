#[path = "../../support/mod.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use serde_json::{json, Value as JsonValue};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn text<'a>(row: &'a reddb::storage::query::UnifiedRecord, field: &str) -> &'a str {
    match row.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text, got {other:?} in {row:?}"),
    }
}

fn int(row: &reddb::storage::query::UnifiedRecord, field: &str) -> i64 {
    match row.get(field) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected {field} integer, got {other:?} in {row:?}"),
    }
}

fn spawn_http_server() -> (support::TempDbFile, String) {
    let (db, rt) = support::persistent_runtime("document-sql-analytics-http");
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    (db, addr.to_string())
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

fn records(response: &JsonValue) -> &[JsonValue] {
    response["result"]["records"]
        .as_array()
        .expect("result.records array")
}

fn json_value<'a>(record: &'a JsonValue, column: &str) -> &'a JsonValue {
    &record["values"][column]
}

#[test]
fn runtime_queries_document_fields_json_paths_arrays_and_groups() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT events");
    exec(
        &rt,
        r#"INSERT INTO events DOCUMENT VALUES
        ({"level":"error","service":{"name":"checkout","tier":"edge"},"tags":["checkout","payment"],"latency_ms":42}),
        ({"level":"info","service":{"name":"checkout","tier":"edge"},"tags":["search"],"latency_ms":7}),
        ({"level":"error","service":{"name":"billing","tier":"core"},"tags":["payment"],"latency_ms":99})"#,
    );

    let projected = exec(
        &rt,
        "SELECT body.level AS severity, json_extract(body, '$.service.name') AS service \
         FROM events \
         WHERE body.service.tier = 'edge' AND body.tags CONTAINS 'checkout' \
         ORDER BY body.latency_ms",
    );
    assert_eq!(projected.result.records.len(), 1);
    assert_eq!(text(&projected.result.records[0], "severity"), "error");
    assert_eq!(
        text(&projected.result.records[0], "service"),
        "\"checkout\""
    );

    let grouped = exec(
        &rt,
        "SELECT body.level AS severity, COUNT(*) AS count \
         FROM events \
         WHERE body.missing = 'no-match' OR body.tags CONTAINS 'payment' \
         GROUP BY body.level \
         ORDER BY body.level",
    );
    assert_eq!(grouped.result.records.len(), 1);
    assert_eq!(text(&grouped.result.records[0], "severity"), "error");
    assert_eq!(int(&grouped.result.records[0], "count"), 2);
}

#[test]
fn runtime_indexes_nested_document_path_filters() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT events");
    exec(
        &rt,
        r#"INSERT INTO events DOCUMENT VALUES
        ({"level":"error","service":{"name":"checkout","tier":"edge"},"latency_ms":42}),
        ({"level":"info","service":{"name":"checkout","tier":"edge"},"latency_ms":7}),
        ({"level":"error","service":{"name":"billing","tier":"core"},"latency_ms":99})"#,
    );
    exec(
        &rt,
        "CREATE INDEX idx_events_service_tier ON events (body.service.tier)",
    );

    let indexes = exec(&rt, "SHOW INDEXES ON events");
    let index = indexes
        .result
        .records
        .iter()
        .find(|record| text(record, "name") == "idx_events_service_tier")
        .expect("idx_events_service_tier should be listed");
    assert_eq!(int(index, "entries_indexed"), 3);

    let explain = exec(
        &rt,
        "EXPLAIN SELECT body.level AS severity FROM events WHERE body.service.tier = 'edge'",
    );
    let plan_ops = explain
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("op") {
            Some(Value::Text(value)) => Some(value.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        plan_ops.contains("index_seek"),
        "nested document path filter should plan through an index; ops={plan_ops}"
    );

    let filtered = exec(
        &rt,
        "SELECT body.level AS severity FROM events \
         WHERE body.service.tier = 'edge' \
         ORDER BY body.latency_ms",
    );
    assert_eq!(filtered.result.records.len(), 2);
    assert_eq!(text(&filtered.result.records[0], "severity"), "info");
    assert_eq!(text(&filtered.result.records[1], "severity"), "error");
}

#[test]
fn http_query_document_fields_json_paths_arrays_and_groups() {
    let (_db, addr) = spawn_http_server();
    post_query(&addr, "CREATE DOCUMENT events");
    post_query(
        &addr,
        r#"INSERT INTO events DOCUMENT VALUES
        ({"level":"error","service":{"name":"checkout","tier":"edge"},"tags":["checkout","payment"],"latency_ms":42}),
        ({"level":"info","service":{"name":"checkout","tier":"edge"},"tags":["search"],"latency_ms":7}),
        ({"level":"error","service":{"name":"billing","tier":"core"},"tags":["payment"],"latency_ms":99})"#,
    );

    let projected = post_query(
        &addr,
        "SELECT body.level AS severity, json_extract(body, '$.service.name') AS service \
         FROM events \
         WHERE body.service.tier = 'edge' AND body.tags CONTAINS 'checkout' \
         ORDER BY body.latency_ms",
    );
    let projected_rows = records(&projected);
    assert_eq!(projected_rows.len(), 1, "projected={projected}");
    assert_eq!(json_value(&projected_rows[0], "severity"), &json!("error"));
    assert_eq!(
        json_value(&projected_rows[0], "service"),
        &json!("\"checkout\"")
    );

    let grouped = post_query(
        &addr,
        "SELECT body.level AS severity, COUNT(*) AS count \
         FROM events \
         WHERE body.missing = 'no-match' OR body.tags CONTAINS 'payment' \
         GROUP BY body.level \
         ORDER BY body.level",
    );
    let grouped_rows = records(&grouped);
    assert_eq!(grouped_rows.len(), 1, "grouped={grouped}");
    assert_eq!(json_value(&grouped_rows[0], "severity"), &json!("error"));
    assert_eq!(json_value(&grouped_rows[0], "count"), &json!(2));
}

// --- Ordered B-tree index tests (#1400) ---

/// CREATE INDEX on a document body path; verify range predicates (>, <, BETWEEN),
/// ORDER BY, and top-N (LIMIT) all resolve through the index.
#[test]
fn runtime_btree_document_path_range_order_and_limit() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT range_docs");
    // Tier values in lexicographic order: bronze < copper < gold < platinum < silver
    exec(
        &rt,
        r#"INSERT INTO range_docs DOCUMENT VALUES
        ({"name":"aaa","tier":"bronze"}),
        ({"name":"bbb","tier":"copper"}),
        ({"name":"ccc","tier":"gold"}),
        ({"name":"ddd","tier":"platinum"}),
        ({"name":"eee","tier":"silver"})"#,
    );
    exec(&rt, "CREATE INDEX idx_range_tier ON range_docs (body.tier)");

    // Equality — uses the companion hash index auto-created by BTree.
    let eq = exec(&rt, "SELECT name FROM range_docs WHERE body.tier = 'gold'");
    assert_eq!(eq.result.records.len(), 1, "equality hit: {eq:?}");
    assert_eq!(text(&eq.result.records[0], "name"), "ccc");

    // Range >: body.tier > 'copper' → gold(ccc), platinum(ddd), silver(eee)
    let gt = exec(
        &rt,
        "SELECT name FROM range_docs WHERE body.tier > 'copper' ORDER BY body.tier",
    );
    assert_eq!(gt.result.records.len(), 3, "gt={gt:?}");
    assert_eq!(text(&gt.result.records[0], "name"), "ccc"); // gold
    assert_eq!(text(&gt.result.records[1], "name"), "ddd"); // platinum
    assert_eq!(text(&gt.result.records[2], "name"), "eee"); // silver

    // Range <: body.tier < 'gold' → bronze(aaa), copper(bbb)
    let lt = exec(
        &rt,
        "SELECT name FROM range_docs WHERE body.tier < 'gold' ORDER BY body.tier",
    );
    assert_eq!(lt.result.records.len(), 2, "lt={lt:?}");
    assert_eq!(text(&lt.result.records[0], "name"), "aaa"); // bronze
    assert_eq!(text(&lt.result.records[1], "name"), "bbb"); // copper

    // Range BETWEEN: body.tier BETWEEN 'copper' AND 'platinum' → copper, gold, platinum
    let between = exec(
        &rt,
        "SELECT name FROM range_docs WHERE body.tier BETWEEN 'copper' AND 'platinum' ORDER BY body.tier",
    );
    assert_eq!(between.result.records.len(), 3, "between={between:?}");
    assert_eq!(text(&between.result.records[0], "name"), "bbb"); // copper
    assert_eq!(text(&between.result.records[1], "name"), "ccc"); // gold
    assert_eq!(text(&between.result.records[2], "name"), "ddd"); // platinum

    // ORDER BY via index: all docs sorted ascending by tier
    let sorted = exec(&rt, "SELECT name FROM range_docs ORDER BY body.tier");
    assert_eq!(sorted.result.records.len(), 5, "sorted={sorted:?}");
    assert_eq!(text(&sorted.result.records[0], "name"), "aaa"); // bronze
    assert_eq!(text(&sorted.result.records[4], "name"), "eee"); // silver

    // Top-N: LIMIT 2 returns the two lowest-tier docs in sorted order
    let top2 = exec(
        &rt,
        "SELECT name FROM range_docs ORDER BY body.tier LIMIT 2",
    );
    assert_eq!(top2.result.records.len(), 2, "top2={top2:?}");
    assert_eq!(text(&top2.result.records[0], "name"), "aaa"); // bronze
    assert_eq!(text(&top2.result.records[1], "name"), "bbb"); // copper
}

/// Verify that INSERT / UPDATE / DELETE correctly refresh the BTree index on a
/// document body path, so range queries return up-to-date results.
/// This specifically exercises the bug-fix for HOT-path and damage-vector checks
/// that previously skipped refresh for dot-path indexed columns.
#[test]
fn runtime_btree_document_path_index_refresh_on_dml() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT refresh_docs");
    // Initial documents:  svc1=gold (in range), svc2=bronze (boundary), svc3=silver (in range)
    exec(
        &rt,
        r#"INSERT INTO refresh_docs DOCUMENT VALUES
        ({"name":"svc1","tier":"gold"}),
        ({"name":"svc2","tier":"bronze"}),
        ({"name":"svc3","tier":"silver"})"#,
    );
    exec(
        &rt,
        "CREATE INDEX idx_refresh_docs_tier ON refresh_docs (body.tier)",
    );

    let above_bronze = |rt: &RedDBRuntime| {
        exec(
            rt,
            "SELECT name FROM refresh_docs WHERE body.tier > 'bronze' ORDER BY body.tier",
        )
        .result
        .records
        .into_iter()
        .map(|r| match r.get("name") {
            Some(Value::Text(s)) => s.to_string(),
            other => panic!("expected name text, got {other:?}"),
        })
        .collect::<Vec<_>>()
    };

    // Baseline: > 'bronze' → gold(svc1), silver(svc3)
    let initial = above_bronze(&rt);
    assert_eq!(initial, vec!["svc1", "svc3"], "initial={initial:?}");

    // INSERT: add svc4 with tier=platinum (platinum > bronze)
    exec(
        &rt,
        r#"INSERT INTO refresh_docs DOCUMENT VALUES ({"name":"svc4","tier":"platinum"})"#,
    );
    let after_insert = above_bronze(&rt);
    assert_eq!(
        after_insert,
        vec!["svc1", "svc4", "svc3"],
        "after INSERT of platinum svc4: {after_insert:?}"
    );

    // UPDATE: move svc1 from gold → aaa (aaa < bronze, drops out of range)
    exec(
        &rt,
        "UPDATE refresh_docs SET tier = 'aaa' WHERE name = 'svc1'",
    );
    let after_update = above_bronze(&rt);
    assert_eq!(
        after_update,
        vec!["svc4", "svc3"],
        "after UPDATE svc1 tier→aaa: {after_update:?}"
    );

    // DELETE: remove svc3
    exec(&rt, "DELETE FROM refresh_docs WHERE name = 'svc3'");
    let after_delete = above_bronze(&rt);
    assert_eq!(
        after_delete,
        vec!["svc4"],
        "after DELETE svc3: {after_delete:?}"
    );
}
