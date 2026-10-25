//! Canonical collection lifecycle on native hosts and in dedicated browser Workers.
//! No application SQL or durable transaction publication is supplied by these fixtures.
use reddb_engine::storage::entity::{EntityData, EntityId, RowData, UnifiedEntity};
use reddb_engine::storage::manager::{ManagerConfig, SegmentManager};
use reddb_engine::storage::segment::{GraphEntityKind, SCAN_BATCH_SIZE};
use reddb_engine::storage::visibility::{
    entity_visible_with_context, MODERATION_STATUS_FIELD, MODERATION_STATUS_PENDING,
    MODERATION_STATUS_REJECTED,
};
use reddb_engine::transaction::{read::SnapshotContext, state::TransactionState, IsolationLevel};
use reddb_types::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const CASES: &[(&str, fn())] = &[
    (
        "manager_models_and_constraint_lock",
        models_and_constraint_lock,
    ),
    ("manager_snapshot_key_versions", snapshot_key_versions),
    (
        "manager_moderation_and_serializable_reads",
        moderation_and_serializable_reads,
    ),
    (
        "manager_bounded_scan_and_consolidation",
        bounded_scan_and_consolidation,
    ),
    ("manager_ordered_graph_candidates", ordered_graph_candidates),
];

fn row(id: u64, key: &str, xmin: u64, xmax: u64) -> UnifiedEntity {
    let mut entity = UnifiedEntity::table_row(EntityId::new(id), "keys", id, vec![]);
    entity.data = EntityData::Row(RowData::with_names(
        vec![Value::text(key), Value::Integer(7)],
        vec!["key".into(), "value".into()],
    ));
    entity.xmin = xmin;
    entity.xmax = xmax;
    entity
}

fn context(state: &TransactionState, connection: u64, serializable: bool) -> SnapshotContext {
    SnapshotContext {
        snapshot: state.current_snapshot(connection),
        manager: state.snapshot_manager(),
        own_xids: state.own_xids(connection),
        requires_index_fallback: true,
        serializable_reader: serializable
            .then(|| state.writer_xid(connection).expect("active reader")),
    }
}

fn models_and_constraint_lock() {
    let manager = SegmentManager::new("mixed");
    let lock = manager.row_constraint_lock();
    let outer = lock.lock();
    let inner = lock.lock();
    let mut entities = crate::record_corpus::entities();
    for entity in &mut entities {
        entity.xmin = 0;
        entity.xmax = 0;
    }
    for entity in &entities {
        manager.insert(entity.clone()).expect("canonical entity");
    }
    manager.force_seal().expect("first segment");
    manager
        .insert(row(4000, "key", 0, 0))
        .expect("second segment");
    manager.force_seal().expect("second seal");
    assert_eq!(manager.scan(None, |_| true).len(), entities.len() + 1);
    assert_eq!(
        manager.scan_fold_parallel(None, || 0, |count, _| count + 1, |a, b| a + b),
        entities.len() + 1,
        "native parallel and WASM serial reductions agree across sealed segments"
    );
    for entity in entities {
        assert_eq!(
            manager.get(entity.id).expect("retained entity").kind,
            entity.kind
        );
    }
    drop(inner);
    drop(outer);
}

fn snapshot_key_versions() {
    let state = TransactionState::new();
    let creator = state.begin(1, IsolationLevel::SnapshotIsolation);
    state
        .commit(1, |_| Ok::<_, ()>(()))
        .expect("creator commits");
    let writer = state.begin(1, IsolationLevel::SnapshotIsolation);
    state.begin(2, IsolationLevel::SnapshotIsolation);
    let reader = context(&state, 2, false);
    let manager = SegmentManager::new("keys");
    manager
        .insert(row(1, "theme", creator, writer))
        .expect("old version");
    manager.force_seal().expect("sealed history");
    manager
        .insert(row(2, "theme", writer, 0))
        .expect("uncommitted version");
    let key = |snapshot: &SnapshotContext| {
        manager.project_key_entity("theme", Some(snapshot), true, |entity| entity.id.raw())
    };
    assert_eq!(key(&reader), Some(1));
    assert_eq!(key(&context(&state, 1, false)), Some(2));
    let child = state.savepoint(1, "child").expect("savepoint");
    manager
        .insert(row(3, "theme", child, 0))
        .expect("own subtransaction");
    assert_eq!(key(&context(&state, 1, false)), Some(3));
    state
        .rollback_to_savepoint(1, "child")
        .expect("rollback child");
    assert_eq!(key(&context(&state, 1, false)), Some(2));
    state.rollback(1).expect("rollback writer");
    assert_eq!(
        key(&reader),
        Some(1),
        "aborted xmax revives the old version"
    );
    assert_eq!(
        manager.query_all(|_| true).len(),
        3,
        "physical history remains"
    );
    state.rollback(2).expect("reader exits");
}

fn moderation_and_serializable_reads() {
    for status in ["", MODERATION_STATUS_PENDING, MODERATION_STATUS_REJECTED] {
        let state = TransactionState::new();
        let first = state.begin(1, IsolationLevel::Serializable);
        let second = state.begin(2, IsolationLevel::Serializable);
        let manager = SegmentManager::new("keys");
        let a = row(10, "a", 0, 0);
        let mut b = row(11, "b", 0, 0);
        if let EntityData::Row(data) = &mut b.data {
            data.named = Some(HashMap::from([
                ("key".into(), Value::text("b")),
                (MODERATION_STATUS_FIELD.into(), Value::text(status)),
            ]));
        }
        manager.insert(a.clone()).expect("a");
        manager.insert(b.clone()).expect("b");
        let visible = status.is_empty();
        assert_eq!(entity_visible_with_context(None, &b), visible);
        assert_eq!(manager.scan(None, |_| true).len(), 1 + usize::from(visible));
        assert_eq!(
            manager
                .scan(Some(&context(&state, 1, true)), |e| e.id == b.id)
                .len(),
            usize::from(visible)
        );
        assert_eq!(
            manager
                .scan(Some(&context(&state, 2, true)), |e| e.id == a.id)
                .len(),
            1
        );
        assert!(!state
            .snapshot_manager()
            .serializable_commit_would_be_dangerous(
                first,
                &HashSet::from([("keys".into(), a.logical_id())]),
            ));
        state.commit(1, |_| Ok::<_, ()>(())).expect("first commits");
        assert_eq!(
            state
                .snapshot_manager()
                .serializable_commit_would_be_dangerous(
                    second,
                    &HashSet::from([("keys".into(), b.logical_id())]),
                ),
            visible,
            "hidden candidates must not create serializable read dependencies"
        );
        state.rollback(2).expect("second exits");
    }
}

fn bounded_scan_and_consolidation() {
    let manager = SegmentManager::with_config(
        "keys",
        ManagerConfig {
            max_sealed_segments: 1_000,
            consolidation_entities_per_tick: 17,
            ..Default::default()
        },
    );
    manager
        .bulk_insert((1..=600).map(|id| row(id, "key", 0, 0)).collect())
        .expect("flat source");
    manager.force_seal().expect("seal");
    for id in 1..=240 {
        manager.delete(EntityId::new(id)).expect("tombstone");
    }
    let source_bytes = manager.resident_bytes();
    let mut seen = vec![];
    let mut retained_bytes = 0;
    assert!(manager.scan_batches(
        None,
        || true,
        |batch| {
            assert!(batch.len() <= SCAN_BATCH_SIZE);
            seen.extend(batch.iter().map(|entity| entity.id.raw()));
            if retained_bytes == 0 {
                let mut ticks = 0;
                while manager.stats().consolidation.runs_completed == 0 {
                    manager.run_maintenance().expect("paced consolidation");
                    ticks += 1;
                    assert!(ticks < 1_000, "consolidation converges");
                }
                assert!(ticks > 1, "copying is paced");
                retained_bytes = manager.resident_bytes();
                assert!(
                    retained_bytes >= source_bytes,
                    "cursor pins retired payload"
                );
                manager.insert(row(900, "late", 0, 0)).expect("late append");
            }
            true
        }
    ));
    seen.sort_unstable();
    assert_eq!(
        seen,
        (241..=600).collect::<Vec<_>>(),
        "captured boundary survives relocation"
    );
    assert!(
        manager.resident_bytes() < retained_bytes,
        "last cursor releases source"
    );
    assert_eq!(manager.count(), 361);
    assert!(!manager.scan_batches(None, || false, |_| panic!("cancelled delivery")));
    assert_eq!(manager.stats().consolidation.tombstones_reclaimed, 240);
}

fn ordered_graph_candidates() {
    let managers: Vec<_> = [2, 7]
        .into_iter()
        .map(|ordinal| {
            let manager = Arc::new(SegmentManager::new("graph"));
            manager
                .bulk_insert(
                    (1..=600)
                        .map(|id| {
                            UnifiedEntity::graph_edge(
                                EntityId::new(id),
                                "step",
                                "2000",
                                "3000",
                                1.0,
                                HashMap::new(),
                            )
                        })
                        .collect(),
                )
                .expect("edges");
            manager.force_seal().expect("sealed edges");
            manager
                .insert(UnifiedEntity::graph_edge(
                    EntityId::new(5),
                    "step",
                    "2000",
                    "3000",
                    1.0,
                    HashMap::new(),
                ))
                .expect("duplicate physical candidate");
            (ordinal, manager)
        })
        .collect();
    let mut seen = vec![];
    let visit = |admit, consume| {
        SegmentManager::visit_ordered_graph_index_batches(
            &managers,
            GraphEntityKind::Edge,
            &[EntityId::new(2000)],
            || true,
            admit,
            consume,
        )
    };
    assert!(visit(
        |_| Ok::<_, ()>(()),
        |batch: &[(EntityId, usize)]| {
            assert!(batch.len() <= SCAN_BATCH_SIZE);
            seen.extend_from_slice(batch);
            Ok(true)
        }
    )
    .expect("ordered graph cursor"));
    let mut expected: Vec<_> = (1..=600)
        .chain(std::iter::once(5))
        .flat_map(|id| [(EntityId::new(id), 2), (EntityId::new(id), 7)])
        .collect();
    expected.sort_unstable();
    assert_eq!(
        seen, expected,
        "duplicates remain adjacent across collections/segments"
    );
    assert_eq!(
        SegmentManager::visit_ordered_graph_index_batches(
            &managers,
            GraphEntityKind::Edge,
            &[EntityId::new(2000)],
            || true,
            |_| Err("budget"),
            |_| panic!("denied delivery"),
        ),
        Err("budget")
    );
}
