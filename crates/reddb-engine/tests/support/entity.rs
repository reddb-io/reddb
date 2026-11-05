//! Entity representation and MVCC read conformance, shared with browser workers.
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use reddb_engine::storage::entity::{
    compute_entity_field_bloom, field_name_bloom, CrossRef, EmbeddingSlot, EntityData, EntityId,
    EntityKind, RefType, RowData, SparseVector, UnifiedEntity, VectorData,
};
use reddb_engine::transaction::{
    read::{
        record_serializable_read, resolve_logical_id, visibility_check, SnapshotContext,
        TableRowVersionStore,
    },
    state::TransactionState,
    IsolationLevel, SnapshotManager,
};
use reddb_types::Value;

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

fn row(id: u64, logical_id: u64, xmin: u64, xmax: u64) -> UnifiedEntity {
    let mut entity = UnifiedEntity::table_row(
        EntityId::new(id),
        "accounts",
        logical_id,
        vec![Value::Integer(7)],
    );
    entity.set_logical_id(EntityId::new(logical_id));
    entity.set_xmin(xmin);
    entity.set_xmax(xmax);
    entity
}

fn entity_models_and_metadata() {
    let mut row = RowData::new(vec![Value::Integer(7), Value::text("alice")]);
    row.schema = Some(Arc::new(vec!["count".into(), "name".into()]));
    assert_eq!(row.get_field("name"), Some(&Value::text("alice")));
    assert_eq!(row.iter_fields().count(), 2);
    assert_eq!(row.get_field("absent"), None);
    row.named = Some(HashMap::from([("count".into(), Value::Integer(8))]));
    assert_eq!(
        row.get_field("count"),
        Some(&Value::Integer(8)),
        "named fields retain precedence"
    );
    assert_eq!(row.iter_fields().count(), 1);

    let mut table = UnifiedEntity::table_row(EntityId::new(1024), "accounts", 1, vec![]);
    assert_eq!(table.logical_id(), table.id);
    assert!(!table.has_explicit_logical_id());
    table.ensure_table_logical_id();
    assert!(table.has_explicit_logical_id());
    table.set_logical_id(EntityId::new(1023));
    table.ensure_table_logical_id();
    assert_eq!(table.logical_id().raw(), 1023);
    let mut node =
        UnifiedEntity::graph_node(EntityId::new(1025), "people", "person", HashMap::new());
    node.ensure_table_logical_id();
    assert_eq!(node.kind.collection(), "people");
    assert!(node.has_explicit_logical_id());
    let edge =
        UnifiedEntity::graph_edge(EntityId::new(1026), "knows", "a", "b", 1.5, HashMap::new());
    assert!(matches!(&edge.kind, EntityKind::GraphEdge(edge) if edge.weight == 1500));
    let mut vector = UnifiedEntity::vector(EntityId::new(1027), "embeddings", vec![0.1, 0.2]);
    vector.ensure_table_logical_id();
    assert!(
        !vector.has_explicit_logical_id(),
        "vector history rollout remains separate"
    );
    let hybrid = VectorData::with_sparse(vec![1.0], SparseVector::new(vec![3], vec![2.0], 8));
    assert!(hybrid.is_hybrid());
    assert_eq!(hybrid.sparse.expect("hybrid sparse component").get(3), 2.0);
}

fn entity_document_field_bloom() {
    let mut body = Vec::new();
    reddb_types::document_body_codec::encode(
        &[
            ("score", &Value::Integer(42)),
            ("name", &Value::text("alice")),
        ],
        &mut body,
    )
    .expect("canonical document container encodes");
    let data = EntityData::Row(RowData::with_names(
        vec![Value::Json(body.clone())],
        vec!["body".into()],
    ));
    assert_eq!(
        compute_entity_field_bloom(&data),
        field_name_bloom("body") | field_name_bloom("score") | field_name_bloom("name")
    );
    for bytes in [b"{\"score\":42}".to_vec(), body[..5].to_vec(), vec![]] {
        let data = EntityData::Row(RowData::with_names(
            vec![Value::Json(bytes)],
            vec!["body".into()],
        ));
        assert_eq!(compute_entity_field_bloom(&data), field_name_bloom("body"));
    }
    let mut row = RowData::new(vec![Value::Integer(42)]);
    row.schema = Some(Arc::new(vec!["score".into()]));
    assert_eq!(compute_entity_field_bloom(&EntityData::Row(row)), 0);
    assert_eq!(field_name_bloom(""), 0);
}

fn entity_auxiliary_and_timestamps() {
    let mut entity = UnifiedEntity::table_row(EntityId::new(1024), "accounts", 1, vec![]);
    assert!(!entity.has_aux());
    assert!(entity.embeddings().is_empty());
    assert!(entity.cross_refs().is_empty());
    assert_eq!(entity.updated_at, entity.created_at);
    entity.add_embedding(EmbeddingSlot::new("content", vec![0.1, 0.2], "fixture"));
    entity.add_cross_ref(CrossRef::with_weight(
        entity.id,
        EntityId::new(1025),
        "people",
        RefType::RowToNode,
        0.75,
    ));
    assert!(entity.has_aux());
    assert!(!entity.is_stale(u64::MAX));
    assert_eq!(
        entity
            .get_embedding("content")
            .expect("embedding exists")
            .dimension,
        2
    );
    assert_eq!(entity.cross_refs()[0].weight, 0.75);
    assert_eq!(
        entity.cross_refs()[0].ref_type.inverse(),
        Some(RefType::NodeToRow)
    );
    let mut cloned = entity.clone();
    cloned.embeddings_mut().clear();
    assert_eq!(
        entity.embeddings().len(),
        1,
        "auxiliary mutation is clone-local"
    );
}

fn entity_own_subtransaction_visibility() {
    let state = TransactionState::new();
    let parent = state.begin(1, IsolationLevel::SnapshotIsolation);
    let released = state.savepoint(1, "released").expect("active transaction");
    state
        .release_savepoint(1, "released")
        .expect("savepoint exists");
    let child = state.savepoint(1, "child").expect("active transaction");
    let ctx = context(&state, 1, false);
    assert!(child > ctx.snapshot.xid);
    assert!(visibility_check(&ctx, released, 0));
    assert!(visibility_check(&ctx, child, 0));
    assert!(
        !visibility_check(&ctx, parent, child),
        "own delete hides a row"
    );
    state
        .rollback_to_savepoint(1, "child")
        .expect("savepoint exists");
    assert!(
        !visibility_check(&ctx, child, 0),
        "aborted own creator stays hidden even in a captured own-xid set"
    );
    assert!(
        visibility_check(&ctx, parent, child),
        "aborted own delete restores visibility"
    );
    assert!(visibility_check(&ctx, 0, 0));
    state.rollback(1).expect("parent exists");
    assert!(!visibility_check(&ctx, released, 0));
}

struct Versions {
    history: Vec<UnifiedEntity>,
    current: UnifiedEntity,
    history_reads: Cell<usize>,
}

impl TableRowVersionStore for Versions {
    fn table_row_versions_by_logical_id(
        &self,
        collection: &str,
        logical_id: EntityId,
    ) -> Vec<UnifiedEntity> {
        self.history_reads.set(self.history_reads.get() + 1);
        self.history
            .iter()
            .filter(|entity| {
                entity.kind.collection() == collection && entity.logical_id() == logical_id
            })
            .cloned()
            .collect()
    }
    fn get_table_row_by_logical_id(
        &self,
        collection: &str,
        logical_id: EntityId,
    ) -> Option<UnifiedEntity> {
        (self.current.kind.collection() == collection && self.current.logical_id() == logical_id)
            .then(|| self.current.clone())
    }
}

fn entity_logical_version_selection() {
    let state = TransactionState::new();
    let creator = state.begin(1, IsolationLevel::SnapshotIsolation);
    state
        .commit(1, |_| Ok::<_, ()>(()))
        .expect("creator commits");
    let writer = state.begin(1, IsolationLevel::SnapshotIsolation);
    state.begin(2, IsolationLevel::SnapshotIsolation);
    let captured = context(&state, 2, false);
    let old = row(1024, 1024, creator, writer);
    let new = row(1025, 1024, writer, 0);
    let versions = Versions {
        history: vec![new.clone(), row(2048, 2048, creator, 0), old],
        current: new,
        history_reads: Cell::new(0),
    };
    let select = |ctx: &SnapshotContext, history, allowed: bool| {
        resolve_logical_id(
            &versions,
            "accounts",
            EntityId::new(1024),
            history,
            |entity| {
                allowed
                    && matches!(entity.kind, EntityKind::TableRow { .. })
                    && visibility_check(ctx, entity.xmin, entity.xmax)
            },
        )
    };
    assert_eq!(
        select(&captured, true, true)
            .expect("old version visible")
            .id
            .raw(),
        1024
    );
    state
        .commit(1, |_| Ok::<_, ()>(()))
        .expect("writer commits");
    assert_eq!(
        select(&captured, true, true)
            .expect("captured exclusions stay fixed")
            .id
            .raw(),
        1024
    );
    let fresh = SnapshotContext {
        snapshot: state.snapshot_manager().fresh_read_snapshot(),
        ..captured.clone()
    };
    assert_eq!(
        select(&fresh, true, true)
            .expect("latest version visible")
            .id
            .raw(),
        1025
    );
    assert!(
        select(&fresh, true, false).is_none(),
        "host gates run before version selection"
    );
    let reads = versions.history_reads.get();
    assert_eq!(
        select(&fresh, false, true)
            .expect("current row visible")
            .id
            .raw(),
        1025
    );
    assert_eq!(
        versions.history_reads.get(),
        reads,
        "current lookup does not materialize history"
    );
    assert!(
        select(&captured, false, true).is_none(),
        "no historical fallback without host request"
    );
    state.rollback(2).expect("reader exists");
}

fn entity_serializable_logical_identity() {
    for (track, table) in [(true, true), (false, true), (true, false)] {
        let state = TransactionState::new();
        let first = state.begin(1, IsolationLevel::Serializable);
        let second = state.begin(2, IsolationLevel::Serializable);
        let a = row(9000, 1024, 0, 0);
        let mut b = row(9001, 1025, 0, 0);
        if !table {
            b = UnifiedEntity::graph_node(b.id, "accounts", "person", HashMap::new());
        }
        record_serializable_read(&context(&state, 1, track), &b);
        record_serializable_read(&context(&state, 2, true), &a);
        let manager: Arc<SnapshotManager> = state.snapshot_manager();
        assert!(!manager.serializable_commit_would_be_dangerous(
            first,
            &HashSet::from([("accounts".into(), a.logical_id())])
        ));
        state.commit(1, |_| Ok::<_, ()>(())).expect("first commits");
        assert_eq!(
            manager.serializable_commit_would_be_dangerous(
                second,
                &HashSet::from([("accounts".into(), EntityId::new(1025))])
            ),
            track && table
        );
        state.rollback(2).expect("second exits");
    }
}

pub const CASES: &[(&str, fn())] = &[
    ("entity_models_and_metadata", entity_models_and_metadata),
    ("entity_document_field_bloom", entity_document_field_bloom),
    (
        "entity_auxiliary_and_timestamps",
        entity_auxiliary_and_timestamps,
    ),
    (
        "entity_own_subtransaction_visibility",
        entity_own_subtransaction_visibility,
    ),
    (
        "entity_logical_version_selection",
        entity_logical_version_selection,
    ),
    (
        "entity_serializable_logical_identity",
        entity_serializable_logical_identity,
    ),
];
