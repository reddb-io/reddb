use reddb::auth::Role;
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn parameter_cache_rebinds_types_and_preserves_arity_errors() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let query = "SELECT $1 AS bound_value";
    for value in [
        Value::Integer(i64::MAX),
        Value::text("a'\"b"),
        Value::Null,
        Value::Boolean(false),
        Value::Vector(vec![0.8, 0.6]),
        Value::Blob(vec![0, 1, 255]),
    ] {
        let result = rt
            .execute_query_with_params(query, std::slice::from_ref(&value))
            .expect("rebound value");
        assert_eq!(result.result.records[0].get("bound_value"), Some(&value));
    }
    for params in [vec![], vec![Value::Integer(1), Value::Integer(2)]] {
        assert!(rt.execute_query_with_params(query, &params).is_err());
    }
    let result = rt
        .execute_query_with_params(query, &[Value::Integer(9)])
        .expect("valid after invalid");
    assert_eq!(
        result.result.records[0].get("bound_value"),
        Some(&Value::Integer(9))
    );
    let result = rt
        .execute_query_with_params("SELECT 7 AS bound_value", &[])
        .expect("exact SQL text");
    assert_eq!(
        result.result.records[0].get("bound_value"),
        Some(&Value::Integer(7))
    );
}

#[test]
fn parameter_cache_preserves_transactions_and_recreated_schema() {
    let directory = tempfile::tempdir().expect("fixture");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(
        directory.path().join("params.rdb"),
    ))
    .expect("runtime");
    rt.execute_query("CREATE TABLE items (id INT PRIMARY KEY, payload TEXT)")
        .expect("table");
    let insert = "INSERT INTO items (id,payload) VALUES ($1,$2)";
    rt.execute_query("BEGIN").expect("begin");
    rt.execute_query_with_params(insert, &[Value::Integer(1), Value::text("keep")])
        .expect("first");
    rt.execute_query("SAVEPOINT before_second")
        .expect("savepoint");
    rt.execute_query_with_params(insert, &[Value::Integer(2), Value::text("discard")])
        .expect("second");
    rt.execute_query("ROLLBACK TO SAVEPOINT before_second")
        .expect("rollback");
    rt.execute_query_with_params(insert, &[Value::Integer(2), Value::text("replacement")])
        .expect("reuse");
    rt.execute_query("COMMIT").expect("commit");
    let query = "SELECT payload FROM items WHERE id=$1";
    let result = rt
        .execute_query_with_params(query, &[Value::Integer(2)])
        .expect("read");
    assert_eq!(
        result.result.records[0].get("payload"),
        Some(&Value::text("replacement"))
    );
    rt.execute_query("DROP TABLE items").expect("drop");
    rt.execute_query("CREATE TABLE items (id INT PRIMARY KEY, payload INT)")
        .expect("new schema");
    rt.execute_query_with_params(insert, &[Value::Integer(2), Value::Integer(99)])
        .expect("new column type");
    let result = rt
        .execute_query_with_params(query, &[Value::Integer(2)])
        .expect("new read");
    assert_eq!(
        result.result.records[0].get("payload"),
        Some(&Value::Integer(99))
    );
}

#[test]
fn parameter_cache_rechecks_identity_and_policy_changes() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    for query in [
        "CREATE TABLE guarded (id INT, owner TEXT)",
        "INSERT INTO guarded (id,owner) VALUES (1,'alice'),(2,'bob')",
        "CREATE POLICY own_rows ON guarded USING (owner=CURRENT_USER())",
        "ALTER TABLE guarded ENABLE ROW LEVEL SECURITY",
    ] {
        rt.execute_query(query).expect("fixture");
    }
    let query = "SELECT id FROM guarded WHERE id >= $1";
    for (user, id) in [("alice", 1), ("bob", 2), ("alice", 1)] {
        set_current_auth_identity(user.into(), Role::Read);
        let result = rt.execute_query_with_params(query, &[Value::Integer(0)]);
        clear_current_auth_identity();
        let result = result.expect("scoped query");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("id"),
            Some(&Value::Integer(id))
        );
    }
    rt.execute_query("DROP POLICY own_rows ON guarded")
        .expect("revoke");
    set_current_auth_identity("alice".into(), Role::Read);
    let result = rt.execute_query_with_params(query, &[Value::Integer(0)]);
    clear_current_auth_identity();
    assert!(result.expect("default denial").result.records.is_empty());
}

#[test]
fn parameter_cache_resolves_current_configuration() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    rt.execute_query("CREATE TABLE probes (id INT)")
        .expect("table");
    rt.execute_query("INSERT INTO probes (id) VALUES (1)")
        .expect("row");
    let query = "SELECT $config.mode AS cfg_mode FROM probes WHERE id=$1";
    for mode in ["first", "second", "first"] {
        rt.execute_query(&format!("SET CONFIG red.config.mode = '{mode}'"))
            .expect("set config");
        let result = rt
            .execute_query_with_params(query, &[Value::Integer(1)])
            .expect("live config");
        assert_eq!(
            result.result.records[0].get("cfg_mode"),
            Some(&Value::text(mode))
        );
    }
}
