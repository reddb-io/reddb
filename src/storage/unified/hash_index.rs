//! Hash Index
//!
//! Provides O(1) exact-match lookups as an alternative to B-tree's O(log n).
//! Ideal for equality queries (`WHERE id = X`, `WHERE email = 'foo@bar.com'`).
//!
//! Each hash index maps a column value (as bytes) to a set of entity IDs.
//! Multi-valued: the same key can map to multiple entities (non-unique index).
//! Unique constraint is enforced at insert time when `unique = true`.

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::entity::EntityId;

/// A single hash index on one or more columns
pub struct HashIndex {
    /// Maps key bytes → set of entity IDs
    entries: HashMap<Vec<u8>, Vec<EntityId>>,
    /// Whether this index enforces uniqueness
    pub unique: bool,
    /// Number of keys
    key_count: usize,
}

impl HashIndex {
    /// Create a new hash index
    pub fn new(unique: bool) -> Self {
        Self {
            entries: HashMap::new(),
            unique,
            key_count: 0,
        }
    }

    /// Insert a key → entity mapping.
    /// Returns `Err` if the index is unique and the key already exists.
    pub fn insert(&mut self, key: Vec<u8>, entity_id: EntityId) -> Result<(), HashIndexError> {
        let entry = self.entries.entry(key).or_default();
        if self.unique && !entry.is_empty() {
            return Err(HashIndexError::DuplicateKey);
        }
        if !entry.contains(&entity_id) {
            entry.push(entity_id);
            self.key_count += 1;
        }
        Ok(())
    }

    /// Lookup all entity IDs for an exact key match. O(1).
    pub fn get(&self, key: &[u8]) -> &[EntityId] {
        self.entries.get(key).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Remove a specific entity ID from a key.
    pub fn remove(&mut self, key: &[u8], entity_id: EntityId) -> bool {
        if let Some(ids) = self.entries.get_mut(key) {
            if let Some(pos) = ids.iter().position(|id| *id == entity_id) {
                ids.swap_remove(pos);
                self.key_count -= 1;
                if ids.is_empty() {
                    self.entries.remove(key);
                }
                return true;
            }
        }
        false
    }

    /// Remove all entries for an entity ID (slower — scans all keys).
    pub fn remove_entity(&mut self, entity_id: EntityId) {
        self.entries.retain(|_, ids| {
            if let Some(pos) = ids.iter().position(|id| *id == entity_id) {
                ids.swap_remove(pos);
                self.key_count -= 1;
            }
            !ids.is_empty()
        });
    }

    /// Check if a key exists
    pub fn contains_key(&self, key: &[u8]) -> bool {
        self.entries
            .get(key)
            .map(|ids| !ids.is_empty())
            .unwrap_or(false)
    }

    /// Number of distinct keys
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the index is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total number of (key, entity_id) entries
    pub fn entry_count(&self) -> usize {
        self.key_count
    }

    /// Clear the index
    pub fn clear(&mut self) {
        self.entries.clear();
        self.key_count = 0;
    }

    /// Approximate memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();
        for (key, ids) in &self.entries {
            size += key.len() + ids.len() * std::mem::size_of::<EntityId>() + 48;
            // HashMap overhead
        }
        size
    }
}

/// Hash index errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashIndexError {
    /// Attempted to insert a duplicate key in a unique index
    DuplicateKey,
    /// The requested index was not found in the manager registry.
    MissingIndex { collection: String, name: String },
}

impl std::fmt::Display for HashIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateKey => write!(f, "duplicate key in unique hash index"),
            Self::MissingIndex { collection, name } => {
                write!(
                    f,
                    "hash index '{name}' was not found for collection '{collection}'"
                )
            }
        }
    }
}

impl std::error::Error for HashIndexError {}

/// Configuration for a hash index
#[derive(Debug, Clone)]
pub struct HashIndexConfig {
    /// Index name
    pub name: String,
    /// Collection name
    pub collection: String,
    /// Column name(s) — concatenated for composite keys
    pub columns: Vec<String>,
    /// Whether to enforce uniqueness
    pub unique: bool,
}

/// Manager for all hash indices across collections
pub struct HashIndexManager {
    /// (collection, index_name) → HashIndex
    indices: RwLock<HashMap<(String, String), HashIndex>>,
}

fn recover_read_guard<'a, T>(
    result: Result<RwLockReadGuard<'a, T>, PoisonError<RwLockReadGuard<'a, T>>>,
) -> RwLockReadGuard<'a, T> {
    match result {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn recover_write_guard<'a, T>(
    result: Result<RwLockWriteGuard<'a, T>, PoisonError<RwLockWriteGuard<'a, T>>>,
) -> RwLockWriteGuard<'a, T> {
    match result {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

impl HashIndexManager {
    /// Create a new manager
    pub fn new() -> Self {
        Self {
            indices: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new hash index
    pub fn create_index(&self, config: &HashIndexConfig) -> Result<(), HashIndexError> {
        let mut indices = recover_write_guard(self.indices.write());
        let key = (config.collection.clone(), config.name.clone());
        indices.insert(key, HashIndex::new(config.unique));
        Ok(())
    }

    /// Drop a hash index
    pub fn drop_index(&self, collection: &str, name: &str) -> bool {
        let mut indices = recover_write_guard(self.indices.write());
        indices
            .remove(&(collection.to_string(), name.to_string()))
            .is_some()
    }

    /// Insert into a named index
    pub fn insert(
        &self,
        collection: &str,
        index_name: &str,
        key: Vec<u8>,
        entity_id: EntityId,
    ) -> Result<(), HashIndexError> {
        let mut indices = recover_write_guard(self.indices.write());
        if let Some(index) = indices.get_mut(&(collection.to_string(), index_name.to_string())) {
            index.insert(key, entity_id)
        } else {
            Err(HashIndexError::MissingIndex {
                collection: collection.to_string(),
                name: index_name.to_string(),
            })
        }
    }

    /// Lookup in a named index
    pub fn lookup(
        &self,
        collection: &str,
        index_name: &str,
        key: &[u8],
    ) -> Result<Vec<EntityId>, HashIndexError> {
        let indices = recover_read_guard(self.indices.read());
        if let Some(index) = indices.get(&(collection.to_string(), index_name.to_string())) {
            Ok(index.get(key).to_vec())
        } else {
            Err(HashIndexError::MissingIndex {
                collection: collection.to_string(),
                name: index_name.to_string(),
            })
        }
    }

    /// Remove from a named index
    pub fn remove(
        &self,
        collection: &str,
        index_name: &str,
        key: &[u8],
        entity_id: EntityId,
    ) -> Result<bool, HashIndexError> {
        let mut indices = recover_write_guard(self.indices.write());
        if let Some(index) = indices.get_mut(&(collection.to_string(), index_name.to_string())) {
            Ok(index.remove(key, entity_id))
        } else {
            Err(HashIndexError::MissingIndex {
                collection: collection.to_string(),
                name: index_name.to_string(),
            })
        }
    }

    /// List all indices for a collection
    pub fn list_indices(&self, collection: &str) -> Vec<String> {
        let indices = recover_read_guard(self.indices.read());
        indices
            .keys()
            .filter(|(coll, _)| coll == collection)
            .map(|(_, name)| name.clone())
            .collect()
    }

    /// Get stats for a specific index
    pub fn index_stats(&self, collection: &str, name: &str) -> Option<HashIndexStats> {
        let indices = recover_read_guard(self.indices.read());
        indices
            .get(&(collection.to_string(), name.to_string()))
            .map(|idx| HashIndexStats {
                name: name.to_string(),
                collection: collection.to_string(),
                unique: idx.unique,
                distinct_keys: idx.len(),
                total_entries: idx.entry_count(),
                memory_bytes: idx.memory_bytes(),
            })
    }
}

impl Default for HashIndexManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for a hash index
#[derive(Debug, Clone)]
pub struct HashIndexStats {
    pub name: String,
    pub collection: String,
    pub unique: bool,
    pub distinct_keys: usize,
    pub total_entries: usize,
    pub memory_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_index_basic() {
        let mut idx = HashIndex::new(false);
        idx.insert(b"alice".to_vec(), EntityId::new(1)).unwrap();
        idx.insert(b"bob".to_vec(), EntityId::new(2)).unwrap();

        assert_eq!(idx.get(b"alice"), &[EntityId::new(1)]);
        assert_eq!(idx.get(b"bob"), &[EntityId::new(2)]);
        assert!(idx.get(b"charlie").is_empty());
        assert_eq!(idx.len(), 2);
    }

    #[test]
    fn test_hash_index_multi_value() {
        let mut idx = HashIndex::new(false);
        idx.insert(b"status_active".to_vec(), EntityId::new(1))
            .unwrap();
        idx.insert(b"status_active".to_vec(), EntityId::new(2))
            .unwrap();
        idx.insert(b"status_active".to_vec(), EntityId::new(3))
            .unwrap();

        assert_eq!(idx.get(b"status_active").len(), 3);
    }

    #[test]
    fn test_hash_index_unique() {
        let mut idx = HashIndex::new(true);
        idx.insert(b"email".to_vec(), EntityId::new(1)).unwrap();

        let result = idx.insert(b"email".to_vec(), EntityId::new(2));
        assert_eq!(result, Err(HashIndexError::DuplicateKey));
    }

    #[test]
    fn test_hash_index_remove() {
        let mut idx = HashIndex::new(false);
        idx.insert(b"key".to_vec(), EntityId::new(1)).unwrap();
        idx.insert(b"key".to_vec(), EntityId::new(2)).unwrap();

        assert!(idx.remove(b"key", EntityId::new(1)));
        assert_eq!(idx.get(b"key"), &[EntityId::new(2)]);

        assert!(idx.remove(b"key", EntityId::new(2)));
        assert!(idx.get(b"key").is_empty());
        assert!(idx.is_empty());
    }

    #[test]
    fn test_hash_index_remove_entity() {
        let mut idx = HashIndex::new(false);
        idx.insert(b"a".to_vec(), EntityId::new(1)).unwrap();
        idx.insert(b"b".to_vec(), EntityId::new(1)).unwrap();
        idx.insert(b"c".to_vec(), EntityId::new(2)).unwrap();

        idx.remove_entity(EntityId::new(1));
        assert!(idx.get(b"a").is_empty());
        assert!(idx.get(b"b").is_empty());
        assert_eq!(idx.get(b"c"), &[EntityId::new(2)]);
    }

    #[test]
    fn test_hash_index_manager() {
        let mgr = HashIndexManager::new();
        mgr.create_index(&HashIndexConfig {
            name: "idx_email".to_string(),
            collection: "users".to_string(),
            columns: vec!["email".to_string()],
            unique: true,
        })
        .unwrap();

        mgr.insert(
            "users",
            "idx_email",
            b"alice@test.com".to_vec(),
            EntityId::new(1),
        )
        .unwrap();
        mgr.insert(
            "users",
            "idx_email",
            b"bob@test.com".to_vec(),
            EntityId::new(2),
        )
        .unwrap();

        let results = mgr.lookup("users", "idx_email", b"alice@test.com").unwrap();
        assert_eq!(results, vec![EntityId::new(1)]);

        let stats = mgr.index_stats("users", "idx_email").unwrap();
        assert_eq!(stats.distinct_keys, 2);
        assert!(stats.unique);
    }

    #[test]
    fn test_hash_index_manager_recovers_from_poisoned_lock() {
        let mgr = HashIndexManager::new();
        mgr.create_index(&HashIndexConfig {
            name: "idx_email".to_string(),
            collection: "users".to_string(),
            columns: vec!["email".to_string()],
            unique: false,
        })
        .unwrap();

        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mgr.indices.write().unwrap();
            panic!("poison hash index manager");
        }));

        mgr.insert(
            "users",
            "idx_email",
            b"alice@test.com".to_vec(),
            EntityId::new(1),
        )
        .unwrap();

        assert_eq!(
            mgr.lookup("users", "idx_email", b"alice@test.com").unwrap(),
            vec![EntityId::new(1)]
        );
    }

    #[test]
    fn test_hash_index_manager_lookup_missing_index_returns_error() {
        let mgr = HashIndexManager::new();

        let err = mgr
            .lookup("users", "idx_missing", b"alice@test.com")
            .expect_err("lookup should fail when the index does not exist");

        assert_eq!(
            err,
            HashIndexError::MissingIndex {
                collection: "users".to_string(),
                name: "idx_missing".to_string(),
            }
        );
    }
}
