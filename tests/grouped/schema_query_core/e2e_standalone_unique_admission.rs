use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use std::sync::{Arc, Barrier};

fn seed(runtime: &RedDBRuntime) {
    runtime
        .execute_query("INSERT INTO unique_probe (id,body) VALUES (1,'existing')")
        .expect("seed implicit collection");
    runtime
        .execute_query("CREATE UNIQUE INDEX unique_probe_key ON unique_probe (id) USING HASH")
        .expect("create standalone index");
}

fn assert_original_row(runtime: &RedDBRuntime) {
    let rows = runtime
        .execute_query("SELECT * FROM unique_probe")
        .expect("read complete collection")
        .result
        .records;
    assert_eq!(rows.len(), 1, "failed/skipped writes must leave no rows");
    assert_eq!(
        rows[0].get("body"),
        Some(&reddb_types::Value::text("existing"))
    );
}

#[test]
fn standalone_unique_rejects_before_installing_any_batch_row() {
    for insert in [
        "INSERT INTO unique_probe (id,body) VALUES (1,'rejected')",
        "INSERT INTO unique_probe (id,body) VALUES (2,'new'),(1,'rejected')",
        "INSERT INTO unique_probe (id,body) VALUES (2,'new'),(2,'duplicate')",
    ] {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        seed(&runtime);
        assert!(runtime.execute_query(insert).is_err(), "reject {insert}");
        assert_original_row(&runtime);
    }
}

#[test]
fn standalone_unique_do_nothing_checks_existing_and_proposed_rows() {
    for target in ["", "(id)"] {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        seed(&runtime);
        let skipped = runtime
            .execute_query(&format!(
                "INSERT INTO unique_probe (id,body) VALUES (1,'skipped') ON CONFLICT {target} DO NOTHING"
            ))
            .expect("standalone conflict is skipped successfully");
        assert_eq!(skipped.affected_rows, 0);
        assert_original_row(&runtime);
        let inserted = runtime.execute_query(&format!(
            "INSERT INTO unique_probe (id,body) VALUES (1,'skipped'),(2,'first'),(2,'second') ON CONFLICT {target} DO NOTHING"
        )).expect("deduplicate before batch installation");
        assert_eq!(inserted.affected_rows, 1);
        let rows = runtime
            .execute_query("SELECT * FROM unique_probe WHERE id=2")
            .expect("read new indexed key")
            .result
            .records;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("body"),
            Some(&reddb_types::Value::text("first"))
        );
    }
}

#[test]
fn standalone_unique_failed_insert_survives_reopen() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("unique.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        seed(&runtime);
        assert!(runtime
            .execute_query("INSERT INTO unique_probe (id,body) VALUES (1,'rejected')")
            .is_err());
        assert_original_row(&runtime);
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .expect("reopen after rejected write");
    assert_original_row(&runtime);
    assert_eq!(
        runtime
            .execute_query(
                "INSERT INTO unique_probe (id,body) VALUES (1,'skipped') ON CONFLICT DO NOTHING"
            )
            .expect("index still enforces conflict after reopen")
            .affected_rows,
        0
    );
    assert_original_row(&runtime);
}

#[test]
fn standalone_unique_serializes_plain_and_conflict_inserts() {
    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime"));
    seed(&runtime);
    for key in 2..18 {
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = [false, true]
            .into_iter()
            .enumerate()
            .map(|(worker, skip)| {
                let runtime = Arc::clone(&runtime);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    set_current_connection_id(2_292_000 + worker as u64);
                    barrier.wait();
                    let suffix = if skip { " ON CONFLICT DO NOTHING" } else { "" };
                    let result = runtime.execute_query(&format!(
                    "INSERT INTO unique_probe (id,body) VALUES ({key},'writer{worker}'){suffix}"
                ));
                    clear_current_connection_id();
                    result.map(|result| result.affected_rows)
                })
            })
            .collect();
        let results: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().expect("writer finishes"))
            .collect();
        assert_eq!(
            results
                .iter()
                .filter_map(|result| result.as_ref().ok())
                .sum::<u64>(),
            1
        );
        let rows = runtime
            .execute_query(&format!("SELECT * FROM unique_probe WHERE id={key}"))
            .expect("indexed readback")
            .result
            .records;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            runtime
                .execute_query("SELECT * FROM unique_probe")
                .expect("full readback")
                .result
                .records
                .len(),
            key as usize
        );
    }
}

#[test]
fn standalone_unique_target_coexists_with_table_constraints() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("CREATE TABLE accounts (id INT PRIMARY KEY, email TEXT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO accounts (id,email) VALUES (1,'one')")
        .expect("seed");
    runtime
        .execute_query("CREATE UNIQUE INDEX account_email ON accounts (email) USING HASH")
        .expect("index");
    assert_eq!(
        runtime
            .execute_query(
                "INSERT INTO accounts (id,email) VALUES (2,'one') ON CONFLICT (email) DO NOTHING"
            )
            .expect("standalone target")
            .affected_rows,
        0
    );
    assert!(runtime
        .execute_query(
            "INSERT INTO accounts (id,email) VALUES (1,'two') ON CONFLICT (email) DO NOTHING"
        )
        .is_err());
    assert_eq!(runtime.execute_query("INSERT INTO accounts (id,email) VALUES (2,'two'),(3,'two') ON CONFLICT (email) DO NOTHING")
        .expect("standalone batch target").affected_rows, 1);
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM accounts")
            .expect("readback")
            .result
            .records
            .len(),
        2
    );
}

#[test]
fn standalone_unique_columnar_admission_rejects_before_bulk_install() {
    use reddb::application::RuntimeEntityPort;
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    seed(&runtime);
    let columns = Arc::new(vec!["id".to_string(), "body".to_string()]);
    let rows = vec![
        vec![
            reddb_types::Value::Integer(2),
            reddb_types::Value::text("new"),
        ],
        vec![
            reddb_types::Value::Integer(1),
            reddb_types::Value::text("duplicate"),
        ],
    ];
    assert!(runtime
        .create_rows_batch_columnar("unique_probe".to_string(), columns, rows)
        .is_err());
    assert_original_row(&runtime);
}

#[test]
fn standalone_unique_admission_uses_physical_key_encoding() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    seed(&runtime);
    assert_eq!(
        runtime
            .execute_query_with_params(
                "INSERT INTO unique_probe (id,body) VALUES ($1,'skipped') ON CONFLICT DO NOTHING",
                &[reddb_types::Value::UnsignedInteger(1)],
            )
            .expect("signed and unsigned share the existing HASH encoding")
            .affected_rows,
        0
    );
    assert_original_row(&runtime);
    // A NULL key never equals another NULL (SQL), so it is never reserved.
    runtime
        .execute_query("INSERT INTO unique_probe (id,body) VALUES (NULL,'null')")
        .expect("first null key");
    assert_eq!(
        runtime
            .execute_query(
                "INSERT INTO unique_probe (id,body) VALUES (NULL,'second') ON CONFLICT DO NOTHING"
            )
            .expect("a NULL key is not a duplicate")
            .affected_rows,
        1
    );
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM unique_probe")
            .expect("readback")
            .result
            .records
            .len(),
        3
    );
}

#[test]
fn standalone_unique_reclaims_aborted_index_entries() {
    set_current_connection_id(2_292_010);
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    seed(&runtime);
    runtime.execute_query("BEGIN").expect("begin");
    runtime
        .execute_query("INSERT INTO unique_probe (id,body) VALUES (2,'aborted')")
        .expect("transaction write");
    runtime.execute_query("ROLLBACK").expect("rollback");
    runtime
        .execute_query("INSERT INTO unique_probe (id,body) VALUES (2,'replacement')")
        .expect("aborted key released");
    let rows = runtime
        .execute_query("SELECT * FROM unique_probe WHERE id=2")
        .expect("replacement lookup")
        .result
        .records;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("body"),
        Some(&reddb_types::Value::text("replacement"))
    );
    clear_current_connection_id();
}

#[test]
fn standalone_unique_rejects_another_active_writer_without_partial_install() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    seed(&runtime);
    set_current_connection_id(2_292_020);
    runtime.execute_query("BEGIN").expect("begin writer");
    runtime
        .execute_query("INSERT INTO unique_probe (id,body) VALUES (2,'pending')")
        .expect("pending row");
    set_current_connection_id(2_292_021);
    let error = runtime
        .execute_query(
            "INSERT INTO unique_probe (id,body) VALUES (2,'competitor') ON CONFLICT DO NOTHING",
        )
        .expect_err("active writer reserves its key");
    assert!(error.to_string().contains("serialization conflict"));
    set_current_connection_id(2_292_020);
    runtime
        .execute_query("ROLLBACK")
        .expect("release pending row");
    set_current_connection_id(2_292_021);
    assert_original_row(&runtime);
    runtime
        .execute_query("INSERT INTO unique_probe (id,body) VALUES (2,'replacement')")
        .expect("key is reusable");
    clear_current_connection_id();
}

#[test]
fn standalone_unique_do_update_targets_existing_row() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    seed(&runtime);
    assert_eq!(runtime.execute_query("INSERT INTO unique_probe (id,body) VALUES (1,'updated') ON CONFLICT (id) DO UPDATE SET body=EXCLUDED.body")
        .expect("resolve standalone conflict to existing row").affected_rows, 1);
    let rows = runtime
        .execute_query("SELECT * FROM unique_probe")
        .expect("readback")
        .result
        .records;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("body"),
        Some(&reddb_types::Value::text("updated"))
    );
}

fn composite_runtime() -> RedDBRuntime {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('x','1')")
        .expect("seed implicit collection");
    runtime
        .execute_query("CREATE UNIQUE INDEX pairs_ab ON pairs (a, b) USING HASH")
        .expect("composite unique hash index");
    runtime
}

#[test]
fn composite_unique_hash_keys_on_every_column() {
    let runtime = composite_runtime();
    // Same first column, different second column: not a duplicate. The index
    // used to key on the first column only and rejected (x,'2').
    runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('x','2')")
        .expect("(x,2) differs from (x,1)");
    runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('y','1')")
        .expect("(y,1) differs from (x,1)");
    // The same tuple is still a duplicate, across statements and within one.
    assert!(runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('x','1')")
        .is_err());
    assert!(runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('z','9'),('z','9')")
        .is_err());
    let rows = runtime
        .execute_query("SELECT * FROM pairs")
        .expect("read")
        .result
        .records
        .len();
    assert_eq!(rows, 3, "rejected writes must leave no rows");
}

#[test]
fn composite_unique_hash_never_conflicts_on_null() {
    let runtime = composite_runtime();
    for _ in 0..2 {
        runtime
            .execute_query("INSERT INTO pairs (a,b) VALUES ('n', NULL)")
            .expect("a NULL column never equals another NULL");
    }
}

#[test]
fn composite_unique_hash_follows_updates_and_deletes() {
    let runtime = composite_runtime();
    runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('x','2')")
        .expect("(x,2)");
    runtime
        .execute_query("UPDATE pairs SET b = '3' WHERE a = 'x' AND b = '2'")
        .expect("move (x,2) to (x,3)");
    runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('x','2')")
        .expect("the update released the old tuple");
    assert!(runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('x','3')")
        .is_err());
    runtime
        .execute_query("DELETE FROM pairs WHERE a = 'x' AND b = '3'")
        .expect("delete (x,3)");
    runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('x','3')")
        .expect("the delete released the tuple");
}

#[test]
fn creating_a_composite_unique_hash_index_checks_existing_tuples() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    for row in ["('x','1')", "('x','2')", "('x','1')"] {
        runtime
            .execute_query(&format!("INSERT INTO pairs (a,b) VALUES {row}"))
            .expect("seed");
    }
    assert!(
        runtime
            .execute_query("CREATE UNIQUE INDEX pairs_ab ON pairs (a, b) USING HASH")
            .is_err(),
        "(x,1) exists twice"
    );

    // Distinct tuples that share a first column are accepted.
    let ok = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    for row in ["('x','1')", "('x','2')"] {
        ok.execute_query(&format!("INSERT INTO pairs (a,b) VALUES {row}"))
            .expect("seed");
    }
    ok.execute_query("CREATE UNIQUE INDEX pairs_ab ON pairs (a, b) USING HASH")
        .expect("(x,1) and (x,2) are distinct tuples");
}

// ---- every index method, BTREE (the default) included -----------------------

const METHODS: [&str; 3] = ["", " USING BTREE", " USING HASH"];

fn count(runtime: &RedDBRuntime, sql: &str) -> usize {
    runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .result
        .records
        .len()
}

fn unique_runtime(using: &str) -> RedDBRuntime {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("INSERT INTO uq (id,body) VALUES (1,'existing')")
        .expect("seed implicit collection");
    runtime
        .execute_query(&format!("CREATE UNIQUE INDEX uq_id ON uq (id){using}"))
        .expect("unique index");
    runtime
}

#[test]
fn unique_index_rejects_duplicate_inserts_for_every_method() {
    for using in METHODS {
        let runtime = unique_runtime(using);
        for insert in [
            "INSERT INTO uq (id,body) VALUES (1,'dup')",
            "INSERT INTO uq (id,body) VALUES (2,'new'),(1,'dup')",
            "INSERT INTO uq (id,body) VALUES (3,'a'),(3,'b')",
        ] {
            assert!(
                runtime.execute_query(insert).is_err(),
                "[{using}] must reject {insert}"
            );
        }
        assert_eq!(count(&runtime, "SELECT * FROM uq"), 1, "[{using}]");
        runtime
            .execute_query("INSERT INTO uq (id,body) VALUES (2,'ok')")
            .expect("a free key is still accepted");
        assert_eq!(
            runtime
                .execute_query("INSERT INTO uq (id,body) VALUES (2,'skip') ON CONFLICT DO NOTHING")
                .expect("do nothing")
                .affected_rows,
            0,
            "[{using}]"
        );
    }
}

#[test]
fn unique_index_treats_nulls_as_distinct_for_every_method() {
    for using in METHODS {
        let runtime = unique_runtime(using);
        for body in ["n1", "n2"] {
            runtime
                .execute_query(&format!("INSERT INTO uq (id,body) VALUES (NULL,'{body}')"))
                .unwrap_or_else(|error| panic!("[{using}] NULL is distinct: {error}"));
        }
        runtime
            .execute_query("INSERT INTO uq (id,body) VALUES (NULL,'n3'),(NULL,'n4')")
            .unwrap_or_else(|error| panic!("[{using}] NULL batch: {error}"));
        runtime
            .execute_query("UPDATE uq SET id = NULL WHERE id = 1")
            .unwrap_or_else(|error| panic!("[{using}] update to NULL: {error}"));
        assert_eq!(count(&runtime, "SELECT * FROM uq"), 5, "[{using}]");
    }
}

#[test]
fn unique_index_rejects_an_update_onto_an_existing_key() {
    for using in METHODS {
        let runtime = unique_runtime(using);
        runtime
            .execute_query("INSERT INTO uq (id,body) VALUES (2,'second')")
            .expect("second row");
        assert!(
            runtime
                .execute_query("UPDATE uq SET id = 1 WHERE id = 2")
                .is_err(),
            "[{using}] id 1 is taken"
        );
        assert_eq!(count(&runtime, "SELECT * FROM uq WHERE id = 1"), 1, "[{using}]");
        assert_eq!(count(&runtime, "SELECT * FROM uq WHERE id = 2"), 1, "[{using}]");
        assert_eq!(count(&runtime, "SELECT * FROM uq"), 2, "[{using}]");

        // Updates that keep the key stay legal, whether or not they name it.
        runtime
            .execute_query("UPDATE uq SET body = 'changed' WHERE id = 2")
            .expect("update a non-key column");
        runtime
            .execute_query("UPDATE uq SET id = 2 WHERE id = 2")
            .expect("rewriting the own key is not a conflict");

        // Moving to a free key releases the old one.
        runtime
            .execute_query("UPDATE uq SET id = 5 WHERE id = 2")
            .expect("move to a free key");
        runtime
            .execute_query("INSERT INTO uq (id,body) VALUES (2,'reuse')")
            .expect("the update released key 2");
        assert!(
            runtime
                .execute_query("INSERT INTO uq (id,body) VALUES (5,'dup')")
                .is_err(),
            "[{using}] the update claimed key 5"
        );
    }
}

#[test]
fn unique_index_rejects_a_statement_that_collides_with_itself() {
    for using in METHODS {
        let runtime = unique_runtime(using);
        runtime
            .execute_query("INSERT INTO uq (id,body) VALUES (2,'second')")
            .expect("second row");
        assert!(
            runtime.execute_query("UPDATE uq SET id = 9").is_err(),
            "[{using}] two rows cannot both become 9"
        );
        assert_eq!(count(&runtime, "SELECT * FROM uq WHERE id = 9"), 0, "[{using}]");
        assert_eq!(count(&runtime, "SELECT * FROM uq"), 2, "[{using}]");
    }
}

#[test]
fn unique_index_follows_deletes_for_every_method() {
    for using in METHODS {
        let runtime = unique_runtime(using);
        runtime
            .execute_query("DELETE FROM uq WHERE id = 1")
            .expect("delete");
        runtime
            .execute_query("INSERT INTO uq (id,body) VALUES (1,'again')")
            .unwrap_or_else(|error| panic!("[{using}] the delete released the key: {error}"));
    }
}

fn composite_unique_runtime(using: &str) -> RedDBRuntime {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("INSERT INTO pairs (a,b) VALUES ('x','1')")
        .expect("seed implicit collection");
    runtime
        .execute_query(&format!("CREATE UNIQUE INDEX pairs_ab ON pairs (a, b){using}"))
        .expect("composite unique index");
    runtime
}

#[test]
fn composite_unique_index_keys_on_every_column_for_every_method() {
    for using in ["", " USING BTREE", " USING HASH"] {
        let runtime = composite_unique_runtime(using);
        runtime
            .execute_query("INSERT INTO pairs (a,b) VALUES ('x','2')")
            .unwrap_or_else(|error| panic!("[{using}] (x,2) differs from (x,1): {error}"));
        runtime
            .execute_query("INSERT INTO pairs (a,b) VALUES ('y','1')")
            .unwrap_or_else(|error| panic!("[{using}] (y,1) differs from (x,1): {error}"));
        assert!(
            runtime
                .execute_query("INSERT INTO pairs (a,b) VALUES ('x','1')")
                .is_err(),
            "[{using}] (x,1) twice"
        );
        assert!(
            runtime
                .execute_query("INSERT INTO pairs (a,b) VALUES ('z','9'),('z','9')")
                .is_err(),
            "[{using}] (z,9) twice in one statement"
        );
        for _ in 0..2 {
            runtime
                .execute_query("INSERT INTO pairs (a,b) VALUES ('n', NULL)")
                .unwrap_or_else(|error| panic!("[{using}] NULL never conflicts: {error}"));
        }
        assert_eq!(count(&runtime, "SELECT * FROM pairs"), 5, "[{using}]");
    }
}

#[test]
fn composite_unique_index_follows_updates_and_deletes_for_every_method() {
    for using in ["", " USING BTREE", " USING HASH"] {
        let runtime = composite_unique_runtime(using);
        runtime
            .execute_query("INSERT INTO pairs (a,b) VALUES ('x','2')")
            .expect("(x,2)");
        assert!(
            runtime
                .execute_query("UPDATE pairs SET b = '1' WHERE a = 'x' AND b = '2'")
                .is_err(),
            "[{using}] (x,1) is taken"
        );
        runtime
            .execute_query("UPDATE pairs SET b = '3' WHERE a = 'x' AND b = '2'")
            .expect("move (x,2) to (x,3)");
        runtime
            .execute_query("INSERT INTO pairs (a,b) VALUES ('x','2')")
            .unwrap_or_else(|error| panic!("[{using}] the update released (x,2): {error}"));
        assert!(
            runtime
                .execute_query("INSERT INTO pairs (a,b) VALUES ('x','3')")
                .is_err(),
            "[{using}] the update claimed (x,3)"
        );
        runtime
            .execute_query("DELETE FROM pairs WHERE a = 'x' AND b = '3'")
            .expect("delete (x,3)");
        runtime
            .execute_query("INSERT INTO pairs (a,b) VALUES ('x','3')")
            .unwrap_or_else(|error| panic!("[{using}] the delete released (x,3): {error}"));
    }
}

#[test]
fn creating_a_unique_index_checks_existing_rows_for_every_method() {
    for (using, columns) in [
        ("", "id"),
        (" USING BTREE", "id"),
        (" USING HASH", "id"),
        ("", "id, grp"),
        (" USING BTREE", "id, grp"),
        (" USING HASH", "id, grp"),
    ] {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        for row in ["(1,'g','a')", "(2,'g','b')", "(1,'g','c')"] {
            runtime
                .execute_query(&format!("INSERT INTO dups (id,grp,body) VALUES {row}"))
                .expect("seed");
        }
        assert!(
            runtime
                .execute_query(&format!("CREATE UNIQUE INDEX dups_key ON dups ({columns}){using}"))
                .is_err(),
            "[{using}] ({columns}) 1 exists twice"
        );
        // A refused CREATE leaves nothing behind: the duplicate is still accepted.
        runtime
            .execute_query("INSERT INTO dups (id,grp,body) VALUES (1,'g','d')")
            .unwrap_or_else(|error| panic!("[{using}] ({columns}) no index was left: {error}"));
    }
}

#[test]
fn creating_a_unique_index_ignores_dead_row_versions_for_every_method() {
    for using in METHODS {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        runtime
            .execute_query("INSERT INTO versions (id,body) VALUES (1,'v1')")
            .expect("seed");
        for body in ["v2", "v3"] {
            runtime
                .execute_query(&format!("UPDATE versions SET body = '{body}' WHERE id = 1"))
                .expect("update leaves a dead version behind");
        }
        runtime
            .execute_query("DELETE FROM versions WHERE id = 1")
            .expect("delete");
        runtime
            .execute_query("INSERT INTO versions (id,body) VALUES (1,'live')")
            .expect("a live row on the same key");
        runtime
            .execute_query(&format!("CREATE UNIQUE INDEX versions_id ON versions (id){using}"))
            .unwrap_or_else(|error| panic!("[{using}] dead versions are not duplicates: {error}"));
        assert!(runtime
            .execute_query("INSERT INTO versions (id,body) VALUES (1,'dup')")
            .is_err());
    }
}

#[test]
fn unique_index_is_dropped_and_recreated_cleanly() {
    for using in METHODS {
        let runtime = unique_runtime(using);
        runtime
            .execute_query("DROP INDEX uq_id ON uq")
            .expect("drop");
        runtime
            .execute_query("INSERT INTO uq (id,body) VALUES (1,'dup')")
            .unwrap_or_else(|error| panic!("[{using}] a dropped index enforces nothing: {error}"));
        runtime
            .execute_query("DELETE FROM uq WHERE body = 'dup'")
            .expect("remove the duplicate again");
        runtime
            .execute_query(&format!("CREATE UNIQUE INDEX uq_id ON uq (id){using}"))
            .unwrap_or_else(|error| panic!("[{using}] recreate under the same name: {error}"));
        assert!(
            runtime
                .execute_query("INSERT INTO uq (id,body) VALUES (1,'dup')")
                .is_err(),
            "[{using}] the recreated index enforces again"
        );
    }
}

#[test]
fn unique_index_survives_reopen_for_every_method() {
    for (using, columns) in [
        ("", "id"),
        (" USING BTREE", "id"),
        ("", "id, grp"),
        (" USING BTREE", "id, grp"),
    ] {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("unique.rdb");
        {
            let runtime =
                RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
            runtime
                .execute_query("INSERT INTO keep (id,grp,body) VALUES (1,'g','existing')")
                .expect("seed");
            runtime
                .execute_query(&format!("CREATE UNIQUE INDEX keep_key ON keep ({columns}){using}"))
                .expect("index");
        }
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
        assert!(
            runtime
                .execute_query("INSERT INTO keep (id,grp,body) VALUES (1,'g','dup')")
                .is_err(),
            "[{using}] ({columns}) the rebuilt index still enforces"
        );
        runtime
            .execute_query("INSERT INTO keep (id,grp,body) VALUES (2,'g','new')")
            .expect("a free key is accepted after reopen");
        assert_eq!(count(&runtime, "SELECT * FROM keep"), 2);
    }
}

#[test]
fn unique_btree_serializes_concurrent_inserts() {
    let runtime = Arc::new(unique_runtime(" USING BTREE"));
    for key in 2..18 {
        let barrier = Arc::new(Barrier::new(2));
        let workers: Vec<_> = (0..2)
            .map(|worker| {
                let runtime = Arc::clone(&runtime);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    set_current_connection_id(2_374_000 + worker as u64);
                    barrier.wait();
                    let result = runtime.execute_query(&format!(
                        "INSERT INTO uq (id,body) VALUES ({key},'writer{worker}')"
                    ));
                    clear_current_connection_id();
                    result.is_ok()
                })
            })
            .collect();
        let winners = workers
            .into_iter()
            .map(|worker| worker.join().expect("writer finishes"))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1, "exactly one writer owns key {key}");
        assert_eq!(count(&runtime, &format!("SELECT * FROM uq WHERE id = {key}")), 1);
    }
}
