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

fn insert_graph_node(rt: &RedDBRuntime, label: &str, name: &str) -> u64 {
    let res = rt
        .execute_query(&format!(
            "INSERT INTO tales NODE (label, name) VALUES ('{label}', '{name}') RETURNING *"
        ))
        .expect("insert graph node");
    match res.result.records[0].get("red_entity_id") {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => *value as u64,
        other => panic!("expected graph node id, got {other:?}"),
    }
}

fn sorted_text_column(result: &RuntimeQueryResult, column: &str) -> Vec<String> {
    let mut values: Vec<String> = result
        .result
        .records
        .iter()
        .map(|record| match record.get(column) {
            Some(Value::Text(value)) => value.as_ref().to_string(),
            other => panic!("expected text column {column}, got {other:?}"),
        })
        .collect();
    values.sort();
    values
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
fn select_star_returns_graph_entities_inserted_into_collection() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert node");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('prince', 'Prince Charming')")
        .expect("insert second node");
    rt.execute_query(
        "INSERT INTO tales EDGE (label, from, to) VALUES ('rescues', 'prince', 'cinderella')",
    )
    .expect("insert edge");

    let all = rt
        .execute_query("SELECT * FROM tales")
        .expect("SELECT * executes");
    assert_eq!(
        all.result.len(),
        3,
        "graph nodes and edges must surface in SELECT * (got {} rows)",
        all.result.len()
    );
    let mut entity_types: Vec<String> = all
        .result
        .records
        .iter()
        .map(|record| match record.get("red_entity_type") {
            Some(Value::Text(value)) => value.as_ref().to_string(),
            other => panic!("expected red_entity_type text, got {other:?}"),
        })
        .collect();
    entity_types.sort();
    assert_eq!(
        entity_types,
        vec![
            "graph_edge".to_string(),
            "graph_node".to_string(),
            "graph_node".to_string(),
        ]
    );

    let edge = all
        .result
        .records
        .iter()
        .find(|record| matches!(record.get("red_entity_type"), Some(Value::Text(t)) if t.as_ref() == "graph_edge"))
        .expect("edge row is present");
    match edge.get("label") {
        Some(Value::Text(value)) => assert_eq!(value.as_ref(), "rescues"),
        other => panic!("expected edge label text, got {other:?}"),
    }
    assert!(matches!(edge.get("from"), Some(Value::NodeRef(_))));
    assert!(matches!(edge.get("to"), Some(Value::NodeRef(_))));

    let filtered = rt
        .execute_query("SELECT label, name FROM tales WHERE label = 'cinderella'")
        .expect("SELECT with WHERE executes");
    assert_eq!(
        filtered.result.len(),
        1,
        "WHERE label='cinderella' matches one node"
    );
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
    let node_id = text_at(&by_label, 0, "node_id").to_string();

    let by_id = rt
        .execute_query(&format!("GRAPH TRAVERSE '{node_id}'"))
        .expect("traverse by numeric id");
    assert_eq!(text_at(&by_id, 0, "node_id"), node_id);
    assert_eq!(text_at(&by_id, 0, "label"), "cinderella");
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
    let node_id = text_at(&res, 0, "node_id").to_string();

    let by_id = rt
        .execute_query(&format!("GRAPH NEIGHBORHOOD '{node_id}'"))
        .expect("neighborhood by numeric id");
    assert_eq!(text_at(&by_id, 0, "node_id"), node_id);
    assert_eq!(text_at(&by_id, 0, "label"), "cinderella");
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
    let source_id = text_at(&res, 0, "source").to_string();
    let target_id = text_at(&res, 0, "target").to_string();

    let by_id = rt
        .execute_query(&format!(
            "GRAPH SHORTEST_PATH '{source_id}' TO '{target_id}'"
        ))
        .expect("shortest path by numeric ids");
    assert_eq!(text_at(&by_id, 0, "source"), source_id);
    assert_eq!(text_at(&by_id, 0, "target"), target_id);
}

// ── Issue #420: EDGE insert accepts node labels in from/to ─────────────────

fn first_text(rec_field: Option<&Value>) -> String {
    match rec_field {
        Some(Value::Text(s)) => s.as_ref().to_string(),
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn edge_insert_resolves_labels_in_from_to() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('alice', 'Alice')")
        .expect("insert alice");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('bob', 'Bob')")
        .expect("insert bob");

    rt.execute_query("INSERT INTO tales EDGE (label, from, to) VALUES ('KNOWS', 'alice', 'bob')")
        .expect("edge by label must succeed");

    // Verify edge is reachable via TRAVERSE
    let res = rt
        .execute_query("GRAPH TRAVERSE 'alice'")
        .expect("traverse from alice");
    let labels: Vec<String> = res
        .result
        .records
        .iter()
        .map(|r| first_text(r.get("label")))
        .collect();
    assert!(
        labels.iter().any(|l| l == "bob"),
        "edge alice→bob should make 'bob' reachable; got {labels:?}"
    );
}

#[test]
fn edge_insert_still_accepts_numeric_ids() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('alice', 'Alice')")
        .expect("alice");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('bob', 'Bob')")
        .expect("bob");
    // First user entity id is 102 (#421). Use TRAVERSE to discover ids.
    let res = rt
        .execute_query("GRAPH TRAVERSE 'alice'")
        .expect("traverse");
    let aid: u64 = match res.result.records[0].get("node_id") {
        Some(Value::Text(s)) => s.as_ref().parse().expect("numeric id"),
        other => panic!("unexpected node_id: {other:?}"),
    };
    let res = rt.execute_query("GRAPH TRAVERSE 'bob'").expect("traverse");
    let bid: u64 = match res.result.records[0].get("node_id") {
        Some(Value::Text(s)) => s.as_ref().parse().expect("numeric id"),
        other => panic!("unexpected node_id: {other:?}"),
    };
    rt.execute_query(&format!(
        "INSERT INTO tales EDGE (label, from, to) VALUES ('KNOWS', {aid}, {bid})"
    ))
    .expect("numeric edge");
}

#[test]
fn edge_insert_mixed_label_and_id() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('alice', 'Alice')")
        .expect("alice");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('bob', 'Bob')")
        .expect("bob");
    let res = rt.execute_query("GRAPH TRAVERSE 'bob'").expect("traverse");
    let bid: u64 = match res.result.records[0].get("node_id") {
        Some(Value::Text(s)) => s.as_ref().parse().expect("numeric"),
        other => panic!("{other:?}"),
    };
    // label for `from`, numeric for `to`
    rt.execute_query(&format!(
        "INSERT INTO tales EDGE (label, from, to) VALUES ('KNOWS', 'alice', {bid})"
    ))
    .expect("mixed label+id");
}

#[test]
fn edge_insert_ambiguous_label_errors() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('hero', 'A')")
        .expect("hero a");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('hero', 'B')")
        .expect("hero b");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('villain', 'V')")
        .expect("villain");

    let err = rt
        .execute_query(
            "INSERT INTO tales EDGE (label, from, to) VALUES ('FIGHTS', 'hero', 'villain')",
        )
        .expect_err("ambiguous label must error");
    assert!(
        format!("{err}").contains("ambiguous"),
        "error should mention ambiguity, got: {err}"
    );
}

#[test]
fn edge_insert_unknown_label_errors() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('alice', 'Alice')")
        .expect("alice");

    let err = rt
        .execute_query("INSERT INTO tales EDGE (label, from, to) VALUES ('KNOWS', 'alice', 'nope')")
        .expect_err("unknown label must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("no graph node") && msg.contains("nope"),
        "error should name missing label, got: {msg}"
    );
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

#[test]
fn match_edge_expansion_honors_label_and_direction() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let alice = insert_graph_node(&rt, "alice", "Alice");
    let bob = insert_graph_node(&rt, "bob", "Bob");
    let clara = insert_graph_node(&rt, "clara", "Clara");
    let dave = insert_graph_node(&rt, "dave", "Dave");

    rt.execute_query(&format!(
        "INSERT INTO tales EDGE (label, from, to) VALUES \
         ('likes', {alice}, {bob}), ('likes', {clara}, {alice}), ('hates', {alice}, {dave})"
    ))
    .expect("insert graph edges");

    let outgoing = rt
        .execute_query("MATCH (a)-[:likes]->(b) WHERE a.name = 'Alice' RETURN b.name")
        .expect("outgoing MATCH executes");
    assert_eq!(sorted_text_column(&outgoing, "b.name"), vec!["Bob"]);

    let incoming = rt
        .execute_query("MATCH (a)<-[:likes]-(b) WHERE a.name = 'Alice' RETURN b.name")
        .expect("incoming MATCH executes");
    assert_eq!(sorted_text_column(&incoming, "b.name"), vec!["Clara"]);

    let undirected = rt
        .execute_query("MATCH (a)-[:likes]-(b) WHERE a.name = 'Alice' RETURN b.name")
        .expect("undirected MATCH executes");
    assert_eq!(
        sorted_text_column(&undirected, "b.name"),
        vec!["Bob", "Clara"]
    );
}

#[test]
fn match_unlabeled_edge_returns_all_direct_pairs() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let alice = insert_graph_node(&rt, "alice", "Alice");
    let bob = insert_graph_node(&rt, "bob", "Bob");
    let clara = insert_graph_node(&rt, "clara", "Clara");

    rt.execute_query(&format!(
        "INSERT INTO tales EDGE (label, from, to) VALUES \
         ('likes', {alice}, {bob}), ('hates', {alice}, {clara})"
    ))
    .expect("insert graph edges");

    let res = rt
        .execute_query("MATCH (a)-[]->(b) WHERE a.name = 'Alice' RETURN b.name")
        .expect("unlabeled MATCH executes");
    assert_eq!(sorted_text_column(&res, "b.name"), vec!["Bob", "Clara"]);
}

#[test]
fn match_return_edge_alias_projects_edge_properties() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let alice = insert_graph_node(&rt, "alice", "Alice");
    let bob = insert_graph_node(&rt, "bob", "Bob");

    rt.execute_query(&format!(
        "INSERT INTO tales EDGE (label, from, to) VALUES ('likes', {alice}, {bob})"
    ))
    .expect("insert graph edge");

    let props = rt
        .execute_query("MATCH (a)-[r:likes]->(b) RETURN r.label, r.source, r.target")
        .expect("edge property projection executes");
    assert_eq!(props.result.len(), 1);
    assert_eq!(text_at(&props, 0, "r.label"), "likes");
    assert_eq!(text_at(&props, 0, "r.source"), alice.to_string());
    assert_eq!(text_at(&props, 0, "r.target"), bob.to_string());

    let whole = rt
        .execute_query("MATCH (a)-[r:likes]->(b) RETURN r")
        .expect("whole edge projection executes");
    assert_eq!(whole.result.len(), 1);
    assert_eq!(text_at(&whole, 0, "r.label"), "likes");
    assert_eq!(text_at(&whole, 0, "r.from"), alice.to_string());
    assert_eq!(text_at(&whole, 0, "r.to"), bob.to_string());
}

#[test]
fn match_limit_caps_projected_rows_after_filtering() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    insert_graph_node(&rt, "hero", "Alice");
    insert_graph_node(&rt, "hero", "Bob");
    insert_graph_node(&rt, "villain", "Clara");

    let res = rt
        .execute_query("MATCH (n) WHERE n.label = 'hero' RETURN n.name LIMIT 1")
        .expect("MATCH LIMIT executes");
    assert_eq!(res.result.len(), 1, "LIMIT 1 must cap filtered MATCH rows");
    assert!(
        matches!(res.result.records[0].get("n.name"), Some(Value::Text(_))),
        "LIMIT applies after projection, so projected n.name must exist"
    );
}

#[test]
fn match_limit_zero_returns_no_rows() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    insert_graph_node(&rt, "hero", "Alice");
    insert_graph_node(&rt, "hero", "Bob");

    let res = rt
        .execute_query("MATCH (n) RETURN n.name LIMIT 0")
        .expect("MATCH LIMIT 0 executes");
    assert_eq!(res.result.len(), 0, "LIMIT 0 returns zero MATCH rows");
}

#[test]
fn match_limit_caps_edge_expansion_rows() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let alice = insert_graph_node(&rt, "alice", "Alice");
    let bob = insert_graph_node(&rt, "bob", "Bob");
    let clara = insert_graph_node(&rt, "clara", "Clara");

    rt.execute_query(&format!(
        "INSERT INTO tales EDGE (label, from, to) VALUES \
         ('likes', {alice}, {bob}), ('likes', {alice}, {clara})"
    ))
    .expect("insert graph edges");

    let res = rt
        .execute_query(
            "MATCH (a)-[:likes]->(b) WHERE a.name = 'Alice' RETURN a.name, b.name LIMIT 1",
        )
        .expect("MATCH edge LIMIT executes");
    assert_eq!(res.result.len(), 1, "LIMIT 1 must cap edge MATCH rows");
}

// ── Issue #419: INSERT surfaces the engine-assigned entity id ───────────────

fn u64_at(result: &RuntimeQueryResult, row: usize, column: &str) -> u64 {
    match result.result.records[row].get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => *value as u64,
        other => panic!("expected unsigned int at row {row} column {column}, got {other:?}"),
    }
}

#[test]
fn insert_node_returning_star_exposes_entity_id() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let res = rt
        .execute_query(
            "INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella') RETURNING *",
        )
        .expect("INSERT NODE RETURNING * executes");
    assert_eq!(res.affected_rows, 1, "one node inserted");
    assert_eq!(res.result.len(), 1, "one RETURNING row");
    let id = u64_at(&res, 0, "red_entity_id");
    assert!(id > 0, "engine-assigned id must be present (got {id})");
    assert_eq!(text_at(&res, 0, "label"), "cinderella");
    assert_eq!(text_at(&res, 0, "name"), "Cinderella");
}

#[test]
fn insert_returning_star_exposes_entity_id_for_non_graph_entities() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let cases = [
        "INSERT INTO users (name) VALUES ('Alice') RETURNING *",
        "INSERT INTO docs DOCUMENT (body) VALUES ('{\"title\":\"one\"}') RETURNING *",
        "INSERT INTO settings KV (key, value) VALUES ('max_retries', 5) RETURNING *",
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'axis') RETURNING *",
    ];

    for sql in cases {
        let res = rt.execute_query(sql).expect("INSERT RETURNING * executes");
        assert_eq!(res.affected_rows, 1, "{sql}");
        assert_eq!(res.result.len(), 1, "{sql}");
        let id = u64_at(&res, 0, "red_entity_id");
        assert!(id > 0, "{sql} must expose red_entity_id");
    }
}

#[test]
fn insert_edge_returning_star_exposes_entity_id() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let a = rt
        .execute_query("INSERT INTO tales NODE (label, name) VALUES ('a', 'A') RETURNING *")
        .expect("insert a");
    let b = rt
        .execute_query("INSERT INTO tales NODE (label, name) VALUES ('b', 'B') RETURNING *")
        .expect("insert b");
    let a_id = u64_at(&a, 0, "red_entity_id");
    let b_id = u64_at(&b, 0, "red_entity_id");

    let res = rt
        .execute_query(&format!(
            "INSERT INTO tales EDGE (label, from, to) VALUES ('KNOWS', {a_id}, {b_id}) RETURNING *"
        ))
        .expect("INSERT EDGE RETURNING * executes");
    assert_eq!(res.affected_rows, 1);
    assert_eq!(res.result.len(), 1);
    let id = u64_at(&res, 0, "red_entity_id");
    assert!(id > 0);
    assert_eq!(text_at(&res, 0, "label"), "KNOWS");
}

#[test]
fn insert_multi_row_node_returning_star_emits_one_row_per_insert() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let res = rt
        .execute_query(
            "INSERT INTO tales NODE (label, name) VALUES ('a', 'A'), ('b', 'B') RETURNING *",
        )
        .expect("multi-row NODE insert executes");
    assert_eq!(res.affected_rows, 2);
    assert_eq!(res.result.len(), 2);
    let id_a = u64_at(&res, 0, "red_entity_id");
    let id_b = u64_at(&res, 1, "red_entity_id");
    assert!(id_a > 0 && id_b > 0 && id_a != id_b);
}

#[test]
fn insert_multi_row_edge_returning_star_emits_one_row_per_insert() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let a = rt
        .execute_query("INSERT INTO tales NODE (label, name) VALUES ('a', 'A') RETURNING *")
        .expect("insert a");
    let b = rt
        .execute_query("INSERT INTO tales NODE (label, name) VALUES ('b', 'B') RETURNING *")
        .expect("insert b");
    let c = rt
        .execute_query("INSERT INTO tales NODE (label, name) VALUES ('c', 'C') RETURNING *")
        .expect("insert c");
    let a_id = u64_at(&a, 0, "red_entity_id");
    let b_id = u64_at(&b, 0, "red_entity_id");
    let c_id = u64_at(&c, 0, "red_entity_id");

    let res = rt
        .execute_query(&format!(
            "INSERT INTO tales EDGE (label, from, to) VALUES \
             ('KNOWS', {a_id}, {b_id}), ('KNOWS', {b_id}, {c_id}) RETURNING *"
        ))
        .expect("multi-row EDGE insert executes");
    assert_eq!(res.affected_rows, 2);
    assert_eq!(res.result.len(), 2);
    let id_a = u64_at(&res, 0, "red_entity_id");
    let id_b = u64_at(&res, 1, "red_entity_id");
    assert!(id_a > 0 && id_b > 0 && id_a != id_b);
    assert_eq!(text_at(&res, 0, "label"), "KNOWS");
    assert_eq!(text_at(&res, 1, "label"), "KNOWS");
}

#[test]
fn insert_multi_row_node_failure_is_atomic() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let err = rt
        .execute_query(
            "INSERT INTO tales NODE (label, name, _ttl_ms) VALUES \
             ('a', 'A', 60000), ('b', 'B', -1) RETURNING *",
        )
        .expect_err("invalid second NODE row must fail");
    assert!(
        err.to_string().contains("_ttl_ms"),
        "expected TTL metadata validation error, got {err}"
    );

    let all = rt
        .execute_query("SELECT * FROM tales")
        .expect("SELECT after failed insert executes");
    assert_eq!(all.result.len(), 0, "failed batch must leave no graph rows");
}

#[test]
fn insert_multi_row_edge_failure_is_atomic() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let a = rt
        .execute_query("INSERT INTO tales NODE (label, name) VALUES ('a', 'A') RETURNING *")
        .expect("insert a");
    let b = rt
        .execute_query("INSERT INTO tales NODE (label, name) VALUES ('b', 'B') RETURNING *")
        .expect("insert b");
    let a_id = u64_at(&a, 0, "red_entity_id");
    let b_id = u64_at(&b, 0, "red_entity_id");

    let err = rt
        .execute_query(&format!(
            "INSERT INTO tales EDGE (label, from, to) VALUES \
             ('KNOWS', {a_id}, {b_id}), ('TREE_CHILD', {b_id}, {a_id}) RETURNING *"
        ))
        .expect_err("invalid second EDGE row must fail");
    assert!(
        err.to_string().contains("TREE_CHILD"),
        "expected reserved edge label error, got {err}"
    );

    let all = rt
        .execute_query("SELECT * FROM tales")
        .expect("SELECT after failed insert executes");
    let edge_count = all
        .result
        .records
        .iter()
        .filter(|record| {
            matches!(
                record.get("red_entity_type"),
                Some(Value::Text(value)) if value.as_ref() == "graph_edge"
            )
        })
        .count();
    assert_eq!(edge_count, 0, "failed batch must leave no graph edges");
}

// ── Issue #421: first user-inserted entity id is documented & pinned ────────
//
// The first ~100 entity ids are consumed by internal collection-descriptor
// records before any user INSERT runs. The exact offset is part of the
// documented contract in `docs/data-models/graphs.md` and
// `docs/engine/file-format.md` (look for "first user id"). If you tripped
// this test by changing the descriptor allocation, update the docs in the
// same commit — the number IS the contract for users computing ids
// off-thread.

#[test]
fn first_user_entity_id_is_one_hundred_and_two() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let res = rt
        .execute_query(
            "INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella') RETURNING *",
        )
        .expect("first user insert");
    let id = u64_at(&res, 0, "red_entity_id");
    assert_eq!(
        id, 102,
        "first user-inserted entity id must be 102 (documented offset). \
         If you changed this, update docs/data-models/graphs.md AND \
         docs/engine/file-format.md."
    );
}

#[test]
fn first_file_backed_user_entity_id_is_one_hundred_and_two() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "reddb-first-user-entity-id-{}-{}.rdb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&path);

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime boots");
    let res = rt
        .execute_query(
            "INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella') RETURNING *",
        )
        .expect("first persistent user insert");
    let id = u64_at(&res, 0, "red_entity_id");
    drop(rt);
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        id, 102,
        "first file-backed user-inserted entity id must match the documented 102 offset"
    );
}

// ── Issue #423: GRAPH PROPERTIES '<id-or-label>' per-node lookup ────────────

#[test]
fn graph_properties_no_arg_returns_graph_wide_stats() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert");
    let res = rt
        .execute_query("GRAPH PROPERTIES")
        .expect("no-arg form still works");
    assert_eq!(res.result.records.len(), 1);
    assert!(res.result.records[0].get("node_count").is_some());
}

#[test]
fn graph_properties_by_label_returns_property_bag() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert");
    let res = rt
        .execute_query("GRAPH PROPERTIES 'cinderella'")
        .expect("by label resolves");
    assert_eq!(res.result.records.len(), 1);
    let rec = &res.result.records[0];
    match rec.get("label") {
        Some(Value::Text(s)) => assert_eq!(s.as_ref(), "cinderella"),
        other => panic!("expected label text, got {other:?}"),
    }
    match rec.get("name") {
        Some(Value::Text(s)) => assert_eq!(s.as_ref(), "Cinderella"),
        other => panic!("expected property 'name' surfaced as column, got {other:?}"),
    }
}

#[test]
fn graph_properties_by_numeric_id_returns_property_bag() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    let ins = rt
        .execute_query(
            "INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella') RETURNING *",
        )
        .expect("insert");
    let id = u64_at(&ins, 0, "red_entity_id");
    let res = rt
        .execute_query(&format!("GRAPH PROPERTIES '{id}'"))
        .expect("by numeric id resolves");
    assert_eq!(res.result.records.len(), 1);
    let rec = &res.result.records[0];
    match rec.get("node_id") {
        Some(Value::Text(s)) => assert_eq!(s.as_ref(), &id.to_string()),
        other => panic!("expected node_id={id}, got {other:?}"),
    }
}

#[test]
fn graph_properties_missing_label_errors() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('cinderella', 'Cinderella')")
        .expect("insert");
    let err = rt
        .execute_query("GRAPH PROPERTIES 'does_not_exist'")
        .expect_err("missing must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("does_not_exist") || msg.to_lowercase().contains("not found"),
        "error must surface missing reference, got: {msg}"
    );
}

#[test]
fn graph_properties_ambiguous_label_errors() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('hero', 'A')")
        .expect("hero a");
    rt.execute_query("INSERT INTO tales NODE (label, name) VALUES ('hero', 'B')")
        .expect("hero b");
    let err = rt
        .execute_query("GRAPH PROPERTIES 'hero'")
        .expect_err("ambiguous must error");
    assert!(
        format!("{err}").contains("ambiguous"),
        "error must mention ambiguity, got: {err}"
    );
}

// ── Issue #422 tracer: GRAPH CENTRALITY LIMIT N ─────────────────────────────

fn seed_centrality_graph(rt: &RedDBRuntime, n: usize) {
    for i in 0..n {
        rt.execute_query(&format!(
            "INSERT INTO net NODE (label, name) VALUES ('n{i}', 'Node {i}')"
        ))
        .unwrap_or_else(|e| panic!("seed node {i}: {e}"));
    }
    // Build a hub-and-spoke so degrees differ — n0 connects to every other node.
    for i in 1..n {
        rt.execute_query(&format!(
            "INSERT INTO net EDGE (label, from, to) VALUES ('e', 'n0', 'n{i}')"
        ))
        .unwrap_or_else(|e| panic!("seed edge n0->n{i}: {e}"));
    }
}

#[test]
fn graph_centrality_limit_caps_returned_rows() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_centrality_graph(&rt, 6);
    let res = rt
        .execute_query("GRAPH CENTRALITY LIMIT 3")
        .expect("limit 3 parses+executes");
    assert_eq!(res.result.records.len(), 3, "LIMIT 3 must cap output rows");
}

#[test]
fn graph_centrality_limit_zero_returns_no_rows() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_centrality_graph(&rt, 4);
    let res = rt
        .execute_query("GRAPH CENTRALITY LIMIT 0")
        .expect("limit 0 parses+executes");
    assert_eq!(
        res.result.records.len(),
        0,
        "LIMIT 0 returns zero rows (SQL semantics)"
    );
}

#[test]
fn graph_centrality_without_limit_uses_implicit_top_100() {
    // Sanity: omitted LIMIT keeps the historical implicit cap (verified by
    // simply executing and producing rows; cap exercised in scale tests).
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_centrality_graph(&rt, 4);
    let res = rt
        .execute_query("GRAPH CENTRALITY")
        .expect("no-limit form still works");
    assert!(
        !res.result.records.is_empty(),
        "default centrality must surface at least one row"
    );
    assert!(
        res.result.records.len() <= 100,
        "default cap is 100, got {}",
        res.result.records.len()
    );
}

#[test]
fn graph_centrality_limit_combined_with_algorithm() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_centrality_graph(&rt, 8);
    let res = rt
        .execute_query("GRAPH CENTRALITY ALGORITHM pagerank LIMIT 2")
        .expect("ALGORITHM + LIMIT both parse");
    assert_eq!(
        res.result.records.len(),
        2,
        "ALGORITHM pagerank LIMIT 2 must cap output rows"
    );
}

// ── Issue #422 slice: GRAPH COMPONENTS LIMIT N ─────────────────────────────

fn seed_components_graph(rt: &RedDBRuntime) {
    for label in ["a1", "a2", "a3", "b1", "b2", "c1"] {
        rt.execute_query(&format!(
            "INSERT INTO components_net NODE (label, name) VALUES ('{label}', '{label}')"
        ))
        .unwrap_or_else(|e| panic!("seed node {label}: {e}"));
    }
    for (from, to) in [("a1", "a2"), ("a2", "a3"), ("b1", "b2")] {
        rt.execute_query(&format!(
            "INSERT INTO components_net EDGE (label, from, to) VALUES ('link', '{from}', '{to}')"
        ))
        .unwrap_or_else(|e| panic!("seed edge {from}->{to}: {e}"));
    }
}

#[test]
fn graph_components_limit_caps_returned_rows() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_components_graph(&rt);
    let res = rt
        .execute_query("GRAPH COMPONENTS MODE weak LIMIT 2")
        .expect("components limit parses+executes");
    assert_eq!(res.result.records.len(), 2, "LIMIT 2 must cap output rows");
}

#[test]
fn graph_components_order_by_size_asc_then_limit() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_components_graph(&rt);
    let res = rt
        .execute_query("GRAPH COMPONENTS MODE weak ORDER BY component_size ASC LIMIT 2")
        .expect("components order+limit parses+executes");
    assert_eq!(res.result.records.len(), 2, "LIMIT 2 must cap output rows");
    assert_eq!(int_at(&res, 0, "size"), 1, "smallest component first");
    assert_eq!(
        int_at(&res, 1, "size"),
        2,
        "second-smallest component second"
    );
}

#[test]
fn graph_community_order_by_size_desc_limit_executes() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    seed_components_graph(&rt);
    let res = rt
        .execute_query("GRAPH COMMUNITY ALGORITHM louvain ORDER BY size DESC LIMIT 1")
        .expect("community order+limit parses+executes");
    assert!(
        res.result.records.len() <= 1,
        "LIMIT 1 must cap community output rows"
    );
}

#[test]
fn graph_shortest_path_limit_zero_returns_no_rows() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("INSERT INTO path_net NODE (label, name) VALUES ('alice', 'Alice')")
        .expect("insert alice");
    rt.execute_query("INSERT INTO path_net NODE (label, name) VALUES ('bob', 'Bob')")
        .expect("insert bob");
    let res = rt
        .execute_query("GRAPH SHORTEST_PATH 'alice' TO 'bob' LIMIT 0")
        .expect("shortest path limit parses+executes");
    assert_eq!(res.result.records.len(), 0, "LIMIT 0 returns zero rows");
}
