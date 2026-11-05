//! A few audit-style inserts must not copy an immutable history at every checkpoint.
use reddb_server::storage::{EntityId, RowData, UnifiedEntity, UnifiedStore, UnifiedStoreConfig};
use reddb_types::Value;

#[test]
fn runtime_audit_append_after_reopen_has_bounded_checkpoint_cost() {
    use reddb_server::runtime::control_events::{
        ActorRef, ControlEvent, ControlEventCtx, ControlEventLedger, EventKind, Outcome,
        RuntimeLedger,
    };
    use reddb_server::{RedDBOptions, RedDBRuntime};
    let dir = tempfile::tempdir().unwrap();
    let options = RedDBOptions::persistent(dir.path().join("runtime.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    let emit = |ledger: &RuntimeLedger| {
        ledger
            .emit(
                &ControlEventCtx {
                    actor: ActorRef::Anonymous,
                    scope: None,
                    request_id: None,
                    trace_id: None,
                },
                ControlEvent {
                    kind: EventKind::PolicyCreate,
                    outcome: Outcome::Allowed,
                    action: "policy.write".into(),
                    resource: Some("history".repeat(256)),
                    reason: None,
                    matched_policy_id: None,
                    fields: Default::default(),
                },
            )
            .unwrap();
    };
    {
        let rt = RedDBRuntime::with_options(options.clone()).unwrap();
        let ledger = RuntimeLedger::new(rt.db().store());
        for _ in 0..2048 {
            emit(&ledger);
        }
        rt.checkpoint().unwrap();
    }
    for round in 0..3 {
        let rt = RedDBRuntime::with_options(options.clone()).unwrap();
        let ledger = RuntimeLedger::new(rt.db().store());
        emit(&ledger);
        let store = rt.db().store();
        let pager = store.pager().unwrap();
        let before = pager.cache_stats();
        rt.checkpoint().unwrap();
        let after = pager.cache_stats();
        let reads = after.hits + after.misses - before.hits - before.misses;
        eprintln!("runtime_audit_checkpoint_page_reads={reads}");
        assert!(
            reads < 512,
            "runtime audit append revisited {reads} historical pages"
        );
        let manager = store.get_collection("red.control_events").unwrap();
        assert_eq!(manager.count(), 2049 + round);
        assert_eq!(
            manager
                .try_paged_physical_cursor()
                .unwrap()
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
                .len(),
            2049 + round
        );
    }
}

fn config() -> UnifiedStoreConfig {
    UnifiedStoreConfig {
        page_cache_slots: Some(32),
        resident_memory_bytes_max: Some(8 * 1024 * 1024),
        ..UnifiedStoreConfig::default()
    }
}

fn row(id: u64) -> UnifiedEntity {
    let mut entity = UnifiedEntity::table_row(EntityId::new(id), "audit_events", id, vec![]);
    entity.data = reddb_server::storage::EntityData::Row(RowData {
        columns: vec![],
        schema: None,
        named: Some(
            [
                ("id".to_string(), Value::Integer(id as i64)),
                ("payload".to_string(), Value::text("history".repeat(256))),
            ]
            .into_iter()
            .collect(),
        ),
    });
    entity
}

#[test]
fn cold_unindexed_append_checkpoint_does_not_revisit_historical_pages() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.rdb");
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    store.create_collection("audit_events").unwrap();
    for id in 1..=2048 {
        store.insert("audit_events", row(id)).unwrap();
    }
    store.persist().unwrap();
    for id in 2049..=2051 {
        store.insert("audit_events", row(id)).unwrap();
        let before = store.pager().unwrap().cache_stats();
        store.persist().unwrap();
        let after = store.pager().unwrap().cache_stats();
        let reads = after.hits + after.misses - before.hits - before.misses;
        eprintln!("cold_append_checkpoint_page_reads={reads}");
        assert!(
            reads < 512,
            "one appended row revisited {reads} historical pages"
        );
    }
    drop(store);
    let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
    assert_eq!(
        reopened.get_collection("audit_events").unwrap().count(),
        2051
    );
    for id in [1, 2048, 2049, 2051] {
        assert!(reopened
            .try_get("audit_events", EntityId::new(id), 64 * 1024)
            .unwrap()
            .is_some());
    }
}

#[test]
fn captured_cold_cursor_keeps_its_generation_during_append_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pinned.rdb");
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    store.create_collection("audit_events").unwrap();
    for id in 1..=32 {
        store.insert("audit_events", row(id)).unwrap();
    }
    store.persist().unwrap();
    let manager = store.get_collection("audit_events").unwrap();
    let cursor = manager.try_paged_physical_cursor().unwrap().unwrap();
    store.insert("audit_events", row(33)).unwrap();
    store.persist().unwrap();
    let captured = cursor.collect::<Result<Vec<_>, _>>().unwrap();
    assert_eq!(captured.len(), 32);
    assert!(captured.iter().all(|(entity, _)| entity.id.raw() <= 32));
    assert_eq!(manager.count(), 33);
    drop(manager);
    drop(store);
    let reopened = UnifiedStore::open_with_config(path, config()).unwrap();
    assert_eq!(reopened.get_collection("audit_events").unwrap().count(), 33);
}

#[test]
fn borrowed_and_weak_backing_sources_prevent_exclusive_publication() {
    let dir = tempfile::tempdir().unwrap();
    let store = UnifiedStore::open_with_config(dir.path().join("owners.rdb"), config()).unwrap();
    store.create_collection("audit_events").unwrap();
    store.insert("audit_events", row(1)).unwrap();
    store.persist().unwrap();
    let manager = store.get_collection("audit_events").unwrap();
    let root = manager.backing_root_page().unwrap();
    let (base, overlay) = manager.current_backing_overlay().unwrap();
    assert!(manager
        .try_with_unshared_backing::<()>(root, || panic!("borrowed base must exclude mutation"))
        .is_none());
    let weak = std::sync::Arc::downgrade(&base);
    drop(base);
    drop(overlay);
    assert!(manager
        .try_with_unshared_backing::<()>(root, || panic!("weak base may acquire a reader"))
        .is_none());
    drop(weak);
    assert_eq!(
        manager.try_with_unshared_backing(root, || (true, None)),
        Some(true)
    );
}

#[test]
fn cold_updates_deletes_and_keyed_inserts_keep_the_replacement_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mutations.rdb");
    let store = UnifiedStore::open_with_config(&path, config()).unwrap();
    store.create_collection("audit_events").unwrap();
    for id in 1..=32 {
        store.insert("audit_events", row(id)).unwrap();
    }
    store.persist().unwrap();
    let mut updated = row(1);
    let reddb_server::storage::EntityData::Row(data) = &mut updated.data else {
        panic!("row")
    };
    data.named
        .as_mut()
        .unwrap()
        .insert("payload".into(), Value::text("changed"));
    store
        .get_collection("audit_events")
        .unwrap()
        .update(updated)
        .unwrap();
    assert!(store.delete("audit_events", EntityId::new(2)).unwrap());
    let mut keyed = row(33);
    let reddb_server::storage::EntityData::Row(data) = &mut keyed.data else {
        panic!("row")
    };
    data.named
        .as_mut()
        .unwrap()
        .insert("key".into(), Value::text("new-key"));
    store.insert("audit_events", keyed).unwrap();
    store.persist().unwrap();
    drop(store);
    let reopened = UnifiedStore::open_with_config(path, config()).unwrap();
    assert_eq!(reopened.get_collection("audit_events").unwrap().count(), 32);
    assert!(reopened
        .try_get("audit_events", EntityId::new(2), 65536)
        .unwrap()
        .is_none());
    let updated = reopened
        .try_get("audit_events", EntityId::new(1), 65536)
        .unwrap()
        .unwrap();
    assert_eq!(
        updated.data.as_row().unwrap().get_field("payload"),
        Some(&Value::text("changed"))
    );
    let manager = reopened.get_collection("audit_events").unwrap();
    let rows = manager
        .try_paged_key_entity_cursor("new-key", None)
        .unwrap()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.id, EntityId::new(33));
}

#[cfg(debug_assertions)]
#[test]
fn cold_sparse_append_recovers_after_interrupted_root_split() {
    const CHILD: &str = "REDDB_COLD_APPEND_CRASH_PATH";
    if let Some(path) = std::env::var_os(CHILD) {
        let mut options = config();
        options.page_cache_slots = Some(512);
        let store = UnifiedStore::open_with_config(&path, options).unwrap();
        store.create_collection("audit_events").unwrap();
        for id in 1..=8 {
            store.insert("audit_events", row(id)).unwrap();
        }
        store.persist().unwrap();
        for id in 9..=264 {
            store.insert("audit_events", row(id)).unwrap();
        }
        // Existing failpoint syncs the changed native pages and exits before
        // catalog publication; recovery must undo that split and replay the WAL.
        std::env::set_var("REDDB_TEST_CHECKPOINT_EXIT_AFTER_SPARSE_ROOT_CHANGE", "1");
        store.persist().unwrap();
        panic!("sparse root-split failpoint was not reached");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("crash.rdb");
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "cold_sparse_append_recovers_after_interrupted_root_split",
            "--nocapture",
        ])
        .env(CHILD, &path)
        .output()
        .unwrap();
    assert_eq!(
        child.status.code(),
        Some(176),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
    assert_eq!(
        reopened.get_collection("audit_events").unwrap().count(),
        264
    );
    for id in 1..=264 {
        assert!(
            reopened
                .try_get("audit_events", EntityId::new(id), 65536)
                .unwrap()
                .is_some(),
            "lost acknowledged row {id}"
        );
    }
    reopened.persist().unwrap();
}
