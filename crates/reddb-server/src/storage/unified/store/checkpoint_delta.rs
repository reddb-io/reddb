//! Bounded checkpoint projection of rows whose exact image is already in the WAL.
use super::commit::StoreWalAction;
use super::*;

enum SparseCheckpointValue {
    Cached,
    Wal(super::checkpoint_wal::WalRowReference),
    Projected(Vec<u8>),
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
        let limit = self
            .config
            .page_cache_slots
            .map_or(256 * 1024, |slots| {
                slots.saturating_mul(crate::storage::engine::PAGE_SIZE) / 32
            })
            .clamp(16 * 1024, 8 * 1024 * 1024);
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
        if ids.len() > crate::storage::unified::segment::CHECKPOINT_MUTATIONS_MAX {
            return Ok(None);
        }
        let retained = self.checkpoint_durable_rows.read();
        let mut upserts = Vec::new();
        let mut projected_bytes = 0_usize;
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
            let source = if retained.rows.get(&(collection.to_owned(), id)) == Some(&raw) {
                SparseCheckpointValue::Cached
            } else if let (Some(commit), Some(reference)) =
                (&self.commit, retained.references.get(collection, id))
            {
                if !commit
                    .checkpoint_row_matches(reference, &raw)
                    .map_err(StoreError::Io)?
                {
                    return Ok(None);
                }
                SparseCheckpointValue::Wal(reference)
            } else {
                return Ok(None);
            };
            let source = if let Some(projected) = projected {
                projected_bytes = projected_bytes.saturating_add(projected.capacity());
                if projected_bytes > 8 * 1024 * 1024 {
                    return Ok(None);
                }
                SparseCheckpointValue::Projected(projected)
            } else {
                source
            };
            upserts.push((id, source));
        }
        if manager.checkpoint_revisions() != current.segments {
            return Ok(None);
        }
        current.deferred_rows = Some(Arc::new(deferred));
        // Validate every source before changing the published tree. Afterwards
        // materialize only bounded batches from those exact durable images;
        // do not reread potentially changed manager payloads in a second pass.
        #[cfg(debug_assertions)]
        let root_before = tree.root_page_id();
        let protected = self.ownership_active_epoch.load(Ordering::Acquire) != 0;
        let mut tree = Arc::clone(tree);
        let mut copied = false;
        let mut apply = |items: &[(Vec<u8>, Vec<u8>)]| -> Result<(), StoreError> {
            if protected && !copied {
                // Inline values can be upserted without changing page
                // ownership or tree links when the leaves have room. Every
                // image was checked against the durable WAL above, so replay
                // can finish a partially applied batch after a crash.
                if tree
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
                    return Ok(());
                }
                // Structural changes must leave the published generation
                // untouched until the catalog points at their replacement.
                tree = self.copy_checkpoint_tree(&tree)?;
                copied = true;
            }
            tree.upsert_batch_sorted(items)
                .map_err(|error| StoreError::Io(std::io::Error::other(error.to_string())))
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
