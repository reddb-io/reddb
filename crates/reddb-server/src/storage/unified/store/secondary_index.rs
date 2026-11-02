//! Generation-bound secondary trees, built inside the existing checkpoint epoch.
//!
//! Resource sketch: conversion reads N source rows once per collection, writes
//! O(N * indexes) compact postings, and holds one decoded row plus one 1 KiB key.
//! Reopen loads only root handles (O(indexes)); queries use the shared page cache.
//! No network I/O or per-index resident posting map is introduced here.
use super::*;
use reddb_file::native_resume::SecondaryIndexResume;

/// Leave eight bytes for the physical entity identity in each B-tree key.
pub use reddb_engine::storage::secondary_index_read::SECONDARY_KEY_BYTES_MAX;
pub type SecondaryIndexRead =
    reddb_engine::storage::secondary_index_read::SecondaryIndexRead<Pager>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecondaryIndexIdentity {
    pub codec_version: u32,
    pub definition: [u8; 32],
}

/// The projector is immutable, deterministic and must not re-enter storage.
/// It writes a canonical key into the supplied bounded scratch buffer. The
/// store appends the physical identity, retaining all MVCC versions. Visibility
/// and logical uniqueness remain the query/transaction layer's responsibility.
pub trait SecondaryIndexProjector: Send + Sync {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError>;
}

#[derive(Clone)]
pub(super) struct SecondaryIndexSpec {
    identity: SecondaryIndexIdentity,
    projector: Arc<dyn SecondaryIndexProjector>,
}

pub(super) type SecondaryIndexSpecs = HashMap<String, BTreeMap<String, SecondaryIndexSpec>>;

#[derive(Clone)]
pub(super) struct PublishedSecondaryIndex {
    pub identity: SecondaryIndexIdentity,
    pub tree: Arc<BTree>,
}

impl PublishedSecondaryIndex {
    pub(super) fn resume(&self) -> SecondaryIndexResume {
        SecondaryIndexResume {
            root_page: self.tree.root_page_id(),
            codec_version: self.identity.codec_version,
            definition: self.identity.definition,
        }
    }
}

impl PublishedKeyIndex {
    pub(super) fn trees(&self) -> impl Iterator<Item = &Arc<BTree>> {
        std::iter::once(&self.tree).chain(self.secondary.values().map(|index| &index.tree))
    }
}

impl UnifiedStore {
    /// Register a projection for subsequent checkpoints. The definition must
    /// include method, ordered columns, uniqueness and any collation options.
    /// A changed fingerprint is never allowed to reuse the previous tree.
    pub fn register_secondary_index(
        &self,
        collection: &str,
        name: &str,
        identity: SecondaryIndexIdentity,
        projector: Arc<dyn SecondaryIndexProjector>,
    ) -> Result<(), StoreError> {
        if name.is_empty() || name.len() > 1024 || identity.codec_version == 0 {
            return Err(StoreError::Serialization(
                "invalid secondary index identity".into(),
            ));
        }
        let _publication = self.vault_publication_lock.lock();
        if self.config.resident_memory_bytes_max.is_none() || self.commit.is_none() {
            return Err(StoreError::Serialization(
                "secondary trees require disk residency and durable page ownership".into(),
            ));
        }
        if !self.resident.collections.read().contains_key(collection) {
            return Err(StoreError::CollectionNotFound(collection.into()));
        }
        let mut specs = self.secondary_index_specs.write();
        let indices = specs.entry(collection.to_owned()).or_default();
        if !indices.contains_key(name) && indices.len() >= 4096 {
            return Err(StoreError::Serialization(
                "too many secondary indexes".into(),
            ));
        }
        indices.insert(
            name.to_owned(),
            SecondaryIndexSpec {
                identity,
                projector,
            },
        );
        Ok(())
    }

    /// Preparation is a full ownership/checkpoint epoch, not merely a catalog
    /// flush. Reopened compatible roots need no source scan or publication.
    pub(crate) fn checkpoint_missing_secondary_indices(&self) -> Result<(), StoreError> {
        let missing = {
            let _publication = self.vault_publication_lock.lock();
            let specs = self.secondary_index_specs.read();
            specs.iter().any(|(collection, indices)| {
                indices.iter().any(|(name, spec)| {
                    self.secondary_index(collection, name, &spec.identity)
                        .is_none()
                })
            })
        };
        if missing {
            self.persist()?;
        }
        Ok(())
    }

    /// Pin an immutable index only if both the data generation and definition
    /// still match. A miss requires rebuilding or a correct scan fallback.
    /// This is a physical base index: callers must merge the live WAL overlay.
    pub fn secondary_index(
        &self,
        collection: &str,
        name: &str,
        identity: &SecondaryIndexIdentity,
    ) -> Option<SecondaryIndexRead> {
        let _publication = self.vault_publication_lock.lock();
        let trees = self.btree_indices.read();
        let data = trees.get(collection)?;
        let keys = self.key_indices.read();
        let generation = keys.get(collection)?;
        if !generation.source.ptr_eq(&Arc::downgrade(data)) {
            return None;
        }
        let index = generation.secondary.get(name)?;
        (index.identity == *identity)
            .then(|| SecondaryIndexRead::with_source(Arc::clone(&index.tree), Arc::clone(data)))
    }

    /// The next checkpoint publishes the removal. Already-pinned readers keep
    /// their old immutable tree until the normal retirement protocol releases it.
    pub fn unregister_secondary_index(&self, collection: &str, name: &str) {
        let _publication = self.vault_publication_lock.lock();
        if let Some(specs) = self.secondary_index_specs.write().get_mut(collection) {
            specs.remove(name);
        }
        if let Some(index) = self
            .key_indices
            .write()
            .get_mut(collection)
            .and_then(|generation| generation.secondary.remove(name))
        {
            self.retire_btree(index.tree);
            self.resident
                .paged_registry_dirty
                .store(true, Ordering::Release);
        }
    }

    pub(super) fn checkpoint_secondary_indices(
        &self,
        collection: &str,
        data: &Arc<BTree>,
        previous: Option<&PublishedKeyIndex>,
        pager: &Arc<Pager>,
        limit_bytes: usize,
    ) -> Result<BTreeMap<String, PublishedSecondaryIndex>, StoreError> {
        use reddb_engine::storage::collection_read::{CollectionCursor, PagedCollection};
        let same_generation = previous.filter(|index| index.source.ptr_eq(&Arc::downgrade(data)));
        let mut next = same_generation
            .map(|index| index.secondary.clone())
            .unwrap_or_default();
        let specs = self.secondary_index_specs.read();
        let Some(specs) = specs.get(collection) else {
            return Ok(next);
        };
        let mut builds = Vec::new();
        for (name, spec) in specs {
            if next
                .get(name)
                .is_some_and(|index| index.identity == spec.identity)
            {
                continue;
            }
            builds.push((name, spec, Arc::new(BTree::new(Arc::clone(pager)))));
        }
        if builds.is_empty() {
            return Ok(next);
        }
        let source = PagedCollection::new(Arc::clone(data));
        let mut cursor = source.scan()?;
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        while let Some(record) = cursor.next_record(limit_bytes)? {
            let (entity, _) = record.decode_with_budget(self.format_version(), limit_bytes)?;
            for (_, spec, tree) in &builds {
                let Some(length) = spec.projector.project(&entity, &mut key)? else {
                    continue;
                };
                if length > key.len() {
                    return Err(StoreError::Serialization(
                        "secondary key exceeds its bounded buffer".into(),
                    ));
                }
                let mut posting = [0; SECONDARY_KEY_BYTES_MAX + 8];
                posting[..length].copy_from_slice(&key[..length]);
                posting[length..length + 8].copy_from_slice(&entity.id.raw().to_be_bytes());
                tree.insert(&posting[..length + 8], &[])
                    .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
            }
        }
        for (name, spec, tree) in builds {
            next.insert(
                name.clone(),
                PublishedSecondaryIndex {
                    identity: spec.identity.clone(),
                    tree,
                },
            );
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests;
