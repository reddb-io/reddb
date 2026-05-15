use crate::storage::unified::entity::{EntityKind, UnifiedEntity};

/// Table-row-only MVCC read resolver for the current statement.
///
/// Candidate discovery stays with each caller. This resolver owns the
/// table-row visibility decision before a candidate is materialized.
#[derive(Clone)]
pub(crate) struct TableRowMvccReadResolver {
    snapshot: TableRowReadSnapshot,
}

#[derive(Clone)]
enum TableRowReadSnapshot {
    CurrentThread,
    Captured(Option<super::impl_core::SnapshotContext>),
}

impl TableRowMvccReadResolver {
    pub(crate) fn current_statement() -> Self {
        Self {
            snapshot: TableRowReadSnapshot::CurrentThread,
        }
    }

    pub(crate) fn captured(snapshot: Option<super::impl_core::SnapshotContext>) -> Self {
        Self {
            snapshot: TableRowReadSnapshot::Captured(snapshot),
        }
    }

    pub(crate) fn resolve_candidate<'a>(
        &self,
        candidate: &'a UnifiedEntity,
    ) -> Option<&'a UnifiedEntity> {
        if !matches!(candidate.kind, EntityKind::TableRow { .. }) {
            return None;
        }
        if self.visible(candidate) {
            Some(candidate)
        } else {
            None
        }
    }

    fn visible(&self, candidate: &UnifiedEntity) -> bool {
        match &self.snapshot {
            TableRowReadSnapshot::CurrentThread => {
                super::impl_core::entity_visible_under_current_snapshot(candidate)
            }
            TableRowReadSnapshot::Captured(snapshot) => {
                super::impl_core::entity_visible_with_context(snapshot.as_ref(), candidate)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;
    use crate::storage::schema::Value;
    use crate::storage::transaction::snapshot::{Snapshot, SnapshotManager};
    use crate::storage::unified::entity::{EntityId, UnifiedEntity};

    fn row_with_xids(xmin: u64, xmax: u64) -> UnifiedEntity {
        let mut row =
            UnifiedEntity::table_row(EntityId::new(1), "accounts", 1, vec![Value::Integer(1)]);
        row.set_xmin(xmin);
        row.set_xmax(xmax);
        row
    }

    fn snapshot_context(xid: u64) -> super::super::impl_core::SnapshotContext {
        super::super::impl_core::SnapshotContext {
            snapshot: Snapshot {
                xid,
                in_progress: HashSet::new(),
            },
            manager: Arc::new(SnapshotManager::new()),
            own_xids: HashSet::new(),
            requires_index_fallback: false,
        }
    }

    #[test]
    fn current_row_fallback_keeps_live_legacy_rows_visible() {
        super::super::impl_core::clear_current_snapshot();
        let resolver = TableRowMvccReadResolver::current_statement();
        let row = row_with_xids(0, 0);

        assert!(resolver.resolve_candidate(&row).is_some());
    }

    #[test]
    fn current_row_fallback_hides_tombstoned_rows() {
        super::super::impl_core::clear_current_snapshot();
        let resolver = TableRowMvccReadResolver::current_statement();
        let row = row_with_xids(0, 4);

        assert!(resolver.resolve_candidate(&row).is_none());
    }

    #[test]
    fn captured_snapshot_applies_xmin_and_xmax_visibility() {
        let resolver = TableRowMvccReadResolver::captured(Some(snapshot_context(10)));

        assert!(resolver.resolve_candidate(&row_with_xids(5, 0)).is_some());
        assert!(resolver.resolve_candidate(&row_with_xids(11, 0)).is_none());
        assert!(resolver.resolve_candidate(&row_with_xids(5, 9)).is_none());
        assert!(resolver.resolve_candidate(&row_with_xids(5, 11)).is_some());
    }
}
