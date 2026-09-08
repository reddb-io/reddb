use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn execute(rt: &RedDBRuntime, query: &str) {
    rt.execute_query(query)
        .unwrap_or_else(|err| panic!("{query}: {err}"));
}

fn duplicate(rt: &RedDBRuntime, query: &str) {
    let error = rt
        .execute_query(query)
        .expect_err("duplicate must fail")
        .to_string();
    assert!(error.to_ascii_lowercase().contains("unique"), "{error}");
}

#[test]
fn table_unique_constraints_enforce_tuples_nulls_updates_and_savepoints() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    execute(&rt, "CREATE TABLE pairs (id INT PRIMARY KEY, left_key INT, right_key TEXT, CONSTRAINT pair_key UNIQUE (left_key, right_key))");
    execute(&rt, "INSERT INTO pairs VALUES (1,1,'a'),(2,1,'b'),(3,2,'a'),(4,NULL,'a'),(5,NULL,'a'),(6,1,NULL),(7,1,NULL)");
    duplicate(&rt, "INSERT INTO pairs VALUES (8,1,'a')");
    duplicate(&rt, "UPDATE pairs SET right_key='a' WHERE id=2");
    execute(&rt, "BEGIN");
    execute(&rt, "SAVEPOINT probe");
    execute(&rt, "INSERT INTO pairs VALUES (8,3,'a')");
    duplicate(&rt, "INSERT INTO pairs VALUES (9,3,'a')");
    execute(&rt, "ROLLBACK TO SAVEPOINT probe");
    execute(&rt, "INSERT INTO pairs VALUES (8,3,'a')");
    execute(&rt, "COMMIT");
    execute(&rt, "DELETE FROM pairs WHERE id=1");
    execute(&rt, "INSERT INTO pairs VALUES (9,1,'a')");
    let result = rt
        .execute_query("SELECT id FROM pairs WHERE left_key=1 AND right_key='b'")
        .expect("failed update is atomic");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(result.result.records[0].get("id"), Some(&Value::Integer(2)));
}

#[test]
fn table_unique_constraints_reject_invalid_definitions_before_creation() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    for clause in [
        "UNIQUE (missing)",
        "UNIQUE (left_key, LEFT_KEY)",
        "CONSTRAINT same UNIQUE (left_key), CONSTRAINT SAME UNIQUE (right_key)",
        "CONSTRAINT uniq_left_key UNIQUE (right_key)",
    ] {
        let error = rt
            .execute_query(&format!(
                "CREATE TABLE invalid_pair (left_key INT UNIQUE, right_key TEXT, {clause})"
            ))
            .expect_err("invalid definition");
        assert!(error.to_string().contains("UNIQUE"), "{error}");
        execute(&rt, "CREATE TABLE invalid_pair (id INT)");
        execute(&rt, "DROP TABLE invalid_pair");
    }
}

#[test]
fn table_unique_constraints_survive_reopen_and_show_create_roundtrip() {
    let directory = tempfile::tempdir().expect("directory");
    let options = RedDBOptions::persistent(directory.path().join("unique.rdb"));
    let ddl;
    {
        let rt = RedDBRuntime::with_options(options.clone()).expect("runtime");
        execute(&rt, "CREATE TABLE pairs (id INT PRIMARY KEY, left_key INT, right_key TEXT, UNIQUE (left_key, right_key), CONSTRAINT uniq_left_key_right_key UNIQUE (right_key, left_key))");
        execute(&rt, "INSERT INTO pairs VALUES (1,1,'a')");
        let result = rt.execute_query("SHOW CREATE TABLE pairs").expect("DDL");
        let Some(Value::Text(value)) = result.result.records[0].get("ddl") else {
            panic!("DDL text")
        };
        ddl = value.to_string();
        assert!(
            ddl.contains("CONSTRAINT uniq_left_key_right_key_2 UNIQUE (left_key, right_key)"),
            "{ddl}"
        );
        assert!(
            ddl.contains("CONSTRAINT uniq_left_key_right_key UNIQUE (right_key, left_key)"),
            "{ddl}"
        );
    }
    let reopened = RedDBRuntime::with_options(options).expect("reopen");
    duplicate(&reopened, "INSERT INTO pairs VALUES (2,1,'a')");
    let fresh = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("fresh");
    for statement in ddl
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        execute(&fresh, statement);
    }
    execute(&fresh, "INSERT INTO pairs VALUES (1,1,'a')");
    duplicate(&fresh, "INSERT INTO pairs VALUES (2,1,'a')");
}

#[test]
fn table_unique_constraints_explain_alter_never_omits_constraint_changes() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    execute(
        &rt,
        "CREATE TABLE pairs (left_key INT, right_key TEXT, UNIQUE (left_key, right_key))",
    );
    execute(&rt, "EXPLAIN ALTER FOR CREATE TABLE pairs (left_key INT, right_key TEXT, UNIQUE (left_key, right_key))");
    for query in [
        "EXPLAIN ALTER FOR CREATE TABLE pairs (left_key INT, right_key TEXT)",
        "EXPLAIN ALTER FOR CREATE TABLE fresh (left_key INT, right_key TEXT, UNIQUE (left_key, right_key))",
    ] {
        let error = rt.execute_query(query).expect_err("unsupported constraint migration");
        assert!(error.to_string().contains("table-level UNIQUE"), "{error}");
    }
}
