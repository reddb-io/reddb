//! Vector Metadata Storage
//!
//! Type-aware metadata storage for vectors, inspired by Chroma's design.
//! Supports efficient filtering on metadata during vector search.
//!
//! # Design
//!
//! - Metadata values are stored by type for efficient comparisons
//! - Inverted indexes enable fast filtering by metadata
//! - Supports rich filter operators (eq, ne, gt, gte, lt, lte, in, contains)

use std::collections::{BTreeMap, HashMap, HashSet};

use super::hnsw::NodeId;
use crate::storage::schema::{CanonicalKey, CanonicalKeyFamily};

// `MetadataValue` / `MetadataEntry` / `MetadataFilter` re-homed to the neutral
// keystone crate (ADR 0053, RQL Phase 2 S4b) so the canonical SQL AST
// (`VectorQuery.filter`) resolves them without a `reddb-server` edge. The
// server-side inverted index below consumes `metadata_value_to_canonical_key`
// from there. This shim keeps `storage::engine::vector_metadata::{...}` valid
// for existing call-sites.
pub use reddb_types::vector_metadata::{
    metadata_value_to_canonical_key, MetadataEntry, MetadataFilter, MetadataValue,
};

/// Inverted index for a single metadata key
#[derive(Debug, Clone, Default)]
struct KeyIndex {
    /// String value -> vector IDs
    string_index: HashMap<String, HashSet<NodeId>>,
    /// Integer value -> vector IDs
    integer_index: HashMap<i64, HashSet<NodeId>>,
    /// Boolean value -> vector IDs
    bool_index: HashMap<bool, HashSet<NodeId>>,
    /// Canonical ordered value -> vector IDs
    ordered_index: BTreeMap<CanonicalKey, HashSet<NodeId>>,
    /// Family seen in this key's metadata values. Mixed families disable range pushdown.
    range_family: Option<CanonicalKeyFamily>,
    has_mixed_families: bool,
    /// All vector IDs that have this key
    all_ids: HashSet<NodeId>,
}

impl KeyIndex {
    fn new() -> Self {
        Self::default()
    }

    fn insert(&mut self, id: NodeId, value: &MetadataValue) {
        self.all_ids.insert(id);
        match value {
            MetadataValue::String(s) => {
                self.string_index.entry(s.clone()).or_default().insert(id);
            }
            MetadataValue::Integer(i) => {
                self.integer_index.entry(*i).or_default().insert(id);
            }
            MetadataValue::Bool(b) => {
                self.bool_index.entry(*b).or_default().insert(id);
            }
            MetadataValue::Float(_) | MetadataValue::Null => {}
        }

        if let Some(key) = metadata_value_to_canonical_key(value) {
            match self.range_family {
                Some(existing) if existing != key.family() => self.has_mixed_families = true,
                None => self.range_family = Some(key.family()),
                _ => {}
            }
            self.ordered_index.entry(key).or_default().insert(id);
        }
    }

    fn remove(&mut self, id: NodeId, value: &MetadataValue) {
        self.all_ids.remove(&id);
        match value {
            MetadataValue::String(s) => {
                if let Some(ids) = self.string_index.get_mut(s) {
                    ids.remove(&id);
                }
            }
            MetadataValue::Integer(i) => {
                if let Some(ids) = self.integer_index.get_mut(i) {
                    ids.remove(&id);
                }
            }
            MetadataValue::Bool(b) => {
                if let Some(ids) = self.bool_index.get_mut(b) {
                    ids.remove(&id);
                }
            }
            _ => {}
        }

        if let Some(key) = metadata_value_to_canonical_key(value) {
            if let Some(ids) = self.ordered_index.get_mut(&key) {
                ids.remove(&id);
                if ids.is_empty() {
                    self.ordered_index.remove(&key);
                }
            }
        }
    }

    fn exact_match_ids(&self, value: &MetadataValue) -> Option<HashSet<NodeId>> {
        match value {
            MetadataValue::String(s) => Some(self.string_index.get(s).cloned().unwrap_or_default()),
            MetadataValue::Integer(i) => {
                Some(self.integer_index.get(i).cloned().unwrap_or_default())
            }
            MetadataValue::Bool(b) => Some(self.bool_index.get(b).cloned().unwrap_or_default()),
            MetadataValue::Null => Some(HashSet::new()),
            MetadataValue::Float(f) if f.is_nan() => Some(HashSet::new()),
            MetadataValue::Float(_) => metadata_value_to_canonical_key(value)
                .map(|key| self.ordered_index.get(&key).cloned().unwrap_or_default()),
        }
    }

    fn supports_range_key(&self, key: &CanonicalKey) -> bool {
        !self.has_mixed_families && self.range_family == Some(key.family())
    }

    fn range_match_ids(
        &self,
        value: &MetadataValue,
        op: MetadataRangeOp,
    ) -> Option<HashSet<NodeId>> {
        let key = metadata_value_to_canonical_key(value)?;
        if !self.supports_range_key(&key) {
            return None;
        }

        let mut out = HashSet::new();
        match op {
            MetadataRangeOp::Gt => {
                for ids in self
                    .ordered_index
                    .range((std::ops::Bound::Excluded(key), std::ops::Bound::Unbounded))
                    .map(|(_, ids)| ids)
                {
                    out.extend(ids.iter().copied());
                }
            }
            MetadataRangeOp::Gte => {
                for ids in self
                    .ordered_index
                    .range((std::ops::Bound::Included(key), std::ops::Bound::Unbounded))
                    .map(|(_, ids)| ids)
                {
                    out.extend(ids.iter().copied());
                }
            }
            MetadataRangeOp::Lt => {
                for ids in self
                    .ordered_index
                    .range((std::ops::Bound::Unbounded, std::ops::Bound::Excluded(key)))
                    .map(|(_, ids)| ids)
                {
                    out.extend(ids.iter().copied());
                }
            }
            MetadataRangeOp::Lte => {
                for ids in self
                    .ordered_index
                    .range((std::ops::Bound::Unbounded, std::ops::Bound::Included(key)))
                    .map(|(_, ids)| ids)
                {
                    out.extend(ids.iter().copied());
                }
            }
        }
        Some(out)
    }
}

#[derive(Debug, Clone, Copy)]
enum MetadataRangeOp {
    Gt,
    Gte,
    Lt,
    Lte,
}

/// Metadata storage with inverted indexes for filtering
pub struct MetadataStore {
    /// Vector ID -> metadata entry
    entries: HashMap<NodeId, MetadataEntry>,
    /// Key -> inverted index
    indexes: HashMap<String, KeyIndex>,
}

impl MetadataStore {
    /// Create a new empty metadata store
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            indexes: HashMap::new(),
        }
    }

    /// Get the number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Insert or update metadata for a vector
    pub fn insert(&mut self, id: NodeId, entry: MetadataEntry) {
        // Remove old indexes
        if let Some(old_entry) = self.entries.get(&id) {
            for key in old_entry.keys() {
                if let Some(value) = old_entry.get(&key) {
                    if let Some(index) = self.indexes.get_mut(&key) {
                        index.remove(id, &value);
                    }
                }
            }
        }

        // Add new indexes
        for key in entry.keys() {
            if let Some(value) = entry.get(&key) {
                self.indexes
                    .entry(key.clone())
                    .or_default()
                    .insert(id, &value);
            }
        }

        self.entries.insert(id, entry);
    }

    /// Get metadata for a vector
    pub fn get(&self, id: NodeId) -> Option<&MetadataEntry> {
        self.entries.get(&id)
    }

    /// Remove metadata for a vector
    pub fn remove(&mut self, id: NodeId) -> Option<MetadataEntry> {
        if let Some(entry) = self.entries.remove(&id) {
            for key in entry.keys() {
                if let Some(value) = entry.get(&key) {
                    if let Some(index) = self.indexes.get_mut(&key) {
                        index.remove(id, &value);
                    }
                }
            }
            Some(entry)
        } else {
            None
        }
    }

    /// Filter entries and return matching vector IDs
    pub fn filter(&self, filter: &MetadataFilter) -> HashSet<NodeId> {
        self.filter_internal(filter)
    }

    fn filter_internal(&self, filter: &MetadataFilter) -> HashSet<NodeId> {
        match filter {
            MetadataFilter::Eq(key, value) => self
                .indexes
                .get(key)
                .and_then(|idx| idx.exact_match_ids(value))
                .unwrap_or_else(|| {
                    self.entries
                        .iter()
                        .filter(|(_, entry)| {
                            entry
                                .get(key)
                                .map(|candidate| candidate.matches_eq(value))
                                .unwrap_or(false)
                        })
                        .map(|(id, _)| *id)
                        .collect()
                }),
            MetadataFilter::Ne(key, value) => {
                let all: HashSet<_> = self.entries.keys().copied().collect();
                if let Some(index) = self.indexes.get(key) {
                    if let Some(exact) = index.exact_match_ids(value) {
                        return all.difference(&exact).copied().collect();
                    }
                }
                self.entries
                    .iter()
                    .filter(|(_, entry)| {
                        entry
                            .get(key)
                            .map(|candidate| !candidate.matches_eq(value))
                            .unwrap_or(true)
                    })
                    .map(|(id, _)| *id)
                    .collect()
            }
            MetadataFilter::Gt(key, value) => self
                .indexes
                .get(key)
                .and_then(|idx| idx.range_match_ids(value, MetadataRangeOp::Gt))
                .unwrap_or_else(|| {
                    self.entries
                        .iter()
                        .filter(|(_, entry)| {
                            entry
                                .get(key)
                                .and_then(|candidate| candidate.compare(value))
                                .map(|ord| ord == std::cmp::Ordering::Greater)
                                .unwrap_or(false)
                        })
                        .map(|(id, _)| *id)
                        .collect()
                }),
            MetadataFilter::Gte(key, value) => self
                .indexes
                .get(key)
                .and_then(|idx| idx.range_match_ids(value, MetadataRangeOp::Gte))
                .unwrap_or_else(|| {
                    self.entries
                        .iter()
                        .filter(|(_, entry)| {
                            entry
                                .get(key)
                                .and_then(|candidate| candidate.compare(value))
                                .map(|ord| ord != std::cmp::Ordering::Less)
                                .unwrap_or(false)
                        })
                        .map(|(id, _)| *id)
                        .collect()
                }),
            MetadataFilter::Lt(key, value) => self
                .indexes
                .get(key)
                .and_then(|idx| idx.range_match_ids(value, MetadataRangeOp::Lt))
                .unwrap_or_else(|| {
                    self.entries
                        .iter()
                        .filter(|(_, entry)| {
                            entry
                                .get(key)
                                .and_then(|candidate| candidate.compare(value))
                                .map(|ord| ord == std::cmp::Ordering::Less)
                                .unwrap_or(false)
                        })
                        .map(|(id, _)| *id)
                        .collect()
                }),
            MetadataFilter::Lte(key, value) => self
                .indexes
                .get(key)
                .and_then(|idx| idx.range_match_ids(value, MetadataRangeOp::Lte))
                .unwrap_or_else(|| {
                    self.entries
                        .iter()
                        .filter(|(_, entry)| {
                            entry
                                .get(key)
                                .and_then(|candidate| candidate.compare(value))
                                .map(|ord| ord != std::cmp::Ordering::Greater)
                                .unwrap_or(false)
                        })
                        .map(|(id, _)| *id)
                        .collect()
                }),
            MetadataFilter::In(key, values) => {
                if let Some(index) = self.indexes.get(key) {
                    if let Some(result) =
                        values.iter().try_fold(HashSet::new(), |mut acc, value| {
                            let ids = index.exact_match_ids(value)?;
                            acc.extend(ids);
                            Some(acc)
                        })
                    {
                        return result;
                    }
                }
                self.entries
                    .iter()
                    .filter(|(_, entry)| {
                        entry
                            .get(key)
                            .map(|candidate| values.iter().any(|value| candidate.matches_eq(value)))
                            .unwrap_or(false)
                    })
                    .map(|(id, _)| *id)
                    .collect()
            }
            MetadataFilter::NotIn(key, values) => {
                let all: HashSet<_> = self.entries.keys().copied().collect();
                if let Some(index) = self.indexes.get(key) {
                    if let Some(matched) =
                        values.iter().try_fold(HashSet::new(), |mut acc, value| {
                            let ids = index.exact_match_ids(value)?;
                            acc.extend(ids);
                            Some(acc)
                        })
                    {
                        return all.difference(&matched).copied().collect();
                    }
                }
                self.entries
                    .iter()
                    .filter(|(_, entry)| {
                        entry
                            .get(key)
                            .map(|candidate| {
                                !values.iter().any(|value| candidate.matches_eq(value))
                            })
                            .unwrap_or(true)
                    })
                    .map(|(id, _)| *id)
                    .collect()
            }
            MetadataFilter::Exists(key) => self
                .indexes
                .get(key)
                .map(|idx| idx.all_ids.clone())
                .unwrap_or_default(),
            MetadataFilter::And(filters) => {
                if filters.is_empty() {
                    return self.entries.keys().copied().collect();
                }
                let mut result = self.filter_internal(&filters[0]);
                for filter in &filters[1..] {
                    let other = self.filter_internal(filter);
                    result = result.intersection(&other).copied().collect();
                }
                result
            }
            MetadataFilter::Or(filters) => {
                let mut result = HashSet::new();
                for filter in filters {
                    result.extend(self.filter_internal(filter));
                }
                result
            }
            MetadataFilter::Not(inner) => {
                let all: HashSet<_> = self.entries.keys().copied().collect();
                let matched = self.filter_internal(inner);
                all.difference(&matched).copied().collect()
            }
            // For complex filters, fall back to scanning
            _ => self
                .entries
                .iter()
                .filter(|(_, entry)| filter.matches(entry))
                .map(|(id, _)| *id)
                .collect(),
        }
    }
}

impl Default for MetadataStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_entry() {
        let mut entry = MetadataEntry::new();
        entry.insert("name", MetadataValue::String("test".to_string()));
        entry.insert("count", MetadataValue::Integer(42));
        entry.insert("score", MetadataValue::Float(2.5));
        entry.insert("active", MetadataValue::Bool(true));

        assert_eq!(
            entry.get("name"),
            Some(MetadataValue::String("test".to_string()))
        );
        assert_eq!(entry.get("count"), Some(MetadataValue::Integer(42)));
        assert!(entry.get("score").is_some());
        assert_eq!(entry.get("active"), Some(MetadataValue::Bool(true)));
        assert!(entry.get("nonexistent").is_none());
    }

    #[test]
    fn test_filter_eq() {
        let mut store = MetadataStore::new();

        let mut entry1 = MetadataEntry::new();
        entry1.insert("type", MetadataValue::String("host".to_string()));

        let mut entry2 = MetadataEntry::new();
        entry2.insert("type", MetadataValue::String("service".to_string()));

        store.insert(1, entry1);
        store.insert(2, entry2);

        let filter = MetadataFilter::eq("type", "host");
        let results = store.filter(&filter);

        assert_eq!(results.len(), 1);
        assert!(results.contains(&1));
    }

    #[test]
    fn test_filter_comparison() {
        let mut store = MetadataStore::new();

        for i in 0..10 {
            let mut entry = MetadataEntry::new();
            entry.insert("score", MetadataValue::Integer(i));
            store.insert(i as u64, entry);
        }

        // score > 5
        let filter = MetadataFilter::gt("score", MetadataValue::Integer(5));
        let results = store.filter(&filter);
        assert_eq!(results.len(), 4); // 6, 7, 8, 9

        // score >= 5
        let filter = MetadataFilter::gte("score", MetadataValue::Integer(5));
        let results = store.filter(&filter);
        assert_eq!(results.len(), 5); // 5, 6, 7, 8, 9

        // score < 3
        let filter = MetadataFilter::lt("score", MetadataValue::Integer(3));
        let results = store.filter(&filter);
        assert_eq!(results.len(), 3); // 0, 1, 2
    }

    #[test]
    fn test_filter_and() {
        let mut store = MetadataStore::new();

        let mut entry1 = MetadataEntry::new();
        entry1.insert("type", MetadataValue::String("host".to_string()));
        entry1.insert("active", MetadataValue::Bool(true));

        let mut entry2 = MetadataEntry::new();
        entry2.insert("type", MetadataValue::String("host".to_string()));
        entry2.insert("active", MetadataValue::Bool(false));

        let mut entry3 = MetadataEntry::new();
        entry3.insert("type", MetadataValue::String("service".to_string()));
        entry3.insert("active", MetadataValue::Bool(true));

        store.insert(1, entry1);
        store.insert(2, entry2);
        store.insert(3, entry3);

        let filter = MetadataFilter::and(vec![
            MetadataFilter::eq("type", "host"),
            MetadataFilter::eq("active", true),
        ]);
        let results = store.filter(&filter);

        assert_eq!(results.len(), 1);
        assert!(results.contains(&1));
    }

    #[test]
    fn test_filter_or() {
        let mut store = MetadataStore::new();

        let mut entry1 = MetadataEntry::new();
        entry1.insert("type", MetadataValue::String("host".to_string()));

        let mut entry2 = MetadataEntry::new();
        entry2.insert("type", MetadataValue::String("service".to_string()));

        let mut entry3 = MetadataEntry::new();
        entry3.insert("type", MetadataValue::String("network".to_string()));

        store.insert(1, entry1);
        store.insert(2, entry2);
        store.insert(3, entry3);

        let filter = MetadataFilter::or(vec![
            MetadataFilter::eq("type", "host"),
            MetadataFilter::eq("type", "service"),
        ]);
        let results = store.filter(&filter);

        assert_eq!(results.len(), 2);
        assert!(results.contains(&1));
        assert!(results.contains(&2));
    }

    #[test]
    fn test_filter_contains() {
        let mut store = MetadataStore::new();

        let mut entry1 = MetadataEntry::new();
        entry1.insert(
            "description",
            MetadataValue::String("SSH vulnerability".to_string()),
        );

        let mut entry2 = MetadataEntry::new();
        entry2.insert(
            "description",
            MetadataValue::String("HTTP server".to_string()),
        );

        store.insert(1, entry1);
        store.insert(2, entry2);

        let filter =
            MetadataFilter::Contains("description".to_string(), "vulnerability".to_string());
        let results = store.filter(&filter);

        assert_eq!(results.len(), 1);
        assert!(results.contains(&1));
    }

    #[test]
    fn test_filter_in() {
        let mut store = MetadataStore::new();

        for i in 0..5 {
            let mut entry = MetadataEntry::new();
            entry.insert("id", MetadataValue::Integer(i));
            store.insert(i as u64, entry);
        }

        let filter = MetadataFilter::In(
            "id".to_string(),
            vec![MetadataValue::Integer(1), MetadataValue::Integer(3)],
        );
        let results = store.filter(&filter);

        assert_eq!(results.len(), 2);
        assert!(results.contains(&1));
        assert!(results.contains(&3));
    }

    #[test]
    fn test_remove_updates_index() {
        let mut store = MetadataStore::new();

        let mut entry = MetadataEntry::new();
        entry.insert("type", MetadataValue::String("host".to_string()));
        store.insert(1, entry);

        assert_eq!(store.filter(&MetadataFilter::eq("type", "host")).len(), 1);

        store.remove(1);

        assert_eq!(store.filter(&MetadataFilter::eq("type", "host")).len(), 0);
    }

    #[test]
    fn test_filter_float_eq_uses_canonical_index() {
        let mut store = MetadataStore::new();

        let mut entry1 = MetadataEntry::new();
        entry1.insert("score", MetadataValue::Float(1.5));
        store.insert(1, entry1);

        let mut entry2 = MetadataEntry::new();
        entry2.insert("score", MetadataValue::Float(2.5));
        store.insert(2, entry2);

        let results = store.filter(&MetadataFilter::eq("score", MetadataValue::Float(2.5)));
        assert_eq!(results, HashSet::from([2]));
    }

    #[test]
    fn test_filter_string_range_uses_ordered_index() {
        let mut store = MetadataStore::new();

        for (id, tier) in [(1, "alpha"), (2, "bravo"), (3, "delta")] {
            let mut entry = MetadataEntry::new();
            entry.insert("tier", MetadataValue::String(tier.to_string()));
            store.insert(id, entry);
        }

        let results = store.filter(&MetadataFilter::gte(
            "tier",
            MetadataValue::String("bravo".to_string()),
        ));
        assert_eq!(results, HashSet::from([2, 3]));
    }
}
