use super::*;
use reddb_engine::storage::collection_read::{CollectionRead, FrozenOverlay, MergedCollection};
use std::collections::HashSet;

type Source = Arc<dyn CollectionRead + Send + Sync>;

struct Input {
    base: Option<Source>,
    overlay: Arc<FrozenOverlay>,
}

impl Input {
    fn capture(
        store: &UnifiedStore,
        manager: &SegmentManager,
        excluded: &HashSet<u64>,
        pending: &HashSet<EntityId>,
        limit: usize,
    ) -> Option<Self> {
        // Account for both the BTreeMap entry and its allocation in addition to
        // retained bytes. An oversized input keeps the ordinary checkpoint.
        const ENTRY: usize = 128;
        let (base, removed) = manager.checkpoint_backing_view(limit / ENTRY)?;
        let mut used = removed.len() * ENTRY;
        let mut rows: FrozenOverlay = removed.into_iter().map(|id| (id, None)).collect();
        let mut complete = true;
        manager.for_each_entity_with_metadata(|entity, metadata| {
            if !matches!(&entity.kind, EntityKind::TableRow { .. })
                || entity.branch_version().is_some()
                || !entity.cross_refs().is_empty()
                || pending.contains(&entity.id)
                || excluded.contains(&entity.xmin)
                || (entity.xmax != 0 && excluded.contains(&entity.xmax))
                || reddb_engine::storage::memory_size::entity_bytes(entity)
                    > limit.saturating_sub(used)
            {
                complete = false;
                return false;
            }
            let bytes = UnifiedStore::serialize_entity_record(
                entity,
                metadata.as_ref(),
                store.format_version(),
            );
            used = used.saturating_add(ENTRY).saturating_add(bytes.len());
            if used > limit {
                complete = false;
                return false;
            }
            rows.insert(entity.id, Some(bytes.into()));
            true
        });
        complete.then(|| Self {
            base,
            overlay: Arc::new(rows),
        })
    }
}

pub(crate) struct PreparedPrimary {
    pub(super) collection: String,
    pub(super) tree: Arc<BTree>,
    manager: std::sync::Weak<SegmentManager>,
    input: Input,
    version: u32,
    limit: usize,
}

impl PreparedPrimary {
    pub(super) fn still_matches(
        &self,
        store: &UnifiedStore,
        manager: &Arc<SegmentManager>,
        excluded: &HashSet<u64>,
        pending: &HashSet<EntityId>,
    ) -> bool {
        if self.version != store.format_version() || !self.manager.ptr_eq(&Arc::downgrade(manager))
        {
            return false;
        }
        let Some(now) = Input::capture(store, manager, excluded, pending, self.limit) else {
            return false;
        };
        let same_base = match (&self.input.base, &now.base) {
            (Some(old), Some(new)) => Arc::ptr_eq(old, new),
            (None, None) => true,
            _ => false,
        };
        same_base && self.input.overlay == now.overlay
    }
}

impl UnifiedStore {
    /// Stage at most one large changed primary. Each completed batch is owned
    /// by the existing durable retired-tree ledger. Writers may commit between
    /// batches; the ordinary checkpoint alone can install this root and trim WAL.
    pub(crate) fn prepare_paced_primary(
        &self,
        budget: usize,
    ) -> Result<Option<PreparedPrimary>, StoreError> {
        if self.is_read_only()
            || self.config.resident_memory_bytes_max.is_none()
            || self.commit.is_none()
            || budget < 1024 * 1024
        {
            return Ok(None);
        }
        let Some(pager) = self.pager.as_ref() else {
            return Ok(None);
        };
        let limit = budget / 4;
        let prepared = {
            let _publication = self.vault_publication_lock.lock();
            let capabilities = pager
                .header()
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                .required_capabilities;
            let required = reddb_file::file_format::FILE_CAPABILITY_TREE_PROOFS
                | reddb_file::file_format::FILE_CAPABILITY_DWB_PUBLICATION;
            if capabilities & required != required {
                return Ok(None);
            }
            let excluded = self
                .snapshot_manager
                .get()
                .map_or_else(HashSet::new, |s| s.checkpoint_excluded_state().1);
            let pending = self.pending_vault_versions.read();
            let versions = self.checkpoint_versions.read();
            let mut chosen = None;
            for name in self.list_collections() {
                let Some(manager) = self.get_collection(&name) else {
                    continue;
                };
                if manager.count() <= CHECKPOINT_BATCH_ROWS
                    || manager.checkpoint_revisions().is_empty()
                {
                    continue;
                }
                let before = versions
                    .get(&name)
                    .map_or(&[][..], |v| v.segments.as_slice());
                // Copying a collection in durable batches is the expensive way
                // to publish it: leave to the incremental checkpoint every
                // delta it takes, however many rows that is.
                let indexes = self
                    .key_indices
                    .read()
                    .get(&name)
                    .map_or(0, |keys| keys.secondary.len());
                if manager
                    .checkpoint_mutations_since(before)
                    .is_some_and(|ids| {
                        ids.len() <= CHECKPOINT_BATCH_ROWS
                            || super::checkpoint_delta::delta_fits(
                                ids.len(),
                                manager.count(),
                                indexes,
                            )
                    })
                {
                    continue;
                }
                let Some(input) = Input::capture(self, &manager, &excluded, &pending, limit) else {
                    continue;
                };
                if input.overlay.len() <= CHECKPOINT_BATCH_ROWS {
                    continue;
                }
                chosen = Some(PreparedPrimary {
                    collection: name,
                    tree: Self::new_derived_btree(pager)?,
                    manager: Arc::downgrade(&manager),
                    input,
                    version: self.format_version(),
                    limit,
                });
                break;
            }
            chosen
        };
        let Some(prepared) = prepared else {
            return Ok(None);
        };
        let mut records = Vec::new();
        let mut bytes = 0usize;
        let mut accept = |id: EntityId, value: &[u8]| -> Result<(), StoreError> {
            if !records.is_empty()
                && (records.len() == CHECKPOINT_BATCH_ROWS
                    || bytes.saturating_add(value.len() + 8) > limit.min(CHECKPOINT_BATCH_BYTES))
            {
                self.write_staged_primary_batch(&prepared.tree, &records)?;
                records.clear();
                bytes = 0;
            }
            bytes += value.len() + 8;
            records.push((id.raw().to_be_bytes().to_vec(), value.to_vec()));
            Ok(())
        };
        if let Some(base) = &prepared.input.base {
            let view = MergedCollection::new(base.as_ref(), Arc::clone(&prepared.input.overlay));
            let mut cursor = view.cursor()?;
            loop {
                match cursor.next_record(limit) {
                    Ok(Some(record)) => accept(record.id, &record.bytes)?,
                    Ok(None) => break,
                    Err(StoreError::ReadLimitExceeded { .. }) => return Ok(None),
                    Err(error) => return Err(error),
                }
            }
        } else {
            for (id, value) in prepared.input.overlay.iter() {
                if let Some(value) = value {
                    accept(*id, value)?;
                }
            }
        }
        if !records.is_empty() {
            self.write_staged_primary_batch(&prepared.tree, &records)?;
        }
        Ok(Some(prepared))
    }

    fn write_staged_primary_batch(
        &self,
        tree: &Arc<BTree>,
        records: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<(), StoreError> {
        {
            let _publication = self.vault_publication_lock.lock();
            let pager = self.pager.as_ref().expect("paged checkpoint");
            let _ownership = pager.lock_ownership_mutations();
            let commit = self.commit.as_ref().expect("logged checkpoint");
            commit.force_sync().map_err(StoreError::Io)?;
            let epoch = commit
                .next_page_ownership_epoch(self)
                .map_err(StoreError::Io)?;
            commit
                .begin_page_ownership_epoch(self, epoch)
                .map_err(StoreError::Io)?;
            struct Fence<'a>(&'a Pager, bool);
            impl Drop for Fence<'_> {
                fn drop(&mut self) {
                    if self.1 {
                        self.0.fence_interrupted_ownership();
                    }
                }
            }
            let mut fence = Fence(pager, true);
            tree.bulk_insert_sorted(records)
                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
            self.retire_btree(Arc::clone(tree));
            self.flush_paged_registry()?;
            commit
                .finish_count_batch_ownership_epoch(self, epoch)
                .map_err(StoreError::Io)?;
            fence.1 = false;
        }
        #[cfg(test)]
        super::checkpoint_overlap::after_primary_batch(records);
        Ok(())
    }
}
