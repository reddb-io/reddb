use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore, UnifiedStoreConfig};
use reddb_types::Value;

fn cold_config() -> UnifiedStoreConfig {
    UnifiedStoreConfig {
        page_cache_slots: Some(8),
        resident_memory_bytes_max: Some(128 * 1024),
        ..UnifiedStoreConfig::default()
    }
}

#[test]
fn paged_reopen_and_checkpoint_keep_cold_rows_under_the_resident_budget() {
    let directory = tempfile::tempdir().expect("private fixture");
    let path = directory.path().join("cold.rdb");
    {
        let store = UnifiedStore::open(&path).expect("create fixture");
        store.create_collection("rows").expect("collection");
        let manager = store.get_collection("rows").expect("rows");
        manager
            .bulk_insert(
                (1..=2048)
                    .map(|id| {
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            "rows",
                            id * 3,
                            vec![Value::Integer(id as i64), Value::text("x".repeat(2048))],
                        )
                    })
                    .collect(),
            )
            .expect("fixture rows");
        store.persist().expect("durable fixture");
    }
    {
        let store = UnifiedStore::open_with_config(&path, cold_config()).expect("bounded reopen");
        assert!(
            store.segment_memory_bytes() <= 128 * 1024,
            "a paged reopen must not hydrate every persisted row: {} resident bytes",
            store.segment_memory_bytes()
        );
        assert_eq!(store.get_collection("rows").expect("rows").count(), 2048);
        let mut row = store
            .try_get("rows", EntityId::new(2048), 128 * 1024)
            .expect("read")
            .expect("cold row");
        let reddb_server::storage::EntityData::Row(data) = &mut row.data else {
            panic!("table row")
        };
        data.columns[1] = Value::text("changed");
        store
            .get_collection("rows")
            .expect("rows")
            .update(row)
            .expect("cold update");
        assert!(store
            .delete("rows", EntityId::new(1024))
            .expect("cold delete"));
        store.persist().expect("merge cold base and mutations");
        assert!(store.segment_memory_bytes() <= 128 * 1024);
    }
    let store =
        UnifiedStore::open_with_config(&path, cold_config()).expect("reopen merged fixture");
    assert_eq!(store.get_collection("rows").expect("rows").count(), 2047);
    for id in [1, 1023, 1025] {
        assert_eq!(
            store
                .try_get("rows", EntityId::new(id), 128 * 1024)
                .expect("read")
                .expect("unchanged cold row")
                .data
                .as_row()
                .expect("row")
                .columns[1],
            Value::text("x".repeat(2048))
        );
    }
    assert!(store
        .try_get("rows", EntityId::new(1024), 128 * 1024)
        .expect("read")
        .is_none());
    assert_eq!(
        store
            .try_get("rows", EntityId::new(2048), 128 * 1024)
            .expect("read")
            .expect("updated cold row")
            .data
            .as_row()
            .expect("row")
            .columns[1],
        Value::text("changed")
    );
    let next = store
        .insert_auto(
            "rows",
            UnifiedEntity::table_row(
                EntityId::new(0),
                "rows",
                0,
                vec![Value::Integer(2049), Value::text("next")],
            ),
        )
        .expect("continued identity");
    let row = store
        .try_get("rows", next, 128 * 1024)
        .expect("read")
        .expect("inserted row");
    assert!(next.raw() > 2048);
    assert!(
        matches!(row.kind, reddb_server::storage::EntityKind::TableRow { row_id, .. } if row_id > 6144)
    );
}

fn cold_fixture(path: &std::path::Path) {
    let store = UnifiedStore::open(path).expect("create fixture");
    store.create_collection("rows").expect("collection");
    store
        .get_collection("rows")
        .expect("rows")
        .bulk_insert(
            (1..=8)
                .map(|id| {
                    UnifiedEntity::table_row(
                        EntityId::new(id),
                        "rows",
                        id * 3,
                        vec![Value::Integer(id as i64)],
                    )
                })
                .collect(),
        )
        .expect("seed rows");
    store.persist().expect("checkpoint seed");
}

#[test]
fn cold_base_rejects_duplicate_individual_and_bulk_ids() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("duplicates.rdb");
    cold_fixture(&path);
    let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
    let manager = store.get_collection("rows").expect("rows");
    let duplicate =
        UnifiedEntity::table_row(EntityId::new(4), "rows", 12, vec![Value::Integer(99)]);
    assert!(
        manager.insert(duplicate.clone()).is_err(),
        "cold IDs are already occupied"
    );
    assert!(
        manager.bulk_insert(vec![duplicate]).is_err(),
        "bulk insertion must also probe the cold base"
    );
    assert_eq!(manager.count(), 8);
    assert_eq!(
        store
            .try_get("rows", EntityId::new(4), 128 * 1024)
            .expect("read")
            .expect("row")
            .data
            .as_row()
            .expect("row")
            .columns[0],
        Value::Integer(4)
    );
}

#[test]
fn cold_hot_update_and_batch_delete_survive_checkpoint() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("mutations.rdb");
    cold_fixture(&path);
    {
        let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
        let manager = store.get_collection("rows").expect("rows");
        let mut row = store
            .try_get("rows", EntityId::new(4), 128 * 1024)
            .expect("read")
            .expect("row");
        let reddb_server::storage::EntityData::Row(data) = &mut row.data else {
            panic!("row")
        };
        data.columns[0] = Value::Integer(99);
        manager
            .update_hot_with_metadata(row, &[], None)
            .expect("cold HOT update");
        assert_eq!(
            store
                .delete_batch("rows", &[EntityId::new(2), EntityId::new(6)])
                .expect("batch delete")
                .len(),
            2
        );
        store.persist().expect("checkpoint");
        store.persist().expect("idle checkpoint");
    }
    let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
    assert_eq!(store.get_collection("rows").expect("rows").count(), 6);
    assert_eq!(
        store
            .try_get("rows", EntityId::new(4), 128 * 1024)
            .expect("read")
            .expect("row")
            .data
            .as_row()
            .expect("row")
            .columns[0],
        Value::Integer(99)
    );
    for id in [2, 6] {
        assert!(store
            .try_get("rows", EntityId::new(id), 128 * 1024)
            .expect("read")
            .is_none());
    }
}

#[test]
fn cold_metadata_reads_and_replacements_survive_checkpoint() {
    use reddb_server::storage::{Metadata, MetadataValue};
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("metadata.rdb");
    cold_fixture(&path);
    {
        let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
        let mut metadata = Metadata::new();
        metadata.set("source", MetadataValue::String("cold".into()));
        store
            .set_metadata("rows", EntityId::new(4), metadata)
            .expect("cold metadata replacement");
        store.persist().expect("checkpoint");
    }
    let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
    let metadata = store
        .get_collection("rows")
        .expect("rows")
        .try_get_metadata(EntityId::new(4), 128 * 1024)
        .expect("fallible metadata")
        .expect("metadata");
    assert_eq!(
        metadata.get("source"),
        Some(&MetadataValue::String("cold".into()))
    );
}

#[test]
fn cold_wal_crash_child() {
    let Some(path) = std::env::var_os("REDDB_BOUNDED_WAL_CRASH_PATH") else {
        return;
    };
    let store = UnifiedStore::open_with_config(std::path::PathBuf::from(path), cold_config())
        .expect("cold child open");
    let mut entity = store
        .try_get("rows", EntityId::new(4), 128 * 1024)
        .expect("read")
        .expect("row");
    let reddb_server::storage::EntityData::Row(row) = &mut entity.data else {
        panic!("row")
    };
    row.columns[0] = Value::Integer(99);
    store
        .get_collection("rows")
        .expect("rows")
        .update(entity.clone())
        .expect("update");
    store
        .set_metadata(
            "rows",
            EntityId::new(4),
            reddb_server::storage::Metadata::new(),
        )
        .expect("persist canonical updated row through public metadata write");
    store
        .delete_batch("rows", &[EntityId::new(2), EntityId::new(6)])
        .expect("durable WAL deletions");
    std::process::exit(91);
}

#[test]
fn cold_wal_replay_updates_existing_ids_without_inflating_counts() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("replay.rdb");
    cold_fixture(&path);
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "cold_wal_crash_child", "--nocapture"])
        .env("REDDB_BOUNDED_WAL_CRASH_PATH", &path)
        .status()
        .expect("crash subprocess");
    assert_eq!(status.code(), Some(91));
    for _ in 0..2 {
        let store = UnifiedStore::open_with_config(&path, cold_config()).expect("recover cold WAL");
        assert_eq!(store.get_collection("rows").expect("rows").count(), 6);
        assert_eq!(
            store
                .try_get("rows", EntityId::new(4), 128 * 1024)
                .expect("read")
                .expect("row")
                .data
                .as_row()
                .expect("row")
                .columns[0],
            Value::Integer(99)
        );
        for id in [2, 6] {
            assert!(store
                .try_get("rows", EntityId::new(id), 128 * 1024)
                .expect("read")
                .is_none());
        }
        store.persist().expect("checkpoint recovered data");
    }
}

#[test]
fn cold_scan_merges_mutations_in_physical_order_and_propagates_limits() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("scan.rdb");
    cold_fixture(&path);
    let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
    let manager = store.get_collection("rows").expect("rows");
    store.delete("rows", EntityId::new(3)).expect("delete");
    manager
        .insert(UnifiedEntity::table_row(
            EntityId::new(9),
            "rows",
            27,
            vec![Value::Integer(9)],
        ))
        .expect("new overlay row");
    let mut ids = Vec::new();
    manager
        .try_for_each_entity(128 * 1024, |entity| {
            ids.push(entity.id.raw());
            true
        })
        .expect("merged physical scan");
    assert_eq!(ids, vec![1, 2, 4, 5, 6, 7, 8, 9]);
    assert!(
        manager.try_for_each_entity(1, |_| true).is_err(),
        "an unreadable record must fail the scan"
    );
    let mut first = Vec::new();
    manager
        .try_scan_for_each(None, 128 * 1024, |entity| {
            first.push(entity.id.raw());
            false
        })
        .expect("early stop");
    assert_eq!(first, vec![1]);
}

#[test]
fn cold_writes_leave_captured_published_generation_unchanged() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("generation.rdb");
    cold_fixture(&path);
    let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
    let manager = store.get_collection("rows").expect("rows");
    let (base, _) = manager.current_backing_overlay().expect("published base");
    let before = base
        .get_record(EntityId::new(4), 128 * 1024)
        .expect("base read")
        .expect("row")
        .bytes;
    store
        .delete_batch("rows", &[EntityId::new(4)])
        .expect("delete overlay");
    store
        .insert_auto(
            "rows",
            UnifiedEntity::table_row(EntityId::new(0), "rows", 0, vec![Value::Integer(99)]),
        )
        .expect("insert overlay");
    assert_eq!(
        base.get_record(EntityId::new(4), 128 * 1024)
            .expect("old base read")
            .expect("old generation retains deleted row")
            .bytes,
        before
    );
    store.persist().expect("publish successor");
    assert_eq!(
        base.get_record(EntityId::new(4), 128 * 1024)
            .expect("pinned base read")
            .expect("pinned old generation")
            .bytes,
        before
    );
    assert!(store
        .try_get("rows", EntityId::new(4), 128 * 1024)
        .expect("current read")
        .is_none());
}

#[test]
fn cold_checkpoint_preserves_deleted_identity_high_water_marks() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("identity-high-water.rdb");
    cold_fixture(&path);
    {
        let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
        store
            .delete("rows", EntityId::new(8))
            .expect("delete highest identity");
        store
            .persist()
            .expect("checkpoint without highest identity");
    }
    let store = UnifiedStore::open_with_config(&path, cold_config()).expect("cold reopen");
    let id = store
        .insert_auto(
            "rows",
            UnifiedEntity::table_row(EntityId::new(0), "rows", 0, vec![Value::Integer(99)]),
        )
        .expect("allocate successor");
    assert!(
        id.raw() > 8,
        "deleted physical identities must not be reused: {id}"
    );
    let entity = store
        .try_get("rows", id, 128 * 1024)
        .expect("read")
        .expect("row");
    assert!(
        matches!(entity.kind, reddb_server::storage::EntityKind::TableRow { row_id, .. } if row_id > 24),
        "deleted row identities must not be reused"
    );
}

#[test]
fn prepared_boot_uses_resume_metadata_without_decoding_rows() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("prepared.rdb");
    cold_fixture(&path);
    {
        let store =
            UnifiedStore::open_with_config(&path, cold_config()).expect("legacy preparation");
        store.create_collection("empty").expect("empty collection");
        store.persist().expect("prepare resume counters");
    }
    assert!(
        UnifiedStore::open(&path).is_err(),
        "resident writers must refuse prepared immutable-base files"
    );
    let mut config = cold_config();
    config.resident_memory_bytes_max = Some(1);
    let store = UnifiedStore::open_with_config(&path, config)
        .expect("prepared boot does not decode any row");
    assert_eq!(store.get_collection("rows").expect("rows").count(), 8);
    assert_eq!(store.get_collection("empty").expect("empty").count(), 0);
    assert!(
        store.try_get("rows", EntityId::new(1), 1).is_err(),
        "the same allowance rejects an actual record read"
    );
}

#[test]
fn prepared_short_wal_boot_does_not_rebuild_unrelated_cold_payloads() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("short-wal.rdb");
    cold_fixture(&path);
    {
        let store = UnifiedStore::open(&path).expect("seed large cold row");
        store
            .get_collection("rows")
            .expect("rows")
            .insert(UnifiedEntity::table_row(
                EntityId::new(9),
                "rows",
                27,
                vec![Value::Blob(
                    (0..32 * 1024)
                        .scan(0x1234_5678_u32, |state, _| {
                            *state ^= *state << 13;
                            *state ^= *state >> 17;
                            *state ^= *state << 5;
                            Some(*state as u8)
                        })
                        .collect(),
                )],
            ))
            .expect("large cold payload");
        store.persist().expect("checkpoint payload");
    }
    {
        let store =
            UnifiedStore::open_with_config(&path, cold_config()).expect("legacy preparation");
        store.persist().expect("prepare resume metadata");
    }
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "cold_wal_crash_child", "--nocapture"])
        .env("REDDB_BOUNDED_WAL_CRASH_PATH", &path)
        .status()
        .expect("crash subprocess");
    assert_eq!(status.code(), Some(91));
    for _ in 0..2 {
        let mut config = cold_config();
        config.resident_memory_bytes_max = Some(4096);
        let store = UnifiedStore::open_with_config(&path, config)
            .expect("short WAL boot must not decode/rebuild unrelated large cold rows");
        assert_eq!(store.get_collection("rows").expect("rows").count(), 7);
        assert_eq!(
            store
                .try_get("rows", EntityId::new(4), 4096)
                .expect("small WAL row")
                .expect("row")
                .data
                .as_row()
                .expect("row")
                .columns[0],
            Value::Integer(99)
        );
        assert!(
            store.try_get("rows", EntityId::new(9), 4096).is_err(),
            "large unrelated row requires a larger read allowance"
        );
        // Intentionally retain the WAL across both opens; no checkpoint/drop
        // hook may erase replay evidence before publishing its row images.
    }
}
