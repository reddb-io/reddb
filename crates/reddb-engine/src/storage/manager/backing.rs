//! Disk-backed collection state. Publication remains a host-owned write fence.

use super::*;
use crate::storage::collection_read::{CollectionRead, FrozenOverlay};
use crate::storage::error::StoreError;
use crate::storage::metadata_codec::serialize_entity_record;
use std::collections::HashSet;

pub(super) struct BackingRead {
    pub source: Arc<dyn CollectionRead + Send + Sync>,
    pub root_page: u32,
    pub total_entities: u64,
    pub format_version: u32,
    pub read_bytes_max: usize,
    pub removed: HashSet<EntityId>,
}

impl SegmentManager {
    pub fn has_backing(&self) -> bool {
        self.backing.read().is_some()
    }

    /// The host must fence writers and publish the new durable tree first.
    /// Captured resident readers retain their old sources through weak tracking.
    pub fn install_backing(
        &self,
        source: Arc<dyn CollectionRead + Send + Sync>,
        root_page: u32,
        format_version: u32,
        read_bytes_max: usize,
        total_entities: u64,
    ) {
        let mut backing = self.backing.write();
        let mut consolidation = self.consolidation.write();
        let mut growing = self.growing.write();
        let mut sealed = self.sealed.write();
        let mut retired = self.retired.write();
        if let Some(source) = growing.take() {
            retired.push(Arc::downgrade(&source));
        }
        for source in sealed.drain(..) {
            retired.push(Arc::downgrade(&source));
        }
        *consolidation = None;
        self.entity_segment.write().clear();
        self.total_entities_atomic
            .store(total_entities, Ordering::Relaxed);
        *backing = Some(BackingRead {
            source,
            root_page,
            total_entities,
            format_version,
            read_bytes_max,
            removed: HashSet::new(),
        });
    }

    pub fn backing_count_at_root(&self, root_page: u32) -> Option<u64> {
        self.backing
            .read()
            .as_ref()
            .filter(|base| base.root_page == root_page)
            .map(|base| base.total_entities)
    }

    /// Preserve I/O and decode failures instead of treating them as absent rows.
    pub fn try_get(
        &self,
        id: EntityId,
        limit_bytes: usize,
    ) -> Result<Option<UnifiedEntity>, StoreError> {
        let backing = self.backing.read();
        if let Some(result) = self.get_with(id, |entity| {
            let required_bytes = crate::storage::memory_size::entity_bytes(entity);
            if required_bytes > limit_bytes {
                Err(StoreError::ReadLimitExceeded {
                    required_bytes,
                    limit_bytes,
                })
            } else {
                Ok(entity.clone())
            }
        }) {
            let mut entity = result?;
            self.restore_owned_row_schemas(std::iter::once(&mut entity));
            return Ok(Some(entity));
        }
        let Some(backing) = backing.as_ref() else {
            return Ok(None);
        };
        if backing.removed.contains(&id) {
            return Ok(None);
        }
        let Some(record) = backing
            .source
            .get_record(id, limit_bytes.min(backing.read_bytes_max))?
        else {
            return Ok(None);
        };
        let (mut entity, _) = record.decode_with_budget(
            backing.format_version,
            limit_bytes.min(backing.read_bytes_max),
        )?;
        self.restore_owned_row_schemas(std::iter::once(&mut entity));
        Ok(Some(entity))
    }

    /// Visit the merged physical view in ID order without hydrating the base.
    /// Only overlay identities and one decoded base record are retained. Like
    /// resident visitors, callbacks must not mutate or re-enter this collection.
    pub fn try_for_each_entity(
        &self,
        limit_bytes: usize,
        mut callback: impl FnMut(&UnifiedEntity) -> bool,
    ) -> Result<(), StoreError> {
        let backing_guard = self.backing.read();
        let Some(backing) = backing_guard.as_ref() else {
            self.for_each_entity(callback);
            return Ok(());
        };
        let limit_bytes = limit_bytes.min(backing.read_bytes_max);
        let mut identities = Vec::new();
        self.for_each_entity(|entity| {
            identities.push(entity.id);
            true
        });
        identities.sort_unstable();
        identities.dedup();
        let mut identities = identities.into_iter().peekable();
        let mut cursor = backing.source.cursor()?;
        loop {
            let base = cursor.peek_id()?;
            let resident = identities.peek().copied();
            if let Some(id) = resident.filter(|id| base.is_none_or(|base| *id <= base)) {
                if base == Some(id) {
                    cursor.skip_record()?;
                }
                identities.next();
                let result = self.get_with(id, |entity| {
                    let required_bytes = crate::storage::memory_size::entity_bytes(entity);
                    if required_bytes > limit_bytes {
                        return Err(StoreError::ReadLimitExceeded {
                            required_bytes,
                            limit_bytes,
                        });
                    }
                    Ok(callback(entity))
                });
                if result.transpose()? == Some(false) {
                    return Ok(());
                }
                continue;
            }
            let Some(id) = base else {
                return Ok(());
            };
            if backing.removed.contains(&id) {
                cursor.skip_record()?;
                continue;
            }
            let record = cursor
                .next_record(limit_bytes)?
                .expect("invariant: peeked physical row exists");
            let (mut entity, _) = record.decode_with_budget(
                backing.format_version,
                limit_bytes.min(backing.read_bytes_max),
            )?;
            self.restore_owned_row_schemas(std::iter::once(&mut entity));
            if !callback(&entity) {
                return Ok(());
            }
        }
    }

    pub fn try_scan_for_each(
        &self,
        snapshot: Option<&SnapshotContext>,
        limit_bytes: usize,
        mut callback: impl FnMut(&UnifiedEntity) -> bool,
    ) -> Result<(), StoreError> {
        self.try_for_each_entity(limit_bytes, |entity| {
            !Self::scan_entity_visible(snapshot, entity) || callback(entity)
        })
    }

    /// The disk base has no segment zone summaries yet: scan all candidates
    /// and retain the caller's residual predicate. Resident mode keeps pruning.
    pub fn try_scan_for_each_zoned_with_stats(
        &self,
        snapshot: Option<&SnapshotContext>,
        zone_preds: &[(&str, ZoneColPred<'_>)],
        limit_bytes: usize,
        callback: impl FnMut(&UnifiedEntity) -> bool,
    ) -> Result<SegmentScanStats, StoreError> {
        if !self.has_backing() {
            return Ok(self.scan_for_each_zoned_with_stats(snapshot, zone_preds, callback));
        }
        self.try_scan_for_each(snapshot, limit_bytes, callback)?;
        Ok(SegmentScanStats {
            segments_total: 1,
            segments_scanned: 1,
            segments_pruned: 0,
        })
    }

    /// Inserts must not overwrite a physical identity in the published base.
    pub(super) fn reject_backing_duplicate(&self, id: EntityId) -> Result<(), SegmentError> {
        let backing = self.backing.read();
        let Some(backing) = backing.as_ref() else {
            return Ok(());
        };
        if !backing.removed.contains(&id)
            && backing
                .source
                .get_record(id, backing.read_bytes_max)
                .map_err(|error| SegmentError::BackingRead(error.to_string()))?
                .is_some()
        {
            return Err(SegmentError::AlreadyExists(id));
        }
        Ok(())
    }

    /// Metadata reads carry the same I/O failure contract as entity reads.
    pub fn try_get_metadata(
        &self,
        id: EntityId,
        limit_bytes: usize,
    ) -> Result<Option<Metadata>, StoreError> {
        let backing = self.backing.read();
        if self.get_with(id, |_| ()).is_some() {
            return Ok(self.get_metadata(id));
        }
        let Some(backing) = backing.as_ref() else {
            return Ok(None);
        };
        if backing.removed.contains(&id) {
            return Ok(None);
        }
        backing
            .source
            .get_record(id, limit_bytes.min(backing.read_bytes_max))?
            .map(|record| {
                record
                    .decode_with_budget(
                        backing.format_version,
                        limit_bytes.min(backing.read_bytes_max),
                    )
                    .map(|(_, metadata)| metadata)
            })
            .transpose()
            .map(Option::flatten)
    }

    pub(super) fn materialize_backing_entity(&self, id: EntityId) -> Result<(), SegmentError> {
        if self.get_with(id, |_| ()).is_some() {
            return Ok(());
        }
        let record = {
            let backing = self.backing.read();
            let Some(backing) = backing.as_ref() else {
                return Ok(());
            };
            if backing.removed.contains(&id) {
                return Ok(());
            }
            backing
                .source
                .get_record(id, backing.read_bytes_max)
                .and_then(|record| {
                    record
                        .map(|record| {
                            record
                                .decode_with_budget(backing.format_version, backing.read_bytes_max)
                        })
                        .transpose()
                })
                .map_err(|error| SegmentError::BackingRead(error.to_string()))?
        };
        let Some((mut entity, metadata)) = record else {
            return Ok(());
        };
        self.restore_owned_row_schemas(std::iter::once(&mut entity));
        self.restore_snapshot_entity(entity)?;
        // This row was already included in the persisted cardinality.
        self.total_entities_atomic.fetch_sub(1, Ordering::Relaxed);
        if let Some(metadata) = metadata {
            self.set_metadata(id, metadata)?;
        }
        Ok(())
    }

    pub(super) fn remember_backing_delete(&self, id: EntityId) {
        if let Some(backing) = self.backing.write().as_mut() {
            backing.removed.insert(id);
        }
    }

    pub fn backing_overlay(
        &self,
        mut encode: impl FnMut(&UnifiedEntity, Option<Metadata>) -> Option<Vec<u8>>,
    ) -> Option<(Arc<dyn CollectionRead + Send + Sync>, Arc<FrozenOverlay>)> {
        let backing = self.backing.read();
        let backing = backing.as_ref()?;
        let mut overlay: FrozenOverlay = backing.removed.iter().map(|id| (*id, None)).collect();
        self.for_each_entity_with_metadata(|entity, metadata| {
            if let Some(bytes) = encode(entity, metadata) {
                overlay.insert(entity.id, Some(Arc::from(bytes)));
            }
            true
        });
        Some((Arc::clone(&backing.source), Arc::new(overlay)))
    }

    pub fn current_backing_overlay(
        &self,
    ) -> Option<(Arc<dyn CollectionRead + Send + Sync>, Arc<FrozenOverlay>)> {
        let version = self.backing.read().as_ref()?.format_version;
        self.backing_overlay(|entity, metadata| {
            Some(serialize_entity_record(entity, metadata.as_ref(), version))
        })
    }
}
