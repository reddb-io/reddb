//! Exact visibility projection for payload-free scalar counts.
//!
//! Preparation emits immutable, checksummed components outside the main pager.
//! Cost: 17 visibility bytes + 8 physical-ID bytes per version, O(N) compact
//! reads per count, bounded decode scratch, no resident posting map.
use super::count_projection::{
    CountProjectionBinding, CountProjectionOpen, CountProjectionReader, CountProjectionWriter,
};
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
use std::fmt::Write as _;
use std::path::PathBuf;

// Separate from SQL-visible registry names; a different definition can never
// accidentally qualify because reads also verify the full fingerprint.
pub(super) const NAME: &str = "\0reddb/visibility-count-v1";
const MIXED_BATCH_ROWS: usize = 64;
const UNIFORM_BATCH_ROWS: usize = 384;
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

#[derive(Clone)]
pub(super) struct PublishedCountProjection {
    identity: SecondaryIndexIdentity,
    source: std::sync::Weak<BTree>,
    reader: CountProjectionReader,
}

/// A private, derived component is sealed one bounded chunk at a time. It is
/// never exposed until a completion footer is durable and its rename commits.
/// No main pager page or catalog generation changes while this builder runs.
pub(crate) struct CountBuild {
    collection: String,
    source: Arc<BTree>,
    cursor: PagedCursor<Pager>,
    writer: Option<CountProjectionWriter>,
    binding: CountProjectionBinding,
    identity: SecondaryIndexIdentity,
    projector: Arc<dyn SecondaryIndexProjector>,
}

impl CountBuild {
    fn project_record(
        &self,
        record: &EncodedRecord,
        version: u32,
        limit: usize,
    ) -> Result<Option<[u8; 25]>, StoreError> {
        let (entity, _) = record.decode_with_budget(version, limit)?;
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        let Some(length) = self.projector.project(&entity, &mut key)? else {
            return Ok(None);
        };
        if length != 17 {
            return Err(StoreError::Serialization(
                "COUNT visibility projection must contain 17 bytes".into(),
            ));
        }
        let mut posting = [0; 25];
        posting[..17].copy_from_slice(&key[..17]);
        posting[17..].copy_from_slice(&entity.id.raw().to_be_bytes());
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
        let mut last_source_id = None;
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
                                    .map(|posting| (record.id, posting))
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
                vec![(
                    record.id,
                    self.project_record(&record, store.format_version(), limit)?,
                )]
            };
            let count = projected.len();
            let mut consumed = 0;
            for (source_id, posting) in projected {
                consumed += 1;
                scanned += 1;
                last_source_id = Some(source_id.raw());
                if let Some(posting) = posting {
                    let key = &posting[..17];
                    match &first_key {
                        Some(first) if first.as_slice() != key => mixed = true,
                        None => first_key = Some(key.to_vec()),
                        _ => {}
                    }
                    postings.push(posting);
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
        if !store.count_build_matches(self)? {
            if let Some(writer) = self.writer.take() {
                writer.discard();
            }
            return Ok(true);
        }
        if scanned != 0 {
            self.writer
                .as_mut()
                .expect("active COUNT projection writer")
                .append(
                    scanned,
                    last_source_id.expect("scanned COUNT source identity"),
                    &postings,
                )
                .map_err(StoreError::Io)?;
        }
        if finished {
            let reader = self
                .writer
                .take()
                .expect("active COUNT projection writer")
                .finish()
                .map_err(StoreError::Io)?;
            let _publication = store.vault_publication_lock.lock();
            let publish = store.count_build_matches_locked(self)?;
            if publish {
                let published_path = reader.path().to_path_buf();
                let replaced = store.count_projections.write().insert(
                    self.collection.clone(),
                    PublishedCountProjection {
                        identity: self.identity.clone(),
                        source: Arc::downgrade(&self.source),
                        reader,
                    },
                );
                if let Some(replaced) = replaced {
                    if replaced.reader.path() != published_path {
                        let _ = std::fs::remove_file(replaced.reader.path());
                    }
                }
                let previous = store
                    .key_indices
                    .write()
                    .get_mut(&self.collection)
                    .and_then(|generation| generation.secondary.remove(NAME));
                if let Some(previous) = previous {
                    store.retire_btree(previous.tree);
                    store.mark_paged_registry_dirty();
                }
            } else {
                let _ = std::fs::remove_file(reader.path());
            }
        }
        #[cfg(test)]
        if std::env::var_os("REDDB_TEST_EXIT_AFTER_COUNT_BATCH_PUBLICATION").is_some() {
            std::process::exit(184);
        }
        Ok(finished)
    }
}

impl UnifiedStore {
    fn count_projection_binding(
        &self,
        collection: &str,
        source: &Arc<BTree>,
        identity: &SecondaryIndexIdentity,
    ) -> Result<CountProjectionBinding, StoreError> {
        let path = self.db_path.as_ref().ok_or_else(|| {
            StoreError::Serialization("COUNT projection requires a database path".into())
        })?;
        let pager = self
            .pager
            .as_ref()
            .ok_or_else(|| StoreError::Serialization("COUNT projection requires a pager".into()))?;
        let header = pager
            .header()
            .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
        let catalog_page = pager
            .read_page(1)
            .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
        let metadata = std::fs::metadata(path).map_err(StoreError::Io)?;
        let mut digest = Sha256::new();
        digest.update(b"reddb:derived-count-component:v1");
        digest.update(path.to_string_lossy().as_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            digest.update(metadata.dev().to_le_bytes());
            digest.update(metadata.ino().to_le_bytes());
            digest.update(metadata.mtime().to_le_bytes());
            digest.update(metadata.mtime_nsec().to_le_bytes());
            digest.update(metadata.ctime().to_le_bytes());
            digest.update(metadata.ctime_nsec().to_le_bytes());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            digest.update(metadata.creation_time().to_le_bytes());
            digest.update(metadata.last_write_time().to_le_bytes());
        }
        #[cfg(not(any(unix, windows)))]
        {
            digest.update(metadata.len().to_le_bytes());
        }
        digest.update(header.version.to_le_bytes());
        digest.update(header.page_size.to_le_bytes());
        digest.update(header.schema_version.to_le_bytes());
        digest.update(header.checkpoint_lsn.to_le_bytes());
        digest.update(header.physical.sequence.to_le_bytes());
        digest.update(header.physical.manifest_checksum.to_le_bytes());
        digest.update(header.physical.collection_roots_checksum.to_le_bytes());
        digest.update(header.physical.catalog_checksum.to_le_bytes());
        digest.update(catalog_page.content());
        digest.update(source.root_page_id().to_le_bytes());
        digest.update(identity.codec_version.to_le_bytes());
        digest.update(identity.definition);
        digest.update((collection.len() as u64).to_le_bytes());
        digest.update(collection.as_bytes());
        Ok(CountProjectionBinding {
            generation: digest.finalize().into(),
            source_root: source.root_page_id(),
            identity: identity.clone(),
            collection: collection.to_owned(),
        })
    }

    fn count_projection_paths(
        &self,
        binding: &CountProjectionBinding,
    ) -> Result<(PathBuf, PathBuf), StoreError> {
        let path = self.db_path.as_ref().ok_or_else(|| {
            StoreError::Serialization("COUNT projection requires a database path".into())
        })?;
        let mut collection_digest = Sha256::new();
        collection_digest.update(binding.collection.as_bytes());
        let collection_digest = collection_digest.finalize();
        let mut collection_tag = String::with_capacity(16);
        for byte in &collection_digest[..8] {
            write!(&mut collection_tag, "{byte:02x}").expect("write collection tag");
        }
        let mut generation_tag = String::with_capacity(32);
        for byte in &binding.generation[..16] {
            write!(&mut generation_tag, "{byte:02x}").expect("write generation tag");
        }
        let mut final_name = path.as_os_str().to_os_string();
        final_name.push(format!(".count-{collection_tag}-{generation_tag}.rdci"));
        let final_path = PathBuf::from(final_name);
        let mut building_name = final_path.as_os_str().to_os_string();
        building_name.push(".building");
        Ok((PathBuf::from(building_name), final_path))
    }

    fn count_build_matches(&self, build: &CountBuild) -> Result<bool, StoreError> {
        let _publication = self.vault_publication_lock.lock();
        self.count_build_matches_locked(build)
    }

    fn count_build_matches_locked(&self, build: &CountBuild) -> Result<bool, StoreError> {
        let current = self.btree_indices.read().get(&build.collection).cloned();
        let spec_matches = self
            .secondary_index_specs
            .read()
            .get(&build.collection)
            .and_then(|specs| specs.get(NAME))
            .is_some_and(|spec| spec.deferred && spec.identity == build.identity);
        if !current
            .as_ref()
            .is_some_and(|source| Arc::ptr_eq(source, &build.source))
            || !spec_matches
        {
            return Ok(false);
        }
        let current_binding =
            self.count_projection_binding(&build.collection, &build.source, &build.identity)?;
        Ok(current_binding == build.binding)
    }

    fn load_count_projection_locked(
        &self,
        collection: &str,
        source: &Arc<BTree>,
        identity: &SecondaryIndexIdentity,
    ) -> Result<bool, StoreError> {
        let binding = self.count_projection_binding(collection, source, identity)?;
        let (_, final_path) = self.count_projection_paths(&binding)?;
        let reader = match CountProjectionReader::open(&final_path, &binding) {
            Ok(reader) => reader,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(_) => {
                let _ = std::fs::remove_file(final_path);
                return Ok(false);
            }
        };
        let installed_path = reader.path().to_path_buf();
        let replaced = self.count_projections.write().insert(
            collection.to_owned(),
            PublishedCountProjection {
                identity: identity.clone(),
                source: Arc::downgrade(source),
                reader,
            },
        );
        if let Some(replaced) = replaced {
            if replaced.reader.path() != installed_path {
                let _ = std::fs::remove_file(replaced.reader.path());
            }
        }
        Ok(true)
    }

    pub(super) fn remove_count_projection(&self, collection: &str) {
        if let Some(projection) = self.count_projections.write().remove(collection) {
            let _ = std::fs::remove_file(projection.reader.path());
        }
    }

    pub(super) fn reconcile_count_projections_after_checkpoint(
        &self,
        unchanged: &std::collections::HashSet<String>,
    ) {
        let previous = std::mem::take(&mut *self.count_projections.write());
        let trees = self.btree_indices.read();
        let mut next = HashMap::with_capacity(previous.len());
        for (collection, mut projection) in previous {
            let Some(source) = trees.get(&collection) else {
                let _ = std::fs::remove_file(projection.reader.path());
                continue;
            };
            if !unchanged.contains(&collection)
                || !projection.source.ptr_eq(&Arc::downgrade(source))
            {
                let _ = std::fs::remove_file(projection.reader.path());
                continue;
            }
            let binding =
                match self.count_projection_binding(&collection, source, &projection.identity) {
                    Ok(binding) => binding,
                    Err(error) => {
                        tracing::warn!(%error, %collection, "COUNT projection rebind failed");
                        continue;
                    }
                };
            let (_, final_path) = match self.count_projection_paths(&binding) {
                Ok(paths) => paths,
                Err(error) => {
                    tracing::warn!(%error, %collection, "COUNT projection path rebind failed");
                    continue;
                }
            };
            let previous_path = projection.reader.path().to_path_buf();
            projection.reader = match projection.reader.rebind(binding, final_path) {
                Ok(reader) => reader,
                Err(error) => {
                    tracing::warn!(%error, %collection, "COUNT projection component rebind failed");
                    let _ = std::fs::remove_file(previous_path);
                    continue;
                }
            };
            projection.source = Arc::downgrade(source);
            next.insert(collection, projection);
        }
        drop(trees);
        *self.count_projections.write() = next;
    }

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
        let binding = self.count_projection_binding(collection, &source, &spec.identity)?;
        if self
            .count_projections
            .read()
            .get(collection)
            .is_some_and(|projection| {
                projection.identity == spec.identity
                    && projection.source.ptr_eq(&Arc::downgrade(&source))
                    && projection.reader.binding() == &binding
            })
        {
            return Ok(None);
        }
        if self.load_count_projection_locked(collection, &source, &spec.identity)? {
            return Ok(None);
        }
        let (building_path, final_path) = self.count_projection_paths(&binding)?;
        let writer =
            match CountProjectionWriter::open_or_create(building_path, final_path, binding.clone())
                .map_err(StoreError::Io)?
            {
                CountProjectionOpen::Ready(reader) => {
                    self.count_projections.write().insert(
                        collection.to_owned(),
                        PublishedCountProjection {
                            identity: spec.identity,
                            source: Arc::downgrade(&source),
                            reader,
                        },
                    );
                    return Ok(None);
                }
                CountProjectionOpen::Building(writer) => writer,
            };
        let source_view = PagedCollection::new(Arc::clone(&source));
        let cursor = match writer.last_source_id() {
            Some(id) => source_view.scan_after(EntityId::new(id))?,
            None => source_view.scan()?,
        };
        Ok(Some(CountBuild {
            collection: collection.to_owned(),
            source,
            cursor,
            writer: Some(writer),
            binding,
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
        if historical {
            let source = self.btree_indices.read().get(collection).cloned();
            if let Some(source) = source {
                let schema = manager.column_schema();
                let projection_identity = identity(schema.as_deref().map(Vec::as_slice));
                if self
                    .secondary_index(collection, NAME, &projection_identity)
                    .is_none()
                {
                    let _ = self.load_count_projection_locked(
                        collection,
                        &source,
                        &projection_identity,
                    )?;
                }
            }
        }
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
        let projection_identity = identity(schema.as_deref().map(Vec::as_slice));
        let current_source = self.btree_indices.read().get(collection).cloned();
        let current_binding = current_source
            .as_ref()
            .map(|source| self.count_projection_binding(collection, source, &projection_identity))
            .transpose()?;
        let mut count = 0u64;
        let mut component = current_source.as_ref().and_then(|source| {
            self.count_projections
                .read()
                .get(collection)
                .filter(|projection| {
                    projection.identity == projection_identity
                        && projection.source.ptr_eq(&Arc::downgrade(source))
                        && current_binding.as_ref() == Some(projection.reader.binding())
                })
                .cloned()
        });
        if component.is_none() {
            self.remove_count_projection(collection);
            if let Some(source) = current_source.as_ref() {
                let _ =
                    self.load_count_projection_locked(collection, source, &projection_identity)?;
                component = self
                    .count_projections
                    .read()
                    .get(collection)
                    .filter(|projection| {
                        projection.identity == projection_identity
                            && projection.source.ptr_eq(&Arc::downgrade(source))
                            && current_binding.as_ref() == Some(projection.reader.binding())
                    })
                    .cloned();
            }
        }
        if let Some(component) = component {
            if let Err(error) = component.reader.try_for_each(|record| {
                if record[16] > 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid COUNT visibility projection",
                    ));
                }
                let id = EntityId::new(u64::from_be_bytes(
                    record[17..25].try_into().expect("eight-byte identity"),
                ));
                if !manager.backing_row_is_shadowed(id) {
                    let xmin = u64::from_be_bytes(record[..8].try_into().expect("eight-byte xmin"));
                    let xmax =
                        u64::from_be_bytes(record[8..16].try_into().expect("eight-byte xmax"));
                    count += u64::from(visible(snapshot, xmin, xmax, record[16] != 0));
                }
                Ok(())
            }) {
                tracing::warn!(
                    %error,
                    path = %component.reader.path().display(),
                    "discarding invalid derived COUNT projection"
                );
                let _ = std::fs::remove_file(component.reader.path());
                self.count_projections.write().remove(collection);
                return Ok(None);
            }
        } else {
            let Some(index) = self.secondary_index(collection, NAME, &projection_identity) else {
                return Ok(None);
            };
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
        }
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
