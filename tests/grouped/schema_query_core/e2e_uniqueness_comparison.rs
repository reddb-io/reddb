use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn insert_uniqueness_preserves_keys_nulls_and_update_exclusion() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime.execute_query("CREATE TABLE accounts (org INT PRIMARY KEY, name TEXT, email TEXT UNIQUE, payload TEXT)").expect("table");
    for sql in [
        "INSERT INTO accounts (org,name,email,payload) VALUES (1,'alice','a@fixture','old')",
        "INSERT INTO accounts (org,name,email,payload) VALUES (2,'alice',NULL,'other org')",
        "INSERT INTO accounts (org,name,email,payload) VALUES (3,'bob',NULL,'other name')",
    ] {
        runtime
            .execute_query(sql)
            .expect("distinct key or nullable unique");
    }
    for sql in [
        "INSERT INTO accounts (org,name,email) VALUES (1,'alice','distinct@fixture')",
        "INSERT INTO accounts (org,name,email) VALUES (4,'carol','a@fixture')",
        "INSERT INTO accounts (org,name,email) VALUES (NULL,'dave','d@fixture')",
    ] {
        assert!(runtime.execute_query(sql).is_err(), "must reject {sql}");
    }
    runtime
        .execute_query("UPDATE accounts SET name='alice',email='a@fixture',payload='new' WHERE org=1 AND name='alice'")
        .expect("same entity retains its keys");
    assert!(runtime
        .execute_query("UPDATE accounts SET email='a@fixture' WHERE name='bob'")
        .is_err());
    let rows = runtime
        .execute_query("SELECT payload FROM accounts WHERE org=1 AND name='alice'")
        .expect("readback");
    assert_eq!(rows.result.records.len(), 1);
    assert_eq!(
        rows.result.records[0].get("payload"),
        Some(&Value::text("new"))
    );
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM accounts")
            .expect("read all")
            .result
            .records
            .len(),
        3
    );
}

#[test]
fn insert_uniqueness_compares_complete_text_and_integer_keys() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("CREATE TABLE names (id TEXT PRIMARY KEY, serial_number INT UNIQUE)")
        .expect("table");
    let prefix = "á\\\"\n".repeat(512);
    let first = Value::text(format!("{prefix}a"));
    let second = Value::text(format!("{prefix}b"));
    let sql = "INSERT INTO names (id,serial_number) VALUES ($1,$2)";
    runtime
        .execute_query_with_params(sql, &[first.clone(), Value::Integer(i64::MIN)])
        .expect("first");
    runtime
        .execute_query_with_params(sql, &[second.clone(), Value::Integer(i64::MAX)])
        .expect("different suffix and integer");
    assert!(runtime
        .execute_query_with_params(sql, &[first, Value::Integer(0)])
        .is_err());
    assert!(runtime
        .execute_query_with_params(sql, &[Value::text("third"), Value::Integer(i64::MAX)])
        .is_err());
    let rows = runtime
        .execute_query("SELECT * FROM names")
        .expect("readback");
    assert_eq!(rows.result.records.len(), 2);
}
