//! Explicit transactions publish logical changes only after their store WAL batch.
//! The outbox row shares that batch, so a crash cannot lose a committed projection.

use super::*;
use crate::replication::cdc::ChangeRecord;
use crate::runtime::execution_context::current_connection_id;
use crate::storage::unified::{DeferredStoreWalActions, EntityData, RowData};
use reddb_engine::storage::store_wal::StoreWalAction;

const OUTBOX_COLLECTION: &str = "red.storage.metadata";
const OUTBOX_KEY: &str = "replication.transaction_outbox";

impl RedDBRuntime {
    pub(super) fn stage_or_publish_logical_record(&self, record: ChangeRecord) {
        if let Some(xid) = self.current_xid() {
            self.inner
                .pending_logical_records
                .lock()
                .entry(current_connection_id())
                .or_default()
                .push((xid, record));
        } else if let Some(primary) = &self.inner.db.replication {
            primary.append_logical_record(record.lsn, record.encode());
        }
    }

    pub(super) fn commit_transactional_replication(
        &self,
        connection_id: u64,
        mut actions: DeferredStoreWalActions,
    ) -> RedDBResult<()> {
        let _publication = self.inner.logical_publication.lock();
        let store = self.inner.db.store();
        let pending = self
            .inner
            .pending_logical_records
            .lock()
            .remove(&connection_id)
            .unwrap_or_default();
        let retained: Vec<_> = pending
            .into_iter()
            .filter(|(xid, _)| !self.snapshot_manager().is_aborted(*xid))
            .collect();
        let outbox_xid = retained.first().map(|(xid, _)| *xid).unwrap_or(0);
        let mut records: Vec<_> = retained.into_iter().map(|(_, record)| record).collect();
        if records.is_empty() {
            return store
                .append_deferred_store_wal_actions(actions)
                .map_err(|error| RedDBError::Internal(error.to_string()));
        }
        self.recover_transactional_replication()?;
        let first = self.inner.cdc.current_lsn();
        for (index, record) in records.iter_mut().enumerate() {
            record.lsn = first
                .checked_add(index as u64 + 1)
                .ok_or_else(|| RedDBError::Internal("logical replication LSN exhausted".into()))?;
        }
        let payloads: Vec<String> = records
            .iter()
            .map(|record| String::from_utf8(record.encode()).expect("JSON is UTF-8"))
            .collect();
        let payload = crate::serde_json::to_string(&payloads)
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        let mut outbox = UnifiedEntity::new(
            store.next_entity_id(),
            EntityKind::TableRow {
                table: Arc::from(OUTBOX_COLLECTION),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        ("key".into(), reddb_types::Value::text(OUTBOX_KEY)),
                        ("value".into(), reddb_types::Value::text(payload)),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        );
        outbox.set_xmin(outbox_xid);
        let action =
            StoreWalAction::upsert_entity(OUTBOX_COLLECTION, &outbox, None, store.format_version());
        // The caller holds the checkpoint fence. Install the outbox privately,
        // then persist it together with the transaction's data in one batch.
        store.get_or_create_collection(OUTBOX_COLLECTION);
        store
            .apply_replayed_action(&action, true)
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        actions.push(action);
        replication_crash_point("before_wal");
        if let Err(error) = store.append_deferred_store_wal_actions(actions) {
            store
                .apply_replayed_action(
                    &StoreWalAction::DeleteEntityRecord {
                        collection: OUTBOX_COLLECTION.into(),
                        entity_id: outbox.id.raw(),
                    },
                    true,
                )
                .map_err(|undo| RedDBError::Internal(undo.to_string()))?;
            return Err(RedDBError::Internal(error.to_string()));
        }
        replication_crash_point("after_wal");
        // Local commit is now durable. Projection failure must not turn it into
        // an in-memory rollback; retain the outbox for recovery instead.
        if let Err(error) = self.publish_transaction_outbox(outbox.id, &records) {
            tracing::error!(%error, "committed replication outbox awaits recovery");
        }
        Ok(())
    }

    fn publish_transaction_outbox(
        &self,
        id: EntityId,
        records: &[ChangeRecord],
    ) -> RedDBResult<()> {
        let Some(primary) = &self.inner.db.replication else {
            return Ok(());
        };
        for record in records {
            primary
                .try_append_logical_record(record.lsn, record.encode())
                .map_err(|error| RedDBError::Internal(error.to_string()))?;
            if self.inner.cdc.current_lsn() < record.lsn {
                let public_id = record
                    .entity_bytes
                    .as_ref()
                    .and_then(|bytes| {
                        UnifiedStore::deserialize_entity(
                            bytes,
                            self.inner.db.store().format_version(),
                        )
                        .ok()
                    })
                    .map(|entity| entity.logical_id().raw())
                    .unwrap_or(record.entity_id);
                self.inner
                    .cdc
                    .publish_committed(crate::replication::cdc::ChangeEvent {
                        lsn: record.lsn,
                        timestamp: record.timestamp,
                        operation: record.operation,
                        collection: record.collection.clone(),
                        entity_id: public_id,
                        entity_kind: record.entity_kind.clone(),
                        changed_columns: None,
                        kv: None,
                    });
            }
        }
        replication_crash_point("after_projection");
        self.inner
            .db
            .store()
            .delete(OUTBOX_COLLECTION, id)
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        Ok(())
    }

    pub(super) fn recover_transactional_replication(&self) -> RedDBResult<()> {
        if self.inner.db.replication.is_none() {
            return Ok(());
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(OUTBOX_COLLECTION) else {
            return Ok(());
        };
        let mut batches = Vec::new();
        manager
            .try_for_each_entity(usize::MAX, &mut |entity: &UnifiedEntity| {
                if let EntityData::Row(row) = &entity.data {
                    if row.get_field("key").and_then(|value| value.as_text()) == Some(OUTBOX_KEY) {
                        batches.push((entity.id, row.get_field("value").cloned()));
                    }
                }
                true
            })
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        let mut decoded = Vec::with_capacity(batches.len());
        for (id, value) in batches {
            let payload = value
                .as_ref()
                .and_then(|value| value.as_text())
                .ok_or_else(|| RedDBError::Internal("invalid replication outbox".into()))?;
            let payloads: Vec<String> = crate::serde_json::from_str(payload)
                .map_err(|error| RedDBError::Internal(error.to_string()))?;
            let records = payloads
                .iter()
                .map(|payload| ChangeRecord::decode(payload.as_bytes()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(RedDBError::Internal)?;
            decoded.push((id, records));
        }
        decoded.sort_by_key(|(_, records)| records.first().map(|record| record.lsn));
        for (id, records) in decoded {
            self.publish_transaction_outbox(id, &records)?;
        }
        Ok(())
    }
}

fn replication_crash_point(point: &str) {
    #[cfg(debug_assertions)]
    if std::env::var("REDDB_TRANSACTION_REPLICATION_CRASH_AT").ok().as_deref() == Some(point) {
        std::process::exit(173);
    }
    let _ = point;
}
