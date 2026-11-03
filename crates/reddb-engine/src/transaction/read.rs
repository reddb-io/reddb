//! Explicit snapshot visibility for entity reads. Hosts retain auth, tenant,
//! moderation and ambient statement-frame policy around these helpers.
use std::sync::Arc;

/// Snapshot + manager pair used for read-path visibility checks.
///
/// The manager is needed in addition to the snapshot because `aborted`
/// state mutates after the snapshot is captured — a ROLLBACK by a
/// committed-at-capture-time writer must still hide its tuples. Keeping
/// the Arc around is O(pointer) and the RwLock reads on `is_aborted`
/// are cheap (HashSet lookup under a parking_lot read guard).
///
/// `own_xids` (Phase 2.3.2e) lists the xids belonging to the current
/// connection's transaction — the parent xid plus open and released
/// savepoint sub-xids. The visibility rule promotes rows stamped with
/// these xids to "always visible (unless aborted)" so the writer sees
/// its own nested-savepoint writes even though their xids exceed
/// `snapshot.xid`.
#[derive(Clone)]
pub struct SnapshotContext {
    pub snapshot: super::snapshot::Snapshot,
    pub manager: Arc<super::snapshot::SnapshotManager>,
    pub own_xids: std::collections::HashSet<super::snapshot::Xid>,
    pub requires_index_fallback: bool,
    pub serializable_reader: Option<super::snapshot::Xid>,
}

/// Record an admitted table-row read after the host has applied visibility and
/// policy gates. SSI keys use logical identity, shared by all physical versions.
pub fn record_serializable_read(
    ctx: &SnapshotContext,
    entity: &crate::storage::entity::UnifiedEntity,
) {
    let Some(reader) = ctx.serializable_reader else {
        return;
    };
    if !matches!(
        &entity.kind,
        crate::storage::entity::EntityKind::TableRow { .. }
    ) {
        return;
    }
    ctx.manager
        .record_serializable_read(reader, entity.kind.collection(), entity.logical_id());
}

/// Apply captured MVCC visibility, including own/aborted savepoint writers.
/// This predicate supplies no authorization, tenant or moderation policy.
#[inline]
pub fn visibility_check(ctx: &SnapshotContext, xmin: u64, xmax: u64) -> bool {
    // Writer aborted → tuple never existed from any future reader's view.
    // Checked *before* the own-xids fast path so an aborted own-sub-xid
    // (rolled-back savepoint) stays hidden from the parent.
    if xmin != 0 && ctx.manager.is_aborted(xmin) {
        return false;
    }
    // Deleter aborted → treat xmax as unset; fall back to xmin-only check.
    let effective_xmax = if xmax != 0 && ctx.manager.is_aborted(xmax) {
        0
    } else {
        xmax
    };
    // Phase 2.3.2e: own-tx writes are always visible to the connection
    // that stamped them, even when xmin/xmax exceed `snapshot.xid` (as
    // happens for sub-xids allocated by SAVEPOINT after BEGIN).
    let own_xmin = xmin != 0 && ctx.own_xids.contains(&xmin);
    let own_xmax = effective_xmax != 0 && ctx.own_xids.contains(&effective_xmax);
    if own_xmax {
        // This connection deleted the row via this xid — hide it from self.
        return false;
    }
    if own_xmin {
        return true;
    }
    ctx.snapshot.sees(xmin, effective_xmax)
}

/// Candidate access for the canonical table-row version selector. The store
/// supplies only physical versions of the requested collection/logical identity.
pub trait TableRowVersionStore {
    fn table_row_versions_by_logical_id(
        &self,
        collection: &str,
        logical_id: crate::storage::entity::EntityId,
    ) -> Vec<crate::storage::entity::UnifiedEntity>;

    fn get_table_row_by_logical_id(
        &self,
        collection: &str,
        logical_id: crate::storage::entity::EntityId,
    ) -> Option<crate::storage::entity::UnifiedEntity>;
}

/// Select the newest visible physical version, or use the current-row lookup
/// when the host has no snapshot requiring history. The host's predicate retains
/// table-kind, authorization and moderation gates before materialization.
pub fn resolve_logical_id(
    store: &impl TableRowVersionStore,
    collection: &str,
    logical_id: crate::storage::entity::EntityId,
    select_versions: bool,
    mut visible: impl FnMut(&crate::storage::entity::UnifiedEntity) -> bool,
) -> Option<crate::storage::entity::UnifiedEntity> {
    if select_versions {
        return store
            .table_row_versions_by_logical_id(collection, logical_id)
            .into_iter()
            .filter(|entity| visible(entity))
            .max_by_key(|entity| entity.xmin);
    }
    store
        .get_table_row_by_logical_id(collection, logical_id)
        .filter(|entity| visible(entity))
}

/// Fallible version access streams candidates instead of retaining a history Vec.
/// Hosts supply only versions of the requested collection/logical identity.
pub trait FallibleTableRowVersionStore {
    type Error;

    fn try_get_table_row_by_logical_id(
        &self,
        collection: &str,
        logical_id: crate::storage::entity::EntityId,
    ) -> Result<Option<crate::storage::entity::UnifiedEntity>, Self::Error>;

    fn try_for_each_table_row_version(
        &self,
        collection: &str,
        logical_id: crate::storage::entity::EntityId,
        visit: impl FnMut(&crate::storage::entity::UnifiedEntity) -> bool,
    ) -> Result<(), Self::Error>;
}

/// Retain one selected version; a later read failure invalidates the selection.
pub fn try_resolve_logical_id<S: FallibleTableRowVersionStore>(
    store: &S,
    collection: &str,
    logical_id: crate::storage::entity::EntityId,
    select_versions: bool,
    mut visible: impl FnMut(&crate::storage::entity::UnifiedEntity) -> bool,
) -> Result<Option<crate::storage::entity::UnifiedEntity>, S::Error> {
    if !select_versions {
        return store
            .try_get_table_row_by_logical_id(collection, logical_id)
            .map(|entity| entity.filter(|entity| visible(entity)));
    }
    let mut selected: Option<crate::storage::entity::UnifiedEntity> = None;
    store.try_for_each_table_row_version(collection, logical_id, |entity| {
        if visible(entity) && selected.as_ref().is_none_or(|old| entity.xmin >= old.xmin) {
            selected = Some(entity.clone());
        }
        true
    })?;
    Ok(selected)
}
