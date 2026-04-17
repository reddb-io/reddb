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
//! - Turso `core/storage/page_cache.rs:24-80` - PageCacheEntry with ref_bit
//! - Turso `core/storage/page_cache.rs:129-150` - advance_clock_hand()
//! - "SIEVE is Simpler than LRU" (NSDI '24)

use std::collections::{HashMap, VecDeque};
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

/// Cache entry with SIEVE visited bit
struct CacheEntry {
    /// The cached page
    page: Page,
    /// Visited bit for SIEVE algorithm (true = recently accessed)
    visited: bool,
    /// Pin count (page cannot be evicted while pinned)
    pin_count: usize,
    /// Whether the page is dirty (modified)
    dirty: bool,
}

impl CacheEntry {
    fn new(page: Page) -> Self {
        Self {
            page,
            visited: false,
            pin_count: 0,
            dirty: false,
        }
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
pub struct PageCache {
    /// Maximum number of pages to cache
    capacity: usize,
    /// Page ID -> Entry index mapping
    index: RwLock<HashMap<u32, usize>>,
    /// FIFO queue of page IDs for eviction order
    fifo: Mutex<VecDeque<u32>>,
    /// Cache entries (indexed by slot)
    entries: RwLock<Vec<Option<CacheEntry>>>,
    /// Free slots
    free_slots: Mutex<Vec<usize>>,
    /// SIEVE eviction hand position
    hand: Mutex<usize>,
    /// Cache statistics
    stats: Mutex<CacheStats>,
}

impl PageCache {
    /// Create a new page cache with specified capacity
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(MIN_CACHE_CAPACITY);

        Self {
            capacity,
            index: RwLock::new(HashMap::with_capacity(capacity)),
            fifo: Mutex::new(VecDeque::with_capacity(capacity)),
            entries: RwLock::new(Vec::with_capacity(capacity)),
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

        // Get entry and mark as visited
        let entries = cache_read(&self.entries);
        if let Some(entry) = entries.get(slot).and_then(|e| e.as_ref()) {
            // Mark as visited (SIEVE)
            // Note: In a truly concurrent implementation, this would use atomics
            let page = entry.page.clone();
            drop(entries);

            // Update visited bit
            let mut entries = cache_write(&self.entries);
            if let Some(Some(entry)) = entries.get_mut(slot) {
                entry.visited = true;
            }

            // Update stats
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

                // Update existing entry
                let mut entries = cache_write(&self.entries);
                if let Some(Some(entry)) = entries.get_mut(slot) {
                    entry.page = page;
                    entry.visited = true;
                }
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

            // Check entry
            let (should_evict, dirty) = {
                let entries = cache_read(&self.entries);
                match entries.get(slot).and_then(|e| e.as_ref()) {
                    Some(entry) => {
                        if entry.pin_count > 0 {
                            // Pinned, can't evict
                            (false, false)
                        } else if entry.visited {
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
                // Clear visited bit
                let mut entries = cache_write(&self.entries);
                if let Some(Some(entry)) = entries.get_mut(slot) {
                    entry.visited = false;
                }
                *hand += 1;
                continue;
            }

            // Evict this entry
            let evicted_page = {
                let mut entries = cache_write(&self.entries);
                let entry = entries[slot].take();
                entry.map(|e| e.page)
            };

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
        let cache = PageCache::new(100);

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
        let cache = PageCache::new(4);

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
        let cache = PageCache::new(4);

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
        let cache = PageCache::new(100);

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
        let cache = PageCache::new(2);

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
        let cache = PageCache::new(100);

        cache.insert(1, make_page(1));
        assert!(cache.contains(1));

        let removed = cache.remove(1);
        assert!(removed.is_some());
        assert!(!cache.contains(1));
    }

    #[test]
    fn test_cache_stats() {
        let cache = PageCache::new(100);

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
        let cache = PageCache::new(100);

        for i in 0..50 {
            cache.insert(i, make_page(i));
        }

        assert_eq!(cache.len(), 50);

        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn test_cache_update_existing() {
        let cache = PageCache::new(100);

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
        let cache = std::sync::Arc::new(PageCache::new(8));
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
        let cache = std::sync::Arc::new(PageCache::new(8));
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
}
