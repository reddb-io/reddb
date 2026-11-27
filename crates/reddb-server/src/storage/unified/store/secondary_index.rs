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

pub(crate) const RUNTIME_SECONDARY_INDEX_CODEC_VERSION: u32 = 3;

#[cfg(test)]
std::thread_local! {
    static LEGACY_RUNTIME_SECONDARY_INDEX_IDENTITY: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    static CHECKPOINT_SECONDARY_INDEX_SKIP_ENTITY:
        std::cell::RefCell<Option<(String, u64)>> = const { std::cell::RefCell::new(None) };
}

pub(crate) fn runtime_secondary_index_codec_version() -> u32 {
    #[cfg(test)]
    if LEGACY_RUNTIME_SECONDARY_INDEX_IDENTITY.with(std::cell::Cell::get) {
        return 2;
    }
    RUNTIME_SECONDARY_INDEX_CODEC_VERSION
}

#[cfg(test)]
pub(crate) struct LegacyRuntimeSecondaryIndexIdentityGuard;

#[cfg(test)]
impl Drop for LegacyRuntimeSecondaryIndexIdentityGuard {
    fn drop(&mut self) {
        LEGACY_RUNTIME_SECONDARY_INDEX_IDENTITY.with(|legacy| legacy.set(false));
    }
}

#[cfg(test)]
pub(crate) fn use_legacy_runtime_secondary_index_identity(
) -> LegacyRuntimeSecondaryIndexIdentityGuard {
    assert!(
        !LEGACY_RUNTIME_SECONDARY_INDEX_IDENTITY.with(|legacy| legacy.replace(true)),
        "legacy runtime secondary-index identity is already active"
    );
    LegacyRuntimeSecondaryIndexIdentityGuard
}

#[cfg(test)]
pub(crate) struct CheckpointSecondaryIndexSkipEntityGuard;

#[cfg(test)]
impl Drop for CheckpointSecondaryIndexSkipEntityGuard {
    fn drop(&mut self) {
        CHECKPOINT_SECONDARY_INDEX_SKIP_ENTITY.with(|skip| {
            skip.borrow_mut().take();
        });
    }
}

#[cfg(test)]
pub(crate) fn checkpoint_secondary_indices_without_entity(
    collection: &str,
    entity_id: u64,
) -> CheckpointSecondaryIndexSkipEntityGuard {
    CHECKPOINT_SECONDARY_INDEX_SKIP_ENTITY.with(|skip| {
        let previous = skip.borrow_mut().replace((
            UnifiedStore::canonical_collection(collection).to_string(),
            entity_id,
        ));
        assert!(
            previous.is_none(),
            "secondary-index checkpoint skip is already active"
        );
    });
    CheckpointSecondaryIndexSkipEntityGuard
}

#[cfg(test)]
fn checkpoint_skips_secondary_index_entity(collection: &str, entity_id: u64) -> bool {
    CHECKPOINT_SECONDARY_INDEX_SKIP_ENTITY.with(|skip| {
        skip.borrow()
            .as_ref()
            .is_some_and(|(candidate, id)| candidate == collection && *id == entity_id)
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecondaryIndexIdentity {
    pub codec_version: u32,
    pub definition: [u8; 32],
}

/// The projector is immutable, deterministic and must not re-enter storage.
/// It writes a canonical key into the supplied bounded scratch buffer. The
/// store appends the physical identity. Current secondary indexes omit retired
/// MVCC versions; historical reads use the query layer's authoritative fallback.
pub trait SecondaryIndexProjector: Send + Sync {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError>;
}

#[derive(Clone)]
pub(super) struct SecondaryIndexSpec {
    pub(super) identity: SecondaryIndexIdentity,
    /// Identities under which the same index was published before columns
    /// were appended to the collection's schema. Every row has the same key
    /// under them as under `identity`, so such a tree answers reads until the
    /// next checkpoint publishes this index under `identity`.
    pub(super) predecessors: Vec<SecondaryIndexIdentity>,
    pub(super) projector: Arc<dyn SecondaryIndexProjector>,
    pub(super) deferred: bool,
    /// Reads answer without it, by walking the collection. Its tree is built
    /// by whichever checkpoint comes next, never by one taken for its sake
    /// while the store is being opened.
    pub(super) optional: bool,
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
    #[cfg(test)]
    pub(crate) fn rebuild_secondary_indices_on_next_checkpoint(
        &self,
        collection: &str,
        names: &[&str],
    ) {
        let collection = Self::canonical_collection(collection);
        let _publication = self.vault_publication_lock.lock();
        let removed = {
            let mut generations = self.key_indices.write();
            let generation = generations
                .get_mut(collection)
                .expect("secondary-index checkpoint fixture generation");
            names
                .iter()
                .filter_map(|name| generation.secondary.remove(*name))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            removed.len(),
            names.len(),
            "secondary-index checkpoint fixture must remove every requested artifact"
        );
        for index in removed {
            self.retire_btree(index.tree);
        }
        self.resident
            .paged_registry_dirty
            .store(true, Ordering::Release);
    }

    /// Check a registered index against its immutable checkpoint source. This
    /// is an operator pass: O(rows * log(postings)), one bounded row at a time.
    /// Source read failures propagate; only derived-tree failures are findings.
    pub(crate) fn verify_persisted_secondary_index(
        &self,
        collection: &str,
        name: &str,
        limit_bytes: usize,
    ) -> Result<Option<String>, StoreError> {
        use reddb_engine::storage::collection_read::{CollectionCursor, PagedCollection};
        let _publication = self.vault_publication_lock.lock();
        let specs = self.secondary_index_specs.read();
        let spec = specs
            .get(collection)
            .and_then(|indices| indices.get(name))
            .ok_or_else(|| StoreError::Serialization("index has no persisted projection".into()))?;
        let trees = self.btree_indices.read();
        let Some(data) = trees.get(collection) else {
            return Ok(Some("collection has no checkpoint generation".into()));
        };
        let keys = self.key_indices.read();
        let index = keys
            .get(collection)
            .filter(|generation| generation.source.ptr_eq(&Arc::downgrade(data)))
            .and_then(|generation| generation.secondary.get(name))
            .filter(|index| index.identity == spec.identity);
        let Some(index) = index else {
            return Ok(Some("missing or incompatible persisted index".into()));
        };
        let source = PagedCollection::new(Arc::clone(data));
        let mut cursor = source.scan()?;
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        let mut expected = 0u64;
        while let Some(record) = cursor.next_record(limit_bytes)? {
            let (entity, _) = record.decode_with_budget(self.format_version(), limit_bytes)?;
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
            match index
                .tree
                .get_with_budget(&posting[..length + 8], limit_bytes)
            {
                Ok(Some(value)) if value.is_empty() => expected += 1,
                Ok(_) => {
                    return Ok(Some(format!(
                        "missing or invalid posting for physical entity {}",
                        entity.id.raw()
                    )))
                }
                Err(error) => return Ok(Some(format!("index read failed: {error}"))),
            }
        }
        let handle = SecondaryIndexRead::with_source(Arc::clone(&index.tree), Arc::clone(data));
        let mut actual = 0u64;
        if let Err(error) = handle.try_for_each(|_, _| {
            actual += 1;
            Ok(())
        }) {
            return Ok(Some(format!("index inventory failed: {error}")));
        }
        Ok((actual != expected)
            .then(|| format!("posting count mismatch: expected {expected}, found {actual}")))
    }

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
        self.register_secondary_index_as(collection, name, identity, Vec::new(), projector, false)
    }

    /// Register an index that statements can do without: one a user declared.
    /// A missing tree for it does not hold up opening the store.
    pub fn register_optional_secondary_index(
        &self,
        collection: &str,
        name: &str,
        identity: SecondaryIndexIdentity,
        projector: Arc<dyn SecondaryIndexProjector>,
    ) -> Result<(), StoreError> {
        self.register_secondary_index_as(collection, name, identity, Vec::new(), projector, true)
    }

    /// As [`Self::register_optional_secondary_index`], for an index whose
    /// identity moved because columns were appended to the collection's
    /// schema. A tree published under one of `predecessors` holds the same
    /// key for every row and keeps answering reads; without this, declaring a
    /// column leaves every index of the collection unread after the next
    /// restart until a checkpoint completes.
    pub fn register_optional_secondary_index_after(
        &self,
        collection: &str,
        name: &str,
        identity: SecondaryIndexIdentity,
        predecessors: Vec<SecondaryIndexIdentity>,
        projector: Arc<dyn SecondaryIndexProjector>,
    ) -> Result<(), StoreError> {
        self.register_secondary_index_as(collection, name, identity, predecessors, projector, true)
    }

    fn register_secondary_index_as(
        &self,
        collection: &str,
        name: &str,
        identity: SecondaryIndexIdentity,
        predecessors: Vec<SecondaryIndexIdentity>,
        projector: Arc<dyn SecondaryIndexProjector>,
        optional: bool,
    ) -> Result<(), StoreError> {
        let collection = Self::canonical_collection(collection);
        if name.is_empty() || name.len() > 1024 || identity.codec_version == 0 {
            return Err(StoreError::Serialization(
                "invalid secondary index identity".into(),
            ));
        }
        let _publication = self.vault_publication_lock.lock();
        if self.config.resident_memory_bytes_max.is_none()
            || (self.commit.is_none() && !self.is_read_only())
        {
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
                predecessors,
                projector,
                deferred: false,
                optional,
            },
        );
        Ok(())
    }

    /// Opening the store waits for this only on behalf of projections the
    /// runtime cannot read without. A checkpoint here covers everything the
    /// WAL replayed, which on a large store is many minutes during which the
    /// server answers nothing, not even its health probes; an index a user
    /// declared is not worth that, and the next checkpoint builds it anyway.
    ///
    /// Preparation is a full ownership/checkpoint epoch, not merely a catalog
    /// flush. Reopened compatible roots need no source scan or publication.
    pub(crate) fn checkpoint_missing_secondary_indices(&self) -> Result<(), StoreError> {
        let missing = {
            let _publication = self.vault_publication_lock.lock();
            let specs = self.secondary_index_specs.read();
            specs.iter().any(|(collection, indices)| {
                indices.iter().any(|(name, spec)| {
                    !spec.deferred
                        && !spec.optional
                        && self
                            .secondary_index(collection, name, &spec.identity)
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
    /// still match, or the tree was published under a definition this one
    /// declares as a predecessor. A miss requires rebuilding or a correct scan
    /// fallback.
    /// This is a physical base index: callers must merge the live WAL overlay.
    pub fn secondary_index(
        &self,
        collection: &str,
        name: &str,
        identity: &SecondaryIndexIdentity,
    ) -> Option<SecondaryIndexRead> {
        let collection = Self::canonical_collection(collection);
        let _publication = self.vault_publication_lock.lock();
        let (published, tree, data) = {
            let trees = self.btree_indices.read();
            let data = trees.get(collection)?;
            let keys = self.key_indices.read();
            let generation = keys.get(collection)?;
            if !generation.source.ptr_eq(&Arc::downgrade(data)) {
                return None;
            }
            let index = generation.secondary.get(name)?;
            (
                index.identity.clone(),
                Arc::clone(&index.tree),
                Arc::clone(data),
            )
        };
        // The publication lock keeps the registration and the published
        // generation consistent. The spec table is read on its own, so this
        // path adds no ordering between it and the generation locks.
        let readable = published == *identity
            || self
                .secondary_index_specs
                .read()
                .get(collection)
                .and_then(|indices| indices.get(name))
                .is_some_and(|spec| {
                    spec.identity == *identity && spec.predecessors.contains(&published)
                });
        readable.then(|| SecondaryIndexRead::with_source(tree, data))
    }

    /// The next checkpoint publishes the removal. Already-pinned readers keep
    /// their old immutable tree until the normal retirement protocol releases it.
    pub fn unregister_secondary_index(&self, collection: &str, name: &str) {
        let collection = Self::canonical_collection(collection);
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
            // The background worker builds a deferred projection over the
            // generation it was declared on. A republished collection is a new
            // generation: that build has just lost its source, and leaving the
            // projection out would republish the collection at every
            // checkpoint that comes before a new build ends.
            if spec.deferred && same_generation.is_some() {
                continue;
            }
            if name == super::count_index::NAME {
                if let Some(tree) = self.checkpoint_count_projection_seed(
                    collection,
                    data.root_page_id(),
                    &spec.identity,
                    pager,
                )? {
                    next.insert(
                        name.clone(),
                        PublishedSecondaryIndex {
                            identity: spec.identity.clone(),
                            tree,
                        },
                    );
                    continue;
                }
            }
            builds.push((name, spec, Self::new_derived_btree(pager)?));
        }
        if builds.is_empty() {
            return Ok(next);
        }
        let source = PagedCollection::new(Arc::clone(data));
        let mut cursor = source.scan()?;
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        let run_bytes = (limit_bytes / 8).min(POSTING_RUNS_BYTES_MAX) / builds.len();
        let mut runs: Vec<PostingRun> = builds.iter().map(|_| PostingRun::default()).collect();
        while let Some(record) = cursor.next_record(limit_bytes)? {
            let (entity, _) = record.decode_with_budget(self.format_version(), limit_bytes)?;
            #[cfg(test)]
            if checkpoint_skips_secondary_index_entity(collection, entity.id.raw()) {
                continue;
            }
            for ((_, spec, tree), run) in builds.iter().zip(&mut runs) {
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
                run.push(&posting[..length + 8]);
                if run.bytes() >= run_bytes {
                    run.load(tree)?;
                }
            }
        }
        for ((_, _, tree), run) in builds.iter().zip(&mut runs) {
            run.load(tree)?;
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

/// Postings buffered for every index a checkpoint rebuilds, in total.
pub(super) const POSTING_RUNS_BYTES_MAX: usize = 64 * 1024 * 1024;

/// Postings of one derived tree, collected in row order and loaded in key order.
///
/// A tree filled one posting at a time pays a root-to-leaf walk and a leaf
/// rewrite per posting, each page proven on the way: about a millisecond per
/// posting, which made the rebuild the cost of the checkpoint. Sorted, the
/// same postings fill each leaf once. A rebuild larger than its run keeps the
/// old path for what follows the first run.
#[derive(Default)]
pub(super) struct PostingRun {
    bytes: Vec<u8>,
    spans: Vec<(u32, u16)>,
    loaded: bool,
}

impl PostingRun {
    pub(super) fn push(&mut self, posting: &[u8]) {
        self.spans
            .push((self.bytes.len() as u32, posting.len() as u16));
        self.bytes.extend_from_slice(posting);
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes.len() + self.spans.len() * std::mem::size_of::<(u32, u16)>()
    }

    pub(super) fn load(&mut self, tree: &BTree) -> Result<(), StoreError> {
        let failed = |error: reddb_engine::storage::btree::BTreeError| {
            StoreError::Io(std::io::Error::other(error.to_string()))
        };
        let bytes = &self.bytes;
        let posting = |&(start, length): &(u32, u16)| {
            &bytes[start as usize..start as usize + usize::from(length)]
        };
        self.spans
            .sort_unstable_by(|a, b| posting(a).cmp(posting(b)));
        if self.loaded {
            for span in &self.spans {
                tree.insert(posting(span), &[]).map_err(failed)?;
            }
        } else {
            for chunk in self.spans.chunks(4096) {
                let items: Vec<_> = chunk
                    .iter()
                    .map(|span| (posting(span).to_vec(), Vec::new()))
                    .collect();
                tree.bulk_insert_sorted(&items).map_err(failed)?;
            }
        }
        self.loaded |= !self.spans.is_empty();
        self.spans.clear();
        self.bytes.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
