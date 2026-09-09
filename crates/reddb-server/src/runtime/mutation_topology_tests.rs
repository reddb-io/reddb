use super::*;
use std::sync::mpsc;
use std::time::Duration;

fn row(id: i64) -> MutationRow {
    MutationRow {
        fields: vec![("record_key".to_string(), Value::Integer(id))],
        metadata: Vec::new(),
        node_links: Vec::new(),
        vector_links: Vec::new(),
    }
}

#[test]
fn admitted_rows_keep_their_index_topology_until_maintenance_finishes() {
    for batch_size in [1, 3] {
        for operation in [
            "create",
            "drop",
            "rebuild",
            "tenant",
            "tenant_drop",
            "rehydrate",
            "drop_table",
        ] {
            let runtime = crate::RedDBRuntime::in_memory().expect("runtime");
            runtime
                .execute_query("CREATE TABLE topology_records (record_key INT, tenant_key INT)")
                .expect("table");
            if operation != "create" {
                runtime
                    .execute_query(
                        "CREATE INDEX record_lookup ON topology_records (record_key) USING HASH",
                    )
                    .expect("existing index");
            }
            if operation == "tenant_drop" {
                runtime.register_tenant_table("topology_records", "tenant_key");
            }
            if operation == "rehydrate" {
                // Retain the persisted descriptor, as at startup before replay.
                runtime
                    .index_store_ref()
                    .drop_index("record_lookup", "topology_records");
            }
            let (admitted_send, admitted_receive) = mpsc::channel();
            let (release_send, release_receive) = mpsc::channel();
            let release_receive = std::sync::Mutex::new(release_receive);
            let hook = || {
                admitted_send.send(()).expect("announce admission");
                release_receive
                    .lock()
                    .expect("release receiver")
                    .recv_timeout(Duration::from_secs(10))
                    .expect("release admitted writer");
            };
            let completed_early = std::thread::scope(|scope| {
                let writer = scope.spawn(|| {
                    let mut engine = MutationEngine::new(&runtime).with_suppress_events();
                    engine.after_admission = Some(&hook);
                    engine.apply(
                        "topology_records".to_string(),
                        (0..batch_size).map(row).collect(),
                    )
                });
                admitted_receive
                    .recv_timeout(Duration::from_secs(10))
                    .expect("writer admitted");
                let (started_send, started_receive) = mpsc::channel();
                let (done_send, done_receive) = mpsc::channel();
                let runtime = &runtime;
                let ddl = scope.spawn(move || {
                    started_send.send(()).expect("DDL started");
                    let result = match operation {
                        "create" => runtime.execute_query(
                            "CREATE INDEX record_lookup ON topology_records (record_key) USING HASH"
                        ).map(|_| ()),
                        "drop" => runtime.execute_query(
                            "DROP INDEX record_lookup ON topology_records"
                        ).map(|_| ()),
                        "rebuild" => runtime.rebuild_runtime_indexes_for_table("topology_records"),
                        "tenant" => {
                            runtime.register_tenant_table("topology_records", "tenant_key");
                            Ok(())
                        }
                        "tenant_drop" => {
                            runtime.unregister_tenant_table("topology_records");
                            Ok(())
                        }
                        "rehydrate" => runtime.rehydrate_runtime_index_registry(),
                        "drop_table" => runtime.execute_query("DROP TABLE topology_records").map(|_| ()),
                        _ => unreachable!(),
                    };
                    done_send.send(()).expect("DDL completed");
                    result
                });
                started_receive
                    .recv_timeout(Duration::from_secs(10))
                    .expect("DDL attempted");
                let completed_early = done_receive
                    .recv_timeout(Duration::from_millis(100))
                    .is_ok();
                release_send.send(()).expect("resume writer");
                writer
                    .join()
                    .expect("writer thread")
                    .expect("insert succeeds");
                ddl.join().expect("DDL thread").expect("DDL succeeds");
                completed_early
            });
            assert!(!completed_early,
                "{operation} changed the admitted index set before a {batch_size}-row write finished");
            if matches!(operation, "create" | "rebuild" | "rehydrate") {
                for id in 0..batch_size {
                    assert_eq!(
                        runtime
                            .index_store_ref()
                            .hash_lookup("topology_records", "record_lookup", &id.to_le_bytes())
                            .expect("lookup")
                            .len(),
                        1,
                        "backfill includes every admitted row once"
                    );
                }
            }
        }
    }
}

#[test]
fn index_backfill_keeps_rows_stable_until_registration() {
    for batch_size in [1, 3] {
        let runtime = crate::RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE topology_records (record_key INT)")
            .expect("table");
        let (snapshot_send, snapshot_receive) = mpsc::channel();
        let (release_send, release_receive) = mpsc::channel();
        let release_receive = std::sync::Mutex::new(release_receive);
        *runtime.index_store_ref().before_build.lock() = Some(Arc::new(move || {
            snapshot_send.send(()).expect("snapshot collected");
            release_receive
                .lock()
                .expect("release receiver")
                .recv_timeout(Duration::from_secs(10))
                .expect("release index builder");
        }));
        let published_early = std::thread::scope(|scope| {
            let ddl = scope.spawn(|| {
                runtime.execute_query(
                    "CREATE INDEX record_lookup ON topology_records (record_key) USING HASH",
                )
            });
            snapshot_receive
                .recv_timeout(Duration::from_secs(10))
                .expect("snapshot taken");
            let (started_send, started_receive) = mpsc::channel();
            let (done_send, done_receive) = mpsc::channel();
            let runtime = &runtime;
            let writer = scope.spawn(move || {
                started_send.send(()).expect("writer started");
                let result = MutationEngine::new(runtime).with_suppress_events().apply(
                    "topology_records".to_string(),
                    (0..batch_size).map(row).collect(),
                );
                done_send.send(()).expect("writer completed");
                result
            });
            started_receive
                .recv_timeout(Duration::from_secs(10))
                .expect("writer attempted");
            let published_early = done_receive
                .recv_timeout(Duration::from_millis(100))
                .is_ok();
            release_send.send(()).expect("resume backfill");
            ddl.join().expect("DDL thread").expect("create index");
            writer.join().expect("writer thread").expect("insert");
            published_early
        });
        // This content check also detects the old missed-backfill bug if the
        // scheduler did not run the writer during the bounded blocking probe.
        for id in 0..batch_size {
            assert_eq!(
                runtime
                    .index_store_ref()
                    .hash_lookup("topology_records", "record_lookup", &id.to_le_bytes())
                    .expect("lookup")
                    .len(),
                1,
                "no row may escape snapshot and maintenance"
            );
        }
        assert!(
            !published_early,
            "row publication waits for the index snapshot to be installed"
        );
    }
}

#[test]
fn admitted_writer_allows_other_row_writers_and_unrelated_ddl() {
    let runtime = crate::RedDBRuntime::in_memory().expect("runtime");
    runtime
        .execute_query("CREATE TABLE topology_records (record_key INT)")
        .expect("table");
    runtime
        .execute_query("CREATE TABLE other_records (record_key INT)")
        .expect("other table");
    let (admitted_send, admitted_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let release_receive = std::sync::Mutex::new(release_receive);
    let hook = || {
        admitted_send.send(()).expect("admitted");
        release_receive
            .lock()
            .expect("release receiver")
            .recv_timeout(Duration::from_secs(10))
            .expect("resume writer");
    };
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            let mut engine = MutationEngine::new(&runtime).with_suppress_events();
            engine.after_admission = Some(&hook);
            engine.apply("topology_records".to_string(), vec![row(1)])
        });
        admitted_receive
            .recv_timeout(Duration::from_secs(10))
            .expect("writer paused");
        // Both operations must complete while the first writer remains paused.
        MutationEngine::new(&runtime)
            .with_suppress_events()
            .apply("topology_records".to_string(), vec![row(2)])
            .expect("parallel row writer");
        runtime
            .execute_query("CREATE INDEX other_lookup ON other_records (record_key) USING HASH")
            .expect("independent collection DDL");
        release_send.send(()).expect("release first writer");
        first.join().expect("first thread").expect("first insert");
    });
    assert_eq!(
        runtime
            .db()
            .store()
            .get_collection("topology_records")
            .expect("collection")
            .query_all(|_| true)
            .len(),
        2
    );
}

#[test]
fn concurrent_first_id_inserts_share_one_complete_automatic_index() {
    for batch_size in [1, 3] {
        let runtime = crate::RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE topology_records (id INT)")
            .expect("table");
        let start = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for worker in 0..8 {
                let runtime = &runtime;
                let start = &start;
                scope.spawn(move || {
                    start.wait();
                    let rows = (0..batch_size)
                        .map(|offset| {
                            let mut row = row(worker * batch_size + offset);
                            row.fields[0].0 = "id".to_string();
                            row
                        })
                        .collect();
                    MutationEngine::new(runtime)
                        .with_suppress_events()
                        .apply("topology_records".to_string(), rows)
                        .expect("concurrent first insert");
                });
            }
        });
        assert_eq!(
            runtime
                .index_store_ref()
                .list_indices("topology_records")
                .len(),
            1
        );
        for id in 0..8 * batch_size {
            assert_eq!(
                runtime
                    .index_store_ref()
                    .hash_lookup("topology_records", "idx_id", &id.to_le_bytes())
                    .expect("automatic index")
                    .len(),
                1
            );
        }
    }
}

#[test]
fn denied_admission_releases_topology_for_ddl_and_retry() {
    let runtime = crate::RedDBRuntime::with_options(
        crate::RedDBOptions::in_memory().with_memory_budget(128 * 1024),
    )
    .expect("runtime");
    runtime
        .execute_query("CREATE TABLE topology_records (id INT)")
        .expect("table");
    runtime.refresh_memory_accounting();
    let remaining =
        runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
    let reservation = runtime
        .admit_non_evictable_growth(
            crate::storage::memory_pools::MemoryPool::SegmentArena,
            "test pressure",
            remaining,
        )
        .expect("reserve headroom");
    assert!(runtime
        .execute_query("INSERT INTO topology_records (id) VALUES (1)")
        .is_err());
    let lock = runtime
        .index_store_ref()
        .collection_topology_lock("topology_records");
    assert!(
        lock.try_write().is_some(),
        "error releases the exclusive first-insert guard"
    );
    assert!(runtime
        .index_store_ref()
        .list_indices("topology_records")
        .is_empty());
    drop(reservation);
    runtime
        .execute_query("CREATE INDEX record_lookup ON topology_records (id) USING HASH")
        .expect("DDL after rejected admission");
    runtime
        .execute_query("INSERT INTO topology_records (id) VALUES (1)")
        .expect("retry");
}

#[test]
fn newly_published_unique_index_serializes_waiting_writers_before_storage() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let runtime = crate::RedDBRuntime::in_memory().expect("runtime");
    runtime
        .execute_query("CREATE TABLE topology_records (record_key INT)")
        .expect("table");
    let (snapshot_send, snapshot_receive) = mpsc::channel();
    let (build_release_send, build_release_receive) = mpsc::channel();
    let build_release_receive = std::sync::Mutex::new(build_release_receive);
    *runtime.index_store_ref().before_build.lock() = Some(Arc::new(move || {
        snapshot_send.send(()).expect("builder paused");
        build_release_receive
            .lock()
            .expect("build receiver")
            .recv_timeout(Duration::from_secs(10))
            .expect("resume builder");
    }));
    let admitted = AtomicUsize::new(0);
    let (admitted_send, admitted_receive) = mpsc::channel();
    let release = (std::sync::Mutex::new(false), std::sync::Condvar::new());
    let hook = || {
        admitted.fetch_add(1, Ordering::SeqCst);
        admitted_send.send(()).expect("writer admitted");
        let (_release_guard, timeout) = release
            .1
            .wait_timeout_while(
                release.0.lock().expect("release flag"),
                Duration::from_secs(10),
                |released| !*released,
            )
            .expect("wait for release");
        assert!(!timeout.timed_out(), "resume admitted writer");
    };
    let successes = std::thread::scope(|scope| {
        let ddl = scope.spawn(|| {
            runtime.execute_query(
                "CREATE UNIQUE INDEX record_lookup ON topology_records (record_key) USING HASH",
            )
        });
        snapshot_receive
            .recv_timeout(Duration::from_secs(10))
            .expect("unique snapshot taken");
        let (started_send, started_receive) = mpsc::channel();
        let mut writers = Vec::new();
        for _ in 0..8 {
            let runtime = &runtime;
            let hook = &hook;
            let started_send = started_send.clone();
            writers.push(scope.spawn(move || {
                started_send.send(()).expect("writer started");
                let mut engine = MutationEngine::new(runtime).with_suppress_events();
                engine.after_admission = Some(hook);
                engine.apply("topology_records".to_string(), vec![row(42)])
            }));
        }
        for _ in 0..8 {
            started_receive
                .recv_timeout(Duration::from_secs(10))
                .expect("writers attempting");
        }
        // None may reach admission while the unique descriptor is unpublished.
        let admitted_before_publication = admitted_receive
            .recv_timeout(Duration::from_millis(100))
            .is_ok();
        build_release_send.send(()).expect("publish unique index");
        ddl.join().expect("DDL thread").expect("unique index built");
        if !admitted_before_publication {
            admitted_receive
                .recv_timeout(Duration::from_secs(10))
                .expect("first admitted writer");
        }
        let second_admitted = admitted_receive
            .recv_timeout(Duration::from_millis(100))
            .is_ok();
        *release.0.lock().expect("release flag") = true;
        release.1.notify_all();
        let successes = writers
            .into_iter()
            .map(|writer| writer.join().expect("writer thread"))
            .filter(Result::is_ok)
            .count();
        assert!(
            !admitted_before_publication,
            "writers wait for unique publication"
        );
        assert!(
            !second_admitted,
            "only one equal key reaches admission before storage"
        );
        successes
    });
    assert_eq!(successes, 1, "only one writer wins the unique key");
    assert_eq!(admitted.load(Ordering::SeqCst), 1);
    assert_eq!(
        runtime
            .db()
            .store()
            .get_collection("topology_records")
            .expect("collection")
            .query_all(|_| true)
            .len(),
        1,
        "losing writers must fail before installing rows"
    );
    assert_eq!(
        runtime
            .index_store_ref()
            .hash_lookup("topology_records", "record_lookup", &42i64.to_le_bytes())
            .expect("unique index lookup")
            .len(),
        1
    );
}
