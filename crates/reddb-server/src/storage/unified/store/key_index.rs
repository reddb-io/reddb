//! Prepared key indexes share the data generation's publication and retirement.
use super::*;
use reddb_engine::storage::collection_read::{CollectionCursor, PagedCollection};
use reddb_engine::storage::segment_key_lookup::SegmentKeyLookup;

impl UnifiedStore {
    /// Physical redo can change records without changing their root address.
    /// Recount bases and discard derived indexes before this private copy is
    /// published; normal startup recreates secondary indexes from definitions.
    pub(crate) fn reconcile_physical_recovery(&self) -> Result<(), StoreError> {
        let _publication = self.vault_publication_lock.lock();
        if self.config.resident_memory_bytes_max.is_none() {
            return Ok(());
        }
        for (name, tree) in self.btree_indices.read().iter() {
            let manager = self
                .get_collection(name)
                .ok_or_else(|| StoreError::CollectionNotFound(name.clone()))?;
            let root = tree.root_page_id();
            if root != 0 && manager.backing_count_at_root(root).is_none() {
                return Err(StoreError::Io(std::io::Error::other(
                    "physical recovery cannot verify the collection backing",
                )));
            }
            if manager.backing_count_at_root(root).is_some() {
                let mut cursor = tree
                    .cursor_first()
                    .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
                let mut count = 0u64;
                while cursor
                    .skip_next()
                    .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?
                {
                    count = count.checked_add(1).ok_or_else(|| {
                        StoreError::Io(std::io::Error::other("recovered count overflow"))
                    })?;
                }
                if !manager.set_recovered_backing_count(root, count) {
                    return Err(StoreError::Io(std::io::Error::other(
                        "recovered backing changed",
                    )));
                }
            }
        }
        let previous = std::mem::take(&mut *self.key_indices.write());
        for index in previous.values() {
            for tree in index.trees() {
                self.retire_btree(Arc::clone(tree));
            }
        }
        self.resident
            .paged_registry_dirty
            .store(true, Ordering::Release);
        Ok(())
    }

    pub(super) fn checkpoint_key_indices(
        &self,
        pager: &Arc<Pager>,
        live: &HashMap<String, Arc<BTree>>,
    ) -> Result<HashMap<String, PublishedKeyIndex>, StoreError> {
        if self.config.resident_memory_bytes_max.is_none() {
            return Ok(HashMap::new());
        }
        let limit = usize::try_from(self.config.resident_memory_bytes_max.expect("disk profile"))
            .unwrap_or(usize::MAX);
        let previous = self.key_indices.read();
        let mut next = HashMap::with_capacity(live.len());
        for (name, data) in live {
            let secondary =
                self.checkpoint_secondary_indices(name, data, previous.get(name), pager, limit)?;
            if let Some(index) = previous
                .get(name)
                .filter(|index| index.source.ptr_eq(&Arc::downgrade(data)))
            {
                let mut reused = index.clone();
                reused.secondary = secondary;
                next.insert(name.clone(), reused);
                continue;
            }
            let keys = Arc::new(BTree::new(Arc::clone(pager)));
            let source = PagedCollection::new(Arc::clone(data));
            let mut cursor = source.scan()?;
            let mut indexed = true;
            while let Some(record) = cursor.next_record(limit)? {
                let (entity, _) = record.decode_with_budget(self.format_version(), limit)?;
                // A legacy positional row has no durable column names. Never
                // turn an unclassified key into a trusted indexed absence.
                if matches!(&entity.data, EntityData::Row(row) if row.named.is_none()
                    && row.schema.is_none() && !row.columns.is_empty())
                {
                    indexed = false;
                    break;
                }
                if let Some(key) = SegmentKeyLookup::key(&entity) {
                    let fingerprint = reddb_file::native_key_index::fingerprint(key);
                    let candidate = reddb_file::native_key_index::encode_candidate(
                        &fingerprint,
                        entity.id.raw(),
                    );
                    keys.insert(&candidate, &[]).map_err(|error| {
                        StoreError::Io(std::io::Error::other(error.to_string()))
                    })?;
                }
            }
            let keys = if indexed {
                keys
            } else {
                self.retire_btree(keys);
                Arc::new(BTree::new(Arc::clone(pager)))
            };
            next.insert(
                name.clone(),
                PublishedKeyIndex {
                    indexed,
                    source: Arc::downgrade(data),
                    tree: keys,
                    secondary,
                },
            );
        }
        for (name, old) in previous.iter() {
            for tree in old.trees() {
                if !next
                    .get(name)
                    .is_some_and(|index| index.trees().any(|retained| Arc::ptr_eq(retained, tree)))
                {
                    self.retire_btree(Arc::clone(tree));
                }
            }
        }
        Ok(next)
    }
}
