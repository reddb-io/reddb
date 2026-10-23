//! Exact checkpoint proofs backed by the existing durable WAL, without retaining payloads.
use super::*;

pub(super) const ROW_REFERENCES_MAX: usize = 8192;
const REFERENCE_BATCH_BYTES_MAX: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct WalRowReference {
    pub(super) generation: u64,
    pub(super) offset: u64,
    pub(super) length: u32,
    pub(super) checksum: u32,
}

pub(super) struct CheckpointWalRow {
    collection: String,
    id: EntityId,
    reference: WalRowReference,
}

#[derive(Default)]
pub(super) struct CheckpointWalBatch {
    rows: Vec<CheckpointWalRow>,
    bytes: usize,
    incomplete: bool,
}

impl CheckpointWalBatch {
    pub(super) fn encode(
        tx_id: u64,
        actions: &[Vec<u8>],
        generation: u64,
    ) -> std::io::Result<(Vec<u8>, Self)> {
        let mut batch = Self::default();
        let bytes = reddb_file::encode_main_wal_commit_batch_with_visitor(
            tx_id,
            actions,
            crate::storage::wal::record::WAL_DEFAULT_TERM,
            |action_offset, action| {
                reddb_file::visit_store_wal_entity_records(action, |collection, offset, record| {
                    if batch.incomplete {
                        return Ok(());
                    }
                    let frame =
                        reddb_file::decode_native_entity_record_frame(record).map_err(|error| {
                            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
                        })?;
                    let entity = frame.map_or(record, |frame| frame.entity);
                    let mut position = 0;
                    let id = read_varu64(entity, &mut position).map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid checkpoint WAL entity ID",
                        )
                    })?;
                    if entity.get(position) != Some(&0) {
                        return Ok(());
                    }
                    let bytes = collection.len().saturating_add(192);
                    if batch.rows.len() == ROW_REFERENCES_MAX
                        || batch.bytes.saturating_add(bytes) > REFERENCE_BATCH_BYTES_MAX
                    {
                        batch.rows.clear();
                        batch.incomplete = true;
                        return Ok(());
                    }
                    batch.rows.push(CheckpointWalRow {
                        collection: collection.to_owned(),
                        id: EntityId::new(id),
                        reference: WalRowReference {
                            generation,
                            offset: action_offset + offset as u64,
                            length: u32::try_from(record.len()).map_err(|_| {
                                std::io::Error::new(
                                    std::io::ErrorKind::InvalidInput,
                                    "WAL row length exceeds u32",
                                )
                            })?,
                            checksum: crc32fast::hash(record),
                        },
                    });
                    batch.bytes += bytes;
                    Ok(())
                })
            },
        )?;
        Ok((bytes, batch))
    }

    pub(super) fn relocate(&mut self, start_lsn: u64) -> std::io::Result<()> {
        for row in &mut self.rows {
            row.reference.offset =
                row.reference.offset.checked_add(start_lsn).ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidInput, "WAL row offset overflow")
                })?;
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct WalRowReferences {
    rows: HashMap<(String, EntityId), WalRowReference>,
    bytes: usize,
}

impl WalRowReferences {
    pub(super) fn memory_bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn get(&self, collection: &str, id: EntityId) -> Option<WalRowReference> {
        self.rows.get(&(collection.to_owned(), id)).copied()
    }

    pub(super) fn remember(&mut self, batch: CheckpointWalBatch, limit: usize) {
        if batch.incomplete || self.bytes > limit {
            *self = Self::default();
            return;
        }
        for row in batch.rows {
            let key = (row.collection, row.id);
            let cost = key.0.len().saturating_add(192);
            if cost > limit {
                self.rows.remove(&key);
                self.recount();
                continue;
            }
            if !self.rows.contains_key(&key) && self.bytes.saturating_add(cost) > limit {
                self.rows.retain(|(name, _), _| name != &key.0);
                self.rows.shrink_to_fit();
                self.recount();
                if self.bytes.saturating_add(cost) > limit {
                    *self = Self::default();
                }
            }
            if self.rows.insert(key, row.reference).is_none() {
                self.bytes += cost;
            }
        }
    }

    fn recount(&mut self) {
        self.bytes = self
            .rows
            .keys()
            .map(|(name, _)| name.len().saturating_add(192))
            .sum();
    }
}

#[cfg(test)]
mod tests {
    use super::super::commit::{StoreCommitCoordinator, StoreWalAction};
    use super::*;

    fn action() -> (StoreWalAction, Vec<u8>) {
        let mut entity = UnifiedEntity::table_row(EntityId::new(7), "rows", 0, vec![]);
        entity.data = EntityData::Row(RowData::with_names(vec![Value::Integer(19)], vec!["n".into()]));
        let record = UnifiedStore::serialize_entity_record(&entity, None, STORE_VERSION_V11);
        (
            StoreWalAction::UpsertEntityRecord {
                collection: "rows".into(),
                record: record.clone(),
            },
            record,
        )
    }

    #[test]
    fn reference_matches_committed_bytes_in_strict_and_grouped_modes() {
        for mode in [DurabilityMode::Strict, DurabilityMode::WalDurableGrouped] {
            let directory = tempfile::tempdir().unwrap();
            let coordinator = StoreCommitCoordinator::open(
                directory.path().join("rows.wal"),
                mode,
                crate::api::GroupCommitOptions::default(),
            )
            .unwrap();
            let (action, record) = action();
            let batch = coordinator.append_checkpoint_actions(&[action]).unwrap();
            assert_eq!(batch.rows.len(), 1);
            let reference = batch.rows[0].reference;
            assert!(coordinator
                .checkpoint_row_matches(reference, &record)
                .unwrap());
            let mut changed = record.clone();
            *changed.last_mut().unwrap() ^= 1;
            assert!(!coordinator
                .checkpoint_row_matches(reference, &changed)
                .unwrap());
        }
    }

    #[test]
    fn truncation_invalidates_proofs_even_when_identical_bytes_reuse_the_lsn() {
        let directory = tempfile::tempdir().unwrap();
        let coordinator = StoreCommitCoordinator::open(
            directory.path().join("rows.wal"),
            DurabilityMode::Strict,
            crate::api::GroupCommitOptions::default(),
        )
        .unwrap();
        let (action, record) = action();
        let old = coordinator
            .append_checkpoint_actions(std::slice::from_ref(&action))
            .unwrap();
        coordinator.truncate().unwrap();
        let new = coordinator.append_checkpoint_actions(&[action]).unwrap();
        let old = old.rows[0].reference;
        let new = new.rows[0].reference;
        assert_eq!(old.offset, new.offset);
        assert!(!coordinator.checkpoint_row_matches(old, &record).unwrap());
        assert!(coordinator.checkpoint_row_matches(new, &record).unwrap());
    }

    #[test]
    fn corrupt_proof_bytes_fail_closed() {
        use std::io::{Seek, SeekFrom, Write};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rows.wal");
        let coordinator = StoreCommitCoordinator::open(
            &path,
            DurabilityMode::Strict,
            crate::api::GroupCommitOptions::default(),
        )
        .unwrap();
        let (action, record) = action();
        let batch = coordinator.append_checkpoint_actions(&[action]).unwrap();
        let reference = batch.rows[0].reference;
        let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.seek(SeekFrom::Start(reference.offset)).unwrap();
        file.write_all(&[record[0] ^ 1]).unwrap();
        file.sync_all().unwrap();
        let error = coordinator
            .checkpoint_row_matches(reference, &record)
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }
}
