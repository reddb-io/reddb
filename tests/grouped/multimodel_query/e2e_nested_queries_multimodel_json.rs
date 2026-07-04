//! Runtime contract for nested query composition across SQL, graph TVFs,
//! joins, and JSON literals.

use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn first_record<'a>(
    result: &'a reddb::runtime::RuntimeQueryResult,
) -> &'a reddb::storage::query::UnifiedRecord {
    assert_eq!(
        result.result.records.len(),
        1,
        "expected one row for `{}`; got {:?}",
        result.query,
        result.result.records
    );
    &result.result.records[0]
}

fn text_value(record: &reddb::storage::query::UnifiedRecord, names: &[&str]) -> String {
    for name in names {
        if let Some(value) = record.get(*name) {
            return match value {
                Value::Text(value) => value.to_string(),
                Value::Integer(value) => value.to_string(),
                Value::UnsignedInteger(value) => value.to_string(),
                Value::Float(value) => value.to_string(),
                other => panic!("expected text-compatible {name}, got {other:?} in {record:?}"),
            };
        }
    }
    panic!("missing any of {names:?} in {record:?}");
}

fn int_value(record: &reddb::storage::query::UnifiedRecord, names: &[&str]) -> i64 {
    for name in names {
        if let Some(value) = record.get(*name) {
            return match value {
                Value::Integer(value) => *value,
                Value::UnsignedInteger(value) => *value as i64,
                other => panic!("expected integer {name}, got {other:?} in {record:?}"),
            };
        }
    }
    panic!("missing any of {names:?} in {record:?}");
}

fn ids_from(rt: &RedDBRuntime, sql: &str) -> Vec<i64> {
    exec(rt, sql)
        .result
        .records
        .iter()
        .map(|record| int_value(record, &["id", "u.id", "users.id", "c0"]))
        .collect()
}

fn seed_users_orders_regions(rt: &RedDBRuntime) {
    exec(
        rt,
        "CREATE TABLE users (id INT, name TEXT, status TEXT, region TEXT, profile_key TEXT)",
    );
    exec(
        rt,
        "INSERT INTO users (id, name, status, region, profile_key) VALUES \
         (1, 'alice', 'active', 'west', 'alice'), \
         (2, 'bob', 'inactive', 'east', 'bob'), \
         (3, 'carol', 'active', 'west', 'carol'), \
         (4, 'dave', 'active', 'south', 'dave')",
    );
    exec(rt, "CREATE TABLE regions (id INT, name TEXT, open BOOLEAN)");
    exec(
        rt,
        "INSERT INTO regions (id, name, open) VALUES \
         (10, 'west', true), (20, 'east', false), (30, 'south', true)",
    );
    exec(
        rt,
        "CREATE TABLE orders (id INT, user_id INT, region_id INT, total INT)",
    );
    exec(
        rt,
        "INSERT INTO orders (id, user_id, region_id, total) VALUES \
         (101, 1, 10, 50), (102, 3, 10, 200), (103, 4, 30, 150), (104, 2, 20, 500)",
    );
}

#[test]
fn subquery_can_nest_inside_subquery() {
    let rt = runtime();
    seed_users_orders_regions(&rt);

    let ids = ids_from(
        &rt,
        "SELECT id FROM users \
         WHERE id IN ( \
             SELECT user_id FROM orders \
             WHERE region_id IN ( \
                 SELECT id FROM regions WHERE open = true AND name = 'west' \
             ) \
         ) \
         ORDER BY id",
    );

    assert_eq!(ids, vec![1, 3]);
}

#[test]
fn from_subquery_can_itself_contain_from_and_where_subqueries() {
    let rt = runtime();
    seed_users_orders_regions(&rt);

    let ids = ids_from(
        &rt,
        "SELECT id FROM ( \
             SELECT user_id AS id FROM ( \
                 SELECT user_id, region_id, total FROM orders WHERE total > 100 \
             ) AS rich_orders \
             WHERE region_id IN (SELECT id FROM regions WHERE open = true) \
         ) AS buyers \
         ORDER BY id",
    );

    assert_eq!(ids, vec![3, 4]);
}

#[test]
fn join_accepts_subquery_side_and_kv_json_side() {
    let rt = runtime();
    seed_users_orders_regions(&rt);
    exec(&rt, "CREATE KV profiles");
    exec(
        &rt,
        "KV PUT profiles.alice = {\"user\":{\"plan\":\"free\",\"tier\":1}}",
    );
    exec(
        &rt,
        "KV PUT profiles.carol = {\"user\":{\"plan\":\"pro\",\"tier\":2}}",
    );

    let result = exec(
        &rt,
        "SELECT u.id, p.value.user.plan AS plan \
         FROM (SELECT id, profile_key FROM users WHERE status = 'active') AS u \
         JOIN profiles p ON u.profile_key = p.key \
         WHERE p.value.user.tier = 2",
    );
    let row = first_record(&result);
    assert_eq!(int_value(row, &["id", "u.id"]), 3);
    assert_eq!(text_value(row, &["plan"]), "pro");
}

#[test]
fn join_accepts_table_and_timeseries_collection() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE services (id INT, metric_name TEXT)");
    exec(
        &rt,
        "INSERT INTO services (id, metric_name) VALUES \
         (1, 'checkout.latency'), (2, 'billing.latency')",
    );
    exec(&rt, "CREATE TIMESERIES service_metrics RETENTION 7 d");
    exec(
        &rt,
        "INSERT INTO service_metrics (metric, value, tags, timestamp) VALUES \
         ('checkout.latency', 42.0, {\"service\":\"checkout\"}, 1704067200000000000), \
         ('search.latency', 7.0, {\"service\":\"search\"}, 1704067200000000001)",
    );

    let result = exec(
        &rt,
        "SELECT s.id, m.value AS metric_value \
         FROM services s JOIN service_metrics m ON s.metric_name = m.metric",
    );
    let row = first_record(&result);
    assert_eq!(int_value(row, &["id", "s.id"]), 1);
    assert_eq!(text_value(row, &["metric_value"]), "42");
}

#[test]
fn graph_table_function_subqueries_can_reference_ctes_and_nested_subqueries() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE gnodes (id INT, enabled BOOLEAN)");
    exec(
        &rt,
        "INSERT INTO gnodes (id, enabled) VALUES (1, true), (2, true), (3, true), (4, false)",
    );
    exec(&rt, "CREATE TABLE gedges (src INT, dst INT)");
    exec(
        &rt,
        "INSERT INTO gedges (src, dst) VALUES (1, 2), (2, 3), (3, 4)",
    );

    let result = exec(
        &rt,
        "WITH enabled_nodes AS (SELECT id FROM gnodes WHERE enabled = true) \
         SELECT * FROM components( \
             nodes => (SELECT id FROM enabled_nodes), \
             edges => ( \
                 SELECT src, dst FROM gedges \
                 WHERE src IN (SELECT id FROM enabled_nodes) \
                   AND dst IN (SELECT id FROM enabled_nodes) \
             ) \
         )",
    );

    assert_eq!(result.engine, "runtime-graph-tvf-inline");
    assert_eq!(
        result.result.records.len(),
        3,
        "records={:?}",
        result.result.records
    );
}

#[test]
fn bare_json_literals_insert_into_documents_kv_and_table_json_columns() {
    let rt = runtime();

    exec(&rt, "CREATE DOCUMENT docs_json");
    exec(
        &rt,
        "INSERT INTO docs_json DOCUMENT VALUES \
         ({\"user\":{\"name\":\"Ada\",\"active\":true},\"tags\":[\"beta\"],\"score\":7})",
    );
    let doc = exec(
        &rt,
        "SELECT body.user.name AS name FROM docs_json WHERE body.user.active = true",
    );
    assert_eq!(text_value(first_record(&doc), &["name"]), "Ada");

    exec(&rt, "CREATE KV kv_json");
    exec(
        &rt,
        "KV PUT kv_json.profile = {\"user\":{\"name\":\"Ada\",\"plan\":\"pro\"}}",
    );
    let kv = exec(
        &rt,
        "SELECT value.user.plan AS plan FROM kv_json WHERE key = 'profile'",
    );
    assert_eq!(text_value(first_record(&kv), &["plan"]), "pro");

    exec(&rt, "CREATE TABLE table_json (id INT, payload JSON)");
    exec(
        &rt,
        "INSERT INTO table_json (id, payload) VALUES \
         (1, {\"user\":{\"name\":\"Ada\",\"plan\":\"pro\"}})",
    );
    let table = exec(
        &rt,
        "SELECT payload.user.name AS name FROM table_json WHERE payload.user.plan = 'pro'",
    );
    assert_eq!(text_value(first_record(&table), &["name"]), "Ada");
}
