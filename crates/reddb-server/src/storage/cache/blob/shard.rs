use std::collections::HashSet;

use indexmap::IndexMap;

use super::cache::{BlobCacheHit, BlobCacheKey};
use super::entry::Entry;
use crate::storage::cache::extended_ttl::{EffectiveExpiry, ExpiryDecision, ExtendedTtlPolicy};

/// Blob Cache L1 shard.
///
/// Decision #218 documents this as the tiered-cache exception to the
/// page-cache SIEVE invariant: priority may bias victim choice, but the shard
/// still falls back to the normal `visited` sweep.
#[derive(Debug)]
pub(super) struct Shard {
    entries: IndexMap<BlobCacheKey, Entry>,
    l2_second_hit_markers: HashSet<BlobCacheKey>,
    slots: Vec<Option<BlobCacheKey>>,
    free_slots: Vec<usize>,
    hand: usize,
    pub(super) bytes: usize,
}

impl Shard {
    pub(super) fn new() -> Self {
        Self {
            entries: IndexMap::new(),
            l2_second_hit_markers: HashSet::new(),
            slots: Vec::new(),
            free_slots: Vec::new(),
            hand: 0,
            bytes: 0,
        }
    }

    pub(super) fn get(
        &mut self,
        key: &BlobCacheKey,
        now_ms: u64,
        namespace_generation: u64,
    ) -> Lookup {
        self.get_by_parts(&key.namespace, &key.key, now_ms, namespace_generation)
    }

    pub(super) fn get_by_parts(
        &mut self,
        namespace: &str,
        key: &str,
        now_ms: u64,
        namespace_generation: u64,
    ) -> Lookup {
        let borrowed = BlobCacheKey::borrowed(namespace, key);
        let remove = {
            let Some(entry) = self.entries.get_mut(&borrowed) else {
                return Lookup::Miss;
            };
            if entry.namespace_generation != namespace_generation {
                Some(RemovalReason::Stale)
            } else if !entry.extended.is_active() {
                if entry.is_expired_at(now_ms) {
                    Some(RemovalReason::Expired)
                } else {
                    entry.visited = true;
                    entry.last_access_unix_ms = now_ms;
                    return Lookup::Hit(entry.hit());
                }
            } else {
                #[cfg(test)]
                super::cache::EFFECTIVE_EXPIRY_COMPUTE_CALLS.with(|c| c.set(c.get() + 1));
                match EffectiveExpiry::compute(
                    entry.expires_at_unix_ms,
                    now_ms,
                    entry.last_access_unix_ms,
                    &entry.extended,
                ) {
                    ExpiryDecision::Fresh => {
                        entry.visited = true;
                        entry.last_access_unix_ms = now_ms;
                        return Lookup::Hit(entry.hit());
                    }
                    ExpiryDecision::Stale {
                        window_remaining_ms,
                    } => {
                        entry.visited = true;
                        entry.last_access_unix_ms = now_ms;
                        return Lookup::Hit(entry.hit_stale(window_remaining_ms));
                    }
                    ExpiryDecision::Expired => {
                        if entry.is_expired_at(now_ms) {
                            Some(RemovalReason::Expired)
                        } else {
                            Some(RemovalReason::IdleEvicted)
                        }
                    }
                }
            }
        };

        let key = BlobCacheKey::new(namespace, key);
        let removed = self.remove(&key).expect("entry exists");
        match remove.expect("removal reason") {
            RemovalReason::Expired => Lookup::Expired(removed),
            RemovalReason::IdleEvicted => Lookup::IdleEvicted(removed),
            RemovalReason::Stale => Lookup::Stale(removed),
        }
    }

    pub(super) fn contains(
        &mut self,
        key: &BlobCacheKey,
        now_ms: u64,
        namespace_generation: u64,
    ) -> Lookup {
        self.contains_by_parts(&key.namespace, &key.key, now_ms, namespace_generation)
    }

    pub(super) fn contains_by_parts(
        &mut self,
        namespace: &str,
        key: &str,
        now_ms: u64,
        namespace_generation: u64,
    ) -> Lookup {
        let borrowed = BlobCacheKey::borrowed(namespace, key);
        let remove = {
            let Some(entry) = self.entries.get_mut(&borrowed) else {
                return Lookup::Miss;
            };
            if entry.namespace_generation != namespace_generation {
                Some(RemovalReason::Stale)
            } else if entry.is_expired_at(now_ms) {
                Some(RemovalReason::Expired)
            } else {
                entry.visited = true;
                return Lookup::Present;
            }
        };

        let key = BlobCacheKey::new(namespace, key);
        let removed = self.remove(&key).expect("entry exists");
        match remove.expect("removal reason") {
            RemovalReason::Expired => Lookup::Expired(removed),
            RemovalReason::Stale => Lookup::Stale(removed),
            RemovalReason::IdleEvicted => unreachable!("contains does not evaluate idle TTL"),
        }
    }

    pub(super) fn existing_version(
        &self,
        key: &BlobCacheKey,
        namespace_generation: u64,
    ) -> Option<u64> {
        self.entries.get(key).and_then(|entry| {
            if entry.namespace_generation == namespace_generation {
                entry.version
            } else {
                None
            }
        })
    }

    pub(super) fn insert(&mut self, key: BlobCacheKey, mut entry: Entry) -> InsertOutcome {
        self.l2_second_hit_markers.remove(&key);
        let old_entry = if let Some(old) = self.entries.shift_remove(&key) {
            let slot_index = old.slot_index;
            self.bytes = self.bytes.saturating_sub(old.size);
            entry.slot_index = slot_index;
            Some(old)
        } else {
            let slot_index = self.free_slots.pop().unwrap_or_else(|| {
                self.slots.push(None);
                self.slots.len() - 1
            });
            entry.slot_index = slot_index;
            None
        };

        self.bytes += entry.size;
        self.slots[entry.slot_index] = Some(key.clone());
        self.entries.insert(key.clone(), entry);
        InsertOutcome {
            old_entry,
            admitted: true,
        }
    }

    pub(super) fn evict_one(&mut self) -> Option<(BlobCacheKey, Entry)> {
        if self.entries.is_empty() {
            self.hand = 0;
            return None;
        }
        let max_sweeps = self.slots.len().saturating_mul(2).max(1);
        for _ in 0..max_sweeps {
            if self.entries.is_empty() {
                self.hand = 0;
                return None;
            }
            if self.hand >= self.slots.len() {
                self.hand = 0;
            }
            let Some(candidate) = self.slots[self.hand].clone() else {
                self.advance_hand();
                continue;
            };
            let Some(entry) = self.entries.get(&candidate) else {
                self.slots[self.hand] = None;
                self.free_slots.push(self.hand);
                self.advance_hand();
                continue;
            };
            if entry.visited {
                if let Some(entry) = self.entries.get_mut(&candidate) {
                    entry.visited = false;
                }
                self.advance_hand();
                continue;
            }

            if self.has_lower_priority_unvisited(entry.priority) {
                self.advance_hand();
                continue;
            }

            let removed = self
                .entries
                .shift_remove(&candidate)
                .expect("candidate exists");
            self.bytes = self.bytes.saturating_sub(removed.size);
            self.slots[self.hand] = None;
            self.free_slots.push(self.hand);
            self.advance_hand();
            return Some((candidate, removed));
        }
        None
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn remove(&mut self, key: &BlobCacheKey) -> Option<Entry> {
        self.l2_second_hit_markers.remove(key);
        let removed = self.entries.shift_remove(key)?;
        self.bytes = self.bytes.saturating_sub(removed.size);
        self.slots[removed.slot_index] = None;
        self.free_slots.push(removed.slot_index);
        if self.entries.is_empty() || self.hand >= self.slots.len() {
            self.hand = 0;
        }
        Some(removed)
    }

    pub(super) fn clear_l2_hit_marker(&mut self, key: &BlobCacheKey) {
        self.l2_second_hit_markers.remove(key);
    }

    pub(super) fn l2_hit_should_promote_on_second_hit(&mut self, key: &BlobCacheKey) -> bool {
        if self.l2_second_hit_markers.remove(key) {
            true
        } else {
            self.l2_second_hit_markers.insert(key.clone());
            false
        }
    }

    pub(super) fn keys_matching(
        &self,
        mut predicate: impl FnMut(&BlobCacheKey) -> bool,
    ) -> Vec<BlobCacheKey> {
        self.entries
            .keys()
            .filter(|key| predicate(key))
            .cloned()
            .collect()
    }

    pub(super) fn entry_has_any_tag(&self, key: &BlobCacheKey, labels: &HashSet<String>) -> bool {
        self.entries
            .get(key)
            .is_some_and(|entry| labels.iter().any(|label| entry.tags.contains(label)))
    }

    pub(super) fn entry_has_any_dependency(
        &self,
        key: &BlobCacheKey,
        labels: &HashSet<String>,
    ) -> bool {
        self.entries.get(key).is_some_and(|entry| {
            labels
                .iter()
                .any(|label| entry.dependencies.contains(label))
        })
    }

    fn has_lower_priority_unvisited(&self, priority: u8) -> bool {
        self.entries
            .values()
            .any(|entry| !entry.visited && entry.priority < priority)
    }

    fn advance_hand(&mut self) {
        if self.slots.is_empty() {
            self.hand = 0;
        } else {
            self.hand = (self.hand + 1) % self.slots.len();
        }
    }
}

pub(super) enum Lookup {
    Hit(BlobCacheHit),
    Present,
    Expired(Entry),
    /// Entry was killed by the idle TTL gate of an `ExtendedTtlPolicy`.
    /// Distinguished from [`Expired`] so the cache-level counter for
    /// idle evictions stays separate from hard-TTL expirations.
    IdleEvicted(Entry),
    Stale(Entry),
    Miss,
}

enum RemovalReason {
    Expired,
    IdleEvicted,
    Stale,
}

pub(super) struct InsertOutcome {
    pub(super) old_entry: Option<Entry>,
    pub(super) admitted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::cache::blob::{BlobCachePolicy, CachePresence};

    fn key(name: &str) -> BlobCacheKey {
        BlobCacheKey::new("n", name)
    }

    fn entry(bytes: &[u8], priority: u8) -> Entry {
        Entry::new(
            bytes.to_vec(),
            Default::default(),
            Default::default(),
            Default::default(),
            BlobCachePolicy::default().priority(priority),
            0,
            1_000,
            "n",
            "k",
        )
    }

    #[test]
    fn insert_replaces_existing_key_and_tracks_bytes() {
        let mut shard = Shard::new();
        let k = key("same");

        let first = shard.insert(k.clone(), entry(b"abc", 128));
        assert!(first.old_entry.is_none());
        assert_eq!(shard.bytes, 3);

        let second = shard.insert(k.clone(), entry(b"xy", 128));
        assert_eq!(second.old_entry.expect("old entry").size, 3);
        assert_eq!(shard.len(), 1);
        assert_eq!(shard.bytes, 2);
        assert!(matches!(shard.contains(&k, 1_000, 0), Lookup::Present));
    }

    #[test]
    fn insert_replaces_existing_key_in_the_same_slot() {
        let mut shard = Shard::new();
        let k = key("same");

        shard.insert(k.clone(), entry(b"abc", 128));
        let first_slot = shard.entries.get(&k).expect("entry").slot_index;

        shard.insert(k.clone(), entry(b"xy", 128));

        let replaced = shard.entries.get(&k).expect("entry");
        assert_eq!(replaced.slot_index, first_slot);
        assert_eq!(shard.slots[first_slot].as_ref(), Some(&k));
    }

    #[test]
    fn remove_empties_entry_slot_without_shifting_survivors() {
        let mut shard = Shard::new();
        let a = key("a");
        let b = key("b");
        let c = key("c");
        shard.insert(a.clone(), entry(b"aa", 128));
        shard.insert(b.clone(), entry(b"bb", 128));
        shard.insert(c.clone(), entry(b"cc", 128));
        let b_slot = shard.entries.get(&b).expect("b").slot_index;
        let c_slot = shard.entries.get(&c).expect("c").slot_index;

        shard.remove(&b).expect("removed");

        assert!(shard.slots[b_slot].is_none());
        assert_eq!(shard.entries.get(&c).expect("c").slot_index, c_slot);
        assert_eq!(shard.slots[c_slot].as_ref(), Some(&c));
    }

    #[test]
    fn remove_keeps_hand_valid_after_current_slot() {
        let mut shard = Shard::new();
        let a = key("a");
        let b = key("b");
        shard.insert(a.clone(), entry(b"aa", 128));
        shard.insert(b.clone(), entry(b"bb", 128));

        let removed = shard.remove(&a).expect("removed");
        assert_eq!(removed.size, 2);
        assert_eq!(shard.len(), 1);
        assert_eq!(shard.bytes, 2);
        let evicted = shard.evict_one().expect("remaining entry evicted");
        assert_eq!(evicted.0, b);
    }

    #[test]
    fn priority_biases_eviction_before_falling_back_to_sieve() {
        let mut shard = Shard::new();
        let low = key("low");
        let high = key("high");
        shard.insert(low.clone(), entry(b"low", 1));
        shard.insert(high.clone(), entry(b"hi", 250));

        assert!(matches!(shard.contains(&high, 1_000, 0), Lookup::Present));
        let (evicted_key, _) = shard.evict_one().expect("victim");
        assert_eq!(evicted_key, low);
        assert!(matches!(shard.contains(&high, 1_000, 0), Lookup::Present));
    }

    #[test]
    fn stale_generation_removes_entry() {
        let mut shard = Shard::new();
        let k = key("stale");
        shard.insert(k.clone(), entry(b"v", 128));

        assert!(matches!(shard.get(&k, 1_000, 1), Lookup::Stale(_)));
        assert!(matches!(shard.get(&k, 1_000, 1), Lookup::Miss));
    }

    #[test]
    fn expired_entry_reports_absent_after_removal() {
        let mut shard = Shard::new();
        let k = key("ttl");
        let mut e = entry(b"v", 128);
        e.expires_at_unix_ms = Some(1_000);
        shard.insert(k.clone(), e);

        assert!(matches!(shard.get(&k, 1_000, 0), Lookup::Expired(_)));
        assert_eq!(
            CachePresence::from(matches!(shard.get(&k, 1_000, 0), Lookup::Hit(_))),
            CachePresence::Absent
        );
    }
}
