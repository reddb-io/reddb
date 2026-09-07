fn set_unique_columns(runtime: &RedDBRuntime, name: &str, columns: &[&str]) {
    let db = runtime.db();
    let mut contract = db.collection_contract(name).expect("contract");
    let table = contract.table_def.as_mut().expect("table definition");
    table
        .constraints
        .retain(|constraint| constraint.name != "extra_unique");
    if !columns.is_empty() {
        table.constraints.push(
            reddb_types::Constraint::new("extra_unique", reddb_types::ConstraintType::Unique)
                .on_columns(columns.iter().map(|column| column.to_string()).collect()),
        );
    }
    db.save_collection_contract(contract)
        .expect("save catalog constraint");
}

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};

#[test]
fn unique_lookup_tracks_multiple_constraints_nulls_and_schema_changes() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query(
            "CREATE TABLE accounts (id INT PRIMARY KEY, email TEXT UNIQUE, org INT, code TEXT)",
        )
        .expect("schema");
    set_unique_columns(&runtime, "accounts", &["org", "code"]);
    runtime.execute_query("INSERT INTO accounts (id,email,org,code) VALUES (1,'one',1,'a'),(2,'two',2,'a'),(3,NULL,1,NULL),(4,NULL,1,NULL)").expect("distinct and nullable keys");
    for sql in [
        "INSERT INTO accounts (id,email,org,code) VALUES (5,'one',3,'a')",
        "INSERT INTO accounts (id,email,org,code) VALUES (5,'five',1,'a')",
        "INSERT INTO accounts (id,email,org,code) VALUES (1,'five',3,'a')",
        "INSERT INTO accounts (id,email,org,code) VALUES (5,'five',3,'a'),(6,'six',3,'a')",
    ] {
        assert!(runtime.execute_query(sql).is_err(), "reject {sql}");
    }
    runtime
        .execute_query("UPDATE accounts SET email='changed',org=3,code='b' WHERE id=1")
        .expect("replace keys");
    runtime
        .execute_query("INSERT INTO accounts (id,email,org,code) VALUES (5,'one',1,'a')")
        .expect("reuse keys");
    assert_eq!(runtime.execute_query("INSERT INTO accounts (id,email,org,code) VALUES (6,'six',1,'a') ON CONFLICT (org,code) DO NOTHING").expect("composite conflict").affected_rows, 0);
    runtime
        .execute_query("CREATE TABLE schema_keys (id INT PRIMARY KEY, code TEXT)")
        .expect("schema");
    runtime
        .execute_query("INSERT INTO schema_keys (id,code) VALUES (1,'one')")
        .expect("warm primary cache");
    set_unique_columns(&runtime, "schema_keys", &["code"]);
    assert!(runtime
        .execute_query("INSERT INTO schema_keys (id,code) VALUES (2,'one')")
        .is_err());
    set_unique_columns(&runtime, "schema_keys", &[]);
    runtime
        .execute_query("INSERT INTO schema_keys (id,code) VALUES (2,'one')")
        .expect("removed constraint no longer applies");
}

#[test]
fn unique_composite_keys_preserve_savepoints_and_reopen() {
    set_current_connection_id(227901);
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("keys.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        runtime
            .execute_query(
                "CREATE TABLE accounts (id INT PRIMARY KEY, email TEXT UNIQUE, org INT, code TEXT)",
            )
            .expect("schema");
        set_unique_columns(&runtime, "accounts", &["org", "code"]);
        runtime
            .execute_query("INSERT INTO accounts (id,email,org,code) VALUES (1,'one',1,'a')")
            .expect("initial");
        runtime.execute_query("BEGIN").expect("begin");
        runtime.execute_query("SAVEPOINT child").expect("savepoint");
        runtime
            .execute_query("UPDATE accounts SET email='two',org=2 WHERE id=1")
            .expect("replace composite");
        runtime
            .execute_query("INSERT INTO accounts (id,email,org,code) VALUES (2,'one',1,'a')")
            .expect("reuse own keys");
        runtime
            .execute_query("ROLLBACK TO SAVEPOINT child")
            .expect("rollback child");
        assert!(runtime
            .execute_query("INSERT INTO accounts (id,email,org,code) VALUES (2,'two',1,'a')")
            .is_err());
        runtime
            .execute_query("INSERT INTO accounts (id,email,org,code) VALUES (2,'two',2,'a')")
            .expect("aborted replacement released");
        runtime.execute_query("COMMIT").expect("commit");
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    assert!(runtime
        .execute_query("INSERT INTO accounts (id,email,org,code) VALUES (3,'three',1,'a')")
        .is_err());
    assert!(runtime
        .execute_query("INSERT INTO accounts (id,email,org,code) VALUES (3,'two',3,'a')")
        .is_err());
    runtime
        .execute_query("DELETE FROM accounts WHERE id=2")
        .expect("delete");
    runtime
        .execute_query("INSERT INTO accounts (id,email,org,code) VALUES (3,'two',2,'a')")
        .expect("reuse deleted composite");
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM accounts")
            .expect("readback")
            .result
            .records
            .len(),
        2
    );
    clear_current_connection_id();
}
