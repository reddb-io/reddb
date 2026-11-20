//! HASH equality over generation-bound secondary pages and the live WAL overlay.
//!
//! One lookup holds bounded decode scratch and an admitted candidate allocation.
//! It reads only matching digest postings plus resident changes; full raw keys
//! are rechecked, so a digest collision cannot create a match. No posting map is
//! hydrated on reopen. Missing generations use a fallible bounded physical scan.
use super::index_store::{
    bounded_hash_index_key, bounded_hash_index_key_from, index_field_value_from, RegisteredIndex,
};
use super::{RedDBRuntime, RuntimeInner};
use crate::storage::unified::hash_index::{HashCandidates, HashIndexError, PagedHashIndex};
use crate::storage::unified::store::secondary_index::{
    SecondaryIndexIdentity, SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX,
};
use crate::storage::{EntityData, EntityId, EntityKind, StoreError, UnifiedEntity};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Weak};

/// Resident changes since the published index was built, keyed like the
/// lookup: the full raw key, not its digest. Mirrors the paged BTREE overlay
/// and is fed by the same index hooks, so a lookup never has to walk the
/// collection's resident rows.
#[derive(Default)]
struct OverlayHash {
    postings: std::collections::HashMap<Vec<u8>, std::collections::BTreeSet<EntityId>>,
    /// Rows whose key could not be captured; they force the scanning lookup.
    scan_required: std::collections::HashSet<EntityId>,
    /// Too many rows changed without a publication: the overlay stopped
    /// tracking and every lookup scans until the next one rebuilds it.
    overflowed: bool,
    /// Every row changed since publication and its current key, if any. A
    /// published posting of a touched row is superseded by the overlay.
    touched: std::collections::HashMap<EntityId, Option<Vec<u8>>>,
    /// The overlay was seeded with every row of a collection that had no
    /// disk base yet, so it answers lookups alone until the first
    /// publication. Cleared with the overlay.
    holds_every_row: bool,
}

/// Rows one index tracks between publications. Past this the overlay gives up
/// rather than grow without bound on a node that never checkpoints.
const OVERLAY_TOUCHED_MAX: usize = 1_000_000;

impl OverlayHash {
    /// True while postings plus this overlay answer a lookup on their own.
    fn is_authoritative(&self) -> bool {
        !self.overflowed && self.scan_required.is_empty()
    }

    /// Stops tracking once too many rows changed. Returns whether tracking
    /// is still on.
    fn admit(&mut self, entity_id: EntityId) -> bool {
        if self.overflowed {
            return false;
        }
        if self.touched.len() >= OVERLAY_TOUCHED_MAX && !self.touched.contains_key(&entity_id) {
            *self = Self {
                overflowed: true,
                ..Self::default()
            };
            return false;
        }
        true
    }

    fn remove_current(&mut self, entity_id: EntityId) {
        self.scan_required.remove(&entity_id);
        let Some(Some(key)) = self.touched.remove(&entity_id) else {
            return;
        };
        let emptied = self.postings.get_mut(&key).is_some_and(|ids| {
            ids.remove(&entity_id);
            ids.is_empty()
        });
        if emptied {
            self.postings.remove(&key);
        }
    }

    fn insert(&mut self, key: Vec<u8>, entity_id: EntityId) {
        if !self.admit(entity_id) {
            return;
        }
        self.remove_current(entity_id);
        self.postings
            .entry(key.clone())
            .or_default()
            .insert(entity_id);
        self.touched.insert(entity_id, Some(key));
    }

    fn remove(&mut self, entity_id: EntityId) {
        if !self.admit(entity_id) {
            return;
        }
        self.remove_current(entity_id);
        self.touched.insert(entity_id, None);
    }

    fn require_scan(&mut self, entity_id: EntityId) {
        if !self.admit(entity_id) {
            return;
        }
        self.remove_current(entity_id);
        self.scan_required.insert(entity_id);
        self.touched.insert(entity_id, None);
    }

    fn memory_bytes(&self) -> usize {
        let postings = self
            .postings
            .iter()
            .map(|(key, ids)| {
                key.capacity()
                    .saturating_add(ids.len().saturating_mul(std::mem::size_of::<EntityId>()))
                    .saturating_add(96)
            })
            .sum::<usize>();
        let touched = self
            .touched
            .values()
            .map(|key| key.as_ref().map_or(0, Vec::capacity).saturating_add(48))
            .sum::<usize>();
        postings
            .saturating_add(touched)
            .saturating_add(self.scan_required.len().saturating_mul(48))
    }
}

#[cfg(test)]
thread_local! {
    /// Lookups on this thread that had to walk the collection instead of
    /// using postings.
    static SCANNING_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) struct HashProjection {
    columns: Vec<String>,
    key_bytes_max: usize,
    schema: Option<Arc<Vec<String>>>,
    #[cfg(test)]
    forced_digest: Option<[u8; 32]>,
}

impl HashProjection {
    fn digest(&self, key: &[u8]) -> [u8; 32] {
        #[cfg(test)]
        if let Some(digest) = self.forced_digest {
            return digest;
        }
        Sha256::digest(key).into()
    }
    fn raw_key(
        &self,
        entity: &UnifiedEntity,
        bytes_max: usize,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        if entity.xmax != 0 || !matches!(entity.kind, EntityKind::TableRow { .. }) {
            return Ok(None);
        }
        let EntityData::Row(row) = &entity.data else {
            return Ok(None);
        };
        if let Some(named) = &row.named {
            bounded_hash_index_key_from(
                &self.columns,
                bytes_max.min(self.key_bytes_max),
                |column| index_field_value_from(column, |name| named.get(name)),
            )
        } else if let Some(schema) = row.schema.as_ref().or(self.schema.as_ref()) {
            bounded_hash_index_key_from(
                &self.columns,
                bytes_max.min(self.key_bytes_max),
                |column| {
                    index_field_value_from(column, |name| {
                        schema
                            .iter()
                            .position(|candidate| candidate == name)
                            .and_then(|position| row.columns.get(position))
                    })
                },
            )
        } else {
            Ok(None)
        }
    }

    /// The key a row's field snapshot projects to. `Ok(None)` when the
    /// snapshot does not carry the whole key (partial update hooks).
    fn raw_key_from_fields(
        &self,
        fields: &[(String, reddb_types::Value)],
    ) -> Result<Option<Vec<u8>>, StoreError> {
        bounded_hash_index_key_from(&self.columns, self.key_bytes_max, |column| {
            index_field_value_from(column, |name| {
                fields
                    .iter()
                    .find(|(candidate, _)| candidate == name)
                    .map(|(_, value)| value)
            })
        })
    }

    fn probe_workspace_factor(&self) -> usize {
        if self.columns.len() > 1
            || self.columns.iter().any(|column| column.contains('.'))
            || self
                .schema
                .as_ref()
                .is_none_or(|schema| schema.iter().any(|column| column == "body"))
        {
            3
        } else {
            2
        }
    }
}

impl SecondaryIndexProjector for HashProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let Some(raw) = self.raw_key(entity, self.key_bytes_max)? else {
            return Ok(None);
        };
        key[..32].copy_from_slice(&self.digest(&raw));
        Ok(Some(32))
    }
}

fn projection_identity(columns: &[String], schema: Option<&[String]>) -> SecondaryIndexIdentity {
    let codec_version =
        crate::storage::unified::store::secondary_index::runtime_secondary_index_codec_version();
    let mut digest = Sha256::new();
    digest.update(match codec_version {
        2 => b"runtime-current-hash-sha256-v2".as_slice(),
        _ => b"runtime-current-hash-sha256-v3".as_slice(),
    });
    digest.update([u8::from(schema.is_some())]);
    for names in [columns, schema.unwrap_or(&[])] {
        digest.update((names.len() as u64).to_le_bytes());
        for name in names {
            digest.update((name.len() as u64).to_le_bytes());
            digest.update(name.as_bytes());
        }
    }
    SecondaryIndexIdentity {
        codec_version,
        definition: digest.finalize().into(),
    }
}

struct RuntimeHashIndex {
    runtime: Weak<RuntimeInner>,
    collection: String,
    name: String,
    identity: SecondaryIndexIdentity,
    projection: Arc<HashProjection>,
    /// Rows removed since the published index was built. Their postings
    /// dangle by construction and say nothing about the rest of the index.
    retired: parking_lot::RwLock<std::collections::HashSet<EntityId>>,
    overlay: parking_lot::RwLock<OverlayHash>,
}

fn read_error(error: impl std::fmt::Display) -> HashIndexError {
    HashIndexError::Read(error.to_string())
}

fn storage_error(error: StoreError) -> HashIndexError {
    match error {
        StoreError::ReadLimitExceeded {
            required_bytes,
            limit_bytes,
        } => HashIndexError::ReadLimit {
            required_bytes,
            limit_bytes,
        },
        other => read_error(other),
    }
}

impl PagedHashIndex for RuntimeHashIndex {
    fn note_retired(&self, entity_id: EntityId) {
        self.retired.write().insert(entity_id);
    }

    fn clear_retired(&self) {
        self.retired.write().clear();
        *self.overlay.write() = OverlayHash::default();
    }

    fn overlay_insert(&self, entity_id: EntityId, fields: &[(String, reddb_types::Value)]) {
        match self.projection.raw_key_from_fields(fields) {
            Ok(Some(key)) => self.overlay.write().insert(key, entity_id),
            // A partial snapshot says nothing about the whole key.
            Ok(None) => {}
            Err(_) => self.overlay.write().require_scan(entity_id),
        }
    }

    fn overlay_delete(&self, entity_id: EntityId, fields: &[(String, reddb_types::Value)]) {
        match self.projection.raw_key_from_fields(fields) {
            Ok(Some(_)) => self.overlay.write().remove(entity_id),
            // Update hooks also publish one changed column at a time; a partial
            // key must not erase the full-row projection `overlay_update` set.
            Ok(None) => {}
            Err(_) => self.overlay.write().require_scan(entity_id),
        }
    }

    fn overlay_update(
        &self,
        entity_id: EntityId,
        old_fields: &[(String, reddb_types::Value)],
        new_fields: &[(String, reddb_types::Value)],
    ) {
        let old_key = self.projection.raw_key_from_fields(old_fields);
        let new_key = self.projection.raw_key_from_fields(new_fields);
        match (old_key, new_key) {
            (Ok(old), Ok(new)) if old == new => {}
            (_, Ok(Some(key))) => self.overlay.write().insert(key, entity_id),
            (_, Ok(None)) => self.overlay.write().remove(entity_id),
            (_, Err(_)) => self.overlay.write().require_scan(entity_id),
        }
    }

    fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of::<HashProjection>()
            + 32 // Arc control blocks for the provider and projection.
            + self.collection.len()
            + self.name.len()
            + self
                .projection
                .columns
                .iter()
                .map(|s| s.len() + 24)
                .sum::<usize>()
            + self.projection.schema.as_ref().map_or(0, |schema| {
                schema.iter().map(|s| s.len() + 24).sum::<usize>()
            })
            + self.overlay.read().memory_bytes()
    }

    fn lookup(&self, key: &[u8]) -> Result<HashCandidates, HashIndexError> {
        let inner = self
            .runtime
            .upgrade()
            .ok_or_else(|| read_error("runtime closed"))?;
        let runtime = RedDBRuntime {
            inner,
            background_tasks: None,
        };
        // Small lookups start with a bounded allowance. A row can be larger
        // than its indexed key, so retry the exact reported decode requirement
        // up to the per-row share of the process budget.
        let ceiling = usize::try_from(runtime.memory_accounting().budget().resolved_bytes / 4)
            .unwrap_or(usize::MAX)
            .max(1);
        let mut limit = ceiling.min(64 * 1024);
        loop {
            match self.lookup_with_limit(&runtime, key, limit) {
                Err(HashIndexError::ReadLimit { required_bytes, .. })
                    if required_bytes > limit && required_bytes <= ceiling =>
                {
                    limit = required_bytes;
                }
                result => return result,
            }
        }
    }
}

impl RuntimeHashIndex {
    fn lookup_with_limit(
        &self,
        runtime: &RedDBRuntime,
        key: &[u8],
        limit: usize,
    ) -> Result<HashCandidates, HashIndexError> {
        let store = runtime.db().store();
        let manager = store
            .get_collection(&self.collection)
            .ok_or_else(|| read_error("indexed collection missing"))?;
        // Admission samples managers; finish it before taking publication or
        // source locks. The output owner outlives both scanning and this runtime.
        let (_scratch, read_bytes_max) = runtime
            .reserve_index_probe_with_limit(limit, self.projection.probe_workspace_factor())
            .map_err(read_error)?;
        let matches = |entity: &UnifiedEntity| -> Result<bool, HashIndexError> {
            let required_bytes = crate::storage::unified::memory_size::entity_bytes(entity);
            if required_bytes > read_bytes_max {
                return Err(HashIndexError::ReadLimit {
                    required_bytes,
                    limit_bytes: read_bytes_max,
                });
            }
            Ok(self
                .projection
                .raw_key(entity, read_bytes_max)
                .map_err(storage_error)?
                .as_deref()
                == Some(key))
        };
        // Freeze publication across base capture, source recheck and overlay.
        // Native handles additionally pin both trees against page reclamation.
        let _publication = store.vault_publication_lock.lock();
        let index = store.secondary_index(&self.collection, &self.name, &self.identity);
        let scan = |accept: &mut dyn FnMut(&UnifiedEntity) -> Result<(), HashIndexError>| {
            if let Some(index) = &index {
                let mut dangling_posting = false;
                let mut cursor = index
                    .candidates(&self.projection.digest(key))
                    .map_err(storage_error)?;
                while let Some(id) = cursor.next_id().map_err(storage_error)? {
                    if let Some(entity) =
                        manager.try_get(id, read_bytes_max).map_err(storage_error)?
                    {
                        accept(&entity)?;
                    } else if !self.retired.read().contains(&id) {
                        // Not a row this index saw removed: the published
                        // postings cannot be trusted to be complete.
                        dangling_posting = true;
                    }
                }
                let mut failure = None;
                if dangling_posting {
                    manager
                        .try_for_each_entity(read_bytes_max, |entity| {
                            if let Err(error) = accept(entity) {
                                failure = Some(error);
                                return false;
                            }
                            true
                        })
                        .map_err(storage_error)?;
                } else {
                    manager.for_each_entity(|entity| {
                        if let Err(error) = accept(entity) {
                            failure = Some(error);
                            return false;
                        }
                        true
                    });
                }
                if let Some(error) = failure {
                    return Err(error);
                }
            } else {
                let mut failure = None;
                manager
                    .try_for_each_entity(read_bytes_max, |entity| {
                        if let Err(error) = accept(entity) {
                            failure = Some(error);
                            return false;
                        }
                        true
                    })
                    .map_err(storage_error)?;
                if let Some(error) = failure {
                    return Err(error);
                }
            }
            Ok(())
        };

        // Postings first: the published candidates this index has not seen
        // change, plus the overlay's resident rows for exactly this key. No
        // walk over the collection, however many rows are resident. Before
        // the first publication there is no published index and no disk
        // base: every row is resident and in the overlay.
        let overlay_alone =
            index.is_none() && !manager.has_backing() && self.overlay.read().holds_every_row;
        if index.is_some() || overlay_alone {
            let mut candidates = Vec::new();
            if let Some(index) = &index {
                let mut cursor = index
                    .candidates(&self.projection.digest(key))
                    .map_err(storage_error)?;
                while let Some(id) = cursor.next_id().map_err(storage_error)? {
                    candidates.try_reserve(1).map_err(read_error)?;
                    candidates.push(id);
                }
            }
            // Published postings the overlay has not seen change come first.
            let mut untouched_published = 0;
            let mut authoritative = {
                let overlay = self.overlay.read();
                if overlay.is_authoritative() {
                    candidates.retain(|id| !overlay.touched.contains_key(id));
                    untouched_published = candidates.len();
                    if let Some(ids) = overlay.postings.get(key) {
                        candidates.try_reserve(ids.len()).map_err(read_error)?;
                        candidates.extend(ids.iter().copied());
                    }
                    true
                } else {
                    false
                }
            };
            let mut ids = Vec::new();
            for (position, id) in candidates.iter().enumerate() {
                if !authoritative {
                    break;
                }
                match manager
                    .try_get(*id, read_bytes_max)
                    .map_err(storage_error)?
                {
                    // WAL recovery may retire a published row before its
                    // replacement reaches the overlay; that posting cannot
                    // speak for the key (same rule as the paged BTREE).
                    Some(entity) if position < untouched_published && entity.xmax != 0 => {
                        authoritative = false;
                    }
                    Some(entity) => {
                        if matches(&entity)? {
                            ids.try_reserve(1).map_err(read_error)?;
                            ids.push(entity.id);
                        }
                    }
                    // A row this index saw retired is expected to be gone.
                    None if self.retired.read().contains(id) => {}
                    None => authoritative = false,
                }
            }
            if authoritative {
                let output = runtime
                    .try_reserve_query_buffer(
                        ids.len().saturating_mul(std::mem::size_of::<EntityId>()),
                    )
                    .ok_or_else(|| read_error("HASH candidates exceed shared memory budget"))?;
                ids.sort_unstable();
                ids.dedup();
                return Ok(HashCandidates::owned(ids, Box::new(output)));
            }
        }
        #[cfg(test)]
        SCANNING_LOOKUPS.with(|count| count.set(count.get() + 1));

        // Count first, then admit exactly the candidate allocation. Reserving
        // the whole decode ceiling made a one-row primary-key lookup fail when
        // the bounded store still had enough headroom for its actual result.
        let mut candidate_count = 0usize;
        scan(&mut |entity| {
            if matches(entity)? {
                candidate_count = candidate_count.saturating_add(1);
            }
            Ok(())
        })?;
        let candidate_bytes = candidate_count.saturating_mul(std::mem::size_of::<EntityId>());
        let output = runtime
            .try_reserve_query_buffer(candidate_bytes)
            .ok_or_else(|| read_error("HASH candidates exceed shared memory budget"))?;
        let mut ids = Vec::new();
        ids.try_reserve_exact(candidate_count).map_err(read_error)?;
        scan(&mut |entity| {
            if matches(entity)? {
                if ids.len() == candidate_count {
                    return Err(read_error(
                        "HASH candidate set changed while publication was fenced",
                    ));
                }
                ids.push(entity.id);
            }
            Ok(())
        })?;
        if ids.len() != candidate_count {
            return Err(read_error(
                "HASH candidate set changed while publication was fenced",
            ));
        }
        // A replacement may be reached through both base and overlay. Sorting
        // in place avoids another set allocation and retains all physical MVCC IDs.
        ids.sort_unstable();
        ids.dedup();
        Ok(HashCandidates::owned(ids, Box::new(output)))
    }
}

impl RedDBRuntime {
    pub(super) fn prepare_paged_hash(
        &self,
        index: &RegisteredIndex,
        prepared: &super::index_store::IndexStore,
    ) -> crate::RedDBResult<()> {
        self.prepare_paged_hash_named(index, prepared, &index.name)
    }

    pub(super) fn prepare_paged_hash_named(
        &self,
        index: &RegisteredIndex,
        prepared: &super::index_store::IndexStore,
        storage_name: &str,
    ) -> crate::RedDBResult<()> {
        let store = self.db().store();
        let schema = store
            .get_collection(&index.collection)
            .and_then(|m| m.column_schema());
        // Encoding and schema bind the root to the complete immutable projection.
        let identity = projection_identity(&index.columns, schema.as_deref().map(Vec::as_slice));
        let projection = Arc::new(HashProjection {
            columns: index.columns.clone(),
            key_bytes_max: usize::try_from(self.memory_accounting().budget().resolved_bytes / 16)
                .unwrap_or(usize::MAX)
                .max(1),
            schema,
            #[cfg(test)]
            forced_digest: None,
        });
        store.register_secondary_index(
            &index.collection,
            storage_name,
            identity.clone(),
            projection.clone(),
        )?;
        // A collection without a disk base has every row resident. Seed the
        // overlay with them once, so lookups (and the unique admission of
        // every inserted row) do not walk the collection until a checkpoint
        // publishes the index.
        let mut overlay = OverlayHash::default();
        if let Some(manager) = store
            .get_collection(&index.collection)
            .filter(|manager| !manager.has_backing())
        {
            manager.for_each_entity(|entity| {
                match projection.raw_key(entity, projection.key_bytes_max) {
                    Ok(Some(key)) => overlay.insert(key, entity.id),
                    Ok(None) => {}
                    Err(_) => overlay.require_scan(entity.id),
                }
                true
            });
            overlay.holds_every_row = !overlay.overflowed;
        }
        prepared.hash.install_paged(
            &index.collection,
            &index.name,
            Arc::new(RuntimeHashIndex {
                runtime: Arc::downgrade(&self.inner),
                collection: index.collection.clone(),
                name: storage_name.to_string(),
                identity,
                projection,
                retired: parking_lot::RwLock::new(std::collections::HashSet::new()),
                overlay: parking_lot::RwLock::new(overlay),
            }),
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::storage::memory_pools::MemoryPool;
    use reddb_types::Value;

    fn fixture() -> (tempfile::TempDir, RedDBRuntime) {
        let directory = tempfile::tempdir().expect("paged HASH fixture");
        let options = RedDBOptions::persistent(directory.path().join("hash.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let seed = RedDBRuntime::with_options(options.clone()).expect("seed");
        for sql in [
            "CREATE TABLE hash_rows (id INTEGER, email TEXT, payload TEXT)",
            "CREATE UNIQUE INDEX by_email ON hash_rows (email) USING HASH",
            "INSERT INTO hash_rows (id, email) VALUES (1, 'alpha')",
            "INSERT INTO hash_rows (id, email) VALUES (2, 'beta')",
        ] {
            seed.execute_query(sql).expect(sql);
        }
        seed.flush().expect("seed checkpoint");
        drop(seed);
        let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options)
        })
        .expect("cold runtime");
        assert!(runtime
            .index_store_ref()
            .hash
            .is_paged("hash_rows", "by_email"));
        (directory, runtime)
    }

    fn lookup(runtime: &RedDBRuntime, key: &str) -> HashCandidates {
        runtime
            .index_store_ref()
            .hash
            .lookup_owned("hash_rows", "by_email", key.as_bytes())
            .expect("lookup")
    }

    #[test]
    fn paged_hash_restore_and_lookup_do_not_scan_prepared_source() {
        use crate::storage::engine::BTree;
        use reddb_engine::storage::collection_read::{
            CollectionCursor, CollectionRead, EncodedRecord, PagedCollection,
        };
        struct NoScan(Arc<dyn CollectionRead + Send + Sync>);
        impl CollectionRead for NoScan {
            fn get_record(
                &self,
                id: EntityId,
                limit: usize,
            ) -> Result<Option<EncodedRecord>, StoreError> {
                self.0.get_record(id, limit)
            }
            fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
                Err(StoreError::Serialization(
                    "unexpected source-wide scan".into(),
                ))
            }
            fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
                Err(StoreError::Serialization(
                    "unexpected source-wide scan".into(),
                ))
            }
        }
        let (_directory, runtime) = fixture();
        let store = runtime.db().store();
        let root = store.collection_root_page("hash_rows").expect("root");
        let manager = store.get_collection("hash_rows").expect("hash_rows");
        manager.install_backing(
            Arc::new(NoScan(Arc::new(PagedCollection::new(Arc::new(
                BTree::with_root(Arc::clone(store.pager().expect("pager")), root),
            ))))),
            root,
            store.format_version(),
            1024 * 1024,
            2,
        );
        runtime
            .rebuild_runtime_indexes_for_table("hash_rows")
            .expect("metadata-only restore");
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
        assert!(lookup(&runtime, "missing").is_empty());
    }

    /// Route the collection's cold reads through a probe that records any
    /// cursor (i.e. scan) over the backing.
    fn track_backing_scans(
        runtime: &RedDBRuntime,
    ) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        use crate::storage::engine::BTree;
        use reddb_engine::storage::collection_read::{
            CollectionCursor, CollectionRead, EncodedRecord, PagedCollection,
        };
        use std::sync::atomic::{AtomicBool, Ordering};

        struct TrackScan {
            source: Arc<dyn CollectionRead + Send + Sync>,
            scanned: Arc<AtomicBool>,
        }
        impl CollectionRead for TrackScan {
            fn get_record(
                &self,
                id: EntityId,
                limit: usize,
            ) -> Result<Option<EncodedRecord>, StoreError> {
                self.source.get_record(id, limit)
            }
            fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, StoreError> {
                self.scanned.store(true, Ordering::SeqCst);
                self.source.cursor()
            }
            fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, StoreError> {
                self.scanned.store(true, Ordering::SeqCst);
                self.source.owned_cursor()
            }
        }

        let store = runtime.db().store();
        let root = store.collection_root_page("hash_rows").expect("root");
        let manager = store.get_collection("hash_rows").expect("hash rows");
        let scanned = Arc::new(AtomicBool::new(false));
        manager.install_backing(
            Arc::new(TrackScan {
                source: Arc::new(PagedCollection::new(Arc::new(BTree::with_root(
                    Arc::clone(store.pager().expect("pager")),
                    root,
                )))),
                scanned: Arc::clone(&scanned),
            }),
            root,
            store.format_version(),
            1024 * 1024,
            2,
        );
        scanned
    }

    #[test]
    fn paged_hash_dangling_posting_scans_authoritative_backing() {
        use std::sync::atomic::Ordering;

        let (_directory, runtime) = fixture();
        let physical_id = lookup(&runtime, "alpha")[0];
        let scanned = track_backing_scans(&runtime);
        let manager = runtime
            .db()
            .store()
            .get_collection("hash_rows")
            .expect("hash rows");
        manager
            .delete(physical_id)
            .expect("remove indexed physical row");
        assert!(!scanned.load(Ordering::SeqCst));

        assert!(lookup(&runtime, "alpha").is_empty());
        assert!(
            scanned.load(Ordering::SeqCst),
            "a dangling posting must invalidate the fast path and scan the backing"
        );
    }

    #[test]
    fn paged_hash_retired_posting_does_not_scan_the_backing() {
        use std::sync::atomic::Ordering;

        let (_directory, runtime) = fixture();
        let physical_id = lookup(&runtime, "alpha")[0];
        let scanned = track_backing_scans(&runtime);
        let hash = &runtime.index_store_ref().hash;
        runtime
            .db()
            .store()
            .get_collection("hash_rows")
            .expect("hash rows")
            .delete(physical_id)
            .expect("remove indexed physical row");
        // The delete path tells the index which row it removed.
        hash.note_paged_retired("hash_rows", physical_id);

        assert!(lookup(&runtime, "alpha").is_empty());
        assert_eq!(lookup(&runtime, "beta").len(), 1);
        assert!(
            !scanned.load(Ordering::SeqCst),
            "a posting the index saw retired must not scan the backing"
        );

        // After a republication nothing retired earlier may excuse a dangle.
        hash.clear_paged_retired();
        assert!(lookup(&runtime, "alpha").is_empty());
        assert!(scanned.load(Ordering::SeqCst));
    }

    #[test]
    fn paged_hash_key_expansion_is_bounded_before_allocation() {
        let fields = vec![("payload".into(), Value::text("x".repeat(1024)))];
        let columns = vec!["payload".into(); 128];
        let error = bounded_hash_index_key(&columns, &fields, 4096)
            .expect_err("bound repeated composite fields");
        assert!(matches!(error, StoreError::ReadLimitExceeded { .. }));
        let single = bounded_hash_index_key(&columns[..1], &fields, 1024)
            .expect("exact bound")
            .expect("key");
        assert_eq!(single.len(), 1024);
    }

    #[test]
    fn paged_hash_concurrent_lookup_mutation_and_checkpoint_finish() {
        let (_directory, runtime) = fixture();
        let (send, receive) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for writer in [false, true] {
            let runtime = runtime.clone();
            let send = send.clone();
            workers.push(std::thread::spawn(move || {
                for round in 0..24 {
                    if writer {
                        let sql = format!(
                            "INSERT INTO hash_rows (id, email) VALUES ({}, 'concurrent-{round}')",
                            round + 10
                        );
                        runtime.execute_query(&sql).expect("concurrent insert");
                        runtime.flush().expect("concurrent checkpoint");
                    } else {
                        assert_eq!(lookup(&runtime, "alpha").len(), 1);
                        assert!(runtime
                            .index_store_ref()
                            .hash
                            .lookup_resident("hash_rows", "by_email", b"alpha")
                            .expect("resident probe")
                            .is_none());
                    }
                }
                send.send(()).expect("completion");
            }));
        }
        for _ in 0..2 {
            receive
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("no publication/admission/index lock inversion");
        }
        for worker in workers {
            worker.join().expect("worker");
        }
    }

    #[test]
    fn paged_hash_sixteen_clients_retain_small_results_within_shared_budget() {
        let (_directory, runtime) = fixture();
        let accounting = runtime.memory_accounting();
        let before = accounting.used_bytes(MemoryPool::QueryBuffers);
        let release = Arc::new(std::sync::Barrier::new(17));
        let (send, receive) = std::sync::mpsc::channel();
        let mut workers = Vec::new();
        for _ in 0..16 {
            let runtime = runtime.clone();
            let release = Arc::clone(&release);
            let send = send.clone();
            workers.push(std::thread::spawn(move || {
                let candidates =
                    runtime
                        .index_store_ref()
                        .hash
                        .lookup_owned("hash_rows", "by_email", b"alpha");
                send.send(
                    candidates
                        .as_ref()
                        .map(|ids| ids.len())
                        .map_err(ToString::to_string),
                )
                .expect("result");
                release.wait();
                drop(candidates);
            }));
        }
        let results: Vec<_> = (0..16)
            .map(|_| {
                receive
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .expect("all clients finish admission")
            })
            .collect();
        let retained = accounting.used_bytes(MemoryPool::QueryBuffers) - before;
        release.wait();
        for worker in workers {
            worker.join().expect("client");
        }
        for result in results {
            assert_eq!(result.expect("small concurrent lookup admitted"), 1);
        }
        assert!(retained <= 16 * 64 * 1024, "{retained}");
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), before);
    }

    #[test]
    fn paged_hash_large_key_retries_with_admitted_scratch() {
        let (_directory, runtime) = fixture();
        let key = "x".repeat(256 * 1024);
        runtime
            .execute_query_with_params(
                "INSERT INTO hash_rows (id, email) VALUES (3, $1)",
                &[Value::text(key.clone())],
            )
            .expect("large key");
        for checkpoint in [false, true] {
            if checkpoint {
                runtime.flush().expect("publish large key");
            }
            assert_eq!(lookup(&runtime, &key).len(), 1);
        }
    }

    #[test]
    fn paged_hash_large_unindexed_payload_uses_exact_row_scratch() {
        let (_directory, mut runtime) = fixture();
        runtime
            .execute_query_with_params(
                "INSERT INTO hash_rows (id, email, payload) VALUES (3, 'large-row', $1)",
                &[Value::text("x".repeat(64 * 1024))],
            )
            .expect("large unindexed payload");
        let manager = runtime
            .db()
            .store()
            .get_collection("hash_rows")
            .expect("hash_rows");
        let row_id = lookup(&runtime, "large-row")
            .into_iter()
            .next()
            .expect("large row candidate");
        let row_bytes = crate::storage::unified::memory_size::entity_bytes(
            &manager
                .try_get(row_id, usize::MAX)
                .expect("read large row")
                .expect("large row exists"),
        );
        drop(runtime.background_tasks.take());
        runtime.refresh_memory_accounting();
        let accounting = runtime.memory_accounting();
        let available = accounting.budget().resolved_bytes - accounting.total_used_bytes();
        let headroom = (row_bytes
            .max(64 * 1024)
            .saturating_mul(2)
            .saturating_add(4096)) as u64;
        assert!(
            available > headroom,
            "fixture must admit controlled pressure"
        );
        let held = runtime
            .try_reserve_query_buffer((available - headroom) as usize)
            .expect("leave exact HASH probe headroom");
        assert_eq!(lookup(&runtime, "large-row").len(), 1);
        drop(held);
    }

    #[test]
    fn paged_hash_overlay_mutations_and_checkpoint_rebase() {
        let (_directory, runtime) = fixture();
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
        for sql in [
            "INSERT INTO hash_rows (id, email) VALUES (3, 'gamma')",
            "UPDATE hash_rows SET email = 'renamed' WHERE email = 'alpha'",
            "DELETE FROM hash_rows WHERE email = 'beta'",
        ] {
            assert_eq!(runtime.execute_query(sql).expect(sql).affected_rows, 1);
        }
        for checkpoint in [false, true] {
            if checkpoint {
                runtime.flush().expect("rebase");
            }
            // Physical candidates may include obsolete MVCC versions. SQL must
            // recheck visibility; direct lookup must find the new overlay keys.
            for key in ["gamma", "renamed"] {
                assert!(!lookup(&runtime, key).is_empty(), "{key}");
                let result = runtime
                    .execute_query_with_params(
                        "SELECT id FROM hash_rows WHERE email = $1",
                        &[Value::text(key)],
                    )
                    .expect("visible new key");
                assert_eq!(result.result.records.len(), 1, "{key}");
            }
            for key in ["alpha", "beta"] {
                let result = runtime
                    .execute_query_with_params(
                        "SELECT id FROM hash_rows WHERE email = $1",
                        &[Value::text(key)],
                    )
                    .expect("obsolete key");
                assert!(result.result.records.is_empty(), "{key}");
            }
            assert!(runtime
                .execute_query("INSERT INTO hash_rows (id, email) VALUES (4, 'renamed')")
                .is_err());
        }
        assert!(runtime.index_store_ref().hash.memory_bytes() < 8192);
    }

    #[test]
    fn autocommit_insert_does_not_turn_indexed_statements_into_scans() {
        let (_directory, runtime) = fixture();
        // The insert reserves a batch of autocommit xids and uses one. The
        // rest sit in every later snapshot's in-progress set having written
        // nothing: they must not push SELECT/UPDATE/DELETE off the index.
        runtime
            .execute_query("INSERT INTO hash_rows (id, email) VALUES (3, 'gamma')")
            .expect("autocommit insert");
        let scanned = track_backing_scans(&runtime);
        let selected = runtime
            .execute_query("SELECT id FROM hash_rows WHERE email = 'alpha'")
            .expect("indexed select");
        assert_eq!(selected.result.records.len(), 1);
        assert!(
            !scanned.load(std::sync::atomic::Ordering::SeqCst),
            "a SELECT by indexed key scanned the collection after an autocommit INSERT"
        );
        runtime
            .execute_query("INSERT INTO hash_rows (id, email) VALUES (4, 'delta')")
            .expect("second autocommit insert");
        let updated = runtime
            .execute_query("UPDATE hash_rows SET payload = 'x' WHERE email = 'beta'")
            .expect("indexed update");
        assert_eq!(updated.affected_rows, 1);
        assert!(
            !scanned.load(std::sync::atomic::Ordering::SeqCst),
            "an UPDATE by indexed key scanned the collection after an autocommit INSERT"
        );
    }

    #[test]
    fn paged_hash_resident_changes_are_found_without_walking_the_collection() {
        let (_directory, runtime) = fixture();
        let scans = || SCANNING_LOOKUPS.with(std::cell::Cell::get);
        for sql in [
            "INSERT INTO hash_rows (id, email) VALUES (3, 'gamma')",
            "UPDATE hash_rows SET email = 'renamed' WHERE email = 'alpha'",
            "UPDATE hash_rows SET payload = 'touched' WHERE email = 'gamma'",
            "DELETE FROM hash_rows WHERE email = 'beta'",
            "INSERT INTO hash_rows (id, email) VALUES (5, 'beta')",
        ] {
            assert_eq!(runtime.execute_query(sql).expect(sql).affected_rows, 1);
        }
        let visible = |key: &str| {
            runtime
                .execute_query_with_params(
                    "SELECT id FROM hash_rows WHERE email = $1",
                    &[Value::text(key)],
                )
                .expect("lookup by key")
                .result
                .records
                .len()
        };
        // Every key is answered from postings: published ones the index has
        // not seen change, plus the overlay's resident rows.
        let before = scans();
        for (key, rows) in [("gamma", 1), ("renamed", 1), ("beta", 1), ("alpha", 0)] {
            assert_eq!(visible(key), rows, "{key}");
            assert_eq!(lookup(&runtime, key).is_empty(), rows == 0, "{key}");
        }
        assert_eq!(lookup(&runtime, "never-indexed").len(), 0);
        assert_eq!(scans(), before, "resident changes must not force a scan");

        // Publication folds the overlay into the index; answers do not move.
        runtime.flush().expect("rebase");
        let before = scans();
        for (key, rows) in [("gamma", 1), ("renamed", 1), ("beta", 1), ("alpha", 0)] {
            assert_eq!(visible(key), rows, "{key} after publication");
        }
        assert_eq!(scans(), before);
    }

    #[test]
    fn overlay_gives_up_instead_of_growing_without_bound() {
        let mut overlay = OverlayHash::default();
        for id in 0..OVERLAY_TOUCHED_MAX as u64 {
            overlay.insert(id.to_be_bytes().to_vec(), EntityId::new(id + 1));
        }
        assert!(overlay.is_authoritative());
        // A row already tracked may still change.
        overlay.remove(EntityId::new(1));
        assert!(overlay.is_authoritative());
        // One more row past the cap: stop tracking, release everything, and
        // send lookups to the scan until a publication rebuilds the overlay.
        overlay.insert(b"over".to_vec(), EntityId::new(u64::MAX));
        assert!(!overlay.is_authoritative());
        assert!(overlay.touched.is_empty() && overlay.postings.is_empty());
        overlay.insert(b"later".to_vec(), EntityId::new(7));
        overlay.require_scan(EntityId::new(8));
        assert!(overlay.touched.is_empty() && overlay.scan_required.is_empty());
    }

    #[test]
    fn paged_hash_answers_from_the_overlay_before_the_first_publication() {
        let directory = tempfile::tempdir().expect("unpublished HASH fixture");
        let options = RedDBOptions::persistent(directory.path().join("unpublished.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options)
        })
        .expect("disk-resident runtime");
        for sql in [
            // An index that exists before its rows, and one built over rows.
            "CREATE TABLE early (id INTEGER, email TEXT)",
            "CREATE INDEX early_email ON early (email) USING HASH",
            "INSERT INTO early (id, email) VALUES (1, 'a'), (2, 'b')",
            "CREATE TABLE late (id INTEGER, email TEXT)",
            "INSERT INTO late (id, email) VALUES (1, 'a'), (2, 'b')",
            "CREATE INDEX late_email ON late (email) USING HASH",
            "UPDATE early SET email = 'c' WHERE id = 2",
            "DELETE FROM late WHERE id = 1",
        ] {
            runtime.execute_query(sql).expect(sql);
        }
        for (table, index) in [("early", "early_email"), ("late", "late_email")] {
            assert!(runtime.index_store_ref().hash.is_paged(table, index));
        }
        let rows = |sql: &str| runtime.execute_query(sql).expect(sql).result.records.len();
        // Nothing was checkpointed: there is no published index to read, and
        // the overlay holds every row, so no lookup walks the collection.
        let before = SCANNING_LOOKUPS.with(std::cell::Cell::get);
        for (sql, expected) in [
            ("SELECT id FROM early WHERE email = 'a'", 1),
            ("SELECT id FROM early WHERE email = 'b'", 0),
            ("SELECT id FROM early WHERE email = 'c'", 1),
            ("SELECT id FROM late WHERE email = 'a'", 0),
            ("SELECT id FROM late WHERE email = 'b'", 1),
        ] {
            assert_eq!(rows(sql), expected, "{sql}");
        }
        assert_eq!(
            SCANNING_LOOKUPS.with(std::cell::Cell::get),
            before,
            "a lookup walked the collection before the first checkpoint"
        );
    }

    #[test]
    fn cold_scan_does_not_deadlock_with_a_checkpoint_appending_to_its_base() {
        // More rows than the reopened page cache can hold, so the scan has to
        // load pages whatever the boot already read.
        const ROWS: usize = 10_000;
        let directory = tempfile::tempdir().expect("wide fixture");
        let options = RedDBOptions::persistent(directory.path().join("wide.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let seed = RedDBRuntime::with_options(options.clone()).expect("seed");
        seed.execute_query("CREATE TABLE wide (seq INTEGER, payload TEXT)")
            .expect("create");
        let payload = "x".repeat(2000);
        for batch in 0..ROWS / 100 {
            let rows = (0..100)
                .map(|row| format!("({}, '{payload}')", batch * 100 + row))
                .collect::<Vec<_>>()
                .join(",");
            seed.execute_query(&format!("INSERT INTO wide (seq, payload) VALUES {rows}"))
                .expect("seed rows");
        }
        seed.flush().expect("seed checkpoint");
        drop(seed);
        let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options.with_memory_budget(16 * 1024 * 1024))
        })
        .expect("cold runtime");
        // One resident row gives the next checkpoint an append to the base.
        runtime
            .execute_query("INSERT INTO wide (seq, payload) VALUES (10000, 'new')")
            .expect("pending row");
        let store = runtime.db().store();
        let pager = Arc::clone(store.pager().expect("pager"));
        let manager = store.get_collection("wide").expect("wide");

        // The scan holds the collection's base while it reads; every page it
        // has to load waits for the ownership epoch. A checkpoint holds that
        // epoch throughout and must not wait for the base inside it.
        let epoch = pager.lock_ownership_mutations();
        let scanned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let reader = {
            let scanned = Arc::clone(&scanned);
            std::thread::spawn(move || {
                manager
                    .try_for_each_entity(usize::MAX, |_| {
                        scanned.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        true
                    })
                    .map_err(|error| error.to_string())
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(
            scanned.load(std::sync::atomic::Ordering::Relaxed) <= ROWS,
            "the scan must be waiting for a page it has to load"
        );
        let (checkpointed, checkpoint_done) = std::sync::mpsc::channel();
        let checkpoint = {
            let runtime = runtime.clone();
            std::thread::spawn(move || {
                let result = runtime.flush().map_err(|error| error.to_string());
                checkpointed.send(result).expect("report checkpoint");
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(epoch);
        checkpoint_done
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("a checkpoint deadlocked with a cold scan of the base it was appending to")
            .expect("checkpoint");
        checkpoint.join().expect("checkpoint thread");
        reader.join().expect("reader thread").expect("scan");
        assert_eq!(scanned.load(std::sync::atomic::Ordering::Relaxed), ROWS + 1);
    }

    #[test]
    fn cold_read_does_not_deadlock_with_a_publication_sealing_its_tree() {
        let (_directory, runtime) = fixture();
        let store = runtime.db().store();
        let pager = Arc::clone(store.pager().expect("pager"));
        let manager = store.get_collection("hash_rows").expect("hash_rows");
        let (reading, reader_started) = std::sync::mpsc::channel();
        let (published, publication_done) = std::sync::mpsc::channel();
        let (store, pager, manager) = (&store, &pager, &manager);
        std::thread::scope(|scope| {
            let publication = scope.spawn(move || {
                // A publication holds the fence and the pager's ownership
                // epoch for its whole duration, and seals each tree inside.
                let _fence = store.vault_publication_lock.lock();
                let _epoch = pager.lock_ownership_mutations();
                reader_started.recv().expect("reader is about to read");
                // Let the reader reach a page that is not cached.
                std::thread::sleep(std::time::Duration::from_millis(300));
                let result = store
                    .flush_paged_registry()
                    .map_err(|error| error.to_string());
                published.send(result).expect("report publication");
            });
            let reader = scope.spawn(move || {
                reading.send(()).expect("announce read");
                let mut rows = 0;
                manager
                    .try_for_each_entity(usize::MAX, |_| {
                        rows += 1;
                        true
                    })
                    .map(|()| rows)
                    .map_err(|error| error.to_string())
            });
            publication_done
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("a publication deadlocked with a cold read of the tree it was sealing")
                .expect("publication");
            publication.join().expect("publication thread");
            assert_eq!(reader.join().expect("reader thread").expect("cold read"), 2);
        });
    }

    #[test]
    fn paged_hash_candidates_retain_admission_until_iterator_is_dropped() {
        let (_directory, runtime) = fixture();
        let accounting = runtime.memory_accounting();
        let before = accounting.used_bytes(MemoryPool::QueryBuffers);
        let ids = lookup(&runtime, "alpha");
        let retained = accounting.used_bytes(MemoryPool::QueryBuffers);
        assert!(retained > before);
        let mut iterator = ids.into_iter();
        assert!(iterator.next().is_some());
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), retained);
        drop(iterator);
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), before);
        drop(runtime.try_reserve_query_buffer(0).expect("sample"));
        let available = accounting.budget().resolved_bytes - accounting.total_used_bytes();
        let held = runtime
            .try_reserve_query_buffer((available - 16 * 1024) as usize)
            .expect("pressure");
        let error = runtime
            .index_store_ref()
            .hash
            .lookup_owned("hash_rows", "by_email", b"alpha")
            .expect_err("deny before allocating");
        assert!(error.to_string().contains("workspace exceeds"), "{error}");
        drop(held);
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
    }

    #[test]
    fn paged_hash_digest_collisions_recheck_full_keys() {
        let (_directory, runtime) = fixture();
        let store = runtime.db().store();
        let projection = Arc::new(HashProjection {
            columns: vec!["email".into()],
            key_bytes_max: 4 * 1024 * 1024,
            schema: store
                .get_collection("hash_rows")
                .expect("hash_rows")
                .column_schema(),
            forced_digest: Some([7; 32]),
        });
        let identity = SecondaryIndexIdentity {
            codec_version: 1,
            definition: [9; 32],
        };
        store.unregister_secondary_index("hash_rows", "by_email");
        store
            .register_secondary_index(
                "hash_rows",
                "by_email",
                identity.clone(),
                projection.clone(),
            )
            .expect("colliding projection");
        store.persist().expect("publish collision fixture");
        runtime.index_store_ref().hash.install_paged(
            "hash_rows",
            "by_email",
            Arc::new(RuntimeHashIndex {
                runtime: Arc::downgrade(&runtime.inner),
                collection: "hash_rows".into(),
                name: "by_email".into(),
                identity,
                projection,
                retired: parking_lot::RwLock::new(std::collections::HashSet::new()),
                overlay: parking_lot::RwLock::new(OverlayHash::default()),
            }),
        );
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
        assert_eq!(lookup(&runtime, "beta").len(), 1);
        assert_ne!(lookup(&runtime, "alpha")[0], lookup(&runtime, "beta")[0]);
        assert!(lookup(&runtime, "absent").is_empty());
    }

    #[test]
    fn paged_hash_page_errors_reach_select_update_delete_and_unique_admission() {
        use crate::storage::engine::{Page, PageType};
        let (_directory, runtime) = fixture();
        let store = runtime.db().store();
        let schema = store
            .get_collection("hash_rows")
            .expect("hash_rows")
            .column_schema();
        let identity = projection_identity(&["email".into()], schema.as_deref().map(Vec::as_slice));
        let native = store
            .secondary_index("hash_rows", "by_email", &identity)
            .expect("published native root");
        let pager = store.pager().expect("pager");
        let root = native.root_page_id();
        let original = pager.read_page(root).expect("original");
        pager
            .write_page(root, Page::new(PageType::Overflow, root))
            .expect("corrupt fixture index");
        for sql in [
            "SELECT id FROM hash_rows WHERE email = 'alpha'",
            "UPDATE hash_rows SET email = 'changed' WHERE email = 'alpha'",
            "DELETE FROM hash_rows WHERE email = 'alpha'",
            "INSERT INTO hash_rows (id, email) VALUES (3, 'alpha')",
        ] {
            let error = runtime
                .execute_query(sql)
                .err()
                .expect("index failure must propagate");
            assert!(
                error.to_string().contains("hash index read failed"),
                "{sql}: {error}"
            );
        }
        pager.write_page(root, original).expect("restore");
        assert_eq!(lookup(&runtime, "alpha").len(), 1);
    }
}
