//! Fixed-size circular page ring used by [`super::strategy::BufferAccessStrategy`].
//!
//! A `BufferRing` is a small, isolated cache that recycles slots in a
//! clock-style hand sweep. It exists specifically so that sequential
//! scans, bulk reads, and bulk writes do **not** populate the main
//! SIEVE pool — see `src/storage/cache/README.md` § Invariant 4.
//!
//! Unlike [`super::sieve::PageCache`], the ring has no pin counts, no
//! visited bits, and no eviction policy beyond "the slot at the hand
//! is the next victim." That is the entire point — sequential scans
//! visit each page exactly once, so any policy beyond FIFO is wasted
//! work.
//!
//! The ring is read-and-written *separately* from the main pool. A
//! page in the ring does not appear in the main pool, and vice versa.
//! Callers that want main-pool semantics simply do not pass a strategy.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

/// Recover from a poisoned read lock. Mirrors the convention used by
/// [`super::sieve`] — poisoning is treated as a non-fatal hint that a
/// previous writer panicked while holding the lock.
fn read_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|p| p.into_inner())
}

fn write_lock<T>(lock: &RwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|p| p.into_inner())
}

/// Fixed-capacity circular cache.
///
/// Inserting into a full ring evicts the slot at the current hand
/// position and returns the evicted `(key, value)` so the caller can
/// flush it (e.g. through the pager's double-write buffer for
/// `BulkWrite` strategies).
pub struct BufferRing<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    capacity: usize,
    slots: RwLock<Vec<Option<(K, V)>>>,
    hand: AtomicUsize,
    map: RwLock<HashMap<K, usize>>,
}

impl<K, V> BufferRing<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    /// Create a new ring with the given fixed capacity. `capacity` is
    /// clamped to at least 1.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            slots: RwLock::new(vec![None; capacity]),
            hand: AtomicUsize::new(0),
            map: RwLock::new(HashMap::with_capacity(capacity)),
        }
    }

    /// Configured ring capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of occupied slots.
    pub fn len(&self) -> usize {
        read_lock(&self.map).len()
    }

    /// Whether the ring has zero occupied slots.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Look up a key. Does NOT update any recency / visited state.
    pub fn get(&self, key: &K) -> Option<V> {
        let map = read_lock(&self.map);
        let idx = *map.get(key)?;
        drop(map);
        let slots = read_lock(&self.slots);
        slots
            .get(idx)
            .and_then(|s| s.as_ref().map(|(_, v)| v.clone()))
    }

    /// Insert a `(key, value)` pair. If the ring is full, the slot at
    /// the current hand position is evicted and the hand advances.
    /// Returns the evicted `(key, value)` if any.
    ///
    /// If `key` is already present in the ring, its slot is updated in
    /// place and `None` is returned.
    pub fn insert(&self, key: K, value: V) -> Option<(K, V)> {
        // Update-in-place fast path.
        {
            let map = read_lock(&self.map);
            if let Some(&idx) = map.get(&key) {
                drop(map);
                let mut slots = write_lock(&self.slots);
                if let Some(slot) = slots.get_mut(idx) {
                    *slot = Some((key, value));
                }
                return None;
            }
        }

        // Find an empty slot first (ring not yet full).
        let mut map = write_lock(&self.map);
        let mut slots = write_lock(&self.slots);
        if map.len() < self.capacity {
            // Find first empty slot starting from the hand.
            let start = self.hand.load(Ordering::Relaxed) % self.capacity;
            for offset in 0..self.capacity {
                let idx = (start + offset) % self.capacity;
                if slots[idx].is_none() {
                    slots[idx] = Some((key.clone(), value));
                    map.insert(key, idx);
                    self.hand
                        .store((idx + 1) % self.capacity, Ordering::Relaxed);
                    return None;
                }
            }
        }

        // Ring full — evict the slot at the hand and replace.
        let victim_idx = self.hand.load(Ordering::Relaxed) % self.capacity;
        let evicted = slots[victim_idx].take();
        if let Some((ref evicted_key, _)) = evicted {
            map.remove(evicted_key);
        }
        slots[victim_idx] = Some((key.clone(), value));
        map.insert(key, victim_idx);
        self.hand
            .store((victim_idx + 1) % self.capacity, Ordering::Relaxed);
        evicted
    }

    /// Drop every slot. Used by `clear_cursors`-style cleanup paths.
    pub fn clear(&self) {
        let mut slots = write_lock(&self.slots);
        let mut map = write_lock(&self.map);
        for slot in slots.iter_mut() {
            *slot = None;
        }
        map.clear();
        self.hand.store(0, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_ring_returns_none() {
        let ring: BufferRing<u32, &str> = BufferRing::new(4);
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
        assert!(ring.get(&1).is_none());
    }

    #[test]
    fn insert_into_empty_slot_no_eviction() {
        let ring: BufferRing<u32, String> = BufferRing::new(4);
        assert!(ring.insert(1, "a".to_string()).is_none());
        assert!(ring.insert(2, "b".to_string()).is_none());
        assert_eq!(ring.get(&1), Some("a".to_string()));
        assert_eq!(ring.get(&2), Some("b".to_string()));
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn ring_recycles_at_capacity() {
        // Insert 20 pages into a 16-slot ring; first 4 must be evicted.
        let ring: BufferRing<u32, u32> = BufferRing::new(16);
        for i in 0..20 {
            let _ = ring.insert(i, i * 10);
        }
        assert_eq!(ring.len(), 16);
        // The first 4 (keys 0..4) should have been evicted by the hand
        // sweep that started at slot 0.
        for i in 0..4 {
            assert!(ring.get(&i).is_none(), "key {i} should be evicted");
        }
        // The last 16 must be present.
        for i in 4..20 {
            assert_eq!(ring.get(&i), Some(i * 10), "key {i} should be present");
        }
    }

    #[test]
    fn insert_returns_evicted_pair_when_full() {
        let ring: BufferRing<u32, &str> = BufferRing::new(2);
        assert!(ring.insert(1, "a").is_none());
        assert!(ring.insert(2, "b").is_none());
        // Now full; next insert evicts.
        let evicted = ring.insert(3, "c");
        assert!(evicted.is_some());
        let (ek, ev) = evicted.unwrap();
        // Hand was at 0 after first insert (slot 0 occupied), so the
        // first eviction comes from slot 0 = key 1.
        assert_eq!(ek, 1);
        assert_eq!(ev, "a");
    }

    #[test]
    fn update_in_place_does_not_evict() {
        let ring: BufferRing<u32, &str> = BufferRing::new(2);
        ring.insert(1, "a");
        ring.insert(2, "b");
        // Re-insert key 1 — should overwrite, not evict.
        let evicted = ring.insert(1, "A");
        assert!(evicted.is_none());
        assert_eq!(ring.get(&1), Some("A"));
        assert_eq!(ring.get(&2), Some("b"));
        assert_eq!(ring.len(), 2);
    }

    #[test]
    fn clear_drops_all_entries() {
        let ring: BufferRing<u32, &str> = BufferRing::new(4);
        ring.insert(1, "a");
        ring.insert(2, "b");
        ring.clear();
        assert!(ring.is_empty());
        assert!(ring.get(&1).is_none());
    }

    #[test]
    fn capacity_clamped_to_at_least_one() {
        let ring: BufferRing<u32, &str> = BufferRing::new(0);
        assert_eq!(ring.capacity(), 1);
        ring.insert(1, "a");
        // Inserting a second key evicts the first.
        let ev = ring.insert(2, "b");
        assert_eq!(ev, Some((1, "a")));
    }
}
