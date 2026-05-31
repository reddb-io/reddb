//! SIEVE Page Cache Implementation
//!
//! A simple, efficient page cache using the SIEVE eviction algorithm.
//! SIEVE outperforms LRU in many workloads while being simpler to implement.
//!
//! # SIEVE Algorithm (NSDI '24)
//!
//! SIEVE uses a single "visited" bit instead of LRU's complex list management:
//! 1. On cache hit: set visited = true
//! 2. On cache miss (insertion): add to FIFO queue
//! 3. On eviction: sweep from "hand" position
//!    - If visited=true: clear bit, skip
//!    - If visited=false: evict this entry
//!
//! # References
//!
//! - Turso `core/storage/page_cache.rs:24-80` - PageCacheShardEntry with ref_bit
//! - Turso `core/storage/page_cache.rs:129-150` - advance_clock_hand()
//! - "SIEVE is Simpler than LRU" (NSDI '24)

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::page::Page;

fn cache_read<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    lock.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cache_write<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    lock.write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cache_lock<'a, T>(lock: &'a Mutex<T>) -> MutexGuard<'a, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Default cache capacity (number of pages)
/// Turso uses 100,000 pages (~400MB for 4KB pages)
pub const DEFAULT_CACHE_CAPACITY: usize = 100_000;

/// Minimum cache capacity
pub const MIN_CACHE_CAPACITY: usize = 2;

/// Lock-protected cache entry.
///
/// Per ADR 0033 (`page-cache-lock-free-hit`), the SIEVE `visited` bit
/// no longer lives here — it moved to [`ShardMeta`], a separate
/// cache-line-packed atomic array, so a cache hit can flip it without
/// upgrading the per-shard write lock. `pin_count` and `dirty` stay in
/// the entry behind the `entries` `RwLock`: they are written only by
/// structural operations (`pin`/`unpin`/`mark_dirty`/`mark_clean`,
/// which take the write lock) and are *never* touched by the hit path.
struct CacheEntry {
    /// The cached page
    page: Page,
    /// Pin count (page cannot be evicted while pinned)
    pin_count: usize,
    /// Whether the page is dirty (modified)
    dirty: bool,
}

impl CacheEntry {
    fn new(page: Page) -> Self {
        Self {
            page,
            pin_count: 0,
            dirty: false,
        }
    }
}

/// Cache line size in bytes (x86-64 / aarch64). The SIEVE `visited`
/// bits and per-slot `tag`s are laid out in their own line-aligned,
/// densely packed arrays (see [`ShardMeta`]) so the lock-free hit path
/// — which only ever touches `visited`/`tag` — never shares a cache
/// line with the lock-protected [`CacheEntry`] payload (page bytes,
/// `pin_count`, `dirty`). ADR 0033.
const CACHE_LINE: usize = 64;

/// Sentinel `tag` value for an unoccupied slot. `u32::MAX` is reserved
/// (no real page id uses it) so the hit path can tell, with a single
/// relaxed load, whether the slot it resolved still belongs to the
/// page it is marking.
const TAG_EMPTY: u32 = u32::MAX;

/// One cache line of packed `visited` bits: one [`AtomicBool`] (1 byte)
/// per slot, 64 slots per line. `#[repr(align(64))]` forces the boxed
/// backing buffer onto a line boundary, so the array starts aligned and
/// no two unrelated lines ever false-share.
#[repr(align(64))]
struct VisitedLine([AtomicBool; CACHE_LINE]);

/// Number of `AtomicU32` tags that fit in one cache line.
const TAGS_PER_LINE: usize = CACHE_LINE / 4;

/// One cache line of packed slot tags: one [`AtomicU32`] (4 bytes) per
/// slot, 16 slots per line. Line-aligned for the same reason as
/// [`VisitedLine`].
#[repr(align(64))]
struct TagLine([AtomicU32; TAGS_PER_LINE]);

/// Cache-line-packed SIEVE metadata, split out of [`CacheEntry`].
///
/// Both arrays are indexed by slot and are *separate, line-aligned*
/// allocations (ADR 0033 acceptance: "visited/tag metadata is laid out
/// in separate cache-line-aligned arrays"). Because the elements are
/// atomics, the hit path mutates `visited` through a shared `&self`
/// with no lock at all — satisfying the contract's "cache hits set
/// visited without acquiring the per-shard write lock".
///
/// The arrays are sized to the shard `capacity`. In normal operation a
/// slot index never reaches `capacity` (`insert` evicts before growing
/// `entries`), so every access lands in range. The one pathological
/// exception — a full cache whose every slot is pinned, where `insert`
/// grows `entries` past `capacity` — is handled by the bounds guards
/// below: an out-of-range slot reports "not visited" / `TAG_EMPTY`,
/// which makes that emergency over-capacity entry the first eviction
/// candidate once a slot frees. No panic, no unbounded metadata.
struct ShardMeta {
    visited: Box<[VisitedLine]>,
    tag: Box<[TagLine]>,
    capacity: usize,
}

impl ShardMeta {
    fn new(capacity: usize) -> Self {
        let visited = (0..capacity.div_ceil(CACHE_LINE))
            .map(|_| VisitedLine(std::array::from_fn(|_| AtomicBool::new(false))))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let tag = (0..capacity.div_ceil(TAGS_PER_LINE))
            .map(|_| TagLine(std::array::from_fn(|_| AtomicU32::new(TAG_EMPTY))))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            visited,
            tag,
            capacity,
        }
    }

    /// Lock-free SIEVE hit mark. Benign-race by contract (ADR 0033): a
    /// concurrent eviction clearing this same bit only costs one extra
    /// SIEVE cycle. `Relaxed` is sufficient — the bit carries no
    /// happens-before obligation toward any other state.
    #[inline]
    fn mark_visited(&self, slot: usize) {
        if let Some(b) = self.visited_bit(slot) {
            b.store(true, Ordering::Relaxed);
        }
    }

    #[inline]
    fn is_visited(&self, slot: usize) -> bool {
        self.visited_bit(slot)
            .map(|b| b.load(Ordering::Relaxed))
            .unwrap_or(false)
    }

    /// Clear the visited bit. Structural (called from the eviction hand
    /// under the per-shard locks), but the store itself is a relaxed
    /// atomic so it composes with the lock-free hit mark.
    #[inline]
    fn clear_visited(&self, slot: usize) {
        if let Some(b) = self.visited_bit(slot) {
            b.store(false, Ordering::Relaxed);
        }
    }

    /// The page id currently tagged at `slot`, or [`TAG_EMPTY`].
    #[inline]
    fn tag(&self, slot: usize) -> u32 {
        self.tag_slot(slot)
            .map(|t| t.load(Ordering::Relaxed))
            .unwrap_or(TAG_EMPTY)
    }

    /// Bind a slot to a page and reset its visited bit. Structural:
    /// the caller holds the per-shard write lock that serialises slot
    /// ownership.
    #[inline]
    fn occupy(&self, slot: usize, page_id: u32) {
        if let Some(t) = self.tag_slot(slot) {
            t.store(page_id, Ordering::Relaxed);
        }
        self.clear_visited(slot);
    }

    /// Release a slot. Structural; caller holds the write lock.
    #[inline]
    fn vacate(&self, slot: usize) {
        if let Some(t) = self.tag_slot(slot) {
            t.store(TAG_EMPTY, Ordering::Relaxed);
        }
        self.clear_visited(slot);
    }

    /// Reset all metadata to the empty state (used by `clear`).
    fn reset(&self) {
        for line in self.visited.iter() {
            for b in line.0.iter() {
                b.store(false, Ordering::Relaxed);
            }
        }
        for line in self.tag.iter() {
            for t in line.0.iter() {
                t.store(TAG_EMPTY, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    fn visited_bit(&self, slot: usize) -> Option<&AtomicBool> {
        if slot >= self.capacity {
            return None;
        }
        self.visited
            .get(slot / CACHE_LINE)
            .map(|line| &line.0[slot % CACHE_LINE])
    }

    #[inline]
    fn tag_slot(&self, slot: usize) -> Option<&AtomicU32> {
        if slot >= self.capacity {
            return None;
        }
        self.tag
            .get(slot / TAGS_PER_LINE)
            .map(|line| &line.0[slot % TAGS_PER_LINE])
    }
}

/// Cache statistics
#[derive(Debug, Default, Clone)]
pub struct CacheStats {
    /// Number of cache hits
    pub hits: u64,
    /// Number of cache misses
    pub misses: u64,
    /// Number of evictions
    pub evictions: u64,
    /// Number of dirty page writebacks
    pub writebacks: u64,
}

impl CacheStats {
    /// Calculate hit rate (0.0 to 1.0)
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// SIEVE-based page cache
///
/// Thread-safe page cache using the SIEVE eviction algorithm.
pub struct PageCacheShard {
    /// Maximum number of pages to cache
    capacity: usize,
    /// Page ID -> Entry index mapping
    index: RwLock<HashMap<u32, usize>>,
    /// FIFO queue of page IDs for eviction order
    fifo: Mutex<VecDeque<u32>>,
    /// Cache entries (indexed by slot)
    entries: RwLock<Vec<Option<CacheEntry>>>,
    /// Cache-line-packed SIEVE metadata (`visited` bits + slot `tag`s),
    /// kept out of `entries` so the hit path can mark `visited` with a
    /// lock-free relaxed atomic store. ADR 0033.
    meta: ShardMeta,
    /// Free slots
    free_slots: Mutex<Vec<usize>>,
    /// SIEVE eviction hand position
    hand: Mutex<usize>,
    /// Cache statistics
    stats: Mutex<CacheStats>,
}

impl PageCacheShard {
    /// Create a new page cache with specified capacity
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(MIN_CACHE_CAPACITY);

        Self {
            capacity,
            index: RwLock::new(HashMap::with_capacity(capacity)),
            fifo: Mutex::new(VecDeque::with_capacity(capacity)),
            entries: RwLock::new(Vec::with_capacity(capacity)),
            meta: ShardMeta::new(capacity),
            free_slots: Mutex::new(Vec::new()),
            hand: Mutex::new(0),
            stats: Mutex::new(CacheStats::default()),
        }
    }

    /// Create a page cache with default capacity
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CACHE_CAPACITY)
    }

    /// Get the current number of cached pages
    pub fn len(&self) -> usize {
        cache_read(&self.index).len()
    }

    /// Check if cache is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get cache capacity
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        cache_lock(&self.stats).clone()
    }

    /// Reset statistics
    pub fn reset_stats(&self) {
        *cache_lock(&self.stats) = CacheStats::default();
    }

    /// Get a page from cache
    ///
    /// Returns None if page is not in cache (cache miss).
    /// On hit, marks the page as visited (SIEVE).
    pub fn get(&self, page_id: u32) -> Option<Page> {
        // Check if page is in cache
        let index = cache_read(&self.index);
        let slot = match index.get(&page_id) {
            Some(&s) => s,
            None => {
                drop(index);
                // Cache miss
                cache_lock(&self.stats).misses += 1;
                return None;
            }
        };
        drop(index);

        // Lock-free hit path (ADR 0033). Clone the page under a *read*
        // lock — never a write lock — then set the SIEVE `visited` bit
        // with a single relaxed atomic store on the separate
        // cache-line-packed metadata array. The hit path acquires no
        // write lock and never touches `pin_count`/`dirty` (those live
        // in `CacheEntry` behind the write lock and are mutated only by
        // structural operations). Dropping the per-hit write-lock
        // upgrade is the whole point: hot-loop reads of an
        // already-cached page (e.g. BTree inserts hammering the
        // rightmost leaf) no longer serialise on the entries writer.
        let entries = cache_read(&self.entries);
        if let Some(entry) = entries.get(slot).and_then(|e| e.as_ref()) {
            let page = entry.page.clone();
            drop(entries);

            // Mark visited lock-free. Confirm the slot still tags this
            // page id first: if a racing eviction repurposed the slot
            // we simply skip the mark (benign — at worst one extra
            // SIEVE cycle, never a wrong-page write). The tag check
            // never changes hit/miss accounting; the entry existed, so
            // this is a hit.
            if self.meta.tag(slot) == page_id {
                self.meta.mark_visited(slot);
            }

            cache_lock(&self.stats).hits += 1;

            Some(page)
        } else {
            cache_lock(&self.stats).misses += 1;
            None
        }
    }

    /// Insert a page into cache
    ///
    /// May trigger eviction if cache is full.
    /// Returns the evicted page (if dirty) that needs to be written back.
    pub fn insert(&self, page_id: u32, page: Page) -> Option<Page> {
        // Check if already in cache
        {
            let index = cache_read(&self.index);
            if let Some(&slot) = index.get(&page_id) {
                drop(index);

                // Update existing entry under the write lock; the
                // SIEVE visited bit lives in the separate metadata
                // array and is set lock-free (matches the prior
                // behaviour of marking a re-inserted page visited).
                let mut entries = cache_write(&self.entries);
                if let Some(Some(entry)) = entries.get_mut(slot) {
                    entry.page = page;
                }
                drop(entries);
                self.meta.mark_visited(slot);
                return None;
            }
        }

        // Need to insert new entry
        let mut evicted = None;

        // Check if we need to evict before getting free_slots lock
        let current_len = self.len();
        if current_len >= self.capacity {
            // Need to evict first (no locks held)
            evicted = self.evict();
        }

        // Find or create a slot
        let slot = {
            let mut free_slots = cache_lock(&self.free_slots);
            if let Some(slot) = free_slots.pop() {
                slot
            } else {
                drop(free_slots);
                // Add a new slot
                let mut entries = cache_write(&self.entries);
                let slot = entries.len();
                entries.push(None);
                slot
            }
        };

        // Insert entry
        {
            let mut entries = cache_write(&self.entries);

            // Ensure slot exists
            while entries.len() <= slot {
                entries.push(None);
            }

            entries[slot] = Some(CacheEntry::new(page));
        }

        // Update index
        {
            let mut index = cache_write(&self.index);
            index.insert(page_id, slot);
        }

        // Bind the metadata slot to this page (tag = page_id, visited
        // reset). Structural: ordered after the entry/index writes that
        // the per-shard write lock serialises.
        self.meta.occupy(slot, page_id);

        // Add to FIFO
        {
            let mut fifo = cache_lock(&self.fifo);
            fifo.push_back(page_id);
        }

        evicted
    }

    /// Evict a page using SIEVE algorithm
    ///
    /// Returns the evicted page if it was dirty.
    fn evict(&self) -> Option<Page> {
        let mut fifo = cache_lock(&self.fifo);
        let mut hand = cache_lock(&self.hand);

        if fifo.is_empty() {
            return None;
        }

        let fifo_len = fifo.len();
        let mut attempts = 0;

        // Sweep from hand position
        loop {
            if attempts >= fifo_len * 2 {
                // Couldn't find anything to evict (all pinned?)
                return None;
            }

            // Wrap around
            if *hand >= fifo_len {
                *hand = 0;
            }

            let page_id = fifo[*hand];
            attempts += 1;

            // Get entry slot
            let slot = {
                let index = cache_read(&self.index);
                match index.get(&page_id) {
                    Some(&s) => s,
                    None => {
                        // Page was removed, skip
                        *hand += 1;
                        continue;
                    }
                }
            };

            // Check entry. `pin_count`/`dirty` are read under the
            // entries lock; the SIEVE `visited` bit is read from the
            // separate atomic metadata array.
            let (should_evict, dirty) = {
                let entries = cache_read(&self.entries);
                match entries.get(slot).and_then(|e| e.as_ref()) {
                    Some(entry) => {
                        if entry.pin_count > 0 {
                            // Pinned, can't evict
                            (false, false)
                        } else if self.meta.is_visited(slot) {
                            // Clear visited bit, skip (SIEVE)
                            (false, false)
                        } else {
                            // Can evict
                            (true, entry.dirty)
                        }
                    }
                    None => {
                        *hand += 1;
                        continue;
                    }
                }
            };

            if !should_evict {
                // Clear visited bit lock-free (no entries write lock
                // needed — `visited` is no longer part of `CacheEntry`).
                self.meta.clear_visited(slot);
                *hand += 1;
                continue;
            }

            // Evict this entry. Contract guard (ADR 0033): the eviction
            // hand must never select a pinned page. We re-check under
            // the write lock — the only place `pin_count` is read for
            // the eviction decision and the place it must hold.
            let evicted_page = {
                let mut entries = cache_write(&self.entries);
                debug_assert!(
                    entries
                        .get(slot)
                        .and_then(|e| e.as_ref())
                        .map(|e| e.pin_count == 0)
                        .unwrap_or(true),
                    "ADR 0033 violation: eviction selected a pinned page (slot {slot})"
                );
                let entry = entries[slot].take();
                entry.map(|e| e.page)
            };

            // Release the metadata slot (tag -> empty, visited -> false).
            self.meta.vacate(slot);

            // Remove from index
            {
                let mut index = cache_write(&self.index);
                index.remove(&page_id);
            }

            // Remove from FIFO
            fifo.remove(*hand);

            // Add slot to free list
            {
                let mut free_slots = cache_lock(&self.free_slots);
                free_slots.push(slot);
            }

            // Update stats
            {
                let mut stats = cache_lock(&self.stats);
                stats.evictions += 1;
                if dirty {
                    stats.writebacks += 1;
                }
            }

            // Return evicted page if dirty
            if dirty {
                return evicted_page;
            } else {
                return None;
            }
        }
    }

    /// Mark a page as dirty
    pub fn mark_dirty(&self, page_id: u32) {
        let index = cache_read(&self.index);
        if let Some(&slot) = index.get(&page_id) {
            drop(index);

            let mut entries = cache_write(&self.entries);
            if let Some(Some(entry)) = entries.get_mut(slot) {
                entry.dirty = true;
            }
        }
    }

    /// Mark a page as clean
    pub fn mark_clean(&self, page_id: u32) {
        let index = cache_read(&self.index);
        if let Some(&slot) = index.get(&page_id) {
            drop(index);

            let mut entries = cache_write(&self.entries);
            if let Some(Some(entry)) = entries.get_mut(slot) {
                entry.dirty = false;
            }
        }
    }

    /// Pin a page (prevent eviction)
    pub fn pin(&self, page_id: u32) -> bool {
        let index = cache_read(&self.index);
        if let Some(&slot) = index.get(&page_id) {
            drop(index);

            let mut entries = cache_write(&self.entries);
            if let Some(Some(entry)) = entries.get_mut(slot) {
                entry.pin_count += 1;
                return true;
            }
        }
        false
    }

    /// Unpin a page
    pub fn unpin(&self, page_id: u32) -> bool {
        let index = cache_read(&self.index);
        if let Some(&slot) = index.get(&page_id) {
            drop(index);

            let mut entries = cache_write(&self.entries);
            if let Some(Some(entry)) = entries.get_mut(slot) {
                if entry.pin_count > 0 {
                    entry.pin_count -= 1;
                    return true;
                }
            }
        }
        false
    }

    /// Remove a page from cache
    pub fn remove(&self, page_id: u32) -> Option<Page> {
        // Get and remove from index
        let slot = {
            let mut index = cache_write(&self.index);
            index.remove(&page_id)?
        };

        // Remove entry
        let entry = {
            let mut entries = cache_write(&self.entries);
            entries.get_mut(slot).and_then(|e| e.take())
        };

        // Release the metadata slot.
        self.meta.vacate(slot);

        // Remove from FIFO
        {
            let mut fifo = cache_lock(&self.fifo);
            fifo.retain(|&id| id != page_id);
        }

        // Add slot to free list
        {
            let mut free_slots = cache_lock(&self.free_slots);
            free_slots.push(slot);
        }

        entry.map(|e| e.page)
    }

    /// Flush all dirty pages
    ///
    /// Returns an iterator of (page_id, page) for dirty pages.
    pub fn flush_dirty(&self) -> Vec<(u32, Page)> {
        let mut dirty_pages = Vec::new();

        let index = cache_read(&self.index);
        let entries = cache_read(&self.entries);

        for (&page_id, &slot) in index.iter() {
            if let Some(Some(entry)) = entries.get(slot) {
                if entry.dirty {
                    dirty_pages.push((page_id, entry.page.clone()));
                }
            }
        }

        drop(entries);
        drop(index);

        // Mark all as clean
        for (page_id, _) in &dirty_pages {
            self.mark_clean(*page_id);
        }

        let count = dirty_pages.len();
        cache_lock(&self.stats).writebacks += count as u64;

        dirty_pages
    }

    /// Bounded counterpart of `flush_dirty` used by the background
    /// writer. Snapshots up to `max` dirty pages, marks them clean,
    /// and returns the (page_id, page) pairs for the caller to
    /// persist via the pager. Clamps to the current dirty set —
    /// returning fewer than `max` simply means we're caught up.
    pub fn flush_some_dirty(&self, max: usize) -> Vec<(u32, Page)> {
        if max == 0 {
            return Vec::new();
        }
        let mut dirty_pages = Vec::with_capacity(max);

        let index = cache_read(&self.index);
        let entries = cache_read(&self.entries);

        for (&page_id, &slot) in index.iter() {
            if dirty_pages.len() >= max {
                break;
            }
            if let Some(Some(entry)) = entries.get(slot) {
                if entry.dirty {
                    dirty_pages.push((page_id, entry.page.clone()));
                }
            }
        }

        drop(entries);
        drop(index);

        for (page_id, _) in &dirty_pages {
            self.mark_clean(*page_id);
        }

        let count = dirty_pages.len();
        cache_lock(&self.stats).writebacks += count as u64;

        dirty_pages
    }

    /// Count dirty pages currently in the cache. Used by the
    /// background writer to compute an adaptive flush budget.
    pub fn dirty_count(&self) -> usize {
        let index = cache_read(&self.index);
        let entries = cache_read(&self.entries);
        let mut count = 0;
        for (_, &slot) in index.iter() {
            if let Some(Some(entry)) = entries.get(slot) {
                if entry.dirty {
                    count += 1;
                }
            }
        }
        count
    }

    /// Clear all entries from cache
    pub fn clear(&self) {
        let mut index = cache_write(&self.index);
        let mut entries = cache_write(&self.entries);
        let mut fifo = cache_lock(&self.fifo);
        let mut free_slots = cache_lock(&self.free_slots);

        index.clear();
        entries.clear();
        fifo.clear();
        free_slots.clear();
        self.meta.reset();
        *cache_lock(&self.hand) = 0;
    }

    /// Check if a page is in cache
    pub fn contains(&self, page_id: u32) -> bool {
        cache_read(&self.index).contains_key(&page_id)
    }

    /// Get all cached page IDs
    pub fn page_ids(&self) -> Vec<u32> {
        cache_read(&self.index).keys().copied().collect()
    }
}

impl Default for PageCacheShard {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

/// Number of independent `PageCacheShard`s inside a `PageCache`. Must
/// be a power of two so `page_id & (NUM_SHARDS - 1)` is a valid shard
/// index. 8 was picked to cover typical bench concurrency (16 workers)
/// without making each shard's own SIEVE queue too small; bumping to
/// 16 is safe if profiling ever shows shard-level contention.
const NUM_SHARDS: usize = 8;

/// Sharded page cache.
///
/// Routes each `page_id` to one of `NUM_SHARDS` independent
/// [`PageCacheShard`]s by the low bits of the id. Readers and writers
/// for disjoint pages hit different shards and do not contend on the
/// shard-internal RwLocks/Mutexes. Each shard runs its own SIEVE
/// eviction loop over `capacity / NUM_SHARDS` slots; hot/cold
/// asymmetry across shards is accepted in exchange for the
/// contention win.
pub struct PageCache {
    shards: Box<[PageCacheShard]>,
    capacity: usize,
}

impl PageCache {
    pub fn new(capacity: usize) -> Self {
        // Keep every shard above MIN_CACHE_CAPACITY so the inner
        // SIEVE invariants stay valid even when callers pass tiny
        // totals (tests frequently do).
        let per_shard = capacity.div_ceil(NUM_SHARDS).max(MIN_CACHE_CAPACITY);
        let total = per_shard * NUM_SHARDS;
        let shards: Vec<PageCacheShard> = (0..NUM_SHARDS)
            .map(|_| PageCacheShard::new(per_shard))
            .collect();
        Self {
            shards: shards.into_boxed_slice(),
            capacity: total,
        }
    }

    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CACHE_CAPACITY)
    }

    #[inline]
    fn shard_for(&self, page_id: u32) -> &PageCacheShard {
        &self.shards[(page_id as usize) & (NUM_SHARDS - 1)]
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.shards.iter().all(|s| s.is_empty())
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn stats(&self) -> CacheStats {
        let mut agg = CacheStats::default();
        for s in self.shards.iter() {
            let cs = s.stats();
            agg.hits += cs.hits;
            agg.misses += cs.misses;
            agg.evictions += cs.evictions;
            agg.writebacks += cs.writebacks;
        }
        agg
    }

    pub fn reset_stats(&self) {
        for s in self.shards.iter() {
            s.reset_stats();
        }
    }

    pub fn get(&self, page_id: u32) -> Option<Page> {
        self.shard_for(page_id).get(page_id)
    }

    pub fn insert(&self, page_id: u32, page: Page) -> Option<Page> {
        self.shard_for(page_id).insert(page_id, page)
    }

    pub fn mark_dirty(&self, page_id: u32) {
        self.shard_for(page_id).mark_dirty(page_id)
    }

    pub fn mark_clean(&self, page_id: u32) {
        self.shard_for(page_id).mark_clean(page_id)
    }

    pub fn pin(&self, page_id: u32) -> bool {
        self.shard_for(page_id).pin(page_id)
    }

    pub fn unpin(&self, page_id: u32) -> bool {
        self.shard_for(page_id).unpin(page_id)
    }

    pub fn remove(&self, page_id: u32) -> Option<Page> {
        self.shard_for(page_id).remove(page_id)
    }

    pub fn contains(&self, page_id: u32) -> bool {
        self.shard_for(page_id).contains(page_id)
    }

    pub fn flush_dirty(&self) -> Vec<(u32, Page)> {
        let mut out = Vec::new();
        for s in self.shards.iter() {
            out.extend(s.flush_dirty());
        }
        out
    }

    pub fn flush_some_dirty(&self, max: usize) -> Vec<(u32, Page)> {
        if max == 0 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(max);
        for s in self.shards.iter() {
            if out.len() >= max {
                break;
            }
            let budget = max - out.len();
            out.extend(s.flush_some_dirty(budget));
        }
        out
    }

    pub fn dirty_count(&self) -> usize {
        self.shards.iter().map(|s| s.dirty_count()).sum()
    }

    pub fn clear(&self) {
        for s in self.shards.iter() {
            s.clear();
        }
    }

    pub fn page_ids(&self) -> Vec<u32> {
        let mut out = Vec::with_capacity(self.len());
        for s in self.shards.iter() {
            out.extend(s.page_ids());
        }
        out
    }
}

impl Default for PageCache {
    fn default() -> Self {
        Self::with_default_capacity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::page::PageType;

    fn make_page(id: u32) -> Page {
        Page::new(PageType::BTreeLeaf, id)
    }

    #[test]
    fn test_cache_basic() {
        let cache = PageCacheShard::new(100);

        assert!(cache.is_empty());
        assert_eq!(cache.capacity(), 100);

        // Insert
        cache.insert(1, make_page(1));
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(1));

        // Get
        let page = cache.get(1);
        assert!(page.is_some());

        // Miss
        let page = cache.get(999);
        assert!(page.is_none());
    }

    #[test]
    fn test_cache_eviction() {
        let cache = PageCacheShard::new(4);

        // Fill cache
        for i in 0..4 {
            cache.insert(i, make_page(i));
        }

        assert_eq!(cache.len(), 4);

        // Insert one more, should trigger eviction
        cache.insert(100, make_page(100));

        // One of the original pages should be evicted
        assert_eq!(cache.len(), 4);
        assert!(cache.contains(100));
    }

    #[test]
    fn test_cache_sieve_visited() {
        let cache = PageCacheShard::new(4);

        // Fill cache
        for i in 0..4 {
            cache.insert(i, make_page(i));
        }

        // Access page 0 (marks as visited)
        cache.get(0);

        // Insert new page, should evict non-visited first
        cache.insert(100, make_page(100));

        // Page 0 should still be there (was visited)
        assert!(cache.contains(0));
        assert!(cache.contains(100));
    }

    #[test]
    fn test_cache_dirty() {
        let cache = PageCacheShard::new(100);

        cache.insert(1, make_page(1));
        cache.mark_dirty(1);

        let dirty = cache.flush_dirty();
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].0, 1);

        // Should be clean now
        let dirty = cache.flush_dirty();
        assert_eq!(dirty.len(), 0);
    }

    #[test]
    fn test_cache_pin() {
        let cache = PageCacheShard::new(2);

        cache.insert(1, make_page(1));
        cache.insert(2, make_page(2));

        // Pin page 1
        assert!(cache.pin(1));

        // Fill cache to trigger eviction
        cache.insert(3, make_page(3));

        // Page 1 should still be there (pinned)
        assert!(cache.contains(1));

        // Unpin
        assert!(cache.unpin(1));
    }

    #[test]
    fn test_cache_remove() {
        let cache = PageCacheShard::new(100);

        cache.insert(1, make_page(1));
        assert!(cache.contains(1));

        let removed = cache.remove(1);
        assert!(removed.is_some());
        assert!(!cache.contains(1));
    }

    #[test]
    fn test_cache_stats() {
        let cache = PageCacheShard::new(100);

        cache.insert(1, make_page(1));

        // Hit
        cache.get(1);
        cache.get(1);

        // Miss
        cache.get(999);

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
        assert!((stats.hit_rate() - 0.666).abs() < 0.01);
    }

    #[test]
    fn test_cache_clear() {
        let cache = PageCacheShard::new(100);

        for i in 0..50 {
            cache.insert(i, make_page(i));
        }

        assert_eq!(cache.len(), 50);

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_update_existing() {
        let cache = PageCacheShard::new(100);

        let mut page1 = make_page(1);
        page1.as_bytes_mut()[100] = 0xAA;
        cache.insert(1, page1);

        let mut page1_updated = make_page(1);
        page1_updated.as_bytes_mut()[100] = 0xBB;
        cache.insert(1, page1_updated);

        // Should still only have one entry
        assert_eq!(cache.len(), 1);

        // Should have updated value
        let retrieved = cache.get(1).unwrap();
        assert_eq!(retrieved.as_bytes()[100], 0xBB);
    }

    #[test]
    fn test_cache_recovers_after_index_lock_poisoning() {
        let cache = std::sync::Arc::new(PageCacheShard::new(8));
        let poison_target = std::sync::Arc::clone(&cache);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target
                .index
                .write()
                .expect("index lock should be acquired");
            panic!("poison index lock");
        })
        .join();

        cache.insert(1, make_page(1));
        assert!(cache.contains(1));
        assert!(cache.get(1).is_some());
    }

    #[test]
    fn test_cache_recovers_after_stats_lock_poisoning() {
        let cache = std::sync::Arc::new(PageCacheShard::new(8));
        let poison_target = std::sync::Arc::clone(&cache);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target
                .stats
                .lock()
                .expect("stats lock should be acquired");
            panic!("poison stats lock");
        })
        .join();

        assert!(cache.get(999).is_none());
        assert_eq!(cache.stats().misses, 1);
        cache.reset_stats();
        assert_eq!(cache.stats().misses, 0);
    }

    /// ADR 0033: the SIEVE `visited`/`tag` metadata lives in its own
    /// cache-line-packed, line-aligned arrays — separate from the
    /// lock-protected `CacheEntry`. Assert the line types are exactly
    /// one 64-byte line and 64-byte aligned (so the boxed backing
    /// buffers start on a line boundary and never false-share), and
    /// that `CacheEntry` no longer carries the visited bit.
    #[test]
    fn test_metadata_layout_is_cache_line_packed() {
        assert_eq!(std::mem::align_of::<VisitedLine>(), CACHE_LINE);
        assert_eq!(std::mem::size_of::<VisitedLine>(), CACHE_LINE);
        assert_eq!(std::mem::align_of::<TagLine>(), CACHE_LINE);
        assert_eq!(std::mem::size_of::<TagLine>(), CACHE_LINE);

        // The metadata arrays are distinct allocations from `entries`.
        let meta = ShardMeta::new(200);
        // Densely packed: 64 visited bits per line, 16 tags per line.
        assert_eq!(meta.visited.len(), 200usize.div_ceil(64));
        assert_eq!(meta.tag.len(), 200usize.div_ceil(16));

        // occupy/vacate round-trip through the tag array.
        meta.occupy(5, 42);
        assert_eq!(meta.tag(5), 42);
        assert!(!meta.is_visited(5));
        meta.mark_visited(5);
        assert!(meta.is_visited(5));
        meta.vacate(5);
        assert_eq!(meta.tag(5), TAG_EMPTY);
        assert!(!meta.is_visited(5));
    }

    /// ADR 0033: a cache hit sets the SIEVE `visited` bit on the
    /// separate metadata array (no write-lock upgrade), and that bit
    /// drives eviction exactly as before — a hit page survives one
    /// eviction sweep while an un-hit page is evicted first.
    #[test]
    fn test_hit_sets_visited_and_preserves_sieve_policy() {
        let cache = PageCacheShard::new(4);
        for i in 0..4 {
            cache.insert(i, make_page(i));
        }

        // Hit pages 0 and 1 (lock-free visited mark via metadata array).
        assert!(cache.get(0).is_some());
        assert!(cache.get(1).is_some());

        // Insert two more: SIEVE evicts the un-hit 2 and 3 first.
        cache.insert(10, make_page(10));
        cache.insert(11, make_page(11));

        assert!(cache.contains(0), "hit page 0 must survive");
        assert!(cache.contains(1), "hit page 1 must survive");
        assert!(cache.contains(10));
        assert!(cache.contains(11));
        assert!(!cache.contains(2));
        assert!(!cache.contains(3));
    }

    /// ADR 0033 acceptance: under concurrent hits + eviction, no pinned
    /// page is ever evicted, and dirty pages are never silently lost —
    /// every dirty page that leaves the cache is returned for
    /// writeback. Stress/loom-style: many threads hammer hits while
    /// other threads drive insertions (and thus the eviction hand).
    #[test]
    fn test_concurrent_hits_never_evict_pinned_or_lose_dirty() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;

        const CAP: u32 = 64;
        const PINNED: u32 = 8; // pages 0..8 stay pinned for the whole run
        const DIRTY: u32 = 8; // pages 8..16 are dirty
        const HITTERS: usize = 6;
        const INSERTERS: usize = 4;
        const ROUNDS: usize = 4_000;

        let cache = Arc::new(PageCacheShard::new(CAP as usize));
        for i in 0..CAP {
            cache.insert(i, make_page(i));
        }
        for p in 0..PINNED {
            assert!(cache.pin(p), "pin {p}");
        }
        for p in PINNED..PINNED + DIRTY {
            cache.mark_dirty(p);
        }

        // Count dirty pages handed back by insert() so we can prove no
        // dirty data is silently dropped.
        let dirty_written_back = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        // Hitters: loop on the lock-free visited path until signalled.
        let mut hitters = Vec::with_capacity(HITTERS);
        for h in 0..HITTERS {
            let cache = Arc::clone(&cache);
            let stop = Arc::clone(&stop);
            hitters.push(std::thread::spawn(move || {
                let mut id = (h as u32) % CAP;
                while !stop.load(Ordering::Relaxed) {
                    let _ = cache.get(id);
                    id = (id + 1) % CAP;
                }
            }));
        }

        // Inserters: bounded churn that drives the eviction hand.
        let mut inserters = Vec::with_capacity(INSERTERS);
        for w in 0..INSERTERS {
            let cache = Arc::clone(&cache);
            let dirty_written_back = Arc::clone(&dirty_written_back);
            inserters.push(std::thread::spawn(move || {
                let base = 1_000 + (w as u32) * 1_000_000;
                for i in 0..ROUNDS {
                    let id = base + i as u32;
                    if cache.insert(id, make_page(id)).is_some() {
                        // insert only returns Some for a dirty victim.
                        dirty_written_back.fetch_add(1, Ordering::Relaxed);
                    }
                    // Invariant checked mid-storm: pinned pages are
                    // never evicted.
                    for p in 0..PINNED {
                        assert!(
                            cache.contains(p),
                            "pinned page {p} was evicted under concurrency"
                        );
                    }
                }
            }));
        }

        // Inserters finish their bounded work; then stop the hitters.
        for h in inserters {
            h.join()
                .expect("inserter thread panicked (invariant broke)");
        }
        stop.store(true, Ordering::Relaxed);
        for h in hitters {
            h.join().expect("hitter thread panicked (invariant broke)");
        }

        // Pinned pages survived the entire storm.
        for p in 0..PINNED {
            assert!(cache.contains(p), "pinned page {p} missing after storm");
        }

        // Dirty accounting: a dirty page still in the cache, plus any
        // dirty victims handed back for writeback, must conserve the
        // original dirty set — no dirty page silently vanished.
        let dirty_resident = (PINNED..PINNED + DIRTY)
            .filter(|&p| cache.contains(p))
            .count();
        assert!(
            dirty_resident + dirty_written_back.load(Ordering::Relaxed) >= DIRTY as usize,
            "dirty pages lost: resident={dirty_resident} written_back={}",
            dirty_written_back.load(Ordering::Relaxed)
        );

        // Note: we deliberately do NOT assert `len <= CAP`. The
        // pre-existing fine-grained-lock `insert` checks length and
        // evicts in separate steps, so concurrent inserters can
        // transiently push a shard past capacity. ShardMeta's bounds
        // guards make that overflow safe (over-capacity slots report
        // "not visited" / TAG_EMPTY and drain first); enforcing a hard
        // cap is out of scope for this lock-free-hit change.
    }

    /// Legacy single-lock baseline used as a regression check for the
    /// sharded `PageCache`. Mirrors the pre-shard cache shape: a single
    /// `RwLock<HashMap<u32, Page>>` so every mutation serializes on one
    /// writer. We only need the surface used by the concurrency test
    /// (`insert` / `get`); keeping it minimal avoids accidentally
    /// drifting toward the sharded design and silently flattening the
    /// regression signal.
    mod legacy_baseline {
        use super::Page;
        use std::collections::HashMap;
        use std::sync::RwLock;

        pub struct LegacyPageCache {
            entries: RwLock<HashMap<u32, Page>>,
        }

        impl LegacyPageCache {
            pub fn new(_capacity: usize) -> Self {
                Self {
                    entries: RwLock::new(HashMap::new()),
                }
            }

            pub fn insert(&self, page_id: u32, page: Page) {
                let mut entries = self.entries.write().unwrap();
                entries.insert(page_id, page);
            }

            pub fn get(&self, page_id: u32) -> Option<Page> {
                let entries = self.entries.read().unwrap();
                entries.get(&page_id).cloned()
            }
        }
    }

    /// Workload shared by both the sharded and legacy runs of the
    /// concurrency property test below. Each worker churns through its
    /// own disjoint `page_id` range so any contention seen between
    /// workers is purely lock-induced, not data-induced.
    fn run_workload<F>(workers: usize, ops_per_worker: usize, run: F) -> std::time::Duration
    where
        F: Fn(u32, &Page) + Send + Sync + 'static + Clone,
    {
        use std::sync::Arc;
        use std::time::Instant;

        let run = Arc::new(run);
        let start = Instant::now();
        let mut handles = Vec::with_capacity(workers);
        for w in 0..workers {
            let run = Arc::clone(&run);
            handles.push(std::thread::spawn(move || {
                let base = (w as u32) * 1_000_000;
                let page = make_page(0);
                for i in 0..ops_per_worker {
                    let id = base + (i as u32);
                    run(id, &page);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        start.elapsed()
    }

    #[test]
    fn test_sharded_cache_scales_concurrently() {
        // Property: with disjoint page_id ranges, 10 workers should
        // beat 1 worker by a meaningful factor on the sharded cache,
        // and the sharded cache should beat the legacy single-lock
        // baseline at 10 workers (since the baseline serializes every
        // mutation through one global RwLock).
        //
        // We assert "sub-linear scaling" in a deliberately loose form
        // (parallel < 7x serial) so CI flakiness on busy runners
        // doesn't false-positive. The legacy comparison is the
        // stronger regression signal.
        use std::sync::Arc;

        const WORKERS: usize = 10;
        const OPS: usize = 5_000;
        const CAPACITY: usize = 200_000;

        // Sharded: 1 worker baseline.
        let sharded = Arc::new(PageCache::new(CAPACITY));
        let s1 = Arc::clone(&sharded);
        let sharded_serial = run_workload(1, OPS * WORKERS, move |id, page| {
            s1.insert(id, page.clone());
            let _ = s1.get(id);
        });

        // Sharded: WORKERS parallel.
        let sharded = Arc::new(PageCache::new(CAPACITY));
        let s2 = Arc::clone(&sharded);
        let sharded_parallel = run_workload(WORKERS, OPS, move |id, page| {
            s2.insert(id, page.clone());
            let _ = s2.get(id);
        });

        // Legacy: WORKERS parallel.
        let legacy = Arc::new(legacy_baseline::LegacyPageCache::new(CAPACITY));
        let l2 = Arc::clone(&legacy);
        let legacy_parallel = run_workload(WORKERS, OPS, move |id, page| {
            l2.insert(id, page.clone());
            let _ = l2.get(id);
        });

        eprintln!(
            "page_cache concurrency: sharded 1w={:?} sharded {}w={:?} legacy {}w={:?}",
            sharded_serial, WORKERS, sharded_parallel, WORKERS, legacy_parallel
        );

        // Sub-linear scaling: parallel must not be worse than ~7x the
        // serial run. Linear (no scaling) would be ~10x.
        assert!(
            sharded_parallel.as_nanos() < sharded_serial.as_nanos() * 7,
            "sharded cache did not scale: 1w={:?} {}w={:?}",
            sharded_serial,
            WORKERS,
            sharded_parallel
        );

        // Regression check: sharded must beat the legacy single-lock
        // baseline at WORKERS workers. We give the legacy a generous
        // 1.2x cushion so a noisy CI box doesn't flip the assertion
        // when both designs are within noise (would itself indicate a
        // sharding regression worth investigating).
        assert!(
            sharded_parallel.as_nanos() * 12 < legacy_parallel.as_nanos() * 10,
            "sharded cache did not beat legacy baseline: sharded {}w={:?} legacy {}w={:?}",
            WORKERS,
            sharded_parallel,
            WORKERS,
            legacy_parallel
        );
    }
}
