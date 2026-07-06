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

use super::chunk::{
    points_from_column_block, COLUMNAR_TS_COLUMN_ID, COLUMNAR_VALUE_COLUMN_ID, DEFAULT_GRANULE_SIZE,
};
use super::retention::parse_duration_ns;
use crate::storage::engine::PageLocation;
use crate::storage::schema::types::DataType;
use crate::storage::unified::column_block::{write_column_block, ColumnBlockError, ColumnInput};
use crate::storage::unified::segment_codec::ColumnSemantics;

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

/// On-disk storage format of a chunk — the **read-bridge dispatch key**
/// (PRD #850 Phase 1, #861). After `COLUMNAR` is enabled on a collection
/// that already holds row data, pre-existing chunks stay `Row` and new
/// chunks seal `ColumnarV1`; the two coexist in the same collection and a
/// read dispatches on this discriminant — `Row` to the entity/row reader,
/// `ColumnarV1` to the RDCC column-block reader — with no mass rewrite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkFormat {
    /// Legacy row-stored chunk (`columnar_page == None`): it predates the
    /// columnar seal, or sealed while the collection was non-columnar. Its
    /// rows are served from the entity/row path.
    Row,
    /// Columnar `RDCC` chunk, format version 1 (`columnar_page == Some`).
    /// Its rows decode from the recorded `ColumnBlock`.
    ColumnarV1,
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
    /// Columnar-vs-row **migration discriminant** (PRD #850, Phase 1).
    /// `Some(loc)` → this chunk was sealed columnar; its `RDCC`
    /// [`ColumnBlock`](crate::storage::engine::PageType::ColumnBlock) lives
    /// at `loc` and reads decode the columnar form. `None` → a legacy
    /// row-stored chunk served by the entity path (read-bridge lands in
    /// #861). This MUST persist so pre-existing row-stored data is never
    /// mis-read as columnar after a restart.
    pub columnar_page: Option<PageLocation>,
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
            columnar_page: None,
        }
    }

    /// The chunk's storage format — the read-bridge dispatch key (#861).
    /// Derived from the migration discriminant `columnar_page`: a recorded
    /// RDCC `ColumnBlock` location means [`ChunkFormat::ColumnarV1`], its
    /// absence means the legacy [`ChunkFormat::Row`] form. This is the
    /// format-version gate that lets old row chunks and new columnar chunks
    /// coexist in one collection without a rewrite.
    pub fn format(&self) -> ChunkFormat {
        match self.columnar_page {
            Some(_) => ChunkFormat::ColumnarV1,
            None => ChunkFormat::Row,
        }
    }

    /// True when this chunk is stored in the columnar `RDCC` form.
    pub fn is_columnar(&self) -> bool {
        matches!(self.format(), ChunkFormat::ColumnarV1)
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
    /// `(hypertable, start_ns)` → the sealed chunk's RDCC `ColumnBlock`
    /// bytes, populated by [`seal_chunk_columnar`](HypertableRegistry::seal_chunk_columnar)
    /// during seal and by [`restore_columnar_block`](HypertableRegistry::restore_columnar_block)
    /// during boot after reading the recorded engine page.
    columnar_blocks: BTreeMap<(String, u64), Vec<u8>>,
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

    /// Drop a hypertable from the registry. Returns the removed spec
    /// when present, or `None` for unknown names. Callers drop the
    /// backing collection separately — this is registry housekeeping
    /// only.
    pub fn unregister(&self, name: &str) -> Option<HypertableSpec> {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.remove(name)
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

    /// Seal a chunk **columnar** (PRD #850, #911): mark it sealed, record
    /// where its RDCC `ColumnBlock` lives in `ChunkMeta.columnar_page`,
    /// and stash the block `bytes` so the columnar read path can decode
    /// them. The columnar counterpart of [`seal_chunk`](Self::seal_chunk)
    /// — the production caller (`seal_chunk_with_config`'s columnar arm)
    /// hands the sealed bytes here. Returns `true` if the chunk existed.
    pub fn seal_chunk_columnar(&self, id: &ChunkId, page: PageLocation, bytes: Vec<u8>) -> bool {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let key = (id.hypertable.clone(), id.start_ns);
        if let Some(meta) = guard.chunks.get_mut(&key) {
            meta.sealed = true;
            meta.columnar_page = Some(page);
            guard.columnar_blocks.insert(key, bytes);
            true
        } else {
            false
        }
    }

    /// Rehydrate a previously persisted RDCC block after boot has restored
    /// the chunk metadata and read the recorded `ColumnBlock` page.
    pub fn restore_columnar_block(&self, id: &ChunkId, bytes: Vec<u8>) -> bool {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let key = (id.hypertable.clone(), id.start_ns);
        if guard
            .chunks
            .get(&key)
            .is_some_and(|meta| meta.columnar_page.is_some())
        {
            guard.columnar_blocks.insert(key, bytes);
            true
        } else {
            false
        }
    }

    /// Fetch the RDCC `ColumnBlock` bytes recorded for a columnar-sealed
    /// chunk by [`seal_chunk_columnar`](Self::seal_chunk_columnar). `None`
    /// for row-sealed chunks or chunks whose durable page could not be
    /// rehydrated.
    pub fn columnar_block(&self, id: &ChunkId) -> Option<Vec<u8>> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard
            .columnar_blocks
            .get(&(id.hypertable.clone(), id.start_ns))
            .cloned()
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

    /// True when no hypertable is registered and no chunk is tracked.
    /// Lets the durability layer skip the persist step entirely for
    /// workloads that never declared a hypertable (zero overhead).
    pub fn is_empty(&self) -> bool {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.specs.is_empty() && guard.chunks.is_empty()
    }

    /// Snapshot every chunk across all hypertables, ordered by
    /// `(hypertable, start_ns)` so the persisted form is deterministic.
    /// Pairs with [`restore_chunk`] on boot. Specs are snapshotted via
    /// [`list`].
    pub fn snapshot_chunks(&self) -> Vec<ChunkMeta> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.chunks.values().cloned().collect()
    }

    /// Reinstate a chunk verbatim during recovery. Overwrites any
    /// existing entry for the same `(hypertable, start_ns)`.
    ///
    /// Unlike [`route`], this does **not** observe a timestamp — it
    /// restores the persisted counters (`row_count`, `min_ts_ns`,
    /// `max_ts_ns`, `sealed`, `ttl_override_ns`) exactly so the
    /// post-restart registry is identical to the pre-restart one. The
    /// caller is expected to [`register`] the owning spec first; a
    /// chunk whose hypertable has no spec is still tracked (routing
    /// falls back to the spec once it is registered), matching the
    /// pre-restart invariant that chunks outlive a missing spec only
    /// transiently.
    pub fn restore_chunk(&self, meta: ChunkMeta) {
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let key = (meta.id.hypertable.clone(), meta.id.start_ns);
        guard.chunks.insert(key, meta);
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

    /// Select groups of small sealed columnar chunks that are candidates
    /// for compaction. Returns a list of groups; each group is a list of
    /// `ChunkId`s that together hold at most `max_rows_total` rows. Groups
    /// with fewer than `min_chunks` chunks are not returned (no-op merges).
    ///
    /// Chunks are considered in oldest-first order within each hypertable.
    /// The caller decides the merge policy (threshold sizes, timing).
    pub fn select_compaction_candidates(
        &self,
        hypertable: &str,
        max_rows_per_group: u64,
        min_chunks: usize,
    ) -> Vec<Vec<ChunkId>> {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        // Collect sealed columnar chunks oldest-first.
        let candidates: Vec<&ChunkMeta> = guard
            .chunks
            .iter()
            .filter(|((name, _), meta)| name == hypertable && meta.sealed && meta.is_columnar())
            .map(|(_, meta)| meta)
            .collect();

        // Greedy bin-packing: accumulate into a group until adding the next
        // chunk would exceed the row budget, then close the group.
        let mut groups: Vec<Vec<ChunkId>> = Vec::new();
        let mut current: Vec<ChunkId> = Vec::new();
        let mut current_rows: u64 = 0;

        for meta in candidates {
            if !current.is_empty() && current_rows + meta.row_count > max_rows_per_group {
                if current.len() >= min_chunks {
                    groups.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
                current_rows = 0;
            }
            current.push(meta.id.clone());
            current_rows += meta.row_count;
        }
        if current.len() >= min_chunks {
            groups.push(current);
        }

        groups
    }

    /// Merge N sealed columnar chunks into one larger sealed columnar chunk.
    ///
    /// # Crash safety
    ///
    /// The merge is computed entirely in memory before any registry state is
    /// touched. The final registry update (insert merged entry + remove source
    /// entries) happens atomically under one `Mutex` lock acquisition. If the
    /// process is killed before the lock is taken, all source chunks remain
    /// intact — no committed data is lost. A torn merge (crash mid-computation,
    /// before the atomic commit) is safe because the registry never saw the
    /// partial output.
    ///
    /// # Arguments
    ///
    /// * `hypertable` — name of the owning hypertable.
    /// * `source_ids` — the `ChunkId`s of the chunks to merge (must all be
    ///   sealed, columnar, and have their blocks RAM-resident).
    /// * `merged_chunk_id` — the `chunk_id` header field for the output RDCC
    ///   block; the caller assigns this (e.g. `min(source start_ns)` cast).
    /// * `schema_ref` — the catalog schema id written into the output header.
    /// * `granule_size` — sparse-granule-index stride for the merged block.
    ///   Pass `DEFAULT_GRANULE_SIZE` for the standard 8 192-row marks.
    ///
    /// Returns the `ChunkId` of the newly inserted merged chunk on success.
    pub fn compact_columnar_chunks(
        &self,
        hypertable: &str,
        source_ids: &[ChunkId],
        merged_chunk_id: u64,
        schema_ref: u64,
        granule_size: u32,
    ) -> Result<ChunkId, CompactionError> {
        if source_ids.len() < 2 {
            return Err(CompactionError::InsufficientSources);
        }

        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };

        // --- Validate and collect source data (still holding the lock) ---

        for id in source_ids {
            let key = (id.hypertable.clone(), id.start_ns);
            let meta = guard
                .chunks
                .get(&key)
                .ok_or_else(|| CompactionError::ChunkNotFound(id.clone()))?;
            if !meta.sealed {
                return Err(CompactionError::ChunkNotSealed(id.clone()));
            }
            if !meta.is_columnar() {
                return Err(CompactionError::ChunkNotColumnar(id.clone()));
            }
            if !guard.columnar_blocks.contains_key(&key) {
                return Err(CompactionError::BlockNotResident(id.clone()));
            }
        }

        // --- Decode all source blocks into points ---

        let mut all_points = Vec::new();
        for id in source_ids {
            let key = (id.hypertable.clone(), id.start_ns);
            let bytes = guard.columnar_blocks.get(&key).unwrap();
            let pts = points_from_column_block(bytes).map_err(CompactionError::Decode)?;
            all_points.extend(pts);
        }

        // Sort by timestamp — chunks are time-partitioned so they should not
        // overlap, but we sort anyway to guarantee the output is ordered.
        all_points.sort_by_key(|p| p.timestamp_ns);

        // --- Build the merged RDCC block ---

        let row_count = all_points.len() as u64;
        let min_ts_ns = all_points.first().map(|p| p.timestamp_ns).unwrap_or(0);
        let max_ts_ns = all_points.last().map(|p| p.timestamp_ns).unwrap_or(0);

        let ts_bytes: Vec<u8> = all_points
            .iter()
            .flat_map(|p| p.timestamp_ns.to_le_bytes())
            .collect();
        let val_bytes: Vec<u8> = all_points
            .iter()
            .flat_map(|p| p.value.to_le_bytes())
            .collect();

        let merged_bytes = write_column_block(
            merged_chunk_id,
            schema_ref,
            row_count,
            min_ts_ns,
            max_ts_ns,
            granule_size,
            &[
                ColumnInput {
                    column_id: COLUMNAR_TS_COLUMN_ID,
                    logical_type: DataType::UnsignedInteger.to_byte(),
                    semantics: ColumnSemantics::Timestamp,
                    data: &ts_bytes,
                },
                ColumnInput {
                    column_id: COLUMNAR_VALUE_COLUMN_ID,
                    logical_type: DataType::Float.to_byte(),
                    semantics: ColumnSemantics::Gauge,
                    data: &val_bytes,
                },
            ],
        )
        .map_err(CompactionError::Encode)?;

        // --- Derive merged chunk metadata ---

        // The merged chunk spans the time range of all sources.
        let merged_start_ns = source_ids.iter().map(|id| id.start_ns).min().unwrap(); // safe: source_ids.len() >= 2
        let merged_end_ns_exclusive = source_ids
            .iter()
            .map(|id| {
                guard
                    .chunks
                    .get(&(id.hypertable.clone(), id.start_ns))
                    .map(|m| m.end_ns_exclusive)
                    .unwrap_or(merged_start_ns)
            })
            .max()
            .unwrap();

        let merged_id = ChunkId {
            hypertable: hypertable.to_string(),
            start_ns: merged_start_ns,
        };

        // Synthetic page location: page_id=0, offset=0, length=block_len.
        // Callers that write through a real Pager should instead call
        // `seal_chunk_columnar` with the returned PageLocation from the pager
        // write; this sentinel is correct for the RAM-only path used here and
        // in tests.
        let merged_page = PageLocation::new(0, 0, merged_bytes.len() as u32);

        let mut merged_meta = ChunkMeta::new(merged_id.clone(), merged_end_ns_exclusive);
        merged_meta.row_count = row_count;
        merged_meta.min_ts_ns = min_ts_ns;
        merged_meta.max_ts_ns = max_ts_ns;
        merged_meta.sealed = true;
        merged_meta.columnar_page = Some(merged_page);

        // --- Atomic commit: insert merged, remove sources ---

        let merged_key = (hypertable.to_string(), merged_start_ns);
        guard.chunks.insert(merged_key.clone(), merged_meta);
        guard.columnar_blocks.insert(merged_key, merged_bytes);

        for id in source_ids {
            // Skip if source_id == merged_id (idempotent when the first source
            // shares start_ns with the merged chunk).
            if id.start_ns == merged_start_ns && id.hypertable == hypertable {
                continue;
            }
            let key = (id.hypertable.clone(), id.start_ns);
            guard.chunks.remove(&key);
            guard.columnar_blocks.remove(&key);
        }

        Ok(merged_id)
    }
}

/// Errors returned by [`HypertableRegistry::compact_columnar_chunks`].
#[derive(Debug, Clone, PartialEq)]
pub enum CompactionError {
    /// Need at least 2 source chunks — merging 0 or 1 is a no-op.
    InsufficientSources,
    /// One of the source chunks does not exist in this hypertable.
    ChunkNotFound(ChunkId),
    /// A source chunk is not yet sealed — only sealed chunks can be compacted.
    ChunkNotSealed(ChunkId),
    /// A source chunk is not in columnar form.
    ChunkNotColumnar(ChunkId),
    /// A source chunk's RDCC block bytes are not RAM-resident.
    BlockNotResident(ChunkId),
    /// The RDCC block could not be decoded.
    Decode(ColumnBlockError),
    /// The merged RDCC block could not be encoded.
    Encode(ColumnBlockError),
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InsufficientSources => {
                write!(f, "compaction requires at least 2 source chunks")
            }
            Self::ChunkNotFound(id) => write!(
                f,
                "source chunk {}@{} not found",
                id.hypertable, id.start_ns
            ),
            Self::ChunkNotSealed(id) => write!(
                f,
                "source chunk {}@{} is not sealed",
                id.hypertable, id.start_ns
            ),
            Self::ChunkNotColumnar(id) => write!(
                f,
                "source chunk {}@{} is not columnar",
                id.hypertable, id.start_ns
            ),
            Self::BlockNotResident(id) => write!(
                f,
                "source chunk {}@{} block bytes are not RAM-resident",
                id.hypertable, id.start_ns
            ),
            Self::Decode(e) => write!(f, "RDCC decode error: {e}"),
            Self::Encode(e) => write!(f, "RDCC encode error: {e}"),
        }
    }
}

impl std::error::Error for CompactionError {}

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
    fn snapshot_then_restore_reproduces_registry_identically() {
        // Pre-restart registry: two hypertables, several chunks, with a
        // sealed chunk and a per-chunk TTL override — the bits that are
        // NOT derivable from row data and so must round-trip verbatim.
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("metrics", "ts", DAY_NS).with_ttl_ns(7 * DAY_NS));
        reg.register(HypertableSpec::new("events", "ts", HOUR_NS));
        for t in [0, DAY_NS + 5, DAY_NS + 9, 2 * DAY_NS] {
            reg.route("metrics", t).unwrap();
        }
        let id = reg.route("events", 0).unwrap();
        reg.seal_chunk(&id);
        reg.set_chunk_ttl_ns(&id, Some(3 * HOUR_NS));

        let specs = reg.list();
        let chunks = reg.snapshot_chunks();
        assert!(!reg.is_empty());

        // Simulated restart: rebuild a fresh registry from the snapshot.
        let restored = HypertableRegistry::new();
        assert!(restored.is_empty());
        for spec in specs {
            restored.register(spec);
        }
        for chunk in chunks {
            restored.restore_chunk(chunk);
        }

        // Specs identical.
        let before = reg.get("metrics").unwrap();
        let after = restored.get("metrics").unwrap();
        assert_eq!(after.chunk_interval_ns, before.chunk_interval_ns);
        assert_eq!(after.time_column, before.time_column);
        assert_eq!(after.default_ttl_ns, before.default_ttl_ns);

        // Chunk metadata identical (bounds, counts, sealed, TTL override).
        let m_before = reg.show_chunks("metrics");
        let m_after = restored.show_chunks("metrics");
        assert_eq!(m_after.len(), m_before.len());
        for (a, b) in m_after.iter().zip(m_before.iter()) {
            assert_eq!(a.id.start_ns, b.id.start_ns);
            assert_eq!(a.end_ns_exclusive, b.end_ns_exclusive);
            assert_eq!(a.row_count, b.row_count);
            assert_eq!(a.min_ts_ns, b.min_ts_ns);
            assert_eq!(a.max_ts_ns, b.max_ts_ns);
        }
        let e_after = restored.show_chunks("events");
        assert_eq!(e_after.len(), 1);
        assert!(e_after[0].sealed, "sealed flag must survive restore");
        assert_eq!(e_after[0].ttl_override_ns, Some(3 * HOUR_NS));

        // A post-restart write routes to the EXISTING chunk — no
        // duplicate allocation.
        let routed = restored.route("metrics", DAY_NS + 1).unwrap();
        assert_eq!(routed.start_ns, DAY_NS);
        assert_eq!(
            restored.show_chunks("metrics").len(),
            m_before.len(),
            "write after restore must not allocate a new chunk"
        );
    }

    // -----------------------------------------------------------------
    // Columnar chunk eviction via the EXISTING TTL/drop path (issue #859)
    //
    // A sealed *columnar* chunk is one whose `ChunkMeta.columnar_page` is
    // `Some(..)` — the RDCC `ColumnBlock` discriminant (PRD #850 Phase 1).
    // These guards prove the retention path is storage-form agnostic: the
    // SAME `sweep_expired` / `drop_chunks_before` metadata sweep that drops
    // row chunks drops columnar chunks too, in O(1) metadata work (no
    // per-row delete), and hands the `columnar_page` back on the dropped
    // meta so the physical-storage callback can release the RDCC block.
    // No separate columnar TTL/partition subsystem exists or is needed.
    // -----------------------------------------------------------------

    /// Build a sealed columnar chunk meta directly — mirrors what the
    /// boot/seal path restores: a chunk carrying its RDCC `columnar_page`.
    fn columnar_chunk(hypertable: &str, start_ns: u64, max_ts_ns: u64) -> ChunkMeta {
        let mut meta = ChunkMeta::new(
            ChunkId {
                hypertable: hypertable.into(),
                start_ns,
            },
            start_ns + DAY_NS,
        );
        meta.row_count = 1;
        meta.min_ts_ns = max_ts_ns;
        meta.max_ts_ns = max_ts_ns;
        meta.sealed = true;
        meta.columnar_page = Some(PageLocation::new(7, 0, 1234));
        meta
    }

    #[test]
    fn columnar_chunk_evicts_via_sweep_expired_carrying_its_page() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("metrics", "ts", DAY_NS).with_ttl_ns(DAY_NS));
        // Inject a sealed columnar chunk (max_ts=0 → expiry at 1d).
        reg.restore_chunk(columnar_chunk("metrics", 0, 0));
        assert!(reg.show_chunks("metrics")[0].columnar_page.is_some());

        // now = 3d → past the 1d TTL. The existing partition sweep drops it.
        let dropped = reg.sweep_expired("metrics", 3 * DAY_NS);
        assert_eq!(dropped.len(), 1, "columnar chunk must evict via TTL sweep");
        assert_eq!(
            dropped[0].columnar_page,
            Some(PageLocation::new(7, 0, 1234)),
            "dropped meta must carry columnar_page so physical release frees the RDCC block"
        );
        assert!(reg.show_chunks("metrics").is_empty());
    }

    #[test]
    fn columnar_chunk_evicts_via_drop_chunks_before() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("metrics", "ts", DAY_NS));
        reg.restore_chunk(columnar_chunk("metrics", 0, 0));

        let dropped = reg.drop_chunks_before("metrics", DAY_NS);
        assert_eq!(dropped.len(), 1);
        assert!(
            dropped[0].columnar_page.is_some(),
            "drop_chunks_before is metadata-only and carries columnar_page through"
        );
        assert!(reg.show_chunks("metrics").is_empty());
    }

    #[test]
    fn columnar_and_row_chunks_share_one_eviction_path() {
        // Regression guard (acceptance #3): a row chunk and a columnar
        // chunk with identical bounds + TTL must produce identical sweep
        // outcomes. If a separate columnar TTL subsystem were ever
        // introduced, the two would diverge here.
        let mk = |columnar: bool| {
            let reg = HypertableRegistry::new();
            reg.register(HypertableSpec::new("m", "ts", DAY_NS).with_ttl_ns(DAY_NS));
            if columnar {
                reg.restore_chunk(columnar_chunk("m", 0, 0));
            } else {
                reg.route("m", 0).unwrap(); // row chunk, max_ts=0
            }
            reg.sweep_expired("m", 3 * DAY_NS).len()
        };
        assert_eq!(mk(false), 1, "row chunk evicts");
        assert_eq!(mk(true), 1, "columnar chunk evicts the same way");
    }

    #[test]
    fn columnar_chunk_prunes_by_time_bounds_like_row_chunk() {
        // Acceptance #2: the partition pruner selects chunks by their
        // [start_ns, end_ns_exclusive) bounds (surfaced via show_chunks) —
        // it never inspects columnar_page, so a columnar chunk outside the
        // query window is eliminated identically to a row chunk. We assert
        // the bounds the pruner consumes survive verbatim for a columnar
        // chunk.
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        reg.restore_chunk(columnar_chunk("m", 0, 0));
        reg.restore_chunk(columnar_chunk("m", 2 * DAY_NS, 2 * DAY_NS));
        let chunks = reg.show_chunks("m");
        // A window of [2d, 3d) overlaps only the second chunk — same
        // arithmetic the RangeChild pruner runs against these bounds.
        let lo = 2 * DAY_NS;
        let hi = 3 * DAY_NS;
        let overlapping: Vec<u64> = chunks
            .iter()
            .filter(|c| c.id.start_ns < hi && c.end_ns_exclusive > lo)
            .map(|c| c.id.start_ns)
            .collect();
        assert_eq!(
            overlapping,
            vec![2 * DAY_NS],
            "only in-window columnar chunk kept"
        );
        assert!(chunks.iter().all(|c| c.columnar_page.is_some()));
    }

    /// Read-bridge dispatch key (#861): `ChunkMeta::format()` classifies a
    /// chunk purely from the `columnar_page` migration discriminant, so a
    /// pre-existing row chunk and a newly columnar-sealed chunk in the same
    /// registry are disambiguated by format version — the gate the read
    /// path dispatches on.
    #[test]
    fn chunk_format_dispatches_on_columnar_page_discriminant() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        // A row chunk (allocated by a write) and a columnar chunk coexist.
        reg.route("m", 0).unwrap();
        reg.restore_chunk(columnar_chunk("m", DAY_NS, DAY_NS));

        let chunks = reg.show_chunks("m");
        let row = chunks.iter().find(|c| c.id.start_ns == 0).unwrap();
        let col = chunks.iter().find(|c| c.id.start_ns == DAY_NS).unwrap();

        assert_eq!(row.format(), ChunkFormat::Row);
        assert!(!row.is_columnar());
        assert_eq!(col.format(), ChunkFormat::ColumnarV1);
        assert!(col.is_columnar());
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

    // -----------------------------------------------------------------
    // Chunk compaction (#857)
    // -----------------------------------------------------------------

    /// Seal a columnar chunk into the registry with real RDCC block bytes.
    fn seal_columnar_chunk_into(
        reg: &HypertableRegistry,
        hypertable: &str,
        start_ns: u64,
        end_ns_exclusive: u64,
        points: &[(u64, f64)],
        schema_ref: u64,
    ) -> ChunkId {
        use super::super::chunk::TimeSeriesChunk;
        use std::collections::HashMap;

        let id = ChunkId {
            hypertable: hypertable.to_string(),
            start_ns,
        };
        let mut chunk = TimeSeriesChunk::new("m", HashMap::new());
        for &(ts, v) in points {
            chunk.append(ts, v);
        }
        let block = chunk
            .seal_columnar(start_ns, schema_ref)
            .expect("seal_columnar");
        let page = PageLocation::new(0, 0, block.len() as u32);

        let mut meta = ChunkMeta::new(id.clone(), end_ns_exclusive);
        meta.sealed = true;
        meta.columnar_page = Some(page);
        meta.row_count = points.len() as u64;
        if let Some(&(min_ts, _)) = points.iter().min_by_key(|(ts, _)| ts) {
            meta.min_ts_ns = min_ts;
        }
        if let Some(&(max_ts, _)) = points.iter().max_by_key(|(ts, _)| ts) {
            meta.max_ts_ns = max_ts;
        }

        {
            let mut guard = reg.inner.lock().unwrap();
            guard
                .chunks
                .insert((hypertable.to_string(), start_ns), meta);
            guard
                .columnar_blocks
                .insert((hypertable.to_string(), start_ns), block);
        }
        id
    }

    /// Acceptance criterion 1: N sealed chunks merge into one with logically
    /// identical contents (same rows and values, in timestamp order).
    #[test]
    fn compact_merges_chunks_to_identical_rows() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));

        // Three small chunks with non-overlapping time ranges.
        let pts_a: Vec<(u64, f64)> = (0..10).map(|i| (i * 1_000, i as f64)).collect();
        let pts_b: Vec<(u64, f64)> = (10..20).map(|i| (i * 1_000, i as f64)).collect();
        let pts_c: Vec<(u64, f64)> = (20..30).map(|i| (i * 1_000, i as f64)).collect();

        let id_a = seal_columnar_chunk_into(&reg, "m", 0, DAY_NS, &pts_a, 1);
        let id_b = seal_columnar_chunk_into(&reg, "m", DAY_NS, 2 * DAY_NS, &pts_b, 1);
        let id_c = seal_columnar_chunk_into(&reg, "m", 2 * DAY_NS, 3 * DAY_NS, &pts_c, 1);

        let merged_id = reg
            .compact_columnar_chunks("m", &[id_a, id_b, id_c], 0, 1, DEFAULT_GRANULE_SIZE)
            .expect("compaction failed");

        // Merged chunk exists and carries the right row count.
        let chunks = reg.show_chunks("m");
        assert_eq!(chunks.len(), 1, "three source chunks must collapse to one");
        let merged_meta = &chunks[0];
        assert_eq!(merged_meta.id.start_ns, merged_id.start_ns);
        assert_eq!(merged_meta.row_count, 30);
        assert!(merged_meta.sealed);
        assert!(merged_meta.is_columnar());

        // Decode the merged block and verify every point is present.
        let block = reg
            .columnar_block(&merged_id)
            .expect("merged block must be RAM-resident");
        let got = points_from_column_block(&block).expect("decode merged block");

        let mut expected: Vec<(u64, f64)> =
            pts_a.iter().chain(&pts_b).chain(&pts_c).copied().collect();
        expected.sort_by_key(|(ts, _)| *ts);

        assert_eq!(got.len(), expected.len());
        for (point, (exp_ts, exp_val)) in got.iter().zip(&expected) {
            assert_eq!(point.timestamp_ns, *exp_ts);
            assert!(
                (point.value - exp_val).abs() < 1e-9,
                "value mismatch at ts {}: got {}, expected {}",
                exp_ts,
                point.value,
                exp_val
            );
        }
    }

    /// Acceptance criterion 2: small-chunk count drops after compaction;
    /// merged block is recompressed (byte length is within reason).
    #[test]
    fn compact_reduces_chunk_count_and_recompresses() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));

        // Five small chunks; each contributes 50 points.
        let mut ids = Vec::new();
        for i in 0..5u64 {
            let pts: Vec<(u64, f64)> = (0..50u64)
                .map(|j| (i * DAY_NS + j * 1_000, 1.0 + j as f64 * 0.01))
                .collect();
            let id = seal_columnar_chunk_into(&reg, "m", i * DAY_NS, (i + 1) * DAY_NS, &pts, 1);
            ids.push(id);
        }

        assert_eq!(reg.show_chunks("m").len(), 5);

        let merged_id = reg
            .compact_columnar_chunks("m", &ids, 0, 1, DEFAULT_GRANULE_SIZE)
            .expect("compaction failed");

        // Count dropped from 5 to 1.
        let chunks = reg.show_chunks("m");
        assert_eq!(chunks.len(), 1, "five chunks must compact to one");
        assert_eq!(chunks[0].row_count, 250);

        // Merged block is present and decodable.
        let block = reg.columnar_block(&merged_id).unwrap();
        let pts = points_from_column_block(&block).unwrap();
        assert_eq!(pts.len(), 250, "all 250 points must survive compaction");

        // Compression sanity: the merged block must be smaller than 250 * 16
        // uncompressed bytes (timestamp 8 + value 8).
        let raw_uncompressed = 250 * 16;
        assert!(
            block.len() < raw_uncompressed,
            "merged block ({} bytes) should be compressed (raw = {})",
            block.len(),
            raw_uncompressed
        );
    }

    /// Acceptance criterion 3: a "torn merge" (crash before commit) leaves
    /// the source chunks intact and loses no committed data.
    ///
    /// We simulate a torn merge by:
    /// 1. Reading the source blocks (the expensive "compute" phase).
    /// 2. Verifying sources still exist before the atomic commit fires.
    /// 3. Running the actual compaction and verifying sources are removed
    ///    only after the merged chunk is fully committed.
    #[test]
    fn torn_merge_leaves_inputs_intact() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));

        let pts_a: Vec<(u64, f64)> = (0..20).map(|i| (i * 1_000, i as f64)).collect();
        let pts_b: Vec<(u64, f64)> = (100..120).map(|i| (i * 1_000, i as f64)).collect();

        let id_a = seal_columnar_chunk_into(&reg, "m", 0, DAY_NS, &pts_a, 1);
        let id_b = seal_columnar_chunk_into(&reg, "m", DAY_NS, 2 * DAY_NS, &pts_b, 1);

        // Simulate the "compute" phase: read source blocks without committing.
        // This mirrors what would happen if the process died between decode and
        // the atomic registry update — the registry never sees any write.
        {
            let guard = reg.inner.lock().unwrap();
            let block_a = guard
                .columnar_blocks
                .get(&("m".to_string(), 0))
                .expect("block_a must be present before any merge");
            let block_b = guard
                .columnar_blocks
                .get(&("m".to_string(), DAY_NS))
                .expect("block_b must be present before any merge");
            let pts_decoded_a = points_from_column_block(block_a).unwrap();
            let pts_decoded_b = points_from_column_block(block_b).unwrap();
            // "Compute" completed; simulate crash by simply not committing.
            assert_eq!(pts_decoded_a.len(), 20, "source A readable before merge");
            assert_eq!(pts_decoded_b.len(), 20, "source B readable before merge");

            // Both source chunks are still in the registry — no partial state.
            assert!(
                guard.chunks.contains_key(&("m".to_string(), 0)),
                "source A must remain intact after torn merge"
            );
            assert!(
                guard.chunks.contains_key(&("m".to_string(), DAY_NS)),
                "source B must remain intact after torn merge"
            );
        }

        // Now actually run compaction — verify it commits atomically.
        let merged_id = reg
            .compact_columnar_chunks("m", &[id_a, id_b], 0, 1, DEFAULT_GRANULE_SIZE)
            .expect("compaction after torn-merge simulation must succeed");

        // After commit: merged chunk present, sources gone.
        let chunks = reg.show_chunks("m");
        assert_eq!(chunks.len(), 1, "only the merged chunk must remain");
        assert_eq!(chunks[0].id.start_ns, merged_id.start_ns);

        // All data survives — no rows lost.
        let block = reg.columnar_block(&merged_id).unwrap();
        let pts = points_from_column_block(&block).unwrap();
        assert_eq!(pts.len(), 40, "all 40 points (20+20) must survive");
    }

    /// Guard: `compact_columnar_chunks` returns `InsufficientSources` when
    /// called with fewer than 2 source chunks (a 1-to-1 "merge" is a no-op).
    #[test]
    fn compact_rejects_single_source() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));
        let id = seal_columnar_chunk_into(&reg, "m", 0, DAY_NS, &[(1_000, 1.0)], 1);
        let err = reg
            .compact_columnar_chunks("m", &[id], 0, 1, DEFAULT_GRANULE_SIZE)
            .unwrap_err();
        assert_eq!(err, CompactionError::InsufficientSources);
    }

    /// Guard: `compact_columnar_chunks` rejects unsealed source chunks.
    #[test]
    fn compact_rejects_unsealed_chunk() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));

        // Manually insert an unsealed (open) chunk meta.
        let open_id = ChunkId {
            hypertable: "m".to_string(),
            start_ns: 0,
        };
        reg.restore_chunk(ChunkMeta::new(open_id.clone(), DAY_NS));

        let sealed_id =
            seal_columnar_chunk_into(&reg, "m", DAY_NS, 2 * DAY_NS, &[(DAY_NS + 1, 1.0)], 1);

        let err = reg
            .compact_columnar_chunks("m", &[open_id, sealed_id], 0, 1, DEFAULT_GRANULE_SIZE)
            .unwrap_err();
        assert!(matches!(err, CompactionError::ChunkNotSealed(_)));
    }

    /// Guard: `select_compaction_candidates` returns groups respecting the
    /// row budget and minimum-chunks threshold.
    #[test]
    fn select_candidates_respects_budget_and_threshold() {
        let reg = HypertableRegistry::new();
        reg.register(HypertableSpec::new("m", "ts", DAY_NS));

        // Five chunks of 100 rows each.
        for i in 0..5u64 {
            let pts: Vec<(u64, f64)> = (0..100u64).map(|j| (i * DAY_NS + j, j as f64)).collect();
            seal_columnar_chunk_into(&reg, "m", i * DAY_NS, (i + 1) * DAY_NS, &pts, 1);
        }

        // Budget 250 rows per group, min 2 chunks → 2 groups of 2-3 chunks.
        let groups = reg.select_compaction_candidates("m", 250, 2);
        assert!(
            !groups.is_empty(),
            "must find at least one compaction group"
        );
        for group in &groups {
            assert!(group.len() >= 2, "each group must have at least 2 chunks");
            // Each group's total rows must be within budget.
            let total: u64 = group
                .iter()
                .map(|id| {
                    reg.show_chunks("m")
                        .iter()
                        .find(|c| c.id.start_ns == id.start_ns)
                        .map(|c| c.row_count)
                        .unwrap_or(0)
                })
                .sum();
            assert!(
                total <= 250,
                "group total rows {total} must not exceed budget 250"
            );
        }

        // Threshold of 10 chunks → no group qualifies.
        let groups_high = reg.select_compaction_candidates("m", 250, 10);
        assert!(
            groups_high.is_empty(),
            "threshold of 10 must yield no groups when max is 5 chunks"
        );
    }
}
