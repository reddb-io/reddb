use super::*;
use reddb_engine::transaction::snapshot::SnapshotManager;

fn config() -> UnifiedStoreConfig {
    UnifiedStoreConfig {
        page_cache_slots: Some(32),
        resident_memory_bytes_max: Some(2 * 1024 * 1024),
        ..UnifiedStoreConfig::default()
    }
}

fn fixture() -> (tempfile::TempDir, UnifiedStore) {
    let directory = tempfile::tempdir().expect("visibility fixture");
    let store =
        UnifiedStore::open_with_config(directory.path().join("count.rdb"), config()).expect("open");
    store.create_collection("rows").expect("collection");
    (directory, store)
}

fn context(manager: &Arc<SnapshotManager>) -> SnapshotContext {
    SnapshotContext {
        snapshot: manager.fresh_read_snapshot(),
        manager: Arc::clone(manager),
        own_xids: Default::default(),
        requires_index_fallback: true,
        serializable_reader: None,
    }
}

fn compare(store: &UnifiedStore, context: Option<&SnapshotContext>, expected: u64) {
    let mut scanned = 0;
    store
        .get_collection("rows")
        .expect("rows")
        .try_scan_for_each(context, usize::MAX, |_| {
            scanned += 1;
            true
        })
        .expect("reference scan");
    assert_eq!(scanned, expected, "reference");
    assert_eq!(
        store
            .try_count_from_index("rows", context)
            .expect("indexed"),
        Some(expected)
    );
}

#[test]
fn paged_count_snapshot_active_aborted_and_own_versions_match_scan() {
    let (_directory, store) = fixture();
    let transactions = Arc::new(SnapshotManager::new());
    let before = context(&transactions);
    let committed = transactions.begin();
    transactions.commit(committed);
    let aborted = transactions.begin();
    transactions.rollback(aborted);
    let active = transactions.begin();
    let states = [
        (0, 0),
        (committed, 0),
        (committed, 0),
        (aborted, 0),
        (active, 0),
        (0, committed),
        (0, aborted),
        (0, active),
        (0, active),
    ];
    for (offset, (xmin, xmax)) in states.into_iter().enumerate() {
        let mut entity =
            UnifiedEntity::table_row(EntityId::new(offset as u64 + 1), "rows", 0, vec![]);
        entity.xmin = xmin;
        entity.xmax = xmax;
        store.insert("rows", entity).expect("row");
    }
    store.register_count_index("rows").expect("projection");
    store.persist().expect("prepare");
    let reader = context(&transactions);
    compare(&store, Some(&before), 5);
    compare(&store, Some(&reader), 6);
    let mut own = reader.clone();
    own.own_xids.insert(active);
    compare(&store, Some(&own), 5);
    transactions.rollback(active);
    compare(&store, Some(&own), 6);
    compare(&store, Some(&reader), 6);
    compare(&store, None, 5);
    let mut serializable = reader;
    serializable.serializable_reader = Some(transactions.begin());
    assert_eq!(
        store
            .try_count_from_index("rows", Some(&serializable))
            .expect("fallback"),
        None
    );
}

#[test]
fn paged_count_moderation_positional_schema_overlay_delete_and_reopen() {
    let (directory, store) = fixture();
    let manager = store.get_collection("rows").expect("rows");
    let schema = vec!["payload".into(), "__moderation_status".into()];
    manager.set_column_schema_if_empty(schema.clone());
    for (offset, marker) in ["", "pending", "rejected"].into_iter().enumerate() {
        let mut entity = UnifiedEntity::table_row(
            EntityId::new(offset as u64 + 1),
            "rows",
            0,
            vec![Value::text("big".repeat(8192)), Value::text(marker)],
        );
        if let EntityData::Row(row) = &mut entity.data {
            row.schema = Some(Arc::new(schema.clone()));
        }
        store.insert("rows", entity).expect("row");
    }
    store.register_count_index("rows").expect("projection");
    store.persist().expect("prepare");
    compare(&store, None, 1);
    let mut cleared = manager
        .try_get(EntityId::new(2), usize::MAX)
        .expect("read")
        .expect("row");
    if let EntityData::Row(row) = &mut cleared.data {
        if let Some(named) = &mut row.named {
            named.insert("__moderation_status".into(), Value::text(""));
        } else {
            row.columns[1] = Value::text("");
        }
    }
    manager
        .update(cleared)
        .expect("clear moderation in overlay");
    compare(&store, None, 2);
    store
        .delete("rows", EntityId::new(1))
        .expect("physical delete");
    compare(&store, None, 1);
    store.persist().expect("publish changes");
    compare(&store, None, 1);
    drop(manager);
    drop(store);
    let store = UnifiedStore::open_with_config(directory.path().join("count.rdb"), config())
        .expect("reopen");
    store
        .get_collection("rows")
        .expect("rows")
        .set_column_schema_if_empty(schema);
    store
        .register_count_index("rows")
        .expect("compatible projector");
    compare(&store, None, 1);
}

#[test]
fn paged_count_multiple_leaves_faults_and_missing_generation() {
    use crate::storage::engine::{Page, PageType};
    let (_directory, store) = fixture();
    for id in 1..=1301 {
        store
            .insert(
                "rows",
                UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![]),
            )
            .expect("row");
    }
    assert_eq!(
        store
            .try_count_from_index("rows", None)
            .expect("unprepared"),
        None
    );
    store.register_count_index("rows").expect("projection");
    store.persist().expect("prepare");
    compare(&store, None, 1301);
    let native = store
        .secondary_index("rows", NAME, &identity(None))
        .expect("projection");
    let pager = store.pager().expect("pager");
    let root = native.root_page_id();
    let original = pager.read_page(root).expect("original");
    pager
        .write_page(root, Page::new(PageType::Overflow, root))
        .expect("inject corrupt index");
    assert!(
        store.try_count_from_index("rows", None).is_err(),
        "unreadable index must not return a partial/zero count"
    );
    pager.write_page(root, original).expect("repair fixture");
    compare(&store, None, 1301);
    store.unregister_secondary_index("rows", NAME);
    assert_eq!(
        store.try_count_from_index("rows", None).expect("missing"),
        None
    );
}

#[test]
fn paged_count_commit_changes_new_snapshots_without_rebuilding_index() {
    let (_directory, store) = fixture();
    store.register_count_index("rows").expect("projection");
    store.persist().expect("empty");
    assert_eq!(
        store
            .try_count_from_index("rows", None)
            .expect("empty scan fallback"),
        None
    );
    let transactions = Arc::new(SnapshotManager::new());
    let writer = transactions.begin();
    let mut entity = UnifiedEntity::table_row(EntityId::new(1), "rows", 1, vec![]);
    entity.xmin = writer;
    store.insert("rows", entity).expect("row");
    store.persist().expect("physical version fixture");
    let before = context(&transactions);
    compare(&store, Some(&before), 0);
    transactions.commit(writer);
    compare(&store, Some(&before), 0);
    compare(&store, Some(&context(&transactions)), 1);
}

#[test]
fn paged_count_legacy_positional_moderation_uses_bound_schema() {
    let projection = VisibilityProjection {
        schema: Some(Arc::new(vec![
            "payload".into(),
            "__moderation_status".into(),
        ])),
    };
    for (marker, hidden) in [("pending", 1), ("rejected", 1), ("", 0)] {
        // Legacy positional records have neither per-row names nor a schema.
        let entity = UnifiedEntity::table_row(
            EntityId::new(1),
            "rows",
            1,
            vec![Value::text("payload"), Value::text(marker)],
        );
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        assert_eq!(
            projection.project(&entity, &mut key).expect("project"),
            Some(17)
        );
        assert_eq!(key[16], hidden);
    }
    assert_ne!(
        identity(projection.schema.as_deref().map(Vec::as_slice)),
        identity(None)
    );
}
