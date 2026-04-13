//! Unified index abstraction shared across RedDB data structures.
//!
//! Tables, graphs, vectors, timeseries, documents and queues each maintain
//! their own access structures today (btree, hash, bitmap, HNSW, inverted
//! lists, adjacency maps...). This module does not replace those concrete
//! implementations — it defines the cross-cutting traits and primitives so
//! the query planner, segment layer and diagnostics can treat them uniformly.
//!
//! # Components
//!
//! - [`IndexBase`]   — metadata, stats, bloom, lifecycle
//! - [`PointIndex`]  — key → values lookups
//! - [`RangeIndex`]  — ordered iteration over key ranges
//! - [`IndexStats`]  — cardinality/selectivity surfaced to the planner
//! - [`IndexKind`]   — enum of supported index families
//! - [`BloomSegment`] — reusable bloom header attachable to any segment
//!
//! Each concrete index (existing or future) can opt-in by implementing the
//! traits that match its access patterns. Cross-structure features such as
//! the hybrid executor, plan cache statistics and segment pruning consume the
//! traits, not concrete types.

pub mod bloom_segment;
pub mod stats;

pub use bloom_segment::{BloomSegment, BloomSegmentBuilder, HasBloom};
pub use stats::{IndexKind, IndexStats};

use std::fmt;

/// Error type emitted by index operations.
#[derive(Debug, Clone)]
pub enum IndexError {
    /// Key was not valid for this index (e.g. wrong type).
    InvalidKey(String),
    /// Value was not valid for this index.
    InvalidValue(String),
    /// Index is read-only / sealed.
    ReadOnly,
    /// Capacity exceeded.
    Full,
    /// Underlying storage error.
    Storage(String),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::InvalidKey(m) => write!(f, "invalid key: {m}"),
            IndexError::InvalidValue(m) => write!(f, "invalid value: {m}"),
            IndexError::ReadOnly => write!(f, "index is read-only"),
            IndexError::Full => write!(f, "index capacity exceeded"),
            IndexError::Storage(m) => write!(f, "storage error: {m}"),
        }
    }
}

impl std::error::Error for IndexError {}

/// Cross-cutting metadata every index exposes. Used by the planner for
/// cost estimation, by the segment layer for bloom-based pruning and by
/// diagnostics tooling.
pub trait IndexBase: Send + Sync {
    /// Human-readable name (e.g. "users.email", "graph.city_by_node").
    fn name(&self) -> &str;

    /// Index family (btree, hash, bitmap, hnsw, ...).
    fn kind(&self) -> IndexKind;

    /// Current statistics (cardinality, estimated selectivity, memory).
    fn stats(&self) -> IndexStats;

    /// Optional bloom filter for fast negative lookups. Cross-structure
    /// pruning relies on this.
    fn bloom(&self) -> Option<&crate::storage::primitives::BloomFilter> {
        None
    }

    /// Returns `true` iff the key is *guaranteed* to be absent from this
    /// index. Default implementation consults [`IndexBase::bloom`] and falls
    /// back to `false` when no bloom is available (meaning "don't know —
    /// caller must probe").
    ///
    /// Concrete indexes may override with tighter signals (e.g. zone map
    /// min/max for range indexes).
    fn definitely_absent(&self, key_bytes: &[u8]) -> bool {
        self.bloom()
            .map(|b| !b.contains(key_bytes))
            .unwrap_or(false)
    }
}

/// Point lookup access pattern. Implemented by hash, btree (as exact match),
/// bitmap, cuckoo, etc.
pub trait PointIndex<K: ?Sized, V>: IndexBase {
    /// Insert `value` under `key`. Multi-value semantics are index-specific.
    fn insert(&mut self, key: &K, value: V) -> Result<(), IndexError>;

    /// Remove all values under `key`. Returns number removed.
    fn remove(&mut self, key: &K) -> Result<usize, IndexError>;

    /// Look up every value associated with `key`.
    fn lookup(&self, key: &K) -> Vec<V>;

    /// Convenience: does the key exist at all?
    fn contains(&self, key: &K) -> bool {
        !self.lookup(key).is_empty()
    }
}

/// Range / ordered access pattern. Implemented by btree, skiplist, zone map.
pub trait RangeIndex<K: ?Sized, V>: PointIndex<K, V> {
    /// Iterate values whose key falls in `[start, end)`.
    /// `None` bounds are open (min/max).
    fn range(&self, start: Option<&K>, end: Option<&K>) -> Vec<(Vec<u8>, V)>;

    /// Total number of distinct keys.
    fn distinct_keys(&self) -> usize;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::primitives::BloomFilter;
    use std::collections::BTreeMap;

    /// Tiny in-memory btree to exercise the traits end-to-end.
    struct TestBTree {
        name: String,
        map: BTreeMap<Vec<u8>, Vec<u64>>,
        bloom: BloomFilter,
    }

    impl TestBTree {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                map: BTreeMap::new(),
                bloom: BloomFilter::with_capacity(1024, 0.01),
            }
        }
    }

    impl IndexBase for TestBTree {
        fn name(&self) -> &str {
            &self.name
        }
        fn kind(&self) -> IndexKind {
            IndexKind::BTree
        }
        fn stats(&self) -> IndexStats {
            IndexStats {
                entries: self.map.values().map(|v| v.len()).sum(),
                distinct_keys: self.map.len(),
                approx_bytes: 0,
                kind: IndexKind::BTree,
                has_bloom: true,
            }
        }
        fn bloom(&self) -> Option<&BloomFilter> {
            Some(&self.bloom)
        }
    }

    impl PointIndex<[u8], u64> for TestBTree {
        fn insert(&mut self, key: &[u8], value: u64) -> Result<(), IndexError> {
            self.bloom.insert(key);
            self.map.entry(key.to_vec()).or_default().push(value);
            Ok(())
        }
        fn remove(&mut self, key: &[u8]) -> Result<usize, IndexError> {
            Ok(self.map.remove(key).map(|v| v.len()).unwrap_or(0))
        }
        fn lookup(&self, key: &[u8]) -> Vec<u64> {
            self.map.get(key).cloned().unwrap_or_default()
        }
    }

    impl RangeIndex<[u8], u64> for TestBTree {
        fn range(&self, start: Option<&[u8]>, end: Option<&[u8]>) -> Vec<(Vec<u8>, u64)> {
            use std::ops::Bound;
            let lo = start.map(Bound::Included).unwrap_or(Bound::Unbounded);
            let hi = end.map(Bound::Excluded).unwrap_or(Bound::Unbounded);
            self.map
                .range::<[u8], _>((lo, hi))
                .flat_map(|(k, vs)| vs.iter().map(move |v| (k.clone(), *v)))
                .collect()
        }
        fn distinct_keys(&self) -> usize {
            self.map.len()
        }
    }

    #[test]
    fn point_index_roundtrip() {
        let mut idx = TestBTree::new("test");
        idx.insert(b"alpha", 1).unwrap();
        idx.insert(b"alpha", 2).unwrap();
        idx.insert(b"beta", 3).unwrap();

        assert_eq!(idx.lookup(b"alpha"), vec![1, 2]);
        assert!(idx.contains(b"beta"));
        assert!(!idx.contains(b"gamma"));
    }

    #[test]
    fn bloom_prunes_absent_keys() {
        let mut idx = TestBTree::new("test");
        idx.insert(b"alpha", 1).unwrap();
        // Bloom must never produce false negatives.
        assert!(!idx.definitely_absent(b"alpha"));
        // Random unknown key: bloom may say "absent" or "unknown".
        // Either way, probing must still return empty.
        assert!(idx.lookup(b"not-there").is_empty());
    }

    #[test]
    fn range_iteration() {
        let mut idx = TestBTree::new("test");
        for (i, k) in [b"a", b"b", b"c", b"d"].iter().enumerate() {
            idx.insert(*k, i as u64).unwrap();
        }
        let out = idx.range(Some(b"b"), Some(b"d"));
        let keys: Vec<&[u8]> = out.iter().map(|(k, _)| k.as_slice()).collect();
        assert_eq!(keys, vec![b"b".as_slice(), b"c".as_slice()]);
    }

    #[test]
    fn stats_surface_cardinality() {
        let mut idx = TestBTree::new("test");
        idx.insert(b"a", 1).unwrap();
        idx.insert(b"a", 2).unwrap();
        idx.insert(b"b", 3).unwrap();
        let s = idx.stats();
        assert_eq!(s.entries, 3);
        assert_eq!(s.distinct_keys, 2);
        assert!(s.has_bloom);
    }
}
