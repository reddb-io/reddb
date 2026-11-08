//! Exact visibility projection for payload-free scalar counts.
//!
//! Preparation shares the secondary-index source pass and publication fences.
//! Cost: 17 visibility bytes + 8 physical-ID bytes per version (plus tree slots),
//! O(N) compact reads per count, bounded decode scratch, no resident posting map.
use super::secondary_index::{
    SecondaryIndexIdentity, SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX,
};
use super::*;
use rayon::prelude::*;
use reddb_engine::storage::collection_read::{
    CollectionCursor, EncodedRecord, PagedCollection, PagedCursor,
};
use reddb_engine::storage::visibility::entity_moderation_hidden;
use reddb_engine::transaction::read::{visibility_check, SnapshotContext};
use sha2::{Digest, Sha256};

// Separate from SQL-visible registry names; a different definition can never
// accidentally qualify because reads also verify the full fingerprint.
const NAME: &str = "\0reddb/visibility-count-v1";
const MIXED_BATCH_ROWS: usize = 4096;
const UNIFORM_BATCH_ROWS: usize = 8192;
const DECODE_GROUP_ROWS: usize = 64;
const DECODE_WORKERS: usize = 2;

fn identity(schema: Option<&[String]>) -> SecondaryIndexIdentity {
    let mut digest = Sha256::new();
    digest.update(b"reddb:count:xmin-be64:xmax-be64:moderation-u8:v1");
    digest.update([u8::from(schema.is_some())]);
    for name in schema.unwrap_or_default() {
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
    }
    SecondaryIndexIdentity {
        codec_version: 1,
        definition: digest.finalize().into(),
    }
}

struct VisibilityProjection {
    schema: Option<Arc<Vec<String>>>,
}

fn moderation_hidden(entity: &UnifiedEntity, schema: Option<&[String]>) -> bool {
    use reddb_engine::storage::visibility::{
        MODERATION_STATUS_FIELD, MODERATION_STATUS_PENDING, MODERATION_STATUS_REJECTED,
    };
    if entity_moderation_hidden(entity) {
        return true;
    }
    let EntityData::Row(row) = &entity.data else {
        return false;
    };
    if row.named.is_some() || row.schema.is_some() {
        return false;
    }
    let marker = schema
        .and_then(|schema| {
            schema
                .iter()
                .position(|name| name == MODERATION_STATUS_FIELD)
        })
        .and_then(|position| row.columns.get(position));
    matches!(marker, Some(Value::Text(status)) if status.as_ref() == MODERATION_STATUS_PENDING || status.as_ref() == MODERATION_STATUS_REJECTED)
}
impl SecondaryIndexProjector for VisibilityProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        key[..8].copy_from_slice(&entity.xmin.to_be_bytes());
        key[8..16].copy_from_slice(&entity.xmax.to_be_bytes());
        key[16] = u8::from(moderation_hidden(
            entity,
            self.schema.as_deref().map(Vec::as_slice),
        ));
        Ok(Some(17))
    }
}

fn visible(snapshot: Option<&SnapshotContext>, xmin: u64, xmax: u64, hidden: bool) -> bool {
    !hidden && snapshot.map_or(xmax == 0, |context| visibility_check(context, xmin, xmax))
}

/// A private, retired tree is durably owned after each batch. It is never
/// exposed as a secondary index until the complete source generation has been
/// scanned. A crash leaves it in the ordinary retired-tree recovery ledger.
pub(crate) struct CountBuild {
    collection: String,
    _active: Arc<()>,
    source: Arc<BTree>,
    tree: Arc<BTree>,
    cursor: PagedCursor<Pager>,
    identity: SecondaryIndexIdentity,
    projector: Arc<dyn SecondaryIndexProjector>,
}

impl CountBuild {
    fn project_record(
        &self,
        record: &EncodedRecord,
        version: u32,
        limit: usize,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let (entity, _) = record.decode_with_budget(version, limit)?;
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        let Some(length) = self.projector.project(&entity, &mut key)? else {
            return Ok(None);
        };
        if length > key.len() {
            return Err(StoreError::Serialization(
                "count projection key too large".into(),
            ));
        }
        let mut posting = Vec::with_capacity(length + 8);
        posting.extend_from_slice(&key[..length]);
        posting.extend_from_slice(&entity.id.raw().to_be_bytes());
        Ok(Some(posting))
    }

    /// Decode outside the publication fence. Uniform visibility keys can be
    /// appended together with bounded leaf work; mixed keys retain the smaller
    /// batch to limit the number of distinct leaves touched under the fence.
    pub(crate) fn step(&mut self, store: &UnifiedStore) -> Result<bool, StoreError> {
        let limit = usize::try_from(store.config.resident_memory_bytes_max.unwrap_or(0))
            .unwrap_or(usize::MAX);
        let mut postings = Vec::with_capacity(UNIFORM_BATCH_ROWS);
        let mut finished = false;
        let mut first_key = None::<Vec<u8>>;
        let mut mixed = false;
        let mut scanned = 0;
        let mut parallel = limit >= DECODE_WORKERS * 2;
        while scanned < UNIFORM_BATCH_ROWS {
            let mut trial = parallel.then(|| self.cursor.fork());
            let mut records = Vec::new();
            let mut source_bytes = 0usize;
            let mut end = false;
            // Bound retained encoded records to half the read allowance and
            // divide the other half between the concurrent decoders.
            let input_limit = (limit / 2).min(4 * 1024 * 1024);
            if parallel {
                while records.len() < DECODE_GROUP_ROWS.min(UNIFORM_BATCH_ROWS - scanned) {
                    match trial
                        .as_mut()
                        .expect("parallel cursor")
                        .next_record(input_limit.saturating_sub(source_bytes))
                    {
                        Ok(Some(record)) => {
                            source_bytes += record.bytes.len();
                            records.push(record);
                        }
                        Ok(None) => {
                            end = true;
                            break;
                        }
                        Err(StoreError::ReadLimitExceeded { .. }) => break,
                        Err(error) => return Err(error),
                    }
                }
            }
            let projected = if !records.is_empty() {
                let worker_limit = limit / (DECODE_WORKERS * 2);
                let chunks: Vec<_> = records
                    .par_chunks(DECODE_GROUP_ROWS / DECODE_WORKERS)
                    .map(|chunk| {
                        chunk
                            .iter()
                            .map(|record| {
                                self.project_record(record, store.format_version(), worker_limit)
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .collect();
                chunks
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
                    .map(|chunks| chunks.into_iter().flatten().collect::<Vec<_>>())
            } else {
                None
            };
            let projected = if let Some(projected) = projected {
                projected
            } else if end && records.is_empty() {
                finished = true;
                break;
            } else {
                // Retry without speculative input when a record exceeds the
                // per-worker allowance or a decoder reports an error.
                parallel = false;
                drop(records);
                end = false;
                let Some(record) = self.cursor.next_record(limit)? else {
                    finished = true;
                    break;
                };
                trial = None;
                vec![self.project_record(&record, store.format_version(), limit)?]
            };
            let count = projected.len();
            let mut consumed = 0;
            for posting in projected {
                consumed += 1;
                scanned += 1;
                if let Some(posting) = posting {
                    let key = &posting[..posting.len() - 8];
                    match &first_key {
                        Some(first) if first.as_slice() != key => mixed = true,
                        None => first_key = Some(key.to_vec()),
                        _ => {}
                    }
                    postings.push((posting, Vec::new()));
                } else {
                    mixed = true;
                }
                if scanned >= MIXED_BATCH_ROWS && mixed {
                    break;
                }
            }
            if consumed == count {
                if let Some(trial) = trial {
                    self.cursor = trial;
                }
                if end {
                    finished = true;
                    break;
                }
            } else {
                // A mixed-key boundary can fall inside the speculative group.
                // Advance the original cursor only through the published rows.
                for _ in 0..consumed {
                    if !self.cursor.skip_record()? {
                        return Err(StoreError::Serialization(
                            "count source cursor changed during decoding".into(),
                        ));
                    }
                }
            }
            if scanned >= MIXED_BATCH_ROWS && mixed {
                break;
            }
        }
        // IDs are ordered, but visibility keys need not be. Sorting the
        // bounded batch lets the B-tree coalesce writes to the same leaf.
        postings.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let _publication = store.vault_publication_lock.lock();
        let current = store.btree_indices.read().get(&self.collection).cloned();
        let spec_matches = store
            .secondary_index_specs
            .read()
            .get(&self.collection)
            .and_then(|specs| specs.get(NAME))
            .is_some_and(|spec| spec.deferred && spec.identity == self.identity);
        if !current
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(source, &self.source))
            || !spec_matches
        {
            store.count_builds.write().remove(&self.collection);
            return Ok(true);
        }
        let Some(commit) = &store.commit else {
            store.count_builds.write().remove(&self.collection);
            return Ok(true);
        };
        let pager = store.pager.as_ref().expect("paged source");
        let _ownership = pager.lock_ownership_mutations();
        commit.force_sync().map_err(StoreError::Io)?;
        let epoch = commit
            .next_page_ownership_epoch(store)
            .map_err(StoreError::Io)?;
        commit
            .begin_page_ownership_epoch(store, epoch)
            .map_err(StoreError::Io)?;
        struct Fence<'a> {
            pager: &'a Pager,
            active: bool,
        }
        impl Drop for Fence<'_> {
            fn drop(&mut self) {
                if self.active {
                    self.pager.fence_interrupted_ownership();
                }
            }
        }
        let mut fence = Fence {
            pager,
            active: true,
        };
        self.tree
            .bulk_insert_sorted(&postings)
            .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
        #[cfg(test)]
        if std::env::var_os("REDDB_TEST_EXIT_DURING_COUNT_BATCH").is_some() {
            std::process::exit(183);
        }
        if finished {
            let mut keys = store.key_indices.write();
            let Some(generation) = keys.get_mut(&self.collection) else {
                return Err(StoreError::Serialization(
                    "count source generation disappeared".into(),
                ));
            };
            if !generation.source.ptr_eq(&Arc::downgrade(&self.source)) {
                return Err(StoreError::Serialization(
                    "count source generation changed".into(),
                ));
            }
            let previous = generation.secondary.insert(
                NAME.to_string(),
                super::secondary_index::PublishedSecondaryIndex {
                    identity: self.identity.clone(),
                    tree: Arc::clone(&self.tree),
                },
            );
            drop(keys);
            if let Some(previous) = previous {
                store.retire_btree(previous.tree);
            }
        } else {
            store.retire_btree(Arc::clone(&self.tree));
        }
        store.flush_paged_registry()?;
        #[cfg(test)]
        if std::env::var_os("REDDB_TEST_EXIT_AFTER_COUNT_BATCH_PUBLICATION").is_some() {
            std::process::exit(184);
        }
        commit
            .finish_count_batch_ownership_epoch(store, epoch)
            .map_err(StoreError::Io)?;
        fence.active = false;
        if finished {
            store.count_builds.write().remove(&self.collection);
        }
        Ok(finished)
    }
}

impl UnifiedStore {
    pub(crate) fn start_count_build(
        &self,
        collection: &str,
    ) -> Result<Option<CountBuild>, StoreError> {
        let _publication = self.vault_publication_lock.lock();
        let collection = Self::canonical_collection(collection);
        let Some(spec) = self
            .secondary_index_specs
            .read()
            .get(collection)
            .and_then(|specs| specs.get(NAME))
            .cloned()
        else {
            return Ok(None);
        };
        if !spec.deferred
            || self
                .secondary_index(collection, NAME, &spec.identity)
                .is_some()
        {
            return Ok(None);
        }
        let Some(source) = self.btree_indices.read().get(collection).cloned() else {
            return Ok(None);
        };
        if source.root_page_id() == 0 || self.commit.is_none() || self.is_read_only() {
            return Ok(None);
        }
        if !self
            .key_indices
            .read()
            .get(collection)
            .is_some_and(|generation| generation.source.ptr_eq(&Arc::downgrade(&source)))
        {
            return Ok(None);
        }
        let active = Arc::new(());
        let mut builds = self.count_builds.write();
        if builds
            .get(collection)
            .and_then(std::sync::Weak::upgrade)
            .is_some()
        {
            return Ok(None);
        }
        builds.insert(collection.to_owned(), Arc::downgrade(&active));
        drop(builds);
        let source_view = PagedCollection::new(Arc::clone(&source));
        let cursor = source_view.scan()?;
        Ok(Some(CountBuild {
            collection: collection.to_owned(),
            _active: active,
            source,
            tree: Arc::new(BTree::new(Arc::clone(
                self.pager.as_ref().expect("paged source"),
            ))),
            cursor,
            identity: spec.identity,
            projector: spec.projector,
        }))
    }

    /// The compact visibility inventory belongs to this exact immutable base.
    /// GC still rechecks any reclaimable posting against the current row.
    pub(super) fn count_visibility_cursor(
        &self,
        collection: &str,
        manager: &SegmentManager,
        after: Option<&[u8]>,
    ) -> Result<
        Option<reddb_engine::storage::secondary_index_read::SecondaryRangeCursor<Pager>>,
        StoreError,
    > {
        let Some(root) = manager.backing_root_page() else {
            return Ok(None);
        };
        if self.collection_root_page(collection) != Some(root) {
            return Ok(None);
        }
        let schema = manager.column_schema();
        self.secondary_index(
            collection,
            NAME,
            &identity(schema.as_deref().map(Vec::as_slice)),
        )
        .map(|index| index.resume_postings(after))
        .transpose()
    }

    /// Only the opt-in cold runtime prepares these indexes. Ordinary stores
    /// retain their current formats and scans. Missing projections are safe
    /// scan fallbacks until the next preparation checkpoint.
    pub(crate) fn register_count_index(&self, collection: &str) -> Result<(), StoreError> {
        let schema = self
            .get_collection(collection)
            .and_then(|manager| manager.column_schema());
        self.register_secondary_index(
            collection,
            NAME,
            identity(schema.as_deref().map(Vec::as_slice)),
            Arc::new(VisibilityProjection { schema }),
        )
    }

    /// Historical COUNT projections are built incrementally by the background
    /// worker. A first publication has no disk history to backfill: include its
    /// projection in the same checkpoint so GC can use it immediately.
    pub(crate) fn register_count_index_deferred(&self, collection: &str) -> Result<(), StoreError> {
        let _publication = self.vault_publication_lock.lock();
        self.register_count_index(collection)?;
        let collection = Self::canonical_collection(collection);
        let manager = self
            .get_collection(collection)
            .expect("registered collection");
        let historical = manager
            .backing_root_page()
            .is_some_and(|root| manager.backing_count_at_root(root) != Some(0));
        self.secondary_index_specs
            .write()
            .get_mut(collection)
            .and_then(|specs| specs.get_mut(NAME))
            .expect("registered count projection")
            .deferred = historical;
        Ok(())
    }

    /// None means the ordinary entity scan is required. In particular SSI
    /// readers retain per-entity logical read tracking on that existing path.
    #[doc(hidden)]
    pub fn try_count_from_index(
        &self,
        collection: &str,
        snapshot: Option<&SnapshotContext>,
    ) -> Result<Option<u64>, StoreError> {
        if snapshot.is_some_and(|context| context.serializable_reader.is_some()) {
            return Ok(None);
        }
        let collection = Self::canonical_collection(collection);
        if self
            .count_builds
            .read()
            .get(collection)
            .and_then(std::sync::Weak::upgrade)
            .is_some()
        {
            return Ok(None);
        }
        let _publication = self.vault_publication_lock.lock();
        let Some(manager) = self.get_collection(collection) else {
            return Ok(None);
        };
        let Some(root) = manager.backing_root_page() else {
            return Ok(None);
        };
        if self.collection_root_page(collection) != Some(root) {
            return Ok(None);
        }
        let schema = manager.column_schema();
        let Some(index) = self.secondary_index(
            collection,
            NAME,
            &identity(schema.as_deref().map(Vec::as_slice)),
        ) else {
            return Ok(None);
        };
        let mut count = 0u64;
        index.try_for_each(|key, id| {
            if key.len() != 17 || key[16] > 1 {
                return Err(StoreError::Serialization(
                    "invalid COUNT visibility projection".into(),
                ));
            }
            if !manager.backing_row_is_shadowed(id) {
                let xmin = u64::from_be_bytes(key[..8].try_into().expect("eight-byte xmin"));
                let xmax = u64::from_be_bytes(key[8..16].try_into().expect("eight-byte xmax"));
                count += u64::from(visible(snapshot, xmin, xmax, key[16] != 0));
            }
            Ok(())
        })?;
        // Resident iteration contains only WAL changes. Replacements were
        // excluded above, so no dedup set or payload clone is necessary.
        manager.for_each_entity(|entity| {
            count += u64::from(visible(
                snapshot,
                entity.xmin,
                entity.xmax,
                moderation_hidden(entity, schema.as_deref().map(Vec::as_slice)),
            ));
            true
        });
        Ok(Some(count))
    }
}

#[cfg(test)]
mod tests;
