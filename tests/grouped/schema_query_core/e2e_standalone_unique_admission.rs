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
    runtime
        .execute_query("INSERT INTO unique_probe (id,body) VALUES (NULL,'null')")
        .expect("first null key");
    assert_eq!(
        runtime
            .execute_query(
                "INSERT INTO unique_probe (id,body) VALUES (NULL,'skipped') ON CONFLICT DO NOTHING"
            )
            .expect("physical HASH indexes reserve the null key too")
            .affected_rows,
        0
    );
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM unique_probe")
            .expect("readback")
            .result
            .records
            .len(),
        2
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
