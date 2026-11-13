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
    pub branch: Option<Arc<BranchReadView>>,
    pub snapshot: super::snapshot::Snapshot,
    pub manager: Arc<super::snapshot::SnapshotManager>,
    pub own_xids: std::collections::HashSet<super::snapshot::Xid>,
    pub requires_index_fallback: bool,
    pub serializable_reader: Option<super::snapshot::Xid>,
}

/// Each inherited epoch is frozen at its fork/merge snapshot. Only the current
/// writer epoch follows the connection's live transaction snapshot.
#[derive(Clone, Debug)]
pub struct BranchReadView {
    pub writer_epoch: u64,
    pub epochs: std::collections::BTreeMap<u64, super::snapshot::Snapshot>,
}

/// Unique keys reserve pending writes in the current epoch, but ancestor
/// epochs keep their frozen fork view. A sibling branch never reserves a key.
pub fn entity_reserves_unique_key(
    ctx: &SnapshotContext,
    entity: &crate::storage::entity::UnifiedEntity,
) -> bool {
    let (Some(view), Some(version)) = (&ctx.branch, entity.branch_version()) else {
        return ctx
            .manager
            .row_reserves_unique_key(entity.xmin, entity.xmax, &ctx.own_xids);
    };
    let inserted = if version.origin == view.writer_epoch {
        ctx.manager
            .row_reserves_unique_key(entity.xmin, 0, &ctx.own_xids)
    } else {
        view.epochs.get(&version.origin).is_some_and(|snapshot| {
            !ctx.manager.is_aborted(entity.xmin) && snapshot.sees(entity.xmin, 0)
        })
    };
    if !inserted {
        return false;
    }
    let deleted = |epoch, xid| {
        if xid == 0 {
            return false;
        }
        if epoch == view.writer_epoch {
            !ctx.manager.row_reserves_unique_key(0, xid, &ctx.own_xids)
        } else {
            view.epochs
                .get(&epoch)
                .is_some_and(|snapshot| !ctx.manager.is_aborted(xid) && snapshot.sees(xid, 0))
        }
    };
    !deleted(version.origin, entity.xmax)
        && !version
            .deletions
            .iter()
            .any(|&(epoch, xid)| deleted(epoch, xid))
}

pub fn entity_visibility_check(
    ctx: &SnapshotContext,
    entity: &crate::storage::entity::UnifiedEntity,
) -> bool {
    let (Some(view), Some(version)) = (&ctx.branch, entity.branch_version()) else {
        return visibility_check(ctx, entity.xmin, entity.xmax);
    };
    let visible_xid = |epoch, xid| {
        view.epochs.get(&epoch).is_some_and(|snapshot| {
            xid == 0
                || (!ctx.manager.is_aborted(xid)
                    && (ctx.own_xids.contains(&xid) || snapshot.sees(xid, 0)))
        })
    };
    visible_xid(version.origin, entity.xmin)
        && (entity.xmax == 0 || !visible_xid(version.origin, entity.xmax))
        && !version
            .deletions
            .iter()
            .any(|&(epoch, xid)| visible_xid(epoch, xid))
}

#[cfg(test)]
mod branch_tests {
    use super::*;
    use crate::storage::entity::{EntityId, UnifiedEntity};
    use crate::transaction::snapshot::{Snapshot, SnapshotManager};
    use std::collections::{BTreeMap, HashSet};

    fn context(epoch: u64, inherited: u64) -> SnapshotContext {
        let mut epochs = BTreeMap::from([(
            0,
            Snapshot {
                xid: inherited,
                in_progress: HashSet::new(),
            },
        )]);
        epochs.insert(
            epoch,
            Snapshot {
                xid: 100,
                in_progress: HashSet::new(),
            },
        );
        SnapshotContext {
            branch: Some(Arc::new(BranchReadView {
                writer_epoch: epoch,
                epochs,
            })),
            snapshot: Snapshot {
                xid: 100,
                in_progress: HashSet::new(),
            },
            manager: Arc::new(SnapshotManager::new()),
            own_xids: HashSet::new(),
            requires_index_fallback: true,
            serializable_reader: None,
        }
    }

    #[test]
    fn sibling_updates_and_deletes_have_independent_visibility() {
        let mut base = UnifiedEntity::table_row(EntityId::new(1), "items", 1, vec![]);
        base.set_xmin(10);
        base.set_branch_origin(0);
        base.set_branch_xmax(0, 30);
        base.set_branch_xmax(1, 40);
        let main = context(0, 100);
        let first = context(1, 20);
        let second = context(2, 20);
        assert!(!entity_visibility_check(&main, &base));
        assert!(!entity_visibility_check(&first, &base));
        assert!(entity_visibility_check(&second, &base));
        base.set_branch_xmax(2, 50);
        base.set_branch_xmax(1, 0);
        assert!(
            entity_visibility_check(&first, &base),
            "rollback only revives its branch"
        );
        assert!(!entity_visibility_check(&second, &base));
        let mut replacement = base.clone();
        replacement.set_branch_origin(1);
        replacement.set_xmin(40);
        replacement.set_xmax(0);
        assert!(entity_visibility_check(&first, &replacement));
        assert!(!entity_visibility_check(&main, &replacement));
        assert!(!entity_visibility_check(&second, &replacement));
    }

    #[test]
    fn active_and_aborted_branch_deletes_remain_invisible() {
        let mut entity = UnifiedEntity::table_row(EntityId::new(1), "items", 1, vec![]);
        entity.set_branch_origin(0);
        entity.set_xmin(10);
        entity.set_branch_xmax(1, 40);
        let mut reader = context(1, 20);
        Arc::make_mut(reader.branch.as_mut().expect("view"))
            .epochs
            .get_mut(&1)
            .expect("writer epoch")
            .in_progress
            .insert(40);
        assert!(entity_visibility_check(&reader, &entity));
        reader.own_xids.insert(40);
        assert!(!entity_visibility_check(&reader, &entity));
        reader.manager.rollback(40);
        assert!(entity_visibility_check(&reader, &entity));
    }
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
