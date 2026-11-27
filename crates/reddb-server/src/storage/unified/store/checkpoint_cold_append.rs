//! WAL-proven, bounded checkpoint changes to rows in a published disk generation.
use super::*;
use reddb_engine::storage::segment_key_lookup::SegmentKeyLookup;

use super::secondary_index::{SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX};
use reddb_engine::storage::collection_read::{EncodedRecord, PagedCollection};

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

fn classified_row(entity: &UnifiedEntity) -> bool {
    matches!(&entity.data, EntityData::Row(row)
        if row.named.is_some() || row.schema.is_some() || row.columns.is_empty())
        && entity.branch_version().is_none()
}

fn key_posting(row: &UnifiedEntity) -> Option<Vec<u8>> {
    let key = SegmentKeyLookup::key(row)?;
    let fingerprint = reddb_file::native_key_index::fingerprint(key);
    Some(reddb_file::native_key_index::encode_candidate(&fingerprint, row.id.raw()).to_vec())
}

type PostingChange<'a> = (&'a Arc<BTree>, Option<Vec<u8>>, Option<Vec<u8>>);

#[derive(Default)]
struct PostingPlan<'a> {
    changes: Vec<PostingChange<'a>>,
    payload_bytes: usize,
}

impl<'a> PostingPlan<'a> {
    fn add(
        &mut self,
        tree: &'a Arc<BTree>,
        before: Option<Vec<u8>>,
        after: Option<Vec<u8>>,
    ) -> Result<bool, StoreError> {
        if before == after {
            return Ok(true);
        }
        if self.changes.len() >= super::checkpoint_delta::POSTING_CHANGES_MAX {
            return Ok(super::checkpoint_delta::refused(
                "more than 65536 index entries changed",
            ));
        }
        self.payload_bytes = self
            .payload_bytes
            .saturating_add(before.as_ref().map_or(0, Vec::capacity))
            .saturating_add(after.as_ref().map_or(0, Vec::capacity));
        self.changes.reserve(1);
        let retained = self
            .changes
            .capacity()
            .saturating_mul(std::mem::size_of::<PostingChange>())
            .saturating_add(self.payload_bytes);
        if retained > 8 * 1024 * 1024 {
            return Ok(super::checkpoint_delta::refused(
                "the changed index entries exceed 8 MiB",
            ));
        }
        for (key, expected) in [(before.as_ref(), true), (after.as_ref(), false)] {
            if let Some(key) = key {
                let found = tree
                    .get(key)
                    .map_err(|e| StoreError::Io(std::io::Error::other(e.to_string())))?;
                if found.is_some() != expected {
                    return Ok(super::checkpoint_delta::refused(
                        "an index entry does not match its row",
                    ));
                }
            }
        }
        self.changes.push((tree, before, after));
        Ok(true)
    }
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
            return super::checkpoint_delta::declined("no ownership epoch, or the previous checkpoint left resident, deferred or vault rows");
        }
        let keys = self.key_indices.read();
        let Some(keys) = keys.get(collection) else {
            return super::checkpoint_delta::declined("the collection has no published key index");
        };
        if !keys.source.ptr_eq(&Arc::downgrade(tree)) {
            return super::checkpoint_delta::declined(
                "the key index was built from another data tree",
            );
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
            return super::checkpoint_delta::declined(
                "an index definition changed, or a reader holds an index tree",
            );
        }
        let Some(ids) = manager.checkpoint_mutations_since(&previous.segments) else {
            return super::checkpoint_delta::declined(
                "the mutation journal no longer reaches the previous checkpoint",
            );
        };
        let root_before = tree.root_page_id();
        manager
            .try_with_unshared_backing_source_delta(
                root_before,
                crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX,
                |source, removed| {
                    let mut changed_xid_high_water_mark = 0;
                    let result = (|| {
                        // The store and the sole backing source are the only tree owners.
                        // Owned cursors can outlive their source: check their separate pin.
                        if Arc::strong_count(tree) != 2 || tree.has_retirement_readers() {
                            return super::checkpoint_delta::declined("a reader holds the data tree");
                        }
                        let exclusive_keys = keys.indexed
                            && Arc::strong_count(&keys.tree) == 2
                            && !keys.tree.has_retirement_readers()
                            && PagedCollection::with_key_index(
                                Arc::clone(tree),
                                Arc::clone(&keys.tree),
                            )
                            .shares_trees_with(source);
                        let ids: std::collections::BTreeSet<_> =
                            ids.iter().chain(removed).copied().collect();
                        // Bound projection work separately from retained memory. Visibility
                        // and equality keys are usually much shorter than the codec maximum.
                        let posting_count = ids.len().saturating_mul(keys.secondary.len());
                        let max_changes =
                            crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX;
                        if ids.len() > max_changes
                            || posting_count > super::checkpoint_delta::POSTING_CHANGES_MAX
                        {
                            return super::checkpoint_delta::declined("more than 8192 changed rows, or 65536 index entries");
                        }
                        let mut data_deletes = Vec::new();
                        let mut postings = PostingPlan::default();
                        for id in &ids {
                            let mut after_key = None;
                            let eligible = manager.get_with(*id, |entity| {
                                changed_xid_high_water_mark =
                                    changed_xid_high_water_mark.max(entity.max_transaction_id());
                                after_key = key_posting(entity);
                                classified_row(entity)
                                    && !current.excluded_xids.contains(&entity.xmin)
                                    && (entity.xmax == 0
                                        || !current.excluded_xids.contains(&entity.xmax))
                                    && !current.pending_vault.contains(id)
                            });
                            let deleting = removed.contains(id);
                            if eligible != (if deleting { None } else { Some(true) }) {
                                return super::checkpoint_delta::declined("a changed row is uncommitted, in the vault, or not a classified table row");
                            }
                            // Keep the old image only while comparing this row's projections.
                            let limit = 1024 * 1024;
                            let old = match tree.get_with_budget(&id.raw().to_be_bytes(), limit) {
                                Ok(Some(bytes)) => {
                                    let record = EncodedRecord { id: *id, bytes };
                                    match record.decode_with_budget(self.format_version(), limit) {
                                        Ok((entity, _)) if classified_row(&entity) => Some(entity),
                                        Ok(_) | Err(StoreError::ReadLimitExceeded { .. }) => {
                                            return super::checkpoint_delta::declined("a published row is unclassified or over 1 MiB")
                                        }
                                        Err(error) => return Err(error),
                                    }
                                }
                                Ok(None) => None,
                                Err(BTreeError::ReadLimitExceeded { .. }) => return super::checkpoint_delta::declined("a published row is over 1 MiB"),
                                Err(error) => {
                                    return Err(StoreError::Io(std::io::Error::other(
                                        error.to_string(),
                                    )))
                                }
                            };
                            if deleting && old.is_some() {
                                data_deletes.push(*id);
                            }
                            let before_key = old.as_ref().and_then(key_posting);
                            if before_key != after_key
                                && (!exclusive_keys
                                    || !postings.add(&keys.tree, before_key, after_key)?)
                            {
                                return super::checkpoint_delta::declined("the key index is shared with a reader");
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
                                    if !postings.add(&index.tree, before, after)? {
                                        return super::checkpoint_delta::declined("an index refused the change");
                                    }
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
                            for (index, before, after) in postings.changes {
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
                                            "validated index posting disappeared".into(),
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
            .unwrap_or_else(|| {
                super::checkpoint_delta::declined(manager.backing_delta_refusal(
                    root_before,
                    crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX,
                ))
            })
    }
}
