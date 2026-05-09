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
