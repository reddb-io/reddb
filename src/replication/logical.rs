//! Logical replication helpers shared by replica apply and point-in-time restore.

use crate::api::{RedDBError, RedDBResult};
use crate::application::entity::metadata_from_json;
use crate::replication::cdc::{ChangeOperation, ChangeRecord};
use crate::storage::{EntityId, RedDB, UnifiedStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    Replica,
    Restore,
}

/// Shared logical change applier so replica replay and PITR converge on the
/// same semantics.
pub struct LogicalChangeApplier;

impl LogicalChangeApplier {
    pub fn apply_record(db: &RedDB, record: &ChangeRecord, _mode: ApplyMode) -> RedDBResult<()> {
        let store = db.store();
        match record.operation {
            ChangeOperation::Delete => {
                let _ = store.delete(&record.collection, EntityId::new(record.entity_id));
            }
            ChangeOperation::Insert | ChangeOperation::Update => {
                let Some(bytes) = &record.entity_bytes else {
                    return Err(RedDBError::Internal(
                        "replication record missing entity payload".to_string(),
                    ));
                };
                let entity = UnifiedStore::deserialize_entity(bytes, store.format_version())
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                let exists = store
                    .get(&record.collection, EntityId::new(record.entity_id))
                    .is_some();
                if exists {
                    let manager = store
                        .get_collection(&record.collection)
                        .ok_or_else(|| RedDBError::NotFound(record.collection.clone()))?;
                    manager
                        .update(entity.clone())
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                } else {
                    store
                        .insert_auto(&record.collection, entity.clone())
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                }
                if let Some(metadata_json) = &record.metadata {
                    let metadata = metadata_from_json(metadata_json)
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                    store
                        .set_metadata(&record.collection, entity.id, metadata)
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                }
                store
                    .context_index()
                    .index_entity(&record.collection, &entity);
            }
        }
        Ok(())
    }
}
