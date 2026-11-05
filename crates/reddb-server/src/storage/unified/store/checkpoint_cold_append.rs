//! Sparse publication for committed cold inserts, including SQL autocommit.
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
        // Updates, deletes, keyed rows and MVCC changes
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
        if !keys.source.ptr_eq(&Arc::downgrade(tree)) {
            return Ok(None);
        }
        let specs = self.secondary_index_specs.read();
        let specs = specs.get(collection);
        // Include internal visibility/count projections, not just declared SQL
        // indexes. Every published tree needs its exact deterministic projector.
        // Missing/new definitions and pinned generations retain replacement.
        if keys.secondary.len() != specs.map_or(0, |s| s.len())
            || keys.secondary.iter().any(|(name, index)| {
                specs.and_then(|s| s.get(name)).is_none_or(|spec| {
                    spec.identity != index.identity
                        || Arc::strong_count(&index.tree) != 1
                        || index.tree.has_retirement_readers()
                })
            })
        {
            return Ok(None);
        }
        let Some(ids) = manager.checkpoint_mutations_since(&previous.segments) else {
            return Ok(None);
        };
        // Bound both retained posting bytes and projection work before allocation.
        // Larger batches keep the existing bounded streaming replacement path.
        let posting_count = ids.len().saturating_mul(keys.secondary.len());
        let posting_bytes = posting_count.saturating_mul(
            super::secondary_index::SECONDARY_KEY_BYTES_MAX
                + 8
                + std::mem::size_of::<(&Arc<BTree>, Vec<u8>)>(),
        );
        if posting_bytes > 1024 * 1024 {
            return Ok(None);
        }
        let root_before = tree.root_page_id();
        manager
            .try_with_unshared_backing(root_before, || {
                let mut appended_xid_high_water_mark = 0;
                let result = (|| {
                    // The store and the sole backing source are the only tree owners.
                    // Owned cursors can outlive their source: check their separate pin.
                    if Arc::strong_count(tree) != 2 || tree.has_retirement_readers() {
                        return Ok(None);
                    }
                    let mut postings = Vec::with_capacity(posting_count);
                    for id in &ids {
                        let eligible = manager.get_with(*id, |entity| {
                            let EntityData::Row(row) = &entity.data else {
                                return false;
                            };
                            appended_xid_high_water_mark =
                                appended_xid_high_water_mark.max(entity.xmin);
                            !current.excluded_xids.contains(&entity.xmin)
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
                        if let Some(specs) = specs {
                            for (name, index) in &keys.secondary {
                                let spec = &specs[name];
                                let mut key = [0; super::secondary_index::SECONDARY_KEY_BYTES_MAX];
                                let length = manager
                                    .get_with(*id, |entity| {
                                        spec.projector.project(entity, &mut key)
                                    })
                                    .ok_or(StoreError::EntityNotFound(*id))??;
                                let Some(length) = length else { continue };
                                if length > key.len() {
                                    return Err(StoreError::Serialization(
                                        "secondary key exceeds its bounded buffer".into(),
                                    ));
                                }
                                let mut posting = Vec::with_capacity(length + 8);
                                posting.extend_from_slice(&key[..length]);
                                posting.extend_from_slice(&id.raw().to_be_bytes());
                                if index
                                    .tree
                                    .get(&posting)
                                    .map_err(|e| {
                                        StoreError::Io(std::io::Error::other(e.to_string()))
                                    })?
                                    .is_some()
                                {
                                    return Ok(None);
                                }
                                postings.push((&index.tree, posting));
                            }
                        }
                    }
                    let applied = self.apply_sparse_checkpoint_tree(
                        collection, manager, tree, previous, current,
                    )?;
                    if applied.is_some() {
                        // Data and projections share the active page-undo epoch;
                        // their roots publish together in the existing catalog fence.
                        for (index, posting) in postings {
                            index.insert(&posting, &[]).map_err(|e| {
                                StoreError::Io(std::io::Error::other(e.to_string()))
                            })?;
                        }
                    }
                    Ok(applied)
                })();
                let published = if result.as_ref().is_ok_and(Option::is_some) {
                    let root = tree.root_page_id();
                    // Carry the appended headers into the existing upper bound
                    // even when an in-place update retains the same root page.
                    // Publish the exact count without recounting old payloads.
                    if let Some(summary) = self.recovery_xid_summary.write().as_mut() {
                        if summary.roots.get(collection) == Some(&root_before) {
                            summary.high_water_mark =
                                summary.high_water_mark.max(appended_xid_high_water_mark);
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
