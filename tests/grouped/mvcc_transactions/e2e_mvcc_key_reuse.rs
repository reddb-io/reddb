use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};

struct Connection;
impl Connection {
    fn use_id(id: u64) -> Self {
        set_current_connection_id(id);
        Self
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        clear_current_connection_id();
    }
}

fn schema(runtime: &RedDBRuntime) {
    runtime
        .execute_query("CREATE TABLE records (id INT PRIMARY KEY, email TEXT UNIQUE, payload TEXT)")
        .expect("schema");
}
fn insert(runtime: &RedDBRuntime, id: i64) -> reddb::RedDBResult<reddb::RuntimeQueryResult> {
    runtime.execute_query(&format!(
        "INSERT INTO records (id,email,payload) VALUES ({id},'email-{id}','value')"
    ))
}

#[test]
fn mvcc_releases_committed_update_and_delete_keys() {
    let _connection = Connection::use_id(227801);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    schema(&runtime);
    insert(&runtime, 1).expect("initial row");
    runtime
        .execute_query("UPDATE records SET id=2,email='email-2' WHERE id=1")
        .expect("replace key");
    insert(&runtime, 1).expect("old primary and unique keys released");
    assert!(insert(&runtime, 2).is_err());
    assert!(runtime
        .execute_query("INSERT INTO records (id,email) VALUES (3,'email-2')")
        .is_err());
    runtime
        .execute_query("DELETE FROM records WHERE id=2")
        .expect("delete");
    insert(&runtime, 2).expect("deleted keys released");
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM records")
            .expect("readback")
            .result
            .records
            .len(),
        2
    );
}

#[test]
fn mvcc_releases_aborted_and_own_superseded_keys() {
    let _connection = Connection::use_id(227802);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    schema(&runtime);
    runtime.execute_query("BEGIN").expect("begin");
    insert(&runtime, 1).expect("pending insert");
    runtime.execute_query("ROLLBACK").expect("rollback insert");
    insert(&runtime, 1).expect("aborted insert releases key");
    runtime.execute_query("BEGIN").expect("begin update");
    runtime
        .execute_query("UPDATE records SET id=2,email='email-2' WHERE id=1")
        .expect("pending update");
    insert(&runtime, 1).expect("own superseded key reusable");
    runtime.execute_query("ROLLBACK").expect("rollback update");
    assert!(insert(&runtime, 1).is_err(), "old key restored");
    insert(&runtime, 2).expect("aborted replacement releases key");
}

#[test]
fn mvcc_savepoint_key_release_preserves_parent_writes() {
    let _connection = Connection::use_id(227803);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    schema(&runtime);
    runtime.execute_query("BEGIN").expect("begin");
    insert(&runtime, 1).expect("parent insert");
    runtime.execute_query("SAVEPOINT s").expect("savepoint");
    runtime
        .execute_query("UPDATE records SET id=2,email='email-2' WHERE id=1")
        .expect("child update");
    insert(&runtime, 1).expect("own parent key released by child");
    runtime
        .execute_query("ROLLBACK TO SAVEPOINT s")
        .expect("rollback child");
    assert!(insert(&runtime, 1).is_err());
    runtime
        .execute_query("SAVEPOINT released")
        .expect("new savepoint");
    insert(&runtime, 2).expect("child key released");
    runtime
        .execute_query("RELEASE SAVEPOINT released")
        .expect("release");
    runtime.execute_query("COMMIT").expect("commit parent");
    assert!(insert(&runtime, 1).is_err());
    assert!(insert(&runtime, 2).is_err());
}

#[test]
fn mvcc_key_conflicts_include_invisible_writers_and_future_commits() {
    let _connection = Connection::use_id(227804);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    schema(&runtime);
    runtime.execute_query("BEGIN").expect("writer begin");
    insert(&runtime, 1).expect("writer reserves key");
    set_current_connection_id(227805);
    runtime.execute_query("BEGIN").expect("reader begin");
    assert!(runtime
        .execute_query("SELECT * FROM records")
        .expect("snapshot")
        .result
        .records
        .is_empty());
    assert!(
        insert(&runtime, 1).is_err(),
        "active invisible writer conflicts"
    );
    set_current_connection_id(227804);
    runtime.execute_query("ROLLBACK").expect("writer abort");
    insert(&runtime, 2).expect("commit newer than reader snapshot");
    set_current_connection_id(227805);
    assert!(
        insert(&runtime, 2).is_err(),
        "new committed key conflicts despite old snapshot"
    );
    insert(&runtime, 1).expect("aborted writer no longer conflicts");
    runtime.execute_query("COMMIT").expect("reader commit");
    set_current_connection_id(227804);
    runtime.execute_query("BEGIN").expect("delete begin");
    runtime
        .execute_query("DELETE FROM records WHERE id=1")
        .expect("pending delete");
    set_current_connection_id(227805);
    assert!(
        insert(&runtime, 1).is_err(),
        "another writer's pending deletion does not release key"
    );
    set_current_connection_id(227804);
    runtime.execute_query("COMMIT").expect("delete commit");
    set_current_connection_id(227805);
    insert(&runtime, 1).expect("committed deletion releases key");
}

#[test]
fn mvcc_concurrent_key_admission_has_one_winner() {
    let _connection = Connection::use_id(227806);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    schema(&runtime);
    for key in 1..=32 {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let outcomes: Vec<bool> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..2)
                .map(|worker| {
                    let runtime = &runtime;
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        let _connection = Connection::use_id(227807 + worker);
                        barrier.wait();
                        insert(runtime, key).is_ok()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().expect("writer thread"))
                .collect()
        });
        assert_eq!(
            outcomes.iter().filter(|success| **success).count(),
            1,
            "one admitted writer for key {key}"
        );
    }
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM records")
            .expect("readback")
            .result
            .records
            .len(),
        32
    );
}

#[test]
fn mvcc_reused_keys_survive_reopen() {
    let _connection = Connection::use_id(227810);
    let directory = tempfile::tempdir().expect("temp directory");
    let path = directory.path().join("keys.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        schema(&runtime);
        insert(&runtime, 1).expect("first");
        runtime
            .execute_query("UPDATE records SET id=2,email='email-2' WHERE id=1")
            .expect("replace");
        insert(&runtime, 1).expect("reuse");
        runtime.execute_query("BEGIN").expect("begin");
        insert(&runtime, 3).expect("pending");
        runtime.execute_query("ROLLBACK").expect("rollback");
        runtime
            .execute_query("BEGIN")
            .expect("begin update and delete");
        runtime
            .execute_query("UPDATE records SET id=4,email='email-4' WHERE id=1")
            .expect("pending replacement");
        runtime
            .execute_query("DELETE FROM records WHERE id=2")
            .expect("pending delete");
        runtime
            .execute_query("ROLLBACK")
            .expect("rollback update and delete");
        runtime
            .db()
            .flush()
            .expect("flush excludes aborted versions");
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    assert!(insert(&runtime, 1).is_err());
    assert!(insert(&runtime, 2).is_err());
    insert(&runtime, 3).expect("aborted key free after reopen");
    insert(&runtime, 4).expect("aborted replacement key free after reopen");
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM records")
            .expect("readback")
            .result
            .records
            .len(),
        4
    );
}

#[test]
fn mvcc_concurrent_unique_update_and_insert_have_one_winner() {
    let _connection = Connection::use_id(227811);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    schema(&runtime);
    for key in 1..=32 {
        insert(&runtime, key).expect("initial row");
        let barrier = std::sync::Barrier::new(2);
        let successes = std::thread::scope(|scope| {
            let update = scope.spawn(|| {
                let _connection = Connection::use_id(227812);
                barrier.wait();
                runtime
                    .execute_query(&format!(
                        "UPDATE records SET email='shared-{key}' WHERE id={key}"
                    ))
                    .is_ok()
            });
            let insert = scope.spawn(|| {
                let _connection = Connection::use_id(227813);
                barrier.wait();
                runtime
                    .execute_query(&format!(
                        "INSERT INTO records (id,email) VALUES ({},'shared-{key}')",
                        key + 100
                    ))
                    .is_ok()
            });
            usize::from(update.join().expect("update writer"))
                + usize::from(insert.join().expect("insert writer"))
        });
        assert_eq!(successes, 1, "one writer owns unique email for key {key}");
        assert_eq!(
            runtime
                .execute_query(&format!("SELECT * FROM records WHERE email='shared-{key}'"))
                .expect("readback")
                .result
                .records
                .len(),
            1
        );
    }
}

#[test]
fn mvcc_on_conflict_reuses_history_and_rejects_active_writer() {
    let _connection = Connection::use_id(227814);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    schema(&runtime);
    insert(&runtime, 1).expect("initial row");
    runtime
        .execute_query("UPDATE records SET id=2,email='email-2' WHERE id=1")
        .expect("replace keys");
    let query = "INSERT INTO records (id,email) VALUES (1,'email-1') ON CONFLICT (id) DO NOTHING";
    assert_eq!(
        runtime
            .execute_query(query)
            .expect("reuse historical key")
            .affected_rows,
        1
    );
    assert_eq!(
        runtime
            .execute_query(query)
            .expect("skip live duplicate")
            .affected_rows,
        0
    );
    runtime.execute_query("BEGIN").expect("begin");
    insert(&runtime, 3).expect("pending key");
    runtime
        .execute_query("SAVEPOINT nested")
        .expect("savepoint");
    runtime.execute_query("INSERT INTO records (id,email) VALUES (3,'email-3') ON CONFLICT (id) DO UPDATE SET payload='own update'")
        .expect("upsert can update its own parent transaction's row");
    set_current_connection_id(227815);
    let pending = "INSERT INTO records (id,email) VALUES (3,'email-3') ON CONFLICT (id) DO UPDATE SET payload='replacement'";
    assert!(
        runtime.execute_query(pending).is_err(),
        "cannot update invisible writer"
    );
    set_current_connection_id(227814);
    runtime.execute_query("ROLLBACK").expect("rollback");
    set_current_connection_id(227815);
    assert_eq!(
        runtime
            .execute_query(pending)
            .expect("aborted key reusable")
            .affected_rows,
        1
    );
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM records")
            .expect("readback")
            .result
            .records
            .len(),
        3
    );
}

#[test]
fn mvcc_typed_single_and_batch_share_unique_admission() {
    use reddb::application::{CreateRowInput, CreateRowsBatchInput, RuntimeEntityPort};
    use reddb_types::Value;
    let _connection = Connection::use_id(227816);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    schema(&runtime);
    for key in 1..=32 {
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..2)
                .map(|worker| {
                    let barrier = &barrier;
                    let runtime = &runtime;
                    scope.spawn(move || {
                        let _connection = Connection::use_id(227817 + worker);
                        let row = CreateRowInput {
                            collection: "records".into(),
                            fields: vec![
                                ("id".into(), Value::Integer(key + worker as i64 * 100)),
                                ("email".into(), Value::text(format!("shared-{key}"))),
                            ],
                            metadata: Vec::new(),
                            node_links: Vec::new(),
                            vector_links: Vec::new(),
                        };
                        barrier.wait();
                        if worker == 0 {
                            runtime.create_row(row).is_ok()
                        } else {
                            runtime
                                .create_rows_batch(CreateRowsBatchInput {
                                    collection: "records".into(),
                                    rows: vec![row],
                                    suppress_events: false,
                                })
                                .is_ok()
                        }
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().expect("typed writer"))
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|success| **success).count(), 1);
    }
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM records")
            .expect("readback")
            .result
            .records
            .len(),
        32
    );
}
