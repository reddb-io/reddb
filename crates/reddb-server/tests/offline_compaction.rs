use reddb_server::storage::compact::{
    compact_paged, recover_paged_wal, salvage_paged_collections, salvage_paged_indexes,
    scrub_paged, scrub_paged_trees,
};
use reddb_server::storage::engine::{BTree, Page, PageType, Pager, PagerConfig, HEADER_SIZE};
use std::sync::Arc;

fn payload(id: u64) -> Vec<u8> {
    if !id.is_multiple_of(100) {
        return vec![(id % 251) as u8; 800];
    }
    let mut state = id + 1;
    (0..48_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

fn recovery_page_fixture(path: &std::path::Path) -> (u32, Page) {
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore};
    use reddb_types::Value;
    {
        let store = UnifiedStore::open(path).unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::Integer(17)]),
            )
            .unwrap();
        store.persist().unwrap();
    }
    let pager = Pager::open_default(path).unwrap();
    let catalog = pager.read_page(1).unwrap();
    let header = reddb_file::decode_native_paged_metadata_header(catalog.content())
        .unwrap()
        .unwrap();
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root = (0..header.collection_count)
        .map(|_| {
            reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset).unwrap()
        })
        .find(|entry| entry.collection == "rows")
        .unwrap()
        .root_page;
    let page = pager.read_page(root).unwrap();
    (root, page)
}

fn damage_page(path: &std::path::Path, page_id: u32) {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(page_id) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
}

fn assert_recovered_fixture(path: &std::path::Path) {
    use reddb_server::storage::{EntityId, UnifiedStore};
    use reddb_types::Value;
    for _ in 0..2 {
        let store = UnifiedStore::open(path).unwrap();
        assert_eq!(
            store
                .get("rows", EntityId::new(1))
                .unwrap()
                .data
                .as_row()
                .unwrap()
                .columns,
            vec![Value::Integer(17)]
        );
    }
}

#[test]
fn recover_pending_double_write_repairs_only_the_staged_copy() {
    use std::io::{Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    let (root, page) = recovery_page_fixture(&source);
    let frame = reddb_file::encode_paged_dwb_frame([(root, page.as_bytes())]);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap();
        file.seek(SeekFrom::Start(
            u64::from(reddb_engine::storage::pager_io::DWB_ZONE_START_PAGE) * 16384,
        ))
        .unwrap();
        file.write_all(&frame).unwrap();
        file.sync_all().unwrap();
    }
    damage_page(&source, root);
    let before = std::fs::read(&source).unwrap();
    let output = directory.path().join("recovered");
    recover_paged_wal(&source, &output).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_recovered_fixture(&output.join("source.rdb"));
    assert_eq!(
        u64::from(root) + 1,
        before.len() as u64 / 16384,
        "fixture must place the damaged page last"
    );
    let mut partial = before.clone();
    partial.truncate(root as usize * 16384 + 1024);
    let partial_source = directory.path().join("partial.rdb");
    std::fs::write(&partial_source, &partial).unwrap();
    let partial_output = directory.path().join("partial-recovered");
    recover_paged_wal(&partial_source, &partial_output)
        .expect("restore a torn final page from DWB");
    assert_eq!(std::fs::read(&partial_source).unwrap(), partial);
    assert_recovered_fixture(&partial_output.join("partial.rdb"));
    let mut unsupported = partial;
    for id in reddb_engine::storage::pager_io::DWB_ZONE_START_PAGE
        ..reddb_engine::storage::pager_io::FIRST_ALLOCATABLE_PAGE
    {
        let mut page = Page::new(PageType::Header, id);
        page.update_checksum();
        let offset = id as usize * 16384;
        unsupported[offset..offset + 16384].copy_from_slice(page.as_bytes());
    }
    let missing_image = directory.path().join("missing-image.rdb");
    std::fs::write(&missing_image, &unsupported).unwrap();
    let missing_wal = reddb_file::unified_wal_path(&missing_image);
    {
        use reddb_server::storage::wal::{WalRecord, WalWriter};
        let mut writer = WalWriter::open(&missing_wal).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 999 }).unwrap();
        writer.sync().unwrap();
    }
    let missing_output = directory.path().join("missing-image-output");
    assert!(recover_paged_wal(&missing_image, &missing_output).is_err());
    assert!(!missing_output.join("missing-image.rdb").exists());
    assert_eq!(std::fs::read(&missing_image).unwrap(), unsupported);

    let mut invalid = before.clone();
    invalid[reddb_engine::storage::pager_io::DWB_ZONE_START_PAGE as usize * 16384 + 20] ^= 1;
    let corrupted = directory.path().join("corrupt-frame.rdb");
    std::fs::write(&corrupted, &invalid).unwrap();
    let output = directory.path().join("corrupt-output");
    assert!(recover_paged_wal(&corrupted, &output).is_err());
    assert!(!output.join("corrupt-frame.rdb").exists());
    assert_eq!(std::fs::read(corrupted).unwrap(), invalid);
    let mut duplicate = before;
    let frame =
        reddb_file::encode_paged_dwb_frame([(root, page.as_bytes()), (root, page.as_bytes())]);
    let offset = reddb_engine::storage::pager_io::DWB_ZONE_START_PAGE as usize * 16384;
    duplicate[offset..offset + frame.len()].copy_from_slice(&frame);
    let duplicate_source = directory.path().join("duplicate.rdb");
    std::fs::write(&duplicate_source, &duplicate).unwrap();
    let output = directory.path().join("duplicate-output");
    assert!(recover_paged_wal(&duplicate_source, &output).is_err());
    assert!(!output.join("duplicate.rdb").exists());
    assert_eq!(std::fs::read(duplicate_source).unwrap(), duplicate);
}

#[test]
fn recovery_refuses_stale_legacy_pager_wal_without_losing_newer_data() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore};
    use reddb_types::Value;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    let (root, old_page) = recovery_page_fixture(&source);
    {
        let pager = Arc::new(Pager::open_default(&source).unwrap());
        let catalog = pager.read_page(1).unwrap();
        let format = reddb_file::decode_native_paged_metadata_header(catalog.content())
            .unwrap()
            .unwrap()
            .format_version;
        let newer = UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![Value::Integer(29)]);
        let encoded =
            reddb_engine::storage::metadata_codec::serialize_entity_record(&newer, None, format);
        let tree = BTree::with_root(Arc::clone(&pager), root);
        tree.insert(&2u64.to_be_bytes(), &encoded).unwrap();
        assert_eq!(tree.root_page_id(), root);
        pager.sync().unwrap();
    }
    let wal = reddb_file::pager_legacy_wal_path(&source);
    {
        let mut writer = WalWriter::open(&wal).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 1,
                page_id: root,
                data: old_page.as_bytes().to_vec(),
            })
            .unwrap();
        writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();
        writer.sync().unwrap();
    }
    let before = std::fs::read(&source).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    let destination = directory.path().join("rejected");
    assert!(recover_paged_wal(&source, &destination).is_err());
    assert!(!destination.join("source.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    std::fs::remove_file(&wal).unwrap();
    let store = UnifiedStore::open(&source).unwrap();
    assert_eq!(
        store
            .get("rows", EntityId::new(2))
            .unwrap()
            .data
            .as_row()
            .unwrap()
            .columns,
        vec![Value::Integer(29)]
    );
}

#[test]
fn recovery_refuses_unmaterialized_multimodel_wal_without_changing_source() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    recovery_page_fixture(&source);
    let wal = reddb_file::unified_wal_path(&source);
    for (name, record) in [
        (
            "vector",
            WalRecord::VectorInsert {
                collection: "embeddings".into(),
                entity_id: 42,
                vector: vec![1.0, 2.0],
            },
        ),
        (
            "probabilistic",
            WalRecord::ProbabilisticDelta {
                kind: 1,
                operation: 1,
                name: "visitors".into(),
                operands: vec![b"alice".to_vec()],
            },
        ),
    ] {
        {
            let mut writer = WalWriter::open(&wal).unwrap();
            writer.append(&record).unwrap();
            writer.sync().unwrap();
        }
        let before = std::fs::read(&source).unwrap();
        let wal_before = std::fs::read(&wal).unwrap();
        let destination = directory.path().join(name);
        recover_paged_wal(&source, &destination).expect_err("runtime data has no base state");
        assert!(!destination.join("source.rdb").exists());
        assert_eq!(std::fs::read(&source).unwrap(), before);
        assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
        std::fs::remove_file(&wal).unwrap();
    }
}

#[test]
fn pending_multimodel_wal_child() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let Some(path) = std::env::var_os("REDDB_TEST_MULTIMODEL_WAL_PATH") else {
        return;
    };
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(std::path::PathBuf::from(path))
            .with_memory_budget(64 * 1024 * 1024)
            .with_bounded_disk_residency(true),
    )
    .unwrap();
    let queries = if std::env::var_os("REDDB_TEST_PROBABILISTIC_BEFORE_DDL").is_some() {
        [
            "HLL ADD visitors 'alice' 'bob' 'carol'",
            "SKETCH ADD frequencies 'red' 3",
            "FILTER ADD seen 'alice'",
            "DROP HLL obsolete_hll",
            "INSERT INTO embeddings VECTOR (embedding, content) VALUES ([1, 0], 'alice')",
        ]
    } else {
        [
            "DROP HLL obsolete_hll",
            "INSERT INTO embeddings VECTOR (embedding, content) VALUES ([1, 0], 'alice')",
            "HLL ADD visitors 'alice' 'bob' 'carol'",
            "SKETCH ADD frequencies 'red' 3",
            "FILTER ADD seen 'alice'",
        ]
    };
    for query in queries {
        runtime
            .execute_query(query)
            .unwrap_or_else(|error| panic!("{query}: {error}"));
    }
    std::process::exit(0);
}

#[test]
fn recover_multimodel_wal_survives_checkpoint_and_reopening() {
    run_multimodel_wal_recovery(false);
}

#[test]
fn recover_multimodel_wal_after_catalog_checkpoint() {
    run_multimodel_wal_recovery(true);
}

fn run_multimodel_wal_recovery(probabilistic_before_ddl: bool) {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("multimodel.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).unwrap();
        for query in [
            "CREATE HLL visitors",
            "CREATE HLL obsolete_hll",
            "CREATE SKETCH frequencies",
            "CREATE FILTER seen",
            "CREATE COLLECTION embeddings KIND vector.turbo DIM 2 METRIC cosine",
        ] {
            runtime
                .execute_query(query)
                .unwrap_or_else(|error| panic!("{query}: {error}"));
        }
        runtime.checkpoint().unwrap();
        runtime.graceful_shutdown(false).unwrap();
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "pending_multimodel_wal_child", "--nocapture"])
        .env("REDDB_TEST_MULTIMODEL_WAL_PATH", &source);
    if probabilistic_before_ddl {
        command.env("REDDB_TEST_PROBABILISTIC_BEFORE_DDL", "1");
    }
    let child = command.output().unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let wal = reddb_file::unified_wal_path(&source);
    let before = std::fs::read(&source).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    let records: Vec<_> = reddb_server::storage::wal::WalReader::open(&wal)
        .unwrap()
        .iter()
        .map(|record| record.unwrap().1)
        .collect();
    assert!(records.iter().any(|record| matches!(
        record,
        reddb_server::storage::wal::WalRecord::VectorInsert { .. }
    )));
    for kind in 1..=3 {
        assert!(records.iter().any(|record| matches!(record, reddb_server::storage::wal::WalRecord::ProbabilisticDelta { kind: found, operation: 1, .. } if *found == kind)), "kind {kind}");
    }
    let snapshot = reddb_file::support_dir_for(&source).join("snapshots/embeddings.tv");
    // Synthesize a legacy empty cache so recovery still validates and retires
    // old derived artifacts, although new runtimes no longer publish them.
    assert!(!snapshot.exists());
    std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    reddb_file::write_turboquant_snapshot(&snapshot, 2, 0x7155_8807_FED4_2913, 0, &[]).unwrap();
    let snapshot_before = std::fs::read(&snapshot).unwrap();
    let destination = directory.path().join("recovered");
    recover_paged_wal(&source, &destination).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    assert_eq!(std::fs::read(&snapshot).unwrap(), snapshot_before);
    let mut damaged_snapshot = snapshot_before.clone();
    *damaged_snapshot.last_mut().unwrap() ^= 1;
    std::fs::write(&snapshot, &damaged_snapshot).unwrap();
    let bad_snapshot_output = directory.path().join("bad-snapshot");
    assert!(recover_paged_wal(&source, &bad_snapshot_output).is_err());
    assert!(!bad_snapshot_output.join("multimodel.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    assert_eq!(std::fs::read(&snapshot).unwrap(), damaged_snapshot);
    std::fs::write(&snapshot, &snapshot_before).unwrap();
    let wrong_vector = records
        .iter()
        .find_map(|record| match record {
            reddb_server::storage::wal::WalRecord::VectorInsert {
                collection,
                entity_id,
                vector,
            } => {
                let mut vector = vector.clone();
                vector[0] += 0.5;
                Some(reddb_server::storage::wal::WalRecord::VectorInsert {
                    collection: collection.clone(),
                    entity_id: *entity_id,
                    vector,
                })
            }
            _ => None,
        })
        .unwrap();
    {
        let mut writer = reddb_server::storage::wal::WalWriter::open(&wal).unwrap();
        writer.append(&wrong_vector).unwrap();
        writer.sync().unwrap();
    }
    let mismatched_wal = std::fs::read(&wal).unwrap();
    let mismatch_output = directory.path().join("mismatched-vector");
    assert!(recover_paged_wal(&source, &mismatch_output).is_err());
    assert!(!mismatch_output.join("multimodel.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), mismatched_wal);
    std::fs::write(&wal, &wal_before).unwrap();
    let recovered = RedDBOptions::persistent(destination.join("multimodel.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for _ in 0..2 {
        let runtime = RedDBRuntime::with_options(recovered.clone()).unwrap();
        let count = runtime.execute_query("HLL COUNT visitors").unwrap();
        assert_eq!(
            count.result.records[0]
                .get("count")
                .unwrap()
                .display_string(),
            "3"
        );
        let sketch = runtime
            .execute_query("SKETCH COUNT frequencies 'red'")
            .unwrap();
        assert_eq!(
            sketch.result.records[0]
                .get("estimate")
                .unwrap()
                .display_string(),
            "3"
        );
        let filter = runtime.execute_query("FILTER CHECK seen 'alice'").unwrap();
        assert_eq!(
            filter.result.records[0]
                .get("exists")
                .unwrap()
                .display_string(),
            "true"
        );
        assert!(runtime.execute_query("HLL COUNT obsolete_hll").is_err());
        for mode in ["EXACT", "APPROXIMATE"] {
            let vectors = runtime
                .execute_query(&format!(
                    "VECTOR SEARCH embeddings SIMILAR TO [1, 0] MODE {mode} LIMIT 2"
                ))
                .unwrap();
            assert!(
                vectors
                    .result
                    .records
                    .iter()
                    .any(|row| row.get("content") == Some(&reddb_types::Value::text("alice"))),
                "{mode}"
            );
        }
        runtime.checkpoint().unwrap();
        runtime.graceful_shutdown(false).unwrap();
    }
}

#[test]
fn recover_interrupted_engine_checkpoint_from_committed_wal() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    let (root, page) = recovery_page_fixture(&source);
    let wal = reddb_file::engine_wal_path(&source);
    let checkpoint_lsn;
    {
        let mut writer = WalWriter::open(&wal).unwrap();
        let mut obsolete_page = page.clone();
        obsolete_page.set_cell_count(0);
        obsolete_page.update_checksum();
        let mut latest_page = page.clone();
        latest_page.as_bytes_mut()[100] ^= 1;
        assert!(
            latest_page.verify_checksum().is_err(),
            "engine WAL captures pages before checksum refresh"
        );
        writer.append(&WalRecord::Begin { tx_id: 7 }).unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 7,
                page_id: root,
                data: obsolete_page.as_bytes().to_vec(),
            })
            .unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 7,
                page_id: root,
                data: latest_page.as_bytes().to_vec(),
            })
            .unwrap();
        checkpoint_lsn = writer.append(&WalRecord::Commit { tx_id: 7 }).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 8 }).unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 8,
                page_id: root,
                data: obsolete_page.as_bytes().to_vec(),
            })
            .unwrap();
        writer.sync().unwrap();
    }
    {
        let pager = Pager::open_default(&source).unwrap();
        pager
            .set_checkpoint_in_progress(true, checkpoint_lsn)
            .unwrap();
    }
    damage_page(&source, root);
    let before = std::fs::read(&source).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    let output = directory.path().join("recovered");
    recover_paged_wal(&source, &output).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    assert_recovered_fixture(&output.join("source.rdb"));
    // An older DWB image must be applied before the newer committed WAL image.
    let mut combined = before.clone();
    let mut old_page = page.clone();
    old_page.set_cell_count(0);
    old_page.update_checksum();
    let frame = reddb_file::encode_paged_dwb_frame([(root, old_page.as_bytes())]);
    let offset = reddb_engine::storage::pager_io::DWB_ZONE_START_PAGE as usize * 16384;
    combined[offset..offset + frame.len()].copy_from_slice(&frame);
    let combined_source = directory.path().join("combined.rdb");
    let combined_wal = reddb_file::engine_wal_path(&combined_source);
    std::fs::write(&combined_source, &combined).unwrap();
    std::fs::write(&combined_wal, &wal_before).unwrap();
    let combined_output = directory.path().join("combined-recovery");
    recover_paged_wal(&combined_source, &combined_output).unwrap();
    assert_eq!(std::fs::read(&combined_source).unwrap(), combined);
    assert_eq!(std::fs::read(&combined_wal).unwrap(), wal_before);
    assert_recovered_fixture(&combined_output.join("combined.rdb"));
    assert_eq!(u64::from(root) + 1, before.len() as u64 / 16384);
    let mut partial = before.clone();
    partial.truncate(root as usize * 16384 + 1024);
    let partial_source = directory.path().join("partial-checkpoint.rdb");
    let partial_wal = reddb_file::engine_wal_path(&partial_source);
    std::fs::write(&partial_source, &partial).unwrap();
    std::fs::write(&partial_wal, &wal_before).unwrap();
    let partial_output = directory.path().join("partial-checkpoint-recovery");
    recover_paged_wal(&partial_source, &partial_output).unwrap();
    assert_eq!(std::fs::read(&partial_source).unwrap(), partial);
    assert_eq!(std::fs::read(&partial_wal).unwrap(), wal_before);
    assert_recovered_fixture(&partial_output.join("partial-checkpoint.rdb"));
    let missing_target = &wal_before[..checkpoint_lsn as usize];
    std::fs::write(&wal, missing_target).unwrap();
    let output = directory.path().join("missing-checkpoint-target");
    let error = recover_paged_wal(&source, &output).expect_err("checkpoint target is absent");
    assert!(error.to_string().contains("checkpoint target"), "{error}");
    assert!(!output.join("source.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), missing_target);
    let mut truncated = wal_before;
    truncated.pop();
    std::fs::write(&wal, &truncated).unwrap();
    let output = directory.path().join("truncated-checkpoint");
    assert!(recover_paged_wal(&source, &output).is_err());
    assert!(!output.join("source.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), truncated);
}

#[test]
fn physical_wal_recovery_advances_xid_floor_for_changed_existing_root() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore, UnifiedStoreConfig};
    use reddb_types::Value;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("xid.rdb");
    let config = UnifiedStoreConfig {
        resident_memory_bytes_max: Some(64 * 1024 * 1024),
        ..Default::default()
    };
    {
        let store = UnifiedStore::open_with_config(&source, config.clone()).unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::Integer(17)]),
            )
            .unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![Value::Integer(18)]),
            )
            .unwrap();
        store.persist().unwrap();
    }
    let copy = directory.path().join("prepare.rdb");
    std::fs::copy(&source, &copy).unwrap();
    let pager = Arc::new(
        Pager::open(
            &copy,
            PagerConfig {
                disk_residency: true,
                ..PagerConfig::default()
            },
        )
        .unwrap(),
    );
    let catalog = pager.read_page(1).unwrap();
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .unwrap()
        .unwrap();
    let old_floor = resume.xid_high_water_mark.unwrap();
    let root = resume.collections["rows"].root_page;
    let format = reddb_file::decode_native_paged_metadata_header(catalog.content())
        .unwrap()
        .unwrap()
        .format_version;
    let mut entity = UnifiedEntity::table_row(
        EntityId::new(50_000),
        "rows",
        60_000,
        vec![Value::Integer(17)],
    );
    entity.xmin = 50_000;
    entity.xmax = 70_000;
    entity.sequence_id = 70_000;
    let encoded =
        reddb_engine::storage::metadata_codec::serialize_entity_record(&entity, None, format);
    let tree = BTree::with_root(Arc::clone(&pager), root);
    tree.insert(&50_000u64.to_be_bytes(), &encoded).unwrap();
    assert_eq!(tree.root_page_id(), root);
    let page = pager.read_page(root).unwrap();
    drop(tree);
    drop(pager);
    let wal = reddb_file::engine_wal_path(&source);
    {
        let mut writer = WalWriter::open(&wal).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 90_000 }).unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 90_000,
                page_id: root,
                data: page.as_bytes().to_vec(),
            })
            .unwrap();
        writer.append(&WalRecord::Commit { tx_id: 90_000 }).unwrap();
        writer.sync().unwrap();
    }
    let before = std::fs::read(&source).unwrap();
    let output = directory.path().join("xid-recovered");
    recover_paged_wal(&source, &output).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let pager = Pager::open(
        &output.join("xid.rdb"),
        PagerConfig {
            read_only: true,
            create: false,
            ..PagerConfig::default()
        },
    )
    .unwrap();
    let catalog = pager.read_page(1).unwrap();
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .unwrap()
        .unwrap();
    assert!(resume.xid_high_water_mark.unwrap() >= old_floor.max(90_000));
    assert!(resume.entity_id_next > 50_000);
    assert!(resume.collections["rows"].entity_id_next > 50_000);
    assert!(resume.collections["rows"].row_id_next > 60_000);
    assert!(resume.collections["rows"].sequence_id_next > 70_000);
    assert_eq!(resume.collections["rows"].entities, 3);
    drop(pager);
    let store = UnifiedStore::open_with_config(output.join("xid.rdb"), config).unwrap();
    assert!(store.next_entity_id().raw() > 50_000);
    let rows = store.get_collection("rows").unwrap();
    assert!(rows.next_row_id() > 60_000);
    assert!(rows.sequence_id_next() > 70_000);
}

#[test]
fn physical_wal_recovery_keeps_secondary_index_consistent_with_changed_data() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};
    use reddb_server::{RedDBOptions, RedDBRuntime};
    use reddb_types::Value;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("indexed.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).unwrap();
        runtime
            .execute_query("CREATE TABLE records (id INT, value TEXT)")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX record_value ON records (value) USING HASH")
            .unwrap();
        runtime
            .execute_query("INSERT INTO records (id, value) VALUES (1, 'before')")
            .unwrap();
        runtime.graceful_shutdown(false).unwrap();
    }
    let modified = directory.path().join("modified.rdb");
    std::fs::copy(&source, &modified).unwrap();
    let pager = Arc::new(
        Pager::open(
            &modified,
            PagerConfig {
                disk_residency: true,
                ..PagerConfig::default()
            },
        )
        .unwrap(),
    );
    let catalog = pager.read_page(1).unwrap();
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .unwrap()
        .unwrap();
    let root = resume.collections["records"].root_page;
    let format = reddb_file::decode_native_paged_metadata_header(catalog.content())
        .unwrap()
        .unwrap()
        .format_version;
    let tree = BTree::with_root(Arc::clone(&pager), root);
    let mut cursor = tree.cursor_first().unwrap();
    let (key, bytes) = cursor.next_with_budget(1024 * 1024).unwrap().unwrap();
    let id = reddb_server::storage::EntityId::new(u64::from_be_bytes(
        key.as_slice().try_into().unwrap(),
    ));
    let (mut entity, metadata) =
        reddb_engine::storage::collection_read::EncodedRecord { id, bytes }
            .decode(format)
            .unwrap();
    let reddb_server::storage::EntityData::Row(row) = &mut entity.data else {
        panic!("row")
    };
    if let Some(named) = row.named.as_mut() {
        named.insert("value".into(), Value::text("after"));
    }
    if row.columns.len() > 1 {
        row.columns[1] = Value::text("after");
    }
    let encoded = reddb_engine::storage::metadata_codec::serialize_entity_record(
        &entity,
        metadata.as_ref(),
        format,
    );
    drop(cursor);
    tree.upsert(&key, &encoded).unwrap();
    assert_eq!(tree.root_page_id(), root);
    let page = pager.read_page(root).unwrap();
    drop(tree);
    drop(pager);
    let wal = reddb_file::engine_wal_path(&source);
    {
        let mut writer = WalWriter::open(&wal).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 700 }).unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 700,
                page_id: root,
                data: page.as_bytes().to_vec(),
            })
            .unwrap();
        writer.append(&WalRecord::Commit { tx_id: 700 }).unwrap();
        writer.sync().unwrap();
    }
    let output = directory.path().join("index-recovered");
    recover_paged_wal(&source, &output).unwrap();
    let options = RedDBOptions::persistent(output.join("indexed.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for _ in 0..2 {
        let runtime = RedDBRuntime::with_options(options.clone()).unwrap();
        let row = runtime
            .execute_query("SELECT id FROM records WHERE value = 'after'")
            .unwrap();
        assert_eq!(
            row.result.records.len(),
            1,
            "recovered index must include updated row"
        );
        assert_eq!(
            row.result.records[0].get("id").unwrap().display_string(),
            "1"
        );
        let row = runtime
            .execute_query("SELECT id FROM records WHERE value = 'before'")
            .unwrap();
        assert!(row.result.records.is_empty());
        let explain = runtime
            .explain_query("SELECT id FROM records WHERE value = 'after'")
            .unwrap();
        let mut nodes = vec![&explain.logical_plan.root];
        let mut indexed = false;
        while let Some(node) = nodes.pop() {
            indexed |= node.operator == "index_seek";
            nodes.extend(&node.children);
        }
        assert!(indexed, "recovered query must use the secondary index");
        runtime.graceful_shutdown(false).unwrap();
    }
}

#[test]
fn recover_pending_wal_on_copy_preserves_source_and_committed_records() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore};
    use reddb_types::Value;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    {
        let store = UnifiedStore::open(&source).unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::Integer(10)]),
            )
            .unwrap();
        store.persist().unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![Value::Integer(20)]),
            )
            .unwrap();
    }
    let wal = reddb_file::unified_wal_path(&source);
    {
        let mut writer = WalWriter::open(&wal).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 999 }).unwrap();
        let uncommitted = reddb_file::encode_store_wal_action_frame(
            &reddb_file::StoreWalActionFrame::DeleteEntityRecord {
                collection: "rows".into(),
                entity_id: EntityId::new(1).raw(),
            },
        )
        .unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 999,
                page_id: 0,
                data: uncommitted,
            })
            .unwrap();
        writer.sync().unwrap();
    }
    let source_image = std::fs::read(&source).unwrap();
    let wal_image = std::fs::read(&wal).unwrap();
    assert!(wal_image.len() > reddb_file::WAL_FILE_HEADER_BYTES);
    let output = directory.path().join("recovered");
    recover_paged_wal(&source, &output).expect("recover pending WAL");
    assert_eq!(std::fs::read(&source).unwrap(), source_image);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_image);
    for _ in 0..2 {
        let store = UnifiedStore::open(output.join("source.rdb")).unwrap();
        for (id, value) in [(1, 10), (2, 20)] {
            assert_eq!(
                store
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::Integer(value)]
            );
        }
    }
    let mut truncated = wal_image.clone();
    truncated.pop();
    std::fs::write(&wal, &truncated).unwrap();
    let failed = directory.path().join("truncated");
    assert!(recover_paged_wal(&source, &failed).is_err());
    assert!(!failed.join("source.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), source_image);
    assert_eq!(std::fs::read(&wal).unwrap(), truncated);
    let mut corrupt = wal_image.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    std::fs::write(&wal, &corrupt).unwrap();
    let failed = directory.path().join("corrupt-wal");
    assert!(recover_paged_wal(&source, &failed).is_err());
    assert!(!failed.join("source.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), source_image);
    assert_eq!(std::fs::read(&wal).unwrap(), corrupt);
    std::fs::write(&wal, &wal_image).unwrap();
    {
        let mut writer = WalWriter::open(&wal).unwrap();
        let action = reddb_file::encode_store_wal_action_frame(
            &reddb_file::StoreWalActionFrame::CreateCollection {
                name: "new_table".into(),
            },
        )
        .unwrap();
        writer
            .append(&WalRecord::TxCommitBatch {
                tx_id: 1000,
                actions: vec![action],
            })
            .unwrap();
        let action = reddb_file::encode_store_wal_action_frame(
            &reddb_file::StoreWalActionFrame::CreateCollection {
                name: "committed_legacy".into(),
            },
        )
        .unwrap();
        writer.append(&WalRecord::Begin { tx_id: 1001 }).unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 1001,
                page_id: 0,
                data: action,
            })
            .unwrap();
        writer.append(&WalRecord::Commit { tx_id: 1001 }).unwrap();
        writer.sync().unwrap();
    }
    let schema_wal = std::fs::read(&wal).unwrap();
    let schema_output = directory.path().join("schema-wal");
    recover_paged_wal(&source, &schema_output).expect("replay committed collection creation");
    let recovered = UnifiedStore::open(schema_output.join("source.rdb")).unwrap();
    assert!(recovered
        .list_collections()
        .iter()
        .any(|name| name == "new_table"));
    assert!(recovered
        .list_collections()
        .iter()
        .any(|name| name == "committed_legacy"));
    assert_eq!(std::fs::read(&source).unwrap(), source_image);
    assert_eq!(std::fs::read(&wal).unwrap(), schema_wal);
}

#[test]
fn recover_pending_wal_preserves_and_continues_the_audit_chain() {
    use reddb_server::runtime::audit_log::AuditLogger;
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore};
    use reddb_types::Value;
    use std::time::Duration;

    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("audited.rdb");
    {
        let store = UnifiedStore::open(&source).unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::Integer(10)]),
            )
            .unwrap();
        store.persist().unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![Value::Integer(20)]),
            )
            .unwrap();
    }
    let audit = reddb_file::legacy_audit_log_path(&source);
    {
        let logger = AuditLogger::for_data_path(&source);
        logger.record(
            "admin/checkpoint",
            "operator",
            "audited.rdb",
            "ok",
            reddb_server::serde_json::Value::Null,
        );
        logger.record(
            "admin/recover",
            "operator",
            "audited.rdb",
            "ok",
            reddb_server::serde_json::Value::Null,
        );
        assert!(logger.wait_idle(Duration::from_secs(2)));
    }
    let archived_plain = std::fs::read_to_string(&audit).unwrap();
    let archived_tip = archived_plain.lines().next_back().unwrap();
    let archived_hash =
        reddb_server::utils::to_hex(&reddb_server::crypto::sha256(archived_tip.as_bytes()));
    let rotation = reddb_file::rotate_audit_log(&audit, 42).unwrap();
    let rotated_audit = rotation.compressed_path.unwrap();
    let rotated_before = std::fs::read(&rotated_audit).unwrap();
    let boundary = format!(r#"{{"event":"rotation-boundary","prev_hash":"{archived_hash}"}}"#);
    let boundary_hash =
        reddb_server::utils::to_hex(&reddb_server::crypto::sha256(boundary.as_bytes()));
    let active =
        format!("{boundary}\n{{\"event\":\"active\",\"prev_hash\":\"{boundary_hash}\"}}\n");
    std::fs::write(&audit, active).unwrap();
    let audit_before = std::fs::read(&audit).unwrap();
    let source_before = std::fs::read(&source).unwrap();
    let wal = reddb_file::unified_wal_path(&source);
    let wal_before = std::fs::read(&wal).unwrap();
    let output = directory.path().join("recovered");

    recover_paged_wal(&source, &output).expect("recover a store with audit evidence");

    assert_eq!(std::fs::read(&source).unwrap(), source_before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    assert_eq!(std::fs::read(&audit).unwrap(), audit_before);
    assert_eq!(std::fs::read(&rotated_audit).unwrap(), rotated_before);
    let recovered = output.join("audited.rdb");
    let recovered_audit = reddb_file::legacy_audit_log_path(&recovered);
    let recovered_rotated = output.join(rotated_audit.file_name().unwrap());
    assert_eq!(std::fs::read(&recovered_audit).unwrap(), audit_before);
    assert_eq!(std::fs::read(&recovered_rotated).unwrap(), rotated_before);
    let report = scrub_paged(&recovered).expect("scrub recovered store with verified audit");
    assert!(report.assets_verified);
    assert!(report.trees.findings.is_empty());
    assert_eq!(report.unclassified_pages, 0);
    assert_eq!(std::fs::read(&recovered_audit).unwrap(), audit_before);
    assert_eq!(std::fs::read(&recovered_rotated).unwrap(), rotated_before);

    let previous_line = String::from_utf8(audit_before)
        .unwrap()
        .lines()
        .next_back()
        .unwrap()
        .to_string();
    {
        let logger = AuditLogger::for_data_path(&recovered);
        logger.record(
            "admin/reopen",
            "operator",
            "audited.rdb",
            "ok",
            reddb_server::serde_json::Value::Null,
        );
        assert!(logger.wait_idle(Duration::from_secs(2)));
    }
    let body = std::fs::read_to_string(&recovered_audit).unwrap();
    let appended: reddb_server::serde_json::Value =
        reddb_server::serde_json::from_str(body.lines().next_back().unwrap()).unwrap();
    let expected =
        reddb_server::utils::to_hex(&reddb_server::crypto::sha256(previous_line.as_bytes()));
    assert_eq!(
        appended.get("prev_hash").and_then(|value| value.as_str()),
        Some(expected.as_str())
    );
    assert!(scrub_paged(&recovered).is_ok());
    let mut damaged = body.clone();
    let marker = "\"prev_hash\":\"";
    let offset = damaged.find(marker).unwrap() + marker.len();
    damaged.replace_range(
        offset..offset + 1,
        if &damaged[offset..offset + 1] == "0" {
            "1"
        } else {
            "0"
        },
    );
    std::fs::write(&recovered_audit, damaged).unwrap();
    let error = scrub_paged(&recovered).expect_err("scrub must reject a broken audit chain");
    assert!(
        error.to_string().contains("audit log hash chain"),
        "{error}"
    );
    std::fs::write(&recovered_audit, body).unwrap();

    let mut tampered = std::fs::read_to_string(&audit).unwrap();
    let prefix = "\"prev_hash\":\"";
    let position = tampered.find(prefix).unwrap() + prefix.len();
    let replacement = if &tampered[position..position + 1] == "0" {
        "1"
    } else {
        "0"
    };
    tampered.replace_range(position..position + 1, replacement);
    std::fs::write(&audit, tampered).unwrap();
    let rejected = directory.path().join("rejected-audit");
    let error = recover_paged_wal(&source, &rejected).expect_err("reject a broken audit chain");
    assert!(
        error.to_string().contains("audit log hash chain"),
        "{error}"
    );
    assert!(!rejected.join("audited.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), source_before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);

    let oversized = format!(
        r#"{{"detail":"{}","prev_hash":"{archived_hash}"}}"#,
        "x".repeat(1024 * 1024)
    );
    std::fs::write(&audit, oversized).unwrap();
    let rejected = directory.path().join("rejected-large-audit");
    let error = recover_paged_wal(&source, &rejected).expect_err("reject an oversized audit event");
    assert!(
        error.to_string().contains("audit log event exceeds"),
        "{error}"
    );
    assert!(!rejected.join("audited.rdb").exists());
}

#[test]
fn recover_pending_wal_for_disk_resident_store() {
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore, UnifiedStoreConfig};
    use reddb_types::Value;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("resident.rdb");
    let config = UnifiedStoreConfig {
        resident_memory_bytes_max: Some(64 * 1024 * 1024),
        ..Default::default()
    };
    {
        let store = UnifiedStore::open_with_config(&source, config.clone()).unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::Integer(10)]),
            )
            .unwrap();
        store.persist().unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![Value::Integer(20)]),
            )
            .unwrap();
    }
    let source_image = std::fs::read(&source).unwrap();
    let wal = reddb_file::unified_wal_path(&source);
    let wal_image = std::fs::read(&wal).unwrap();
    let destination = directory.path().join("resident-recovered");
    recover_paged_wal(&source, &destination).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), source_image);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_image);
    for _ in 0..2 {
        let store =
            UnifiedStore::open_with_config(destination.join("resident.rdb"), config.clone())
                .unwrap();
        for (id, expected) in [(1, 10), (2, 20)] {
            assert_eq!(
                store
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::Integer(expected)]
            );
        }
    }
}

#[test]
fn recover_wal_larger_than_256_mib_without_collecting_uncommitted_payloads() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore};
    use reddb_types::Value;
    use std::io::Read;
    fn digest(path: &std::path::Path) -> blake3::Hash {
        let mut input = std::fs::File::open(path).unwrap();
        let mut hash = blake3::Hasher::new();
        let mut bytes = vec![0; 1024 * 1024];
        loop {
            let n = input.read(&mut bytes).unwrap();
            if n == 0 {
                break;
            }
            hash.update(&bytes[..n]);
        }
        hash.finalize()
    }
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("large-wal.rdb");
    {
        let store = UnifiedStore::open(&source).unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::Integer(10)]),
            )
            .unwrap();
        store.persist().unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![Value::Integer(20)]),
            )
            .unwrap();
    }
    let wal = reddb_file::unified_wal_path(&source);
    let mut random = 1u64;
    let bytes: Vec<u8> = (0..1024 * 1024)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random.to_le_bytes()[0]
        })
        .collect();
    let action = reddb_file::encode_store_wal_action_frame(
        &reddb_file::StoreWalActionFrame::SetAuxMetadata { bytes },
    )
    .unwrap();
    let frame = WalRecord::PageWrite {
        tx_id: 999,
        page_id: 0,
        data: action,
    }
    .encode();
    {
        let mut writer = WalWriter::open(&wal).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 999 }).unwrap();
        for _ in 0..258 {
            writer.append_bytes(&frame).unwrap();
        }
        writer.sync().unwrap();
    }
    assert!(std::fs::metadata(&wal).unwrap().len() > 256 * 1024 * 1024);
    let wal_hash = digest(&wal);
    let source_image = std::fs::read(&source).unwrap();
    let output = directory.path().join("large-recovered");
    recover_paged_wal(&source, &output).expect("stream large WAL");
    assert_eq!(digest(&wal), wal_hash);
    assert_eq!(std::fs::read(&source).unwrap(), source_image);
    let store = UnifiedStore::open(output.join("large-wal.rdb")).unwrap();
    for (id, expected) in [(1, 10), (2, 20)] {
        assert_eq!(
            store
                .get("rows", EntityId::new(id))
                .unwrap()
                .data
                .as_row()
                .unwrap()
                .columns,
            vec![Value::Integer(expected)]
        );
    }
}

#[test]
fn pending_runtime_wal_child() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let Some(path) = std::env::var_os("REDDB_TEST_PENDING_RUNTIME_PATH") else {
        return;
    };
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(std::path::PathBuf::from(path))
            .with_memory_budget(64 * 1024 * 1024)
            .with_bounded_disk_residency(true),
    )
    .unwrap();
    runtime
        .execute_query("INSERT INTO records (id, value) VALUES (2, 'after-crash')")
        .unwrap();
    std::process::exit(0);
}

#[test]
fn recover_pending_runtime_wal_preserves_sql_and_index_results() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("runtime.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).unwrap();
        runtime
            .execute_query("CREATE TABLE records (id INT, value TEXT)")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX record_value ON records (value) USING HASH")
            .unwrap();
        runtime
            .execute_query("INSERT INTO records (id, value) VALUES (1, 'before-crash')")
            .unwrap();
        runtime.graceful_shutdown(false).unwrap();
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pending_runtime_wal_child"])
        .env("REDDB_TEST_PENDING_RUNTIME_PATH", &source)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stdout)
    );
    let wal = reddb_file::unified_wal_path(&source);
    let before = std::fs::read(&source).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    let metadata = reddb_file::physical_metadata_binary_path(&source);
    assert!(
        !metadata.exists(),
        "runtime metadata belongs inside the database"
    );
    assert!(wal_before.len() > reddb_file::WAL_FILE_HEADER_BYTES);
    let output = directory.path().join("runtime-recovered");
    recover_paged_wal(&source, &output).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    assert!(!metadata.exists());
    assert!(!reddb_file::physical_metadata_binary_path(&output.join("runtime.rdb")).exists());
    let options = RedDBOptions::persistent(output.join("runtime.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    let runtime = RedDBRuntime::with_options(options).unwrap();
    let row = runtime
        .execute_query("SELECT id FROM records WHERE value = 'after-crash'")
        .unwrap();
    assert_eq!(row.result.records.len(), 1);
    assert_eq!(
        row.result.records[0].get("id").unwrap().display_string(),
        "2"
    );
    let explain = runtime
        .explain_query("SELECT id FROM records WHERE value = 'after-crash'")
        .unwrap();
    let mut nodes = vec![&explain.logical_plan.root];
    let mut uses_index = false;
    while let Some(node) = nodes.pop() {
        uses_index |= node.operator == "index_seek";
        nodes.extend(&node.children);
    }
    assert!(uses_index, "recovered query should use its secondary index");
    runtime.graceful_shutdown(false).unwrap();
}

#[test]
fn pending_schema_wal_child() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let Some(path) = std::env::var_os("REDDB_TEST_SCHEMA_WAL_PATH") else {
        return;
    };
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(std::path::PathBuf::from(path))
            .with_memory_budget(64 * 1024 * 1024)
            .with_bounded_disk_residency(true),
    )
    .unwrap();
    runtime
        .execute_query("CREATE TABLE customers (id INT, name TEXT)")
        .unwrap();
    runtime
        .execute_query("CREATE INDEX customer_name ON customers (name) USING HASH")
        .unwrap();
    runtime
        .execute_query("ALTER TABLE customers ADD COLUMN points INT")
        .unwrap();
    runtime
        .execute_query("INSERT INTO customers (id, name, points) VALUES (7, 'Ada', 42)")
        .unwrap();
    runtime.execute_query("DROP TABLE obsolete").unwrap();
    std::process::exit(0);
}

#[test]
fn recover_schema_wal_preserves_created_and_dropped_tables_and_index() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("schema.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).unwrap();
        runtime
            .execute_query("CREATE TABLE obsolete (id INT)")
            .unwrap();
        runtime
            .execute_query("INSERT INTO obsolete (id) VALUES (99)")
            .unwrap();
        runtime.graceful_shutdown(false).unwrap();
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "pending_schema_wal_child", "--nocapture"])
        .env("REDDB_TEST_SCHEMA_WAL_PATH", &source)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let wal = reddb_file::unified_wal_path(&source);
    {
        use reddb_server::storage::wal::{WalRecord, WalWriter};
        let mut writer = WalWriter::open(&wal).unwrap();
        let action = reddb_file::encode_store_wal_action_frame(
            &reddb_file::StoreWalActionFrame::CreateCollection {
                name: "uncommitted_table".into(),
            },
        )
        .unwrap();
        writer.append(&WalRecord::Begin { tx_id: 9001 }).unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 9001,
                page_id: 0,
                data: action,
            })
            .unwrap();
        writer.sync().unwrap();
    }
    let before = std::fs::read(&source).unwrap();
    let wal_before = std::fs::read(&wal).unwrap();
    let output = directory.path().join("recovered");
    recover_paged_wal(&source, &output).expect("recover committed DDL and data");
    assert_eq!(std::fs::read(&source).unwrap(), before);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_before);
    let recovered = RedDBOptions::persistent(output.join("schema.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for _ in 0..2 {
        let runtime = RedDBRuntime::with_options(recovered.clone()).unwrap();
        let row = runtime
            .execute_query("SELECT id, points FROM customers WHERE name = 'Ada'")
            .unwrap();
        assert_eq!(row.result.records.len(), 1);
        assert_eq!(
            row.result.records[0].get("id").unwrap().display_string(),
            "7"
        );
        assert_eq!(
            row.result.records[0]
                .get("points")
                .unwrap()
                .display_string(),
            "42"
        );
        assert!(runtime.execute_query("SELECT id FROM obsolete").is_err());
        assert!(runtime
            .execute_query("SELECT * FROM uncommitted_table")
            .is_err());
        let explain = runtime
            .explain_query("SELECT id FROM customers WHERE name = 'Ada'")
            .unwrap();
        let mut nodes = vec![&explain.logical_plan.root];
        let mut indexed = false;
        while let Some(node) = nodes.pop() {
            indexed |= node.operator == "index_seek";
            nodes.extend(&node.children);
        }
        assert!(indexed, "recovered query must use the recreated index");
        runtime.graceful_shutdown(false).unwrap();
    }
}

#[test]
fn recover_wal_child_process() {
    let Some(source) = std::env::var_os("REDDB_WAL_RECOVERY_TEST_SOURCE") else {
        return;
    };
    let destination = std::env::var_os("REDDB_WAL_RECOVERY_TEST_DEST").unwrap();
    let _ = recover_paged_wal(
        std::path::Path::new(&source),
        std::path::Path::new(&destination),
    );
}

#[test]
fn recover_wal_with_vault_child() {
    use reddb_server::auth::vault::Vault;
    use reddb_server::storage::{EntityId, UnifiedStore};
    let Some(source) = std::env::var_os("REDDB_WAL_VAULT_TEST_SOURCE") else {
        return;
    };
    let destination =
        std::path::PathBuf::from(std::env::var_os("REDDB_WAL_VAULT_TEST_DEST").unwrap());
    if std::env::var_os("REDDB_WAL_VAULT_EXPECT_DENIAL").is_some() {
        let error = recover_paged_wal(std::path::Path::new(&source), &destination)
            .expect_err("vault must be authenticated");
        assert!(error.to_string().contains("vault"), "{error}");
        assert!(!destination.exists());
        return;
    }
    recover_paged_wal(std::path::Path::new(&source), &destination).expect("replay with vault");
    let output = destination.join("source.rdb");
    {
        let pager = Pager::open(
            &output,
            PagerConfig {
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )
        .unwrap();
        let state = Vault::open(&pager).unwrap().load(&pager).unwrap().unwrap();
        assert_eq!(state.kv["red.secret"], "preserved-secret");
    }
    let store = UnifiedStore::open(&output).unwrap();
    assert!(store.get("rows", EntityId::new(1)).is_some());
    assert!(store.get("rows", EntityId::new(2)).is_some());
}

#[test]
fn recover_pending_wal_authenticates_and_preserves_vault() {
    use reddb_server::auth::vault::{KeyPair, Vault, VaultState};
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore};
    use reddb_types::Value;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    let certificate = KeyPair::generate().certificate_hex();
    {
        let store = UnifiedStore::open(&source).unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![Value::Integer(10)]),
            )
            .unwrap();
        store.persist().unwrap();
        let pager = store.pager().unwrap();
        let vault = Vault::with_certificate(&pager, &certificate).unwrap();
        let mut state = VaultState {
            bootstrapped: true,
            ..VaultState::default()
        };
        state
            .kv
            .insert("red.secret".into(), "preserved-secret".into());
        vault.save(&pager, &state).unwrap();
        pager.sync().unwrap();
        store
            .insert_auto(
                "rows",
                UnifiedEntity::table_row(EntityId::new(2), "rows", 2, vec![Value::Integer(20)]),
            )
            .unwrap();
    }
    let original = std::fs::read(&source).unwrap();
    let wal = reddb_file::unified_wal_path(&source);
    let wal_original = std::fs::read(&wal).unwrap();
    for mode in ["missing", "wrong", "correct"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "recover_wal_with_vault_child", "--nocapture"])
            .env("REDDB_WAL_VAULT_TEST_SOURCE", &source)
            .env("REDDB_WAL_VAULT_TEST_DEST", directory.path().join(mode))
            .env_remove("REDDB_WAL_VAULT_EXPECT_DENIAL")
            .env_remove("REDDB_CERTIFICATE")
            .env_remove("REDDB_CERTIFICATE_FILE")
            .env_remove("REDDB_VAULT_PASSPHRASE")
            .env_remove("REDDB_VAULT_PASSPHRASE_FILE");
        if mode == "wrong" {
            child.env("REDDB_CERTIFICATE", KeyPair::generate().certificate_hex());
        }
        if mode == "correct" {
            child.env("REDDB_CERTIFICATE", &certificate);
        } else {
            child.env("REDDB_WAL_VAULT_EXPECT_DENIAL", "1");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert_eq!(std::fs::read(&wal).unwrap(), wal_original);
    }
}

#[test]
fn interrupted_wal_copy_does_not_publish_or_modify_source() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let wal = reddb_file::unified_wal_path(&source);
    {
        let mut writer = WalWriter::open(&wal).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 123 }).unwrap();
        writer.sync().unwrap();
    }
    let source_image = std::fs::read(&source).unwrap();
    let wal_image = std::fs::read(&wal).unwrap();
    let destination = directory.path().join("interrupted");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "recover_wal_child_process"])
        .env("REDDB_WAL_RECOVERY_TEST_SOURCE", &source)
        .env("REDDB_WAL_RECOVERY_TEST_DEST", &destination)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    while !destination.join(".wal-replay").join("source.rdb").exists() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "recovery ended before interruption"
        );
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("recovery did not start copying");
        }
        std::thread::yield_now();
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(!destination.join("source.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), source_image);
    assert_eq!(std::fs::read(&wal).unwrap(), wal_image);
}

fn fixture(path: &std::path::Path) {
    let pager = Arc::new(Pager::open_default(path).expect("new pager"));
    // Reproduce abandoned allocations below the live root, not just a free tail.
    for _ in 0..512 {
        let page = pager
            .allocate_page(PageType::Overflow)
            .expect("abandoned page");
        pager
            .write_page(page.page_id(), page)
            .expect("write abandoned page");
    }
    let tree = BTree::new(Arc::clone(&pager));
    for id in 0u64..1000 {
        tree.insert(&id.to_be_bytes(), &payload(id))
            .expect("insert");
    }
    let mut metadata = Vec::new();
    reddb_file::encode_native_paged_metadata_header(
        &mut metadata,
        reddb_file::NativePagedMetadataHeader {
            format_version: 11,
            collection_count: 1,
        },
    );
    reddb_file::encode_native_paged_collection_root(&mut metadata, "rows", tree.root_page_id());
    metadata.extend_from_slice(&0u32.to_le_bytes());
    let mut page = Page::new(PageType::Header, 1);
    page.as_bytes_mut()[HEADER_SIZE..HEADER_SIZE + metadata.len()].copy_from_slice(&metadata);
    pager.write_page(1, page).expect("catalog");
    pager.sync().expect("checkpoint");
}

#[test]
fn system_collection_aliases_persist_under_canonical_names_without_renaming_user_collections() {
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore};
    use reddb_types::Value;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("names.rdb");
    let names = [
        ("red_config", "red.config"),
        ("red_queue_meta", "red.queue.meta"),
        ("red.timeseries.meta", "red.timeseries.meta"),
        ("red_queue_meta_user", "red_queue_meta_user"),
    ];
    {
        let store = UnifiedStore::open(&source).unwrap();
        for (position, (input, _)) in names.iter().enumerate() {
            let id = (position + 1) as u64;
            store
                .insert_auto(
                    input,
                    UnifiedEntity::table_row(
                        EntityId::new(id),
                        *input,
                        id,
                        vec![Value::text(format!("value-{id}"))],
                    ),
                )
                .unwrap();
        }
        store.persist().unwrap();
    }
    for _ in 0..2 {
        let store = UnifiedStore::open(&source).unwrap();
        let collections = store.list_collections();
        for (position, (input, expected)) in names.iter().enumerate() {
            let id = (position + 1) as u64;
            assert!(
                collections.iter().any(|name| name == expected),
                "missing {expected}: {collections:?}"
            );
            if input != expected {
                assert!(!collections.iter().any(|name| name == input));
            }
            for spelling in [input, expected] {
                let entity = store
                    .get(spelling, EntityId::new(id))
                    .expect("preserved row");
                assert_eq!(entity.kind.collection(), *expected);
                assert_eq!(
                    entity.data.as_row().unwrap().columns,
                    vec![Value::text(format!("value-{id}"))]
                );
            }
        }
        store.persist().unwrap();
    }
}

#[test]
fn compact_rewrites_live_records_and_preserves_source() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let before = std::fs::read(&source).expect("source image");
    let destination = directory.path().join("compacted");
    let report = compact_paged(&source, &destination).expect("compact");
    assert!(
        !destination.join("compaction.progress.json").exists(),
        "a completed single-file compact must remove transient progress state"
    );
    assert!(report.destination_bytes < report.source_bytes / 2);
    assert_eq!(report.records, 1000);
    assert_eq!(std::fs::read(&source).expect("preserved image"), before);
    let pager = Arc::new(
        Pager::open(
            destination.join("source.rdb"),
            PagerConfig {
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )
        .expect("reopen"),
    );
    let page = pager.read_page(1).expect("catalog");
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root =
        reddb_file::decode_native_paged_collection_root(page.content(), &mut offset).expect("root");
    let tree = BTree::with_root(pager, root.root_page);
    for id in 0u64..1000 {
        assert_eq!(
            tree.get(&id.to_be_bytes()).expect("lookup"),
            Some(payload(id))
        );
    }
}

#[test]
fn compact_rewrites_a_source_with_page_aligned_trailing_allocation() {
    use std::io::Write;

    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let committed_bytes = std::fs::metadata(&source).expect("source metadata").len();
    {
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&source)
            .expect("open source tail");
        file.write_all(&vec![0xA5; 2 * 16384])
            .expect("append stale page-aligned allocation");
        file.sync_all().expect("sync source tail");
    }
    let before = std::fs::read(&source).expect("source image");
    let destination = directory.path().join("compacted");

    let report = compact_paged(&source, &destination).expect("compact trailing allocation");

    assert_eq!(report.source_bytes, committed_bytes + 2 * 16384);
    assert_eq!(std::fs::read(&source).expect("preserved source"), before);
    assert!(report.destination_bytes < committed_bytes);
    let pager = Pager::open(
        destination.join("source.rdb"),
        PagerConfig {
            read_only: true,
            create: false,
            ..PagerConfig::default()
        },
    )
    .expect("reopen compacted source");
    assert_eq!(
        pager.page_count().expect("compacted page count") as u64 * 16384,
        report.destination_bytes
    );
}

#[test]
fn compact_rejects_truncated_or_partial_trailing_allocation() {
    let directory = tempfile::tempdir().expect("directory");
    for (name, length_adjustment) in [("truncated", -16384_i64), ("partial-tail", 1_i64)] {
        let source = directory.path().join(format!("{name}.rdb"));
        fixture(&source);
        let committed_bytes = std::fs::metadata(&source).expect("source metadata").len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&source)
            .expect("open source")
            .set_len(
                committed_bytes
                    .checked_add_signed(length_adjustment)
                    .expect("fixture length"),
            )
            .expect("resize source");
        let destination = directory.path().join(format!("{name}-compacted"));

        let error = compact_paged(&source, &destination).expect_err("reject unsafe boundary");

        assert!(
            error
                .to_string()
                .contains("source allocation boundary differs from file length"),
            "unexpected {name} error: {error}"
        );
        assert!(!destination.exists());
    }
}

#[test]
fn scrub_paged_trees_reports_corruption_without_changing_source() {
    use std::io::{Read, Seek, SeekFrom, Write};

    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    {
        let pager = Arc::new(Pager::open_default(&source).expect("open source"));
        let catalog = pager.read_page(1).expect("catalog");
        let mut offset = reddb_file::METADATA_HEADER_BYTES;
        let first = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset)
            .expect("first root");
        let second = BTree::new(Arc::clone(&pager));
        for id in 0u64..3 {
            second
                .insert(&id.to_be_bytes(), b"healthy")
                .expect("insert");
        }
        let mut metadata = Vec::new();
        reddb_file::encode_native_paged_metadata_header(
            &mut metadata,
            reddb_file::NativePagedMetadataHeader {
                format_version: 11,
                collection_count: 2,
            },
        );
        reddb_file::encode_native_paged_collection_root(&mut metadata, "rows", first.root_page);
        reddb_file::encode_native_paged_collection_root(
            &mut metadata,
            "healthy",
            second.root_page_id(),
        );
        metadata.extend_from_slice(&0u32.to_le_bytes());
        let mut catalog = Page::new(PageType::Header, 1);
        catalog.content_mut()[..metadata.len()].copy_from_slice(&metadata);
        pager.write_page(1, catalog).expect("catalog");
        pager.sync().expect("checkpoint");
    }
    let before = std::fs::read(&source).expect("source image");
    let report = scrub_paged_trees(&source).expect("scrub healthy trees");
    assert_eq!(report.trees_verified, 2);
    assert_eq!(report.records_verified, 1003);
    assert!(report.findings.is_empty());
    assert_eq!(std::fs::read(&source).expect("preserved image"), before);

    let pager = Pager::open_default(&source).expect("open source");
    let catalog = pager.read_page(1).expect("catalog");
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset)
        .expect("root")
        .root_page;
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 0x80;
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged = std::fs::read(&source).expect("damaged image");
    let report = scrub_paged_trees(&source).expect("report corruption");
    assert_eq!(report.trees_verified, 1);
    assert_eq!(report.records_verified, 3);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].collection, "rows");
    assert_eq!(report.findings[0].root_page, root);
    let destination = directory.path().join("unsafe-recovery");
    assert!(salvage_paged_indexes(&source, &destination).is_err());
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).expect("preserved damage"), damaged);
}

#[test]
fn destroyed_paged_superblocks_do_not_trigger_embedded_carving() {
    use std::io::{Seek, SeekFrom, Write};

    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let destination = directory.path().join("recovered.rdb");
    fixture(&source);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .expect("open source for fault injection");
    file.write_all(&vec![0; reddb_file::PAGED_SUPERBLOCK_ZONE_SIZE])
        .expect("destroy both superblocks");
    file.seek(SeekFrom::End(0)).expect("end");
    let mut embedded_value =
        reddb_file::encode_native_store_header(reddb_file::STORE_VERSION_CURRENT);
    embedded_value.extend_from_slice(&[0; 16]);
    reddb_file::append_native_store_crc32_footer(&mut embedded_value);
    file.write_all(&embedded_value)
        .expect("embedded-looking bytes");
    file.sync_all().expect("sync fault injection");
    drop(file);
    let damaged = std::fs::read(&source).expect("damaged image");
    let error = reddb_file::salvage_embedded_store(&source, &destination)
        .expect_err("paged source must not be carved as embedded");
    assert!(error.to_string().contains("paged"), "{error}");
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).expect("preserved source"), damaged);
}

#[test]
fn compact_refuses_busy_source_and_existing_destination() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let destination = directory.path().join("compacted");
    let pager = Pager::open_default(&source).expect("writer");
    assert!(scrub_paged_trees(&source).is_err());
    assert!(salvage_paged_indexes(&source, &destination).is_err());
    assert!(compact_paged(&source, &destination).is_err());
    assert!(!destination.exists());
    drop(pager);
    std::fs::create_dir(&destination).expect("existing output");
    assert!(compact_paged(&source, &destination).is_err());
}

#[test]
fn compact_preserves_chained_authenticated_vault_and_native_blob() {
    use reddb_server::auth::vault::{KeyPair, Vault, VaultState};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let pager = Pager::open_default(&source).expect("pager");
    let certificate = KeyPair::generate().certificate_hex();
    let vault = Vault::with_certificate(&pager, &certificate).expect("vault");
    let mut state = VaultState {
        bootstrapped: true,
        ..VaultState::default()
    };
    state
        .kv
        .insert("red.secret.large".into(), "private-secret".repeat(9000));
    vault.save(&pager, &state).expect("save chained vault");
    let payload = vec![0x73; 5000];
    let mut blob = pager.allocate_page(PageType::NativeMeta).expect("blob");
    let encoded = reddb_file::encode_native_blob_page(0, &payload);
    blob.content_mut()[..encoded.len()].copy_from_slice(&encoded);
    let root = blob.page_id();
    pager.write_page(root, blob).expect("blob page");
    let summary = reddb_file::NativeVectorArtifactPageSummary {
        collection: "rows".into(),
        artifact_kind: "test-opaque".into(),
        root_page: root,
        page_count: 1,
        byte_len: payload.len() as u64,
        checksum: reddb_file::native_store_page_checksum(&payload),
    };
    let encoded = reddb_file::encode_native_vector_artifact_store_page(&[summary]);
    let mut summary = pager
        .allocate_page(PageType::NativeMeta)
        .expect("artifact summary");
    summary.content_mut()[..encoded.len()].copy_from_slice(&encoded);
    let mut physical = pager.physical_header().expect("physical");
    physical.vector_artifact_page = summary.page_id();
    physical.vector_artifact_checksum = reddb_file::native_store_page_checksum(&encoded);
    pager
        .write_page(summary.page_id(), summary)
        .expect("write summary");
    pager
        .update_physical_header(physical)
        .expect("publish physical");
    pager.sync().expect("sync");
    drop(pager);
    let destination = directory.path().join("compacted");
    compact_paged(&source, &destination).expect("compact");
    let pager = Pager::open_default(destination.join("source.rdb")).expect("reopen");
    let restored = Vault::with_certificate(&pager, &certificate)
        .expect("original certificate")
        .load(&pager)
        .expect("authenticated decrypt")
        .expect("vault state");
    assert!(restored.bootstrapped);
    assert_eq!(restored.kv, state.kv);
    let physical = pager.physical_header().expect("physical");
    let page = pager
        .read_page(physical.vector_artifact_page)
        .expect("summary");
    let summary =
        reddb_file::decode_native_vector_artifact_store_page(page.content()).expect("decode");
    let blob = pager.read_page(summary[0].root_page).expect("blob");
    assert_eq!(
        reddb_file::decode_native_blob_page(blob.content())
            .expect("payload")
            .1,
        payload
    );
}

#[test]
fn index_salvage_refuses_unverified_vault() {
    use reddb_server::auth::vault::{KeyPair, Vault, VaultState};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let destination = directory.path().join("recovered");
    fixture(&source);
    let pager = Pager::open_default(&source).expect("pager");
    let certificate = KeyPair::generate().certificate_hex();
    let vault = Vault::with_certificate(&pager, &certificate).expect("vault");
    let mut state = VaultState {
        bootstrapped: true,
        ..VaultState::default()
    };
    state.kv.insert("red.secret".into(), "secret".into());
    vault.save(&pager, &state).expect("save vault");
    pager.sync().expect("checkpoint");
    drop(pager);
    let image = std::fs::read(&source).unwrap();
    assert!(scrub_paged(&source).is_err());
    let error = salvage_paged_indexes(&source, &destination).expect_err("unverified vault");
    assert!(error.to_string().contains("vault"), "{error}");
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), image);
}

#[test]
fn authenticated_vault_recovery_child() {
    use reddb_server::auth::vault::Vault;
    let Some(source) = std::env::var_os("REDDB_TEST_VAULT_SOURCE") else {
        return;
    };
    let source = std::path::Path::new(&source);
    let destination = std::path::PathBuf::from(std::env::var_os("REDDB_TEST_VAULT_DEST").unwrap());
    let accepted = std::env::var_os("REDDB_TEST_VAULT_ACCEPT").is_some();
    if !accepted {
        let scrub_error = scrub_paged(source).expect_err("vault must be rejected");
        assert!(scrub_error.to_string().contains("vault"), "{scrub_error}");
        let salvage_error =
            salvage_paged_indexes(source, &destination).expect_err("vault must be rejected");
        assert!(
            salvage_error.to_string().contains("vault"),
            "{salvage_error}"
        );
        assert!(!destination.exists());
        return;
    }
    assert_eq!(scrub_paged(source).unwrap().trees.findings.len(), 1);
    let report = salvage_paged_indexes(source, &destination).expect("recover authenticated vault");
    assert_eq!(report.indexes.len(), 1);
    let pager = Pager::open(
        destination.join("source.rdb"),
        PagerConfig {
            read_only: true,
            create: false,
            ..PagerConfig::default()
        },
    )
    .unwrap();
    let state = Vault::open(&pager).unwrap().load(&pager).unwrap().unwrap();
    assert_eq!(state.kv["red.secret.large"], "private-secret".repeat(9000));
}

#[test]
fn salvage_authenticates_vault_before_and_after_relocation() {
    use reddb_server::auth::vault::{KeyPair, Vault, VaultState};
    use reddb_server::{RedDBOptions, RedDBRuntime};
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&source)
                .with_memory_budget(64 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        runtime
            .execute_query("CREATE TABLE records (id INT, value TEXT)")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX record_value ON records (value) USING HASH")
            .unwrap();
        runtime
            .execute_query("INSERT INTO records (id, value) VALUES (1, 'saved')")
            .unwrap();
        runtime.graceful_shutdown(false).unwrap();
    }
    let pager = Pager::open(
        &source,
        PagerConfig {
            disk_residency: true,
            ..PagerConfig::default()
        },
    )
    .unwrap();
    let certificate = KeyPair::generate().certificate_hex();
    let vault = Vault::with_certificate(&pager, &certificate).unwrap();
    let mut state = VaultState {
        bootstrapped: true,
        ..VaultState::default()
    };
    state
        .kv
        .insert("red.secret.large".into(), "private-secret".repeat(9000));
    vault.save(&pager, &state).unwrap();
    let catalog = pager.read_page(1).unwrap();
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .unwrap()
        .unwrap();
    let root = resume.collections["records"].secondary_indices["record_value"].root_page;
    pager.sync().unwrap();
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let before = std::fs::read(&source).unwrap();
    for mode in ["missing", "wrong", "correct"] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "authenticated_vault_recovery_child",
                "--nocapture",
            ])
            .env("REDDB_TEST_VAULT_SOURCE", &source)
            .env("REDDB_TEST_VAULT_DEST", directory.path().join(mode))
            .env_remove("REDDB_TEST_VAULT_ACCEPT")
            .env_remove("REDDB_CERTIFICATE")
            .env_remove("REDDB_CERTIFICATE_FILE")
            .env_remove("REDDB_VAULT_PASSPHRASE")
            .env_remove("REDDB_VAULT_PASSPHRASE_FILE");
        if mode == "wrong" {
            child.env("REDDB_CERTIFICATE", KeyPair::generate().certificate_hex());
        }
        if mode == "correct" {
            child
                .env("REDDB_CERTIFICATE", &certificate)
                .env("REDDB_TEST_VAULT_ACCEPT", "1");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
    let mut damaged_vault = before.clone();
    damaged_vault[2 * 16384 + HEADER_SIZE + 60] ^= 1;
    let tampered = directory.path().join("tampered.rdb");
    std::fs::write(&tampered, &damaged_vault).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "authenticated_vault_recovery_child",
            "--nocapture",
        ])
        .env("REDDB_TEST_VAULT_SOURCE", &tampered)
        .env(
            "REDDB_TEST_VAULT_DEST",
            directory.path().join("tampered-output"),
        )
        .env_remove("REDDB_TEST_VAULT_ACCEPT")
        .env_remove("REDDB_CERTIFICATE_FILE")
        .env_remove("REDDB_VAULT_PASSPHRASE")
        .env_remove("REDDB_VAULT_PASSPHRASE_FILE")
        .env("REDDB_CERTIFICATE", &certificate)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tampered: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(std::fs::read(&tampered).unwrap(), damaged_vault);
}

#[test]
fn physical_scrub_detects_native_metadata_damage_without_writes() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let pager = Pager::open_default(&source).expect("pager");
    let data = reddb_file::encode_native_collection_roots_page(&std::collections::BTreeMap::new());
    let mut page = pager
        .allocate_page(PageType::NativeMeta)
        .expect("native metadata");
    let id = page.page_id();
    page.content_mut()[..data.len()].copy_from_slice(&data);
    pager.write_page(id, page).expect("write metadata");
    let mut physical = pager.physical_header().unwrap();
    physical.collection_roots_page = id;
    physical.collection_roots_checksum = reddb_file::native_store_page_checksum(&data);
    pager.update_physical_header(physical).unwrap();
    pager.sync().unwrap();
    drop(pager);
    let before = std::fs::read(&source).unwrap();
    let report = scrub_paged(&source).expect("scrub healthy metadata");
    assert!(report.assets_verified);
    assert!(report.trees.findings.is_empty());
    assert!(
        report.unclassified_pages >= 512,
        "abandoned allocations remain explicitly unclassified"
    );
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(id) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged = std::fs::read(&source).unwrap();
    assert!(scrub_paged(&source).is_err());
    assert_eq!(std::fs::read(&source).unwrap(), damaged);
}

#[test]
fn salvage_preserves_healthy_collection_and_reports_lost_data_page() {
    use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore};
    use reddb_types::Value;
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().expect("directory");
    let initial = directory.path().join("initial.rdb");
    {
        let store = UnifiedStore::open(&initial).expect("store");
        for (collection, first_id) in [("healthy", 1), ("damaged", 101)] {
            store.create_collection(collection).unwrap();
            for id in first_id..first_id + 5 {
                let entity = UnifiedEntity::table_row(
                    EntityId::new(id),
                    collection,
                    id,
                    vec![Value::Integer(id as i64)],
                );
                store.insert(collection, entity).unwrap();
            }
        }
        store.persist().expect("checkpoint");
    }
    let compacted = directory.path().join("input");
    compact_paged(&initial, &compacted).expect("prepare fully owned store");
    let source = compacted.join("initial.rdb");
    let pager = Pager::open(
        &source,
        PagerConfig {
            read_only: true,
            create: false,
            ..PagerConfig::default()
        },
    )
    .unwrap();
    let catalog = pager.read_page(1).unwrap();
    let header = reddb_file::decode_native_paged_metadata_header(catalog.content())
        .unwrap()
        .unwrap();
    let mut pos = reddb_file::METADATA_HEADER_BYTES;
    let mut root = 0;
    for _ in 0..header.collection_count {
        let entry =
            reddb_file::decode_native_paged_collection_root(catalog.content(), &mut pos).unwrap();
        if entry.collection == "damaged" {
            root = entry.root_page;
        }
    }
    assert_ne!(root, 0);
    assert_eq!(
        pager.read_page(root).unwrap().page_type().unwrap(),
        PageType::BTreeLeaf
    );
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let before = std::fs::read(&source).unwrap();
    let destination = directory.path().join("recovered");
    let report =
        salvage_paged_collections(&source, &destination).expect("recover healthy collection");
    assert_eq!(report.collections.len(), 1);
    assert_eq!(report.collections[0].collection, "damaged");
    assert_eq!(report.collections[0].root_page, root);
    let loss_report = std::fs::read_to_string(destination.join("salvage.json")).unwrap();
    assert!(loss_report.contains("\"skipped_entities\":null"));
    assert!(loss_report.contains("\"page_generation_freshness_verified\":false"));
    assert!(loss_report.contains("\"safe_for_writes\":false"));
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let output = destination.join("initial.rdb");
    let recovered_image = std::fs::read(&output).unwrap();
    let error = match UnifiedStore::open(&output) {
        Ok(_) => panic!("recovered legacy store must reject writable open"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("read-only"), "{error}");
    assert_eq!(std::fs::read(&output).unwrap(), recovered_image);
    let pager = Arc::new(
        Pager::open(
            &output,
            PagerConfig {
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )
        .expect("open fenced result read-only"),
    );
    assert_eq!(
        pager.header().unwrap().version,
        reddb_file::file_format::PAGE_RECOVERY_READ_ONLY_FILE_VERSION
    );
    assert!(
        reddb_file::file_format::PAGE_RECOVERY_READ_ONLY_FILE_VERSION
            > reddb_file::file_format::PAGE_SECONDARY_INDEX_FILE_VERSION
    );
    let catalog = pager.read_page(1).unwrap();
    let header = reddb_file::decode_native_paged_metadata_header(catalog.content())
        .unwrap()
        .unwrap();
    let mut pos = reddb_file::METADATA_HEADER_BYTES;
    let mut healthy_root = 0;
    let mut damaged_root = None;
    for _ in 0..header.collection_count {
        let entry =
            reddb_file::decode_native_paged_collection_root(catalog.content(), &mut pos).unwrap();
        if entry.collection == "healthy" {
            healthy_root = entry.root_page;
        }
        if entry.collection == "damaged" {
            damaged_root = Some(entry.root_page);
        }
    }
    assert_eq!(damaged_root, Some(0));
    let tree = BTree::with_root(pager, healthy_root);
    let mut cursor = tree.cursor_first().unwrap();
    let mut records = Vec::new();
    while let Some(record) = cursor.next_with_budget(1024 * 1024).unwrap() {
        records.push(record);
    }
    assert_eq!(records.len(), 5);
    let bytes = records
        .into_iter()
        .find(|(key, _)| key == &1u64.to_be_bytes())
        .unwrap()
        .1;
    let (entity, _) = reddb_engine::storage::collection_read::EncodedRecord {
        id: EntityId::new(1),
        bytes,
    }
    .decode(header.format_version)
    .unwrap();
    let reddb_server::storage::EntityData::Row(row) = entity.data else {
        panic!("expected row")
    };
    assert_eq!(row.columns[0], Value::Integer(1));
    drop(cursor);
    drop(tree);
    for copy in 0..reddb_file::PAGED_SUPERBLOCK_SLOT_COUNT {
        let path = directory.path().join(format!("one-superblock-{copy}.rdb"));
        let mut bytes = recovered_image.clone();
        bytes[copy * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE + 40] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let error = match Pager::open(
            &path,
            PagerConfig {
                disk_residency: true,
                ..PagerConfig::default()
            },
        ) {
            Ok(_) => panic!("single surviving fence must refuse a writer"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("read-only"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn ordinary_format_upgrade_cannot_create_recovery_only_version() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    let pager = Pager::open_default(&source).unwrap();
    let initial = pager.header().unwrap().version;
    let error = pager
        .fence_file_format_version(reddb_file::file_format::PAGE_RECOVERY_READ_ONLY_FILE_VERSION)
        .expect_err("recovery fence must not be available as a normal upgrade");
    assert!(error.to_string().contains("reserved"), "{error}");
    assert_eq!(pager.header().unwrap().version, initial);
    pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("ordinary writer remains usable");
}

#[test]
fn collection_salvage_refuses_a_damaged_interior_root() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let pager = Pager::open_default(&source).unwrap();
    let catalog = pager.read_page(1).unwrap();
    let mut pos = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut pos)
        .unwrap()
        .root_page;
    assert_eq!(
        pager.read_page(root).unwrap().page_type().unwrap(),
        PageType::BTreeInterior
    );
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let before = std::fs::read(&source).unwrap();
    let destination = directory.path().join("rejected");
    let error =
        salvage_paged_collections(&source, &destination).expect_err("cannot classify descendants");
    assert!(error.to_string().contains("leaf-root"), "{error}");
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
}

#[test]
fn salvage_recovers_readable_leaves_of_a_multi_page_collection() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().unwrap();
    let initial = directory.path().join("source.rdb");
    fixture(&initial);
    let compacted = directory.path().join("compacted");
    compact_paged(&initial, &compacted).unwrap();
    let source = compacted.join("source.rdb");
    let pager = Arc::new(Pager::open_default(&source).unwrap());
    let catalog = pager.read_page(1).unwrap();
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset)
        .unwrap()
        .root_page;
    let tree = BTree::with_root(Arc::clone(&pager), root);
    let leaves = structural_leaves(&pager, &tree);
    assert!(leaves.len() > 2);
    let (damaged_id, lost) = leaves
        .iter()
        .find_map(|id| {
            let page = pager.read_page(*id).unwrap();
            let mut keys = Vec::new();
            for i in 0..usize::from(page.cell_count()) {
                let bytes = page.as_bytes();
                let slot = HEADER_SIZE + 8 + i * 2;
                let cell = u16::from_le_bytes([bytes[slot], bytes[slot + 1]]) as usize;
                let key_len = u16::from_le_bytes([bytes[cell], bytes[cell + 1]]) as usize;
                assert_eq!(key_len, 8);
                keys.push(u64::from_be_bytes(
                    bytes[cell + 4..cell + 12].try_into().unwrap(),
                ));
            }
            (!keys.is_empty() && keys.iter().all(|key| key % 100 != 0)).then_some((*id, keys))
        })
        .expect("leaf without overflow values");
    let overflow_leaf = leaves
        .iter()
        .copied()
        .find(|id| {
            let page = pager.read_page(*id).unwrap();
            (0..usize::from(page.cell_count())).any(|i| {
                let bytes = page.as_bytes();
                let slot = HEADER_SIZE + 8 + i * 2;
                let cell = u16::from_le_bytes([bytes[slot], bytes[slot + 1]]) as usize;
                let key = u64::from_be_bytes(bytes[cell + 4..cell + 12].try_into().unwrap());
                key % 100 == 0
            })
        })
        .expect("leaf with overflow value");
    assert!(!lost.is_empty() && lost.len() < 1000);
    drop(tree);
    drop(pager);
    let clean = std::fs::read(&source).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(damaged_id) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let original = std::fs::read(&source).unwrap();
    let recovered = directory.path().join("recovered-leaves");
    let report = salvage_paged_collections(&source, &recovered).expect("recover intact leaves");
    assert_eq!(report.collections.len(), 1);
    assert!(report.machine_json().contains(&damaged_id.to_string()));
    assert_eq!(std::fs::read(&source).unwrap(), original);
    let output = recovered.join("source.rdb");
    assert!(Pager::open_default(&output).is_err());
    let pager = Arc::new(
        Pager::open(
            &output,
            PagerConfig {
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )
        .unwrap(),
    );
    let catalog = pager.read_page(1).unwrap();
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset)
        .unwrap()
        .root_page;
    let tree = BTree::with_root(pager, root);
    for id in 0u64..1000 {
        assert_eq!(
            tree.get(&id.to_be_bytes()).unwrap(),
            if lost.contains(&id) {
                None
            } else {
                Some(payload(id))
            },
            "record {id}"
        );
    }
    let overflow_source = directory.path().join("overflow.rdb");
    let mut ambiguous = clean;
    ambiguous[overflow_leaf as usize * 16384 + 100] ^= 1;
    std::fs::write(&overflow_source, &ambiguous).unwrap();
    let refused = directory.path().join("unclassified-overflow");
    let error = salvage_paged_collections(&overflow_source, &refused)
        .expect_err("unknown overflow ownership must be refused");
    assert!(error.to_string().contains("unclassified"), "{error}");
    assert!(!refused.join("overflow.rdb").exists());
    assert_eq!(std::fs::read(&overflow_source).unwrap(), ambiguous);
}

#[test]
fn salvage_recovers_tree_with_multiple_interior_levels() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("deep.rdb");
    let key = |id: u64| {
        let mut value = id.to_be_bytes().to_vec();
        value.extend_from_slice(&[0x5a; 800]);
        value
    };
    {
        let pager = Arc::new(Pager::open_default(&source).unwrap());
        let tree = BTree::new(Arc::clone(&pager));
        for id in 0u64..800 {
            tree.insert(&key(id), &id.to_le_bytes()).unwrap();
        }
        let mut metadata = Vec::new();
        reddb_file::encode_native_paged_metadata_header(
            &mut metadata,
            reddb_file::NativePagedMetadataHeader {
                format_version: 11,
                collection_count: 1,
            },
        );
        reddb_file::encode_native_paged_collection_root(&mut metadata, "rows", tree.root_page_id());
        metadata.extend_from_slice(&0u32.to_le_bytes());
        let mut page = Page::new(PageType::Header, 1);
        page.content_mut()[..metadata.len()].copy_from_slice(&metadata);
        pager.write_page(1, page).unwrap();
        pager.sync().unwrap();
    }
    let compacted = directory.path().join("deep-compacted");
    compact_paged(&source, &compacted).unwrap();
    let source = compacted.join("deep.rdb");
    let pager = Arc::new(Pager::open_default(&source).unwrap());
    let catalog = pager.read_page(1).unwrap();
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset)
        .unwrap()
        .root_page;
    let tree = BTree::with_root(Arc::clone(&pager), root);
    let pages = tree.retired_pages().unwrap();
    assert!(
        pages
            .iter()
            .filter(
                |id| pager.read_page(**id).unwrap().page_type().unwrap() == PageType::BTreeInterior
            )
            .count()
            > 1
    );
    let leaves = structural_leaves(&pager, &tree);
    let damaged = leaves[leaves.len() / 2];
    let page = pager.read_page(damaged).unwrap();
    let lost: Vec<u64> = (0..usize::from(page.cell_count()))
        .map(|i| {
            let bytes = page.as_bytes();
            let slot = HEADER_SIZE + 8 + i * 2;
            let cell = u16::from_le_bytes([bytes[slot], bytes[slot + 1]]) as usize;
            u64::from_be_bytes(bytes[cell + 4..cell + 12].try_into().unwrap())
        })
        .collect();
    drop(tree);
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(damaged) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let before = std::fs::read(&source).unwrap();
    let output = directory.path().join("deep-recovered");
    salvage_paged_collections(&source, &output).unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let pager = Arc::new(
        Pager::open(
            output.join("deep.rdb"),
            PagerConfig {
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )
        .unwrap(),
    );
    let catalog = pager.read_page(1).unwrap();
    let mut offset = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(catalog.content(), &mut offset)
        .unwrap()
        .root_page;
    let tree = BTree::with_root(pager, root);
    for id in 0u64..800 {
        assert_eq!(
            tree.get(&key(id)).unwrap(),
            if lost.contains(&id) {
                None
            } else {
                Some(id.to_le_bytes().to_vec())
            }
        );
    }
}

#[test]
fn collection_salvage_refuses_catalog_cross_references() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let pager = Pager::open_default(&source).unwrap();
    let page = pager.read_page(1).unwrap();
    let mut pos = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(page.content(), &mut pos).unwrap();
    let mut metadata = Vec::new();
    reddb_file::encode_native_paged_metadata_header(
        &mut metadata,
        reddb_file::NativePagedMetadataHeader {
            format_version: 11,
            collection_count: 1,
        },
    );
    reddb_file::encode_native_paged_collection_root(&mut metadata, "rows", root.root_page);
    metadata.extend_from_slice(&1u32.to_le_bytes());
    reddb_file::encode_native_paged_cross_ref(&mut metadata, 1, 2, 0, "rows");
    let mut catalog = Page::new(PageType::Header, 1);
    catalog.content_mut()[..metadata.len()].copy_from_slice(&metadata);
    pager.write_page(1, catalog).unwrap();
    pager.sync().unwrap();
    drop(pager);
    let before = std::fs::read(&source).unwrap();
    let destination = directory.path().join("rejected");
    let error = salvage_paged_collections(&source, &destination)
        .expect_err("cross references require separate recovery");
    assert!(error.to_string().contains("reference-free"), "{error}");
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
}

#[test]
fn collection_salvage_never_discards_damaged_system_collections() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let pager = Pager::open_default(&source).unwrap();
    let page = pager.read_page(1).unwrap();
    let mut pos = reddb_file::METADATA_HEADER_BYTES;
    let root = reddb_file::decode_native_paged_collection_root(page.content(), &mut pos)
        .unwrap()
        .root_page;
    let mut metadata = Vec::new();
    reddb_file::encode_native_paged_metadata_header(
        &mut metadata,
        reddb_file::NativePagedMetadataHeader {
            format_version: 11,
            collection_count: 1,
        },
    );
    reddb_file::encode_native_paged_collection_root(&mut metadata, "red_config", root);
    metadata.extend_from_slice(&0u32.to_le_bytes());
    let mut catalog = Page::new(PageType::Header, 1);
    catalog.content_mut()[..metadata.len()].copy_from_slice(&metadata);
    pager.write_page(1, catalog).unwrap();
    pager.sync().unwrap();
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let before = std::fs::read(&source).unwrap();
    let destination = directory.path().join("rejected");
    let error =
        salvage_paged_collections(&source, &destination).expect_err("system collection damage");
    assert!(error.to_string().contains("system collections"), "{error}");
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
}

#[test]
fn compact_rejects_pending_wal_and_does_not_publish_corruption() {
    use std::io::{Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let wal = reddb_file::unified_wal_path(&source);
    let mut pending = reddb_file::encode_wal_file_header().to_vec();
    pending.extend_from_slice(b"uncheckpointed");
    std::fs::write(&wal, pending).expect("pending WAL");
    assert!(scrub_paged_trees(&source)
        .expect_err("pending WAL")
        .to_string()
        .contains("WAL"));
    let destination = directory.path().join("compacted");
    assert!(salvage_paged_indexes(&source, &destination).is_err());
    assert!(compact_paged(&source, &destination)
        .expect_err("pending WAL")
        .to_string()
        .contains("WAL"));
    assert!(!destination.exists());
    std::fs::remove_file(&wal).expect("fixture WAL");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .expect("fixture corruption");
    file.seek(SeekFrom::Start(16384 + 100)).expect("seek");
    file.write_all(&[0xFF]).expect("corrupt catalog");
    drop(file);
    assert!(scrub_paged_trees(&source).is_err());
    assert!(compact_paged(&source, &destination).is_err());
    assert!(!destination.join("source.rdb").exists());
}

#[test]
fn offline_recovery_refuses_real_committed_and_incomplete_wal_without_modifying_source() {
    use reddb_server::storage::wal::{WalRecord, WalWriter};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let source_image = std::fs::read(&source).unwrap();
    for (name, path) in [
        ("unified", reddb_file::unified_wal_path(&source)),
        ("engine", reddb_file::engine_wal_path(&source)),
        ("legacy", reddb_file::pager_legacy_wal_path(&source)),
    ] {
        for committed in [false, true] {
            {
                let mut writer = WalWriter::open(&path).unwrap();
                writer.append(&WalRecord::Begin { tx_id: 42 }).unwrap();
                writer
                    .append(&WalRecord::PageWrite {
                        tx_id: 42,
                        page_id: 1,
                        data: vec![42; 32],
                    })
                    .unwrap();
                if committed {
                    writer.append(&WalRecord::Commit { tx_id: 42 }).unwrap();
                }
                writer.sync().unwrap();
            }
            let wal_image = std::fs::read(&path).unwrap();
            let output = directory.path().join(format!("{name}-{committed}"));
            assert!(scrub_paged(&source)
                .expect_err("pending WAL")
                .to_string()
                .contains("WAL"));
            assert!(salvage_paged_indexes(&source, &output)
                .expect_err("pending WAL")
                .to_string()
                .contains("WAL"));
            assert!(!output.exists());
            assert_eq!(std::fs::read(&path).unwrap(), wal_image);
            assert_eq!(std::fs::read(&source).unwrap(), source_image);
            std::fs::remove_file(&path).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn offline_tools_use_real_source_sidecars_for_symlink_paths() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    let alias = directory.path().join("alias.rdb");
    fixture(&source);
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    let mut pending = reddb_file::encode_wal_file_header().to_vec();
    pending.extend_from_slice(b"uncheckpointed");
    std::fs::write(reddb_file::unified_wal_path(&source), pending).unwrap();
    let before = std::fs::read(&source).unwrap();
    assert!(scrub_paged_trees(&alias).is_err());
    assert!(scrub_paged(&alias).is_err());
    let destination = directory.path().join("recovered");
    assert!(salvage_paged_indexes(&alias, &destination).is_err());
    assert!(!destination.exists());
    assert_eq!(std::fs::read(&source).unwrap(), before);
}

#[test]
fn compact_real_runtime_preserves_resume_indexes_and_subsequent_writes() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).expect("native runtime");
        runtime
            .execute_query("CREATE TABLE records (id INT, value TEXT)")
            .expect("table");
        runtime
            .execute_query("CREATE INDEX record_value ON records (value) USING HASH")
            .expect("index");
        for id in 0..80 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO records (id, value) VALUES ({id}, 'value-{id}')"
                ))
                .expect("insert");
        }
        runtime
            .execute_query("UPDATE records SET value = 'changed' WHERE id = 10")
            .expect("update version");
        runtime
            .execute_query("DELETE FROM records WHERE id = 20")
            .expect("delete version");
        runtime
            .graceful_shutdown(false)
            .expect("shutdown before compaction");
    }
    let report = scrub_paged_trees(&source).expect("scrub real runtime trees");
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    assert!(report.trees_verified >= 2, "collection and secondary index");
    let destination = directory.path().join("compacted");
    compact_paged(&source, &destination).expect("compact actual runtime");
    let options = RedDBOptions::persistent(destination.join("source.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for round in 0..2 {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("reopen runtime");
        let count = runtime
            .execute_query("SELECT COUNT(*) AS n FROM records")
            .expect("count");
        assert_eq!(
            count.result.records[0]
                .get("n")
                .expect("n")
                .display_string(),
            if round == 0 { "79" } else { "80" }
        );
        let indexed = runtime
            .execute_query("SELECT id FROM records WHERE value = 'changed'")
            .expect("indexed lookup");
        assert_eq!(indexed.result.records.len(), 1);
        assert_eq!(
            indexed.result.records[0]
                .get("id")
                .expect("id")
                .display_string(),
            "10"
        );
        if round == 0 {
            runtime
                .execute_query("INSERT INTO records (id, value) VALUES (99, 'after')")
                .expect("post-compact write");
        }
        runtime
            .graceful_shutdown(false)
            .expect("post-compact shutdown");
    }
}

#[test]
fn salvage_rebuilds_corrupt_secondary_index_from_verified_rows() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    use std::io::{Read, Seek, SeekFrom, Write};

    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).expect("runtime");
        runtime
            .execute_query("CREATE TABLE records (id INT, value TEXT)")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX record_value ON records (value) USING HASH")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX record_id ON records (id) USING HASH")
            .unwrap();
        for id in 0..80 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO records (id, value) VALUES ({id}, 'value-{id}')"
                ))
                .unwrap();
        }
        runtime
            .execute_query("UPDATE records SET value = 'changed' WHERE id = 10")
            .unwrap();
        runtime
            .execute_query("DELETE FROM records WHERE id = 20")
            .unwrap();
        runtime.graceful_shutdown(false).expect("checkpoint");
    }
    let pager = Pager::open(
        &source,
        PagerConfig {
            read_only: true,
            create: false,
            ..PagerConfig::default()
        },
    )
    .expect("pager");
    let healthy = scrub_paged(&source).expect("physical scrub");
    assert!(healthy.assets_verified);
    assert!(healthy.trees.findings.is_empty());
    let catalog = pager.read_page(1).expect("catalog");
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .expect("decode catalog")
        .expect("resume");
    let root = resume.collections["records"].secondary_indices["record_value"].root_page;
    assert_ne!(root, 0);
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(root) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged = std::fs::read(&source).unwrap();
    let scrub = scrub_paged(&source).expect("scrub damaged index");
    assert!(!scrub.assets_verified);
    assert_eq!(scrub.trees.findings.len(), 1);
    assert_eq!(std::fs::read(&source).unwrap(), damaged);
    let nested = source.with_file_name("source.rdb.ops").join("recovered");
    reddb_file::OperationalManifest::for_db_path(&source)
        .recover_or_bootstrap(&["records".into()])
        .expect("legacy operational manifest fixture");
    let error = salvage_paged_indexes(&source, &nested).expect_err("destination inside source");
    assert!(
        error.to_string().contains("destination is inside source"),
        "{error}"
    );
    assert!(!nested.exists());
    let destination = directory.path().join("recovered");
    let report = salvage_paged_indexes(&source, &destination).expect("recover index");
    assert_eq!(report.indexes.len(), 1);
    assert_eq!(report.indexes[0].collection, "records");
    assert_eq!(report.indexes[0].index, "record_value");
    let saved = std::fs::read_to_string(destination.join("salvage.json")).expect("loss report");
    assert!(saved.contains("record_value"));
    assert!(saved.contains("discarded_for_rebuild"));
    assert!(saved.contains("\"page_generation_freshness_verified\":false"));
    assert_eq!(std::fs::read(&source).unwrap(), damaged);
    let recovered = RedDBOptions::persistent(destination.join("source.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for round in 0..2 {
        let runtime = RedDBRuntime::with_options(recovered.clone()).expect("open recovered store");
        let count = runtime
            .execute_query("SELECT COUNT(*) AS n FROM records")
            .unwrap();
        assert_eq!(
            count.result.records[0].get("n").unwrap().display_string(),
            "79"
        );
        let row = runtime
            .execute_query("SELECT id FROM records WHERE value = 'changed'")
            .unwrap();
        assert_eq!(row.result.records.len(), 1);
        assert_eq!(
            row.result.records[0].get("id").unwrap().display_string(),
            "10"
        );
        let explain = runtime
            .explain_query("SELECT id FROM records WHERE value = 'changed'")
            .expect("explain recovered index");
        let mut pending = vec![&explain.logical_plan.root];
        let mut found_index = false;
        while let Some(node) = pending.pop() {
            found_index |= node.operator == "index_seek";
            pending.extend(&node.children);
        }
        assert!(
            found_index,
            "recovered query should choose an index seek: {:?}",
            explain.logical_plan.root
        );
        assert!(runtime
            .execute_query("SELECT id FROM records WHERE value = 'value-20'")
            .unwrap()
            .result
            .records
            .is_empty());
        runtime
            .graceful_shutdown(false)
            .expect("checkpoint recovered store");
        drop(runtime);
        if round == 0 {
            let pager = Pager::open(
                destination.join("source.rdb"),
                PagerConfig {
                    read_only: true,
                    create: false,
                    ..PagerConfig::default()
                },
            )
            .expect("read recovered catalog");
            let page = pager.read_page(1).expect("catalog");
            let resume = reddb_file::native_resume::decode_catalog_resume(page.content())
                .unwrap()
                .expect("resume");
            assert!(
                resume.collections["records"]
                    .secondary_indices
                    .contains_key("record_value"),
                "index should be rebuilt before publication"
            );
            assert!(resume.collections["records"]
                .secondary_indices
                .contains_key("record_id"));
        }
    }

    // A damaged operational manifest must not be copied as if it were verified.
    let manifest = source
        .with_file_name("source.rdb.ops")
        .join("manifest.json");
    let manifest_bytes = std::fs::read(&manifest).unwrap();
    let mut bad_manifest = manifest_bytes.clone();
    let checksum = bad_manifest
        .windows(b"checksum".len())
        .position(|part| part == b"checksum")
        .unwrap();
    let digit = (checksum + b"checksum".len()..bad_manifest.len())
        .find(|&i| bad_manifest[i].is_ascii_hexdigit())
        .unwrap();
    bad_manifest[digit] = if bad_manifest[digit] == b'0' {
        b'1'
    } else {
        b'0'
    };
    std::fs::write(&manifest, bad_manifest).unwrap();
    let rejected = directory.path().join("rejected-manifest");
    assert!(salvage_paged_indexes(&source, &rejected).is_err());
    assert!(!rejected.exists());
    std::fs::write(&manifest, manifest_bytes).unwrap();

    // A valid page paired with the wrong published digest is also unsafe.
    let pager = Pager::open(
        &source,
        PagerConfig {
            disk_residency: true,
            ..PagerConfig::default()
        },
    )
    .expect("open for metadata fault injection");
    let mut physical = pager.physical_header().unwrap();
    assert_ne!(physical.manifest_page, 0);
    physical.manifest_checksum ^= 1;
    pager.update_physical_header(physical).unwrap();
    pager.sync().unwrap();
    drop(pager);
    let rejected = directory.path().join("rejected-metadata");
    assert!(scrub_paged(&source).is_err());
    let err = salvage_paged_indexes(&source, &rejected).expect_err("metadata digest mismatch");
    assert!(err.to_string().contains("metadata"), "{err}");
    assert!(!rejected.exists());
    let pager = Pager::open(
        &source,
        PagerConfig {
            disk_residency: true,
            ..PagerConfig::default()
        },
    )
    .expect("restore metadata");
    physical.manifest_checksum ^= 1;
    pager.update_physical_header(physical).unwrap();
    pager.sync().unwrap();
    drop(pager);

    // An allocation with unknown ownership must not be silently discarded.
    let pager = Pager::open(
        &source,
        PagerConfig {
            disk_residency: true,
            ..PagerConfig::default()
        },
    )
    .expect("allocate unclassified page");
    let page = pager.allocate_page(PageType::Overflow).unwrap();
    pager.write_page(page.page_id(), page).unwrap();
    pager.sync().unwrap();
    drop(pager);
    let unclassified = std::fs::read(&source).unwrap();
    let rejected = directory.path().join("rejected-unclassified");
    let err = salvage_paged_indexes(&source, &rejected).expect_err("unclassified page");
    assert!(err.to_string().contains("unclassified"), "{err}");
    assert!(!rejected.join("source.rdb").exists());
    assert_eq!(std::fs::read(&source).unwrap(), unclassified);

    // A second damaged authority must not be silently repaired or omitted
    // from a report that only describes index recovery.
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(100)).unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let damaged_authority = std::fs::read(&source).unwrap();
    let rejected = directory.path().join("rejected-superblock");
    assert!(salvage_paged_indexes(&source, &rejected).is_err());
    assert!(!rejected.exists());
    assert_eq!(std::fs::read(&source).unwrap(), damaged_authority);
}

#[test]
fn salvage_rebuilds_a_multi_page_secondary_index_after_leaf_damage() {
    use reddb_server::{RedDBOptions, RedDBRuntime};
    use std::io::{Read, Seek, SeekFrom, Write};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.rdb");
    let options = RedDBOptions::persistent(&source)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options).unwrap();
        runtime
            .execute_query("CREATE TABLE records (id INT, value TEXT)")
            .unwrap();
        runtime
            .execute_query("CREATE INDEX record_value ON records (value) USING HASH")
            .unwrap();
        for start in (0..1500).step_by(300) {
            let tuples = (start..start + 300)
                .map(|id| format!("({id}, 'value-{id:04}')"))
                .collect::<Vec<_>>()
                .join(",");
            runtime
                .execute_query(&format!("INSERT INTO records (id, value) VALUES {tuples}"))
                .unwrap();
        }
        runtime.graceful_shutdown(false).unwrap();
    }
    let pager = Arc::new(
        Pager::open(
            &source,
            PagerConfig {
                read_only: true,
                create: false,
                ..PagerConfig::default()
            },
        )
        .unwrap(),
    );
    let catalog = pager.read_page(1).unwrap();
    let resume = reddb_file::native_resume::decode_catalog_resume(catalog.content())
        .unwrap()
        .unwrap();
    let root = resume.collections["records"].secondary_indices["record_value"].root_page;
    assert_eq!(
        pager.read_page(root).unwrap().page_type().unwrap(),
        PageType::BTreeInterior
    );
    let tree = BTree::with_root(Arc::clone(&pager), root);
    let leaves = structural_leaves(&pager, &tree);
    assert!(leaves.len() > 1);
    let leaf = leaves[leaves.len() / 2];
    drop(tree);
    drop(pager);
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&source)
        .unwrap();
    file.seek(SeekFrom::Start(u64::from(leaf) * 16384 + 100))
        .unwrap();
    let mut byte = [0];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Current(-1)).unwrap();
    file.write_all(&[byte[0] ^ 1]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let before = std::fs::read(&source).unwrap();
    let output = directory.path().join("recovered");
    let report = salvage_paged_indexes(&source, &output).expect("recover multi-page index");
    assert_eq!(report.indexes.len(), 1);
    assert_eq!(std::fs::read(&source).unwrap(), before);
    let recovered = RedDBOptions::persistent(output.join("source.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    let runtime = RedDBRuntime::with_options(recovered).unwrap();
    let count = runtime
        .execute_query("SELECT COUNT(*) AS n FROM records")
        .unwrap();
    assert_eq!(
        count.result.records[0].get("n").unwrap().display_string(),
        "1500"
    );
    let row = runtime
        .execute_query("SELECT id FROM records WHERE value = 'value-0777'")
        .unwrap();
    assert_eq!(row.result.records.len(), 1);
    assert_eq!(
        row.result.records[0].get("id").unwrap().display_string(),
        "777"
    );
    runtime.graceful_shutdown(false).unwrap();
}

#[test]
fn compact_child_process() {
    let Some(source) = std::env::var_os("REDDB_COMPACTION_TEST_SOURCE") else {
        return;
    };
    let destination = std::env::var_os("REDDB_COMPACTION_TEST_DESTINATION").expect("destination");
    compact_paged(
        std::path::Path::new(&source),
        std::path::Path::new(&destination),
    )
    .expect("child compaction");
}

#[test]
fn compact_process_loss_preserves_original_and_does_not_publish_partial_file() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let before = std::fs::read(&source).expect("source bytes");
    let destination = directory.path().join("interrupted");
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "compact_child_process", "--nocapture"])
        .env("REDDB_COMPACTION_TEST_SOURCE", &source)
        .env("REDDB_COMPACTION_TEST_DESTINATION", &destination)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("child");
    let started = std::time::Instant::now();
    loop {
        if std::fs::metadata(destination.join(".compacting.rdb"))
            .is_ok_and(|metadata| metadata.len() > 1280 * 1024)
        {
            break;
        }
        assert!(
            child.try_wait().expect("status").is_none(),
            "child exited before crash injection"
        );
        if started.elapsed() > std::time::Duration::from_secs(30) {
            child.kill().expect("timeout kill");
            child.wait().expect("reap");
            panic!("child did not reach rewrite");
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    child.kill().expect("forced process loss");
    child.wait().expect("reap killed child");
    assert!(
        !destination.join("source.rdb").exists(),
        "incomplete database must not be published"
    );
    assert_eq!(std::fs::read(&source).expect("source after crash"), before);
    compact_paged(&source, &directory.path().join("retry")).expect("retry after process loss");
}

#[test]
fn compact_preserves_overflow_catalog_and_empty_collections() {
    use reddb_server::storage::UnifiedStore;
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    {
        let store = UnifiedStore::open(&source).expect("store");
        for id in 0..700 {
            store
                .create_collection(format!(
                    "collection_{id:04}_with_long_names_for_catalog_overflow"
                ))
                .expect("collection");
        }
        store.persist().expect("catalog checkpoint");
    }
    let destination = directory.path().join("compacted");
    compact_paged(&source, &destination).expect("compact overflow catalog");
    let store = UnifiedStore::open(destination.join("source.rdb")).expect("reopen catalog");
    assert_eq!(store.list_collections().len(), 700);
    store.persist().expect("checkpoint after relocation");
}

#[test]
fn compact_rejects_unknown_catalog_extension_and_preserves_bytes() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    {
        let pager = Pager::open_default(&source).expect("pager");
        let mut page = pager.read_page(1).expect("catalog");
        page.content_mut()[28..32].copy_from_slice(b"FUTR");
        pager.write_page(1, page).expect("future catalog extension");
        pager.sync().expect("sync");
    }
    let before = std::fs::read(&source).expect("source");
    let destination = directory.path().join("compacted");
    assert!(compact_paged(&source, &destination)
        .expect_err("unsupported extension")
        .to_string()
        .contains("unknown catalog extension"));
    assert_eq!(std::fs::read(&source).expect("preserved source"), before);
    assert!(!destination.exists());
}

#[test]
fn compact_repeat_has_stable_size() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let first = directory.path().join("first");
    let report = compact_paged(&source, &first).expect("first compaction");
    let second = directory.path().join("second");
    let repeated = compact_paged(&first.join("source.rdb"), &second).expect("repeated compaction");
    assert_eq!(report.destination_bytes, repeated.destination_bytes);
    assert_eq!(report.records, repeated.records);
    // st_blocks includes filesystem extent metadata: the same dense file can
    // occupy different block counts on a fragmented filesystem. Require exact
    // database size/record stability above, and actual space reclamation here.
    for compacted in [&report, &repeated] {
        if let (Some(original), Some(allocated)) = (
            report.source_allocated_bytes,
            compacted.destination_allocated_bytes,
        ) {
            assert!(
                allocated < original,
                "compaction must reclaim abandoned pages"
            );
        }
    }
    eprintln!("compaction size evidence: {}", report.machine_json());
}

#[test]
fn compact_preserves_empty_support_scaffolding_but_refuses_hidden_wal() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    fixture(&source);
    let support = reddb_file::support_dir_for(&source);
    std::fs::create_dir_all(support.join("indexes")).expect("index scaffold");
    std::fs::create_dir_all(support.join("snapshots")).expect("snapshot scaffold");
    let destination = directory.path().join("compacted");
    compact_paged(&source, &destination).expect("empty support scaffold");
    let copied = reddb_file::support_dir_for(&destination.join("source.rdb"));
    assert!(copied.join("indexes").is_dir());
    assert!(copied.join("snapshots").is_dir());
    std::fs::create_dir_all(support.join("wal")).expect("tiered WAL directory");
    let wal = reddb_file::unified_wal_path_in(&support, &source);
    std::fs::write(wal, b"uncheckpointed").expect("hidden WAL");
    let rejected = directory.path().join("rejected");
    assert!(compact_paged(&source, &rejected)
        .expect_err("non-empty support layout")
        .to_string()
        .contains("non-empty support layout"));
    assert!(!rejected.exists());
}

fn structural_leaves(pager: &Arc<Pager>, tree: &BTree) -> Vec<u32> {
    tree.retired_pages()
        .expect("structural inventory")
        .into_iter()
        .rev()
        .filter(|id| {
            pager
                .read_page(*id)
                .expect("page")
                .page_type()
                .expect("type")
                == PageType::BTreeLeaf
        })
        .collect()
}

fn assert_leaf_links(pager: &Arc<Pager>, tree: &BTree) {
    let leaves = structural_leaves(pager, tree);
    assert!(leaves.len() > 3, "exercise non-rightmost splits");
    for (index, id) in leaves.iter().enumerate() {
        let page = pager.read_page(*id).expect("leaf");
        let previous = u32::from_le_bytes(page.content()[..4].try_into().expect("previous"));
        let next = u32::from_le_bytes(page.content()[4..8].try_into().expect("next"));
        assert_eq!(
            previous,
            index.checked_sub(1).map_or(0, |i| leaves[i]),
            "previous link at {id}"
        );
        assert_eq!(
            next,
            leaves.get(index + 1).copied().unwrap_or(0),
            "next link at {id}"
        );
    }
}

#[test]
fn non_rightmost_splits_preserve_bidirectional_links_after_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let source = directory.path().join("source.rdb");
    let pager = Arc::new(Pager::open_default(&source).expect("pager"));
    let tree = BTree::new(Arc::clone(&pager));
    for id in (0u64..256).rev() {
        tree.insert(
            &id.to_be_bytes(),
            &vec![u8::try_from(id).expect("fixture byte"); 800],
        )
        .expect("descending insert");
    }
    assert_leaf_links(&pager, &tree);
    for id in (0u64..256).step_by(3) {
        tree.delete(&id.to_be_bytes())
            .expect("delete and rebalance");
    }
    assert_leaf_links(&pager, &tree);
    let root = tree.root_page_id();
    pager.sync().expect("sync");
    drop(tree);
    drop(pager);
    let pager = Arc::new(Pager::open_default(&source).expect("reopen"));
    let tree = BTree::with_root(Arc::clone(&pager), root);
    assert_leaf_links(&pager, &tree);
    for id in 0u64..256 {
        assert_eq!(
            tree.get(&id.to_be_bytes()).expect("lookup"),
            (!id.is_multiple_of(3)).then(|| vec![u8::try_from(id).expect("fixture byte"); 800])
        );
    }
}

#[test]
fn compact_repairs_historical_backlinks_but_refuses_forward_omissions() {
    for forward in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let source = directory.path().join("source.rdb");
        fixture(&source);
        {
            let pager = Arc::new(Pager::open_default(&source).expect("pager"));
            let page = pager.read_page(1).expect("catalog");
            let mut offset = reddb_file::METADATA_HEADER_BYTES;
            let root = reddb_file::decode_native_paged_collection_root(page.content(), &mut offset)
                .expect("root");
            let tree = BTree::with_root(Arc::clone(&pager), root.root_page);
            let leaves = structural_leaves(&pager, &tree);
            assert!(leaves.len() > 3);
            // Historical split: A -> B -> C is intact, but C still points to A.
            // A forward skip must instead fail, even if the leaf checksums pass.
            let (id, offset, link) = if forward {
                (leaves[0], 4, leaves[2])
            } else {
                (leaves[2], 0, leaves[0])
            };
            let mut page = pager.read_page(id).expect("leaf");
            page.content_mut()[offset..offset + 4].copy_from_slice(&link.to_le_bytes());
            pager.write_page(id, page).expect("historical link");
            pager.sync().expect("sync");
        }
        let before = std::fs::read(&source).expect("source bytes");
        let destination = directory.path().join("compacted");
        if forward {
            let error = compact_paged(&source, &destination).expect_err("forward omission");
            assert!(error.to_string().contains("leaf chain"));
            assert!(!destination.join("source.rdb").exists());
        } else {
            let report = compact_paged(&source, &destination)
                .expect("repair independently verified backlink");
            assert_eq!(report.records, 1000);
            assert!(report
                .machine_json()
                .contains("\"repaired_leaf_backlinks\":1"));
            let pager =
                Arc::new(Pager::open_default(destination.join("source.rdb")).expect("reopen"));
            let page = pager.read_page(1).expect("catalog");
            let mut offset = reddb_file::METADATA_HEADER_BYTES;
            let root = reddb_file::decode_native_paged_collection_root(page.content(), &mut offset)
                .expect("root");
            let tree = BTree::with_root(Arc::clone(&pager), root.root_page);
            assert_leaf_links(&pager, &tree);
            for id in 0u64..1000 {
                assert_eq!(
                    tree.get(&id.to_be_bytes()).expect("lookup"),
                    Some(payload(id))
                );
            }
        }
        assert_eq!(std::fs::read(&source).expect("unchanged source"), before);
    }
}

#[test]
fn compact_relocates_internal_vector_metadata_without_sidecars() {
    use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
    let directory = tempfile::tempdir().expect("fixture");
    let source = directory.path().join("vectors.rdb");
    let options = |path| {
        RedDBOptions::persistent(path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("native profile")
            .with_memory_budget(64 * 1024 * 1024)
            .with_bounded_disk_residency(false)
    };
    {
        let runtime = RedDBRuntime::with_options(options(&source)).expect("runtime");
        runtime
            .execute_query("CREATE VECTOR embeddings DIM 2 METRIC cosine")
            .expect("vector collection");
        runtime
            .execute_query(
                "INSERT INTO embeddings VECTOR (embedding, content) VALUES ([1, 0], 'kept')",
            )
            .expect("vector");
        runtime
            .execute_query("VECTOR SEARCH embeddings SIMILAR TO [1,0] LIMIT 1")
            .expect("build index");
        runtime.checkpoint().expect("checkpoint");
        runtime
            .db()
            .rebuild_index_registry(Some("embeddings"))
            .expect("publish artifact references");
        let metadata = runtime.db().physical_metadata().expect("internal metadata");
        assert!(
            metadata
                .indexes
                .iter()
                .any(|index| index.artifact_root_page.is_some()),
            "fixture must exercise native artifact addresses"
        );
        runtime.graceful_shutdown(false).expect("shutdown");
    }
    assert!(!reddb_file::physical_metadata_binary_path(&source).exists());
    let before = std::fs::read(&source).expect("source bytes");
    let destination = directory.path().join("compacted-vectors");
    compact_paged(&source, &destination).expect("relocate internal metadata");
    assert_eq!(std::fs::read(&source).expect("source bytes"), before);
    let compacted = destination.join("vectors.rdb");
    assert!(!reddb_file::physical_metadata_binary_path(&compacted).exists());
    for _ in 0..2 {
        let runtime = RedDBRuntime::with_options(options(&compacted)).expect("cold boot");
        let result = runtime
            .execute_query("VECTOR SEARCH embeddings SIMILAR TO [1,0] LIMIT 1")
            .expect("native index after relocation");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("content"),
            Some(&reddb_types::Value::text("kept"))
        );
        runtime
            .graceful_shutdown(false)
            .expect("checkpoint relocated metadata");
    }
}
