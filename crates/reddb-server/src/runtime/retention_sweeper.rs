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
    static TTL_EXPIRY_NOW_MS: Cell<Option<u64>> = const { Cell::new(None) };
    static TIMESTAMP_CONTEXT: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    static DELETE_CANDIDATES: RefCell<Option<Vec<crate::storage::EntityId>>> = const { RefCell::new(None) };
}

pub(crate) fn with_ttl_delete_candidates<T>(
    candidates: Vec<crate::storage::EntityId>,
    collection: &str,
    now_ms: u64,
    f: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TTL_EXPIRY_NOW_MS.with(|slot| slot.set(self.0));
        }
    }
    let previous = TTL_EXPIRY_NOW_MS.with(|slot| slot.replace(Some(now_ms)));
    let _restore = Restore(previous);
    with_delete_candidates(candidates, collection, "rid", f)
}

pub(crate) fn ttl_expiry_now_ms() -> Option<u64> {
    TTL_EXPIRY_NOW_MS.with(Cell::get)
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
    /// Cumulative individual-TTL rows reclaimed since boot.
    pub ttl_rows_swept_total: u64,
    /// Conservative remaining due rows after the latest bounded TTL tick.
    pub ttl_pending_estimate: u64,
    /// Duration of the latest individual-TTL tick for this collection.
    pub ttl_last_sweep_duration_ms: u64,
}

/// Per-runtime sweeper state map. Keyed by collection name.
#[derive(Default)]
pub(crate) struct RetentionSweeperState {
    states: HashMap<String, SweeperState>,
    cursors: HashMap<String, crate::storage::unified::store::MvccReclaimCursor>,
    ttl_projection_cursors: HashMap<String, crate::storage::unified::store::MvccReclaimCursor>,
    next_collection: Option<String>,
    next_ttl_collection: Option<String>,
}

impl RetentionSweeperState {
    pub(crate) fn new() -> Self {
        Self {
            states: HashMap::new(),
            cursors: HashMap::new(),
            ttl_projection_cursors: HashMap::new(),
            next_collection: None,
            next_ttl_collection: None,
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

    pub(crate) fn rebuild_ttl_projection_tick(
        &mut self,
        store: &crate::storage::unified::store::UnifiedStore,
        collection: &str,
        budget: usize,
    ) -> Result<(usize, bool), crate::storage::StoreError> {
        if !self.ttl_projection_cursors.contains_key(collection) {
            let Some(cursor) = store.mvcc_reclaim_cursor(collection.to_owned()) else {
                return Ok((0, true));
            };
            self.ttl_projection_cursors
                .insert(collection.to_owned(), cursor);
        }
        let cursor = self
            .ttl_projection_cursors
            .get_mut(collection)
            .expect("TTL projection cursor was inserted");
        let (ids, scanned) = store.retention_scan_tick(cursor, budget)?;
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| crate::storage::StoreError::CollectionNotFound(collection.into()))?;
        for id in ids {
            manager.rebuild_metadata_expiry(id)?;
        }
        let complete = cursor.finished();
        if complete {
            self.ttl_projection_cursors.remove(collection);
            manager.mark_metadata_expiry_projection_ready();
        }
        Ok((scanned, complete))
    }

    pub(crate) fn resume_at(&mut self, collection: String) {
        self.next_collection = Some(collection);
    }

    pub(crate) fn next_ttl_collection(&self) -> Option<&str> {
        self.next_ttl_collection.as_deref()
    }

    pub(crate) fn resume_ttl_at(&mut self, collection: String) {
        self.next_ttl_collection = Some(collection);
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

    pub(crate) fn record_ttl_tick(
        &mut self,
        collection: &str,
        rows_swept: u64,
        pending_estimate: u64,
        duration_ms: u64,
        at_unix_ms: u64,
    ) {
        let entry = self.states.entry(collection.to_string()).or_default();
        entry.last_sweep_at_ms = at_unix_ms;
        entry.ttl_rows_swept_total = entry.ttl_rows_swept_total.saturating_add(rows_swept);
        entry.ttl_pending_estimate = pending_estimate;
        entry.ttl_last_sweep_duration_ms = duration_ms;
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
    use crate::{RedDBOptions, RedDBRuntime};
    use reddb_types::Value;

    fn row_count(runtime: &RedDBRuntime, table: &str) -> u64 {
        let result = runtime
            .execute_query(&format!("SELECT COUNT(*) AS n FROM {table}"))
            .expect("count TTL rows");
        match result.result.records[0].get("n").expect("count") {
            Value::UnsignedInteger(value) => *value,
            Value::Integer(value) => u64::try_from(*value).expect("non-negative count"),
            value => panic!("unexpected count {value:?}"),
        }
    }

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
    fn individual_ttl_sweep_is_bounded_without_policy_call() {
        let mut runtime = RedDBRuntime::in_memory().expect("runtime");
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE ttl_rows (id INTEGER)")
            .expect("table");
        for id in 0..5 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO ttl_rows (id) VALUES ({id}) WITH EXPIRES AT 1"
                ))
                .expect("expired row");
        }
        runtime
            .execute_query("INSERT INTO ttl_rows (id) VALUES (99) WITH EXPIRES AT 9999999999999")
            .expect("future row");
        let manager = runtime
            .db()
            .store()
            .get_collection("ttl_rows")
            .expect("TTL manager");
        assert_eq!(manager.scheduled_metadata_expiry_count(), 6);
        assert_eq!(manager.expired_metadata_ids(2, 10).len(), 5);

        assert_eq!(runtime.sweep_individual_ttl_tick_at(2, 2), 2);
        assert_eq!(row_count(&runtime, "ttl_rows"), 4);
        assert_eq!(runtime.sweep_individual_ttl_tick_at(2, 2), 2);
        assert_eq!(row_count(&runtime, "ttl_rows"), 2);
        assert_eq!(runtime.sweep_individual_ttl_tick_at(2, 2), 1);
        assert_eq!(row_count(&runtime, "ttl_rows"), 1);

        let metrics = runtime
            .execute_query(
                "SELECT ttl_rows_swept_total, ttl_pending_sweep_estimate, \
                 ttl_last_sweep_duration_ms FROM red.retention WHERE name = 'ttl_rows'",
            )
            .expect("TTL retention metrics");
        let row = &metrics.result.records[0];
        assert_eq!(
            row.get("ttl_rows_swept_total"),
            Some(&Value::UnsignedInteger(5))
        );
        assert_eq!(
            row.get("ttl_pending_sweep_estimate"),
            Some(&Value::UnsignedInteger(0))
        );
        assert!(matches!(
            row.get("ttl_last_sweep_duration_ms"),
            Some(Value::UnsignedInteger(_))
        ));
    }

    #[test]
    fn background_sweeper_expires_individual_ttl_without_policy_call() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query("CREATE TABLE ttl_background (id INTEGER)")
            .expect("table");
        runtime
            .execute_query("INSERT INTO ttl_background (id) VALUES (1) WITH EXPIRES AT 1")
            .expect("expired row");
        let manager = runtime
            .db()
            .store()
            .get_collection("ttl_background")
            .expect("TTL manager");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while manager.scheduled_metadata_expiry_count() != 0 && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        assert_eq!(manager.scheduled_metadata_expiry_count(), 0);
        assert_eq!(row_count(&runtime, "ttl_background"), 0);
    }

    #[test]
    fn bounded_ttl_sweeps_allow_concurrent_reads_and_writes() {
        let mut runtime = RedDBRuntime::in_memory().expect("runtime");
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE ttl_concurrent (id INTEGER)")
            .expect("table");
        for id in 0..16 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO ttl_concurrent (id) VALUES ({id}) WITH EXPIRES AT 1"
                ))
                .expect("expired row");
        }

        let reader_runtime = runtime.clone();
        let reader = std::thread::spawn(move || {
            for _ in 0..32 {
                let result = reader_runtime
                    .execute_query("SELECT 1")
                    .expect("scalar read");
                assert_eq!(result.result.records.len(), 1);
            }
        });
        let writer_runtime = runtime.clone();
        let writer = std::thread::spawn(move || {
            for id in 100..116 {
                writer_runtime
                    .execute_query(&format!(
                        "INSERT INTO ttl_concurrent (id) VALUES ({id}) \
                         WITH EXPIRES AT 9999999999999"
                    ))
                    .expect("concurrent future row");
            }
        });

        let mut swept = 0;
        while swept < 16 {
            let batch = runtime.sweep_individual_ttl_tick_at(4, 2);
            assert!(batch <= 4);
            swept += batch;
        }
        reader.join().expect("reader thread");
        writer.join().expect("writer thread");
        assert_eq!(swept, 16);
        assert_eq!(row_count(&runtime, "ttl_concurrent"), 16);
    }

    #[test]
    fn ttl_reclamation_returns_pages_for_reuse() {
        let directory = tempfile::tempdir().expect("TTL reuse fixture");
        let path = directory.path().join("ttl-reuse.rdb");
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .expect("runtime");
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE ttl_reuse (id INTEGER, payload TEXT)")
            .expect("table");
        let payload = "x".repeat(64 * 1024);
        for id in 0..8 {
            runtime
                .execute_query_with_params(
                    "INSERT INTO ttl_reuse (id, payload) VALUES ($1, $2) WITH EXPIRES AT 1",
                    &[Value::Integer(id), Value::text(payload.clone())],
                )
                .expect("TTL payload");
        }
        runtime.checkpoint().expect("initial checkpoint");
        let initial_page_count = runtime
            .db()
            .store()
            .pager()
            .expect("pager")
            .page_count()
            .expect("initial page count");
        let mut swept = 0;
        while swept < 8 {
            swept += runtime.sweep_individual_ttl_tick_at(2, 2);
        }
        runtime.checkpoint().expect("tombstone checkpoint");

        let store = runtime.db().store();
        let mut cursor = store
            .mvcc_reclaim_cursor("ttl_reuse".into())
            .expect("TTL GC cursor");
        let mut reclaimed = 0;
        for _ in 0..64 {
            reclaimed += store
                .reclaim_mvcc_tick(
                    &mut cursor,
                    &runtime.snapshot_manager(),
                    runtime.mvcc_vacuum_cutoff_xid(),
                    32,
                    8,
                )
                .expect("TTL MVCC reclaim");
            if cursor.finished() {
                break;
            }
        }
        assert!(cursor.finished());
        assert_eq!(reclaimed, 8);
        drop(cursor);
        for _ in 0..3 {
            runtime.checkpoint().expect("publish free pages");
        }
        let pager = store.pager().expect("pager");
        let total_before_reuse = pager.page_count().expect("page count");
        let free_before_reuse = pager.free_page_count().expect("free pages");

        for id in 100..108 {
            runtime
                .execute_query_with_params(
                    "INSERT INTO ttl_reuse (id, payload) VALUES ($1, $2) \
                     WITH EXPIRES AT 9999999999999",
                    &[Value::Integer(id), Value::text(payload.clone())],
                )
                .expect("replacement payload");
        }
        runtime.checkpoint().expect("reuse checkpoint");
        let total_after_reuse = pager.page_count().expect("page count after reuse");
        assert!(
            total_after_reuse <= initial_page_count.saturating_add(16),
            "replacement TTL payloads failed to reuse the initial allocation: initial={initial_page_count}, before reuse={total_before_reuse}, free before reuse={free_before_reuse}, after reuse={total_after_reuse}"
        );
    }

    #[test]
    fn persisted_ttl_deadlines_rebuild_without_materializing_the_collection() {
        let directory = tempfile::tempdir().expect("TTL fixture");
        let options = RedDBOptions::persistent(directory.path().join("ttl.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let deadline = 9_000_000_000_000u64;
        let mut runtime = RedDBRuntime::with_options(options.clone()).expect("seed runtime");
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE ttl_restart (id INTEGER)")
            .expect("table");
        for id in 0..3 {
            runtime
                .execute_query(&format!(
                    "INSERT INTO ttl_restart (id) VALUES ({id}) WITH EXPIRES AT {deadline}"
                ))
                .expect("future row");
        }
        assert_eq!(runtime.sweep_individual_ttl_tick_at(2, deadline), 2);
        assert_eq!(row_count(&runtime, "ttl_restart"), 1);
        assert_eq!(
            runtime
                .db()
                .store()
                .get_collection("ttl_restart")
                .expect("TTL collection before checkpoint")
                .scheduled_metadata_expiry_count(),
            1
        );
        runtime
            .flush()
            .expect("checkpoint during bounded sweep backlog");
        drop(runtime);

        let mut runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options)
        })
        .expect("reopen disk-resident runtime");
        drop(runtime.background_tasks.take());
        let manager = runtime
            .db()
            .store()
            .get_collection("ttl_restart")
            .expect("TTL collection");
        assert_eq!(manager.scheduled_metadata_expiry_count(), 1);
        assert_eq!(runtime.sweep_individual_ttl_tick_at(2, deadline), 1);
        assert_eq!(row_count(&runtime, "ttl_restart"), 0);
        drop(manager);
        drop(runtime);
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
