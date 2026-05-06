//! Sharded, bounded LRU cache for entity lookups by raw id.
//!
//! Replaces the original store-wide `RwLock<HashMap<u64, (String, UnifiedEntity)>>`
//! that backed `UnifiedStore::get_any()`. The original cache had three problems
//! flagged in `docs/perf/delete-sequential-2026-05-06.md` (#85):
//!
//! 1. A single `RwLock` serialised every lookup and every invalidation,
//!    so a delete-heavy workload paid one global write lock per row.
//! 2. Eviction was naïve ("drop the first key the iterator yields") with no
//!    recency tracking, so hot entries could be discarded before cold ones.
//! 3. There was no observability: hit-rate had to be guessed from access
//!    patterns. The OLTP DELETE workload (hit-rate ≈ 0%) was indistinguishable
//!    from a graph-traversal workload (hit-rate > 0%) at the metric layer.
//!
//! This module solves all three:
//!
//! - **Sharding** by `id & (N_SHARDS - 1)` cuts contention by `N_SHARDS`×.
//!   Invalidations from `delete_batch` only collide with reads on the same
//!   shard.
//! - **Bounded LRU** per shard: a `HashMap` carries the entries and a
//!   `VecDeque<u64>` carries access order. On a hit we mark the key
//!   most-recently-used; on capacity overflow we drop the least-recently-used.
//! - **Counters** (`AtomicU64` for hits / misses / evictions) drive
//!   `EntityCache::hit_rate()` for live observability.
//!
//! ## Shape
//!
//! Same key/value shape as the original cache so callers don't change:
//!
//! ```ignore
//! type Key   = u64;                    // EntityId::raw()
//! type Value = (String, UnifiedEntity);// (collection, entity)
//! ```

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::RwLock;

use super::entity::UnifiedEntity;

/// Number of shards. Must be a power of two so we can replace the modulo
/// with a bitmask. Sixteen is a reasonable compromise between extra memory
/// (one `RwLock` + two collections per shard) and contention reduction —
/// we expect at most a handful of concurrent writers, so doubling beyond
/// 16 buys nothing on the workloads that motivated this rewrite.
const N_SHARDS: usize = 16;
const SHARD_MASK: u64 = (N_SHARDS as u64) - 1;

/// Total cache capacity (across all shards). Matches the previous 10_000
/// behaviour from the flat `HashMap`.
const DEFAULT_CAPACITY: usize = 10_000;

#[inline]
fn shard_capacity(total: usize) -> usize {
    // Round up so total ≥ DEFAULT_CAPACITY even if not divisible.
    total.div_ceil(N_SHARDS)
}

/// Per-shard bounded LRU.
///
/// `entries` owns the values; `order` is a recency queue from oldest
/// (front) to newest (back). On hit we move the key to the back; on miss
/// we push and, if over capacity, pop the front.
///
/// We keep the implementation minimal: O(n) `retain` for the recency queue
/// on hits is fine for our shard size (≈ 625 entries) and avoids the
/// borrow-checker pain of a hand-rolled doubly-linked list. If we ever
/// want true O(1), the `lru` crate is a drop-in replacement — but the
/// hot path in #85 is the *invalidation*, not the read.
struct Shard {
    entries: HashMap<u64, (String, UnifiedEntity)>,
    order: VecDeque<u64>,
    capacity: usize,
}

impl Shard {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn touch(&mut self, key: u64) {
        // Move `key` to the back of the recency queue. O(n) in shard size.
        if let Some(pos) = self.order.iter().position(|&k| k == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key);
    }

    fn get(&mut self, key: u64) -> Option<(String, UnifiedEntity)> {
        let value = self.entries.get(&key).cloned()?;
        self.touch(key);
        Some(value)
    }

    /// Insert and return the number of evicted entries (0 or 1).
    fn insert(&mut self, key: u64, value: (String, UnifiedEntity)) -> usize {
        if self.entries.insert(key, value).is_some() {
            self.touch(key);
            return 0;
        }
        self.order.push_back(key);
        if self.entries.len() > self.capacity {
            if let Some(victim) = self.order.pop_front() {
                self.entries.remove(&victim);
                return 1;
            }
        }
        0
    }

    fn remove(&mut self, key: u64) -> bool {
        if self.entries.remove(&key).is_some() {
            if let Some(pos) = self.order.iter().position(|&k| k == key) {
                self.order.remove(pos);
            }
            true
        } else {
            false
        }
    }

    fn retain<F>(&mut self, mut keep: F)
    where
        F: FnMut(u64, &(String, UnifiedEntity)) -> bool,
    {
        self.entries.retain(|k, v| keep(*k, v));
        self.order.retain(|k| self.entries.contains_key(k));
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Sharded, bounded LRU cache for `UnifiedStore::get_any` lookups.
pub struct EntityCache {
    shards: [RwLock<Shard>; N_SHARDS],
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
}

impl EntityCache {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    pub fn with_capacity(total_capacity: usize) -> Self {
        let per_shard = shard_capacity(total_capacity);
        // `[T; N]` with non-Copy T can't be built via `[expr; N]`; build
        // by repeated initialisation.
        let shards = std::array::from_fn(|_| RwLock::new(Shard::new(per_shard)));
        Self {
            shards,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    #[inline]
    fn shard_for(&self, key: u64) -> &RwLock<Shard> {
        &self.shards[(key & SHARD_MASK) as usize]
    }

    /// Cache lookup. Updates `hits` / `misses`.
    ///
    /// Acquires the shard write lock on a hit (LRU touch needs it). If the
    /// touch cost ever shows up in profiles we can split into a fast read-only
    /// probe + deferred touch, but at shard size ≈ 625 it's not the bottleneck.
    pub fn get(&self, key: u64) -> Option<(String, UnifiedEntity)> {
        let mut shard = self.shard_for(key).write();
        match shard.get(key) {
            Some(v) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(v)
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Insert. Counts evictions for observability.
    pub fn insert(&self, key: u64, value: (String, UnifiedEntity)) {
        let mut shard = self.shard_for(key).write();
        let evicted = shard.insert(key, value);
        if evicted > 0 {
            self.evictions.fetch_add(evicted as u64, Ordering::Relaxed);
        }
    }

    /// Remove a single key. Returns `true` if the key was present.
    ///
    /// Hot path on `delete_batch` / `delete`. Probes with a *read* lock first:
    /// most deletes target rows that were never read, so the cache miss case
    /// avoids the write-lock acquisition entirely. This is the load-bearing
    /// optimisation for #85's `delete_sequential` regression — the original
    /// code took the write lock unconditionally, even for a 100 % miss
    /// workload.
    pub fn remove(&self, key: u64) -> bool {
        let lock = self.shard_for(key);
        {
            let shard = lock.read();
            if !shard.entries.contains_key(&key) {
                return false;
            }
        }
        lock.write().remove(key)
    }

    /// Bulk remove. Same fast-path optimisation as `remove`: skip the
    /// shard's write lock entirely when none of its candidate keys are
    /// present.
    pub fn remove_many(&self, keys: impl IntoIterator<Item = u64>) {
        // Bucket keys by shard so we acquire each lock at most once.
        let mut buckets: [Vec<u64>; N_SHARDS] = std::array::from_fn(|_| Vec::new());
        for key in keys {
            buckets[(key & SHARD_MASK) as usize].push(key);
        }
        for (idx, bucket) in buckets.into_iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let lock = &self.shards[idx];
            // Read-lock probe — skip the write lock if nothing matches.
            {
                let shard = lock.read();
                if shard.is_empty() {
                    continue;
                }
                if !bucket.iter().any(|k| shard.entries.contains_key(k)) {
                    continue;
                }
            }
            let mut shard = lock.write();
            for key in bucket {
                shard.remove(key);
            }
        }
    }

    /// Drop every entry whose `(collection, entity)` value fails `keep`.
    /// Used by collection-drop paths.
    pub fn retain<F>(&self, mut keep: F)
    where
        F: FnMut(u64, &(String, UnifiedEntity)) -> bool,
    {
        for shard in &self.shards {
            shard.write().retain(&mut keep);
        }
    }

    /// Total entries across all shards. Diagnostic only — locks every shard.
    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.read().entries.len()).sum()
    }

    /// Hit / (hit + miss). Returns `None` if no lookups have occurred.
    pub fn hit_rate(&self) -> Option<f64> {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 {
            None
        } else {
            Some(hits as f64 / total as f64)
        }
    }

    /// Snapshot of the three counters: `(hits, misses, evictions)`.
    pub fn stats(&self) -> EntityCacheStats {
        EntityCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            entries: self.len(),
        }
    }
}

impl Default for EntityCache {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::unified::entity::{
        EntityData, EntityId, EntityKind, NodeData, UnifiedEntity,
    };

    fn make_entity(id: u64) -> UnifiedEntity {
        UnifiedEntity::new(
            EntityId::new(id),
            EntityKind::Vector {
                collection: "test".to_string(),
            },
            EntityData::Node(NodeData::new()),
        )
    }

    #[test]
    fn miss_then_hit_updates_counters() {
        let cache = EntityCache::new();
        assert!(cache.get(42).is_none());
        cache.insert(42, ("col".into(), make_entity(42)));
        let got = cache.get(42).expect("should hit");
        assert_eq!(got.0, "col");
        let stats = cache.stats();
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(cache.hit_rate(), Some(0.5));
    }

    #[test]
    fn lru_evicts_oldest_when_shard_full() {
        // Capacity 32 → 2/shard. Pin keys to one shard by using multiples of 16.
        let cache = EntityCache::with_capacity(32);
        let s = 16; // shard 0 stride

        cache.insert(s, ("c".into(), make_entity(s)));
        cache.insert(s * 2, ("c".into(), make_entity(s * 2)));
        // Touch first key — second becomes LRU.
        let _ = cache.get(s);
        cache.insert(s * 3, ("c".into(), make_entity(s * 3)));

        assert!(cache.get(s).is_some(), "first key recently used, kept");
        assert!(cache.get(s * 2).is_none(), "second key was LRU, evicted");
        assert!(cache.get(s * 3).is_some(), "newest key kept");
        assert!(cache.stats().evictions >= 1);
    }

    #[test]
    fn remove_fast_path_skips_write_lock_on_cold_cache() {
        // We can't observe the lock acquisition directly, but we can check
        // remove() returns false without panicking and the cache stays empty.
        let cache = EntityCache::new();
        assert!(!cache.remove(123));
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn remove_many_handles_mixed_shards() {
        let cache = EntityCache::new();
        for k in 0..32u64 {
            cache.insert(k, ("c".into(), make_entity(k)));
        }
        assert_eq!(cache.len(), 32);
        cache.remove_many(0..32);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn retain_drops_entries_failing_predicate() {
        let cache = EntityCache::new();
        cache.insert(1, ("keep".into(), make_entity(1)));
        cache.insert(2, ("drop".into(), make_entity(2)));
        cache.retain(|_, (col, _)| col == "keep");
        assert!(cache.get(1).is_some());
        assert!(cache.get(2).is_none());
    }

    #[test]
    fn hit_rate_none_until_first_lookup() {
        let cache = EntityCache::new();
        assert_eq!(cache.hit_rate(), None);
        cache.insert(1, ("c".into(), make_entity(1)));
        // insert alone shouldn't drive hit_rate
        assert_eq!(cache.hit_rate(), None);
        let _ = cache.get(1);
        assert_eq!(cache.hit_rate(), Some(1.0));
    }
}
