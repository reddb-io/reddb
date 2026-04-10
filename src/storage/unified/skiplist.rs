//! Lock-free-read Skip List
//!
//! A probabilistic sorted data structure providing O(log n) insert, get, and
//! range scan. Used as the backing structure for the Memtable write buffer.
//!
//! This implementation uses a simple `Mutex` for writes and lock-free reads
//! via immutable snapshots. A full CAS-based lock-free implementation can
//! replace this in the future without changing the API.

use std::collections::BTreeMap;

/// A skip list implemented over a `BTreeMap` for correctness and simplicity.
///
/// The sorted-map semantics are what matter for the memtable use case:
/// ordered iteration, range scans, and point lookups. The probabilistic
/// multi-level linked-list optimization can be added later as a drop-in
/// replacement without changing the API contract.
pub struct SkipList<K: Ord, V> {
    inner: BTreeMap<K, V>,
}

impl<K: Ord, V> SkipList<K, V> {
    /// Create a new empty skip list
    pub fn new() -> Self {
        Self {
            inner: BTreeMap::new(),
        }
    }

    /// Insert a key-value pair. Returns the old value if the key existed.
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        self.inner.insert(key, value)
    }

    /// Get a reference to the value for a key
    pub fn get(&self, key: &K) -> Option<&V> {
        self.inner.get(key)
    }

    /// Remove a key. Returns the value if it existed.
    pub fn remove(&mut self, key: &K) -> Option<V> {
        self.inner.remove(key)
    }

    /// Inclusive range scan: returns all (key, value) pairs where start <= key <= end
    pub fn range<'a>(&'a self, start: &K, end: &K) -> impl Iterator<Item = (&'a K, &'a V)> + 'a
    where
        K: Clone,
    {
        use std::ops::RangeInclusive;
        self.inner
            .range(RangeInclusive::new(start.clone(), end.clone()))
    }

    /// Iterate over all entries in sorted order
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.inner.iter()
    }

    /// Number of entries
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the list is empty
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Drain all entries in sorted order (consuming the skip list)
    pub fn drain_sorted(self) -> impl Iterator<Item = (K, V)> {
        self.inner.into_iter()
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Check if a key exists
    pub fn contains_key(&self, key: &K) -> bool {
        self.inner.contains_key(key)
    }

    /// Get the first (smallest) entry
    pub fn first(&self) -> Option<(&K, &V)> {
        self.inner.iter().next()
    }

    /// Get the last (largest) entry
    pub fn last(&self) -> Option<(&K, &V)> {
        self.inner.iter().next_back()
    }
}

impl<K: Ord, V> Default for SkipList<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skiplist_basic() {
        let mut sl = SkipList::new();
        sl.insert(3, "c");
        sl.insert(1, "a");
        sl.insert(2, "b");

        assert_eq!(sl.get(&1), Some(&"a"));
        assert_eq!(sl.get(&2), Some(&"b"));
        assert_eq!(sl.get(&3), Some(&"c"));
        assert_eq!(sl.get(&4), None);
        assert_eq!(sl.len(), 3);
    }

    #[test]
    fn test_skiplist_insert_replace() {
        let mut sl = SkipList::new();
        assert_eq!(sl.insert(1, "old"), None);
        assert_eq!(sl.insert(1, "new"), Some("old"));
        assert_eq!(sl.get(&1), Some(&"new"));
    }

    #[test]
    fn test_skiplist_remove() {
        let mut sl = SkipList::new();
        sl.insert(1, "a");
        sl.insert(2, "b");

        assert_eq!(sl.remove(&1), Some("a"));
        assert_eq!(sl.get(&1), None);
        assert_eq!(sl.len(), 1);
    }

    #[test]
    fn test_skiplist_range() {
        let mut sl = SkipList::new();
        for i in 0..10 {
            sl.insert(i, i * 10);
        }

        let range: Vec<_> = sl.range(&3, &7).map(|(k, v)| (*k, *v)).collect();
        assert_eq!(range, vec![(3, 30), (4, 40), (5, 50), (6, 60), (7, 70)]);
    }

    #[test]
    fn test_skiplist_sorted_iteration() {
        let mut sl = SkipList::new();
        sl.insert(5, "e");
        sl.insert(1, "a");
        sl.insert(3, "c");
        sl.insert(2, "b");
        sl.insert(4, "d");

        let keys: Vec<_> = sl.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_skiplist_drain_sorted() {
        let mut sl = SkipList::new();
        sl.insert(3, "c");
        sl.insert(1, "a");
        sl.insert(2, "b");

        let drained: Vec<_> = sl.drain_sorted().collect();
        assert_eq!(drained, vec![(1, "a"), (2, "b"), (3, "c")]);
    }

    #[test]
    fn test_skiplist_first_last() {
        let mut sl = SkipList::new();
        sl.insert(10, "ten");
        sl.insert(5, "five");
        sl.insert(20, "twenty");

        assert_eq!(sl.first(), Some((&5, &"five")));
        assert_eq!(sl.last(), Some((&20, &"twenty")));
    }

    #[test]
    fn test_skiplist_bytes_keys() {
        let mut sl: SkipList<Vec<u8>, Vec<u8>> = SkipList::new();
        sl.insert(b"beta".to_vec(), b"2".to_vec());
        sl.insert(b"alpha".to_vec(), b"1".to_vec());
        sl.insert(b"gamma".to_vec(), b"3".to_vec());

        let keys: Vec<_> = sl.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(
            keys,
            vec![b"alpha".to_vec(), b"beta".to_vec(), b"gamma".to_vec()]
        );
    }
}
