//! Owned resident index candidates and intersection workspaces.

use super::*;
use crate::storage::unified::hash_index::HashCandidates;
use crate::{RedDBError, RedDBResult};
use std::collections::HashSet;
use std::ops::Bound;

pub(crate) struct CandidateSet {
    ids: HashSet<u64>,
    _owner: Option<Box<dyn Send + Sync>>,
}

impl std::ops::Deref for CandidateSet {
    type Target = HashSet<u64>;
    fn deref(&self) -> &Self::Target {
        &self.ids
    }
}

/// Keep the full EntityId. The old u32 TID cast could alias unrelated rows.
pub(crate) struct CandidateBitmap {
    ids: roaring::RoaringTreemap,
    _owner: Option<Box<dyn Send + Sync>>,
}

impl CandidateBitmap {
    pub(crate) fn len(&self) -> u64 {
        self.ids.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
    pub(crate) fn intersect_with(&mut self, other: &Self) {
        self.ids &= &other.ids;
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = u64> + Clone + '_ {
        self.ids.iter()
    }
}

fn read_error(error: impl std::fmt::Display) -> RedDBError {
    RedDBError::Internal(error.to_string())
}

fn size_overflow() -> RedDBError {
    read_error("index candidate allocation size overflow")
}

impl IndexStore {
    pub(crate) fn candidate_workspace(
        &self,
        bytes: usize,
    ) -> RedDBResult<Option<Box<dyn Send + Sync>>> {
        self.hash.sample_candidate_memory().map_err(read_error)?;
        self.hash
            .reserve_candidate_memory(bytes)
            .map_err(read_error)
    }

    /// The caller samples before acquiring index/storage locks. Counting and
    /// copying use the same immutable iterator while the postings are frozen.
    fn collect_candidates_from_sample<I>(&self, ids: I) -> RedDBResult<HashCandidates>
    where
        I: Iterator<Item = EntityId> + Clone,
    {
        let count = ids
            .clone()
            .try_fold(0usize, |n, _| n.checked_add(1).ok_or_else(size_overflow))?;
        if count == 0 {
            return Ok(Vec::new().into());
        }
        let bytes = count
            .checked_mul(std::mem::size_of::<EntityId>())
            .ok_or_else(size_overflow)?;
        let owner = self
            .hash
            .reserve_candidate_memory(bytes)
            .map_err(read_error)?;
        let mut result = Vec::new();
        result.try_reserve_exact(count).map_err(read_error)?;
        result.extend(ids);
        Ok(match owner {
            Some(owner) => HashCandidates::owned(result, owner),
            None => result.into(),
        })
    }

    pub(crate) fn collect_candidates<I>(&self, ids: I) -> RedDBResult<HashCandidates>
    where
        I: Iterator<Item = EntityId> + Clone,
    {
        self.hash.sample_candidate_memory().map_err(read_error)?;
        self.collect_candidates_from_sample(ids)
    }

    pub(crate) fn candidate_set<I>(&self, ids: I) -> RedDBResult<CandidateSet>
    where
        I: Iterator<Item = u64> + Clone,
    {
        let count = ids.clone().count();
        // std/hashbrown: power-of-two buckets, <=7/8 load, 8-byte value and
        // one control byte per bucket. Include rounding, control tail/alignment.
        let bytes = count
            .checked_mul(32)
            .and_then(|n| n.checked_add(64))
            .ok_or_else(size_overflow)?;
        let owner = self.candidate_workspace(bytes)?;
        let mut set = HashSet::new();
        set.try_reserve(count).map_err(read_error)?;
        set.extend(ids);
        Ok(CandidateSet {
            ids: set,
            _owner: owner,
        })
    }

    pub(crate) fn candidate_bitmap(&self, mut ids: HashCandidates) -> RedDBResult<CandidateBitmap> {
        ids.as_mut_slice().sort_unstable_by_key(|id| id.raw());
        let bytes = bitmap_workspace_bytes(&ids)?;
        let owner = self.candidate_workspace(bytes)?;
        let mut bitmap = roaring::RoaringTreemap::new();
        for id in ids.iter() {
            bitmap.insert(id.raw());
        }
        Ok(CandidateBitmap {
            ids: bitmap,
            _owner: owner,
        })
    }

    pub(crate) fn sorted_range_candidates(
        &self,
        collection: &str,
        column: &str,
        bounds: (Bound<CanonicalKey>, Bound<CanonicalKey>),
        limit: usize,
        same_family: bool,
        filter: Option<&HashSet<u64>>,
    ) -> RedDBResult<Option<HashCandidates>> {
        self.hash.sample_candidate_memory().map_err(read_error)?;
        let guard = self.sorted.indices.read();
        let Some(index) = guard.get(&(collection.to_owned(), column.to_owned())) else {
            return Ok(None);
        };
        let (lower, upper) = (&bounds.0, &bounds.1);
        let key = match (lower, upper) {
            (Included(key) | Excluded(key), _) | (_, Included(key) | Excluded(key)) => key,
            _ => return Ok(None),
        };
        if !same_family && !index.supports_range_key(key) {
            return Ok(None);
        }
        if let (Included(low) | Excluded(low), Included(high) | Excluded(high)) = (lower, upper) {
            if low.family() != high.family() {
                return Ok(None);
            }
            if low > high
                || (low == high && (matches!(lower, Excluded(_)) || matches!(upper, Excluded(_))))
            {
                return Ok(Some(Vec::new().into()));
            }
        }
        let ids = index
            .entries
            .range(bounds)
            .flat_map(|(_, ids)| ids.iter().copied())
            .filter(|id| filter.is_none_or(|set| set.contains(&id.raw())))
            .take(limit);
        self.collect_candidates_from_sample(ids).map(Some)
    }

    pub(crate) fn sorted_in_candidates(
        &self,
        collection: &str,
        column: &str,
        mut keys: Vec<CanonicalKey>,
        limit: usize,
        filter: Option<&HashSet<u64>>,
    ) -> RedDBResult<Option<HashCandidates>> {
        keys.sort_unstable();
        keys.dedup();
        self.hash.sample_candidate_memory().map_err(read_error)?;
        let guard = self.sorted.indices.read();
        let Some(index) = guard.get(&(collection.to_owned(), column.to_owned())) else {
            return Ok(None);
        };
        let ids = keys
            .iter()
            .filter_map(|key| index.entries.get(key))
            .flat_map(|ids| ids.iter().copied())
            .filter(|id| filter.is_none_or(|set| set.contains(&id.raw())))
            .take(limit);
        self.collect_candidates_from_sample(ids).map(Some)
    }

    pub(crate) fn composite_range_candidates(
        &self,
        collection: &str,
        columns: &[String],
        prefix: &[CanonicalKey],
        low: CanonicalKey,
        high: CanonicalKey,
        limit: usize,
    ) -> RedDBResult<Option<HashCandidates>> {
        self.hash.sample_candidate_memory().map_err(read_error)?;
        let guard = self.sorted.composite.read();
        let Some(index) = guard.get(&(collection.to_owned(), columns.to_vec())) else {
            return Ok(None);
        };
        if limit == 0 || low > high {
            return Ok(Some(Vec::new().into()));
        }
        let mut lower = prefix.to_vec();
        lower.push(low);
        let mut upper = prefix.to_vec();
        upper.push(high);
        let ids = index
            .entries
            .range(lower..=upper)
            .flat_map(|(_, ids)| ids.iter().copied())
            .take(limit);
        self.collect_candidates_from_sample(ids).map(Some)
    }
}

/// roaring 0.11: a 16-bit container uses a geometrically grown u16 array or
/// an 8-KiB bitmap. Budget three payload capacities for array growth/conversion
/// and intersection scratch. Container Vec growth costs <=128 bytes/container;
/// each high-32 partition also reserves its BTree node, minimum Vec capacity and
/// the temporary removal-key list used by RoaringTreemap's in-place AND.
fn bitmap_workspace_bytes(ids: &[EntityId]) -> RedDBResult<usize> {
    let mut bytes = 0usize;
    let mut start = 0;
    let mut previous_partition = None;
    while start < ids.len() {
        let group = ids[start].raw() >> 16;
        let partition = ids[start].raw() >> 32;
        if previous_partition != Some(partition) {
            bytes = bytes.checked_add(1024).ok_or_else(size_overflow)?;
            previous_partition = Some(partition);
        }
        let mut end = start + 1;
        while end < ids.len() && ids[end].raw() >> 16 == group {
            end += 1;
        }
        let capacity = (end - start).clamp(4, 4096).next_power_of_two();
        bytes = bytes
            .checked_add(128 + 3 * capacity * 2)
            .ok_or_else(size_overflow)?;
        start = end;
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::unified::hash_index::{HashCandidateAdmission, HashIndexError};
    use std::sync::{Arc, Weak};

    struct ReplaceDuringSample {
        store: Weak<IndexStore>,
    }
    impl HashCandidateAdmission for ReplaceDuringSample {
        fn sample(&self) -> Result<(), HashIndexError> {
            let store = self.store.upgrade().unwrap();
            let mut indices = store
                .sorted
                .indices
                .try_write()
                .expect("sample must run outside index read lock");
            let mut replacement = SortedColumnIndex::new();
            replacement.insert(
                CanonicalKey::Signed(CanonicalKeyFamily::Integer, 1),
                EntityId::new(99),
            );
            indices.insert(("t".into(), "a".into()), replacement);
            Ok(())
        }
        fn reserve(&self, bytes: usize) -> Result<Box<dyn Send + Sync>, HashIndexError> {
            assert_eq!(bytes, std::mem::size_of::<EntityId>());
            assert!(
                self.store
                    .upgrade()
                    .unwrap()
                    .sorted
                    .indices
                    .try_write()
                    .is_none(),
                "freeze the selected generation until copying completes"
            );
            Ok(Box::new(()))
        }
    }

    #[test]
    fn sorted_candidate_sampling_is_unlocked_and_selects_the_current_generation() {
        let store = Arc::new(IndexStore::new());
        store
            .hash
            .install_candidate_admission(Arc::new(ReplaceDuringSample {
                store: Arc::downgrade(&store),
            }));
        let mut old = SortedColumnIndex::new();
        old.insert(
            CanonicalKey::Signed(CanonicalKeyFamily::Integer, 1),
            EntityId::new(1),
        );
        store
            .sorted
            .indices
            .write()
            .insert(("t".into(), "a".into()), old);
        let key = CanonicalKey::Signed(CanonicalKeyFamily::Integer, 1);
        let ids = store
            .sorted_range_candidates(
                "t",
                "a",
                (Included(key.clone()), Included(key)),
                usize::MAX,
                false,
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(ids, vec![EntityId::new(99)]);
    }
}
