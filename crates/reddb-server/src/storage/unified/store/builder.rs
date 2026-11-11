use super::*;

impl Default for UnifiedStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_types::Value;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_store_basic() {
        let store = UnifiedStore::new();
        store.create_collection("hosts").unwrap();

        let entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::text("192.168.1.1".to_string())],
        );

        let id = store.insert("hosts", entity).unwrap();
        assert!(store.get("hosts", id).is_some());
    }

    #[test]
    fn test_store_auto_create() {
        let store = UnifiedStore::new();

        let entity =
            UnifiedEntity::vector(store.next_entity_id(), "embeddings", vec![0.1, 0.2, 0.3]);

        let id = store.insert_auto("new_collection", entity).unwrap();
        assert!(store.get("new_collection", id).is_some());
    }

    #[test]
    fn test_cross_references() {
        let store = UnifiedStore::new();

        // Create hosts collection
        let host_entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::text("192.168.1.1".to_string())],
        );
        let host_id = store.insert_auto("hosts", host_entity).unwrap();

        // Create vulns collection
        let vuln_entity = UnifiedEntity::table_row(
            store.next_entity_id(),
            "vulns",
            1,
            vec![Value::text("CVE-2024-1234".to_string())],
        );
        let vuln_id = store.insert_auto("vulns", vuln_entity).unwrap();

        // Add cross-reference
        store
            .add_cross_ref("hosts", host_id, "vulns", vuln_id, RefType::RelatedTo, 1.0)
            .unwrap();

        // Verify forward reference
        let refs = store.get_refs_from(host_id);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, vuln_id);

        // Verify reverse reference
        let back_refs = store.get_refs_to(vuln_id);
        assert_eq!(back_refs.len(), 1);
        assert_eq!(back_refs[0].0, host_id);
    }

    /// Pins issue #113: when a delete batch covers entities with no
    /// inbound *or* outbound cross-refs, `unindex_cross_refs_batch` must
    /// take the read-only fast path and skip the `reverse_refs` write
    /// lock entirely. Observable via `unindex_cross_refs_fast_path_hits`.
    #[test]
    fn unindex_cross_refs_batch_takes_fast_path_when_no_inbound_refs() {
        let store = UnifiedStore::new();

        // Insert plain rows with zero cross-refs.
        let mut ids = Vec::new();
        for i in 0..16 {
            let row = UnifiedEntity::table_row(
                store.next_entity_id(),
                "rows",
                (i + 1) as u64,
                vec![Value::text(format!("v{i}"))],
            );
            ids.push(store.insert_auto("rows", row).unwrap());
        }

        let before = store.unindex_cross_refs_fast_path_hits();
        store.delete_batch("rows", &ids).unwrap();
        let after = store.unindex_cross_refs_fast_path_hits();

        // Exactly one fast-path hit recorded for the single batch call.
        assert_eq!(
            after - before,
            1,
            "expected unindex_cross_refs_batch to take the read-only fast path"
        );

        // And of course, the rows are gone.
        for id in &ids {
            assert!(store.get("rows", *id).is_none());
        }
    }

    /// Inverse of the above: when at least one deleted entity *does*
    /// have an inbound cross-ref, the slow path must run and the
    /// fast-path counter must NOT advance.
    #[test]
    fn unindex_cross_refs_batch_uses_slow_path_when_inbound_refs_exist() {
        let store = UnifiedStore::new();

        let host = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::text("h".to_string())],
        );
        let host_id = store.insert_auto("hosts", host).unwrap();

        let vuln = UnifiedEntity::table_row(
            store.next_entity_id(),
            "vulns",
            1,
            vec![Value::text("v".to_string())],
        );
        let vuln_id = store.insert_auto("vulns", vuln).unwrap();

        store
            .add_cross_ref("hosts", host_id, "vulns", vuln_id, RefType::RelatedTo, 1.0)
            .unwrap();

        let before = store.unindex_cross_refs_fast_path_hits();
        // Deleting the *target* (vuln_id) — it has an inbound ref from host_id,
        // so reverse_refs has it as a key and the slow path is mandatory.
        store.delete_batch("vulns", &[vuln_id]).unwrap();
        let after = store.unindex_cross_refs_fast_path_hits();

        assert_eq!(
            after, before,
            "fast-path counter must not advance when inbound refs exist"
        );

        // Reverse ref is gone.
        assert!(store.get_refs_to(vuln_id).is_empty());
    }

    #[test]
    fn test_expand_refs() {
        let store = UnifiedStore::new();

        // Create a chain: A → B → C
        let _ = store.get_or_create_collection("test");

        let a = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.1]);
        let a_id = store.insert_auto("test", a).unwrap();

        let b = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.2]);
        let b_id = store.insert_auto("test", b).unwrap();

        let c = UnifiedEntity::vector(store.next_entity_id(), "v", vec![0.3]);
        let c_id = store.insert_auto("test", c).unwrap();

        store
            .add_cross_ref("test", a_id, "test", b_id, RefType::SimilarTo, 0.9)
            .unwrap();
        store
            .add_cross_ref("test", b_id, "test", c_id, RefType::SimilarTo, 0.8)
            .unwrap();

        // Expand from A with depth 2
        let expanded = store.expand_refs(a_id, 2, None);
        assert_eq!(expanded.len(), 2); // Should find B and C
    }

    #[test]
    fn test_query_all_collections() {
        let store = UnifiedStore::new();

        // Insert into multiple collections
        store
            .insert_auto(
                "hosts",
                UnifiedEntity::table_row(store.next_entity_id(), "hosts", 1, vec![]),
            )
            .unwrap();

        store
            .insert_auto(
                "vulns",
                UnifiedEntity::table_row(store.next_entity_id(), "vulns", 1, vec![]),
            )
            .unwrap();

        // Query all
        let results = store.query_all(|_| true);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_stats() {
        let store = UnifiedStore::new();

        let _ = store.get_or_create_collection("test");
        for i in 0..5 {
            store
                .insert_auto(
                    "test",
                    UnifiedEntity::vector(store.next_entity_id(), "v", vec![i as f32]),
                )
                .unwrap();
        }

        let stats = store.stats();
        assert_eq!(stats.collection_count, 1);
        assert_eq!(stats.total_entities, 5);
    }

    struct FileGuard {
        path: PathBuf,
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_path(name: &str) -> (FileGuard, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("rb_store_{}_{}.rdb", name, std::process::id()));
        let guard = FileGuard { path: path.clone() };
        let _ = std::fs::remove_file(&path);
        (guard, path)
    }

    #[test]
    fn test_cross_refs_persist_file_mode() {
        let (_guard, path) = temp_path("file");
        let store = UnifiedStore::new();

        let row = UnifiedEntity::table_row(
            store.next_entity_id(),
            "hosts",
            1,
            vec![Value::text("10.0.0.1".to_string())],
        );
        let row_id = store.insert_auto("hosts", row).unwrap();

        let node =
            UnifiedEntity::graph_node(store.next_entity_id(), "host", "asset", HashMap::new());
        let node_id = store.insert_auto("graph", node).unwrap();

        let vector =
            UnifiedEntity::vector(store.next_entity_id(), "embeddings", vec![0.1, 0.2, 0.3]);
        let vector_id = store.insert_auto("embeddings", vector).unwrap();

        store
            .add_cross_ref("hosts", row_id, "graph", node_id, RefType::RowToNode, 1.0)
            .unwrap();
        store
            .add_cross_ref(
                "graph",
                node_id,
                "embeddings",
                vector_id,
                RefType::NodeToVector,
                1.0,
            )
            .unwrap();

        store.save_to_file(&path).unwrap();

        let loaded = UnifiedStore::load_from_file(&path).unwrap();
        let refs = loaded.get_refs_from(row_id);
        assert!(refs.iter().any(|(id, kind, coll)| {
            *id == node_id && *kind == RefType::RowToNode && coll == "graph"
        }));

        let graph_refs = loaded.get_refs_from(node_id);
        assert!(graph_refs.iter().any(|(id, kind, coll)| {
            *id == vector_id && *kind == RefType::NodeToVector && coll == "embeddings"
        }));

        let expanded = loaded.expand_refs(row_id, 2, None);
        assert!(expanded
            .iter()
            .any(|(entity, depth, _)| { entity.id == node_id && *depth == 1 }));
        assert!(expanded
            .iter()
            .any(|(entity, depth, _)| { entity.id == vector_id && *depth == 2 }));
    }

    #[test]
    fn test_cross_refs_persist_paged_mode() {
        let (_guard, path) = temp_path("paged");
        let store = UnifiedStore::open(&path).unwrap();

        let row = UnifiedEntity::table_row(store.next_entity_id(), "hosts", 1, vec![]);
        let row_id = store.insert_auto("hosts", row).unwrap();

        let node =
            UnifiedEntity::graph_node(store.next_entity_id(), "host", "asset", HashMap::new());
        let node_id = store.insert_auto("graph", node).unwrap();

        store
            .add_cross_ref("hosts", row_id, "graph", node_id, RefType::RowToNode, 1.0)
            .unwrap();

        store.persist().unwrap();

        drop(store);

        let loaded = UnifiedStore::open(&path).unwrap();
        let refs = loaded.get_refs_from(row_id);
        assert!(refs.iter().any(|(id, kind, coll)| {
            *id == node_id && *kind == RefType::RowToNode && coll == "graph"
        }));
    }

    /// #2376: every DDL ends in `persist()`, which deletes and re-inserts
    /// every key of every collection B-tree. Concurrent `insert_auto` calls
    /// into the same tree (control events, index registry) raced that
    /// rebuild: duplicate-key errors on both sides and torn trees that
    /// broke every later DDL and the reopen.
    #[test]
    fn concurrent_insert_auto_and_persist_keep_paged_btree_consistent() {
        const WRITER_COUNT: usize = 4;
        const ROW_COUNT_PER_WRITER: usize = 150;
        const COLLECTION: &str = "red.control_events";

        let (_guard, path) = temp_path("concurrent_persist_2376");
        let store = Arc::new(UnifiedStore::open(&path).unwrap());
        let writers_done = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let persister = {
            let store = Arc::clone(&store);
            let writers_done = Arc::clone(&writers_done);
            std::thread::spawn(move || {
                let mut errors = Vec::new();
                while writers_done.load(std::sync::atomic::Ordering::Acquire) < WRITER_COUNT {
                    if let Err(err) = store.persist() {
                        errors.push(format!("persist: {err}"));
                    }
                }
                errors
            })
        };
        let writers: Vec<_> = (0..WRITER_COUNT)
            .map(|writer| {
                let store = Arc::clone(&store);
                let writers_done = Arc::clone(&writers_done);
                std::thread::spawn(move || {
                    let mut errors = Vec::new();
                    for i in 0..ROW_COUNT_PER_WRITER {
                        let row = UnifiedEntity::table_row(
                            EntityId::new(0),
                            COLLECTION,
                            0,
                            vec![Value::text(format!("writer{writer}-row{i}"))],
                        );
                        if let Err(err) = store.insert_auto(COLLECTION, row) {
                            errors.push(format!("insert_auto: {err}"));
                        }
                    }
                    writers_done.fetch_add(1, std::sync::atomic::Ordering::Release);
                    errors
                })
            })
            .collect();
        let mut errors: Vec<String> = writers
            .into_iter()
            .flat_map(|writer| writer.join().expect("writer thread panicked"))
            .collect();
        errors.extend(persister.join().expect("persist thread panicked"));
        assert!(
            errors.is_empty(),
            "race produced errors:\n{}",
            errors.join("\n")
        );

        store.persist().expect("final persist");
        let row_count = WRITER_COUNT * ROW_COUNT_PER_WRITER;
        let btree = store
            .btree_indices
            .read()
            .get(COLLECTION)
            .cloned()
            .expect("collection B-tree exists");
        assert_eq!(btree.count().expect("B-tree count"), row_count);
        drop(btree);
        drop(Arc::try_unwrap(store).ok().expect("all threads joined"));

        let reopened = UnifiedStore::open(&path).expect("reopen after concurrent persist");
        let reopened_count = reopened
            .get_collection(COLLECTION)
            .map(|manager| manager.query_all(|_| true).len())
            .unwrap_or(0);
        assert_eq!(reopened_count, row_count);
    }

    #[test]
    fn test_paged_mode_survives_multiple_reopens() {
        let (_guard, path) = temp_path("paged_multi_reopen");

        let store = UnifiedStore::open(&path).unwrap();
        store.set_config_tree(
            "red.system",
            &crate::json!({
                "hostname": "test-host",
                "arch": "x86_64",
                "started_at": 123_u64
            }),
        );
        let initial = store
            .get_collection("red_config")
            .map(|m| m.query_all(|_| true).len())
            .unwrap_or(0);
        assert!(initial >= 3);
        drop(store);

        let reopened = UnifiedStore::open(&path).unwrap();
        let first_reopen = reopened
            .get_collection("red_config")
            .map(|m| m.query_all(|_| true).len())
            .unwrap_or(0);
        assert_eq!(first_reopen, initial);
        drop(reopened);

        let reopened_again = UnifiedStore::open(&path).unwrap();
        let second_reopen = reopened_again
            .get_collection("red_config")
            .map(|m| m.query_all(|_| true).len())
            .unwrap_or(0);
        assert_eq!(second_reopen, initial);
    }

    #[test]
    fn set_config_tree_replaces_earlier_rows_for_the_same_key() {
        let store = UnifiedStore::new();
        let config_rows = |store: &UnifiedStore| {
            store
                .get_collection("red_config")
                .map(|m| m.query_all(|_| true).len())
                .unwrap_or(0)
        };

        store.set_config_tree("red.system", &crate::json!({ "pid": 1_u64, "os": "linux" }));
        let after_first = config_rows(&store);
        for pid in 2..=5_u64 {
            store.set_config_tree("red.system", &crate::json!({ "pid": pid, "os": "linux" }));
        }

        assert_eq!(config_rows(&store), after_first);
        assert_eq!(
            store.get_config("red.system.pid"),
            Some(reddb_types::Value::UnsignedInteger(5))
        );
    }

    #[test]
    fn set_config_tree_skips_keys_that_already_hold_the_value() {
        let store = UnifiedStore::new();
        store.set_config_tree("red.system", &crate::json!({ "os": "linux", "pid": 1_u64 }));
        let ids_of = |store: &UnifiedStore| {
            let mut ids: Vec<u64> = store
                .get_collection("red_config")
                .map(|m| m.query_all(|_| true).iter().map(|e| e.id.raw()).collect())
                .unwrap_or_default();
            ids.sort_unstable();
            ids
        };
        let before = ids_of(&store);

        store.set_config_tree("red.system", &crate::json!({ "os": "linux", "pid": 1_u64 }));
        assert_eq!(
            ids_of(&store),
            before,
            "identical values must not rewrite rows"
        );

        store.set_config_tree("red.system", &crate::json!({ "os": "linux", "pid": 2_u64 }));
        let after = ids_of(&store);
        assert_eq!(after.len(), before.len());
        assert_ne!(after, before, "a changed value is rewritten");
        assert_eq!(
            store.get_config("red.system.pid"),
            Some(reddb_types::Value::UnsignedInteger(2))
        );
        assert_eq!(
            store.get_config("red.system.os"),
            Some(reddb_types::Value::text("linux"))
        );
    }

    fn add_duplicate_system_value(store: &UnifiedStore) {
        let mut row = UnifiedEntity::table_row(EntityId::new(0), "red_config", 0, vec![]);
        row.data = EntityData::Row(RowData::with_names(
            vec![Value::text("red.system.value"), Value::text("duplicate")],
            vec!["key".into(), "value".into()],
        ));
        store.insert_auto("red_config", row).unwrap();
    }

    #[test]
    fn embedded_config_replacement_is_one_wal_publication_and_replays() {
        for mode in [DurabilityMode::Strict, DurabilityMode::WalDurableGrouped] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("config.rdb");
            let mut store = UnifiedStore::new();
            store
                .try_set_config_tree("red.system", &crate::json!({"value": "old", "other": 1}))
                .unwrap();
            add_duplicate_system_value(&store);
            let snapshot = store.to_binary_dump_bytes();
            let initial =
                crate::storage::EmbeddedRdbArtifact::create_with_snapshot(&path, &snapshot)
                    .unwrap();
            store.config.embedded_wal_path = Some(path.clone());
            store.config.durability_mode = mode;
            store
                .try_set_config_tree("red.system", &crate::json!({"value": "new", "other": 2}))
                .unwrap();
            let open = crate::storage::EmbeddedRdbArtifact::open(&path).unwrap();
            assert_eq!(
                open.selected_superblock.generation,
                initial.selected_superblock.generation + 1
            );
            let replay = UnifiedStore::load_from_bytes(&snapshot).unwrap();
            for payload in crate::storage::EmbeddedRdbArtifact::read_wal_payloads(&open).unwrap() {
                replay.apply_encoded_store_wal_action(&payload).unwrap();
            }
            assert_eq!(
                replay.get_config("red.system.value"),
                Some(reddb_types::Value::text("new"))
            );
            assert_eq!(
                replay
                    .get_collection("red_config")
                    .unwrap()
                    .query_all(|_| true)
                    .len(),
                2
            );
            assert_eq!(
                replay.get_config("red.system.other"),
                Some(reddb_types::Value::UnsignedInteger(2))
            );
        }
    }

    #[test]
    fn embedded_config_replacement_restores_old_value_when_wal_fails() {
        for mode in [DurabilityMode::Strict, DurabilityMode::WalDurableGrouped] {
            let directory = tempfile::tempdir().unwrap();
            let mut store = UnifiedStore::new();
            store
                .try_set_config_tree("red.system", &crate::json!({"value": "old", "other": 1}))
                .unwrap();
            add_duplicate_system_value(&store);
            // A directory cannot be opened as the embedded WAL artifact.
            store.config.embedded_wal_path = Some(directory.path().to_path_buf());
            store.config.durability_mode = mode;
            assert!(store
                .try_set_config_tree("red.system", &crate::json!({"value": "new", "other": 2}))
                .is_err());
            assert_eq!(
                store.get_config("red.system.value"),
                Some(reddb_types::Value::text("duplicate"))
            );
            assert_eq!(
                store
                    .get_collection("red_config")
                    .unwrap()
                    .query_all(|_| true)
                    .len(),
                3
            );
            assert_eq!(
                store.get_config("red.system.other"),
                Some(reddb_types::Value::UnsignedInteger(1))
            );
            assert!(!UnifiedStore::deferred_store_wal_capture_active());
        }
    }

    #[test]
    fn embedded_system_config_batch_survives_interrupted_wal_publication() {
        const CHILD: &str = "REDDB_TEST_SYSTEM_CONFIG_CRASH_PATH";
        if let Some(path) = std::env::var_os(CHILD) {
            let path = PathBuf::from(path);
            let (_, snapshot) =
                crate::storage::EmbeddedRdbArtifact::open_with_snapshot(&path).unwrap();
            let mut store = UnifiedStore::load_from_bytes(&snapshot.unwrap()).unwrap();
            store.config.embedded_wal_path = Some(path);
            store.config.durability_mode =
                if std::env::var("REDDB_TEST_SYSTEM_CONFIG_GROUPED").is_ok() {
                    DurabilityMode::WalDurableGrouped
                } else {
                    DurabilityMode::Strict
                };
            store
                .try_set_config_tree("red.system", &crate::json!({"value": "new", "other": 2}))
                .unwrap();
            panic!("crash point was not reached");
        }
        let directory = tempfile::tempdir().unwrap();
        for mode in [DurabilityMode::Strict, DurabilityMode::WalDurableGrouped] {
            for point in [
                "wal_after_frame_write",
                "wal_after_frame_sync",
                "wal_after_superblock_write",
            ] {
                let path = directory.path().join(format!("{mode:?}-{point}.rdb"));
                let store = UnifiedStore::new();
                store
                    .try_set_config_tree("red.system", &crate::json!({"value": "old", "other": 1}))
                    .unwrap();
                let snapshot = store.to_binary_dump_bytes();
                crate::storage::EmbeddedRdbArtifact::create_with_snapshot(&path, &snapshot)
                    .unwrap();
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", "storage::unified::store::builder::tests::embedded_system_config_batch_survives_interrupted_wal_publication", "--nocapture"])
                    .env(CHILD, &path).env("REDDB_EMBEDDED_RDB_CRASH_AT", point);
                if mode == DurabilityMode::WalDurableGrouped {
                    command.env("REDDB_TEST_SYSTEM_CONFIG_GROUPED", "1");
                }
                let output = command.output().unwrap();
                assert_eq!(
                    output.status.code(),
                    Some(173),
                    "child did not reach {point}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let (open, snapshot) =
                    crate::storage::EmbeddedRdbArtifact::open_with_snapshot(&path).unwrap();
                let recovered = UnifiedStore::load_from_bytes(&snapshot.unwrap()).unwrap();
                for payload in
                    crate::storage::EmbeddedRdbArtifact::read_wal_payloads(&open).unwrap()
                {
                    recovered.apply_encoded_store_wal_action(&payload).unwrap();
                }
                let values = (
                    recovered.get_config("red.system.value"),
                    recovered.get_config("red.system.other"),
                );
                assert!(
                    values
                        == (
                            Some(reddb_types::Value::text("old")),
                            Some(reddb_types::Value::UnsignedInteger(1))
                        )
                        || values
                            == (
                                Some(reddb_types::Value::text("new")),
                                Some(reddb_types::Value::UnsignedInteger(2))
                            ),
                    "partial config after {mode:?} {point}: {values:?}"
                );
                assert_eq!(
                    recovered
                        .get_collection("red_config")
                        .unwrap()
                        .query_all(|_| true)
                        .len(),
                    2
                );
            }
        }
    }

    #[test]
    fn test_global_ids_unique_across_collections() {
        let store = UnifiedStore::new();

        let entity_a = UnifiedEntity::table_row(EntityId::new(0), "alpha", 1, vec![]);
        let entity_b = UnifiedEntity::table_row(EntityId::new(0), "beta", 1, vec![]);

        let id_a = store.insert_auto("alpha", entity_a).unwrap();
        let id_b = store.insert_auto("beta", entity_b).unwrap();

        assert_ne!(id_a, id_b);

        store
            .add_cross_ref("alpha", id_a, "beta", id_b, RefType::RelatedTo, 1.0)
            .unwrap();

        let expanded = store.expand_refs(id_a, 1, None);
        assert!(expanded.iter().any(|(entity, _, _)| entity.id == id_b));
    }
}
