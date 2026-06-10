use super::*;
use crate::application::entity::metadata_to_json;
use crate::replication::cdc::{server_json_to_wire_json, ChangeRecord};
use crate::runtime::impl_core::current_connection_id;
use std::time::{SystemTime, UNIX_EPOCH};

impl RedDBRuntime {
    fn latest_metadata_for(
        &self,
        collection: &str,
        entity_id: u64,
    ) -> Option<reddb_wire::replication::ChangeRecordJsonValue> {
        self.inner
            .db
            .store()
            .get_metadata(collection, EntityId::new(entity_id))
            .map(|metadata| server_json_to_wire_json(metadata_to_json(&metadata)))
    }

    /// Emit a CDC record without invalidating the result cache.
    ///
    /// Used by `MutationEngine::append_batch` which calls
    /// `invalidate_result_cache` once for the whole batch before this
    /// loop, avoiding N write-lock acquisitions.
    pub(crate) fn cdc_emit_no_cache_invalidate(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) -> u64 {
        let lsn = self
            .inner
            .cdc
            .emit(operation, collection, entity_id, entity_kind);

        // Append to logical WAL replication buffer (if primary mode)
        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let entity = if operation == crate::replication::cdc::ChangeOperation::Delete {
                None
            } else {
                store.get(collection, EntityId::new(entity_id))
            };
            let record = ChangeRecord {
                term: self.current_replication_term(),
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id,
                entity_kind: entity_kind.to_string(),
                entity_bytes: entity
                    .as_ref()
                    .map(|e| UnifiedStore::serialize_entity(e, store.format_version())),
                metadata: self.latest_metadata_for(collection, entity_id),
                refresh_records: None,
                range_id: None,
                ownership_epoch: None,
            };
            let encoded = record.encode();
            primary.append_logical_record(record.lsn, encoded);
        }
        lsn
    }

    pub(crate) fn cdc_emit_insert_batch_no_cache_invalidate(
        &self,
        collection: &str,
        ids: &[EntityId],
        entity_kind: &str,
    ) -> Vec<u64> {
        if ids.is_empty() {
            return Vec::new();
        }

        // Without logical replication, CDC only needs the in-memory event
        // ring. Reserve all LSNs and push the batch under one mutex instead
        // of taking the ring lock once per inserted row.
        if self.inner.db.replication.is_none() {
            return self.inner.cdc.emit_batch_same_collection(
                crate::replication::cdc::ChangeOperation::Insert,
                collection,
                entity_kind,
                ids.iter().map(|id| id.raw()),
            );
        }

        // Replication needs one logical-WAL record per entity with the
        // serialized entity bytes, so keep the existing per-row path.
        ids.iter()
            .map(|id| {
                self.cdc_emit_no_cache_invalidate(
                    crate::replication::cdc::ChangeOperation::Insert,
                    collection,
                    id.raw(),
                    entity_kind,
                )
            })
            .collect()
    }

    pub fn cdc_emit(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) -> u64 {
        let lsn = self
            .inner
            .cdc
            .emit(operation, collection, entity_id, entity_kind);
        // Perf: prior to this we called `invalidate_result_cache()`
        // which wipes EVERY cached query, across every table, under
        // a write lock — turning each INSERT into a serialisation
        // point for all readers. Swap to the per-table variant so
        // unrelated query caches survive.
        self.invalidate_result_cache_for_table(collection);

        // Append to logical WAL replication buffer (if primary mode)
        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let entity = if operation == crate::replication::cdc::ChangeOperation::Delete {
                None
            } else {
                store.get(collection, EntityId::new(entity_id))
            };
            let record = ChangeRecord {
                term: self.current_replication_term(),
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id,
                entity_kind: entity_kind.to_string(),
                entity_bytes: entity
                    .as_ref()
                    .map(|entity| UnifiedStore::serialize_entity(entity, store.format_version())),
                metadata: self.latest_metadata_for(collection, entity_id),
                refresh_records: None,
                range_id: None,
                ownership_epoch: None,
            };
            let encoded = record.encode();
            primary.append_logical_record(record.lsn, encoded);
        }
        lsn
    }

    pub(crate) fn cdc_emit_kv(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        key: &str,
        entity_id: u64,
        before: Option<crate::json::Value>,
        after: Option<crate::json::Value>,
    ) -> u64 {
        let lsn = self
            .inner
            .cdc
            .emit_kv(operation, collection, key, entity_id, before, after);
        self.inner.kv_stats.incr_watch_events_emitted();
        self.invalidate_result_cache_for_table(collection);
        lsn
    }

    pub(crate) fn record_kv_watch_event(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        key: &str,
        entity_id: u64,
        before: Option<crate::json::Value>,
        after: Option<crate::json::Value>,
    ) {
        if self.current_xid().is_some() {
            let conn_id = current_connection_id();
            let event = crate::replication::cdc::KvWatchEvent {
                collection: collection.to_string(),
                key: key.to_string(),
                op: operation,
                before,
                after,
                lsn: 0,
                committed_at: 0,
                dropped_event_count: 0,
            };
            self.inner
                .pending_kv_watch_events
                .write()
                .entry(conn_id)
                .or_default()
                .push(event);
            return;
        }

        self.cdc_emit_kv(operation, collection, key, entity_id, before, after);
    }

    pub(crate) fn cdc_emit_prebuilt(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity: &UnifiedEntity,
        entity_kind: &str,
        metadata: Option<&crate::storage::Metadata>,
        invalidate_cache: bool,
    ) -> u64 {
        self.cdc_emit_prebuilt_with_columns(
            operation,
            collection,
            entity,
            entity_kind,
            metadata,
            invalidate_cache,
            None,
        )
    }

    /// `cdc_emit_prebuilt` plus the list of column names whose values
    /// changed on this update. Callers that have already computed a
    /// `RowDamageVector` pass it here so downstream CDC consumers can
    /// filter events by touched column without re-diffing.
    /// `changed_columns` is only meaningful for `Update` operations —
    /// insert and delete events ignore it.
    pub(crate) fn cdc_emit_prebuilt_with_columns(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity: &UnifiedEntity,
        entity_kind: &str,
        metadata: Option<&crate::storage::Metadata>,
        invalidate_cache: bool,
        changed_columns: Option<Vec<String>>,
    ) -> u64 {
        if invalidate_cache {
            self.invalidate_result_cache();
        }

        let public_id = entity.logical_id().raw();
        let lsn = self.inner.cdc.emit_with_columns(
            operation,
            collection,
            public_id,
            entity_kind,
            changed_columns,
        );

        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let record = ChangeRecord {
                term: self.current_replication_term(),
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id: entity.id.raw(),
                entity_kind: entity_kind.to_string(),
                entity_bytes: Some(UnifiedStore::serialize_entity(
                    entity,
                    store.format_version(),
                )),
                metadata: metadata
                    .map(metadata_to_json)
                    .map(server_json_to_wire_json)
                    .or_else(|| self.latest_metadata_for(collection, entity.id.raw())),
                refresh_records: None,
                range_id: None,
                ownership_epoch: None,
            };
            let encoded = record.encode();
            primary.append_logical_record(record.lsn, encoded);
        }

        lsn
    }

    pub(crate) fn current_replication_term(&self) -> u64 {
        self.inner.db.options().replication.term
    }

    pub(crate) fn cdc_emit_prebuilt_batch<'a, I>(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        entity_kind: &str,
        items: I,
        invalidate_cache: bool,
    ) where
        I: IntoIterator<
            Item = (
                &'a str,
                &'a UnifiedEntity,
                Option<&'a crate::storage::Metadata>,
            ),
        >,
    {
        let items: Vec<(&str, &UnifiedEntity, Option<&crate::storage::Metadata>)> =
            items.into_iter().collect();
        if items.is_empty() {
            return;
        }

        if invalidate_cache {
            self.invalidate_result_cache();
        }

        for (collection, entity, metadata) in items {
            self.cdc_emit_prebuilt(operation, collection, entity, entity_kind, metadata, false);
        }
    }

    /// Poll CDC events since a given LSN.
    pub fn cdc_poll(
        &self,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::ChangeEvent> {
        self.inner.cdc.poll(since_lsn, max_count)
    }

    /// PLAN.md Phase 11.4 — current CDC LSN. Public mutation
    /// surfaces (HTTP query, gRPC entity ops) call this immediately
    /// after a successful write to feed `enforce_commit_policy`.
    pub fn cdc_current_lsn(&self) -> u64 {
        self.inner.cdc.current_lsn()
    }

    pub fn kv_watch_events_since(
        &self,
        collection: &str,
        key: &str,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::KvWatchEvent> {
        self.inner
            .cdc
            .poll(since_lsn, max_count)
            .into_iter()
            .filter_map(|event| event.kv)
            .filter(|event| event.collection == collection && event.key == key)
            .collect()
    }

    pub fn kv_watch_events_since_prefix(
        &self,
        collection: &str,
        prefix: &str,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::KvWatchEvent> {
        self.inner
            .cdc
            .poll(since_lsn, max_count)
            .into_iter()
            .filter_map(|event| event.kv)
            .filter(|event| event.collection == collection && event.key.starts_with(prefix))
            .collect()
    }

    pub(crate) fn kv_watch_subscribe<'a>(
        &'a self,
        collection: impl Into<String>,
        key: impl Into<String>,
        from_lsn: Option<u64>,
    ) -> crate::runtime::kv_watch::KvWatchStream<'a> {
        crate::runtime::kv_watch::KvWatchStream::subscribe(
            &self.inner.cdc,
            &self.inner.kv_stats,
            collection,
            key,
            from_lsn,
            self.kv_watch_idle_timeout_ms(),
        )
    }

    pub(crate) fn kv_watch_subscribe_prefix<'a>(
        &'a self,
        collection: impl Into<String>,
        prefix: impl Into<String>,
        from_lsn: Option<u64>,
    ) -> crate::runtime::kv_watch::KvWatchStream<'a> {
        crate::runtime::kv_watch::KvWatchStream::subscribe_prefix(
            &self.inner.cdc,
            &self.inner.kv_stats,
            collection,
            prefix,
            from_lsn,
            self.kv_watch_idle_timeout_ms(),
        )
    }

    pub(crate) fn kv_watch_idle_timeout_ms(&self) -> u64 {
        self.config_u64("red.config.kv.watch.idle_timeout_ms", 60_000)
    }
}
