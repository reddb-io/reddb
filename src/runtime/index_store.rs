//! Unified Index Store
//!
//! Holds all user-created secondary indices (Hash, Bitmap, Spatial) and
//! provides a single point of access for the query executor.
//!
//! The executor calls `lookup()` with a collection, column, and value —
//! the IndexStore finds the right index and returns matching entity IDs.

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound::{Excluded, Included, Unbounded};

use crate::storage::schema::{value_to_canonical_key, CanonicalKey, CanonicalKeyFamily, Value};
use crate::storage::unified::bitmap_index::BitmapIndexManager;
use crate::storage::unified::entity::EntityId;
use crate::storage::unified::hash_index::{HashIndexConfig, HashIndexManager};
use crate::storage::unified::spatial_index::SpatialIndexManager;

enum CanonicalizedValue {
    Exact(CanonicalKey),
    Unsupported,
}

fn read_unpoisoned<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    lock.read()
}

fn write_unpoisoned<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    lock.write()
}

/// In-memory sorted index for exact range scans over a single canonical family.
///
/// Point lookups remain safe even when the column contains mixed families,
/// because BTree seeks are exact on the canonical key. Range scans are only
/// enabled when every indexed value for the column belongs to the same family.
pub struct SortedColumnIndex {
    /// Sorted entries: canonical key → entity IDs
    entries: BTreeMap<CanonicalKey, Vec<EntityId>>,
    /// Family seen in this index. Mixed families keep exact lookup safe but
    /// disable range pushdown.
    range_family: Option<CanonicalKeyFamily>,
    has_mixed_families: bool,
    families: BTreeSet<CanonicalKeyFamily>,
}

impl SortedColumnIndex {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            range_family: None,
            has_mixed_families: false,
            families: BTreeSet::new(),
        }
    }

    pub fn insert(&mut self, key: CanonicalKey, entity_id: EntityId) {
        self.families.insert(key.family());
        match self.range_family {
            Some(existing) if existing != key.family() => self.has_mixed_families = true,
            None => self.range_family = Some(key.family()),
            _ => {}
        }
        self.entries.entry(key).or_default().push(entity_id);
    }

    fn range_enabled(&self, family: CanonicalKeyFamily) -> bool {
        !self.has_mixed_families && self.range_family == Some(family)
    }

    pub fn supports_range_key(&self, key: &CanonicalKey) -> bool {
        self.range_enabled(key.family())
    }

    pub fn supports_mixed_integral_ranges(&self) -> bool {
        !self.families.is_empty()
            && self.families.iter().all(|family| {
                matches!(
                    family,
                    CanonicalKeyFamily::Integer | CanonicalKeyFamily::UnsignedInteger
                )
            })
    }

    /// Range scan: returns all entity IDs where key is in [low, high].
    pub fn range(&self, low: CanonicalKey, high: CanonicalKey) -> Option<Vec<EntityId>> {
        if !self.range_enabled(low.family()) || low.family() != high.family() {
            return None;
        }
        if low > high {
            return Some(Vec::new());
        }
        Some(self.collect_range(low..=high))
    }

    /// Greater than: returns all entity IDs where key > threshold.
    pub fn greater_than(&self, threshold: CanonicalKey) -> Option<Vec<EntityId>> {
        if !self.range_enabled(threshold.family()) {
            return None;
        }
        Some(self.collect_range((Excluded(threshold), Unbounded)))
    }

    pub fn greater_equal(&self, threshold: CanonicalKey) -> Option<Vec<EntityId>> {
        if !self.range_enabled(threshold.family()) {
            return None;
        }
        Some(self.collect_range((Included(threshold), Unbounded)))
    }

    pub fn less_than(&self, threshold: CanonicalKey) -> Option<Vec<EntityId>> {
        if !self.range_enabled(threshold.family()) {
            return None;
        }
        Some(self.collect_range((Unbounded, Excluded(threshold))))
    }

    pub fn less_equal(&self, threshold: CanonicalKey) -> Option<Vec<EntityId>> {
        if !self.range_enabled(threshold.family()) {
            return None;
        }
        Some(self.collect_range((Unbounded, Included(threshold))))
    }

    pub fn len(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }

    /// Range scan with early stop at `limit` entity IDs.
    /// Iterates the BTree in key order — cheaper than `range()` for LIMIT-bounded
    /// queries because it stops as soon as enough IDs are collected.
    /// Returns None when float values make ordering unsafe.
    pub fn range_limited(
        &self,
        low: CanonicalKey,
        high: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if !self.range_enabled(low.family()) || low.family() != high.family() {
            return None;
        }
        if low > high {
            return Some(Vec::new());
        }
        Some(self.collect_range_limited(low..=high, limit))
    }

    pub fn range_limited_same_family(
        &self,
        low: CanonicalKey,
        high: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if low.family() != high.family() {
            return None;
        }
        if low > high {
            return Some(Vec::new());
        }
        if !self.families.contains(&low.family()) {
            return Some(Vec::new());
        }
        Some(self.collect_range_limited(low..=high, limit))
    }

    /// Greater-than scan with early stop at `limit`.
    pub fn greater_than_limited(
        &self,
        threshold: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if !self.range_enabled(threshold.family()) {
            return None;
        }
        Some(self.collect_range_limited((Excluded(threshold), Unbounded), limit))
    }

    /// Greater-or-equal scan with early stop at `limit`.
    pub fn greater_equal_limited(
        &self,
        threshold: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if !self.range_enabled(threshold.family()) {
            return None;
        }
        Some(self.collect_range_limited((Included(threshold), Unbounded), limit))
    }

    /// Less-than scan with early stop at `limit`.
    pub fn less_than_limited(
        &self,
        threshold: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if !self.range_enabled(threshold.family()) {
            return None;
        }
        Some(self.collect_range_limited((Unbounded, Excluded(threshold)), limit))
    }

    /// Less-or-equal scan with early stop at `limit`.
    pub fn less_equal_limited(
        &self,
        threshold: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if !self.range_enabled(threshold.family()) {
            return None;
        }
        Some(self.collect_range_limited((Unbounded, Included(threshold)), limit))
    }

    /// IN-list multi-point lookup: performs one BTree point-lookup per value
    /// (O(log n) each) instead of a range scan — matches MongoDB's
    /// `IndexBoundsChecker` MUST_ADVANCE / multi-interval seek behaviour.
    ///
    /// `values` need not be sorted; the method sorts internally.
    /// Stops after collecting `limit` entity IDs total.
    /// Returns `None` when inexact numeric values make the index unsafe.
    pub fn in_lookup_limited(
        &self,
        values: &[CanonicalKey],
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let mut sorted_vals = values.to_vec();
        sorted_vals.sort_unstable();
        sorted_vals.dedup();

        let mut result = Vec::with_capacity(limit.min(sorted_vals.len() * 4));
        'outer: for key in &sorted_vals {
            if let Some(ids) = self.entries.get(key) {
                for &id in ids {
                    result.push(id);
                    if result.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
        Some(result)
    }

    /// Like `in_lookup_limited` but only returns IDs also present in `filter_set`.
    /// Used for bitmap-AND of a hash-index predicate + an IN-list sorted-index predicate.
    pub fn in_lookup_limited_filtered_by_set(
        &self,
        values: &[CanonicalKey],
        filter_set: &std::collections::HashSet<u64>,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let mut sorted_vals = values.to_vec();
        sorted_vals.sort_unstable();
        sorted_vals.dedup();

        let mut result = Vec::with_capacity(limit.min(sorted_vals.len() * 4));
        'outer: for key in &sorted_vals {
            if let Some(ids) = self.entries.get(key) {
                for &id in ids {
                    if filter_set.contains(&id.raw()) {
                        result.push(id);
                        if result.len() >= limit {
                            break 'outer;
                        }
                    }
                }
            }
        }
        Some(result)
    }

    /// Covered-query projection: return the BTree **keys** (= column values) for a range.
    /// The key IS the value — no entity fetch needed for `SELECT col FROM t WHERE col op x`.
    /// Stops after `limit` distinct keys (each key may have multiple entity IDs, but for
    /// covered queries we only need the value once per distinct key).
    pub fn range_lookup_values<R>(&self, range: R, limit: usize) -> Vec<CanonicalKey>
    where
        R: std::ops::RangeBounds<CanonicalKey>,
    {
        self.entries
            .range(range)
            .take(limit)
            .map(|(key, _)| key.clone())
            .collect()
    }

    /// Covered-query projection for IN-lists: return BTree keys for the given values.
    pub fn in_lookup_values(&self, values: &[CanonicalKey], limit: usize) -> Vec<CanonicalKey> {
        let mut sorted_vals = values.to_vec();
        sorted_vals.sort_unstable();
        sorted_vals.dedup();
        let mut result = Vec::with_capacity(sorted_vals.len().min(limit));
        for key in sorted_vals {
            if self.entries.contains_key(&key) {
                result.push(key);
                if result.len() >= limit {
                    break;
                }
            }
        }
        result
    }

    fn collect_range<R>(&self, range: R) -> Vec<EntityId>
    where
        R: std::ops::RangeBounds<CanonicalKey>,
    {
        let mut result = Vec::new();
        for ids in self.entries.range(range).map(|(_, ids)| ids) {
            result.extend_from_slice(ids);
        }
        result
    }

    fn collect_range_limited<R>(&self, range: R, limit: usize) -> Vec<EntityId>
    where
        R: std::ops::RangeBounds<CanonicalKey>,
    {
        let mut result = Vec::with_capacity(limit.min(512));
        'outer: for ids in self.entries.range(range).map(|(_, ids)| ids) {
            for &id in ids {
                result.push(id);
                if result.len() >= limit {
                    break 'outer;
                }
            }
        }
        result
    }

    /// Bitmap AND: iterate the sorted range and collect only IDs present in
    /// `filter_set` (a hash-index candidate set from an equality predicate).
    /// Stops after `limit` results — matching PG's bitmap heap scan behaviour
    /// where the intersection is fetched in physical order.
    ///
    /// This avoids fetching ALL hash-index candidates when the range predicate
    /// is highly selective (e.g. `city='X'` ∩ `age > 30` → ~1K not 50K).
    /// Returns None when float values make the ordering unsafe.
    pub fn collect_range_filtered_by_set<R>(
        &self,
        range: R,
        filter_set: &std::collections::HashSet<u64>,
        limit: usize,
    ) -> Option<Vec<EntityId>>
    where
        R: std::ops::RangeBounds<CanonicalKey>,
    {
        if self.has_mixed_families {
            return None;
        }
        let mut result = Vec::new();
        'outer: for ids in self.entries.range(range).map(|(_, ids)| ids) {
            for &id in ids {
                if filter_set.contains(&id.raw()) {
                    result.push(id);
                    if result.len() >= limit {
                        break 'outer;
                    }
                }
            }
        }
        Some(result)
    }
}

/// In-memory sorted index over a TUPLE of canonical keys — the composite
/// index used to accelerate `WHERE col_a = X AND col_b > Y LIMIT N` style
/// queries where a single-column index would force a post-filter scan.
///
/// Ordering is the lexicographic ordering of the underlying `Vec<CanonicalKey>`
/// (Rust's derived `Ord` on `Vec` compares element-by-element), which
/// matches how PostgreSQL's multi-column B-tree behaves for prefix-seek
/// + trailing-range queries.
pub struct SortedCompositeIndex {
    entries: BTreeMap<Vec<CanonicalKey>, Vec<EntityId>>,
}

impl SortedCompositeIndex {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: Vec<CanonicalKey>, entity_id: EntityId) {
        self.entries.entry(key).or_default().push(entity_id);
    }

    pub fn len(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
    }

    /// Range scan with an exact prefix on the first `prefix.len()` columns
    /// and a range on the column at index `prefix.len()`. Returns up to
    /// `limit` entity IDs in key order.
    ///
    /// Preconditions — caller must ensure:
    /// - `prefix.len()` is < composite arity (strictly a prefix)
    /// - `low.family() == high.family()` so range semantics are well-defined
    pub fn prefix_range(
        &self,
        prefix: &[CanonicalKey],
        low: CanonicalKey,
        high: CanonicalKey,
        limit: usize,
    ) -> Vec<EntityId> {
        if limit == 0 {
            return Vec::new();
        }
        let mut low_key = prefix.to_vec();
        low_key.push(low);
        let mut high_key = prefix.to_vec();
        high_key.push(high);
        let mut out = Vec::with_capacity(limit.min(128));
        for (_, ids) in self.entries.range(low_key..=high_key) {
            for id in ids {
                out.push(*id);
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }

    /// Exact-prefix equality scan: returns entity IDs for every key
    /// whose first `prefix.len()` components match `prefix` exactly.
    /// Used by `WHERE col_a = X AND col_b = Y` on a `(col_a, col_b)`
    /// composite — treated as a prefix-only seek with no trailing range.
    pub fn prefix_eq(&self, prefix: &[CanonicalKey], limit: usize) -> Vec<EntityId> {
        if limit == 0 || prefix.is_empty() {
            return Vec::new();
        }
        // Low = prefix itself; high = prefix with one extra component that
        // is guaranteed to be > than any "same-prefix" key. We use the
        // half-open upper bound trick with BTreeMap::range(low..upper).
        let low = prefix.to_vec();
        let mut out = Vec::with_capacity(limit.min(128));
        for (k, ids) in self.entries.range(low.clone()..) {
            if k.len() < prefix.len() || &k[..prefix.len()] != prefix {
                break;
            }
            for id in ids {
                out.push(*id);
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }
}

/// Manages sorted column indices per (collection, column).
pub struct SortedIndexManager {
    indices: RwLock<HashMap<(String, String), SortedColumnIndex>>,
    composite: RwLock<HashMap<(String, Vec<String>), SortedCompositeIndex>>,
}

impl SortedIndexManager {
    pub fn new() -> Self {
        Self {
            indices: RwLock::new(HashMap::new()),
            composite: RwLock::new(HashMap::new()),
        }
    }

    /// Build a composite (multi-column) sorted index from existing entities.
    /// Every entity must expose a value for every listed column; entities
    /// missing any one column are skipped.
    pub fn build_composite(
        &self,
        collection: &str,
        columns: &[String],
        entities: &[(EntityId, Vec<(String, Value)>)],
    ) -> usize {
        let mut index = SortedCompositeIndex::new();
        let mut count = 0;
        'entity: for (eid, fields) in entities {
            let mut tuple: Vec<CanonicalKey> = Vec::with_capacity(columns.len());
            for col in columns {
                let found = fields.iter().find(|(name, _)| name == col);
                let key = match found {
                    Some((_, val)) => match classify_sorted_value(val) {
                        CanonicalizedValue::Exact(k) => k,
                        CanonicalizedValue::Unsupported => continue 'entity,
                    },
                    None => continue 'entity,
                };
                tuple.push(key);
            }
            index.insert(tuple, *eid);
            count += 1;
        }
        write_unpoisoned(&self.composite)
            .insert((collection.to_string(), columns.to_vec()), index);
        count
    }

    pub fn has_composite_index(&self, collection: &str, columns: &[String]) -> bool {
        let key = (collection.to_string(), columns.to_vec());
        read_unpoisoned(&self.composite).contains_key(&key)
    }

    /// Composite prefix-eq + trailing range lookup.
    /// `columns` must match a previously-built composite index exactly.
    pub fn composite_prefix_range_lookup(
        &self,
        collection: &str,
        columns: &[String],
        prefix: &[CanonicalKey],
        low: CanonicalKey,
        high: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let guard = read_unpoisoned(&self.composite);
        let idx = guard.get(&(collection.to_string(), columns.to_vec()))?;
        Some(idx.prefix_range(prefix, low, high, limit))
    }

    /// Composite exact prefix lookup.
    pub fn composite_prefix_eq_lookup(
        &self,
        collection: &str,
        columns: &[String],
        prefix: &[CanonicalKey],
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let guard = read_unpoisoned(&self.composite);
        let idx = guard.get(&(collection.to_string(), columns.to_vec()))?;
        Some(idx.prefix_eq(prefix, limit))
    }

    /// Update a composite index when an entity's fields change. Drops the
    /// old tuple and inserts the new one when any of the listed columns
    /// differs between `old_fields` and `new_fields`.
    pub fn composite_entity_update(
        &self,
        collection: &str,
        columns: &[String],
        entity_id: EntityId,
        old_fields: &[(String, Value)],
        new_fields: &[(String, Value)],
    ) {
        let build_tuple = |fields: &[(String, Value)]| -> Option<Vec<CanonicalKey>> {
            let mut tuple = Vec::with_capacity(columns.len());
            for col in columns {
                let val = fields.iter().find(|(name, _)| name == col).map(|(_, v)| v)?;
                match classify_sorted_value(val) {
                    CanonicalizedValue::Exact(k) => tuple.push(k),
                    CanonicalizedValue::Unsupported => return None,
                }
            }
            Some(tuple)
        };
        let old_tuple = build_tuple(old_fields);
        let new_tuple = build_tuple(new_fields);
        if old_tuple == new_tuple {
            return;
        }
        let mut guard = write_unpoisoned(&self.composite);
        let idx = match guard.get_mut(&(collection.to_string(), columns.to_vec())) {
            Some(i) => i,
            None => return,
        };
        if let Some(old) = old_tuple {
            if let Some(ids) = idx.entries.get_mut(&old) {
                ids.retain(|id| *id != entity_id);
                if ids.is_empty() {
                    idx.entries.remove(&old);
                }
            }
        }
        if let Some(new) = new_tuple {
            idx.insert(new, entity_id);
        }
    }

    /// Insert a single entity into all composite indexes registered on
    /// `collection`. Called from the entity-insert hot path alongside
    /// the existing single-column index maintenance.
    pub fn composite_entity_insert(
        &self,
        collection: &str,
        entity_id: EntityId,
        fields: &[(String, Value)],
    ) {
        let mut guard = write_unpoisoned(&self.composite);
        for ((coll, cols), idx) in guard.iter_mut() {
            if coll != collection {
                continue;
            }
            let mut tuple = Vec::with_capacity(cols.len());
            let mut complete = true;
            for col in cols {
                let val = fields.iter().find(|(name, _)| name == col).map(|(_, v)| v);
                match val.map(classify_sorted_value) {
                    Some(CanonicalizedValue::Exact(k)) => tuple.push(k),
                    _ => {
                        complete = false;
                        break;
                    }
                }
            }
            if complete {
                idx.insert(tuple, entity_id);
            }
        }
    }

    /// Build a sorted index from existing entities.
    pub fn build_index(
        &self,
        collection: &str,
        column: &str,
        entities: &[(EntityId, Vec<(String, Value)>)],
    ) -> usize {
        let mut index = SortedColumnIndex::new();
        let mut count = 0;
        for (eid, fields) in entities {
            for (col, val) in fields {
                if col == column {
                    match classify_sorted_value(val) {
                        CanonicalizedValue::Exact(key) => {
                            index.insert(key, *eid);
                            count += 1;
                        }
                        CanonicalizedValue::Unsupported => {}
                    }
                }
            }
        }
        write_unpoisoned(&self.indices).insert((collection.to_string(), column.to_string()), index);
        count
    }

    /// Range lookup.
    pub(crate) fn range_lookup(
        &self,
        collection: &str,
        column: &str,
        low: CanonicalKey,
        high: CanonicalKey,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        match indices.get(&key) {
            Some(index) => index.range(low, high),
            None => None,
        }
    }

    /// Greater-than lookup.
    pub(crate) fn gt_lookup(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        match indices.get(&key) {
            Some(index) => index.greater_than(threshold),
            None => None,
        }
    }

    pub(crate) fn ge_lookup(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        match indices.get(&key) {
            Some(index) => index.greater_equal(threshold),
            None => None,
        }
    }

    pub(crate) fn lt_lookup(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        match indices.get(&key) {
            Some(index) => index.less_than(threshold),
            None => None,
        }
    }

    pub(crate) fn le_lookup(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        match indices.get(&key) {
            Some(index) => index.less_equal(threshold),
            None => None,
        }
    }

    /// Range lookup with early stop at `limit` — avoids collecting all IDs
    /// when only the first N results are needed (LIMIT-bounded queries).
    pub(crate) fn range_lookup_limited(
        &self,
        collection: &str,
        column: &str,
        low: CanonicalKey,
        high: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices.get(&key)?.range_limited(low, high, limit)
    }

    pub(crate) fn range_lookup_limited_same_family(
        &self,
        collection: &str,
        column: &str,
        low: CanonicalKey,
        high: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices
            .get(&key)?
            .range_limited_same_family(low, high, limit)
    }

    pub(crate) fn gt_lookup_limited(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices.get(&key)?.greater_than_limited(threshold, limit)
    }

    pub(crate) fn ge_lookup_limited(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices.get(&key)?.greater_equal_limited(threshold, limit)
    }

    pub(crate) fn lt_lookup_limited(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices.get(&key)?.less_than_limited(threshold, limit)
    }

    pub(crate) fn le_lookup_limited(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices.get(&key)?.less_equal_limited(threshold, limit)
    }

    /// Bitmap AND: range [low, high] filtered to IDs in `filter_set`.
    pub(crate) fn range_filtered_by_set(
        &self,
        collection: &str,
        column: &str,
        low: CanonicalKey,
        high: CanonicalKey,
        filter_set: &std::collections::HashSet<u64>,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        if low > high {
            return Some(Vec::new());
        }
        let idx = indices.get(&key)?;
        if !idx.supports_range_key(&low) || low.family() != high.family() {
            return None;
        }
        idx.collect_range_filtered_by_set(low..=high, filter_set, limit)
    }

    /// Bitmap AND: gt filtered to IDs in `filter_set`.
    pub(crate) fn gt_filtered_by_set(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        filter_set: &std::collections::HashSet<u64>,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        let idx = indices.get(&key)?;
        if !idx.supports_range_key(&threshold) {
            return None;
        }
        idx.collect_range_filtered_by_set((Excluded(threshold), Unbounded), filter_set, limit)
    }

    /// Bitmap AND: ge filtered to IDs in `filter_set`.
    pub(crate) fn ge_filtered_by_set(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        filter_set: &std::collections::HashSet<u64>,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        let idx = indices.get(&key)?;
        if !idx.supports_range_key(&threshold) {
            return None;
        }
        idx.collect_range_filtered_by_set((Included(threshold), Unbounded), filter_set, limit)
    }

    /// Bitmap AND: lt filtered to IDs in `filter_set`.
    pub(crate) fn lt_filtered_by_set(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        filter_set: &std::collections::HashSet<u64>,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        let idx = indices.get(&key)?;
        if !idx.supports_range_key(&threshold) {
            return None;
        }
        idx.collect_range_filtered_by_set((Unbounded, Excluded(threshold)), filter_set, limit)
    }

    /// Bitmap AND: le filtered to IDs in `filter_set`.
    pub(crate) fn le_filtered_by_set(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        filter_set: &std::collections::HashSet<u64>,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        let idx = indices.get(&key)?;
        if !idx.supports_range_key(&threshold) {
            return None;
        }
        idx.collect_range_filtered_by_set((Unbounded, Included(threshold)), filter_set, limit)
    }

    /// IN-list multi-point lookup on a sorted index.
    /// Performs one BTree point-lookup per value — O(k log n) for k values
    /// instead of O(n) for a range scan covering all values.
    /// Stops after `limit` total entity IDs.
    pub(crate) fn in_lookup_limited(
        &self,
        collection: &str,
        column: &str,
        values: &[CanonicalKey],
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices.get(&key)?.in_lookup_limited(values, limit)
    }

    /// IN-list multi-point lookup filtered by a hash-index candidate set.
    /// Bitmap AND: sorted-index point lookups ∩ hash-index set.
    pub(crate) fn in_lookup_limited_filtered_by_set(
        &self,
        collection: &str,
        column: &str,
        values: &[CanonicalKey],
        filter_set: &std::collections::HashSet<u64>,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices
            .get(&key)?
            .in_lookup_limited_filtered_by_set(values, filter_set, limit)
    }

    /// Covered-query range projection: return BTree keys (= column values) for a range.
    /// No entity fetch required — the BTree key IS the column value.
    pub(crate) fn range_lookup_values(
        &self,
        collection: &str,
        column: &str,
        low: CanonicalKey,
        high: CanonicalKey,
        limit: usize,
    ) -> Option<Vec<CanonicalKey>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        let idx = indices.get(&key)?;
        if !idx.supports_range_key(&low) || low.family() != high.family() {
            return None;
        }
        Some(idx.range_lookup_values((Included(low), Included(high)), limit))
    }

    /// Covered-query gt/ge/lt/le projection.
    pub(crate) fn compare_lookup_values(
        &self,
        collection: &str,
        column: &str,
        threshold: CanonicalKey,
        op: &crate::storage::query::ast::CompareOp,
        limit: usize,
    ) -> Option<Vec<CanonicalKey>> {
        use crate::storage::query::ast::CompareOp;
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        let idx = indices.get(&key)?;
        if !idx.supports_range_key(&threshold) {
            return None;
        }
        let values = match op {
            CompareOp::Gt => idx.range_lookup_values((Excluded(threshold), Unbounded), limit),
            CompareOp::Ge => idx.range_lookup_values((Included(threshold), Unbounded), limit),
            CompareOp::Lt => idx.range_lookup_values((Unbounded, Excluded(threshold)), limit),
            CompareOp::Le => idx.range_lookup_values((Unbounded, Included(threshold)), limit),
            _ => return None,
        };
        Some(values)
    }

    /// Covered-query IN-list projection.
    pub(crate) fn in_lookup_values(
        &self,
        collection: &str,
        column: &str,
        values: &[CanonicalKey],
        limit: usize,
    ) -> Option<Vec<CanonicalKey>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        Some(indices.get(&key)?.in_lookup_values(values, limit))
    }

    /// Check if a sorted index exists for a column.
    pub fn has_index(&self, collection: &str, column: &str) -> bool {
        let indices = read_unpoisoned(&self.indices);
        indices.contains_key(&(collection.to_string(), column.to_string()))
    }

    pub fn supports_mixed_integral_ranges(&self, collection: &str, column: &str) -> bool {
        let indices = read_unpoisoned(&self.indices);
        indices
            .get(&(collection.to_string(), column.to_string()))
            .is_some_and(SortedColumnIndex::supports_mixed_integral_ranges)
    }

    /// Insert one value into an existing sorted index.
    /// No-op if the index hasn't been created yet — the next
    /// `build_index` or `create_index` call will pick up the entity on
    /// its full scan.
    pub(crate) fn insert_one(
        &self,
        collection: &str,
        column: &str,
        value: &Value,
        entity_id: EntityId,
    ) {
        let mut indices = write_unpoisoned(&self.indices);
        let k = (collection.to_string(), column.to_string());
        if let Some(index) = indices.get_mut(&k) {
            match classify_sorted_value(value) {
                CanonicalizedValue::Exact(key) => index.insert(key, entity_id),
                CanonicalizedValue::Unsupported => {}
            }
        }
    }

    /// Remove a single `entity_id` from the index. Linear in the
    /// number of entries at that key — fine for the benchmark's low
    /// per-key cardinality (age has ~200 buckets, city ~50).
    pub(crate) fn delete_one(
        &self,
        collection: &str,
        column: &str,
        value: &Value,
        entity_id: EntityId,
    ) {
        let mut indices = write_unpoisoned(&self.indices);
        let k = (collection.to_string(), column.to_string());
        if let Some(index) = indices.get_mut(&k) {
            let Some(key) = value_to_sorted_key(value) else {
                return;
            };
            if let Some(bucket) = index.entries.get_mut(&key) {
                bucket.retain(|id| *id != entity_id);
                if bucket.is_empty() {
                    index.entries.remove(&key);
                }
            }
        }
    }
}

fn classify_sorted_value(val: &Value) -> CanonicalizedValue {
    match value_to_canonical_key(val) {
        Some(key) => CanonicalizedValue::Exact(key),
        None => CanonicalizedValue::Unsupported,
    }
}

pub(crate) fn value_to_sorted_key(val: &Value) -> Option<CanonicalKey> {
    match classify_sorted_value(val) {
        CanonicalizedValue::Exact(key) => Some(key),
        CanonicalizedValue::Unsupported => None,
    }
}

/// Convert a canonical sorted key back to a `Value` for covered-query projection.
pub(crate) fn sorted_key_to_value(key: CanonicalKey) -> Value {
    key.into_value()
}

/// Metadata about a registered index
#[derive(Debug, Clone)]
pub struct RegisteredIndex {
    pub name: String,
    pub collection: String,
    pub columns: Vec<String>,
    pub method: IndexMethodKind,
    pub unique: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexMethodKind {
    Hash,
    Bitmap,
    Spatial,
    BTree,
}

/// Unified index store aggregating all secondary index managers.
pub struct IndexStore {
    pub hash: HashIndexManager,
    pub bitmap: BitmapIndexManager,
    pub spatial: SpatialIndexManager,
    pub sorted: SortedIndexManager,
    /// Registry of all created indices: (collection, index_name) → metadata
    registry: RwLock<HashMap<(String, String), RegisteredIndex>>,
}

impl IndexStore {
    pub fn new() -> Self {
        Self {
            hash: HashIndexManager::new(),
            bitmap: BitmapIndexManager::new(),
            spatial: SpatialIndexManager::new(),
            sorted: SortedIndexManager::new(),
            registry: RwLock::new(HashMap::new()),
        }
    }

    /// Register and build an index from existing entities.
    pub fn create_index(
        &self,
        name: &str,
        collection: &str,
        columns: &[String],
        method: IndexMethodKind,
        unique: bool,
        entities: &[(EntityId, Vec<(String, Value)>)],
    ) -> Result<usize, String> {
        let col = columns.first().map(|s| s.as_str()).unwrap_or("");

        match method {
            IndexMethodKind::Hash => {
                self.hash
                    .create_index(&HashIndexConfig {
                        name: name.to_string(),
                        collection: collection.to_string(),
                        columns: columns.to_vec(),
                        unique,
                    })
                    .map_err(|e| e.to_string())?;

                // Index existing entities
                let mut count = 0;
                for (entity_id, fields) in entities {
                    for (field_name, value) in fields {
                        if field_name == col {
                            let key = value_to_bytes(value);
                            self.hash
                                .insert(collection, name, key, *entity_id)
                                .map_err(|err| err.to_string())?;
                            count += 1;
                        }
                    }
                }
                Ok(count)
            }
            IndexMethodKind::Bitmap => {
                self.bitmap.create_index(collection, col);

                let mut count = 0;
                for (entity_id, fields) in entities {
                    for (field_name, value) in fields {
                        if field_name == col {
                            let key = value_to_bytes(value);
                            self.bitmap
                                .insert(collection, col, *entity_id, &key)
                                .map_err(|err| err.to_string())?;
                            count += 1;
                        }
                    }
                }
                Ok(count)
            }
            IndexMethodKind::Spatial => {
                self.spatial.create_index(collection, col);
                // Spatial indexing happens via insert with lat/lon
                Ok(0)
            }
            IndexMethodKind::BTree => {
                // Multi-column BTree → composite sorted index.
                // `CREATE INDEX idx_cc ON users (city, age) USING BTREE`
                // registers a `Vec<CanonicalKey>` → ids BTreeMap so
                // `WHERE city='NYC' AND age>30` can seek prefix + range
                // instead of intersecting two single-col results.
                if columns.len() > 1 {
                    let count = self.sorted.build_composite(collection, columns, entities);
                    return Ok(count);
                }
                // Build sorted in-memory index for range scans (single col)
                let count = self.sorted.build_index(collection, col, entities);
                // Also build hash index for equality lookups on same column
                self.hash
                    .create_index(&HashIndexConfig {
                        name: format!("{name}_hash"),
                        collection: collection.to_string(),
                        columns: columns.to_vec(),
                        unique: false,
                    })
                    .map_err(|err| err.to_string())?;
                for (entity_id, fields) in entities {
                    for (field_name, value) in fields {
                        if field_name == col {
                            let key = value_to_bytes(value);
                            self.hash
                                .insert(collection, &format!("{name}_hash"), key, *entity_id)
                                .map_err(|err| err.to_string())?;
                        }
                    }
                }
                Ok(count)
            }
        }
    }

    /// Drop an index
    pub fn drop_index(&self, name: &str, collection: &str) -> bool {
        let mut registry = write_unpoisoned(&self.registry);
        let key = (collection.to_string(), name.to_string());
        if let Some(info) = registry.remove(&key) {
            match info.method {
                IndexMethodKind::Hash => self.hash.drop_index(collection, name),
                IndexMethodKind::Bitmap => {
                    let col = info.columns.first().map(|s| s.as_str()).unwrap_or("");
                    self.bitmap.drop_index(collection, col)
                }
                IndexMethodKind::Spatial => {
                    let col = info.columns.first().map(|s| s.as_str()).unwrap_or("");
                    self.spatial.drop_index(collection, col)
                }
                IndexMethodKind::BTree => false,
            };
            true
        } else {
            false
        }
    }

    /// Register index metadata
    pub fn register(&self, info: RegisteredIndex) {
        let mut registry = write_unpoisoned(&self.registry);
        registry.insert((info.collection.clone(), info.name.clone()), info);
    }

    /// Lookup entity IDs via hash index for a collection.column = value
    pub fn hash_lookup(
        &self,
        collection: &str,
        index_name: &str,
        key: &[u8],
    ) -> Result<Vec<EntityId>, String> {
        self.hash
            .lookup(collection, index_name, key)
            .map_err(|err| err.to_string())
    }

    /// Lookup entity IDs via bitmap index for a collection.column = value
    pub fn bitmap_lookup(
        &self,
        collection: &str,
        column: &str,
        value: &[u8],
    ) -> Result<Vec<EntityId>, String> {
        self.bitmap
            .lookup(collection, column, value)
            .map_err(|err| err.to_string())
    }

    /// Count via bitmap (O(1))
    pub fn bitmap_count(
        &self,
        collection: &str,
        column: &str,
        value: &[u8],
    ) -> Result<u64, String> {
        self.bitmap
            .count(collection, column, value)
            .map_err(|err| err.to_string())
    }

    /// Find which index (if any) covers a collection + column
    pub fn find_index_for_column(&self, collection: &str, column: &str) -> Option<RegisteredIndex> {
        let registry = read_unpoisoned(&self.registry);
        registry
            .values()
            .find(|idx| idx.collection == collection && idx.columns.contains(&column.to_string()))
            .cloned()
    }

    /// List all indices for a collection
    pub fn list_indices(&self, collection: &str) -> Vec<RegisteredIndex> {
        let registry = read_unpoisoned(&self.registry);
        registry
            .values()
            .filter(|idx| idx.collection == collection)
            .cloned()
            .collect()
    }

    /// Collect the column-name set covered by any index on `collection`
    /// without cloning the full `RegisteredIndex` values — the
    /// `persist_applied_entity_mutations` HOT gate only needs to
    /// intersect `modified_columns` against this set, so cloning the
    /// index name / method / unique-flag every UPDATE is wasted work.
    pub fn indexed_columns_set(&self, collection: &str) -> std::collections::HashSet<String> {
        let registry = read_unpoisoned(&self.registry);
        let mut out = std::collections::HashSet::new();
        for idx in registry.values() {
            if idx.collection == collection {
                for col in &idx.columns {
                    out.insert(col.clone());
                }
            }
        }
        out
    }

    /// Batched counterpart of `index_entity_insert`: takes a full
    /// slice of `(EntityId, Vec<(String, Value)>)` pairs and walks
    /// the index registry ONCE, then loops inside each registered
    /// index. For an N-row insert with K indexes, this turns the
    /// previous `N × K` registry-lock acquisitions into exactly one.
    /// Matches the PG `heap_multi_insert` +  `ExecInsertIndexTuples`
    /// fusion so bulk ingest doesn't pay per-row overhead.
    pub fn index_entity_insert_batch(
        &self,
        collection: &str,
        rows: &[(EntityId, Vec<(String, Value)>)],
    ) -> Result<(), String> {
        if rows.is_empty() {
            return Ok(());
        }
        let registry = self.registry.read();

        // Pre-collect the index list for this collection once so the
        // hot loop isn't re-scanning the whole registry per row.
        let relevant: Vec<&RegisteredIndex> = registry
            .values()
            .filter(|idx| idx.collection == collection)
            .collect();
        if relevant.is_empty() {
            return Ok(());
        }

        // For each index, walk rows × fields but `break` as soon as
        // the matching column is found — every field name appears
        // at most once per row, so we average O(ncols/2) iterations
        // per (row, index) instead of O(ncols). Beats the
        // HashMap-pre-build shape when indexes < cols (the common
        // OLTP schema), because the inner `break` keeps string
        // compares short and amortised.
        for idx in &relevant {
            let col = idx.columns.first().map(|s| s.as_str()).unwrap_or("");
            // Hoist the "{name}_hash" auxiliary index name out of
            // the per-row inner loop for BTree indexes. Previously
            // every row paid a fresh `format!()` allocation.
            let btree_hash_name = matches!(idx.method, IndexMethodKind::BTree)
                .then(|| format!("{}_hash", idx.name));
            for (entity_id, fields) in rows {
                for (field_name, value) in fields {
                    if field_name != col {
                        continue;
                    }
                    let key = value_to_bytes(value);
                    match idx.method {
                        IndexMethodKind::Hash => {
                            self.hash
                                .insert(collection, &idx.name, key, *entity_id)
                                .map_err(|err| err.to_string())?;
                        }
                        IndexMethodKind::Bitmap => {
                            self.bitmap
                                .insert(collection, col, *entity_id, &key)
                                .map_err(|err| err.to_string())?;
                        }
                        IndexMethodKind::BTree => {
                            if !self.sorted.has_index(collection, col) {
                                return Err(format!(
                                    "sorted index for collection '{collection}' column '{col}' was not found"
                                ));
                            }
                            self.sorted.insert_one(collection, col, value, *entity_id);
                            let hash_name = btree_hash_name.as_deref().unwrap_or("");
                            self.hash
                                .insert(collection, hash_name, key, *entity_id)
                                .map_err(|err| err.to_string())?;
                        }
                        IndexMethodKind::Spatial => {}
                    }
                    // Column names are unique per row — early-exit
                    // the inner field scan.
                    break;
                }
            }
        }
        Ok(())
    }

    /// Insert one entity's relevant column values into every index
    /// registered on its collection. Called from the entity insert
    /// pipeline so that CREATE INDEX can land before or after the
    /// data without losing new rows. Missing backing structures are
    /// surfaced as errors instead of being silently ignored.
    pub fn index_entity_insert(
        &self,
        collection: &str,
        entity_id: EntityId,
        fields: &[(String, Value)],
    ) -> Result<(), String> {
        let registry = self.registry.read();
        for idx in registry.values() {
            if idx.collection != collection {
                continue;
            }

            // Composite BTree (multi-column) — maintain the tuple index.
            if matches!(idx.method, IndexMethodKind::BTree) && idx.columns.len() > 1 {
                self.sorted
                    .composite_entity_insert(collection, entity_id, fields);
                continue;
            }

            let col = idx.columns.first().map(|s| s.as_str()).unwrap_or("");
            for (field_name, value) in fields {
                if field_name == col {
                    let key = value_to_bytes(value);
                    match idx.method {
                        IndexMethodKind::Hash => {
                            self.hash
                                .insert(collection, &idx.name, key, entity_id)
                                .map_err(|err| err.to_string())?;
                        }
                        IndexMethodKind::Bitmap => {
                            self.bitmap
                                .insert(collection, col, entity_id, &key)
                                .map_err(|err| err.to_string())?;
                        }
                        IndexMethodKind::BTree => {
                            if !self.sorted.has_index(collection, col) {
                                return Err(format!(
                                    "sorted index for collection '{collection}' column '{col}' was not found"
                                ));
                            }
                            self.sorted.insert_one(collection, col, value, entity_id);
                            self.hash
                                .insert(collection, &format!("{}_hash", idx.name), key, entity_id)
                                .map_err(|err| err.to_string())?;
                        }
                        IndexMethodKind::Spatial => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// Mirror of `index_entity_insert` for the delete path. Removes the
    /// row from every secondary index registered on its collection. Index
    /// misses are tolerated (index may have been dropped between fetch
    /// and delete) — only structural errors propagate.
    pub fn index_entity_delete(
        &self,
        collection: &str,
        entity_id: EntityId,
        fields: &[(String, Value)],
    ) -> Result<(), String> {
        let registry = self.registry.read();
        for idx in registry.values() {
            if idx.collection != collection {
                continue;
            }

            // Composite BTree — drop the entry under the old tuple, if
            // present. Swap with None-fields on the update side flushes
            // it out cleanly.
            if matches!(idx.method, IndexMethodKind::BTree) && idx.columns.len() > 1 {
                self.sorted
                    .composite_entity_update(collection, &idx.columns, entity_id, fields, &[]);
                continue;
            }

            let col = idx.columns.first().map(|s| s.as_str()).unwrap_or("");
            for (field_name, value) in fields {
                if field_name == col {
                    let key = value_to_bytes(value);
                    match idx.method {
                        IndexMethodKind::Hash => {
                            // Missing index on delete is non-fatal.
                            let _ = self.hash.remove(collection, &idx.name, &key, entity_id);
                        }
                        IndexMethodKind::Bitmap => {
                            let _ = self.bitmap.remove(collection, col, entity_id);
                        }
                        IndexMethodKind::BTree => {
                            self.sorted.delete_one(collection, col, value, entity_id);
                            let _ = self.hash.remove(
                                collection,
                                &format!("{}_hash", idx.name),
                                &key,
                                entity_id,
                            );
                        }
                        IndexMethodKind::Spatial => {}
                    }
                }
            }
        }
        Ok(())
    }

    /// Apply an update to every secondary index registered on the
    /// collection. For each indexed column, if the value changed, deletes
    /// the entry under the old key and inserts under the new key. Columns
    /// that are absent from `new_fields` (typed as set-to-NULL) are
    /// removed from the index. New indexed columns appearing only in
    /// `new_fields` get inserted.
    pub fn index_entity_update(
        &self,
        collection: &str,
        entity_id: EntityId,
        old_fields: &[(String, Value)],
        new_fields: &[(String, Value)],
    ) -> Result<(), String> {
        // Snapshot the indexed columns once to avoid holding the registry
        // lock across delete/insert sub-calls (those re-acquire it).
        let indexed_cols: std::collections::HashSet<String> = {
            let registry = self.registry.read();
            registry
                .values()
                .filter(|idx| idx.collection == collection)
                .filter_map(|idx| idx.columns.first().cloned())
                .collect()
        };
        if indexed_cols.is_empty() {
            return Ok(());
        }

        // Compute the full damage-vector once, then filter to the
        // indexed columns. Drops the previous O(indexed × old.len)
        // pairwise scan to a single pass over old + new.
        let damage = crate::application::entity::row_damage_vector(old_fields, new_fields);

        for (col, old_value, new_value) in &damage.changed {
            if !indexed_cols.contains(col) {
                continue;
            }
            self.index_entity_delete(collection, entity_id, &[(col.clone(), old_value.clone())])?;
            self.index_entity_insert(collection, entity_id, &[(col.clone(), new_value.clone())])?;
        }
        for (col, old_value) in &damage.removed {
            if !indexed_cols.contains(col) {
                continue;
            }
            self.index_entity_delete(collection, entity_id, &[(col.clone(), old_value.clone())])?;
        }
        for (col, new_value) in &damage.added {
            if !indexed_cols.contains(col) {
                continue;
            }
            self.index_entity_insert(collection, entity_id, &[(col.clone(), new_value.clone())])?;
        }
        Ok(())
    }
}

impl Default for IndexStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a Value to bytes for index key
fn value_to_bytes(value: &Value) -> Vec<u8> {
    match value {
        Value::Text(s) => s.as_bytes().to_vec(),
        Value::Integer(n) => n.to_le_bytes().to_vec(),
        Value::UnsignedInteger(n) => n.to_le_bytes().to_vec(),
        Value::Float(n) => n.to_le_bytes().to_vec(),
        Value::Boolean(b) => vec![*b as u8],
        _ => format!("{:?}", value).into_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(values: &[EntityId]) -> Vec<u64> {
        values.iter().map(|id| id.raw()).collect()
    }

    #[test]
    fn test_sorted_key_supports_text_ranges() {
        let mut index = SortedColumnIndex::new();
        index.insert(
            value_to_sorted_key(&Value::text("alpha".to_string())).unwrap(),
            EntityId::new(1),
        );
        index.insert(
            value_to_sorted_key(&Value::text("bravo".to_string())).unwrap(),
            EntityId::new(2),
        );
        index.insert(
            value_to_sorted_key(&Value::text("charlie".to_string())).unwrap(),
            EntityId::new(3),
        );

        assert_eq!(
            ids(&index
                .greater_than(value_to_sorted_key(&Value::text("alpha".to_string())).unwrap())
                .unwrap()),
            vec![2, 3]
        );
        assert_eq!(
            ids(&index
                .less_equal(value_to_sorted_key(&Value::text("alpha".to_string())).unwrap())
                .unwrap()),
            vec![1]
        );
        assert_eq!(
            ids(&index
                .range(
                    value_to_sorted_key(&Value::text("bravo".to_string())).unwrap(),
                    value_to_sorted_key(&Value::text("charlie".to_string())).unwrap(),
                )
                .unwrap()),
            vec![2, 3]
        );
    }

    #[test]
    fn test_sorted_index_disables_range_lookup_when_mixed_families_are_present() {
        let manager = SortedIndexManager::new();
        let entities = vec![
            (
                EntityId::new(1),
                vec![("score".to_string(), Value::Integer(10))],
            ),
            (
                EntityId::new(2),
                vec![("score".to_string(), Value::Float(10.5))],
            ),
        ];

        manager.build_index("numbers", "score", &entities);

        assert_eq!(
            manager.range_lookup(
                "numbers",
                "score",
                value_to_sorted_key(&Value::Integer(0)).unwrap(),
                value_to_sorted_key(&Value::Integer(20)).unwrap(),
            ),
            None
        );
    }

    #[test]
    fn test_sorted_index_keeps_exact_in_lookup_when_mixed_families_are_present() {
        let manager = SortedIndexManager::new();
        let entities = vec![
            (
                EntityId::new(1),
                vec![("mixed".to_string(), Value::Integer(10))],
            ),
            (
                EntityId::new(2),
                vec![("mixed".to_string(), Value::text("ten".to_string()))],
            ),
        ];

        manager.build_index("mixed_table", "mixed", &entities);

        let matched = manager
            .in_lookup_limited(
                "mixed_table",
                "mixed",
                &[value_to_sorted_key(&Value::Integer(10)).unwrap()],
                10,
            )
            .unwrap();
        assert_eq!(ids(&matched), vec![1]);
    }

    #[test]
    fn test_index_entity_insert_errors_when_registered_hash_index_is_missing() {
        let store = IndexStore::new();
        store.register(RegisteredIndex {
            name: "idx_email".to_string(),
            collection: "users".to_string(),
            columns: vec!["email".to_string()],
            method: IndexMethodKind::Hash,
            unique: false,
        });

        let err = store
            .index_entity_insert(
                "users",
                EntityId::new(1),
                &[("email".to_string(), Value::text("a@b.com".to_string()))],
            )
            .expect_err("missing backing hash index should surface as an error");

        assert!(err.contains("idx_email"));
        assert!(err.contains("users"));
    }

    /// Helper that fully provisions a hash index on `users.email` with
    /// the matching registry entry — the realistic state after a
    /// `CREATE INDEX` call.
    fn provision_hash_index(store: &IndexStore) {
        store
            .create_index(
                "idx_email",
                "users",
                &["email".to_string()],
                IndexMethodKind::Hash,
                false,
                &[],
            )
            .unwrap();
        store.register(RegisteredIndex {
            name: "idx_email".to_string(),
            collection: "users".to_string(),
            columns: vec!["email".to_string()],
            method: IndexMethodKind::Hash,
            unique: false,
        });
    }

    #[test]
    fn test_index_entity_delete_removes_row_from_hash_index() {
        let store = IndexStore::new();
        provision_hash_index(&store);
        let id = EntityId::new(7);
        let key = ("email".to_string(), Value::text("a@b.com".to_string()));

        store
            .index_entity_insert("users", id, &[key.clone()])
            .unwrap();
        assert_eq!(
            ids(&store.hash_lookup("users", "idx_email", b"a@b.com").unwrap()),
            vec![7]
        );

        store
            .index_entity_delete("users", id, &[key.clone()])
            .unwrap();
        assert!(store
            .hash_lookup("users", "idx_email", b"a@b.com")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_index_entity_delete_tolerates_missing_index() {
        // No backing hash index exists — delete must still return Ok
        // (deleting from a dropped index is a non-fatal no-op).
        let store = IndexStore::new();
        store.register(RegisteredIndex {
            name: "idx_email".to_string(),
            collection: "users".to_string(),
            columns: vec!["email".to_string()],
            method: IndexMethodKind::Hash,
            unique: false,
        });
        store
            .index_entity_delete(
                "users",
                EntityId::new(1),
                &[("email".to_string(), Value::text("a@b.com".to_string()))],
            )
            .expect("delete must tolerate a missing backing index");
    }

    #[test]
    fn test_index_entity_update_moves_row_to_new_key() {
        let store = IndexStore::new();
        provision_hash_index(&store);
        let id = EntityId::new(11);
        let old = ("email".to_string(), Value::text("old@x.com".to_string()));
        let new = ("email".to_string(), Value::text("new@x.com".to_string()));

        store
            .index_entity_insert("users", id, &[old.clone()])
            .unwrap();

        store
            .index_entity_update("users", id, &[old.clone()], &[new.clone()])
            .unwrap();

        // Lookup under old key returns nothing, new key returns the id.
        assert!(store
            .hash_lookup("users", "idx_email", b"old@x.com")
            .unwrap()
            .is_empty());
        assert_eq!(
            ids(&store
                .hash_lookup("users", "idx_email", b"new@x.com")
                .unwrap()),
            vec![11]
        );
    }

    #[test]
    fn test_index_entity_update_skips_unchanged_columns() {
        // Update where the indexed column value didn't change must
        // leave the index untouched (no spurious delete + reinsert).
        let store = IndexStore::new();
        provision_hash_index(&store);
        let id = EntityId::new(13);
        let same = ("email".to_string(), Value::text("a@b.com".to_string()));

        store
            .index_entity_insert("users", id, &[same.clone()])
            .unwrap();
        store
            .index_entity_update("users", id, &[same.clone()], &[same.clone()])
            .unwrap();
        assert_eq!(
            ids(&store.hash_lookup("users", "idx_email", b"a@b.com").unwrap()),
            vec![13]
        );
    }

    #[test]
    fn test_index_entity_update_indexes_newly_added_column() {
        // Column present only in `new_fields` (was absent in `old`)
        // must be inserted into the index via the damage_vector.added
        // bucket.
        let store = IndexStore::new();
        provision_hash_index(&store);
        let id = EntityId::new(17);
        let old: Vec<(String, Value)> = vec![]; // email wasn't set before
        let new = vec![("email".to_string(), Value::text("fresh@x.com".to_string()))];

        store.index_entity_update("users", id, &old, &new).unwrap();

        assert_eq!(
            ids(&store
                .hash_lookup("users", "idx_email", b"fresh@x.com")
                .unwrap()),
            vec![17]
        );
    }

    #[test]
    fn test_index_entity_update_removes_dropped_column() {
        // Column present in `old_fields` but absent from `new_fields`
        // (SET email = NULL / DROP column) must be removed via the
        // damage_vector.removed bucket.
        let store = IndexStore::new();
        provision_hash_index(&store);
        let id = EntityId::new(19);
        let old = vec![("email".to_string(), Value::text("bye@x.com".to_string()))];
        let new: Vec<(String, Value)> = vec![]; // email dropped

        store.index_entity_insert("users", id, &old).unwrap();
        assert_eq!(
            ids(&store
                .hash_lookup("users", "idx_email", b"bye@x.com")
                .unwrap()),
            vec![19]
        );

        store.index_entity_update("users", id, &old, &new).unwrap();

        assert!(store
            .hash_lookup("users", "idx_email", b"bye@x.com")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn test_index_entity_update_ignores_non_indexed_column_changes() {
        // Changing a column that isn't registered in the index
        // registry must be a no-op — touch the index zero times.
        let store = IndexStore::new();
        provision_hash_index(&store); // only idx_email is registered
        let id = EntityId::new(23);

        let insert = vec![("email".to_string(), Value::text("a@b.com".to_string()))];
        store.index_entity_insert("users", id, &insert).unwrap();

        // Update only changes `age` (not registered). email unchanged.
        let old = vec![
            ("email".to_string(), Value::text("a@b.com".to_string())),
            ("age".to_string(), Value::Integer(30)),
        ];
        let new = vec![
            ("email".to_string(), Value::text("a@b.com".to_string())),
            ("age".to_string(), Value::Integer(31)),
        ];
        store.index_entity_update("users", id, &old, &new).unwrap();

        // Index is still intact under the unchanged email key.
        assert_eq!(
            ids(&store.hash_lookup("users", "idx_email", b"a@b.com").unwrap()),
            vec![23]
        );
    }
}
