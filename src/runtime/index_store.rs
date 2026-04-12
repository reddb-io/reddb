//! Unified Index Store
//!
//! Holds all user-created secondary indices (Hash, Bitmap, Spatial) and
//! provides a single point of access for the query executor.
//!
//! The executor calls `lookup()` with a collection, column, and value —
//! the IndexStore finds the right index and returns matching entity IDs.

use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

use crate::storage::schema::Value;
use crate::storage::unified::bitmap_index::BitmapIndexManager;
use crate::storage::unified::entity::EntityId;
use crate::storage::unified::hash_index::{HashIndexConfig, HashIndexManager};
use crate::storage::unified::spatial_index::SpatialIndexManager;

/// In-memory sorted index for range scans. BTreeMap<i64, Vec<EntityId>>.
/// Supports BETWEEN, >, <, >=, <= queries in O(log N + K).
pub struct SortedColumnIndex {
    /// Sorted entries: numeric key → entity IDs
    entries: BTreeMap<i64, Vec<EntityId>>,
}

impl SortedColumnIndex {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: i64, entity_id: EntityId) {
        self.entries.entry(key).or_default().push(entity_id);
    }

    /// Range scan: returns all entity IDs where key is in [low, high].
    pub fn range(&self, low: i64, high: i64) -> Vec<EntityId> {
        if low > high {
            return Vec::new();
        }
        let mut result = Vec::new();
        for (_key, ids) in self.entries.range(low..=high) {
            result.extend_from_slice(ids);
        }
        result
    }

    /// Greater than: returns all entity IDs where key > threshold.
    pub fn greater_than(&self, threshold: i64) -> Vec<EntityId> {
        use std::ops::RangeFrom;
        let mut result = Vec::new();
        for (key, ids) in self.entries.range(RangeFrom {
            start: threshold + 1,
        }) {
            let _ = key;
            result.extend_from_slice(ids);
        }
        result
    }

    pub fn len(&self) -> usize {
        self.entries.values().map(|v| v.len()).sum()
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
                    if let Some(key) = value_to_i64(val) {
                        index.insert(key, *eid);
                        count += 1;
                    }
                }
            }
        }
        self.indices
            .write()
            .unwrap()
            .insert((collection.to_string(), column.to_string()), index);
        count
    }

    /// Range lookup.
    pub fn range_lookup(
        &self,
        collection: &str,
        column: &str,
        low: i64,
        high: i64,
    ) -> Vec<EntityId> {
        let indices = self.indices.read().unwrap();
        let key = (collection.to_string(), column.to_string());
        match indices.get(&key) {
            Some(index) => index.range(low, high),
            None => Vec::new(),
        }
    }

    /// Greater-than lookup.
    pub fn gt_lookup(&self, collection: &str, column: &str, threshold: i64) -> Vec<EntityId> {
        let indices = self.indices.read().unwrap();
        let key = (collection.to_string(), column.to_string());
        match indices.get(&key) {
            Some(index) => index.greater_than(threshold),
            None => Vec::new(),
        }
    }

    /// Check if a sorted index exists for a column.
    pub fn has_index(&self, collection: &str, column: &str) -> bool {
        let indices = self.indices.read().unwrap();
        indices.contains_key(&(collection.to_string(), column.to_string()))
    }
}

fn value_to_i64(val: &Value) -> Option<i64> {
    match val {
        Value::Integer(n) => Some(*n),
        Value::UnsignedInteger(n) => Some(*n as i64),
        Value::Float(f) => Some(*f as i64),
        _ => None,
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
                            let _ = self.hash.insert(collection, name, key, *entity_id);
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
                            self.bitmap.insert(collection, col, *entity_id, &key);
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
                let _ = self.hash.create_index(&HashIndexConfig {
                    name: format!("{name}_hash"),
                    collection: collection.to_string(),
                    columns: columns.to_vec(),
                    unique: false,
                });
                for (entity_id, fields) in entities {
                    for (field_name, value) in fields {
                        if field_name == col {
                            let key = value_to_bytes(value);
                            let _ = self.hash.insert(
                                collection,
                                &format!("{name}_hash"),
                                key,
                                *entity_id,
                            );
                        }
                    }
                }
                Ok(count)
            }
        }
    }

    /// Drop an index
    pub fn drop_index(&self, name: &str, collection: &str) -> bool {
        let mut registry = self.registry.write().unwrap();
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
        let mut registry = self.registry.write().unwrap();
        registry.insert((info.collection.clone(), info.name.clone()), info);
    }

    /// Lookup entity IDs via hash index for a collection.column = value
    pub fn hash_lookup(&self, collection: &str, index_name: &str, key: &[u8]) -> Vec<EntityId> {
        self.hash.lookup(collection, index_name, key)
    }

    /// Lookup entity IDs via bitmap index for a collection.column = value
    pub fn bitmap_lookup(&self, collection: &str, column: &str, value: &[u8]) -> Vec<EntityId> {
        self.bitmap.lookup(collection, column, value)
    }

    /// Count via bitmap (O(1))
    pub fn bitmap_count(&self, collection: &str, column: &str, value: &[u8]) -> u64 {
        self.bitmap.count(collection, column, value)
    }

    /// Find which index (if any) covers a collection + column
    pub fn find_index_for_column(&self, collection: &str, column: &str) -> Option<RegisteredIndex> {
        let registry = self.registry.read().unwrap();
        registry
            .values()
            .find(|idx| idx.collection == collection && idx.columns.contains(&column.to_string()))
            .cloned()
    }

    /// List all indices for a collection
    pub fn list_indices(&self, collection: &str) -> Vec<RegisteredIndex> {
        let registry = self.registry.read().unwrap();
        registry
            .values()
            .filter(|idx| idx.collection == collection)
            .cloned()
            .collect()
    }

    /// Insert a value into all relevant indices for a collection
    pub fn index_entity_insert(
        &self,
        collection: &str,
        entity_id: EntityId,
        fields: &[(String, Value)],
    ) {
        let registry = self.registry.read().unwrap();
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
                            let _ = self.hash.insert(collection, &idx.name, key, entity_id);
                        }
                        IndexMethodKind::Bitmap => {
                            self.bitmap.insert(collection, col, entity_id, &key);
                        }
                        _ => {}
                    }
                }
            }
        }
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
