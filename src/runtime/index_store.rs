//! Unified Index Store
//!
//! Holds all user-created secondary indices (Hash, Bitmap, Spatial) and
//! provides a single point of access for the query executor.
//!
//! The executor calls `lookup()` with a collection, column, and value —
//! the IndexStore finds the right index and returns matching entity IDs.

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::ops::Bound::{Excluded, Included, Unbounded};

use crate::storage::schema::Value;
use crate::storage::unified::bitmap_index::BitmapIndexManager;
use crate::storage::unified::entity::EntityId;
use crate::storage::unified::hash_index::{HashIndexConfig, HashIndexManager};
use crate::storage::unified::spatial_index::SpatialIndexManager;

/// Numeric key used by the in-memory sorted index.
///
/// The key preserves the natural order between signed and unsigned integers
/// without lossy casts. In particular, `u64` values above `i64::MAX` remain
/// correctly ordered after every signed integer instead of wrapping negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SortedNumericKey {
    Signed(i64),
    Unsigned(u64),
}

impl Ord for SortedNumericKey {
    fn cmp(&self, other: &Self) -> Ordering {
        match (*self, *other) {
            (Self::Signed(left), Self::Signed(right)) => left.cmp(&right),
            (Self::Unsigned(left), Self::Unsigned(right)) => left.cmp(&right),
            (Self::Signed(left), Self::Unsigned(right)) => {
                if left < 0 {
                    Ordering::Less
                } else {
                    (left as u64).cmp(&right)
                }
            }
            (Self::Unsigned(left), Self::Signed(right)) => {
                if right < 0 {
                    Ordering::Greater
                } else {
                    left.cmp(&(right as u64))
                }
            }
        }
    }
}

impl PartialOrd for SortedNumericKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

enum SortedNumericValue {
    Exact(SortedNumericKey),
    Inexact,
    Unsupported,
}

fn read_unpoisoned<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    lock.read()
}

fn write_unpoisoned<'a, T>(lock: &'a RwLock<T>) -> RwLockWriteGuard<'a, T> {
    lock.write()
}

/// In-memory sorted index for exact integral range scans.
/// Supports BETWEEN, >, <, >=, <= queries in O(log N + K) when the indexed
/// column contains only `Integer` and `UnsignedInteger` values.
pub struct SortedColumnIndex {
    /// Sorted entries: numeric key → entity IDs
    entries: BTreeMap<SortedNumericKey, Vec<EntityId>>,
    /// Floats on the indexed column make the integral-only ordering unsafe for
    /// pushdown, so lookups must fall back to a full scan.
    has_inexact_numeric_values: bool,
}

impl SortedColumnIndex {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            has_inexact_numeric_values: false,
        }
    }

    pub fn insert(&mut self, key: SortedNumericKey, entity_id: EntityId) {
        self.entries.entry(key).or_default().push(entity_id);
    }

    pub fn mark_inexact_numeric_values(&mut self) {
        self.has_inexact_numeric_values = true;
    }

    /// Range scan: returns all entity IDs where key is in [low, high].
    pub fn range(&self, low: SortedNumericKey, high: SortedNumericKey) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        if low > high {
            return Some(Vec::new());
        }
        Some(self.collect_range(low..=high))
    }

    /// Greater than: returns all entity IDs where key > threshold.
    pub fn greater_than(&self, threshold: SortedNumericKey) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        Some(self.collect_range((Excluded(threshold), Unbounded)))
    }

    pub fn greater_equal(&self, threshold: SortedNumericKey) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        Some(self.collect_range((Included(threshold), Unbounded)))
    }

    pub fn less_than(&self, threshold: SortedNumericKey) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        Some(self.collect_range((Unbounded, Excluded(threshold))))
    }

    pub fn less_equal(&self, threshold: SortedNumericKey) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
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
        low: SortedNumericKey,
        high: SortedNumericKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        if low > high {
            return Some(Vec::new());
        }
        Some(self.collect_range_limited(low..=high, limit))
    }

    /// Greater-than scan with early stop at `limit`.
    pub fn greater_than_limited(
        &self,
        threshold: SortedNumericKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        Some(self.collect_range_limited((Excluded(threshold), Unbounded), limit))
    }

    /// Greater-or-equal scan with early stop at `limit`.
    pub fn greater_equal_limited(
        &self,
        threshold: SortedNumericKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        Some(self.collect_range_limited((Included(threshold), Unbounded), limit))
    }

    /// Less-than scan with early stop at `limit`.
    pub fn less_than_limited(
        &self,
        threshold: SortedNumericKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        Some(self.collect_range_limited((Unbounded, Excluded(threshold)), limit))
    }

    /// Less-or-equal scan with early stop at `limit`.
    pub fn less_equal_limited(
        &self,
        threshold: SortedNumericKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        if self.has_inexact_numeric_values {
            return None;
        }
        Some(self.collect_range_limited((Unbounded, Included(threshold)), limit))
    }

    fn collect_range<R>(&self, range: R) -> Vec<EntityId>
    where
        R: std::ops::RangeBounds<SortedNumericKey>,
    {
        let mut result = Vec::new();
        for ids in self.entries.range(range).map(|(_, ids)| ids) {
            result.extend_from_slice(ids);
        }
        result
    }

    fn collect_range_limited<R>(&self, range: R, limit: usize) -> Vec<EntityId>
    where
        R: std::ops::RangeBounds<SortedNumericKey>,
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
}

/// Manages sorted column indices per (collection, column).
pub struct SortedIndexManager {
    indices: RwLock<HashMap<(String, String), SortedColumnIndex>>,
}

impl SortedIndexManager {
    pub fn new() -> Self {
        Self {
            indices: RwLock::new(HashMap::new()),
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
                    match classify_sorted_numeric_value(val) {
                        SortedNumericValue::Exact(key) => {
                            index.insert(key, *eid);
                            count += 1;
                        }
                        SortedNumericValue::Inexact => index.mark_inexact_numeric_values(),
                        SortedNumericValue::Unsupported => {}
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
        low: SortedNumericKey,
        high: SortedNumericKey,
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
        threshold: SortedNumericKey,
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
        threshold: SortedNumericKey,
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
        threshold: SortedNumericKey,
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
        threshold: SortedNumericKey,
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
        low: SortedNumericKey,
        high: SortedNumericKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices.get(&key)?.range_limited(low, high, limit)
    }

    pub(crate) fn gt_lookup_limited(
        &self,
        collection: &str,
        column: &str,
        threshold: SortedNumericKey,
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
        threshold: SortedNumericKey,
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
        threshold: SortedNumericKey,
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
        threshold: SortedNumericKey,
        limit: usize,
    ) -> Option<Vec<EntityId>> {
        let indices = read_unpoisoned(&self.indices);
        let key = (collection.to_string(), column.to_string());
        indices.get(&key)?.less_equal_limited(threshold, limit)
    }

    /// Check if a sorted index exists for a column.
    pub fn has_index(&self, collection: &str, column: &str) -> bool {
        let indices = read_unpoisoned(&self.indices);
        indices.contains_key(&(collection.to_string(), column.to_string()))
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
            match classify_sorted_numeric_value(value) {
                SortedNumericValue::Exact(key) => index.insert(key, entity_id),
                SortedNumericValue::Inexact => index.mark_inexact_numeric_values(),
                SortedNumericValue::Unsupported => {}
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
            let Some(key) = value_to_sorted_numeric_key(value) else {
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

fn classify_sorted_numeric_value(val: &Value) -> SortedNumericValue {
    match val {
        Value::Integer(n) => SortedNumericValue::Exact(SortedNumericKey::Signed(*n)),
        Value::UnsignedInteger(n) => SortedNumericValue::Exact(SortedNumericKey::Unsigned(*n)),
        Value::Float(_) => SortedNumericValue::Inexact,
        _ => SortedNumericValue::Unsupported,
    }
}

pub(crate) fn value_to_sorted_numeric_key(val: &Value) -> Option<SortedNumericKey> {
    match classify_sorted_numeric_value(val) {
        SortedNumericValue::Exact(key) => Some(key),
        SortedNumericValue::Inexact | SortedNumericValue::Unsupported => None,
    }
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
                // Build sorted in-memory index for range scans
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
    fn test_sorted_numeric_key_orders_unsigned_above_i64_max_without_wrap() {
        let mut index = SortedColumnIndex::new();
        index.insert(SortedNumericKey::Signed(i64::MIN), EntityId::new(1));
        index.insert(SortedNumericKey::Signed(i64::MAX), EntityId::new(2));
        index.insert(
            SortedNumericKey::Unsigned(i64::MAX as u64 + 1),
            EntityId::new(3),
        );
        index.insert(SortedNumericKey::Unsigned(u64::MAX), EntityId::new(4));

        assert_eq!(
            ids(&index
                .greater_than(SortedNumericKey::Signed(i64::MAX))
                .unwrap()),
            vec![3, 4]
        );
        assert_eq!(
            ids(&index
                .less_equal(SortedNumericKey::Signed(i64::MIN))
                .unwrap()),
            vec![1]
        );
        assert_eq!(
            ids(&index
                .range(
                    SortedNumericKey::Signed(i64::MAX),
                    SortedNumericKey::Unsigned(i64::MAX as u64 + 1),
                )
                .unwrap()),
            vec![2, 3]
        );
    }

    #[test]
    fn test_sorted_index_disables_exact_lookup_when_float_values_are_present() {
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
                SortedNumericKey::Signed(0),
                SortedNumericKey::Signed(20),
            ),
            None
        );
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
                &[("email".to_string(), Value::Text("a@b.com".to_string()))],
            )
            .expect_err("missing backing hash index should surface as an error");

        assert!(err.contains("idx_email"));
        assert!(err.contains("users"));
    }
}
