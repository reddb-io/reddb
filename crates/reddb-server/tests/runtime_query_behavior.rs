use std::sync::Arc;

use reddb_server::auth::{store::AuthStore, AuthConfig};
use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult};

fn int_at(result: &RuntimeQueryResult, row: usize, column: &str) -> i64 {
    match result.result.records[row].get(column) {
        Some(Value::Integer(value)) => *value,
        other => panic!("expected integer at row {row} column {column}, got {other:?}"),
    }
}

fn text_at<'a>(result: &'a RuntimeQueryResult, row: usize, column: &str) -> &'a str {
    match result.result.records[row].get(column) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected text at row {row} column {column}, got {other:?}"),
    }
}

#[test]
fn join_query_executes_against_real_table_rows() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
        .expect("create users");
    rt.execute_query("CREATE TABLE orders (id INT, user_id INT, total INT)")
        .expect("create orders");
    rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Ada'), (2, 'Linus'), (3, 'Grace')")
        .expect("insert users");
    rt.execute_query(
        "INSERT INTO orders (id, user_id, total) VALUES (10, 1, 50), (11, 2, 75), (12, 99, 999)",
    )
    .expect("insert orders");

    let result = rt
        .execute_query(
            "FROM users u JOIN orders o ON u.id = o.user_id \
             RETURN u.name, o.total ORDER BY o.total",
        )
        .expect("join executes");

    assert_eq!(result.engine, "runtime-join");
    assert_eq!(result.result.len(), 2);
    assert_eq!(text_at(&result, 0, "name"), "Ada");
    assert_eq!(int_at(&result, 0, "total"), 50);
    assert_eq!(text_at(&result, 1, "name"), "Linus");
    assert_eq!(int_at(&result, 1, "total"), 75);
}

#[test]
fn config_reference_compares_stored_value_without_reparsing_sql() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE TABLE tokens (id INT, token TEXT)")
        .expect("create tokens");
    rt.execute_query("INSERT INTO tokens (id, token) VALUES (1, 'normal_id'), (2, 'other_id')")
        .expect("insert tokens");

    rt.execute_query("SET CONFIG my.attack = '1=1 OR 1=1'")
        .expect("store injection-shaped config");
    let blocked = rt
        .execute_query("SELECT id FROM tokens WHERE token = $config.my.attack")
        .expect("config predicate executes");
    assert_eq!(
        blocked.result.len(),
        0,
        "stored config payload must be compared as text, not parsed as SQL"
    );

    rt.execute_query("SET CONFIG my.attack = 'normal_id'")
        .expect("store matching config");
    let matched = rt
        .execute_query("SELECT id FROM tokens WHERE token = $config.my.attack")
        .expect("config predicate executes");
    assert_eq!(matched.result.len(), 1);
    assert_eq!(int_at(&matched, 0, "id"), 1);
}

#[test]
fn secret_reference_compares_vault_value_without_reparsing_sql() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "reddb-runtime-secret-test-{}-{}.rdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&path);

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime boots");
    let pager = rt
        .db()
        .store()
        .pager()
        .expect("persistent runtime has pager")
        .clone();
    let auth_store = Arc::new(
        AuthStore::with_vault(AuthConfig::default(), pager, Some("runtime-secret-test"))
            .expect("vault opens"),
    );
    rt.set_auth_store(auth_store);

    rt.execute_query("CREATE TABLE tokens (id INT, token TEXT)")
        .expect("create tokens");
    rt.execute_query("INSERT INTO tokens (id, token) VALUES (1, 'normal_id'), (2, 'other_id')")
        .expect("insert tokens");

    rt.execute_query("SET SECRET my.attack = '1=1 OR 1=1'")
        .expect("store injection-shaped secret");
    let blocked = rt
        .execute_query("SELECT id FROM tokens WHERE token = $secret.my.attack")
        .expect("secret predicate executes");
    assert_eq!(
        blocked.result.len(),
        0,
        "stored secret payload must be compared as text, not parsed as SQL"
    );

    rt.execute_query("SET SECRET my.attack = 'normal_id'")
        .expect("store matching secret");
    let matched = rt
        .execute_query("SELECT id FROM tokens WHERE token = $secret.my.attack")
        .expect("secret predicate executes");
    assert_eq!(matched.result.len(), 1);
    assert_eq!(int_at(&matched, 0, "id"), 1);

    drop(rt);
    let _ = std::fs::remove_file(path);
}

// ── Issue #299 conformance: queue full → DLQ routing ─────────────────────────

#[test]
fn event_routes_to_outbox_dlq_when_target_queue_full() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE QUEUE user_events MAX_SIZE 1")
        .expect("create queue");
    rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO user_events")
        .expect("create table with events");

    // First insert fills the queue (max_size=1).
    rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
        .expect("first insert");

    // Second insert should overflow and route to DLQ.
    rt.execute_query("INSERT INTO users (id, name) VALUES (2, 'Bob')")
        .expect("second insert");

    // DLQ must exist and hold the overflow event.
    let dlq_result = rt
        .execute_query("QUEUE LEN user_events_outbox_dlq")
        .expect("DLQ is queryable");
    let dlq_len = match dlq_result.result.records[0].get("len") {
        Some(Value::UnsignedInteger(n)) => *n as usize,
        other => panic!("expected len, got {other:?}"),
    };
    assert!(
        dlq_len >= 1,
        "overflow event should be in user_events_outbox_dlq, got {dlq_len}"
    );
}

#[test]
fn target_queue_stays_at_max_size_on_overflow() {
    // Verifies the drain retry path: the original queue is not written past
    // its max_size — overflow goes to DLQ instead.
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE QUEUE orders_events MAX_SIZE 1")
        .expect("create queue");
    rt.execute_query("CREATE TABLE orders (id INT) WITH EVENTS TO orders_events")
        .expect("create table");

    rt.execute_query("INSERT INTO orders (id) VALUES (1)")
        .expect("first insert");
    rt.execute_query("INSERT INTO orders (id) VALUES (2)")
        .expect("second insert");
    rt.execute_query("INSERT INTO orders (id) VALUES (3)")
        .expect("third insert");

    // Original queue must not exceed max_size.
    let q_result = rt
        .execute_query("QUEUE LEN orders_events")
        .expect("queue is queryable");
    let q_len = match q_result.result.records[0].get("len") {
        Some(Value::UnsignedInteger(n)) => *n as usize,
        other => panic!("expected len, got {other:?}"),
    };
    assert_eq!(
        q_len, 1,
        "target queue must not exceed max_size; overflow routed to DLQ"
    );

    // DLQ must have the 2 overflow events.
    let dlq_result = rt
        .execute_query("QUEUE LEN orders_events_outbox_dlq")
        .expect("DLQ is queryable");
    let dlq_len = match dlq_result.result.records[0].get("len") {
        Some(Value::UnsignedInteger(n)) => *n as usize,
        other => panic!("expected len, got {other:?}"),
    };
    assert_eq!(dlq_len, 2, "two overflow events should be in DLQ");
}

#[test]
fn dlq_is_auto_created_on_first_overflow() {
    // Verifies DLQ auto-creation — no explicit CREATE QUEUE for the DLQ.
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE QUEUE logs_events MAX_SIZE 1")
        .expect("create queue");
    rt.execute_query("CREATE TABLE logs (msg TEXT) WITH EVENTS TO logs_events")
        .expect("create table");

    rt.execute_query("INSERT INTO logs (msg) VALUES ('first')")
        .expect("first insert");
    rt.execute_query("INSERT INTO logs (msg) VALUES ('second')")
        .expect("second insert → DLQ");

    // DLQ was never explicitly created but must exist now.
    let dlq_check = rt.execute_query("QUEUE LEN logs_events_outbox_dlq");
    assert!(
        dlq_check.is_ok(),
        "DLQ should be auto-created on first overflow: {:?}",
        dlq_check.err()
    );
    let dlq_len = match dlq_check.unwrap().result.records[0].get("len") {
        Some(Value::UnsignedInteger(n)) => *n as usize,
        other => panic!("expected len, got {other:?}"),
    };
    assert_eq!(dlq_len, 1, "one event in auto-created DLQ");
}

// ── Issue #414: SELECT row-projection must surface graph entities ────────────

#[test]
fn select_star_returns_graph_nodes_inserted_into_collection() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query(
        "INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')",
    )
    .expect("insert node");
    rt.execute_query(
        "INSERT INTO tales NODE (label, name) VALUES ('prince', 'Prince Charming')",
    )
    .expect("insert second node");

    let all = rt
        .execute_query("SELECT * FROM tales")
        .expect("SELECT * executes");
    assert_eq!(
        all.result.len(),
        2,
        "graph nodes must surface in SELECT * (got {} rows)",
        all.result.len()
    );

    let filtered = rt
        .execute_query("SELECT label, name FROM tales WHERE label = 'cinderella'")
        .expect("SELECT with WHERE executes");
    assert_eq!(filtered.result.len(), 1, "WHERE label='cinderella' matches one node");
    assert_eq!(text_at(&filtered, 0, "label"), "cinderella");
    assert_eq!(text_at(&filtered, 0, "name"), "Cinderella");
}

#[test]
fn aggregate_over_graph_collection_still_works() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('a', 'A')")
        .expect("insert a");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('b', 'B')")
        .expect("insert b");

    let agg = rt
        .execute_query("SELECT COUNT(*) AS n FROM tales")
        .expect("aggregate executes");
    let n = match agg.result.records[0].get("n") {
        Some(Value::UnsignedInteger(v)) => *v as usize,
        Some(Value::Integer(v)) => *v as usize,
        other => panic!("expected count value, got {other:?}"),
    };
    assert!(n >= 1, "aggregate must still see graph entities (got {n})");
}

// ── Issue #416: GRAPH algorithms must resolve labels to ids ─────────────────

#[test]
fn graph_traverse_resolves_label_to_node_id() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert");

    let by_label = rt
        .execute_query("GRAPH TRAVERSE 'cinderella'")
        .expect("traverse by label");
    assert!(
        !by_label.result.records.is_empty(),
        "GRAPH TRAVERSE must resolve a label to its node id"
    );
    let label0 = by_label
        .result
        .records
        .iter()
        .find_map(|r| match r.get("label") {
            Some(Value::Text(s)) => Some(s.as_ref().to_string()),
            _ => None,
        })
        .expect("label column present");
    assert_eq!(label0, "cinderella");
}

#[test]
fn graph_neighborhood_resolves_label_to_node_id() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert");

    let res = rt
        .execute_query("GRAPH NEIGHBORHOOD 'cinderella'")
        .expect("neighborhood by label");
    assert!(
        !res.result.records.is_empty(),
        "GRAPH NEIGHBORHOOD must resolve a label to its node id"
    );
}

#[test]
fn graph_traverse_ambiguous_label_errors() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('hero', 'A')")
        .expect("insert a");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('hero', 'B')")
        .expect("insert b");

    let err = rt
        .execute_query("GRAPH TRAVERSE 'hero'")
        .expect_err("ambiguous label must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("ambiguous"),
        "error should mention ambiguity, got: {msg}"
    );
}

#[test]
fn graph_traverse_unknown_reference_errors() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert");

    rt.execute_query("GRAPH TRAVERSE 'does_not_exist'")
        .expect_err("unknown reference must error");
}

#[test]
fn graph_shortest_path_resolves_labels_for_both_endpoints() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('alice', 'Alice')")
        .expect("insert a");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('bob', 'Bob')")
        .expect("insert b");

    let res = rt
        .execute_query("GRAPH SHORTEST_PATH 'alice' TO 'bob'")
        .expect("shortest path by labels");
    assert_eq!(
        res.result.records.len(),
        1,
        "SHORTEST_PATH always returns a single summary row"
    );
    let rec = &res.result.records[0];
    match rec.get("source") {
        Some(Value::Text(s)) => {
            assert_ne!(s.as_ref(), "alice", "source must be resolved to numeric id");
            assert!(s.as_ref().parse::<u64>().is_ok(), "source must be numeric");
        }
        other => panic!("expected text source, got {other:?}"),
    }
}

// ── Issue #415: MATCH WHERE / RETURN n.foo on single-node patterns ──────────

#[test]
fn match_where_filters_nodes_by_label_property() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert cinderella");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('prince', 'Prince')")
        .expect("insert prince");

    let res = rt
        .execute_query("MATCH (n) WHERE n.label = 'cinderella' RETURN n.name")
        .expect("MATCH executes");
    assert_eq!(
        res.result.len(),
        1,
        "WHERE n.label='cinderella' must keep exactly one node, got {}",
        res.result.len()
    );
    let name = match res.result.records[0].get("n.name") {
        Some(Value::Text(s)) => s.as_ref().to_string(),
        other => panic!("expected n.name text, got {other:?}"),
    };
    assert_eq!(name, "Cinderella");
}

#[test]
fn match_return_property_projects_actual_values() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('a', 'Alice')")
        .expect("insert a");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('b', 'Bob')")
        .expect("insert b");

    let res = rt
        .execute_query("MATCH (n) RETURN n.name")
        .expect("MATCH RETURN n.name executes");
    assert_eq!(res.result.len(), 2);
    let mut names: Vec<String> = res
        .result
        .records
        .iter()
        .map(|r| match r.get("n.name") {
            Some(Value::Text(s)) => s.as_ref().to_string(),
            other => panic!("expected n.name text, got {other:?}"),
        })
        .collect();
    names.sort();
    assert_eq!(names, vec!["Alice".to_string(), "Bob".to_string()]);
}

#[test]
fn match_return_whole_node_surfaces_property_bag() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert");

    let res = rt
        .execute_query("MATCH (n) WHERE n.label = 'cinderella' RETURN n")
        .expect("MATCH RETURN n executes");
    assert_eq!(res.result.len(), 1);
    // The whole-entity projection should populate at least the node's
    // user-supplied properties as record fields.
    let rec = &res.result.records[0];
    let name = rec
        .get("n.name")
        .and_then(|v| match v {
            Value::Text(s) => Some(s.as_ref().to_string()),
            _ => None,
        })
        .expect("RETURN n must surface property 'name'");
    assert_eq!(name, "Cinderella");
}
