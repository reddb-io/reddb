//! Explicit transactions publish logical changes only after their store WAL batch.
//! The outbox row shares that batch, so a crash cannot lose a committed projection.

use super::*;
use crate::replication::cdc::ChangeRecord;
use crate::runtime::execution_context::current_connection_id;
use crate::storage::unified::{DeferredStoreWalActions, EntityData, RowData};
use reddb_engine::storage::store_wal::StoreWalAction;
use std::sync::atomic::Ordering;

const OUTBOX_COLLECTION: &str = "red.storage.metadata";
const OUTBOX_KEY: &str = "replication.transaction_outbox";
// Members are hex-encoded inside the envelope and again by the JSON stream.
// Leave room below the default 32 MiB gRPC message bound for both expansions.
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const MAX_PENDING_RECORDS: usize = 65_536;

thread_local! {
    static COMMIT_RECEIPTS: std::cell::RefCell<Vec<(usize, Option<u64>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A synchronous statement owns its receipt even if another connection
/// commits before its response is built. Nested calls also notify their parent.
pub(super) struct CommitReceiptScope {
    index: usize,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl CommitReceiptScope {
    pub(super) fn begin(runtime: &RedDBRuntime) -> Self {
        let index = COMMIT_RECEIPTS.with(|frames| {
            let mut frames = frames.borrow_mut();
            let index = frames.len();
            frames.push((Arc::as_ptr(&runtime.inner) as usize, None));
            index
        });
        Self {
            index,
            _thread: std::marker::PhantomData,
        }
    }

    pub(super) fn lsn(&self) -> Option<u64> {
        COMMIT_RECEIPTS.with(|frames| frames.borrow()[self.index].1)
    }
}

impl Drop for CommitReceiptScope {
    fn drop(&mut self) {
        COMMIT_RECEIPTS.with(|frames| {
            let mut frames = frames.borrow_mut();
            assert_eq!(
                frames.len(),
                self.index + 1,
                "commit receipt scopes must nest"
            );
            frames.pop();
        });
    }
}

#[derive(Default)]
pub(super) struct PendingLogicalChanges {
    records: Vec<(u64, ChangeRecord)>,
    encoded_bytes: usize,
    rejected: bool,
}

impl PendingLogicalChanges {
    pub(super) fn len(&self) -> usize {
        self.records.len()
    }

    pub(super) fn truncate(&mut self, length: usize) {
        self.records.truncate(length);
        self.encoded_bytes = self
            .records
            .iter()
            .map(|(_, record)| record.encode().len())
            .sum();
    }

    fn push(&mut self, xid: u64, record: ChangeRecord) -> RedDBResult<()> {
        let bytes = record.encode().len();
        if self.rejected
            || self.records.len() >= MAX_PENDING_RECORDS
            || self.encoded_bytes.saturating_add(bytes) > MAX_PENDING_BYTES
        {
            self.rejected = true;
            return Err(Self::limit_error());
        }
        self.encoded_bytes += bytes;
        self.records.push((xid, record));
        Ok(())
    }

    fn limit_error() -> RedDBError {
        RedDBError::InvalidOperation(
            "transaction replication buffer limit exceeded; ROLLBACK is required".into(),
        )
    }
}

impl RedDBRuntime {
    pub(super) fn stage_or_publish_logical_record(&self, record: ChangeRecord) -> RedDBResult<()> {
        if let Some(xid) = self
            .current_xid()
            .filter(|_| UnifiedStore::deferred_store_wal_capture_active())
        {
            return self
                .inner
                .pending_logical_records
                .lock()
                .entry(current_connection_id())
                .or_default()
                .push(xid, record);
        }
        if self.inner.db.replication.is_none() {
            return Ok(());
        }
        let store = self.inner.db.store();
        let _storage = store.vault_publication_lock.lock();
        let _publication = self.inner.logical_publication.lock();
        // This path also retains already-running autocommits when an earlier
        // transaction's projection failed. Never skip an unpublished prefix.
        self.persist_logical_outbox(vec![record], 0, DeferredStoreWalActions::default())?;
        self.recover_transactional_replication().map_err(|error| {
            RedDBError::Internal(format!(
                "commit is locally durable; replication publication is pending: {error}"
            ))
        })
    }

    pub(super) fn commit_transactional_replication(
        &self,
        connection_id: u64,
        actions: DeferredStoreWalActions,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let pending = self
            .inner
            .pending_logical_records
            .lock()
            .remove(&connection_id)
            .unwrap_or_default();
        if pending.rejected {
            return Err(PendingLogicalChanges::limit_error());
        }
        let retained: Vec<_> = pending
            .records
            .into_iter()
            .filter(|(xid, _)| !self.snapshot_manager().is_aborted(*xid))
            .collect();
        let mut outbox_xid = retained.first().map(|(xid, _)| *xid).unwrap_or(0);
        let mut records: Vec<_> = retained.into_iter().map(|(_, record)| record).collect();
        if self.inner.db.replication.is_some() {
            for (collection, id, xid, _) in self
                .inner
                .pending_tombstones
                .read()
                .get(&connection_id)
                .into_iter()
                .flatten()
            {
                if self.snapshot_manager().is_aborted(*xid) {
                    continue;
                }
                if outbox_xid == 0 {
                    outbox_xid = *xid;
                }
                let cdc_lsn = self.inner.cdc.emit(
                    crate::replication::cdc::ChangeOperation::Delete,
                    collection,
                    id.raw(),
                    "entity",
                );
                records.push(ChangeRecord {
                    term: self.current_replication_term(),
                    lsn: cdc_lsn,
                    timestamp: crate::utils::now_unix_millis(),
                    operation: crate::replication::cdc::ChangeOperation::Delete,
                    collection: collection.clone(),
                    entity_id: id.raw(),
                    entity_kind: "entity".into(),
                    entity_bytes: None,
                    metadata: None,
                    refresh_records: None,
                    range_id: None,
                    ownership_epoch: None,
                });
            }
        }
        if records.is_empty() {
            return store
                .append_deferred_store_wal_actions(actions)
                .map_err(|error| RedDBError::Internal(error.to_string()));
        }
        self.persist_logical_outbox(records, outbox_xid, actions)
    }

    fn persist_logical_outbox(
        &self,
        mut records: Vec<ChangeRecord>,
        outbox_xid: u64,
        mut actions: DeferredStoreWalActions,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let _storage = store.vault_publication_lock.lock();
        if records.is_empty() {
            return store
                .append_deferred_store_wal_actions(actions)
                .map_err(|error| RedDBError::Internal(error.to_string()));
        }
        let _publication = self.inner.logical_publication.lock();
        let first = self.inner.logical_allocated_lsn.load(Ordering::Acquire);
        if outbox_xid != 0 {
            if records.len() > MAX_PENDING_RECORDS
                || records
                    .iter()
                    .map(|record| record.encode().len())
                    .sum::<usize>()
                    > MAX_PENDING_BYTES
            {
                return Err(PendingLogicalChanges::limit_error());
            }
            records =
                vec![
                    ChangeRecord::for_transaction(0, crate::utils::now_unix_millis(), records)
                        .with_term(self.current_replication_term()),
                ];
        }
        for (index, record) in records.iter_mut().enumerate() {
            record.lsn = first
                .checked_add(index as u64 + 1)
                .ok_or_else(|| RedDBError::Internal("logical replication LSN exhausted".into()))?;
        }
        let payloads: Vec<String> = records
            .iter()
            .map(|record| String::from_utf8(record.encode()).expect("JSON is UTF-8"))
            .collect();
        if outbox_xid != 0
            && payloads.iter().any(|payload| {
                payload.len().saturating_mul(2).saturating_add(96)
                    > crate::grpc::RedDBGrpcServer::max_message_bytes().saturating_sub(64 * 1024)
            })
        {
            return Err(RedDBError::InvalidOperation(
                "transaction exceeds replication transport limit; ROLLBACK is required".into(),
            ));
        }
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
                        (
                            "cdc_lsn".into(),
                            reddb_types::Value::UnsignedInteger(self.inner.cdc.current_lsn()),
                        ),
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
        let last = records.last().expect("nonempty outbox").lsn;
        self.inner
            .logical_allocated_lsn
            .store(last, Ordering::Release);
        COMMIT_RECEIPTS.with(|frames| {
            for (runtime, receipt) in frames.borrow_mut().iter_mut() {
                if *runtime == Arc::as_ptr(&self.inner) as usize {
                    *receipt = Some(last);
                }
            }
        });
        replication_crash_point("after_wal");
        // The source transaction's visibility has not been committed yet.
        // Projection and outbox retirement run only after MVCC finalization.
        Ok(())
    }

    fn publish_transaction_outbox(&self, records: &[ChangeRecord]) -> RedDBResult<()> {
        let Some(primary) = &self.inner.db.replication else {
            return Ok(());
        };
        let publishes_new = records
            .last()
            .is_some_and(|record| record.lsn > primary.current_logical_lsn());
        for record in records {
            primary
                .try_append_logical_record(record.lsn, record.encode())
                .map_err(|error| RedDBError::Internal(error.to_string()))?;
        }
        if publishes_new {
            replication_crash_point("after_projection");
        }
        Ok(())
    }

    pub(super) fn recover_transactional_replication(&self) -> RedDBResult<()> {
        if self.inner.db.replication.is_none() {
            return Ok(());
        }
        let store = self.inner.db.store();
        let _storage = store.vault_publication_lock.lock();
        let _publication = self.inner.logical_publication.lock();
        let Some(manager) = store.get_collection(OUTBOX_COLLECTION) else {
            return Ok(());
        };
        let mut batches = Vec::new();
        manager
            .try_for_each_entity(usize::MAX, &mut |entity: &UnifiedEntity| {
                if let EntityData::Row(row) = &entity.data {
                    if row.get_field("key").and_then(|value| value.as_text()) == Some(OUTBOX_KEY) {
                        batches.push((
                            entity.id,
                            entity.xmin,
                            row.get_field("value").cloned(),
                            row.get_field("cdc_lsn")
                                .and_then(|value| match value {
                                    reddb_types::Value::UnsignedInteger(value) => Some(*value),
                                    _ => None,
                                })
                                .unwrap_or(0),
                        ));
                    }
                }
                true
            })
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        let mut decoded = Vec::with_capacity(batches.len());
        for (id, xid, value, cdc_lsn) in batches {
            self.inner.cdc.set_current_lsn(cdc_lsn);
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
            if let Some(last) = records.last() {
                self.inner
                    .logical_allocated_lsn
                    .fetch_max(last.lsn, Ordering::AcqRel);
            }
            decoded.push((id, xid, records));
        }
        decoded.sort_by_key(|(_, _, records)| records.first().map(|record| record.lsn));
        let mut previous = None;
        for (id, xid, records) in decoded {
            if self.snapshot_manager().is_active(xid) {
                break;
            }
            self.publish_transaction_outbox(&records)?;
            // Retain the latest durable envelope as the canonical logical and
            // CDC frontier, independent of the derived spool's lifetime.
            if let Some(previous) = previous.replace(id) {
                store
                    .delete(OUTBOX_COLLECTION, previous)
                    .map_err(|error| RedDBError::Internal(error.to_string()))?;
            }
        }
        Ok(())
    }
}

fn replication_crash_point(point: &str) {
    #[cfg(debug_assertions)]
    if std::env::var("REDDB_TRANSACTION_REPLICATION_CRASH_AT")
        .ok()
        .as_deref()
        == Some(point)
    {
        std::process::exit(173);
    }
    let _ = point;
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn commit_receipt_ignores_other_connections_and_other_runtimes() {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory()
                .with_replication(crate::replication::ReplicationConfig::primary()),
        )
        .unwrap();
        runtime
            .execute_query("CREATE TABLE receipts (id INT)")
            .unwrap();
        let outer = CommitReceiptScope::begin(&runtime);
        let first = runtime
            .execute_query("INSERT INTO receipts (id) VALUES (1)")
            .unwrap();
        let first_lsn =
            crate::replication::CausalBookmark::decode(first.bookmark.as_deref().unwrap())
                .unwrap()
                .commit_lsn();
        assert_eq!(outer.lsn(), Some(first_lsn));
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    runtime
                        .execute_query("INSERT INTO receipts (id) VALUES (2)")
                        .unwrap();
                })
                .join()
                .unwrap();
        });
        assert!(runtime.primary_logical_head_lsn() > first_lsn);
        assert_eq!(outer.lsn(), Some(first_lsn));
        let other = RedDBRuntime::with_options(
            RedDBOptions::in_memory()
                .with_replication(crate::replication::ReplicationConfig::primary()),
        )
        .unwrap();
        other
            .execute_query("CREATE TABLE unrelated (id INT)")
            .unwrap();
        other
            .execute_query("INSERT INTO unrelated (id) VALUES (1), (2), (3)")
            .unwrap();
        assert_eq!(outer.lsn(), Some(first_lsn));
    }
}
