//! Issue #584 — DeclarativeRetention slice 12.
//!
//! Background sweeper that physically removes rows whose timestamp
//! column has fallen beyond the collection's retention window. The
//! lazy-on-scan filter from slice 11 (`retention_filter`) hides
//! expired rows from reads the moment a policy is set; this slice
//! complements that filter with a low-priority background task that
//! reclaims storage in bounded batches.
//!
//! The sweeper executes deletes through the standard
//! `RedDBRuntime::execute_query` chokepoint (`DELETE FROM <collection>
//! WHERE id IN (...)`) so that WAL participation, snapshot guards and
//! event emission ride on the same single code path as user-issued
//! DELETEs — replicas replay sweeper deletes deterministically with
//! no special handling on the replication side.
//!
//! Per-collection runtime state (`last_sweep_at_ms`, `rows_swept_total`,
//! `last_pending_estimate`) lives on `RuntimeInner::retention_sweeper`
//! and is surfaced via the three extra columns on `red.retention`.
//! State is in-memory only — counters reset across restart, mirroring
//! the existing materialized-view scheduler state.

use std::collections::HashMap;

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
    /// Number of rows the last tick observed as expired *but not yet
    /// swept* — i.e. either still inside the batch or queued for the
    /// next tick. Surfaced as
    /// `red.retention.current_rows_pending_sweep_estimate`.
    pub last_pending_estimate: u64,
}

/// Per-runtime sweeper state map. Keyed by collection name.
#[derive(Debug, Default)]
pub(crate) struct RetentionSweeperState {
    states: HashMap<String, SweeperState>,
}

impl RetentionSweeperState {
    pub(crate) fn new() -> Self {
        Self {
            states: HashMap::new(),
        }
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
        self.states
            .get(collection)
            .cloned()
            .unwrap_or_default()
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

    /// Drop bookkeeping for a collection (DROP TABLE / DROP COLLECTION).
    pub(crate) fn forget(&mut self, collection: &str) {
        self.states.remove(collection);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_tick_accumulates_rows_swept_total() {
        let mut state = RetentionSweeperState::new();
        state.record_tick("events", 100, 50, 1_000);
        state.record_tick("events", 50, 0, 2_000);
        let s = state.get("events");
        assert_eq!(s.rows_swept_total, 150);
        assert_eq!(s.last_pending_estimate, 0);
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
