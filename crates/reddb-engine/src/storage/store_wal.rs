//! Canonical logical store actions; physical WAL framing belongs to reddb-file.
use super::entity::UnifiedEntity;
use super::metadata::Metadata;
use std::io;

#[derive(Debug, Clone)]
pub enum StoreWalAction {
    CreateCollection {
        name: String,
    },
    DropCollection {
        name: String,
    },
    UpsertEntityRecord {
        collection: String,
        record: Vec<u8>,
    },
    DeleteEntityRecord {
        collection: String,
        entity_id: u64,
    },
    /// Batched upsert — one WAL action carrying N serialized entity
    /// records for the same collection. Saves the per-row Begin/
    /// PageWrite/Commit framing overhead on the bulk insert hot path.
    /// Replay applies every contained record in order.
    BulkUpsertEntityRecords {
        collection: String,
        records: Vec<Vec<u8>>,
    },
    /// Atomic full-collection replace — issue #595 slice 9c.
    /// Replay drops the in-memory collection state and rebuilds it
    /// from the contained records. Used by REFRESH MATERIALIZED VIEW
    /// so a concurrent reader sees either the prior contents or the
    /// new contents, never a partial state, and a crash mid-refresh
    /// leaves the prior contents intact on recovery (the action is
    /// only durable once the WAL commit lands).
    RefreshCollection {
        collection: String,
        records: Vec<Vec<u8>>,
    },
    /// Single-file collection contracts must survive acknowledged DDL even
    /// when no snapshot checkpoint has run before process loss.
    SetAuxMetadata {
        bytes: Vec<u8>,
    },
}

impl StoreWalAction {
    pub fn upsert_entity(
        collection: &str,
        entity: &UnifiedEntity,
        metadata: Option<&Metadata>,
        format_version: u32,
    ) -> Self {
        Self::UpsertEntityRecord {
            collection: collection.to_string(),
            record: super::metadata_codec::serialize_entity_record(
                entity,
                metadata,
                format_version,
            ),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        reddb_file::encode_store_wal_action_frame(&self.to_frame())
            .expect("encode store wal action frame")
    }

    fn to_frame(&self) -> reddb_file::StoreWalActionFrame {
        match self {
            Self::CreateCollection { name } => {
                reddb_file::StoreWalActionFrame::CreateCollection { name: name.clone() }
            }
            Self::DropCollection { name } => {
                reddb_file::StoreWalActionFrame::DropCollection { name: name.clone() }
            }
            Self::UpsertEntityRecord { collection, record } => {
                reddb_file::StoreWalActionFrame::UpsertEntityRecord {
                    collection: collection.clone(),
                    record: record.clone(),
                }
            }
            Self::DeleteEntityRecord {
                collection,
                entity_id,
            } => reddb_file::StoreWalActionFrame::DeleteEntityRecord {
                collection: collection.clone(),
                entity_id: *entity_id,
            },
            Self::BulkUpsertEntityRecords {
                collection,
                records,
            } => reddb_file::StoreWalActionFrame::BulkUpsertEntityRecords {
                collection: collection.clone(),
                records: records.clone(),
            },
            Self::RefreshCollection {
                collection,
                records,
            } => reddb_file::StoreWalActionFrame::RefreshCollection {
                collection: collection.clone(),
                records: records.clone(),
            },
            Self::SetAuxMetadata { bytes } => reddb_file::StoreWalActionFrame::SetAuxMetadata {
                bytes: bytes.clone(),
            },
        }
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        match reddb_file::decode_store_wal_action_frame(bytes)? {
            reddb_file::StoreWalActionFrame::CreateCollection { name } => {
                Ok(Self::CreateCollection { name })
            }
            reddb_file::StoreWalActionFrame::DropCollection { name } => {
                Ok(Self::DropCollection { name })
            }
            reddb_file::StoreWalActionFrame::UpsertEntityRecord { collection, record } => {
                Ok(Self::UpsertEntityRecord { collection, record })
            }
            reddb_file::StoreWalActionFrame::DeleteEntityRecord {
                collection,
                entity_id,
            } => Ok(Self::DeleteEntityRecord {
                collection,
                entity_id,
            }),
            reddb_file::StoreWalActionFrame::BulkUpsertEntityRecords {
                collection,
                records,
            } => Ok(Self::BulkUpsertEntityRecords {
                collection,
                records,
            }),
            reddb_file::StoreWalActionFrame::RefreshCollection {
                collection,
                records,
            } => Ok(Self::RefreshCollection {
                collection,
                records,
            }),
            reddb_file::StoreWalActionFrame::SetAuxMetadata { bytes } => {
                Ok(Self::SetAuxMetadata { bytes })
            }
        }
    }
}

/// Host integration boundary for canonical logical WAL replay. The host applies
/// pager effects at its existing mutation boundaries; replay targets are private
/// until recovery completes successfully.
pub trait StoreReplayTarget {
    fn set_aux_metadata(&self, _bytes: &[u8]) -> Result<(), super::error::StoreError> {
        Err(super::error::StoreError::Serialization(
            "auxiliary metadata WAL actions are unsupported by this replay target".to_string(),
        ))
    }
    fn has_collection(&self, name: &str) -> bool;
    fn create_collection(&self, name: &str) -> Result<(), super::error::StoreError>;
    fn drop_collection(&self, name: &str) -> Result<(), super::error::StoreError>;
    fn upsert(&self, collection: &str, record: &[u8]) -> Result<(), super::error::StoreError>;
    fn delete(
        &self,
        collection: &str,
        id: super::entity::EntityId,
    ) -> Result<(), super::error::StoreError>;
    fn refresh(
        &self,
        collection: &str,
        records: &[Vec<u8>],
    ) -> Result<(), super::error::StoreError>;
}

/// Apply the existing native action semantics. Hosts retain publication and
/// rollback responsibilities; recovery targets remain private until success.
pub fn apply_replayed_action(
    target: &impl StoreReplayTarget,
    action: &StoreWalAction,
) -> Result<(), super::error::StoreError> {
    match action {
        StoreWalAction::SetAuxMetadata { bytes } => target.set_aux_metadata(bytes),
        StoreWalAction::CreateCollection { name } => {
            if !target.has_collection(name) {
                let _ = target.create_collection(name);
            }
            Ok(())
        }
        StoreWalAction::DropCollection { name } => target.drop_collection(name),
        StoreWalAction::UpsertEntityRecord { collection, record } => {
            target.upsert(collection, record)
        }
        StoreWalAction::DeleteEntityRecord {
            collection,
            entity_id,
        } => target.delete(collection, super::entity::EntityId::new(*entity_id)),
        StoreWalAction::BulkUpsertEntityRecords {
            collection,
            records,
        } => {
            for record in records {
                target.upsert(collection, record)?;
            }
            Ok(())
        }
        StoreWalAction::RefreshCollection {
            collection,
            records,
        } => target.refresh(collection, records),
    }
}
