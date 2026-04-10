//! Per-segment Bloom Filter Registry
//!
//! Provides probabilistic key-existence testing per segment to avoid
//! unnecessary B-tree lookups. When a query checks for a key, the bloom
//! filter can definitively say "not in this segment" — skipping a scan.
//!
//! # Integration
//!
//! - On entity insert: `registry.add_key(collection, segment_id, key_bytes)`
//! - On query: `registry.candidate_segments(collection, key_bytes)` returns
//!   only those segments that *might* contain the key.
//! - On segment seal: the bloom filter is frozen and can be serialized.
//! - On segment merge/compaction: bloom filters are merged via bitwise OR.

use std::collections::HashMap;
use std::sync::RwLock;

use super::segment::SegmentId;
use crate::storage::primitives::bloom::BloomFilter;

/// Default expected entities per segment for bloom sizing
const DEFAULT_EXPECTED_ENTITIES: usize = 100_000;

/// Default false positive rate (1%)
const DEFAULT_FP_RATE: f64 = 0.01;

/// Per-segment bloom filter with tracking metadata
pub struct SegmentBloom {
    /// The bloom filter
    pub filter: BloomFilter,
    /// Number of keys inserted
    pub key_count: usize,
    /// Whether the bloom is frozen (segment sealed)
    pub frozen: bool,
}

impl SegmentBloom {
    /// Create a new bloom filter for a segment
    pub fn new(expected_elements: usize, fp_rate: f64) -> Self {
        Self {
            filter: BloomFilter::with_capacity(expected_elements, fp_rate),
            key_count: 0,
            frozen: false,
        }
    }

    /// Add a key to the bloom filter
    pub fn add(&mut self, key: &[u8]) {
        if !self.frozen {
            self.filter.insert(key);
            self.key_count += 1;
        }
    }

    /// Check if a key might exist
    pub fn might_contain(&self, key: &[u8]) -> bool {
        self.filter.contains(key)
    }

    /// Freeze the bloom filter (segment sealed)
    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    /// Get estimated false positive rate based on actual insertions
    pub fn estimated_fp_rate(&self) -> f64 {
        self.filter.estimate_fp_rate(self.key_count)
    }

    /// Get memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        self.filter.byte_size() + std::mem::size_of::<Self>()
    }
}

/// Registry managing bloom filters across all segments in all collections.
///
/// Thread-safe via RwLock — reads are concurrent, writes are exclusive.
pub struct BloomFilterRegistry {
    /// (collection, segment_id) → SegmentBloom
    blooms: RwLock<HashMap<(String, SegmentId), SegmentBloom>>,
    /// Default expected elements per segment
    expected_elements: usize,
    /// Default false positive rate
    fp_rate: f64,
}

impl BloomFilterRegistry {
    /// Create a new registry with default parameters
    pub fn new() -> Self {
        Self {
            blooms: RwLock::new(HashMap::new()),
            expected_elements: DEFAULT_EXPECTED_ENTITIES,
            fp_rate: DEFAULT_FP_RATE,
        }
    }

    /// Create a registry with custom parameters
    pub fn with_config(expected_elements: usize, fp_rate: f64) -> Self {
        Self {
            blooms: RwLock::new(HashMap::new()),
            expected_elements,
            fp_rate,
        }
    }

    /// Register a bloom filter for a new segment.
    /// Called when a new GrowingSegment is created.
    pub fn register_segment(&self, collection: &str, segment_id: SegmentId) {
        let bloom = SegmentBloom::new(self.expected_elements, self.fp_rate);
        let mut blooms = self.blooms.write().unwrap();
        blooms.insert((collection.to_string(), segment_id), bloom);
    }

    /// Add a key to a segment's bloom filter.
    /// Called on entity insert.
    pub fn add_key(&self, collection: &str, segment_id: SegmentId, key: &[u8]) {
        let mut blooms = self.blooms.write().unwrap();
        if let Some(bloom) = blooms.get_mut(&(collection.to_string(), segment_id)) {
            bloom.add(key);
        }
    }

    /// Return segment IDs that *might* contain the given key.
    /// Segments whose bloom says "definitely not" are excluded.
    pub fn candidate_segments(&self, collection: &str, key: &[u8]) -> Vec<SegmentId> {
        let blooms = self.blooms.read().unwrap();
        blooms
            .iter()
            .filter_map(|((coll, seg_id), bloom)| {
                if coll == collection && bloom.might_contain(key) {
                    Some(*seg_id)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Check if a specific segment might contain a key.
    /// Returns `true` if no bloom filter exists for the segment (conservative).
    pub fn might_contain(&self, collection: &str, segment_id: SegmentId, key: &[u8]) -> bool {
        let blooms = self.blooms.read().unwrap();
        match blooms.get(&(collection.to_string(), segment_id)) {
            Some(bloom) => bloom.might_contain(key),
            None => true, // No bloom → conservative, assume it might be there
        }
    }

    /// Freeze a segment's bloom filter (called on seal).
    pub fn freeze_segment(&self, collection: &str, segment_id: SegmentId) {
        let mut blooms = self.blooms.write().unwrap();
        if let Some(bloom) = blooms.get_mut(&(collection.to_string(), segment_id)) {
            bloom.freeze();
        }
    }

    /// Remove a segment's bloom filter (called on segment drop/archive).
    pub fn remove_segment(&self, collection: &str, segment_id: SegmentId) {
        let mut blooms = self.blooms.write().unwrap();
        blooms.remove(&(collection.to_string(), segment_id));
    }

    /// Merge two segments' bloom filters into a new one (for compaction).
    /// Returns `None` if either segment has no bloom or filters are incompatible.
    pub fn merge_segments(
        &self,
        collection: &str,
        seg_a: SegmentId,
        seg_b: SegmentId,
        new_seg_id: SegmentId,
    ) -> bool {
        let blooms = self.blooms.read().unwrap();
        let key_a = (collection.to_string(), seg_a);
        let key_b = (collection.to_string(), seg_b);

        let merged = match (blooms.get(&key_a), blooms.get(&key_b)) {
            (Some(a), Some(b)) => a.filter.merge(&b.filter),
            _ => return false,
        };

        drop(blooms);

        if let Some(merged_filter) = merged {
            let key_count = {
                let blooms = self.blooms.read().unwrap();
                let a_count = blooms.get(&key_a).map_or(0, |b| b.key_count);
                let b_count = blooms.get(&key_b).map_or(0, |b| b.key_count);
                a_count + b_count
            };

            let bloom = SegmentBloom {
                filter: merged_filter,
                key_count,
                frozen: true,
            };

            let mut blooms = self.blooms.write().unwrap();
            blooms.insert((collection.to_string(), new_seg_id), bloom);
            true
        } else {
            false
        }
    }

    /// Get statistics about the registry
    pub fn stats(&self) -> BloomRegistryStats {
        let blooms = self.blooms.read().unwrap();
        let mut total_memory = 0;
        let mut total_keys = 0;
        let mut segment_count = 0;

        for bloom in blooms.values() {
            total_memory += bloom.memory_bytes();
            total_keys += bloom.key_count;
            segment_count += 1;
        }

        BloomRegistryStats {
            segment_count,
            total_keys,
            total_memory_bytes: total_memory,
        }
    }
}

impl Default for BloomFilterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for the bloom filter registry
#[derive(Debug, Clone)]
pub struct BloomRegistryStats {
    /// Number of segments with bloom filters
    pub segment_count: usize,
    /// Total keys across all blooms
    pub total_keys: usize,
    /// Total memory usage in bytes
    pub total_memory_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_registry_basic() {
        let registry = BloomFilterRegistry::new();
        registry.register_segment("users", 1);

        registry.add_key("users", 1, b"alice");
        registry.add_key("users", 1, b"bob");

        assert!(registry.might_contain("users", 1, b"alice"));
        assert!(registry.might_contain("users", 1, b"bob"));
        assert!(!registry.might_contain("users", 1, b"charlie"));
    }

    #[test]
    fn test_candidate_segments() {
        let registry = BloomFilterRegistry::with_config(100, 0.01);
        registry.register_segment("users", 1);
        registry.register_segment("users", 2);
        registry.register_segment("users", 3);

        registry.add_key("users", 1, b"alice");
        registry.add_key("users", 2, b"bob");
        registry.add_key("users", 3, b"charlie");

        let candidates = registry.candidate_segments("users", b"alice");
        assert!(candidates.contains(&1));
        // segments 2 and 3 should (almost certainly) not contain "alice"
        // but we can't guarantee no false positives in a bloom filter test
    }

    #[test]
    fn test_freeze_segment() {
        let registry = BloomFilterRegistry::new();
        registry.register_segment("data", 1);
        registry.add_key("data", 1, b"before_freeze");

        registry.freeze_segment("data", 1);

        // After freeze, adding keys should be a no-op
        registry.add_key("data", 1, b"after_freeze");
        // "before_freeze" should still be found
        assert!(registry.might_contain("data", 1, b"before_freeze"));
    }

    #[test]
    fn test_merge_segments() {
        let registry = BloomFilterRegistry::with_config(100, 0.01);
        registry.register_segment("data", 1);
        registry.register_segment("data", 2);

        registry.add_key("data", 1, b"from_seg1");
        registry.add_key("data", 2, b"from_seg2");

        assert!(registry.merge_segments("data", 1, 2, 3));

        // Merged bloom should contain keys from both
        assert!(registry.might_contain("data", 3, b"from_seg1"));
        assert!(registry.might_contain("data", 3, b"from_seg2"));
    }

    #[test]
    fn test_remove_segment() {
        let registry = BloomFilterRegistry::new();
        registry.register_segment("data", 1);
        registry.add_key("data", 1, b"key");

        assert!(registry.might_contain("data", 1, b"key"));

        registry.remove_segment("data", 1);
        // After removal, conservative default returns true (no bloom = might exist)
        assert!(registry.might_contain("data", 1, b"key"));
    }

    #[test]
    fn test_stats() {
        let registry = BloomFilterRegistry::with_config(100, 0.01);
        registry.register_segment("a", 1);
        registry.register_segment("b", 2);

        registry.add_key("a", 1, b"x");
        registry.add_key("a", 1, b"y");
        registry.add_key("b", 2, b"z");

        let stats = registry.stats();
        assert_eq!(stats.segment_count, 2);
        assert_eq!(stats.total_keys, 3);
        assert!(stats.total_memory_bytes > 0);
    }
}
