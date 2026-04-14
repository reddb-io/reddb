//! Snapshot reuse coordinator — Phase 5 / PLAN.md backlog 3.4.
//!
//! Caches the current MVCC snapshot per session and lets
//! successive read-only queries reuse it without round-tripping
//! through the transaction manager. Invalidates on any write
//! visible to the session.
//!
//! Mirrors PG's `xact_completion` counter pattern:
//!
//! - Every commit / abort bumps a global atomic counter.
//! - Each session caches `(snapshot, last_seen_counter)`.
//! - On the next read-only query, compare `last_seen_counter`
//!   to the global counter. If unchanged, reuse the cached
//!   snapshot. If incremented, invalidate and refetch.
//!
//! ## Why this matters
//!
//! reddb's snapshot fetch goes through the dormant
//! `MvccCoordinator` which serialises behind the global
//! transaction lock. For OLTP-style workloads with many
//! independent read-only queries, the lock is the bottleneck.
//! Snapshot reuse short-circuits the lock when nothing has
//! changed since the last fetch.
//!
//! ## Wiring
//!
//! Not yet called by `runtime/impl_core.rs::execute_query`.
//! Phase 5 wiring adds:
//! 1. A `SnapshotCache` field on `RuntimeSession` (or
//!    equivalent per-session struct).
//! 2. The dispatch loop checks `cache.try_reuse(global_counter)`
//!    before the snapshot fetch and falls back to the slow
//!    path when the counter has advanced.
//! 3. Every commit/abort path bumps `global_counter` via
//!    `bump_completion_counter()`.

use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide xact_completion counter. Bumped on every
/// commit or abort. Sessions compare against their last-seen
/// snapshot to decide whether to reuse.
pub static GLOBAL_COMPLETION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Increment the global counter. Called from the transaction
/// manager's commit / abort paths.
///
/// Returns the new counter value so callers can stash it for
/// observability / debugging.
pub fn bump_completion_counter() -> u64 {
    GLOBAL_COMPLETION_COUNTER.fetch_add(1, Ordering::Release) + 1
}

/// Snapshot identifier — opaque from this module's perspective.
/// reddb's actual snapshot type lives in
/// `storage::transaction::coordinator::Snapshot`; we keep this
/// generic so the cache doesn't bring the whole MVCC graph
/// into its dependency footprint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct SnapshotId(pub u64);

/// Per-session snapshot cache. Owned by the session struct;
/// not thread-shared (one session = one user, one cache).
#[derive(Debug, Default)]
pub struct SnapshotCache {
    cached: Option<SnapshotId>,
    last_seen_counter: u64,
}

impl SnapshotCache {
    /// New empty cache. Initialises to "no cached snapshot,
    /// last seen counter is 0".
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to reuse the cached snapshot. Returns `Some(id)`
    /// when (a) we have a cached snapshot AND (b) the global
    /// counter hasn't advanced since we cached it. Returns
    /// `None` otherwise — the caller must fetch a fresh one.
    pub fn try_reuse(&self) -> Option<SnapshotId> {
        let global = GLOBAL_COMPLETION_COUNTER.load(Ordering::Acquire);
        if global == self.last_seen_counter {
            self.cached
        } else {
            None
        }
    }

    /// Stash a freshly-fetched snapshot. Records the current
    /// global counter so a later `try_reuse` can validate it
    /// hasn't been invalidated by an intervening commit.
    pub fn cache(&mut self, snapshot: SnapshotId) {
        self.cached = Some(snapshot);
        self.last_seen_counter = GLOBAL_COMPLETION_COUNTER.load(Ordering::Acquire);
    }

    /// Force-invalidate the cache. Used when the session knows
    /// it just performed a write — even if `try_reuse` would
    /// otherwise return the cached snapshot, the write makes
    /// it stale for self-visibility.
    pub fn invalidate(&mut self) {
        self.cached = None;
        self.last_seen_counter = 0;
    }

    /// Diagnostic: how many bumps have we missed? Difference
    /// between the global counter and our last-seen value.
    /// Used by EXPLAIN / metrics to track reuse effectiveness.
    pub fn staleness(&self) -> u64 {
        let global = GLOBAL_COMPLETION_COUNTER.load(Ordering::Acquire);
        global.saturating_sub(self.last_seen_counter)
    }
}
