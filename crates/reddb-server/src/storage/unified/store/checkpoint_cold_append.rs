//! WAL-proven, bounded checkpoint changes to rows in a published disk generation.
use super::*;
use reddb_engine::storage::segment_key_lookup::SegmentKeyLookup;

use super::secondary_index::{SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX};
use reddb_engine::storage::collection_read::EncodedRecord;

fn posting(
    projector: &dyn SecondaryIndexProjector,
    entity: &UnifiedEntity,
) -> Result<Option<Vec<u8>>, StoreError> {
    let mut key = [0; SECONDARY_KEY_BYTES_MAX];
    let Some(length) = projector.project(entity, &mut key)? else {
        return Ok(None);
    };
    let bytes = key.get(..length).ok_or_else(|| {
        StoreError::Serialization("secondary key exceeds its bounded buffer".into())
    })?;
    let mut posting = Vec::with_capacity(length + 8);
    posting.extend_from_slice(bytes);
    posting.extend_from_slice(&entity.id.raw().to_be_bytes());
    Ok(Some(posting))
}

fn unkeyed_row(entity: &UnifiedEntity) -> bool {
    matches!(&entity.data, EntityData::Row(row)
        if row.named.is_some() || row.schema.is_some() || row.columns.is_empty())
        && SegmentKeyLookup::key(entity).is_none()
        && entity.branch_version().is_none()
}

impl UnifiedStore {
    pub(super) fn sparse_cold_row_tree(
        &self,
        collection: &str,
        manager: &Arc<SegmentManager>,
        tree: &Arc<BTree>,
        previous: &CheckpointVersion,
        current: &mut CheckpointVersion,
    ) -> Result<Option<Arc<BTree>>, StoreError> {
        // An unpublished/normalized row can have a different set of postings.
        // Only reconcile the projections of complete, committed row images.
        if self.commit.is_none()
            || self.ownership_active_epoch.load(Ordering::Acquire) == 0
            || !previous.segments.is_empty()
            || previous.pending_vault != current.pending_vault
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
        let root_before = tree.root_page_id();
        manager
            .try_with_unshared_backing_delta(
                root_before,
                crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX,
                |removed| {
                    let mut changed_xid_high_water_mark = 0;
                    let result = (|| {
                        // The store and the sole backing source are the only tree owners.
                        // Owned cursors can outlive their source: check their separate pin.
                        if Arc::strong_count(tree) != 2 || tree.has_retirement_readers() {
                            return Ok(None);
                        }
                        let ids: std::collections::BTreeSet<_> =
                            ids.iter().chain(removed).copied().collect();
                        // Bound projection work separately from retained memory. Visibility
                        // and equality keys are usually much shorter than the codec maximum.
                        const POSTING_BUDGET: usize = 1024 * 1024;
                        let posting_count = ids.len().saturating_mul(keys.secondary.len());
                        let max_changes =
                            crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX;
                        if ids.len() > max_changes || posting_count > max_changes {
                            return Ok(None);
                        }
                        let mut data_deletes = Vec::new();
                        let mut postings = Vec::with_capacity(posting_count);
                        let mut retained_bytes = postings.capacity().saturating_mul(
                            std::mem::size_of::<(&Arc<BTree>, Option<Vec<u8>>, Option<Vec<u8>>)>(),
                        );
                        if retained_bytes > POSTING_BUDGET {
                            return Ok(None);
                        }
                        for id in &ids {
                            let eligible = manager.get_with(*id, |entity| {
                                changed_xid_high_water_mark =
                                    changed_xid_high_water_mark.max(entity.max_transaction_id());
                                unkeyed_row(entity)
                                    && !current.excluded_xids.contains(&entity.xmin)
                                    && (entity.xmax == 0
                                        || !current.excluded_xids.contains(&entity.xmax))
                                    && !current.pending_vault.contains(id)
                            });
                            let deleting = removed.contains(id);
                            if eligible != (if deleting { None } else { Some(true) }) {
                                return Ok(None);
                            }
                            // Keep the old image only while comparing this row's projections.
                            let limit = 1024 * 1024;
                            let old = match tree.get_with_budget(&id.raw().to_be_bytes(), limit) {
                                Ok(Some(bytes)) => {
                                    let record = EncodedRecord { id: *id, bytes };
                                    match record.decode_with_budget(self.format_version(), limit) {
                                        Ok((entity, _)) if unkeyed_row(&entity) => Some(entity),
                                        Ok(_) | Err(StoreError::ReadLimitExceeded { .. }) => {
                                            return Ok(None)
                                        }
                                        Err(error) => return Err(error),
                                    }
                                }
                                Ok(None) => None,
                                Err(BTreeError::ReadLimitExceeded { .. }) => return Ok(None),
                                Err(error) => {
                                    return Err(StoreError::Io(std::io::Error::other(
                                        error.to_string(),
                                    )))
                                }
                            };
                            if deleting && old.is_some() {
                                data_deletes.push(*id);
                            }
                            if let Some(specs) = specs {
                                for (name, index) in &keys.secondary {
                                    let projector = specs[name].projector.as_ref();
                                    let before = old
                                        .as_ref()
                                        .map(|row| posting(projector, row))
                                        .transpose()?
                                        .flatten();
                                    let after = if deleting {
                                        None
                                    } else {
                                        manager
                                            .get_with(*id, |row| posting(projector, row))
                                            .ok_or(StoreError::EntityNotFound(*id))??
                                    };
                                    if before == after {
                                        continue;
                                    }
                                    retained_bytes = retained_bytes
                                        .saturating_add(before.as_ref().map_or(0, Vec::capacity))
                                        .saturating_add(after.as_ref().map_or(0, Vec::capacity));
                                    if retained_bytes > POSTING_BUDGET {
                                        return Ok(None);
                                    }
                                    for (key, expected) in
                                        [(before.as_ref(), true), (after.as_ref(), false)]
                                    {
                                        if let Some(key) = key {
                                            let found = index.tree.get(key).map_err(|e| {
                                                StoreError::Io(std::io::Error::other(e.to_string()))
                                            })?;
                                            if found.is_some() != expected {
                                                return Ok(None);
                                            }
                                        }
                                    }
                                    postings.push((&index.tree, before, after));
                                }
                            }
                        }
                        let applied = self.apply_sparse_checkpoint_tree(
                            collection, manager, tree, previous, current, removed,
                        )?;
                        if applied.is_some() {
                            // Data and projections share the active page-undo epoch;
                            // their roots publish together in the existing catalog fence.
                            for id in data_deletes {
                                if !tree.delete(&id.raw().to_be_bytes()).map_err(|e| {
                                    StoreError::Io(std::io::Error::other(e.to_string()))
                                })? {
                                    return Err(StoreError::Serialization(
                                        "validated physical row disappeared".into(),
                                    ));
                                }
                            }
                            for (index, before, after) in postings {
                                if let Some(key) = after {
                                    index.insert(&key, &[]).map_err(|e| {
                                        StoreError::Io(std::io::Error::other(e.to_string()))
                                    })?;
                                }
                                if let Some(key) = before {
                                    let removed = index.delete(&key).map_err(|e| {
                                        StoreError::Io(std::io::Error::other(e.to_string()))
                                    })?;
                                    if !removed {
                                        return Err(StoreError::Serialization(
                                            "validated secondary posting disappeared".into(),
                                        ));
                                    }
                                }
                            }
                        }
                        Ok(applied)
                    })();
                    let published = if result.as_ref().is_ok_and(Option::is_some) {
                        let root = tree.root_page_id();
                        // Carry the changed headers into the existing upper bound
                        // even when an in-place update retains the same root page.
                        // Publish the exact count without recounting old payloads.
                        if let Some(summary) = self.recovery_xid_summary.write().as_mut() {
                            if summary.roots.get(collection) == Some(&root_before) {
                                summary.high_water_mark =
                                    summary.high_water_mark.max(changed_xid_high_water_mark);
                                summary.roots.insert(collection.to_owned(), root);
                            }
                        }
                        Some((root, manager.count() as u64))
                    } else {
                        None
                    };
                    (result, published)
                },
            )
            .unwrap_or(Ok(None))
    }
}
