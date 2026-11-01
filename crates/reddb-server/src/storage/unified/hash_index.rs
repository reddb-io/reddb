//! Hash Index
//!
//! Provides O(1) exact-match lookups as an alternative to B-tree's O(log n).
//! Ideal for equality queries (`WHERE id = X`, `WHERE email = 'foo@bar.com'`).
//!
//! Each hash index maps a column value (as bytes) to a set of entity IDs.
//! Multi-valued: the same key can map to multiple entities (non-unique index).
//! Unique constraint is enforced at insert time when `unique = true`.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::RwLock;

use super::entity::EntityId;

/// An owned candidate allocation. A disk provider keeps its memory reservation
/// attached until the IDs (or their consuming iterator) have been destroyed.
pub struct HashCandidates {
    ids: Vec<EntityId>,
    owner: Option<Box<dyn Send + Sync>>,
}

impl HashCandidates {
    pub(crate) fn owned(ids: Vec<EntityId>, owner: Box<dyn Send + Sync>) -> Self {
        Self {
            ids,
            owner: Some(owner),
        }
    }

    pub fn truncate(&mut self, len: usize) {
        self.ids.truncate(len);
    }

    /// Only resident providers may enter legacy unaccounted vector paths.
    pub(crate) fn into_resident(self) -> Option<Vec<EntityId>> {
        self.owner.is_none().then_some(self.ids)
    }
}

impl From<Vec<EntityId>> for HashCandidates {
    fn from(ids: Vec<EntityId>) -> Self {
        Self { ids, owner: None }
    }
}
impl std::ops::Deref for HashCandidates {
    type Target = [EntityId];
    fn deref(&self) -> &Self::Target {
        &self.ids
    }
}
impl std::fmt::Debug for HashCandidates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.ids.fmt(f)
    }
}
impl PartialEq<Vec<EntityId>> for HashCandidates {
    fn eq(&self, rhs: &Vec<EntityId>) -> bool {
        self.ids == *rhs
    }
}
impl PartialEq for HashCandidates {
    fn eq(&self, rhs: &Self) -> bool {
        self.ids == rhs.ids
    }
}
pub struct HashCandidateIterator {
    ids: std::vec::IntoIter<EntityId>,
    _owner: Option<Box<dyn Send + Sync>>,
}
impl Iterator for HashCandidateIterator {
    type Item = EntityId;
    fn next(&mut self) -> Option<Self::Item> {
        self.ids.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.ids.size_hint()
    }
}
impl ExactSizeIterator for HashCandidateIterator {}
impl std::iter::FusedIterator for HashCandidateIterator {}
impl IntoIterator for HashCandidates {
    type Item = EntityId;
    type IntoIter = HashCandidateIterator;
    fn into_iter(self) -> Self::IntoIter {
        HashCandidateIterator {
            ids: self.ids.into_iter(),
            _owner: self.owner,
        }
    }
}

/// The source store owns all mutations. This provider resolves physical
/// candidates; snapshot visibility and logical uniqueness remain runtime work.
pub(crate) trait PagedHashIndex: Send + Sync {
    fn lookup(&self, key: &[u8]) -> Result<HashCandidates, HashIndexError>;
    fn memory_bytes(&self) -> usize;
}

/// A single hash index on one or more columns
pub struct HashIndex {
    /// Maps key bytes → set of entity IDs
    entries: HashMap<Vec<u8>, Vec<EntityId>>,
    paged: Option<Arc<dyn PagedHashIndex>>,
    /// Whether this index enforces uniqueness
    pub unique: bool,
    /// Number of keys
    key_count: usize,
    /// Estimated key, posting-list and map-entry bytes.
    entry_memory_bytes: usize,
}

impl HashIndex {
    /// Create a new hash index
    pub fn new(unique: bool) -> Self {
        Self {
            entries: HashMap::new(),
            paged: None,
            unique,
            key_count: 0,
            entry_memory_bytes: 0,
        }
    }

    /// Insert a key → entity mapping.
    /// Returns `Err` if the index is unique and the key already exists.
    pub fn insert(&mut self, key: Vec<u8>, entity_id: EntityId) -> Result<(), HashIndexError> {
        if self.paged.is_some() {
            return Ok(());
        }
        let key_bytes = key.len();
        let entry = self.entries.entry(key).or_default();
        if self.unique && !entry.is_empty() {
            return Err(HashIndexError::DuplicateKey);
        }
        if !entry.contains(&entity_id) {
            if entry.is_empty() {
                self.entry_memory_bytes += key_bytes + 48;
            }
            self.entry_memory_bytes += std::mem::size_of::<EntityId>();
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
                self.entry_memory_bytes -= std::mem::size_of::<EntityId>();
                if ids.is_empty() {
                    self.entry_memory_bytes -= key.len() + 48;
                    self.entries.remove(key);
                }
                return true;
            }
        }
        false
    }

    /// Remove all entries for an entity ID (slower — scans all keys).
    pub fn remove_entity(&mut self, entity_id: EntityId) {
        self.entries.retain(|key, ids| {
            if let Some(pos) = ids.iter().position(|id| *id == entity_id) {
                ids.swap_remove(pos);
                self.key_count -= 1;
                self.entry_memory_bytes -= std::mem::size_of::<EntityId>();
            }
            if ids.is_empty() {
                self.entry_memory_bytes -= key.len() + 48;
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
        self.entry_memory_bytes = 0;
    }

    /// Approximate memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.entry_memory_bytes
            + self
                .paged
                .as_ref()
                .map_or(0, |provider| provider.memory_bytes())
    }
}

/// Hash index errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HashIndexError {
    /// Attempted to insert a duplicate key in a unique index
    DuplicateKey,
    /// An authoritative disk read or memory admission failed.
    Read(String),
    /// A retry may reserve a larger bounded decode/candidate workspace.
    ReadLimit {
        required_bytes: usize,
        limit_bytes: usize,
    },
    /// The requested index was not found in the manager registry.
    MissingIndex { collection: String, name: String },
}

impl std::fmt::Display for HashIndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadLimit {
                required_bytes,
                limit_bytes,
            } => write!(
                f,
                "hash index read needs {required_bytes} bytes; reserved workspace is {limit_bytes}"
            ),
            Self::Read(message) => write!(f, "hash index read failed: {message}"),
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
    /// Collection → index name → backing; borrowed probes allocate no names.
    indices: RwLock<HashMap<String, HashMap<String, HashIndex>>>,
}

impl HashIndexManager {
    /// Publish a fully built collection under the runtime topology fence.
    /// Moving the backing preserves the old indexes if preparation fails.
    pub(crate) fn replace_collection_from(&self, collection: &str, prepared: Self) {
        let mut indices = self.indices.write();
        indices.remove(collection);
        indices.extend(prepared.indices.into_inner());
    }

    /// Create a new manager
    pub fn new() -> Self {
        Self {
            indices: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new hash index
    pub fn create_index(&self, config: &HashIndexConfig) -> Result<(), HashIndexError> {
        let mut indices = self.indices.write();
        indices
            .entry(config.collection.clone())
            .or_default()
            .insert(config.name.clone(), HashIndex::new(config.unique));
        Ok(())
    }

    /// Drop a hash index
    pub fn drop_index(&self, collection: &str, name: &str) -> bool {
        let mut indices = self.indices.write();
        let removed = indices
            .get_mut(collection)
            .and_then(|indexes| indexes.remove(name))
            .is_some();
        if indices.get(collection).is_some_and(HashMap::is_empty) {
            indices.remove(collection);
        }
        removed
    }

    /// Insert into a named index
    pub fn insert(
        &self,
        collection: &str,
        index_name: &str,
        key: Vec<u8>,
        entity_id: EntityId,
    ) -> Result<(), HashIndexError> {
        let mut indices = self.indices.write();
        if let Some(index) = indices
            .get_mut(collection)
            .and_then(|indexes| indexes.get_mut(index_name))
        {
            index.insert(key, entity_id)
        } else {
            Err(HashIndexError::MissingIndex {
                collection: collection.to_string(),
                name: index_name.to_string(),
            })
        }
    }

    pub(crate) fn install_paged(
        &self,
        collection: &str,
        name: &str,
        provider: Arc<dyn PagedHashIndex>,
    ) {
        let mut index = HashIndex::new(false);
        index.paged = Some(provider);
        self.indices
            .write()
            .entry(collection.into())
            .or_default()
            .insert(name.into(), index);
    }

    pub(crate) fn is_paged(&self, collection: &str, name: &str) -> bool {
        self.indices
            .read()
            .get(collection)
            .and_then(|indexes| indexes.get(name))
            .is_some_and(|index| index.paged.is_some())
    }

    /// Atomically select the resident-only path. A separate `is_paged` probe
    /// can race DDL replacement and accidentally enter disk I/O in an Option API.
    pub(crate) fn lookup_resident(
        &self,
        collection: &str,
        name: &str,
        key: &[u8],
    ) -> Result<Option<Vec<EntityId>>, HashIndexError> {
        let indices = self.indices.read();
        let index = indices
            .get(collection)
            .and_then(|indexes| indexes.get(name))
            .ok_or_else(|| HashIndexError::MissingIndex {
                collection: collection.into(),
                name: name.into(),
            })?;
        Ok(index.paged.is_none().then(|| index.get(key).to_vec()))
    }

    /// Compatibility API for independently managed resident HASH indexes.
    pub fn lookup(
        &self,
        collection: &str,
        name: &str,
        key: &[u8],
    ) -> Result<Vec<EntityId>, HashIndexError> {
        self.lookup_resident(collection, name, key)?.ok_or_else(|| {
            HashIndexError::Read("paged HASH requires an owned candidate result".into())
        })
    }

    /// Never hold the manager lock across disk I/O or runtime admission:
    /// mutation/publication and memory sampling acquire these locks in reverse.
    pub(crate) fn lookup_owned(
        &self,
        collection: &str,
        index_name: &str,
        key: &[u8],
    ) -> Result<HashCandidates, HashIndexError> {
        let provider = {
            let indices = self.indices.read();
            let index = indices
                .get(collection)
                .and_then(|indexes| indexes.get(index_name))
                .ok_or_else(|| HashIndexError::MissingIndex {
                    collection: collection.into(),
                    name: index_name.into(),
                })?;
            match &index.paged {
                Some(provider) => Arc::clone(provider),
                None => return Ok(index.get(key).to_vec().into()),
            }
        };
        provider.lookup(key)
    }

    /// Remove from a named index
    pub fn remove(
        &self,
        collection: &str,
        index_name: &str,
        key: &[u8],
        entity_id: EntityId,
    ) -> Result<bool, HashIndexError> {
        let mut indices = self.indices.write();
        if let Some(index) = indices
            .get_mut(collection)
            .and_then(|indexes| indexes.get_mut(index_name))
        {
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
        let indices = self.indices.read();
        indices
            .get(collection)
            .into_iter()
            .flat_map(|indexes| indexes.keys().cloned())
            .collect()
    }

    /// Get stats for a specific index
    pub fn index_stats(&self, collection: &str, name: &str) -> Option<HashIndexStats> {
        let indices = self.indices.read();
        indices
            .get(collection)
            .and_then(|indexes| indexes.get(name))
            .filter(|idx| idx.paged.is_none())
            .map(|idx| HashIndexStats {
                name: name.to_string(),
                collection: collection.to_string(),
                unique: idx.unique,
                distinct_keys: idx.len(),
                total_entries: idx.entry_count(),
                memory_bytes: idx.memory_bytes(),
            })
    }

    /// Approximate resident bytes across every hash index — this manager's
    /// contribution to the `index_memory` pool (ADR 0073 §2).
    pub fn memory_bytes(&self) -> u64 {
        self.indices
            .read()
            .values()
            .flat_map(|indexes| indexes.values())
            .map(|idx| idx.memory_bytes() as u64)
            .sum()
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
    fn backing_probes_allocate_nothing_and_collection_replacement_is_isolated() {
        struct NoReadProvider;
        impl PagedHashIndex for NoReadProvider {
            fn lookup(&self, _: &[u8]) -> Result<HashCandidates, HashIndexError> {
                panic!("a backing probe must not read the provider");
            }
            fn memory_bytes(&self) -> usize {
                0
            }
        }

        let manager = HashIndexManager::new();
        let config = HashIndexConfig {
            collection: "resident".into(),
            name: "shared_name".into(),
            columns: vec!["id".into()],
            unique: false,
        };
        manager.create_index(&config).expect("create resident index");
        manager.install_paged("paged", "shared_name", Arc::new(NoReadProvider));
        let (_, allocations) = crate::storage::no_malloc_hot_paths::measure_allocations(|| {
            assert!(!manager.is_paged("resident", "shared_name"));
            assert!(manager.is_paged("paged", "shared_name"));
            assert!(!manager.is_paged("paged", "missing"));
            assert!(!manager.is_paged("missing", "shared_name"));
        });
        assert_eq!(allocations.allocs, 0);

        let replacement = HashIndexManager::new();
        replacement.install_paged("resident", "replacement", Arc::new(NoReadProvider));
        manager.replace_collection_from("resident", replacement);
        assert_eq!(manager.list_indices("resident"), vec!["replacement"]);
        assert!(manager.is_paged("paged", "shared_name"));
        assert!(manager.is_paged("resident", "replacement"));
        assert!(!manager.drop_index("resident", "shared_name"));
        assert!(manager.drop_index("resident", "replacement"));
        assert!(!manager.indices.read().contains_key("resident"));
        assert!(manager.is_paged("paged", "shared_name"));
    }

    fn assert_memory_accounting(index: &HashIndex) {
        let recomputed = std::mem::size_of::<HashIndex>()
            + index
                .entries
                .iter()
                .map(|(key, ids)| key.len() + ids.len() * std::mem::size_of::<EntityId>() + 48)
                .sum::<usize>();
        assert_eq!(index.memory_bytes(), recomputed);
    }

    #[test]
    fn memory_accounting_tracks_duplicates_removal_and_clear() {
        for unique in [false, true] {
            let mut index = HashIndex::new(unique);
            assert_memory_accounting(&index);
            for id in 0..128 {
                let key = vec![b'x'; usize::try_from(id % 13).expect("small key")];
                let _ = index.insert(key.clone(), EntityId::new(id));
                assert_memory_accounting(&index);
                let _ = index.insert(key, EntityId::new(id));
                assert_memory_accounting(&index);
            }
            for id in 0..128 {
                if id % 2 == 0 {
                    index.remove_entity(EntityId::new(id));
                } else {
                    let key = vec![b'x'; usize::try_from(id % 13).expect("small key")];
                    index.remove(&key, EntityId::new(id));
                }
                assert_memory_accounting(&index);
            }
            assert!(index.is_empty());
            index
                .insert(b"again".to_vec(), EntityId::new(1))
                .expect("empty index");
            index.clear();
            assert_memory_accounting(&index);
        }
    }

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
            let _guard = mgr.indices.write();
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
