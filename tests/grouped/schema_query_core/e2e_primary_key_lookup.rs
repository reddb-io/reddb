use reddb::{RedDBOptions, RedDBRuntime};

fn insert(runtime: &RedDBRuntime, key: i64) {
    runtime
        .execute_query(&format!(
            "INSERT INTO records (payload, id) VALUES ('value', {key})"
        ))
        .expect("distinct primary key");
}

fn reject(runtime: &RedDBRuntime, key: i64) {
    assert!(runtime
        .execute_query(&format!(
            "INSERT INTO records (payload, id) VALUES ('duplicate', {key})"
        ))
        .is_err());
}

#[test]
fn primary_key_lookup_tracks_bulk_update_delete_and_rollback() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("CREATE TABLE records (payload TEXT, id INT PRIMARY KEY)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO records (payload,id) VALUES ('one',1),('two',2),('three',3)")
        .expect("bulk insert");
    reject(&runtime, 2);
    insert(&runtime, 4);
    reject(&runtime, 4);
    runtime
        .execute_query("UPDATE records SET id=20 WHERE id=2")
        .expect("change key");
    reject(&runtime, 20);
    runtime
        .execute_query("DELETE FROM records WHERE id=3")
        .expect("delete key");
    insert(&runtime, 30);
    runtime.execute_query("BEGIN").expect("begin");
    insert(&runtime, 5);
    reject(&runtime, 5);
    runtime.execute_query("ROLLBACK").expect("rollback");
    assert!(runtime
        .execute_query("SELECT * FROM records WHERE id=5")
        .expect("rolled back row hidden")
        .result
        .records
        .is_empty());
    insert(&runtime, 6);
    reject(&runtime, 6);
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM records")
            .expect("readback")
            .result
            .records
            .len(),
        5
    );
}

#[test]
fn primary_key_lookup_rebuilds_after_reopen() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("keys.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        runtime
            .execute_query("CREATE TABLE records (payload TEXT, id INT PRIMARY KEY)")
            .expect("table");
        for key in 1..=64 {
            insert(&runtime, key);
        }
        reject(&runtime, 64);
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    reject(&runtime, 1);
    reject(&runtime, 64);
    insert(&runtime, 65);
    reject(&runtime, 65);
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM records")
            .expect("readback")
            .result
            .records
            .len(),
        65
    );
}
