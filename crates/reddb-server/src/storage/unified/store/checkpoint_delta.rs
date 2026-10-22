//! Bounded checkpoint projection of rows whose exact image is already in the WAL.
use super::commit::StoreWalAction;
use super::*;

#[derive(Default)]
pub(super) struct DurableRows {
    rows: HashMap<(String, EntityId), Vec<u8>>,
    bytes: usize,
}

impl DurableRows {
    pub(super) fn memory_bytes(&self) -> usize {
        self.bytes
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
        let previous = self.rows.get(&key);
        let old = previous.map_or(0, Vec::capacity);
        let overhead = if previous.is_none() {
            collection.len() + 128
        } else {
            0
        };
        let mut bytes = self
            .bytes
            .saturating_sub(old)
            .saturating_add(record.capacity())
            .saturating_add(overhead);
        if bytes > limit || (previous.is_none() && self.rows.len() == 1024) {
            self.rows.clear();
            self.rows.shrink_to_fit();
            self.bytes = 0;
            // Evicted images retain the conservative validation path. Keep
            // accepting later WAL images so unrelated pressure does not force
            // every subsequent changed collection through a full rewrite.
            bytes = record.capacity().saturating_add(collection.len() + 128);
            if bytes > limit {
                return;
            }
        }
        self.rows.insert(key, record);
        self.bytes = bytes;
    }
}

impl UnifiedStore {
    /// Called only after the WAL append succeeds. The ordinary inline KV path
    /// does not retain records here. The checkpoint's existing force_sync is
    /// the durability fence before any sparse projection changes pager pages.
    pub(super) fn remember_checkpoint_rows(&self, actions: Vec<StoreWalAction>) {
        if self.pager.is_none() || self.commit.is_none() {
            return;
        }
        let limit = self
            .config
            .page_cache_slots
            .map_or(256 * 1024, |slots| {
                slots.saturating_mul(crate::storage::engine::PAGE_SIZE) / 32
            })
            .clamp(16 * 1024, 8 * 1024 * 1024);
        let mut retained = self.checkpoint_durable_rows.write();
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
                _ => {}
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
        if Arc::strong_count(tree) != 1 || tree.has_retirement_readers() {
            return Ok(None);
        }
        if previous.untracked_visibility_revision != current.untracked_visibility_revision {
            return Ok(None);
        }
        let Some(deferred) = previous.deferred_rows.as_ref() else {
            return Ok(None);
        };
        let Some(ids) = manager.checkpoint_mutations_since(&previous.segments) else {
            return Ok(None);
        };
        let mut ids: std::collections::BTreeSet<_> = ids.into_iter().collect();
        ids.extend(deferred.iter().copied());
        ids.extend(
            previous
                .pending_vault
                .symmetric_difference(&current.pending_vault)
                .copied(),
        );
        if ids.len() > 1024 {
            return Ok(None);
        }
        let retained = self.checkpoint_durable_rows.read();
        let mut upserts = Vec::new();
        let mut deferred = Vec::new();
        for id in ids {
            let key = id.raw().to_be_bytes().to_vec();
            let existing = tree
                .get(&key)
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
            let metadata = manager.get_metadata(id);
            let mut defer = false;
            let row = manager.get_with(id, |entity| {
                if current.pending_vault.contains(&id)
                    || (matches!(entity.data, EntityData::Row(_) | EntityData::Vector(_))
                        && current.excluded_xids.contains(&entity.xmin))
                {
                    defer = true;
                    return None;
                }
                let raw =
                    Self::serialize_entity_record(entity, metadata.as_ref(), self.format_version());
                let projected = if matches!(entity.data, EntityData::Row(_))
                    && current.excluded_xids.contains(&entity.xmax)
                {
                    defer = true;
                    let mut normalized = entity.clone();
                    normalized.xmax = 0;
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
                    return Ok(None);
                }
                continue;
            };
            let Some((raw, projected)) = row else {
                if existing.is_some() {
                    return Ok(None);
                }
                continue;
            };
            if existing.as_ref() == Some(projected.as_ref().unwrap_or(&raw)) {
                continue;
            }
            if retained.rows.get(&(collection.to_owned(), id)) != Some(&raw) {
                return Ok(None);
            }
            upserts.push((key, projected.unwrap_or(raw)));
        }
        drop(retained);
        if manager.checkpoint_revisions() != current.segments {
            return Ok(None);
        }
        current.deferred_rows = Some(Arc::new(deferred));
        // All eligibility checks finish before changing the published tree.
        // These are durable logical WAL updates, just like an ordinary inline
        // write; unlogged manager edits still build an unpublished replacement.
        #[cfg(debug_assertions)]
        let root_before = tree.root_page_id();
        tree.upsert_batch_sorted(&upserts)
            .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))?;
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
        Ok(Some(Arc::clone(tree)))
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
}
