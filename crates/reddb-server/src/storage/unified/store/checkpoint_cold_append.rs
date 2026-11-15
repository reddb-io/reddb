//! Sparse publication for committed cold inserts and bounded replacements.
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
        if self.commit.is_none()
            || self.ownership_active_epoch.load(Ordering::Acquire) == 0
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
            // The published generation and its keyed PagedCollection are the
            // two stable owners. Owned cursors are tracked separately below.
            || Arc::strong_count(&keys.tree) != 2
            || keys.tree.has_retirement_readers()
        {
            return Ok(None);
        }
        let specs = self.secondary_index_specs.read();
        let specs = specs.get(collection);
        if keys.secondary.len() != specs.map_or(0, |specs| specs.len())
            || keys.secondary.iter().any(|(name, index)| {
                specs.and_then(|specs| specs.get(name)).is_none_or(|spec| {
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
        let posting_count = ids
            .len()
            .saturating_mul(keys.secondary.len().saturating_add(1));
        let posting_bytes = posting_count.saturating_mul(
            super::secondary_index::SECONDARY_KEY_BYTES_MAX
                + 8
                + std::mem::size_of::<(&Arc<BTree>, Vec<u8>)>(),
        );
        if posting_bytes > 1024 * 1024 {
            return Ok(None);
        }

        let mut removed = Vec::new();
        let mut derived_deletes = Vec::with_capacity(posting_count);
        let mut derived_inserts = Vec::with_capacity(posting_count);
        let mut appended_xid_high_water_mark = 0;
        let mut inserted_entities = 0_u64;
        for id in &ids {
            let encoded = tree
                .get(&id.raw().to_be_bytes())
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
            let old = encoded
                .as_deref()
                .map(|bytes| Self::deserialize_entity_record(bytes, self.format_version()))
                .transpose()?
                .map(|(entity, _)| entity);
            let resident = manager.get_with(*id, Clone::clone);
            let deferred = resident.as_ref().is_some_and(|entity| {
                current.excluded_xids.contains(&entity.xmin) || current.pending_vault.contains(id)
            });
            // A pending replacement is not part of this checkpoint. Validate
            // derived postings against the still-published base row and let
            // `apply_sparse_checkpoint_tree` retain the ID for the next
            // commit/rollback projection.
            let new = if deferred { old.clone() } else { resident };
            if old.is_none() && new.is_some() {
                inserted_entities = inserted_entities
                    .checked_add(1)
                    .ok_or_else(|| StoreError::Internal("backing entity count overflow".into()))?;
            }
            if let Some(entity) = new.as_ref() {
                let EntityData::Row(row) = &entity.data else {
                    return Ok(None);
                };
                if entity.xmax != 0
                    || (row.named.is_none() && row.schema.is_none() && !row.columns.is_empty())
                {
                    return Ok(None);
                }
                appended_xid_high_water_mark = appended_xid_high_water_mark.max(entity.xmin);
            } else if old.is_some() {
                removed.push(*id);
            }

            let old_key = old.as_ref().and_then(SegmentKeyLookup::key).map(|key| {
                let fingerprint = reddb_file::native_key_index::fingerprint(key);
                reddb_file::native_key_index::encode_candidate(&fingerprint, id.raw()).to_vec()
            });
            let new_key = new.as_ref().and_then(SegmentKeyLookup::key).map(|key| {
                let fingerprint = reddb_file::native_key_index::fingerprint(key);
                reddb_file::native_key_index::encode_candidate(&fingerprint, id.raw()).to_vec()
            });
            if old_key != new_key {
                if let Some(posting) = old_key {
                    derived_deletes.push((&keys.tree, posting));
                }
                if let Some(posting) = new_key {
                    derived_inserts.push((&keys.tree, posting));
                }
            }

            if let Some(specs) = specs {
                for (name, index) in &keys.secondary {
                    let spec = &specs[name];
                    let project =
                        |entity: Option<&UnifiedEntity>| -> Result<Option<Vec<u8>>, StoreError> {
                            let Some(entity) = entity else {
                                return Ok(None);
                            };
                            let mut key = [0; super::secondary_index::SECONDARY_KEY_BYTES_MAX];
                            let Some(length) = spec.projector.project(entity, &mut key)? else {
                                return Ok(None);
                            };
                            if length > key.len() {
                                return Err(StoreError::Serialization(
                                    "secondary key exceeds its bounded buffer".into(),
                                ));
                            }
                            let mut posting = Vec::with_capacity(length + 8);
                            posting.extend_from_slice(&key[..length]);
                            posting.extend_from_slice(&id.raw().to_be_bytes());
                            Ok(Some(posting))
                        };
                    let old_posting = project(old.as_ref())?;
                    let new_posting = project(new.as_ref())?;
                    if old_posting != new_posting {
                        if let Some(posting) = old_posting {
                            derived_deletes.push((&index.tree, posting));
                        }
                        if let Some(posting) = new_posting {
                            derived_inserts.push((&index.tree, posting));
                        }
                    }
                }
            }
        }
        for (index, posting) in &derived_deletes {
            if index
                .get(posting)
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?
                .is_none()
            {
                return Ok(None);
            }
        }
        for (index, posting) in &derived_inserts {
            if index
                .get(posting)
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?
                .is_some()
            {
                return Ok(None);
            }
        }

        let root_before = tree.root_page_id();
        let backing_entities = manager.backing_count_at_root(root_before).ok_or_else(|| {
            StoreError::Internal("cold checkpoint lost its backing entity count".into())
        })?;
        let removed_entities = u64::try_from(removed.len())
            .map_err(|_| StoreError::Internal("backing removal count exceeds u64".into()))?;
        let published_entities = backing_entities
            .checked_add(inserted_entities)
            .and_then(|count| count.checked_sub(removed_entities))
            .ok_or_else(|| StoreError::Internal("invalid backing entity count delta".into()))?;
        manager
            .try_with_unshared_backing_changes(root_before, &removed, || {
                let result = (|| {
                    if Arc::strong_count(tree) != 2 || tree.has_retirement_readers() {
                        return Ok(None);
                    }
                    let applied = self.apply_sparse_checkpoint_tree(
                        collection, manager, tree, previous, current,
                    )?;
                    if applied.is_some() {
                        for (index, posting) in &derived_deletes {
                            if !index.delete(posting).map_err(|error| {
                                StoreError::Io(std::io::Error::other(error.to_string()))
                            })? {
                                return Err(StoreError::Internal(
                                    "validated checkpoint posting disappeared".into(),
                                ));
                            }
                        }
                        for (index, posting) in &derived_inserts {
                            index.insert(posting, &[]).map_err(|error| {
                                StoreError::Io(std::io::Error::other(error.to_string()))
                            })?;
                        }
                    }
                    Ok(applied)
                })();
                let published = if result.as_ref().is_ok_and(Option::is_some) {
                    let root = tree.root_page_id();
                    if let Some(summary) = self.recovery_xid_summary.write().as_mut() {
                        if summary.roots.get(collection) == Some(&root_before) {
                            summary.high_water_mark =
                                summary.high_water_mark.max(appended_xid_high_water_mark);
                            summary.roots.insert(collection.to_owned(), root);
                        }
                    }
                    Some((root, published_entities))
                } else {
                    None
                };
                (result, published)
            })
            .unwrap_or(Ok(None))
    }
}
