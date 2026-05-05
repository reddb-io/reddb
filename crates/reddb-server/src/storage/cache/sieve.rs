//! SIEVE Page Cache
//!
//! Implementation of the SIEVE cache eviction algorithm for database pages.
//!
//! SIEVE (Simple, Efficient, and Versatile Eviction) is a modern cache
//! eviction algorithm that is simpler than LRU but often performs better.
//!
//! Key properties:
//! - O(1) insertion, lookup, and eviction
//! - No metadata updates on cache hits (just set visited bit)
//! - Uses circular buffer with single "hand" pointer
//! - Sweeps to find eviction candidates
//!
//! Reference: "SIEVE is Simpler than LRU: An Efficient Turn-Key Eviction Algorithm for Web Caches"
//! by Yazhuo Zhang et al. (2023)

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};

fn recover_read_guard<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn recover_write_guard<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    match lock.write() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Page identifier type
pub type PageId = u64;

/// Default page size (4KB)
pub const DEFAULT_PAGE_SIZE: usize = 4096;

/// Cache entry containing page data and metadata
#[derive(Debug)]
struct CacheEntry<V> {
    /// The cached value
    value: V,
    /// Visited flag (set on access)
    visited: AtomicBool,
    /// Entry index in the circular buffer
    index: usize,
    /// Dirty flag (modified since loaded)
    dirty: AtomicBool,
    /// Pin count (prevent eviction)
    pin_count: AtomicUsize,
}

impl<V> CacheEntry<V> {
    fn new(value: V, index: usize) -> Self {
        Self {
            value,
            visited: AtomicBool::new(true), // New entries start as visited
            index,
            dirty: AtomicBool::new(false),
            pin_count: AtomicUsize::new(0),
        }
    }

    fn is_visited(&self) -> bool {
        self.visited.load(Ordering::Relaxed)
    }

    fn set_visited(&self, visited: bool) {
        self.visited.store(visited, Ordering::Relaxed);
    }

    fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }

    fn clear_dirty(&self) {
        self.dirty.store(false, Ordering::Relaxed);
    }

    fn pin(&self) {
        self.pin_count.fetch_add(1, Ordering::SeqCst);
    }

    fn unpin(&self) {
        self.pin_count.fetch_sub(1, Ordering::SeqCst);
    }

    fn is_pinned(&self) -> bool {
        self.pin_count.load(Ordering::SeqCst) > 0
    }
}

/// Circular buffer slot
#[derive(Debug, Clone)]
enum Slot<K>
where
    K: Clone,
{
    /// Empty slot
    Empty,
    /// Occupied with key
    Occupied(K),
}

/// Cache configuration
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Maximum number of entries
    pub capacity: usize,
    /// Page size in bytes
    pub page_size: usize,
    /// Enable statistics collection
    pub collect_stats: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            capacity: 1024,
            page_size: DEFAULT_PAGE_SIZE,
            collect_stats: true,
        }
    }
}

impl CacheConfig {
    /// Create with specific capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            ..Default::default()
        }
    }

    /// Set page size
    pub fn with_page_size(mut self, page_size: usize) -> Self {
        self.page_size = page_size;
        self
    }

    /// Calculate total memory usage
    pub fn memory_size(&self) -> usize {
        self.capacity * self.page_size
    }
}

/// Cache statistics
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Total cache hits
    pub hits: u64,
    /// Total cache misses
    pub misses: u64,
    /// Total insertions
    pub insertions: u64,
    /// Total evictions
    pub evictions: u64,
    /// Current entries
    pub entries: usize,
    /// Dirty pages written back
    pub writebacks: u64,
    /// Hand sweeps performed
    pub sweeps: u64,
}

impl CacheStats {
    /// Calculate hit ratio
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Calculate miss ratio
    pub fn miss_ratio(&self) -> f64 {
        1.0 - self.hit_ratio()
    }
}

/// Atomic cache statistics
struct AtomicStats {
    hits: AtomicU64,
    misses: AtomicU64,
    insertions: AtomicU64,
    evictions: AtomicU64,
    writebacks: AtomicU64,
    sweeps: AtomicU64,
}

impl AtomicStats {
    fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            insertions: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            writebacks: AtomicU64::new(0),
            sweeps: AtomicU64::new(0),
        }
    }

    fn to_stats(&self, entries: usize) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            insertions: self.insertions.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            entries,
            writebacks: self.writebacks.load(Ordering::Relaxed),
            sweeps: self.sweeps.load(Ordering::Relaxed),
        }
    }
}

/// Page cache callback for writeback
pub trait PageWriter<K, V>: Send + Sync {
    /// Write a dirty page back to storage
    fn write_page(&self, key: &K, value: &V) -> std::io::Result<()>;
}

/// No-op page writer (for read-only caches)
pub struct NoOpWriter;

impl<K, V> PageWriter<K, V> for NoOpWriter {
    fn write_page(&self, _key: &K, _value: &V) -> std::io::Result<()> {
        Ok(())
    }
}

/// SIEVE Page Cache
pub struct PageCache<K, V, W = NoOpWriter>
where
    K: Clone + Eq + Hash,
    V: Clone,
    W: PageWriter<K, V>,
{
    /// Configuration
    config: CacheConfig,
    /// Key to entry mapping
    entries: RwLock<HashMap<K, Arc<CacheEntry<V>>>>,
    /// Circular buffer of slots
    slots: RwLock<Vec<Slot<K>>>,
    /// Current hand position
    hand: AtomicUsize,
    /// Current entry count
    count: AtomicUsize,
    /// Statistics
    stats: AtomicStats,
    /// Page writer for dirty pages
    writer: W,
    /// Per-strategy buffer rings.
    ///
    /// Lazily allocated when a non-`Normal` strategy is first used. The
    /// rings are completely isolated from the main pool — a page in a
    /// ring does NOT appear in `entries`/`slots`, and vice versa. See
    /// `src/storage/cache/README.md` § Invariant 4.
    rings:
        RwLock<HashMap<super::strategy::BufferAccessStrategy, Arc<super::ring::BufferRing<K, V>>>>,
}

impl<K, V> PageCache<K, V, NoOpWriter>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    /// Create new cache with default writer
    pub fn new(config: CacheConfig) -> Self {
        Self::with_writer(config, NoOpWriter)
    }

    /// Create with specific capacity
    pub fn with_capacity(capacity: usize) -> Self {
        Self::new(CacheConfig::with_capacity(capacity))
    }
}

impl<K, V, W> PageCache<K, V, W>
where
    K: Clone + Eq + Hash,
    V: Clone,
    W: PageWriter<K, V>,
{
    /// Create new cache with custom writer
    pub fn with_writer(config: CacheConfig, writer: W) -> Self {
        let capacity = config.capacity;
        Self {
            config,
            entries: RwLock::new(HashMap::with_capacity(capacity)),
            slots: RwLock::new(vec![Slot::Empty; capacity]),
            hand: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
            stats: AtomicStats::new(),
            writer,
            rings: RwLock::new(HashMap::new()),
        }
    }

    /// Strategy-aware get.
    ///
    /// `Normal` behaves exactly like [`PageCache::get`]. Non-`Normal`
    /// strategies look in the main pool first (a hit is a free win),
    /// then fall through to the strategy's dedicated ring buffer.
    /// Hits in the ring do NOT promote the page into the main pool —
    /// that is the whole point of the strategy: keep scans out of the
    /// hot working set.
    pub fn get_with(&self, key: &K, strategy: super::strategy::BufferAccessStrategy) -> Option<V> {
        // Always check the main pool first — a present page should be
        // served from there at zero cost regardless of strategy.
        if let Some(v) = self.get(key) {
            return Some(v);
        }
        // Non-Normal strategies fall through to their ring.
        if strategy.is_ring() {
            if let Some(ring) = self.get_ring(strategy) {
                return ring.get(key);
            }
        }
        None
    }

    /// Strategy-aware insert.
    ///
    /// `Normal` behaves exactly like [`PageCache::insert`]. Non-`Normal`
    /// strategies route the write into the dedicated ring instead of
    /// the main pool. The ring's eviction return is propagated up so
    /// callers (the pager) can flush dirty pages through the
    /// double-write buffer.
    pub fn insert_with(
        &self,
        key: K,
        value: V,
        strategy: super::strategy::BufferAccessStrategy,
    ) -> Option<(K, V)> {
        if !strategy.is_ring() {
            // Normal path: existing insert, returning the prior value
            // wrapped in (key, value) tuple shape for caller uniformity.
            let prev = self.insert(key.clone(), value);
            return prev.map(|v| (key, v));
        }
        let ring = self.ensure_ring(strategy);
        ring.insert(key, value)
    }

    /// Look up the ring for `strategy`, creating it lazily if needed.
    fn ensure_ring(
        &self,
        strategy: super::strategy::BufferAccessStrategy,
    ) -> Arc<super::ring::BufferRing<K, V>> {
        // Fast path: ring already exists.
        {
            let rings = recover_read_guard(&self.rings);
            if let Some(r) = rings.get(&strategy) {
                return Arc::clone(r);
            }
        }
        // Slow path: create under write lock, double-check first.
        let mut rings = recover_write_guard(&self.rings);
        if let Some(r) = rings.get(&strategy) {
            return Arc::clone(r);
        }
        let cap = strategy.ring_size().unwrap_or(16);
        let ring = Arc::new(super::ring::BufferRing::new(cap));
        rings.insert(strategy, Arc::clone(&ring));
        ring
    }

    /// Read-only ring lookup (does not allocate).
    fn get_ring(
        &self,
        strategy: super::strategy::BufferAccessStrategy,
    ) -> Option<Arc<super::ring::BufferRing<K, V>>> {
        let rings = recover_read_guard(&self.rings);
        rings.get(&strategy).cloned()
    }

    /// Drop every strategy ring. Used by tests and by post-checkpoint
    /// cleanup.
    pub fn clear_strategy_rings(&self) {
        let rings = recover_read_guard(&self.rings);
        for ring in rings.values() {
            ring.clear();
        }
    }

    /// Get an entry from cache
    pub fn get(&self, key: &K) -> Option<V> {
        let entries = recover_read_guard(&self.entries);

        if let Some(entry) = entries.get(key) {
            // Set visited flag (no lock needed - atomic)
            entry.set_visited(true);

            if self.config.collect_stats {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
            }

            Some(entry.value.clone())
        } else {
            if self.config.collect_stats {
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
            }
            None
        }
    }

    /// Check if key exists in cache
    pub fn contains(&self, key: &K) -> bool {
        recover_read_guard(&self.entries).contains_key(key)
    }

    /// Insert an entry into cache
    pub fn insert(&self, key: K, value: V) -> Option<V> {
        // Check if update first (no locks held while checking)
        {
            let entries = recover_read_guard(&self.entries);
            if let Some(entry) = entries.get(&key) {
                entry.set_visited(true);
                let old_value = entry.value.clone();
                drop(entries);
                return self.update_existing(key, value, old_value);
            }
        }

        // Need to insert new entry - evict if needed.
        //
        // `count` is read with Acquire to pair with the Release stores
        // in fetch_add/fetch_sub. We do not need SeqCst here — there's
        // no cross-atomic ordering requirement, and the subsequent
        // write lock on `entries` provides the actual synchronisation.
        let index = if self.count.load(Ordering::Acquire) >= self.config.capacity {
            self.evict_one()
        } else {
            None
        };

        // Now acquire locks in consistent order: entries first, then slots
        let mut entries = recover_write_guard(&self.entries);
        let mut slots = recover_write_guard(&self.slots);

        // Double-check the key wasn't inserted while we waited
        if entries.contains_key(&key) {
            if let Some(entry) = entries.get(&key) {
                entry.set_visited(true);
            }
            return None;
        }

        // Find slot index
        let slot_index = if let Some(idx) = index {
            idx
        } else {
            // Find empty slot
            slots.iter().position(|s| matches!(s, Slot::Empty))?
        };

        // Insert into slot and entry map
        let entry = Arc::new(CacheEntry::new(value, slot_index));
        slots[slot_index] = Slot::Occupied(key.clone());
        entries.insert(key, entry);

        // Release-store: pairs with the Acquire-load above. The
        // entries write lock has already published the slot, so
        // counters need only single-variable Release semantics.
        self.count.fetch_add(1, Ordering::Release);

        if self.config.collect_stats {
            self.stats.insertions.fetch_add(1, Ordering::Relaxed);
        }

        None
    }

    /// Update existing entry (internal)
    fn update_existing(&self, key: K, new_value: V, old_value: V) -> Option<V> {
        let mut entries = recover_write_guard(&self.entries);

        if let Some(old_entry) = entries.get(&key) {
            let index = old_entry.index;
            let new_entry = Arc::new(CacheEntry::new(new_value, index));
            entries.insert(key, new_entry);
            Some(old_value)
        } else {
            None
        }
    }

    /// Remove an entry from cache
    pub fn remove(&self, key: &K) -> Option<V> {
        let mut entries = recover_write_guard(&self.entries);

        if let Some(entry) = entries.remove(key) {
            let mut slots = recover_write_guard(&self.slots);
            slots[entry.index] = Slot::Empty;
            self.count.fetch_sub(1, Ordering::Release);

            // Writeback if dirty
            if entry.is_dirty() {
                let _ = self.writer.write_page(key, &entry.value);
                if self.config.collect_stats {
                    self.stats.writebacks.fetch_add(1, Ordering::Relaxed);
                }
            }

            Some(entry.value.clone())
        } else {
            None
        }
    }

    /// Evict one entry using SIEVE algorithm
    ///
    /// **Atomic ordering note:** the `hand` pointer is read and
    /// written with `Relaxed` because it does not coordinate
    /// visibility of any page content — the hand is just a sweep
    /// position, and concurrent writers always re-acquire the
    /// `entries`/`slots` write locks before touching anything the
    /// hand selects. The `pin_count` check inside `is_pinned()`
    /// stays SeqCst (in `CacheEntry`) and that single SeqCst load
    /// is what coordinates pin/unpin visibility across threads.
    fn evict_one(&self) -> Option<usize> {
        let capacity = self.config.capacity;
        let max_sweeps = capacity * 2;

        for _ in 0..max_sweeps {
            let current_hand = self.hand.load(Ordering::Relaxed);

            // Acquire both locks in consistent order: entries first, then slots
            let mut entries = recover_write_guard(&self.entries);
            let mut slots = recover_write_guard(&self.slots);

            if let Slot::Occupied(ref key) = slots[current_hand] {
                if let Some(entry) = entries.get(key) {
                    if entry.is_pinned() {
                        // Can't evict pinned entry, advance hand
                    } else if entry.is_visited() {
                        // Reset visited flag, give second chance
                        entry.set_visited(false);
                    } else {
                        // Evict this entry
                        let key_clone = key.clone();
                        let Some(entry) = entries.remove(&key_clone) else {
                            let next = (current_hand + 1) % capacity;
                            self.hand.store(next, Ordering::Relaxed);
                            continue;
                        };

                        // Writeback if dirty
                        if entry.is_dirty() {
                            let _ = self.writer.write_page(&key_clone, &entry.value);
                            if self.config.collect_stats {
                                self.stats.writebacks.fetch_add(1, Ordering::Relaxed);
                            }
                        }

                        slots[current_hand] = Slot::Empty;
                        self.count.fetch_sub(1, Ordering::Release);

                        if self.config.collect_stats {
                            self.stats.evictions.fetch_add(1, Ordering::Relaxed);
                            self.stats.sweeps.fetch_add(1, Ordering::Relaxed);
                        }

                        // Advance hand for next eviction
                        let next = (current_hand + 1) % capacity;
                        self.hand.store(next, Ordering::Relaxed);

                        return Some(current_hand);
                    }
                }
            }

            // Advance hand and try next slot
            let next = (current_hand + 1) % capacity;
            self.hand.store(next, Ordering::Relaxed);
        }

        if self.config.collect_stats {
            self.stats.sweeps.fetch_add(1, Ordering::Relaxed);
        }

        None
    }

    /// Pin a page (prevent eviction)
    pub fn pin(&self, key: &K) -> bool {
        let entries = recover_read_guard(&self.entries);
        if let Some(entry) = entries.get(key) {
            entry.pin();
            true
        } else {
            false
        }
    }

    /// Unpin a page
    pub fn unpin(&self, key: &K) -> bool {
        let entries = recover_read_guard(&self.entries);
        if let Some(entry) = entries.get(key) {
            entry.unpin();
            true
        } else {
            false
        }
    }

    /// Mark a page as dirty
    pub fn mark_dirty(&self, key: &K) -> bool {
        let entries = recover_read_guard(&self.entries);
        if let Some(entry) = entries.get(key) {
            entry.mark_dirty();
            true
        } else {
            false
        }
    }

    /// Flush all dirty pages
    pub fn flush(&self) -> std::io::Result<usize> {
        let entries = recover_read_guard(&self.entries);
        let mut flushed = 0;

        for (key, entry) in entries.iter() {
            if entry.is_dirty() {
                self.writer.write_page(key, &entry.value)?;
                entry.clear_dirty();
                flushed += 1;
            }
        }

        if self.config.collect_stats {
            self.stats
                .writebacks
                .fetch_add(flushed as u64, Ordering::Relaxed);
        }

        Ok(flushed)
    }

    /// Clear all entries
    pub fn clear(&self) {
        // Flush dirty pages first
        let _ = self.flush();

        let mut entries = recover_write_guard(&self.entries);
        let mut slots = recover_write_guard(&self.slots);

        entries.clear();
        for slot in slots.iter_mut() {
            *slot = Slot::Empty;
        }

        // clear() is exclusive (write locks above); Relaxed is safe.
        self.count.store(0, Ordering::Relaxed);
        self.hand.store(0, Ordering::Relaxed);
    }

    /// Get current entry count
    pub fn len(&self) -> usize {
        // Acquire pairs with the Release stores in insert/remove/evict.
        self.count.load(Ordering::Acquire)
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get capacity
    pub fn capacity(&self) -> usize {
        self.config.capacity
    }

    /// Get statistics
    pub fn stats(&self) -> CacheStats {
        self.stats.to_stats(self.len())
    }

    /// Get configuration
    pub fn config(&self) -> &CacheConfig {
        &self.config
    }

    /// Get all cached keys
    pub fn keys(&self) -> Vec<K> {
        recover_read_guard(&self.entries).keys().cloned().collect()
    }

    /// Get dirty page count
    pub fn dirty_count(&self) -> usize {
        self.entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|e| e.is_dirty())
            .count()
    }
}

/// Page buffer (fixed-size byte array)
#[derive(Clone)]
pub struct Page {
    /// Page data
    data: Vec<u8>,
    /// Page size
    size: usize,
}

impl Page {
    /// Create new page with default size
    pub fn new() -> Self {
        Self::with_size(DEFAULT_PAGE_SIZE)
    }

    /// Create page with specific size
    pub fn with_size(size: usize) -> Self {
        Self {
            data: vec![0u8; size],
            size,
        }
    }

    /// Create page from data
    pub fn from_data(data: Vec<u8>) -> Self {
        let size = data.len();
        Self { data, size }
    }

    /// Get page data
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Get mutable page data
    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Get page size
    pub fn size(&self) -> usize {
        self.size
    }

    /// Read bytes at offset
    pub fn read(&self, offset: usize, len: usize) -> Option<&[u8]> {
        if offset + len <= self.size {
            Some(&self.data[offset..offset + len])
        } else {
            None
        }
    }

    /// Write bytes at offset
    pub fn write(&mut self, offset: usize, data: &[u8]) -> bool {
        if offset + data.len() <= self.size {
            self.data[offset..offset + data.len()].copy_from_slice(data);
            true
        } else {
            false
        }
    }

    /// Read u32 at offset
    pub fn read_u32(&self, offset: usize) -> Option<u32> {
        self.read(offset, 4).map(|bytes| {
            let mut array = [0u8; 4];
            array.copy_from_slice(bytes);
            u32::from_le_bytes(array)
        })
    }

    /// Write u32 at offset
    pub fn write_u32(&mut self, offset: usize, value: u32) {
        self.write(offset, &value.to_le_bytes());
    }

    /// Read u64 at offset
    pub fn read_u64(&self, offset: usize) -> Option<u64> {
        self.read(offset, 8).map(|bytes| {
            let mut array = [0u8; 8];
            array.copy_from_slice(bytes);
            u64::from_le_bytes(array)
        })
    }

    /// Write u64 at offset
    pub fn write_u64(&mut self, offset: usize, value: u64) {
        self.write(offset, &value.to_le_bytes());
    }
}

impl Default for Page {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("size", &self.size)
            .field("data", &format!("[{} bytes]", self.data.len()))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(10);

        // Insert
        cache.insert(1, "one".to_string());
        cache.insert(2, "two".to_string());

        // Get
        assert_eq!(cache.get(&1), Some("one".to_string()));
        assert_eq!(cache.get(&2), Some("two".to_string()));
        assert_eq!(cache.get(&3), None);

        // Contains
        assert!(cache.contains(&1));
        assert!(!cache.contains(&3));

        // Remove
        assert_eq!(cache.remove(&1), Some("one".to_string()));
        assert_eq!(cache.get(&1), None);
    }

    #[test]
    fn test_eviction() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(3);

        // Fill cache
        cache.insert(1, "one".to_string());
        cache.insert(2, "two".to_string());
        cache.insert(3, "three".to_string());

        assert_eq!(cache.len(), 3);

        // Access some entries to set visited
        cache.get(&1);
        cache.get(&3);

        // Insert new entry - should evict entry 2 (unvisited)
        cache.insert(4, "four".to_string());

        assert_eq!(cache.len(), 3);
        assert!(cache.contains(&4));

        // Entry 2 should be evicted (wasn't visited)
        // Note: actual eviction depends on hand position
    }

    #[test]
    fn test_stats() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(10);

        cache.insert(1, "one".to_string());
        cache.get(&1); // Hit
        cache.get(&2); // Miss

        let stats = cache.stats();
        assert_eq!(stats.insertions, 1);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.hit_ratio(), 0.5);
    }

    #[test]
    fn test_pin_unpin() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(2);

        cache.insert(1, "one".to_string());
        cache.insert(2, "two".to_string());

        // Pin entry 1
        assert!(cache.pin(&1));

        // Try to evict by inserting more
        cache.insert(3, "three".to_string());

        // Pinned entry should still be there
        assert!(cache.contains(&1));

        // Unpin
        cache.unpin(&1);
    }

    #[test]
    fn test_page() {
        let mut page = Page::with_size(64);

        // Write and read
        page.write(0, b"hello");
        assert_eq!(page.read(0, 5), Some(b"hello".as_slice()));

        // Write u32
        page.write_u32(8, 0x12345678);
        assert_eq!(page.read_u32(8), Some(0x12345678));

        // Write u64
        page.write_u64(16, 0xDEADBEEF);
        assert_eq!(page.read_u64(16), Some(0xDEADBEEF));

        // Bounds check
        assert_eq!(page.read(60, 10), None);
    }

    #[test]
    fn test_clear() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(10);

        cache.insert(1, "one".to_string());
        cache.insert(2, "two".to_string());

        cache.clear();

        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn test_keys() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(10);

        cache.insert(1, "one".to_string());
        cache.insert(2, "two".to_string());
        cache.insert(3, "three".to_string());

        let keys = cache.keys();
        assert_eq!(keys.len(), 3);
        assert!(keys.contains(&1));
        assert!(keys.contains(&2));
        assert!(keys.contains(&3));
    }

    #[test]
    fn test_update() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(10);

        cache.insert(1, "one".to_string());
        assert_eq!(cache.get(&1), Some("one".to_string()));

        // Update
        let old = cache.insert(1, "ONE".to_string());
        assert_eq!(old, Some("one".to_string()));
        assert_eq!(cache.get(&1), Some("ONE".to_string()));
    }

    #[test]
    fn test_dirty_pages() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(10);

        cache.insert(1, "one".to_string());
        cache.insert(2, "two".to_string());

        assert_eq!(cache.dirty_count(), 0);

        cache.mark_dirty(&1);
        assert_eq!(cache.dirty_count(), 1);

        cache.mark_dirty(&2);
        assert_eq!(cache.dirty_count(), 2);
    }

    #[test]
    fn test_config() {
        let config = CacheConfig::with_capacity(1024).with_page_size(8192);

        assert_eq!(config.capacity, 1024);
        assert_eq!(config.page_size, 8192);
        assert_eq!(config.memory_size(), 1024 * 8192);
    }

    // ---------------------------------------------------------------
    // Target 4: BufferAccessStrategy / ring tests
    // ---------------------------------------------------------------

    use super::super::strategy::BufferAccessStrategy;

    #[test]
    fn normal_strategy_is_backwards_compatible() {
        // get_with(Normal) and insert_with(Normal) must behave exactly
        // like get/insert — same hot pool, same eviction.
        let cache: PageCache<u64, String> = PageCache::with_capacity(8);
        let prev = cache.insert_with(1, "a".to_string(), BufferAccessStrategy::Normal);
        assert!(prev.is_none());
        assert_eq!(
            cache.get_with(&1, BufferAccessStrategy::Normal),
            Some("a".to_string())
        );
        // Plain get/insert see the same value.
        assert_eq!(cache.get(&1), Some("a".to_string()));
    }

    #[test]
    fn sequential_scan_does_not_pollute_main_pool() {
        // Warm the main pool, then do a scan via SequentialScan.
        // The hot keys must still be in the main pool afterwards.
        let cache: PageCache<u64, String> = PageCache::with_capacity(64);
        for i in 0..50 {
            cache.insert(i, format!("hot-{i}"));
        }
        // Now scan 200 cold pages via SequentialScan strategy.
        for k in 1000..1200u64 {
            let _ = cache.insert_with(k, format!("cold-{k}"), BufferAccessStrategy::SequentialScan);
        }
        // Hot keys must still be present in the main pool.
        for i in 0..50u64 {
            assert!(
                cache.contains(&i),
                "hot key {i} was evicted by sequential scan"
            );
        }
    }

    #[test]
    fn scan_pages_are_findable_via_strategy_get() {
        // Pages inserted via SequentialScan are reachable through
        // get_with(SequentialScan) but NOT through plain get (they live
        // in the ring, not the main pool).
        let cache: PageCache<u64, String> = PageCache::with_capacity(64);
        cache.insert_with(
            42,
            "scanned".to_string(),
            BufferAccessStrategy::SequentialScan,
        );
        // Plain get hits main pool only — must miss.
        assert_eq!(cache.get(&42), None);
        // get_with sees both pools.
        assert_eq!(
            cache.get_with(&42, BufferAccessStrategy::SequentialScan),
            Some("scanned".to_string())
        );
    }

    #[test]
    fn bulk_read_and_bulk_write_are_independent_rings() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(64);
        cache.insert_with(1, "r".to_string(), BufferAccessStrategy::BulkRead);
        cache.insert_with(2, "w".to_string(), BufferAccessStrategy::BulkWrite);

        // Each strategy sees its own page only.
        assert_eq!(
            cache.get_with(&1, BufferAccessStrategy::BulkRead),
            Some("r".to_string())
        );
        assert_eq!(
            cache.get_with(&2, BufferAccessStrategy::BulkWrite),
            Some("w".to_string())
        );

        // Cross-strategy lookups miss because rings are isolated.
        assert!(cache
            .get_with(&1, BufferAccessStrategy::BulkWrite)
            .is_none());
        assert!(cache.get_with(&2, BufferAccessStrategy::BulkRead).is_none());
    }

    #[test]
    fn bulk_write_evicts_dirty_page_on_overflow() {
        // Fill a BulkWrite ring (capacity 32) past its limit and verify
        // insert_with returns the evicted (key, value) pair so the
        // pager can flush it.
        let cache: PageCache<u64, String> = PageCache::with_capacity(64);
        let mut last_evicted = None;
        for i in 0..40u64 {
            let evicted = cache.insert_with(i, format!("v{i}"), BufferAccessStrategy::BulkWrite);
            if evicted.is_some() {
                last_evicted = evicted;
            }
        }
        // Some eviction must have happened (40 inserts into a 32-slot ring).
        assert!(last_evicted.is_some());
        // The first 8 keys should be evicted.
        for i in 0..8u64 {
            assert!(
                cache
                    .get_with(&i, BufferAccessStrategy::BulkWrite)
                    .is_none(),
                "key {i} should have been evicted from bulk_write ring"
            );
        }
    }

    #[test]
    fn clear_strategy_rings_drops_all_ring_pages() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(64);
        // Seed both pools.
        cache.insert(99, "main".to_string());
        cache.insert_with(1, "ring".to_string(), BufferAccessStrategy::SequentialScan);
        // Clear only the rings.
        cache.clear_strategy_rings();
        // Main pool survives.
        assert_eq!(cache.get(&99), Some("main".to_string()));
        // Ring is empty.
        assert!(cache
            .get_with(&1, BufferAccessStrategy::SequentialScan)
            .is_none());
    }

    #[test]
    fn ring_is_lazily_allocated() {
        let cache: PageCache<u64, String> = PageCache::with_capacity(64);
        // Initially no rings exist.
        assert!(cache
            .get_with(&1, BufferAccessStrategy::SequentialScan)
            .is_none());
        // Inserting via a strategy creates the ring.
        cache.insert_with(1, "a".to_string(), BufferAccessStrategy::SequentialScan);
        assert_eq!(
            cache.get_with(&1, BufferAccessStrategy::SequentialScan),
            Some("a".to_string())
        );
    }
}
