//! Per-node load telemetry — issue #1245 (PRD #1237, Phase C).
//!
//! Records three occupancy signals that together let red-ui show which
//! node is hot (ADR 0060 §2 "node samples" data class):
//!
//! * `active_queries` — gauge: queries currently executing on this node.
//!   Incremented at every public `execute_query*` entry and decremented at
//!   the common `finish_query_lifecycle` exit so every path is counted
//!   exactly once.
//! * `connects_total` — monotonic counter: lifetime connection acquisitions
//!   from the pool. Paired with `disconnects_total` to derive churn rate
//!   without storing any per-client address.
//! * `disconnects_total` — monotonic counter: lifetime connection releases
//!   (pool slot returned).  `connects - disconnects` equals the current
//!   active-connection count observable through `PoolState`.
//!
//! ## Cardinality (ADR 0060 §4)
//!
//! No per-client, per-query, or per-collection labels are admitted.  The
//! `node_id` Prometheus label equals the hostname at the time of the first
//! scrape — a single fixed value per process.  Total series count:
//! `3 metrics × 1 node` = 3, fixed at compile time.
//!
//! ## Export surfaces (ADR 0060 §7)
//!
//! * `/metrics` — `reddb_node_active_queries{node_id}` (gauge),
//!   `reddb_node_connects_total{node_id}` and
//!   `reddb_node_disconnects_total{node_id}` (counters).
//! * `/cluster/status` — `"load"` object with `active_queries`,
//!   `connects_total`, `disconnects_total`.  Present as an `available:
//!   true` envelope as soon as any connection has been seen; honest
//!   `unavailable` envelope until then (honesty rule ADR 0060 §6).
//! * red-ui trend windows — same snapshot read model.
//!
//! ## Hot-path overhead
//!
//! Each observe call is one relaxed atomic add (gauge: `fetch_add`/
//! `fetch_sub`, counter: `fetch_add`).  No allocation, no lock, no
//! syscall.  The `active_queries` decrement runs inside
//! `finish_query_lifecycle` which already holds no lock; the connects /
//! disconnects increments run at pool acquire / release which hold the
//! pool mutex only until the pool state is updated — the atomic adds
//! happen *after* `drop(pool)` where applicable.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// Point-in-time snapshot of the three load signals.
#[derive(Debug, Clone, Default)]
pub struct NodeLoadSnapshot {
    /// Current count of queries executing on this node. May transiently be
    /// negative if a scrape races a decrement; callers should clamp to 0.
    pub active_queries: i64,
    /// Lifetime pool acquisitions (monotonic).
    pub connects_total: u64,
    /// Lifetime pool releases (monotonic).
    pub disconnects_total: u64,
}

impl NodeLoadSnapshot {
    /// `true` once at least one connection event has been recorded, which
    /// means the counters carry real signal.  Before any activity both
    /// counters are 0 and `/cluster/status` renders an `unavailable`
    /// envelope (ADR 0060 §6 honesty rule).
    pub fn has_activity(&self) -> bool {
        self.connects_total > 0
    }
}

/// Process-local node-load recorder.  A single shared instance lives
/// inside `RuntimeInner`; the three atomics are updated concurrently
/// from any thread.
#[derive(Debug, Default)]
pub struct NodeLoadTelemetry {
    active_queries: AtomicI64,
    connects_total: AtomicU64,
    disconnects_total: AtomicU64,
}

impl NodeLoadTelemetry {
    /// Increment the in-flight query gauge.  Call at the start of every
    /// `execute_query*` entry before delegating to the inner path.
    pub fn query_start(&self) {
        self.active_queries.fetch_add(1, Ordering::Relaxed);
    }

    /// Decrement the in-flight query gauge.  Call once at
    /// `finish_query_lifecycle` exit, after all query work is done.
    pub fn query_finish(&self) {
        self.active_queries.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record a pool acquisition (connection checkout).  No client address
    /// or identity is admitted — ADR 0060 §4.
    pub fn record_connect(&self) {
        self.connects_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a pool release (connection drop).
    pub fn record_disconnect(&self) {
        self.disconnects_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Lock-free point-in-time snapshot.  Callers render an `unavailable`
    /// envelope when `snapshot.has_activity() == false` (§6).
    pub fn snapshot(&self) -> NodeLoadSnapshot {
        NodeLoadSnapshot {
            active_queries: self.active_queries.load(Ordering::Relaxed),
            connects_total: self.connects_total.load(Ordering::Relaxed),
            disconnects_total: self.disconnects_total.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_queries_rises_and_falls_with_inflight_work() {
        let t = NodeLoadTelemetry::default();
        assert_eq!(t.snapshot().active_queries, 0);

        t.query_start();
        t.query_start();
        assert_eq!(t.snapshot().active_queries, 2, "two in-flight queries");

        t.query_finish();
        assert_eq!(t.snapshot().active_queries, 1, "one finished");

        t.query_finish();
        assert_eq!(t.snapshot().active_queries, 0, "both finished");
    }

    #[test]
    fn connect_disconnect_churn_increments_without_per_client_labels() {
        let t = NodeLoadTelemetry::default();
        assert!(!t.snapshot().has_activity());

        t.record_connect();
        t.record_connect();
        t.record_disconnect();

        let snap = t.snapshot();
        assert_eq!(snap.connects_total, 2);
        assert_eq!(snap.disconnects_total, 1);
        assert!(snap.has_activity());
    }

    #[test]
    fn has_activity_false_until_first_connection() {
        let t = NodeLoadTelemetry::default();
        assert!(!t.snapshot().has_activity());
        t.query_start();
        t.query_finish();
        // Query activity alone does not trigger has_activity (no connection yet).
        assert!(!t.snapshot().has_activity());
        t.record_connect();
        assert!(t.snapshot().has_activity());
    }

    #[test]
    fn snapshot_is_consistent_across_fields() {
        let t = NodeLoadTelemetry::default();
        for _ in 0..5 {
            t.record_connect();
        }
        for _ in 0..3 {
            t.record_disconnect();
        }
        for _ in 0..4 {
            t.query_start();
        }
        for _ in 0..2 {
            t.query_finish();
        }

        let snap = t.snapshot();
        assert_eq!(snap.connects_total, 5);
        assert_eq!(snap.disconnects_total, 3);
        assert_eq!(snap.active_queries, 2);
    }
}
