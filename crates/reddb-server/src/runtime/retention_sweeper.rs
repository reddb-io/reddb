//! Issue #584 — DeclarativeRetention slice 12.
//!
//! Background sweeper that physically removes rows whose timestamp column has fallen beyond the collection's
//! retention window. The
//! lazy-on-scan filter from slice 11 (`retention_filter`) hides
//! expired rows from reads the moment a policy is set; this slice
//! complements that filter with a low-priority background task that
//! reclaims storage in bounded batches.
//!
//! Mutable and append-only collection sweeps execute deletes through the standard
//! `RedDBRuntime::execute_query` chokepoint (`DELETE FROM <collection>
//! WHERE <timestamp> < <cutoff>`) so that WAL participation, snapshot guards and
//! event emission ride on the same single code path as user-issued
//! DELETEs — replicas replay sweeper deletes deterministically with
//! no special handling on the replication side. The scoped maintenance
//! context permits retention deletes on append-only collections while manual
//! DELETE remains prohibited. No external segment manifest is published.
//!
//! Per-collection runtime state (`last_sweep_at_ms`, `rows_swept_total`,
//! `segments_retired_total`, `last_pending_estimate`) lives on
//! `RuntimeInner::retention_sweeper` and is surfaced via the sweeper
//! columns on `red.retention`.
//! State is in-memory only — counters reset across restart, mirroring
//! the existing materialized-view scheduler state.

use std::cell::Cell;
use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static DELETE_LIMIT: Cell<Option<usize>> = const { Cell::new(None) };
    static TIMESTAMP_CONTEXT: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    static DELETE_CANDIDATES: RefCell<Option<Vec<crate::storage::EntityId>>> = const { RefCell::new(None) };
}

pub(crate) fn with_delete_candidates<T>(
    candidates: Vec<crate::storage::EntityId>,
    collection: &str,
    column: &str,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Vec<crate::storage::EntityId>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DELETE_CANDIDATES.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let limit = candidates.len();
    let previous = DELETE_CANDIDATES.with(|slot| slot.replace(Some(candidates)));
    let _restore = Restore(previous);
    with_delete_limit(limit, collection, column, f)
}

/// Only the engine's scoped retention statement may delete append-only rows.
/// The collection identity prevents the exemption leaking into trigger writes.
pub(crate) fn is_retention_delete(collection: &str) -> bool {
    DELETE_CANDIDATES.with(|slot| slot.borrow().is_some())
        && TIMESTAMP_CONTEXT.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|(name, _)| name == collection)
        })
}

pub(crate) fn take_delete_candidates() -> Option<Vec<crate::storage::EntityId>> {
    DELETE_CANDIDATES.with(|slot| slot.borrow_mut().take())
}

/// Scope the limit to the synchronous maintenance statement and restore it on
/// every exit, including unwinding. The DELETE consumes it before publication.
pub(crate) fn with_delete_limit<T>(
    limit: usize,
    collection: &str,
    column: &str,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<usize>, Option<(String, String)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            DELETE_LIMIT.with(|slot| slot.set(self.0));
            TIMESTAMP_CONTEXT.with(|slot| *slot.borrow_mut() = self.1.take());
        }
    }
    let previous = DELETE_LIMIT.with(|slot| slot.replace(Some(limit)));
    let previous_context = TIMESTAMP_CONTEXT
        .with(|slot| slot.replace(Some((collection.to_owned(), column.to_owned()))));
    let _restore = Restore(previous, previous_context);
    f()
}

pub(crate) fn take_delete_limit() -> Option<usize> {
    DELETE_LIMIT.with(Cell::take)
}

pub(crate) fn cutoff_ms(now_ms: u64, retention_ms: u64) -> i64 {
    let cutoff = i128::from(now_ms) - i128::from(retention_ms);
    cutoff.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

/// Resolve automatic timestamps in milliseconds only within the scoped
/// maintenance DELETE. Missing legacy values use the conservative end of the
/// stored second; malformed values do not qualify for deletion.
pub(crate) fn maintenance_timestamp(
    entity: &crate::storage::UnifiedEntity,
    field: &str,
) -> Option<reddb_types::Value> {
    let active = TIMESTAMP_CONTEXT.with(|slot| {
        slot.borrow().as_ref().is_some_and(|(collection, column)| {
            collection == entity.kind.collection()
                && column == field
                && matches!(field, "created_at" | "updated_at")
        })
    });
    if !active {
        return None;
    }
    let value = entity.data.as_row().and_then(|row| row.get_field(field));
    let millis = match value {
        Some(reddb_types::Value::TimestampMs(value))
        | Some(reddb_types::Value::BigInt(value))
        | Some(reddb_types::Value::Integer(value)) => Some(*value),
        Some(reddb_types::Value::Timestamp(value)) => value.checked_mul(1_000),
        Some(reddb_types::Value::UnsignedInteger(value)) => i64::try_from(*value).ok(),
        Some(_) => None,
        None => {
            let seconds = if field == "created_at" {
                entity.created_at
            } else {
                entity.updated_at
            };
            i64::try_from(seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(1_000))
                .and_then(|millis| millis.checked_add(999))
        }
    };
    Some(
        millis
            .map(reddb_types::Value::Integer)
            .unwrap_or(reddb_types::Value::Null),
    )
}

/// Default per-tick batch size for the background sweeper. Acceptance
/// criterion: `default batch size red.retention.sweeper_batch (e.g.
/// 1000)`. Kept low enough that one tick is bounded work — the
/// sweeper never holds locks long enough to block the write path.
pub(crate) const DEFAULT_SWEEPER_BATCH: usize = 1_000;

/// Per-collection sweeper state surfaced on `red.retention`.
#[derive(Debug, Clone, Default)]
pub(crate) struct SweeperState {
    /// Wall-clock millis of the last sweep attempt — `0` until the
    /// sweeper first ticked the collection.
    pub last_sweep_at_ms: u64,
    /// Cumulative rows reclaimed since boot.
    pub rows_swept_total: u64,
    /// Cumulative append-only segments retired since boot.
    pub segments_retired_total: u64,
    /// Conservative signal that a full batch or an error may have left
    /// work for the next tick. This is not an exact backlog count.
    pub last_pending_estimate: u64,
}

/// Per-runtime sweeper state map. Keyed by collection name.
#[derive(Default)]
pub(crate) struct RetentionSweeperState {
    states: HashMap<String, SweeperState>,
    cursors: HashMap<String, crate::storage::unified::store::MvccReclaimCursor>,
    next_collection: Option<String>,
}

impl RetentionSweeperState {
    pub(crate) fn new() -> Self {
        Self {
            states: HashMap::new(),
            cursors: HashMap::new(),
            next_collection: None,
        }
    }

    pub(crate) fn scan_tick(
        &mut self,
        store: &crate::storage::unified::store::UnifiedStore,
        collection: &str,
        budget: usize,
    ) -> Result<(Vec<crate::storage::EntityId>, usize), crate::storage::StoreError> {
        if !self.cursors.contains_key(collection) {
            let Some(cursor) = store.mvcc_reclaim_cursor(collection.to_owned()) else {
                return Ok((Vec::new(), 0));
            };
            self.cursors.insert(collection.to_owned(), cursor);
        }
        let cursor = self
            .cursors
            .get_mut(collection)
            .expect("cursor was inserted");
        let result = store.retention_scan_tick(cursor, budget)?;
        if cursor.finished() {
            self.cursors.remove(collection);
        }
        Ok(result)
    }

    pub(crate) fn next_collection(&self) -> Option<&str> {
        self.next_collection.as_deref()
    }

    pub(crate) fn resume_at(&mut self, collection: String) {
        self.next_collection = Some(collection);
    }

    /// Snapshot of every tracked collection's sweeper state.
    pub(crate) fn snapshot(&self) -> Vec<(String, SweeperState)> {
        self.states
            .iter()
            .map(|(name, state)| (name.clone(), state.clone()))
            .collect()
    }

    /// Lookup the sweeper state for `collection`. Returns a fresh
    /// `SweeperState::default()` when the collection has never been
    /// ticked — keeps the call-site free of `Option` plumbing.
    pub(crate) fn get(&self, collection: &str) -> SweeperState {
        self.states.get(collection).cloned().unwrap_or_default()
    }

    /// Record the outcome of a sweeper tick.
    pub(crate) fn record_tick(
        &mut self,
        collection: &str,
        rows_swept: u64,
        pending_estimate: u64,
        at_unix_ms: u64,
    ) {
        let entry = self.states.entry(collection.to_string()).or_default();
        entry.last_sweep_at_ms = at_unix_ms;
        entry.rows_swept_total = entry.rows_swept_total.saturating_add(rows_swept);
        entry.last_pending_estimate = pending_estimate;
    }

    #[cfg(test)]
    pub(crate) fn record_segments_retired(
        &mut self,
        collection: &str,
        segments_retired: u64,
        at_unix_ms: u64,
    ) {
        let entry = self.states.entry(collection.to_string()).or_default();
        entry.last_sweep_at_ms = at_unix_ms;
        entry.segments_retired_total = entry
            .segments_retired_total
            .saturating_add(segments_retired);
    }

    /// Drop bookkeeping for a collection (DROP TABLE / DROP COLLECTION).
    pub(crate) fn forget(&mut self, collection: &str) {
        self.states.remove(collection);
        self.cursors.remove(collection);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maintenance_delete_limit_is_consumed_and_restored_on_unwind() {
        assert_eq!(take_delete_limit(), None);
        with_delete_limit(2, "events", "created_at", || {
            assert_eq!(take_delete_limit(), Some(2));
            assert_eq!(take_delete_limit(), None);
            let _ = std::panic::catch_unwind(|| {
                with_delete_limit(3, "events", "created_at", || panic!("test unwind"))
            });
            assert_eq!(take_delete_limit(), None);
        });
        assert_eq!(take_delete_limit(), None);
    }

    #[test]
    fn retention_cutoff_never_wraps_long_durations() {
        assert_eq!(cutoff_ms(10, 3), 7);
        assert_eq!(cutoff_ms(10, u64::MAX), i64::MIN);
        assert_eq!(cutoff_ms(u64::MAX, 0), i64::MAX);
    }

    #[test]
    fn legacy_timestamp_fallback_is_conservative_and_scoped() {
        use crate::storage::{EntityId, UnifiedEntity};
        let mut entity = UnifiedEntity::table_row(EntityId::new(1), "events", 1, vec![]);
        entity.created_at = 10;
        assert_eq!(maintenance_timestamp(&entity, "created_at"), None);
        with_delete_limit(1, "events", "created_at", || {
            assert_eq!(
                maintenance_timestamp(&entity, "created_at"),
                Some(reddb_types::Value::Integer(10_999))
            );
            assert_eq!(maintenance_timestamp(&entity, "updated_at"), None);
        });
        assert_eq!(maintenance_timestamp(&entity, "created_at"), None);
    }

    #[test]
    fn record_tick_accumulates_rows_swept_total() {
        let mut state = RetentionSweeperState::new();
        state.record_tick("events", 100, 50, 1_000);
        state.record_tick("events", 50, 0, 2_000);
        let s = state.get("events");
        assert_eq!(s.rows_swept_total, 150);
        assert_eq!(s.segments_retired_total, 0);
        assert_eq!(s.last_pending_estimate, 0);
        assert_eq!(s.last_sweep_at_ms, 2_000);
    }

    #[test]
    fn record_segments_retired_accumulates_without_rows() {
        let mut state = RetentionSweeperState::new();
        state.record_segments_retired("events", 1, 1_000);
        state.record_segments_retired("events", 2, 2_000);
        let s = state.get("events");
        assert_eq!(s.rows_swept_total, 0);
        assert_eq!(s.segments_retired_total, 3);
        assert_eq!(s.last_sweep_at_ms, 2_000);
    }

    #[test]
    fn get_unknown_collection_returns_zeroed_state() {
        let state = RetentionSweeperState::new();
        let s = state.get("missing");
        assert_eq!(s.last_sweep_at_ms, 0);
        assert_eq!(s.rows_swept_total, 0);
        assert_eq!(s.last_pending_estimate, 0);
    }

    #[test]
    fn forget_removes_bookkeeping() {
        let mut state = RetentionSweeperState::new();
        state.record_tick("events", 10, 0, 1);
        state.forget("events");
        assert_eq!(state.get("events").rows_swept_total, 0);
    }
}
