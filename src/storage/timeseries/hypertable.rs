//! Hypertables — time-range-partitioned tables à la TimescaleDB.
//!
//! A hypertable is a logical collection that auto-partitions writes
//! into child chunks, each covering a fixed time interval
//! (`chunk_interval_ns`). Queries that filter by the time column
//! see the partition pruner eliminate child chunks whose bounds
//! fall outside the predicate; drops happen per-chunk so operators
//! can retain the last N days without a full-table scan.
//!
//! This module defines the **metadata + router**. The physical
//! chunk is the same [`super::chunk::TimeSeriesChunk`] the standalone
//! time-series path already uses — the hypertable layer just tracks
//! which chunk a row goes into.
//!
//! SQL surface (parsed elsewhere in the sprint):
//!
//! ```sql
//! CREATE HYPERTABLE metrics (
//!   ts    BIGINT,
//!   host  TEXT,
//!   value DOUBLE
//! ) CHUNK INTERVAL '1 day';
//!
//! SELECT drop_chunks('metrics', INTERVAL '90 days');
//! SELECT show_chunks('metrics');
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::retention::parse_duration_ns;

/// Spec declared by `CREATE HYPERTABLE`.
#[derive(Debug, Clone)]
pub struct HypertableSpec {
    pub name: String,
    /// Column name that carries the time axis (must be unix-ns BIGINT
    /// or parseable to one).
    pub time_column: String,
    /// Fixed width of a single chunk, in nanoseconds.
    pub chunk_interval_ns: u64,
    /// Default TTL applied to every new chunk when the DDL didn't
    /// request an explicit override. `None` means "no TTL — chunks
    /// live until explicit `drop_chunks` / retention policy fires".
    ///
    /// The effective expiry of a chunk is `max_ts_ns + ttl_ns`, so
    /// a chunk is safely droppable once `now_ns ≥ expiry`. That
    /// matches the contract callers already learnt from the
    /// retention daemon — partition TTL is the **declarative** way
    /// to say the same thing at CREATE time without a separate
    /// `add_retention_policy` call.
    pub default_ttl_ns: Option<u64>,
}

impl HypertableSpec {
    pub fn new(
        name: impl Into<String>,
        time_column: impl Into<String>,
        chunk_interval_ns: u64,
    ) -> Self {
        Self {
            name: name.into(),
            time_column: time_column.into(),
            chunk_interval_ns: chunk_interval_ns.max(1),
            default_ttl_ns: None,
        }
    }

    /// Convenience: construct from a Timescale-style duration string
    /// (`"1d"`, `"1h"`, `"30m"`…).
    pub fn from_interval_string(
        name: impl Into<String>,
        time_column: impl Into<String>,
        interval: &str,
    ) -> Option<Self> {
        let ns = parse_duration_ns(interval)?;
        if ns == 0 {
            return None;
        }
        Some(Self::new(name, time_column, ns))
    }

    /// Builder-style: attach a default TTL. Uses the same duration
    /// grammar as `chunk_interval` (`"90d"`, `"30s"`, …).
    pub fn with_ttl(mut self, ttl: &str) -> Option<Self> {
        let ns = parse_duration_ns(ttl)?;
        if ns == 0 {
            return None;
        }
        self.default_ttl_ns = Some(ns);
        Some(self)
    }

    /// Direct setter when the TTL is already computed in ns.
    pub fn with_ttl_ns(mut self, ttl_ns: u64) -> Self {
        self.default_ttl_ns = if ttl_ns == 0 { None } else { Some(ttl_ns) };
        self
    }

    /// Align `timestamp_ns` to the chunk's floor — the chunk that
    /// row belongs to starts at this timestamp and covers
    /// `[start, start + chunk_interval_ns)`.
    pub fn chunk_start(&self, timestamp_ns: u64) -> u64 {
        (timestamp_ns / self.chunk_interval_ns) * self.chunk_interval_ns
    }

    pub fn chunk_end_exclusive(&self, timestamp_ns: u64) -> u64 {
        self.chunk_start(timestamp_ns)
            .saturating_add(self.chunk_interval_ns)
    }
}

/// Identifier of a single child chunk. Stable across restart so
/// catalog + retention can reference it unambiguously.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChunkId {
    pub hypertable: String,
    /// Chunk start (inclusive), aligned to `chunk_interval_ns`.
    pub start_ns: u64,
}

/// Metadata tracked per child chunk. Physical storage lives in
/// `TimeSeriesChunk` keyed by `(hypertable, start_ns)`.
#[derive(Debug, Clone)]
pub struct ChunkMeta {
    pub id: ChunkId,
    pub end_ns_exclusive: u64,
    pub row_count: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
    pub sealed: bool,
    /// Optional per-chunk TTL override. `None` means "fall back to
    /// the hypertable's default TTL". Setting this lets mixed-TTL
    /// policies live inside the same hypertable — e.g. keep the
    /// current month of data forever but expire everything older
    /// than 90 days.
    pub ttl_override_ns: Option<u64>,
}

impl ChunkMeta {
    pub fn new(id: ChunkId, end_ns_exclusive: u64) -> Self {
        Self {
            id,
            end_ns_exclusive,
            row_count: 0,
            min_ts_ns: u64::MAX,
            max_ts_ns: 0,
            sealed: false,
            ttl_override_ns: None,
        }
    }

    pub fn observe(&mut self, ts_ns: u64) {
        self.row_count += 1;
        if ts_ns < self.min_ts_ns {
            self.min_ts_ns = ts_ns;
        }
        if ts_ns > self.max_ts_ns {
            self.max_ts_ns = ts_ns;
        }
    }

    /// Effective TTL = per-chunk override if present, otherwise the
    /// hypertable default. `None` = the chunk has no automatic
    /// expiry.
    pub fn effective_ttl_ns(&self, default_ttl_ns: Option<u64>) -> Option<u64> {
        self.ttl_override_ns.or(default_ttl_ns)
    }

    /// Absolute epoch-ns at which the chunk becomes droppable. Uses
    /// `max_ts_ns` as the baseline — the newest row the chunk has
    /// ever accepted — so an empty chunk (no rows yet) never
    /// expires until at least one row lands.
    pub fn expiry_ns(&self, default_ttl_ns: Option<u64>) -> Option<u64> {
        let ttl = self.effective_ttl_ns(default_ttl_ns)?;
        if self.row_count == 0 {
            return None;
        }
        Some(self.max_ts_ns.saturating_add(ttl))
    }

    pub fn is_expired_at(&self, now_ns: u64, default_ttl_ns: Option<u64>) -> bool {
        match self.expiry_ns(default_ttl_ns) {
            Some(expiry) => now_ns >= expiry,
            None => false,
        }
    }
}

/// In-memory catalog of hypertables and their chunks. Thread-safe
/// because INSERTs can land from multiple writers simultaneously.
#[derive(Clone, Default)]
pub struct HypertableRegistry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Default)]
struct RegistryInner {
    specs: BTreeMap<String, HypertableSpec>,
    /// `(hypertable, start_ns)` → chunk meta. `BTreeMap` so lookups
    /// by name produce an ordered view (show_chunks must be
    /// deterministic).
    chunks: BTreeMap<(String, u64), ChunkMeta>,
}

impl std::fmt::Debug for HypertableRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        f.debug_struct("HypertableRegistry")
            .field("hypertables", &guard.specs.len())
            .field("chunks", &guard.chunks.len())
            .finish()
    }
}

impl HypertableRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new hypertable. Replaces the previous spec if one
    /// existed with the same name; chunks for that name are kept —
    /// the operator is assumed to know what they're doing when they
    /// redefine (e.g. widening `chunk_interval_ns`).
    pub fn register(&self, spec: HypertableSpec) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.insert(spec.name.clone(), spec);
    }

    pub fn get(&self, name: &str) -> Option<HypertableSpec> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.get(name).cloned()
    }

    pub fn list(&self) -> Vec<HypertableSpec> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.values().cloned().collect()
    }

    /// Route a write: returns the `ChunkId` the row belongs in,
    /// allocating the chunk on first write. `None` when the
    /// hypertable is unknown.
    pub fn route(&self, hypertable: &str, timestamp_ns: u64) -> Option<ChunkId> {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let spec = guard.specs.get(hypertable)?.clone();
        let start = spec.chunk_start(timestamp_ns);
        let end = spec.chunk_end_exclusive(timestamp_ns);
        let id = ChunkId {
            hypertable: spec.name.clone(),
            start_ns: start,
        };
        let key = (spec.name.clone(), start);
        let meta = guard
            .chunks
            .entry(key)
            .or_insert_with(|| ChunkMeta::new(id.clone(), end));
        meta.observe(timestamp_ns);
        Some(id)
    }

    /// Return every chunk for `hypertable`, oldest-first.
    pub fn show_chunks(&self, hypertable: &str) -> Vec<ChunkMeta> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard
            .chunks
            .iter()
            .filter(|((name, _), _)| name == hypertable)
            .map(|(_, meta)| meta.clone())
            .collect()
    }

    /// Drop every chunk of `hypertable` whose `max_ts_ns` is at or
    /// below `cutoff_ns`. Returns the count dropped — the physical
    /// storage release is the caller's responsibility (this module
    /// only owns the metadata).
    pub fn drop_chunks_before(&self, hypertable: &str, cutoff_ns: u64) -> Vec<ChunkMeta> {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let mut dropped = Vec::new();
        let keys: Vec<(String, u64)> = guard
            .chunks
            .iter()
            .filter(|((name, _), meta)| name == hypertable && meta.max_ts_ns <= cutoff_ns)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some(meta) = guard.chunks.remove(&key) {
                dropped.push(meta);
            }
        }
        dropped
    }

    /// Sweep chunks whose effective TTL has fired. A chunk is
    /// droppable when `now_ns ≥ max_ts_ns + effective_ttl_ns` — the
    /// registry hands back every removed `ChunkMeta` so the
    /// physical-storage callback can release bytes + indexes. Chunks
    /// without an effective TTL (neither per-chunk override nor
    /// hypertable default) are never touched.
    ///
    /// This is the "TTL applied at the partition level" primitive:
    /// one O(1) metadata sweep reclaims every row of every expired
    /// chunk, instead of scanning rows individually like an
    /// entity-level TTL would. Empty hypertables stay empty.
    pub fn sweep_expired(&self, hypertable: &str, now_ns: u64) -> Vec<ChunkMeta> {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let Some(spec) = guard.specs.get(hypertable).cloned() else {
            return Vec::new();
        };
        let expired_keys: Vec<(String, u64)> = guard
            .chunks
            .iter()
            .filter(|((name, _), meta)| {
                name == hypertable && meta.is_expired_at(now_ns, spec.default_ttl_ns)
            })
            .map(|(k, _)| k.clone())
            .collect();
        let mut dropped = Vec::with_capacity(expired_keys.len());
        for key in expired_keys {
            if let Some(meta) = guard.chunks.remove(&key) {
                dropped.push(meta);
            }
        }
        dropped
    }

    /// Sweep every registered hypertable in one shot — the loop the
    /// retention daemon runs every cycle. Returns a flat list of
    /// `(hypertable_name, chunk_dropped)` pairs.
    pub fn sweep_all_expired(&self, now_ns: u64) -> Vec<(String, ChunkMeta)> {
        let names: Vec<String> = {
            let guard = match self.inner.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            guard.specs.keys().cloned().collect()
        };
        let mut out = Vec::new();
        for name in names {
            for meta in self.sweep_expired(&name, now_ns) {
                out.push((name.clone(), meta));
            }
        }
        out
    }

    /// Install / replace the hypertable-wide default TTL. `None`
    /// disables automatic expiry — chunks live until explicit
    /// `drop_chunks` / per-chunk override fires.
    pub fn set_default_ttl_ns(&self, hypertable: &str, ttl_ns: Option<u64>) -> bool {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        match guard.specs.get_mut(hypertable) {
            Some(spec) => {
                spec.default_ttl_ns = match ttl_ns {
                    Some(0) | None => None,
                    Some(v) => Some(v),
                };
                true
            }
            None => false,
        }
    }

    /// Override the TTL for a single chunk. Useful for "keep this
    /// specific chunk longer because it contains an incident
    /// replay" or "expire this one faster because it was filled
    /// from a backfill we're about to redo". `None` removes the
    /// override and falls back to the hypertable default.
    pub fn set_chunk_ttl_ns(&self, id: &ChunkId, ttl_ns: Option<u64>) -> bool {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(meta) = guard.chunks.get_mut(&(id.hypertable.clone(), id.start_ns)) {
            meta.ttl_override_ns = ttl_ns;
            true
        } else {
            false
        }
    }

    /// Inspect the list of chunks that are *about* to expire within
    /// `horizon_ns`. Powers preview endpoints ("what will the next
    /// sweep drop?") without actually dropping anything.
    pub fn chunks_expiring_within(
        &self,
        hypertable: &str,
        now_ns: u64,
        horizon_ns: u64,
    ) -> Vec<ChunkMeta> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let Some(spec) = guard.specs.get(hypertable).cloned() else {
            return Vec::new();
        };
        let cutoff = now_ns.saturating_add(horizon_ns);
        guard
            .chunks
            .iter()
            .filter(|((name, _), _)| name == hypertable)
            .filter_map(|(_, meta)| {
                let expiry = meta.expiry_ns(spec.default_ttl_ns)?;
                if expiry <= cutoff {
                    Some(meta.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Seal a chunk — future writes to the same `start_ns` bucket
    /// will still land (open-ended), but the `sealed` flag signals
    /// the maintenance layer that the chunk can now be compressed /
    /// uploaded / migrated. Returns `true` if the chunk existed.
    pub fn seal_chunk(&self, id: &ChunkId) -> bool {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(meta) = guard.chunks.get_mut(&(id.hypertable.clone(), id.start_ns)) {
            meta.sealed = true;
            true
        } else {
            false
        }
    }

    /// Total row count across every chunk of `hypertable`. Used by
    /// catalog views + benchmark harnesses.
    pub fn total_rows(&self, hypertable: &str) -> u64 {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard
            .chunks
            .iter()
            .filter(|((name, _), _)| name == hypertable)
            .map(|(_, meta)| meta.row_count)
            .sum()
    }

    /// List the hypertables the retention daemon should sweep.
    pub fn names(&self) -> Vec<String> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.keys().cloned().collect()
    }

    /// Drop the whole hypertable (spec + every chunk). Returns the
    /// number of chunks removed.
    pub fn drop_hypertable(&self, name: &str) -> usize {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.remove(name);
        let keys: Vec<(String, u64)> = guard
            .chunks
            .keys()
            .filter(|(n, _)| n == name)
            .cloned()
            .collect();
        for key in &keys {
            guard.chunks.remove(key);
        }
        keys.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_NS: u64 = 86_400_000_000_000;
    const HOUR_NS: u64 = 3_600_000_000_000;

    #[test]
    fn chunk_start_aligns_to_interval_floor() {
        let spec = HypertableSpec::new("m", "ts", DAY_NS);
        assert_eq!(spec.chunk_start(0), 0);
        assert_eq!(spec.chunk_start(DAY_NS - 1), 0);
        assert_eq!(spec.chunk_start(DAY_NS), DAY_NS);
        assert_eq!(spec.chunk_start(3 * DAY_NS + 123), 3 * DAY_NS);
    }

    #[test]
    fn interval_string_accepts_duration_units() {
        let s = HypertableSpec::from_interval_string("m", "ts", "1d").unwrap();
        assert_eq!(s.chunk_interval_ns, DAY_NS);
        let s = HypertableSpec::from_interval_string("m", "ts", "1h").unwrap();
        assert_eq!(s.chunk_interval_ns, HOUR_NS);
        assert!(HypertableSpec::from_interval_string("m", "ts", "raw").is_none());
        assert!(HypertableSpec::from_interval_string("m", "ts", "garbage").is_none());
    }

    #[test]
    fn route_allocates_chunk_on_first_write() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("metrics", "ts", DAY_NS));
        let id = reg.route("metrics", DAY_NS + 100).unwrap();
        assert_eq!(id.hypertable, "metrics");
        assert_eq!(id.start_ns, DAY_NS);
        let chunks = reg.show_chunks("metrics");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].row_count, 1);
        assert_eq!(chunks[0].min_ts_ns, DAY_NS + 100);
        assert_eq!(chunks[0].max_ts_ns, DAY_NS + 100);
        assert_eq!(chunks[0].end_ns_exclusive, 2 * DAY_NS);
    }

    #[test]
    fn route_groups_writes_within_same_chunk() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        for offset in [10u64, 100, 1_000, DAY_NS - 1] {
            let id = reg.route("m", offset).unwrap();
            assert_eq!(id.start_ns, 0);
        }
        let chunks = reg.show_chunks("m");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].row_count, 4);
    }

    #[test]
    fn route_splits_writes_across_adjacent_chunks() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        reg.route("m", DAY_NS - 1).unwrap();
        reg.route("m", DAY_NS).unwrap();
        reg.route("m", 2 * DAY_NS).unwrap();
        let chunks = reg.show_chunks("m");
        assert_eq!(chunks.len(), 3);
        assert!(chunks[0].id.start_ns <= chunks[1].id.start_ns);
        assert!(chunks[1].id.start_ns <= chunks[2].id.start_ns);
    }

    #[test]
    fn route_returns_none_for_unknown_hypertable() {
        let reg = HypertableRegistry::new();
        assert!(reg.route("nope", 0).is_none());
    }

    #[test]
    fn drop_chunks_before_removes_matching_chunks() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        reg.route("m", 0).unwrap(); // chunk start 0, max=0
        reg.route("m", DAY_NS).unwrap(); // chunk start DAY_NS, max=DAY_NS
        reg.route("m", 2 * DAY_NS + 5).unwrap(); // chunk start 2*DAY_NS

        let dropped = reg.drop_chunks_before("m", DAY_NS);
        // max_ts_ns of first chunk is 0, of second chunk is DAY_NS.
        // cutoff = DAY_NS, so both are "<= cutoff" → both dropped.
        assert_eq!(dropped.len(), 2);
        let remaining = reg.show_chunks("m");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id.start_ns, 2 * DAY_NS);
    }

    #[test]
    fn show_chunks_is_ordered_by_start() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        for ts in [5 * DAY_NS, 2 * DAY_NS, 7 * DAY_NS, 1 * DAY_NS] {
            reg.route("m", ts).unwrap();
        }
        let starts: Vec<u64> = reg.show_chunks("m").iter().map(|c| c.id.start_ns).collect();
        assert_eq!(starts, vec![DAY_NS, 2 * DAY_NS, 5 * DAY_NS, 7 * DAY_NS]);
    }

    #[test]
    fn seal_chunk_flips_flag() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        let id = reg.route("m", 0).unwrap();
        assert!(reg.seal_chunk(&id));
        assert!(reg.show_chunks("m")[0].sealed);
    }

    #[test]
    fn drop_hypertable_removes_everything() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        reg.route("m", 0).unwrap();
        reg.route("m", DAY_NS).unwrap();
        assert_eq!(reg.drop_hypertable("m"), 2);
        assert!(reg.get("m").is_none());
        assert!(reg.show_chunks("m").is_empty());
    }

    #[test]
    fn total_rows_sums_every_chunk() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        for ts in 0..1000 {
            reg.route("m", ts).unwrap();
        }
        for ts in DAY_NS..DAY_NS + 500 {
            reg.route("m", ts).unwrap();
        }
        assert_eq!(reg.total_rows("m"), 1500);
    }

    #[test]
    fn names_lists_registered_hypertables() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("a", "ts", DAY_NS));
        reg.register(HypertableSpec::new("b", "ts", HOUR_NS));
        let mut names = reg.names();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
    }

    // -----------------------------------------------------------------
    // Partition-level TTL
    // -----------------------------------------------------------------

    #[test]
    fn with_ttl_parses_duration_and_sets_default() {
        let s = HypertableSpec::new("m", "ts", DAY_NS)
            .with_ttl("7d")
            .unwrap();
        assert_eq!(s.default_ttl_ns, Some(7 * DAY_NS));
        assert!(HypertableSpec::new("m", "ts", DAY_NS)
            .with_ttl("raw")
            .is_none());
        assert!(HypertableSpec::new("m", "ts", DAY_NS)
            .with_ttl("garbage")
            .is_none());
    }

    #[test]
    fn chunk_with_no_rows_never_expires() {
        let meta = ChunkMeta::new(
            ChunkId {
                hypertable: "m".into(),
                start_ns: 0,
            },
            DAY_NS,
        );
        assert!(!meta.is_expired_at(u64::MAX, Some(1)));
    }

    #[test]
    fn chunk_expires_when_now_crosses_max_ts_plus_ttl() {
        let mut meta = ChunkMeta::new(
            ChunkId {
                hypertable: "m".into(),
                start_ns: 0,
            },
            DAY_NS,
        );
        meta.observe(500);
        // TTL = 1000, max_ts = 500 → expires at 1500.
        assert!(!meta.is_expired_at(1000, Some(1000)));
        assert!(!meta.is_expired_at(1499, Some(1000)));
        assert!(meta.is_expired_at(1500, Some(1000)));
    }

    #[test]
    fn per_chunk_override_wins_over_hypertable_default() {
        let mut meta = ChunkMeta::new(
            ChunkId {
                hypertable: "m".into(),
                start_ns: 0,
            },
            DAY_NS,
        );
        meta.observe(500);
        // Default would say "expire at 500+1000 = 1500"; override
        // narrows to 500+100 = 600.
        meta.ttl_override_ns = Some(100);
        assert!(meta.is_expired_at(600, Some(1000)));
        assert!(!meta.is_expired_at(599, Some(1000)));
    }

    #[test]
    fn sweep_expired_drops_chunks_past_ttl_and_returns_them() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS).with_ttl_ns(2 * DAY_NS));
        // 3 chunks — at 0, DAY, 2*DAY.
        for t in [0, DAY_NS, 2 * DAY_NS] {
            reg.route("m", t).unwrap();
        }
        // now = 3 * DAY + 1 → chunks with max_ts in {0, DAY_NS} both
        // expired (max + 2d ≤ now), chunk at 2*DAY_NS still alive.
        let dropped = reg.sweep_expired("m", 3 * DAY_NS + 1);
        let mut starts: Vec<u64> = dropped.iter().map(|m| m.id.start_ns).collect();
        starts.sort();
        assert_eq!(starts, vec![0, DAY_NS]);
        let remaining = reg.show_chunks("m");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id.start_ns, 2 * DAY_NS);
    }

    #[test]
    fn sweep_without_ttl_keeps_every_chunk() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS)); // no TTL
        for t in [0, DAY_NS, 2 * DAY_NS] {
            reg.route("m", t).unwrap();
        }
        let dropped = reg.sweep_expired("m", 10_000 * DAY_NS);
        assert!(dropped.is_empty());
        assert_eq!(reg.show_chunks("m").len(), 3);
    }

    #[test]
    fn sweep_all_expired_iterates_every_hypertable() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("fast", "ts", HOUR_NS).with_ttl_ns(HOUR_NS));
        reg.register(HypertableSpec::new("slow", "ts", DAY_NS).with_ttl_ns(7 * DAY_NS));
        // fast: chunk at 0 expires fast; slow: chunk at 0 still
        // within its 7-day TTL.
        reg.route("fast", 0).unwrap();
        reg.route("slow", 0).unwrap();
        let dropped = reg.sweep_all_expired(2 * HOUR_NS);
        assert_eq!(dropped.len(), 1);
        assert_eq!(dropped[0].0, "fast");
        assert_eq!(reg.show_chunks("slow").len(), 1);
    }

    #[test]
    fn set_chunk_ttl_ns_lets_caller_pin_or_shorten() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS).with_ttl_ns(DAY_NS));
        let id = reg.route("m", 0).unwrap();
        // Raise TTL to 100 days — chunk should survive the sweep.
        assert!(reg.set_chunk_ttl_ns(&id, Some(100 * DAY_NS)));
        let dropped = reg.sweep_expired("m", 10 * DAY_NS);
        assert!(dropped.is_empty());
        // Now shorten to 1 hour and sweep.
        reg.set_chunk_ttl_ns(&id, Some(HOUR_NS));
        let dropped = reg.sweep_expired("m", 10 * HOUR_NS);
        assert_eq!(dropped.len(), 1);
    }

    #[test]
    fn chunks_expiring_within_previews_without_dropping() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS).with_ttl_ns(DAY_NS));
        // Chunks with max_ts at 0, 1d, 2d → expiries at 1d, 2d, 3d.
        for t in [0, DAY_NS, 2 * DAY_NS] {
            reg.route("m", t).unwrap();
        }
        // now=0, horizon=1.5d → only the first chunk (expiry 1d)
        // fits. Tight horizon proves the cutoff math.
        let preview = reg.chunks_expiring_within("m", 0, DAY_NS + DAY_NS / 2);
        assert_eq!(preview.len(), 1);
        assert_eq!(preview[0].id.start_ns, 0);
        // Wider horizon pulls in the second chunk too.
        let preview2 = reg.chunks_expiring_within("m", 0, 2 * DAY_NS);
        assert_eq!(preview2.len(), 2);
        // Registry still has every chunk — preview never drops.
        assert_eq!(reg.show_chunks("m").len(), 3);
    }
}
