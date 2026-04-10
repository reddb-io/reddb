//! Unified Index Store
//!
//! Holds all user-created secondary indices (Hash, Bitmap, Spatial) and
//! provides a single point of access for the query executor.
//!
//! The executor calls `lookup()` with a collection, column, and value —
//! the IndexStore finds the right index and returns matching entity IDs.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::storage::schema::Value;
use crate::storage::unified::bitmap_index::BitmapIndexManager;
use crate::storage::unified::entity::EntityId;
use crate::storage::unified::hash_index::{HashIndexConfig, HashIndexManager};
use crate::storage::unified::spatial_index::SpatialIndexManager;

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
    /// Registry of all created indices: (collection, index_name) → metadata
    registry: RwLock<HashMap<(String, String), RegisteredIndex>>,
}

impl IndexStore {
    pub fn new() -> Self {
        Self {
            hash: HashIndexManager::new(),
            bitmap: BitmapIndexManager::new(),
            spatial: SpatialIndexManager::new(),
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
                // B-tree is the default, handled by the segment system
                Ok(entities.len())
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
