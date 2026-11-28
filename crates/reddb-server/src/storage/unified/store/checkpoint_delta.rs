//! Bounded checkpoint projection of rows whose exact image is already in the WAL.
use super::commit::StoreWalAction;
use super::*;

enum SparseCheckpointValue {
    Cached,
    Wal(super::checkpoint_wal::WalRowReference),
    Projected(Vec<u8>),
}

thread_local! {
    /// Why the incremental checkpoint of the collection at hand gave up.
    static DECLINED: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

/// The incremental checkpoint gives up in many places and the collection is
/// then republished in full, which is what makes a checkpoint long. Keep the
/// first reason so the publication can say which bound a workload outgrew.
/// How long a delta waits for a lookup in progress to let go of a tree. A
/// lookup does not wait for the checkpoint, so it ends well inside this. A
/// scan does not, and its tree is published again.
const READER_PIN_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

/// Whether the readers `pinned` reports have left within [`READER_PIN_WAIT`].
pub(super) fn readers_leave(mut pinned: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + READER_PIN_WAIT;
    while pinned() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
    true
}

pub(super) fn declined<T>(reason: &'static str) -> Result<Option<T>, StoreError> {
    refused(reason);
    Ok(None)
}

pub(super) fn refused(reason: &'static str) -> bool {
    DECLINED.with(|slot| slot.set(slot.get().or(Some(reason))));
    false
}

pub(super) fn take_declined() -> Option<&'static str> {
    DECLINED.with(std::cell::Cell::take)
}

/// Index entries one incremental checkpoint may change across a collection.
pub(super) const POSTING_CHANGES_MAX: usize =
    8 * crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX;

/// Whether a delta of `changed` rows is one the incremental checkpoint takes:
/// within the mutation journal and within the index entries it may change.
/// Staging a replacement tree asks the same question, so it copies a
/// collection only when the delta would have been declined.
pub(super) fn delta_fits(changed: usize, indexes: usize) -> bool {
    changed <= crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX
        && changed.saturating_mul(indexes) <= POSTING_CHANGES_MAX
}

/// Rows maintenance may change in one large collection between two
/// checkpoints. Each of them costs the checkpoint about half a millisecond
/// with the writers waiting, so this is also what maintenance adds to it.
pub(crate) const MAINTENANCE_ROWS_PER_CHECKPOINT: usize =
    crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX / 4;
/// A collection up to this size is not paced: copying it whole costs a
/// checkpoint about what a full delta does.
pub(crate) const MAINTENANCE_PACED_FROM_ROWS: u64 =
    4 * crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX as u64;
/// Part of a delta kept for the statements that arrive before the checkpoint.
const STATEMENT_ROWS_RESERVED: usize =
    crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX / 8;
/// What maintenance may do in a period however busy the collection is, so
/// retention never stops for good on a table that is always being written.
const MAINTENANCE_ROWS_FLOOR: usize = 256;
/// A period ends at a checkpoint, or after this long on a store that takes none.
const MAINTENANCE_PERIOD_MAX: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Changed rows of one large collection at which a checkpoint is due. Half a
/// delta, so that a statement of a few thousand rows still fits behind it.
pub(crate) const DELTA_CHECKPOINT_ROWS: usize =
    crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX / 2;
/// How often a statement looks for such a collection.
const DELTA_SAMPLE_MS: u64 = 5;
/// Checkpoints by volume closer together than this are worth a warning,
/// at most one a minute.
const VOLUME_CHECKPOINT_WARNING_MS: u64 = 30_000;
const VOLUME_WARNING_EVERY_MS: u64 = 60_000;

pub(in crate::storage::unified) struct MaintenanceSpend {
    since: std::time::Instant,
    rows: usize,
    reported: bool,
}

impl UnifiedStore {
    /// Rows of `collection` that background maintenance (version reclaim,
    /// retention) may still remove or mark before the next checkpoint.
    ///
    /// A checkpoint writes a collection's changes as a delta only while they
    /// fit `delta_fits`; past that it copies the whole collection with every
    /// writer waiting. Version reclaim takes 512 rows a second: over an
    /// 800,000-row backlog it passed the bound in sixteen seconds, and each
    /// checkpoint then copied the collection again, two minutes at a time.
    /// Maintenance has no deadline, so on a large collection it takes a share
    /// of the next delta and leaves the rest for after the checkpoint.
    ///
    /// It is not held back where that buys nothing: on a small collection, and
    /// once statements alone have changed more rows than a delta takes. The
    /// share is also a ceiling on what retention and reclaim keep up with in a
    /// large collection, until a delta removes rows in batches.
    pub(crate) fn maintenance_allowance(&self, collection: &str) -> usize {
        if self.pager.is_none() {
            return usize::MAX;
        }
        let collection = Self::canonical_collection(collection);
        let Some(manager) = self.get_collection(collection) else {
            return usize::MAX;
        };
        if manager.count() as u64 <= MAINTENANCE_PACED_FROM_ROWS {
            return usize::MAX;
        }
        // With no baseline, or one the journal no longer reaches, the next
        // checkpoint copies the collection whatever maintenance does now.
        // Maintenance stops at its share, so it is not what got it there.
        let Some(pending) = self.checkpoint_pending_rows(collection, &manager) else {
            return usize::MAX;
        };
        let mut all = self.maintenance_spent.lock();
        if all
            .get(collection)
            .is_some_and(|period| period.since.elapsed() >= MAINTENANCE_PERIOD_MAX)
        {
            all.remove(collection);
        }
        let spent = all.get(collection).map_or(0, |period| period.rows);
        let delta = (crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX
            - STATEMENT_ROWS_RESERVED)
            .saturating_sub(pending);
        let allowance = MAINTENANCE_ROWS_PER_CHECKPOINT
            .saturating_sub(spent)
            .min(delta)
            .max(MAINTENANCE_ROWS_FLOOR.saturating_sub(spent));
        if allowance == 0 {
            if let Some(period) = all.get_mut(collection).filter(|period| !period.reported) {
                period.reported = true;
                tracing::info!(
                    target: "reddb::maintenance",
                    collection,
                    rows = period.rows,
                    "maintenance waits for the next checkpoint"
                );
            }
        }
        allowance
    }

    /// A large collection whose changes since the last checkpoint are about to
    /// outgrow what a checkpoint writes as a delta.
    ///
    /// A checkpoint used to be due by time (the operator's schedule) and by
    /// memory pressure only. A load passes 8,192 changed rows long before
    /// either, and the checkpoint that comes then copies the whole collection
    /// with every writer waiting: a 406,000-row table held its INSERT for 31
    /// seconds. A checkpoint is also due by volume, the way WAL size makes one
    /// due in PostgreSQL (`max_wal_size`): taken while the delta still fits,
    /// it costs the changed rows instead of the table. Smaller collections are
    /// left alone; copying one costs about what a full delta does.
    pub(crate) fn collection_outgrowing_its_delta(&self) -> Option<String> {
        use std::sync::atomic::Ordering;
        self.pager.as_ref()?;
        let now = crate::utils::now_unix_millis();
        let last = self.delta_sampled_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < DELTA_SAMPLE_MS
            || self
                .delta_sampled_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return None;
        }
        let large: Vec<(String, Arc<SegmentManager>)> = self
            .resident
            .collections
            .read()
            .iter()
            .filter(|(_, manager)| manager.count() as u64 > MAINTENANCE_PACED_FROM_ROWS)
            .map(|(name, manager)| (name.clone(), Arc::clone(manager)))
            .collect();
        large
            .into_iter()
            .find(|(name, manager)| {
                // Mutations first: cheap, and an upper bound on the rows.
                self.checkpoint_mutations(name, manager) >= DELTA_CHECKPOINT_ROWS as u64
                    && self
                        .checkpoint_pending_rows(name, manager)
                        .is_some_and(|rows| rows >= DELTA_CHECKPOINT_ROWS)
            })
            .map(|(name, _)| name)
    }

    /// Mutations a collection's segments recorded since its last checkpoint.
    fn checkpoint_mutations(&self, collection: &str, manager: &Arc<SegmentManager>) -> u64 {
        let before: HashMap<u64, u64> = {
            let versions = self.checkpoint_versions.read();
            let Some(previous) = versions.get(collection) else {
                return 0;
            };
            previous.segments.iter().copied().collect()
        };
        manager
            .checkpoint_revisions()
            .into_iter()
            .map(|(segment, revision)| {
                revision.saturating_sub(before.get(&segment).copied().unwrap_or(0))
            })
            .sum()
    }

    /// Note a checkpoint taken for volume. Returns the seconds since the
    /// previous one when they are close together and no warning went out in
    /// the last minute.
    pub(crate) fn note_volume_checkpoint(&self) -> Option<u64> {
        use std::sync::atomic::Ordering;
        let now = crate::utils::now_unix_millis();
        let previous = self.volume_checkpoint_ms.swap(now, Ordering::Relaxed);
        let apart = now.saturating_sub(previous);
        if previous == 0 || apart >= VOLUME_CHECKPOINT_WARNING_MS {
            return None;
        }
        let warned = self.volume_warned_ms.load(Ordering::Relaxed);
        if now.saturating_sub(warned) < VOLUME_WARNING_EVERY_MS {
            return None;
        }
        self.volume_warned_ms.store(now, Ordering::Relaxed);
        Some(apart / 1_000)
    }

    /// Record `rows` changed by maintenance against the collection's period.
    pub(crate) fn charge_maintenance(&self, collection: &str, rows: usize) {
        if rows == 0 || self.pager.is_none() {
            return;
        }
        let collection = Self::canonical_collection(collection);
        let mut all = self.maintenance_spent.lock();
        let period = all
            .entry(collection.to_string())
            .or_insert_with(|| MaintenanceSpend {
                since: std::time::Instant::now(),
                rows: 0,
                reported: false,
            });
        period.rows = period.rows.saturating_add(rows);
    }

    /// Distinct rows the next checkpoint's delta already has to write for
    /// `collection`; `None` when it cannot be a delta.
    fn checkpoint_pending_rows(
        &self,
        collection: &str,
        manager: &Arc<SegmentManager>,
    ) -> Option<usize> {
        let (segments, deferred) = {
            let versions = self.checkpoint_versions.read();
            let previous = versions.get(collection)?;
            if !previous.manager.ptr_eq(&Arc::downgrade(manager)) {
                return None;
            }
            (
                previous.segments.clone(),
                Arc::clone(previous.deferred_rows.as_ref()?),
            )
        };
        let mut rows: std::collections::HashSet<EntityId> = manager
            .checkpoint_mutations_since(&segments)?
            .into_iter()
            .collect();
        rows.extend(deferred.iter().copied());
        rows.extend(manager.backing_removed_rows());
        Some(rows.len())
    }
}

#[derive(Default)]
pub(super) struct DurableRows {
    rows: HashMap<(String, EntityId), Vec<u8>>,
    bytes: usize,
    collection_rows: HashMap<String, usize>,
    references: super::checkpoint_wal::WalRowReferences,
}

impl DurableRows {
    pub(super) fn memory_bytes(&self) -> usize {
        self.bytes + self.references.memory_bytes()
    }

    fn bytes_after_insert(&self, key: &(String, EntityId), capacity: usize) -> usize {
        let previous = self.rows.get(key);
        self.bytes
            .saturating_sub(previous.map_or(0, Vec::capacity))
            .saturating_add(capacity)
            .saturating_add(if previous.is_none() {
                key.0.len() + 128
            } else {
                0
            })
            .saturating_add(if self.collection_rows.contains_key(&key.0) {
                0
            } else {
                key.0.len() + 128
            })
    }

    fn forget_collection(&mut self, collection: &str) {
        if self.collection_rows.remove(collection).is_none() {
            return;
        }
        let mut reclaimed = collection.len() + 128;
        self.rows.retain(|(name, _), record| {
            if name != collection {
                return true;
            }
            reclaimed += record.capacity() + name.len() + 128;
            false
        });
        self.rows.shrink_to_fit();
        self.collection_rows.shrink_to_fit();
        self.bytes = self.bytes.saturating_sub(reclaimed);
    }

    fn remember(&mut self, collection: &str, record: Vec<u8>, limit: usize) {
        let frame = match reddb_file::decode_native_entity_record_frame(&record) {
            Ok(frame) => frame,
            Err(_) => return,
        };
        let entity = frame.map_or(record.as_slice(), |frame| frame.entity);
        let mut position = 0;
        let Ok(id) = read_varu64(entity, &mut position) else {
            return;
        };
        // The existing entity codec's table-row discriminator follows its ID.
        // Other models keep their existing inline/checkpoint paths.
        if entity.get(position) != Some(&0) {
            return;
        }
        let key = (collection.to_owned(), EntityId::new(id));
        let standalone_bytes = record
            .capacity()
            .saturating_add(2 * (collection.len() + 128));
        if standalone_bytes > limit {
            return;
        }
        if !self.rows.contains_key(&key)
            && self.collection_rows.get(collection).copied().unwrap_or(0) == 1024
        {
            self.forget_collection(collection);
        }
        let mut bytes = self.bytes_after_insert(&key, record.capacity());
        if bytes > limit {
            // Pressure in one collection must not evict another collection's
            // exact WAL proof while the shared byte budget can retain it.
            self.forget_collection(collection);
            bytes = self.bytes_after_insert(&key, record.capacity());
            if bytes > limit {
                let references = std::mem::take(&mut self.references);
                *self = Self {
                    references,
                    ..Self::default()
                };
                bytes = standalone_bytes;
            }
        }
        if self.rows.insert(key, record).is_none() {
            *self
                .collection_rows
                .entry(collection.to_owned())
                .or_default() += 1;
        }
        self.bytes = bytes;
    }
}

impl UnifiedStore {
    /// Whether the incremental checkpoint is certain to have what it needs to
    /// apply these changes in place: a published tree to apply them to, a
    /// delta within its bounds, and a retained WAL image for every changed row
    /// that still exists. It can still decline for other reasons, and then
    /// publishes the collection again inside the checkpoint.
    pub(super) fn delta_is_ready(
        &self,
        collection: &str,
        manager: &SegmentManager,
        changed: &[EntityId],
    ) -> bool {
        let indexes = self
            .key_indices
            .read()
            .get(collection)
            .map_or(0, |keys| keys.secondary.len());
        if !delta_fits(changed.len(), indexes)
            || !self
                .btree_indices
                .read()
                .get(collection)
                .is_some_and(|tree| tree.has_page_proofs() && tree.root_page_id() != 0)
        {
            return false;
        }
        let retained = self.checkpoint_durable_rows.read();
        let mut key = (collection.to_owned(), EntityId::new(0));
        changed.iter().all(|&id| {
            key.1 = id;
            manager.get_with(id, |_| ()).is_none()
                || retained.rows.contains_key(&key)
                || retained.references.get(collection, id).is_some()
        })
    }

    /// Called only after the WAL append succeeds. The ordinary inline KV path
    /// does not retain records here. The checkpoint's existing force_sync is
    /// the durability fence before any sparse projection changes pager pages.
    pub(super) fn remember_checkpoint_rows(
        &self,
        actions: Vec<StoreWalAction>,
        references: super::checkpoint_wal::CheckpointWalBatch,
    ) {
        if self.pager.is_none() || self.commit.is_none() {
            return;
        }
        // A store that does not size its page cache runs the pager's default
        // one. Budgeting this as if it had none kept a few hundred rows, and
        // every checkpoint of more than that republished its collection.
        let slots = self
            .config
            .page_cache_slots
            .unwrap_or_else(|| crate::storage::engine::PagerConfig::default().cache_size);
        let limit = (slots.saturating_mul(crate::storage::engine::PAGE_SIZE) / 32)
            .clamp(16 * 1024, 8 * 1024 * 1024);
        let mut removed: Vec<(String, EntityId)> = Vec::new();
        {
            let mut retained = self.checkpoint_durable_rows.write();
            retained.references.remember(references, limit / 2);
            let limit = limit / 2;
            for action in actions {
                match action {
                    StoreWalAction::UpsertEntityRecord { collection, record } => {
                        retained.remember(&collection, record, limit)
                    }
                    StoreWalAction::BulkUpsertEntityRecords {
                        collection,
                        records,
                    } => {
                        for record in records {
                            retained.remember(&collection, record, limit);
                        }
                    }
                    StoreWalAction::DeleteEntityRecord {
                        collection,
                        entity_id,
                    } => removed.push((collection, EntityId::new(entity_id))),
                    _ => {}
                }
            }
        }
        // A delete staged under a WAL capture (a keyed replacement, a
        // transaction) removed its row before this append; nothing confirmed
        // it afterwards, and the next checkpoint published the collection
        // again for one replaced key.
        if self.config.resident_memory_bytes_max.is_none() || self.is_read_only() {
            return;
        }
        removed.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        for removals in removed.chunk_by(|left, right| left.0 == right.0) {
            if let Some(manager) = self.get_collection(&removals[0].0) {
                let ids: Vec<EntityId> = removals.iter().map(|(_, id)| *id).collect();
                manager.confirm_logged_removals(&ids);
            }
        }
    }

    /// Every changed ID must be accounted for by the bounded segment journal.
    /// An unchanged tree image needs no write; a different image requires an
    /// exact retained WAL record. Deferred MVCC rows are revisited when their
    /// transaction outcome changes, without inspecting unchanged payloads.
    /// Unknown edits, reader pins and exhausted journals retain the full path.
    pub(super) fn sparse_checkpoint_tree(
        &self,
        collection: &str,
        manager: &Arc<SegmentManager>,
        tree: &Arc<BTree>,
        previous: &CheckpointVersion,
        current: &mut CheckpointVersion,
    ) -> Result<Option<Arc<BTree>>, StoreError> {
        if manager.has_backing() {
            return self.sparse_cold_row_tree(collection, manager, tree, previous, current);
        }
        if Arc::strong_count(tree) != 1 || tree.has_retirement_readers() {
            return declined("a reader holds the data tree");
        }
        self.apply_sparse_checkpoint_tree(
            collection,
            manager,
            tree,
            previous,
            current,
            &Default::default(),
        )
    }

    pub(super) fn apply_sparse_checkpoint_tree(
        &self,
        collection: &str,
        manager: &Arc<SegmentManager>,
        tree: &Arc<BTree>,
        previous: &CheckpointVersion,
        current: &mut CheckpointVersion,
        logged_removals: &std::collections::HashSet<EntityId>,
    ) -> Result<Option<Arc<BTree>>, StoreError> {
        if previous.untracked_visibility_revision != current.untracked_visibility_revision {
            return declined("untracked visibility changed");
        }
        let Some(deferred) = previous.deferred_rows.as_ref() else {
            return declined("the previous checkpoint recorded no deferred rows");
        };
        let Some(ids) = manager.checkpoint_mutations_since(&previous.segments) else {
            return declined("the mutation journal no longer reaches the previous checkpoint");
        };
        let mut ids: std::collections::BTreeSet<_> = ids.into_iter().collect();
        ids.extend(deferred.iter().copied());
        ids.extend(
            previous
                .pending_vault
                .symmetric_difference(&current.pending_vault)
                .copied(),
        );
        if ids.len() > crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX {
            return declined("more than 8192 changed rows");
        }
        let retained = self.checkpoint_durable_rows.read();
        let mut upserts = Vec::new();
        let mut projected_bytes = 0_usize;
        let mut deferred = Vec::new();
        for id in ids {
            // The cold path validates and applies these physical deletes with their postings.
            if logged_removals.contains(&id) {
                continue;
            }
            let key = id.raw().to_be_bytes().to_vec();
            let existing = tree
                .get(&key)
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
            let metadata = manager.get_metadata(id);
            let mut defer = false;
            let row = manager.get_with(id, |entity| {
                if current.pending_vault.contains(&id)
                    || current.excluded_xids.contains(&entity.xmin)
                {
                    defer = true;
                    return None;
                }
                let raw =
                    Self::serialize_entity_record(entity, metadata.as_ref(), self.format_version());
                let normalize_branch = entity.branch_version().is_some_and(|branch| {
                    branch
                        .deletions
                        .iter()
                        .any(|(_, xid)| current.excluded_xids.contains(xid))
                });
                let normalize_xmax = current.excluded_xids.contains(&entity.xmax);
                let projected = if normalize_xmax || normalize_branch {
                    defer = true;
                    let mut normalized = entity.clone();
                    if normalize_xmax {
                        normalized.xmax = 0;
                    }
                    normalized.retain_branch_deletions(|xid| !current.excluded_xids.contains(&xid));
                    Some(Self::serialize_entity_record(
                        &normalized,
                        metadata.as_ref(),
                        self.format_version(),
                    ))
                } else {
                    None
                };
                Some((raw, projected))
            });
            if defer {
                deferred.push(id);
            }
            let Some(row) = row else {
                if existing.is_some() {
                    return declined("a removed row is still published");
                }
                continue;
            };
            let Some((raw, projected)) = row else {
                if existing.is_some() {
                    return declined("a removed row is still published");
                }
                continue;
            };
            if existing.as_ref() == Some(projected.as_ref().unwrap_or(&raw)) {
                continue;
            }
            let source = if retained.rows.get(&(collection.to_owned(), id)) == Some(&raw) {
                SparseCheckpointValue::Cached
            } else if let (Some(commit), Some(reference)) =
                (&self.commit, retained.references.get(collection, id))
            {
                if !commit
                    .checkpoint_row_matches(reference, &raw)
                    .map_err(StoreError::Io)?
                {
                    return declined("a changed row differs from its WAL record");
                }
                SparseCheckpointValue::Wal(reference)
            } else {
                return declined("a changed row has no retained WAL record");
            };
            let source = if let Some(projected) = projected {
                projected_bytes = projected_bytes.saturating_add(projected.capacity());
                if projected_bytes > 8 * 1024 * 1024 {
                    return declined("normalized rows exceed 8 MiB");
                }
                SparseCheckpointValue::Projected(projected)
            } else {
                source
            };
            upserts.push((id, source));
        }
        if manager.checkpoint_revisions() != current.segments {
            return declined("rows changed while the checkpoint read them");
        }
        current.has_unprojected_rows = !deferred.is_empty();
        current.deferred_rows = Some(Arc::new(deferred));
        // Validate every source before changing the published tree. Afterwards
        // materialize only bounded batches from those exact durable images;
        // do not reread potentially changed manager payloads in a second pass.
        #[cfg(debug_assertions)]
        let root_before = tree.root_page_id();
        let protected = self.ownership_active_epoch.load(Ordering::Acquire) != 0;
        let tree = Arc::clone(tree);
        let apply = |items: &[(Vec<u8>, Vec<u8>)]| -> Result<(), StoreError> {
            if protected
                && tree
                    .try_upsert_inline_without_splits(items)
                    .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?
            {
                #[cfg(test)]
                if std::env::var_os("REDDB_TEST_EXIT_AFTER_INLINE_SPARSE_WRITE").is_some() {
                    self.pager
                        .as_ref()
                        .expect("paged checkpoint")
                        .sync()
                        .map_err(|error| {
                            StoreError::Io(std::io::Error::other(error.to_string()))
                        })?;
                    std::process::exit(190);
                }
            } else {
                // The ownership epoch records each old page before its first
                // overwrite. Recovery restores those images before reading
                // roots, then releases new pages and replays committed rows.
                tree.upsert_batch_sorted(items)
                    .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
            }
            #[cfg(debug_assertions)]
            if std::env::var_os("REDDB_TEST_EXIT_AFTER_SPARSE_BATCH").is_some() {
                self.pager
                    .as_ref()
                    .expect("paged checkpoint")
                    .sync()
                    .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
                std::process::exit(193);
            }
            Ok(())
        };
        let mut batch = Vec::new();
        let mut batch_bytes = 0_usize;
        for (id, source) in upserts {
            let value = match source {
                SparseCheckpointValue::Cached => retained
                    .rows
                    .get(&(collection.to_owned(), id))
                    .expect("validated retained image")
                    .clone(),
                SparseCheckpointValue::Wal(reference) => self
                    .commit
                    .as_ref()
                    .expect("validated durable WAL")
                    .checkpoint_row_bytes(reference)
                    .map_err(StoreError::Io)?,
                SparseCheckpointValue::Projected(value) => value,
            };
            let key = id.raw().to_be_bytes().to_vec();
            let bytes = key.len() + value.len();
            if !batch.is_empty()
                && (batch_bytes.saturating_add(bytes) > 1024 * 1024 || batch.len() >= 512)
            {
                apply(&batch)?;
                batch.clear();
                batch_bytes = 0;
            }
            batch_bytes += bytes;
            batch.push((key, value));
        }
        if !batch.is_empty() {
            apply(&batch)?;
        }
        drop(retained);
        #[cfg(debug_assertions)]
        if tree.root_page_id() != root_before
            && std::env::var_os("REDDB_TEST_CHECKPOINT_EXIT_AFTER_SPARSE_ROOT_CHANGE").is_some()
        {
            self.pager
                .as_ref()
                .expect("paged projection")
                .sync()
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
            std::process::exit(176);
        }
        Ok(Some(tree))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: u64, payload_bytes: usize) -> Vec<u8> {
        let mut entity = UnifiedEntity::table_row(
            EntityId::new(id),
            "rows",
            id,
            vec![Value::text("p".repeat(payload_bytes))],
        );
        entity.created_at = 0;
        entity.updated_at = 0;
        UnifiedStore::serialize_entity_record(&entity, None, STORE_VERSION_V11)
    }

    #[test]
    fn row_capacity_eviction_keeps_accepting_exact_wal_images() {
        let mut retained = DurableRows::default();
        let limit = 8 * 1024 * 1024;
        for id in 1..=1025 {
            retained.remember("rows", record(id, 16), limit);
            assert!(retained.rows.len() <= 1024);
            assert!(retained.memory_bytes() <= limit);
        }
        assert!(!retained
            .rows
            .contains_key(&("rows".into(), EntityId::new(1))));
        assert_eq!(
            retained.rows.get(&("rows".into(), EntityId::new(1025))),
            Some(&record(1025, 16))
        );
    }

    #[test]
    fn byte_capacity_and_oversized_images_do_not_disable_later_retention() {
        let mut retained = DurableRows::default();
        let limit = 16 * 1024;
        for id in 1..=200 {
            retained.remember("rows", record(id, 256), limit);
            assert!(retained.memory_bytes() <= limit);
        }
        assert!(retained.rows.len() < 200, "byte pressure must evict images");
        assert_eq!(
            retained.rows.get(&("rows".into(), EntityId::new(200))),
            Some(&record(200, 256))
        );
        let mut oversized = record(201, limit);
        // The entity codec compresses repeated payloads. The retention budget
        // accounts for allocated capacity, so explicitly exceed that allowance.
        oversized.reserve_exact(limit + 1);
        assert!(oversized.capacity() > limit);
        retained.remember("rows", oversized, limit);
        assert!(retained.memory_bytes() <= limit);
        assert!(!retained
            .rows
            .contains_key(&("rows".into(), EntityId::new(201))));
        retained.remember("rows", record(202, 16), limit);
        assert_eq!(
            retained.rows.get(&("rows".into(), EntityId::new(202))),
            Some(&record(202, 16))
        );
    }

    #[test]
    fn unrelated_row_pressure_preserves_earlier_exact_images() {
        let mut retained = DurableRows::default();
        let limit = 8 * 1024 * 1024;
        let expected = record(1, 4096);
        retained.remember("payload", expected.clone(), limit);
        for id in 1..=1100 {
            retained.remember("noise", record(id, 16), limit);
            assert_eq!(
                retained.rows.get(&("payload".into(), EntityId::new(1))),
                Some(&expected),
                "unrelated row pressure discarded the earlier WAL proof"
            );
            assert!(retained.memory_bytes() <= limit);
        }
    }

    #[test]
    fn unrelated_byte_pressure_and_oversized_images_preserve_earlier_proofs() {
        let mut retained = DurableRows::default();
        let limit = 16 * 1024;
        let expected = record(1, 16);
        retained.remember("payload", expected.clone(), limit);
        for id in 1..=200 {
            retained.remember("noise", record(id, 256), limit);
            assert_eq!(
                retained.rows.get(&("payload".into(), EntityId::new(1))),
                Some(&expected)
            );
            assert!(retained.memory_bytes() <= limit);
        }
        let mut oversized = record(201, 16);
        oversized.reserve_exact(limit + 1);
        retained.remember("noise", oversized, limit);
        assert_eq!(
            retained.rows.get(&("payload".into(), EntityId::new(1))),
            Some(&expected)
        );
        assert!(retained.memory_bytes() <= limit);
    }
}
