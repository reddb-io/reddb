//! Sparse publication for nontransactional, unindexed cold inserts (e.g. audit history).
use super::*;
use reddb_engine::storage::segment_key_lookup::SegmentKeyLookup;

impl UnifiedStore {
    pub(super) fn sparse_cold_append_tree(
        &self,
        collection: &str,
        manager: &Arc<SegmentManager>,
        tree: &Arc<BTree>,
        previous: &CheckpointVersion,
        current: &mut CheckpointVersion,
    ) -> Result<Option<Arc<BTree>>, StoreError> {
        // Updates, deletes, keyed rows, indexed projections and MVCC changes
        // retain immutable replacement. This path reuses the existing durable
        // page-undo epoch and exact WAL-image proof for new audit-style rows.
        if self.commit.is_none()
            || self.ownership_active_epoch.load(Ordering::Acquire) == 0
            || !previous.segments.is_empty()
            || previous.has_unprojected_rows
            || previous
                .deferred_rows
                .as_ref()
                .is_none_or(|ids| !ids.is_empty())
        {
            return Ok(None);
        }
        let keys = self.key_indices.read();
        let Some(keys) = keys.get(collection) else {
            return Ok(None);
        };
        if !keys.source.ptr_eq(&Arc::downgrade(tree))
            || !keys.secondary.is_empty()
            || self
                .secondary_index_specs
                .read()
                .get(collection)
                .is_some_and(|s| !s.is_empty())
        {
            return Ok(None);
        }
        let Some(ids) = manager.checkpoint_mutations_since(&previous.segments) else {
            return Ok(None);
        };
        let root_before = tree.root_page_id();
        manager
            .try_with_unshared_backing(root_before, || {
                let result = (|| {
                    // The store and the sole backing source are the only tree owners.
                    // Owned cursors can outlive their source: check their separate pin.
                    if Arc::strong_count(tree) != 2 || tree.has_retirement_readers() {
                        return Ok(None);
                    }
                    for id in &ids {
                        let eligible = manager.get_with(*id, |entity| {
                            let EntityData::Row(row) = &entity.data else {
                                return false;
                            };
                            entity.xmin == 0
                                && entity.xmax == 0
                                && !current.pending_vault.contains(id)
                                && SegmentKeyLookup::key(entity).is_none()
                                && (row.named.is_some()
                                    || row.schema.is_some()
                                    || row.columns.is_empty())
                        });
                        if eligible != Some(true)
                            || tree
                                .get(&id.raw().to_be_bytes())
                                .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?
                                .is_some()
                        {
                            return Ok(None);
                        }
                    }
                    self.apply_sparse_checkpoint_tree(collection, manager, tree, previous, current)
                })();
                let published = if result.as_ref().is_ok_and(Option::is_some) {
                    let root = tree.root_page_id();
                    // Appended rows have XID zero, so an existing base bound remains
                    // valid even if a split changed the root. Publish the new exact
                    // count too, avoiding an O(history) recount of an in-place tree.
                    if let Some(summary) = self.recovery_xid_summary.write().as_mut() {
                        if summary.roots.get(collection) == Some(&root_before) {
                            summary.roots.insert(collection.to_owned(), root);
                        }
                    }
                    Some((root, manager.count() as u64))
                } else {
                    None
                };
                (result, published)
            })
            .unwrap_or(Ok(None))
    }
}
