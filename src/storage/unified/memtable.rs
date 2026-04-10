//! Memtable — Write Buffer backed by a Skip List
//!
//! All writes go to the memtable first (in-memory, ordered). When the memtable
//! reaches a size threshold, it is flushed to the B-tree / sealed segment.
//!
//! # Write Path
//! 1. WAL write (durability)
//! 2. Memtable insert (skip list)
//! 3. Background flush when threshold reached
//!
//! # Read Path
//! 1. Check memtable first (most recent writes)
//! 2. Then check sealed segments
//! 3. Merge results (memtable takes precedence)

use super::skiplist::SkipList;

/// Tombstone marker for deleted keys
const TOMBSTONE: &[u8] = b"\x00__TOMBSTONE__\x00";

/// Configuration for the memtable
#[derive(Debug, Clone)]
pub struct MemtableConfig {
    /// Maximum size in bytes before triggering a flush
    pub max_bytes: usize,
    /// Flush threshold (percentage of max_bytes, 0.0-1.0)
    pub flush_threshold: f64,
}

impl Default for MemtableConfig {
    fn default() -> Self {
        Self {
            max_bytes: 64 * 1024 * 1024, // 64 MB
            flush_threshold: 0.75,
        }
    }
}

/// In-memory write buffer backed by a sorted skip list.
///
/// Keys and values are byte vectors, providing a generic KV interface
/// that the segment layer can use for any entity type.
pub struct Memtable {
    /// Sorted key-value store
    data: SkipList<Vec<u8>, Vec<u8>>,
    /// Approximate size in bytes
    size_bytes: usize,
    /// Number of entries (including tombstones)
    entry_count: usize,
    /// Number of tombstones
    tombstone_count: usize,
    /// Configuration
    config: MemtableConfig,
}

impl Memtable {
    /// Create a new memtable with default config
    pub fn new() -> Self {
        Self::with_config(MemtableConfig::default())
    }

    /// Create a new memtable with custom config
    pub fn with_config(config: MemtableConfig) -> Self {
        Self {
            data: SkipList::new(),
            size_bytes: 0,
            entry_count: 0,
            tombstone_count: 0,
            config,
        }
    }

    /// Put a key-value pair into the memtable
    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        let entry_size = key.len() + value.len() + 16; // overhead estimate

        if let Some(old_value) = self.data.insert(key.to_vec(), value.to_vec()) {
            // Replace: subtract old size, add new
            let old_size = key.len() + old_value.len() + 16;
            self.size_bytes = self.size_bytes.saturating_sub(old_size) + entry_size;
            // Check if we're replacing a tombstone
            if old_value == TOMBSTONE {
                self.tombstone_count -= 1;
            }
        } else {
            self.size_bytes += entry_size;
            self.entry_count += 1;
        }
    }

    /// Get a value by key.
    /// Returns `None` if the key doesn't exist or was deleted (tombstone).
    pub fn get(&self, key: &[u8]) -> Option<&[u8]> {
        match self.data.get(&key.to_vec()) {
            Some(value) if value.as_slice() != TOMBSTONE => Some(value),
            _ => None,
        }
    }

    /// Check if a key exists (and is not a tombstone)
    pub fn contains(&self, key: &[u8]) -> bool {
        self.get(key).is_some()
    }

    /// Check if a key has been explicitly deleted (tombstone)
    pub fn is_tombstone(&self, key: &[u8]) -> bool {
        self.data
            .get(&key.to_vec())
            .map(|v| v.as_slice() == TOMBSTONE)
            .unwrap_or(false)
    }

    /// Delete a key by inserting a tombstone marker.
    /// The tombstone ensures that reads don't fall through to older segments.
    pub fn delete(&mut self, key: &[u8]) {
        let entry_size = key.len() + TOMBSTONE.len() + 16;

        if let Some(old_value) = self.data.insert(key.to_vec(), TOMBSTONE.to_vec()) {
            let old_size = key.len() + old_value.len() + 16;
            self.size_bytes = self.size_bytes.saturating_sub(old_size) + entry_size;
            if old_value != TOMBSTONE {
                self.tombstone_count += 1;
            }
        } else {
            self.size_bytes += entry_size;
            self.entry_count += 1;
            self.tombstone_count += 1;
        }
    }

    /// Scan a range of keys (inclusive), excluding tombstones
    pub fn scan(&self, start: &[u8], end: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.data
            .range(&start.to_vec(), &end.to_vec())
            .filter(|(_, v)| v.as_slice() != TOMBSTONE)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Approximate size in bytes
    pub fn size_bytes(&self) -> usize {
        self.size_bytes
    }

    /// Number of entries (including tombstones)
    pub fn entry_count(&self) -> usize {
        self.entry_count
    }

    /// Number of live (non-tombstone) entries
    pub fn live_count(&self) -> usize {
        self.entry_count - self.tombstone_count
    }

    /// Whether the memtable should be flushed based on config threshold
    pub fn should_flush(&self) -> bool {
        self.size_bytes >= (self.config.max_bytes as f64 * self.config.flush_threshold) as usize
    }

    /// Whether the memtable is at maximum capacity
    pub fn is_full(&self) -> bool {
        self.size_bytes >= self.config.max_bytes
    }

    /// Drain all entries in sorted order (for flushing to B-tree).
    /// Tombstones are included so the flush layer can propagate deletes.
    pub fn drain_sorted(self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.data.drain_sorted().collect()
    }

    /// Iterate all entries (including tombstones) in sorted order
    pub fn iter(&self) -> impl Iterator<Item = (&Vec<u8>, &Vec<u8>)> {
        self.data.iter()
    }

    /// Clear the memtable
    pub fn clear(&mut self) {
        self.data.clear();
        self.size_bytes = 0;
        self.entry_count = 0;
        self.tombstone_count = 0;
    }

    /// Get memtable statistics
    pub fn stats(&self) -> MemtableStats {
        MemtableStats {
            size_bytes: self.size_bytes,
            entry_count: self.entry_count,
            live_count: self.live_count(),
            tombstone_count: self.tombstone_count,
            max_bytes: self.config.max_bytes,
            fill_ratio: self.size_bytes as f64 / self.config.max_bytes as f64,
        }
    }
}

impl Default for Memtable {
    fn default() -> Self {
        Self::new()
    }
}

/// Memtable statistics
#[derive(Debug, Clone)]
pub struct MemtableStats {
    pub size_bytes: usize,
    pub entry_count: usize,
    pub live_count: usize,
    pub tombstone_count: usize,
    pub max_bytes: usize,
    pub fill_ratio: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memtable_basic() {
        let mut mt = Memtable::new();
        mt.put(b"key1", b"value1");
        mt.put(b"key2", b"value2");

        assert_eq!(mt.get(b"key1"), Some(b"value1".as_ref()));
        assert_eq!(mt.get(b"key2"), Some(b"value2".as_ref()));
        assert_eq!(mt.get(b"key3"), None);
        assert_eq!(mt.entry_count(), 2);
        assert_eq!(mt.live_count(), 2);
    }

    #[test]
    fn test_memtable_overwrite() {
        let mut mt = Memtable::new();
        mt.put(b"key", b"old");
        mt.put(b"key", b"new");

        assert_eq!(mt.get(b"key"), Some(b"new".as_ref()));
        assert_eq!(mt.entry_count(), 1); // still 1 entry
    }

    #[test]
    fn test_memtable_delete_tombstone() {
        let mut mt = Memtable::new();
        mt.put(b"key1", b"value1");
        mt.put(b"key2", b"value2");

        mt.delete(b"key1");

        assert_eq!(mt.get(b"key1"), None); // deleted
        assert!(mt.is_tombstone(b"key1")); // has tombstone
        assert_eq!(mt.get(b"key2"), Some(b"value2".as_ref())); // still there
        assert_eq!(mt.live_count(), 1);
        assert_eq!(mt.tombstone_count, 1);
    }

    #[test]
    fn test_memtable_scan() {
        let mut mt = Memtable::new();
        mt.put(b"a", b"1");
        mt.put(b"b", b"2");
        mt.put(b"c", b"3");
        mt.put(b"d", b"4");
        mt.delete(b"c"); // tombstone

        let results = mt.scan(b"b", b"d");
        assert_eq!(results.len(), 2); // b and d (c is tombstoned)
        assert_eq!(results[0], (b"b".to_vec(), b"2".to_vec()));
        assert_eq!(results[1], (b"d".to_vec(), b"4".to_vec()));
    }

    #[test]
    fn test_memtable_drain_sorted() {
        let mut mt = Memtable::new();
        mt.put(b"c", b"3");
        mt.put(b"a", b"1");
        mt.put(b"b", b"2");

        let drained = mt.drain_sorted();
        let keys: Vec<_> = drained.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn test_memtable_should_flush() {
        let config = MemtableConfig {
            max_bytes: 100,
            flush_threshold: 0.5,
        };
        let mut mt = Memtable::with_config(config);

        assert!(!mt.should_flush());

        // Insert enough data to cross the threshold (50 bytes)
        mt.put(b"aaaaaaaaaa", b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        assert!(mt.should_flush());
    }

    #[test]
    fn test_memtable_stats() {
        let mut mt = Memtable::new();
        mt.put(b"k1", b"v1");
        mt.put(b"k2", b"v2");
        mt.delete(b"k1");

        let stats = mt.stats();
        assert_eq!(stats.entry_count, 2);
        assert_eq!(stats.live_count, 1);
        assert_eq!(stats.tombstone_count, 1);
        assert!(stats.size_bytes > 0);
    }

    #[test]
    fn test_memtable_clear() {
        let mut mt = Memtable::new();
        mt.put(b"k1", b"v1");
        mt.put(b"k2", b"v2");

        mt.clear();
        assert_eq!(mt.entry_count(), 0);
        assert_eq!(mt.size_bytes(), 0);
        assert_eq!(mt.get(b"k1"), None);
    }
}
