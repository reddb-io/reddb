//! Disk-backed collection state. Publication remains a host-owned write fence.

use super::*;
use crate::storage::collection_read::{
    CollectionCursor, CollectionRead, FrozenOverlay, MergedCollection,
};
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

/// Frozen base/overlay cursor for a lazy query source. The source generation
/// remains pinned through the owned page cursor, not a manager topology lock.
pub struct PagedEntityCursor {
    cursor: Box<dyn CollectionCursor>,
    format_version: u32,
    read_bytes_max: usize,
    snapshot: Option<SnapshotContext>,
    visible_only: bool,
    schema: Option<Arc<Vec<String>>>,
    key: Option<String>,
    finished: bool,
}

impl Iterator for PagedEntityCursor {
    type Item = Result<(UnifiedEntity, Option<Metadata>), StoreError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        loop {
            let record = match self.cursor.next_record(self.read_bytes_max) {
                Ok(Some(record)) => record,
                Ok(None) => {
                    self.finished = true;
                    return None;
                }
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            };
            let (mut entity, metadata) =
                match record.decode_with_budget(self.format_version, self.read_bytes_max) {
                    Ok(record) => record,
                    Err(error) => {
                        self.finished = true;
                        return Some(Err(error));
                    }
                };
            if self.visible_only
                && !SegmentManager::scan_entity_visible(self.snapshot.as_ref(), &entity)
            {
                continue;
            }
            if let super::super::entity::EntityData::Row(row) = &mut entity.data {
                if row.named.is_none() && row.schema.is_none() {
                    row.schema = self.schema.clone();
                }
            }
            if self.key.as_deref().is_some_and(|key| {
                crate::storage::segment_key_lookup::SegmentKeyLookup::key(&entity) != Some(key)
            }) {
                continue;
            }
            return Some(Ok((entity, metadata)));
        }
    }
}

impl SegmentManager {
    /// The host must fence publication while capturing the base and overlay.
    /// This retains O(dirty overlay) state and decodes at most one base row.
    pub fn try_paged_entity_cursor(
        &self,
        snapshot: Option<SnapshotContext>,
    ) -> Result<Option<PagedEntityCursor>, StoreError> {
        self.try_paged_cursor(snapshot, true, None)
    }

    /// Raw physical records and their frozen metadata, including superseded
    /// versions. Hosts fence publication while capturing this cursor.
    pub fn try_paged_physical_cursor(&self) -> Result<Option<PagedEntityCursor>, StoreError> {
        self.try_paged_cursor(None, false, None)
    }

    /// Freeze only this key's payloads and metadata, retaining other dirty IDs
    /// as masks. Every decoded candidate is checked against the logical key.
    pub fn try_paged_key_entity_cursor(
        &self,
        key: &str,
        snapshot: Option<SnapshotContext>,
    ) -> Result<Option<PagedEntityCursor>, StoreError> {
        self.try_paged_cursor(snapshot, true, Some(key))
    }

    /// Like the visible keyed cursor, but includes superseded physical history.
    pub fn try_paged_key_physical_cursor(
        &self,
        key: &str,
    ) -> Result<Option<PagedEntityCursor>, StoreError> {
        self.try_paged_cursor(None, false, Some(key))
    }

    fn try_paged_cursor(
        &self,
        snapshot: Option<SnapshotContext>,
        visible_only: bool,
        key: Option<&str>,
    ) -> Result<Option<PagedEntityCursor>, StoreError> {
        let Some((format_version, read_bytes_max)) = self
            .backing
            .read()
            .as_ref()
            .map(|base| (base.format_version, base.read_bytes_max))
        else {
            return Ok(None);
        };
        let view = match key {
            Some(key) => self.current_backing_key_overlay(key)?,
            None => self.current_backing_overlay(),
        };
        let Some((base, overlay)) = view else {
            return Ok(None);
        };
        let merged = MergedCollection::new(base.as_ref(), overlay);
        let cursor = match key {
            Some(key) => match merged.owned_key_cursor(key)? {
                Some(cursor) => cursor,
                None => merged.owned_cursor()?,
            },
            None => merged.owned_cursor()?,
        };
        Ok(Some(PagedEntityCursor {
            cursor,
            format_version,
            read_bytes_max,
            snapshot,
            visible_only,
            schema: self.column_schema(),
            key: key.map(str::to_owned),
            finished: false,
        }))
    }

    pub fn has_backing(&self) -> bool {
        self.backing.read().is_some()
    }

    /// Identity of the immutable base currently installed by the host.
    pub fn backing_root_page(&self) -> Option<u32> {
        self.backing.read().as_ref().map(|base| base.root_page)
    }

    /// The host applies the same row allowance to model-specific expansion
    /// such as decrypting a compressed Vault value after native record decode.
    pub fn backing_read_bytes_max(&self) -> Option<usize> {
        self.backing
            .read()
            .as_ref()
            .map(|backing| backing.read_bytes_max)
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
            self.restore_sequence_id_next(source.read().sequence_id_next());
            retired.push(Arc::downgrade(&source));
        }
        for source in sealed.drain(..) {
            self.restore_sequence_id_next(source.read().sequence_id_next());
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

    /// Whether a projected base posting has been replaced or physically
    /// removed. The host must fence publication/mutations while merging the
    /// projected base with resident rows. This never reads a base payload.
    pub fn backing_row_is_shadowed(&self, id: EntityId) -> bool {
        let backing = self.backing.read();
        backing
            .as_ref()
            .is_none_or(|base| base.removed.contains(&id))
            || self.get_with(id, |_| ()).is_some()
    }

    /// Preserve I/O and decode failures instead of treating them as absent rows.
    pub fn try_get(
        &self,
        id: EntityId,
        limit_bytes: usize,
    ) -> Result<Option<UnifiedEntity>, StoreError> {
        let backing = self.backing.read();
        let limit_bytes = backing
            .as_ref()
            .map_or(limit_bytes, |base| limit_bytes.min(base.read_bytes_max));
        if let Some(result) = self.get_with(id, |entity| {
            let required_bytes = if limit_bytes == usize::MAX {
                0
            } else {
                crate::storage::memory_size::entity_bytes(entity)
            };
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

    /// Fallible keyed projection. Resident mode keeps the segment key indexes.
    /// Prepared bases seek physical candidates through their paired key index;
    /// unindexed legacy sources retain a one-row streaming fallback. Hosts must
    /// fence publication, and callbacks must not mutate or re-enter this collection.
    pub fn try_project_key_entity<T>(
        &self,
        key: &str,
        snapshot: Option<&SnapshotContext>,
        versioned: bool,
        limit_bytes: usize,
        mut project: impl FnMut(&UnifiedEntity) -> T,
    ) -> Result<Option<T>, StoreError> {
        let backing = self.backing.read();
        let limit_bytes = backing
            .as_ref()
            .map_or(limit_bytes, |base| limit_bytes.min(base.read_bytes_max));
        let mut best = self
            .project_key_entity(key, snapshot, versioned, |entity| {
                let required_bytes = if limit_bytes == usize::MAX {
                    0
                } else {
                    crate::storage::memory_size::entity_bytes(entity)
                };
                if required_bytes > limit_bytes {
                    Err(StoreError::ReadLimitExceeded {
                        required_bytes,
                        limit_bytes,
                    })
                } else {
                    Ok((entity.xmin, project(entity)))
                }
            })
            .transpose()?;
        let Some(backing) = backing.as_ref() else {
            return Ok(best.map(|(_, projected)| projected));
        };
        if !versioned && best.is_some() {
            return Ok(best.map(|(_, projected)| projected));
        }
        let mut cursor = match backing.source.key_cursor(key)? {
            Some(cursor) => cursor,
            None => backing.source.cursor()?,
        };
        while let Some(id) = cursor.peek_id()? {
            // A physical replacement or deletion masks the old base value,
            // even when it no longer matches this logical key.
            if backing.removed.contains(&id) || self.get_with(id, |_| ()).is_some() {
                cursor.skip_record()?;
                continue;
            }
            let record = cursor
                .next_record(limit_bytes)?
                .expect("invariant: peeked physical row exists");
            let (mut entity, _) = record.decode_with_budget(backing.format_version, limit_bytes)?;
            self.restore_owned_row_schemas(std::iter::once(&mut entity));
            if crate::storage::segment_key_lookup::SegmentKeyLookup::key(&entity) != Some(key)
                || (versioned && !Self::scan_entity_visible(snapshot, &entity))
            {
                continue;
            }
            if best.as_ref().is_none_or(|(xmin, _)| entity.xmin >= *xmin) {
                best = Some((entity.xmin, project(&entity)));
            }
            if !versioned {
                break;
            }
        }
        Ok(best.map(|(_, projected)| projected))
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

    /// Physical key history. Indexed bases avoid unrelated payload reads;
    /// resident and unindexed sources still verify each logical key.
    pub fn try_for_each_key_entity(
        &self,
        key: &str,
        mut callback: impl FnMut(&UnifiedEntity) -> bool,
    ) -> Result<(), StoreError> {
        if let Some(cursor) = self.try_paged_key_physical_cursor(key)? {
            for record in cursor {
                let (entity, _) = record?;
                if !callback(&entity) {
                    break;
                }
            }
            return Ok(());
        }
        self.try_for_each_entity(usize::MAX, |entity| {
            crate::storage::segment_key_lookup::SegmentKeyLookup::key(entity) != Some(key)
                || callback(entity)
        })
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

    /// Preserve the resident projection fast path; cold mode streams candidates.
    /// Hosts own admission for retained projections independently of row decoding.
    pub fn try_scan_project_zoned_with_stats<T, F>(
        &self,
        snapshot: Option<&SnapshotContext>,
        zone_preds: &[(&str, ZoneColPred<'_>)],
        read_bytes_max: usize,
        project: F,
    ) -> Result<(Vec<T>, SegmentScanStats), StoreError>
    where
        T: Send,
        F: Fn(&UnifiedEntity) -> Option<T> + Sync,
    {
        if !self.has_backing() {
            return Ok(self.scan_project_zoned_with_stats(snapshot, zone_preds, project));
        }
        let mut projected = Vec::new();
        let stats = self.try_scan_for_each_zoned_with_stats(
            snapshot,
            zone_preds,
            read_bytes_max,
            |entity| {
                if let Some(value) = project(entity) {
                    projected.push(value);
                }
                true
            },
        )?;
        Ok((projected, stats))
    }

    /// Point-read allowance applies to each row. Hosts separately admit the
    /// retained output vector; this method is not a global result quota.
    pub fn try_get_many(
        &self,
        ids: &[EntityId],
        read_bytes_max: usize,
    ) -> Result<Vec<Option<UnifiedEntity>>, StoreError> {
        if !self.has_backing() && read_bytes_max == usize::MAX {
            return Ok(self.get_many(ids));
        }
        ids.iter()
            .map(|id| self.try_get(*id, read_bytes_max))
            .collect()
    }

    pub fn try_for_each_id(
        &self,
        ids: &[EntityId],
        read_bytes_max: usize,
        mut visit: impl FnMut(usize, &UnifiedEntity) -> bool,
    ) -> Result<(), StoreError> {
        if self.has_backing() {
            for (position, id) in ids.iter().enumerate() {
                if let Some(entity) = self.try_get(*id, read_bytes_max)? {
                    if !visit(position, &entity) {
                        break;
                    }
                }
            }
            return Ok(());
        }
        let mut stopped = false;
        let mut failure = None;
        self.for_each_id(ids, |position, entity| {
            if stopped {
                return;
            }
            let required_bytes = if read_bytes_max == usize::MAX {
                0
            } else {
                crate::storage::memory_size::entity_bytes(entity)
            };
            if required_bytes > read_bytes_max {
                failure = Some(StoreError::ReadLimitExceeded {
                    required_bytes,
                    limit_bytes: read_bytes_max,
                });
                stopped = true;
            } else {
                stopped = !visit(position, entity);
            }
        });
        failure.map_or(Ok(()), Err)
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

    fn current_backing_key_overlay(
        &self,
        key: &str,
    ) -> Result<Option<(Arc<dyn CollectionRead + Send + Sync>, Arc<FrozenOverlay>)>, StoreError>
    {
        let backing = self.backing.read();
        let Some(backing) = backing.as_ref() else {
            return Ok(None);
        };
        let mut overlay: FrozenOverlay = backing.removed.iter().map(|id| (*id, None)).collect();
        let schema = self.column_schema();
        let key_column = schema
            .as_ref()
            .and_then(|schema| schema.iter().position(|name| name == "key"));
        let mut failure = None;
        self.for_each_entity_source(|segment, entity| {
            let key_matches = match (&entity.data, key_column) {
                (crate::storage::entity::EntityData::Row(row), Some(index)) if row.named.is_none() && row.schema.is_none() => {
                    matches!(row.columns.get(index), Some(reddb_types::Value::Text(stored)) if stored.as_ref() == key)
                }
                _ => crate::storage::segment_key_lookup::SegmentKeyLookup::key(entity) == Some(key),
            };
            let bytes = if key_matches {
                let required_bytes = crate::storage::memory_size::entity_bytes(entity);
                if required_bytes > backing.read_bytes_max {
                    failure = Some(StoreError::ReadLimitExceeded {
                        required_bytes,
                        limit_bytes: backing.read_bytes_max,
                    });
                    return false;
                }
                let metadata = segment.get_metadata(entity.id);
                Some(Arc::from(serialize_entity_record(entity, metadata.as_ref(), backing.format_version)))
            } else {
                // A row renamed away from the requested key must still hide
                // the old base candidate. No unrelated payload/metadata copy.
                None
            };
            overlay.insert(entity.id, bytes);
            true
        });
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(Some((Arc::clone(&backing.source), Arc::new(overlay))))
    }
}
