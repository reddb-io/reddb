//! Cold bootstrap cannot infer allocator history from surviving rows alone.
use reddb_server::storage::{EntityId, EntityKind, UnifiedEntity, UnifiedStore};
use reddb_types::Value;

fn row(value: i64) -> UnifiedEntity {
    UnifiedEntity::table_row(EntityId::new(0), "rows", 0, vec![Value::Integer(value)])
}

fn deleted_maximum(checkpoint_after_delete: bool) {
    let directory = tempfile::tempdir().expect("isolated native fixture");
    let path = directory.path().join("database.rdb");
    let store = UnifiedStore::open(&path).expect("writer");
    let first = store.insert_auto("rows", row(1)).expect("first row");
    let deleted = store
        .insert_auto("rows", row(2))
        .expect("largest allocated row");
    let old = store.get("rows", deleted).expect("allocated payload");
    let EntityKind::TableRow {
        row_id: old_row_id, ..
    } = old.kind
    else {
        panic!("table row")
    };
    store.persist().expect("checkpoint both identities");
    assert!(store.delete("rows", deleted).expect("delete largest row"));
    if checkpoint_after_delete {
        store.persist().expect("checkpoint surviving row");
    }
    drop(store);

    let reopened = UnifiedStore::open(&path).expect("native reopen and WAL recovery");
    assert!(reopened.get("rows", first).is_some());
    assert!(reopened.get("rows", deleted).is_none());
    let fresh = reopened
        .insert_auto("rows", row(3))
        .expect("new row after reopen");
    let current = reopened.get("rows", fresh).expect("fresh payload");
    let EntityKind::TableRow {
        row_id: fresh_row_id,
        ..
    } = current.kind
    else {
        panic!("table row")
    };
    eprintln!("checkpoint_after_delete={checkpoint_after_delete} deleted_entity={} fresh_entity={} deleted_row={old_row_id} fresh_row={fresh_row_id} old_sequence={} fresh_sequence={}", deleted.raw(),fresh.raw(),old.sequence_id,current.sequence_id);
    assert!(
        fresh.raw() > deleted.raw(),
        "reopen reused a previously allocated physical entity identity"
    );
    assert!(
        fresh_row_id > old_row_id,
        "reopen reused a previously allocated row identity"
    );
}

#[test]
fn checkpoint_preserves_allocator_high_water_after_deleting_the_maximum() {
    deleted_maximum(true);
}

#[test]
fn wal_replay_preserves_allocator_high_water_after_deleting_the_maximum() {
    deleted_maximum(false);
}

#[test]
fn dropping_the_last_collection_keeps_global_identity_history() {
    let directory = tempfile::tempdir().expect("isolated fixture");
    let path = directory.path().join("dropped.rdb");
    let store = UnifiedStore::open(&path).expect("writer");
    let previous = store.insert_auto("rows", row(1)).expect("first row");
    store.drop_collection("rows").expect("drop last collection");
    store.persist().expect("checkpoint empty catalog");
    drop(store);
    let reopened = UnifiedStore::open(&path).expect("empty catalog reopen");
    let current = reopened
        .insert_auto(
            "other",
            UnifiedEntity::table_row(EntityId::new(0), "other", 0, vec![Value::Integer(2)]),
        )
        .expect("new collection");
    assert!(current.raw() > previous.raw());
}

#[test]
fn a_known_but_invalid_allocator_tail_fails_open() {
    use reddb_server::storage::engine::Pager;
    use reddb_server::storage::StoreError;
    for unknown_collection in [false, true] {
        let directory = tempfile::tempdir().expect("isolated corrupt fixture");
        let path = directory.path().join("invalid.rdb");
        let store = UnifiedStore::open(&path).expect("writer");
        store.insert_auto("rows", row(1)).expect("row");
        store.persist().expect("valid allocator catalog");
        drop(store);
        let pager = Pager::open_default(&path).expect("fixture-only pager");
        let mut page = pager.read_page(1).expect("catalog page");
        let content = page.content_mut();
        let start = content
            .windows(4)
            .position(|bytes| bytes == b"RDA1")
            .expect("allocator tail");
        if unknown_collection {
            // Keep valid framing/CRC, but point the allocator at an absent name.
            content[start + 20..start + 24].copy_from_slice(b"oops");
        } else {
            content[start + 4..start + 12].fill(0);
        }
        page.update_checksum();
        pager.write_page(1, page).expect("write semantic fault");
        pager.sync().expect("durable test fault");
        drop(pager);
        let error = match UnifiedStore::open(&path) {
            Ok(_) => panic!("invalid allocator metadata must not become legacy fallback"),
            Err(error) => error,
        };
        assert!(matches!(&error, StoreError::StorageIntegrity(_)), "{error}");
        assert!(error.to_string().contains("allocator"), "{error}");
    }
}

#[test]
fn empty_collection_keeps_allocator_history_without_any_surviving_payload() {
    let directory = tempfile::tempdir().expect("isolated native fixture");
    let path = directory.path().join("empty.rdb");
    let store = UnifiedStore::open(&path).expect("writer");
    let first = store.insert_auto("rows", row(1)).expect("first row");
    let second = store.insert_auto("rows", row(2)).expect("second row");
    assert!(store.delete("rows", first).expect("delete first"));
    assert!(store.delete("rows", second).expect("delete second"));
    store.persist().expect("checkpoint empty collection");
    drop(store);
    let reopened = UnifiedStore::open(&path).expect("empty reopen");
    assert_eq!(
        reopened
            .get_collection("rows")
            .expect("empty collection exists")
            .count(),
        0
    );
    let fresh = reopened.insert_auto("rows", row(3)).expect("fresh row");
    let entity = reopened.get("rows", fresh).expect("fresh payload");
    let EntityKind::TableRow { row_id, .. } = entity.kind else {
        panic!("table row")
    };
    assert_eq!((fresh.raw(), row_id), (3, 3));
    reopened.persist().expect("checkpoint new allocation");
    drop(reopened);
    let again = UnifiedStore::open(&path).expect("second reopen");
    assert_eq!(again.next_entity_id(), EntityId::new(4));
    assert_eq!(
        again
            .get_collection("rows")
            .expect("rows")
            .row_allocator_next(),
        4
    );
}
