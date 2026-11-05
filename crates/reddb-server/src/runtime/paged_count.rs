//! Exact scalar COUNT over generation-bound visibility projections.

impl super::RedDBRuntime {
    pub(super) fn prepare_paged_counts(&self) -> crate::RedDBResult<()> {
        let store = self.db().store();
        if store.config().resident_memory_bytes_max.is_none() {
            return Ok(());
        }
        for collection in store.list_collections() {
            store.register_count_index(&collection)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::storage::{EntityId, StoreError};
    use crate::{RedDBOptions, RedDBRuntime};
    use reddb_engine::storage::collection_read::{CollectionCursor, CollectionRead, EncodedRecord};
    use reddb_types::Value;
    use std::sync::Arc;

    struct NoPayloadReads;
    impl CollectionRead for NoPayloadReads {
        fn get_record(&self, _: EntityId, _: usize) -> Result<Option<EncodedRecord>, StoreError> {
            Err(StoreError::Serialization(
                "unexpected COUNT payload read".into(),
            ))
        }
        fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
            Err(StoreError::Serialization(
                "unexpected COUNT payload scan".into(),
            ))
        }
        fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
            Err(StoreError::Serialization(
                "unexpected COUNT payload scan".into(),
            ))
        }
    }

    fn fixture() -> (tempfile::TempDir, RedDBRuntime, RedDBOptions) {
        let directory = tempfile::tempdir().expect("COUNT fixture");
        let options = RedDBOptions::persistent(directory.path().join("count.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let seed = RedDBRuntime::with_options(options.clone()).expect("seed");
        seed.execute_query("CREATE TABLE count_rows (id INTEGER, payload TEXT)")
            .expect("table");
        for id in 0..12 {
            seed.execute_query_with_params(
                "INSERT INTO count_rows (id, payload) VALUES ($1, $2)",
                &[
                    Value::Integer(id),
                    Value::text("large payload".repeat(2048)),
                ],
            )
            .expect("row");
        }
        seed.flush().expect("seed checkpoint");
        drop(seed);
        let runtime = open(&options);
        (directory, runtime, options)
    }

    fn open(options: &RedDBOptions) -> RedDBRuntime {
        crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options.clone())
        })
        .expect("cold runtime")
    }

    fn deny_payload_reads(runtime: &RedDBRuntime) {
        let store = runtime.db().store();
        let manager = store.get_collection("count_rows").expect("collection");
        let root = manager.backing_root_page().expect("prepared root");
        let count = manager.backing_count_at_root(root).expect("physical count");
        manager.install_backing(
            Arc::new(NoPayloadReads),
            root,
            store.format_version(),
            1024,
            count,
        );
    }

    fn count(runtime: &RedDBRuntime) -> u64 {
        let result = runtime
            .execute_query("SELECT COUNT(*) AS n FROM count_rows")
            .expect("COUNT");
        match result.result.records[0].get("n").expect("n") {
            Value::UnsignedInteger(n) => *n,
            Value::Integer(n) => *n as u64,
            value => panic!("unexpected count {value:?}"),
        }
    }

    #[test]
    fn paged_count_reopen_reads_no_row_payloads() {
        let (_directory, runtime, options) = fixture();
        assert_eq!(count(&runtime), 12);
        drop(runtime);
        let runtime = open(&options);
        deny_payload_reads(&runtime);
        assert_eq!(count(&runtime), 12);
    }

    #[test]
    fn paged_count_overlay_checkpoint_and_rollback_preserve_exact_count() {
        let (_directory, runtime, options) = fixture();
        for sql in [
            "INSERT INTO count_rows (id, payload) VALUES (12, 'new')",
            "UPDATE count_rows SET payload = 'changed' WHERE id = 0",
            "DELETE FROM count_rows WHERE id = 1",
        ] {
            runtime.execute_query(sql).expect(sql);
        }
        assert_eq!(count(&runtime), 12);
        runtime.execute_query("BEGIN").expect("begin");
        runtime
            .execute_query("DELETE FROM count_rows WHERE id = 2")
            .expect("own delete");
        assert_eq!(count(&runtime), 11);
        runtime
            .execute_query("SAVEPOINT nested")
            .expect("savepoint");
        runtime
            .execute_query("INSERT INTO count_rows (id, payload) VALUES (13, 'nested')")
            .expect("own insert");
        assert_eq!(count(&runtime), 12);
        runtime
            .execute_query("ROLLBACK TO SAVEPOINT nested")
            .expect("rollback savepoint");
        assert_eq!(count(&runtime), 11);
        runtime.execute_query("ROLLBACK").expect("rollback");
        assert_eq!(count(&runtime), 12);
        runtime.flush().expect("checkpoint");
        drop(runtime);
        let runtime = open(&options);
        deny_payload_reads(&runtime);
        assert_eq!(count(&runtime), 12);
    }

    #[test]
    fn paged_count_filtered_and_column_counts_keep_payload_scan() {
        let (_directory, runtime, _options) = fixture();
        deny_payload_reads(&runtime);
        for sql in [
            "SELECT COUNT(*) FROM count_rows WHERE id = 0",
            "SELECT COUNT(payload) FROM count_rows",
            "SELECT id, COUNT(*) FROM count_rows GROUP BY id",
        ] {
            let error = runtime
                .execute_query(sql)
                .expect_err("payload-dependent scan must fail");
            assert!(
                error.to_string().contains("unexpected COUNT payload"),
                "{sql}: {error}"
            );
        }
        for sql in [
            "SELECT COUNT(*) AS n FROM count_rows LIMIT 0",
            "SELECT COUNT(*) AS n FROM count_rows OFFSET 1",
        ] {
            assert!(runtime
                .execute_query(sql)
                .expect(sql)
                .result
                .records
                .is_empty());
        }
    }

    #[test]
    fn paged_count_concurrent_readers_writes_and_checkpoints_finish() {
        let (_directory, runtime, _options) = fixture();
        let (send, receive) = std::sync::mpsc::channel();
        let start = Arc::new(std::sync::Barrier::new(5));
        let mut workers = Vec::new();
        for worker in 0..5 {
            let runtime = runtime.clone();
            let send = send.clone();
            let start = Arc::clone(&start);
            workers.push(std::thread::spawn(move || {
                start.wait();
                for round in 0..16 {
                    if worker == 0 {
                        runtime
                            .execute_query(&format!(
                                "INSERT INTO count_rows (id, payload) VALUES ({}, 'concurrent')",
                                100 + round
                            ))
                            .expect("insert");
                        runtime.flush().expect("checkpoint live row");
                        runtime
                            .execute_query(&format!(
                                "DELETE FROM count_rows WHERE id = {}",
                                100 + round
                            ))
                            .expect("delete");
                    } else {
                        let observed = count(&runtime);
                        assert!((12..=13).contains(&observed), "snapshot count {observed}");
                    }
                }
                send.send(()).expect("completion");
            }));
        }
        for _ in 0..5 {
            receive
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("no publication lock inversion");
        }
        for worker in workers {
            worker.join().expect("worker");
        }
        assert_eq!(count(&runtime), 12);
    }
}
