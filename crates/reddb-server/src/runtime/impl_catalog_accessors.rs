//! Runtime catalog / stats / maintenance accessors.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 8/10, issue #1629).
//! Houses connection `acquire`, `checkpoint`, the remote-write assertion,
//! `run_maintenance`, `scan_collection`, and the catalog /
//! attention-summary / `stats` readers.
use super::*;

fn runtime_pool_lock(runtime: &RedDBRuntime) -> std::sync::MutexGuard<'_, PoolState> {
    runtime
        .inner
        .pool
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl RedDBRuntime {
    pub fn acquire(&self) -> RedDBResult<RuntimeConnection> {
        self.acquire_connection(true)
    }

    // Transport admission already bounds wire sessions. Share connection IDs
    // and rollback cleanup without imposing the embedded pool's separate cap.
    pub(crate) fn acquire_wire_connection(&self) -> RedDBResult<RuntimeConnection> {
        self.acquire_connection(false)
    }

    fn acquire_connection(&self, uses_pool_slot: bool) -> RedDBResult<RuntimeConnection> {
        let mut pool = self
            .inner
            .pool
            .lock()
            .map_err(|e| RedDBError::Internal(format!("connection pool lock poisoned: {e}")))?;
        if uses_pool_slot && pool.pooled_active >= self.inner.pool_config.max_connections {
            return Err(RedDBError::Internal(
                "connection pool exhausted".to_string(),
            ));
        }

        let id = if let Some(id) = pool.idle.pop() {
            id
        } else {
            let id = pool.next_id;
            pool.next_id += 1;
            id
        };
        pool.active += 1;
        if uses_pool_slot {
            pool.pooled_active += 1;
        }
        pool.total_checkouts += 1;
        drop(pool);

        // Issue #1245 — record the connection acquisition after releasing
        // the pool lock so the lock hold time is unchanged.
        self.inner.node_load_telemetry.record_connect();

        Ok(RuntimeConnection {
            background_tasks: self.background_tasks.clone(),
            id,
            uses_pool_slot,
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn checkpoint(&self) -> RedDBResult<()> {
        // Local fsync always allowed — losing the lease shouldn't
        // prevent us from durably persisting what's already in memory.
        // The remote upload is the side-effect that risks clobbering a
        // peer's state, so it's behind the lease gate.
        self.checkpoint_local()?;
        if let Err(err) = self.assert_remote_write_allowed("checkpoint") {
            tracing::warn!(
                target: "reddb::serverless::lease",
                error = %err,
                "checkpoint: skipping remote upload — lease not held"
            );
            return Ok(());
        }
        self.inner
            .db
            .upload_to_remote_backend()
            .map_err(|err| RedDBError::Engine(err.to_string()))
    }

    pub(super) fn checkpoint_local(&self) -> RedDBResult<()> {
        let started = std::time::Instant::now();
        self.seal_hypertable_chunks_for_checkpoint(
            self.inner.checkpoint_columnar_emission_budget_chunks,
        )?;
        let store = self.inner.db.store();
        let publication = store.vault_publication_lock.lock();
        // Collections created since startup need the compact visibility
        // inventory before their first resident-to-disk publication too.
        self.prepare_paged_counts()?;
        self.persist_probabilistic_snapshots()?;
        self.inner
            .db
            .flush_local_after_runtime_snapshots()
            .map_err(|err| {
                // Issue #205 — local flush failure is a CheckpointFailed
                // operator-grade event. The local-flush path also covers
                // the WAL fsync we depend on, so a failure here doubles as
                // the WalFsyncFailed signal for the runtime entry point.
                let msg = err.to_string();
                crate::telemetry::operator_event::OperatorEvent::CheckpointFailed {
                    lsn: 0,
                    error: msg.clone(),
                }
                .emit_global();
                crate::telemetry::operator_event::OperatorEvent::WalFsyncFailed {
                    path: "<flush_local_only>".to_string(),
                    error: msg.clone(),
                }
                .emit_global();
                RedDBError::Engine(msg)
            })?;
        // The checkpoint now contains every mutation fenced above. Rebase the
        // ordered live overlay while publication is still exclusive so a
        // concurrent writer cannot be cleared after its WAL change.
        self.index_store_ref().clear_paged_btree_overlays();
        self.inner.probabilistic.replayed_names.lock().clear();
        drop(publication);
        let checkpoint_lsn = self.cdc_current_lsn();
        self.inner
            .checkpoint_projection_stats
            .record_checkpoint(checkpoint_lsn, started.elapsed().as_millis() as u64);
        Ok(())
    }

    /// Guard remote-mutating operations on the writer lease.
    /// Returns `Ok(())` when no remote backend is configured (the
    /// lease is irrelevant) or the lease state is `NotRequired` /
    /// `Held`. Returns `RedDBError::ReadOnly` when the lease is
    /// `NotHeld`, with an audit-friendly action label so the caller
    /// can record the rejection.
    pub(crate) fn assert_remote_write_allowed(&self, action: &str) -> RedDBResult<()> {
        if self.inner.db.remote_backend.is_none() {
            return Ok(());
        }
        match self.inner.write_gate.lease_state() {
            crate::runtime::write_gate::LeaseGateState::NotHeld => {
                self.inner.audit_log.record(
                    action,
                    "system",
                    "remote_backend",
                    "err: writer lease not held",
                    crate::json::Value::Null,
                );
                Err(RedDBError::ReadOnly(format!(
                    "writer lease not held — {action} blocked (serverless fence)"
                )))
            }
            _ => Ok(()),
        }
    }

    pub fn run_maintenance(&self) -> RedDBResult<()> {
        self.inner
            .db
            .run_maintenance()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        use reddb_rql::ast::PolicyAction;

        self.check_collection_privilege(PolicyAction::Select, collection)?;
        let store = self.inner.db.store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.to_string()))?;

        let offset = cursor.map(|cursor| cursor.offset).unwrap_or(0);
        let limit = limit.max(1);
        let read_bytes = usize::try_from(self.memory_budget().resolved_bytes / 16)
            .unwrap_or(usize::MAX)
            .clamp(1, 16 * 1024 * 1024);
        let (snapshot, _retention) = self.native_read_snapshot()?;
        let _snapshot_guard = super::impl_core::CurrentSnapshotGuard::install(snapshot.clone());
        let (total, items) = manager.try_page_in_id_order_at_snapshot(
            Some(&snapshot),
            offset,
            limit,
            read_bytes,
            |entity| {
                crate::runtime::rls_injection::entity_passes_rls_action(
                    self,
                    collection,
                    PolicyAction::Select,
                    entity,
                )
            },
        )?;
        let end = total.min(offset.saturating_add(limit));
        let next = (end < total).then_some(ScanCursor { offset: end });

        Ok(ScanPage {
            collection: collection.to_string(),
            items,
            next,
            total,
        })
    }

    pub fn get_collection_entity(
        &self,
        collection: &str,
        id: crate::storage::EntityId,
    ) -> RedDBResult<Option<crate::storage::UnifiedEntity>> {
        use reddb_rql::ast::PolicyAction;

        self.check_collection_privilege(PolicyAction::Select, collection)?;
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(None);
        };
        let (snapshot, _retention) = self.native_read_snapshot()?;
        let _snapshot_guard = super::impl_core::CurrentSnapshotGuard::install(snapshot.clone());
        let read_bytes = usize::try_from(self.memory_budget().resolved_bytes / 16)
            .unwrap_or(usize::MAX)
            .clamp(1, 16 * 1024 * 1024);
        let mut entity = manager.try_get(id, read_bytes)?.filter(|candidate| {
            candidate.logical_id() == id
                && super::impl_core::entity_visible_with_context(Some(&snapshot), candidate)
        });
        // Public IDs are logical identities. A direct physical lookup can
        // return a superseded version or miss the version visible to a reader.
        if entity.is_none() {
            manager.try_for_each_entity(read_bytes, |candidate| {
                if candidate.logical_id() == id
                    && super::impl_core::entity_visible_with_context(Some(&snapshot), candidate)
                    && entity.as_ref().is_none_or(|selected: &UnifiedEntity| {
                        (candidate.xmin, candidate.id) > (selected.xmin, selected.id)
                    })
                {
                    entity = Some(candidate.clone());
                }
                true
            })?;
        }
        manager.restore_owned_row_schemas(entity.iter_mut());
        Ok(entity.filter(|entity| {
            crate::runtime::rls_injection::entity_passes_rls_action(
                self,
                collection,
                PolicyAction::Select,
                entity,
            )
        }))
    }

    pub(crate) fn native_read_snapshot(
        &self,
    ) -> RedDBResult<(
        super::impl_core::SnapshotContext,
        crate::storage::transaction::snapshot::SnapshotRetentionGuard,
    )> {
        let state = &self.inner.transaction_state;
        let manager = state.snapshot_manager();
        if let Some(context) = super::impl_core::capture_current_snapshot()
            .filter(|context| Arc::ptr_eq(&context.manager, &manager))
        {
            let (_, retention) = manager.retain_snapshot(context.snapshot.clone());
            return Ok((context, retention));
        }
        let connection_id = super::impl_core::current_connection_id();
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let (snapshot, retention) = state.retained_current_snapshot(connection_id);
        let branch = self.vcs_branch_read_view(&snapshot, None)?;
        let context = state.context(connection_id);
        Ok((
            super::impl_core::SnapshotContext {
                branch,
                snapshot,
                manager,
                own_xids: self.own_transaction_xids(),
                requires_index_fallback: true,
                serializable_reader: context
                    .filter(|context| {
                        context.isolation
                            == crate::storage::transaction::IsolationLevel::Serializable
                    })
                    .map(|context| context.xid),
            },
            retention,
        ))
    }

    pub fn catalog(&self) -> CatalogModelSnapshot {
        self.inner.db.catalog_model_snapshot()
    }

    /// Fallible catalog for disk-resident databases. The decode workspace shares
    /// query admission with concurrent reads and is released on every error.
    pub fn try_catalog(&self) -> RedDBResult<CatalogModelSnapshot> {
        if self
            .inner
            .db
            .store()
            .config()
            .resident_memory_bytes_max
            .is_none()
        {
            return Ok(self.catalog());
        }
        let read_bytes = usize::try_from(self.memory_budget().resolved_bytes / 16)
            .unwrap_or(usize::MAX)
            .clamp(1, 16 * 1024 * 1024);
        let _scratch = self
            .try_reserve_query_buffer(read_bytes * 2)
            .ok_or_else(|| {
                self.memory_accounting().record_admission_denied();
                RedDBError::MaterializationLimitExceeded {
                    executor: "catalog",
                    limit: usize::try_from(self.memory_budget().resolved_bytes)
                        .unwrap_or(usize::MAX),
                    current: usize::try_from(self.memory_accounting().total_used_bytes())
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                }
            })?;
        Ok(self.inner.db.try_catalog_model_snapshot(read_bytes)?)
    }

    /// Fallible catalog inventory scoped to known collection names. This keeps
    /// filtered `red.collections` queries from decoding every unrelated
    /// collection before their predicate is applied.
    pub(crate) fn try_catalog_for_collections(
        &self,
        collection_names: &std::collections::HashSet<String>,
        observe_legacy_payloads: bool,
    ) -> RedDBResult<CatalogModelSnapshot> {
        if self
            .inner
            .db
            .store()
            .config()
            .resident_memory_bytes_max
            .is_none()
        {
            return Ok(self.inner.db.try_catalog_model_snapshot_for_collections(
                collection_names,
                observe_legacy_payloads,
                usize::MAX,
            )?);
        }
        let read_bytes = usize::try_from(self.memory_budget().resolved_bytes / 16)
            .unwrap_or(usize::MAX)
            .clamp(1, 16 * 1024 * 1024);
        let _scratch = self
            .try_reserve_query_buffer(read_bytes * 2)
            .ok_or_else(|| {
                self.memory_accounting().record_admission_denied();
                RedDBError::MaterializationLimitExceeded {
                    executor: "catalog",
                    limit: usize::try_from(self.memory_budget().resolved_bytes)
                        .unwrap_or(usize::MAX),
                    current: usize::try_from(self.memory_accounting().total_used_bytes())
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                }
            })?;
        Ok(self.inner.db.try_catalog_model_snapshot_for_collections(
            collection_names,
            observe_legacy_payloads,
            read_bytes,
        )?)
    }

    pub fn catalog_consistency_report(&self) -> crate::catalog::CatalogConsistencyReport {
        self.inner.db.catalog_consistency_report()
    }

    pub fn catalog_attention_summary(&self) -> CatalogAttentionSummary {
        crate::catalog::attention_summary(&self.catalog())
    }

    pub fn collection_attention(&self) -> Vec<CollectionDescriptor> {
        crate::catalog::collection_attention(&self.catalog())
    }

    pub fn index_attention(&self) -> Vec<CatalogIndexStatus> {
        crate::catalog::index_attention(&self.catalog())
    }

    pub fn graph_projection_attention(&self) -> Vec<CatalogGraphProjectionStatus> {
        crate::catalog::graph_projection_attention(&self.catalog())
    }

    pub fn analytics_job_attention(&self) -> Vec<CatalogAnalyticsJobStatus> {
        crate::catalog::analytics_job_attention(&self.catalog())
    }

    pub fn stats(&self) -> RuntimeStats {
        let pool = runtime_pool_lock(self);
        RuntimeStats {
            active_connections: pool.active,
            idle_connections: pool.idle.len(),
            total_checkouts: pool.total_checkouts,
            paged_mode: self.inner.db.is_paged(),
            started_at_unix_ms: self.inner.started_at_unix_ms,
            store: self.inner.db.stats(),
            system: SystemInfo::collect(),
            result_blob_cache: self.inner.result_blob_cache.stats(),
            kv: self.inner.kv_stats.snapshot(),
            metrics_ingest: self.inner.metrics_ingest_stats.snapshot(),
        }
    }
}

#[cfg(test)]
mod security_boundary_tests {
    use super::*;
    use crate::application::entity::{DeleteEntityInput, PatchEntityInput};
    use crate::application::ports::RuntimeEntityPort;
    use crate::auth::Role;
    use crate::runtime::execution_context::{with_snapshot_bundle, SnapshotBundle};
    use crate::storage::{EntityData, UnifiedEntity};
    use reddb_types::Value;

    fn text_field<'a>(entity: &'a UnifiedEntity, name: &str) -> Option<&'a str> {
        let EntityData::Row(row) = &entity.data else {
            return None;
        };
        let value = row
            .named
            .as_ref()
            .and_then(|named| named.get(name))
            .or_else(|| {
                row.schema.as_ref().and_then(|schema| {
                    schema
                        .iter()
                        .position(|column| column == name)
                        .and_then(|index| row.columns.get(index))
                })
            })?;
        match value {
            Value::Text(value) => Some(value.as_ref()),
            _ => None,
        }
    }

    #[test]
    fn typed_entity_boundary_filters_before_pagination_and_hides_foreign_ids() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        runtime
            .execute_query(
                "CREATE TABLE docs (id INT, tenant_id TEXT, body TEXT) TENANT BY (tenant_id)",
            )
            .expect("tenant table");
        runtime
            .execute_query(
                "INSERT INTO docs (id, tenant_id, body) VALUES \
                 (1, 'acme', 'first'), (2, 'globex', 'hidden'), (3, 'acme', 'second')",
            )
            .expect("seed platform rows");

        let unfiltered = runtime
            .scan_collection("docs", None, 10)
            .expect("platform scan");
        let foreign_id = unfiltered
            .items
            .iter()
            .find(|entity| text_field(entity, "tenant_id") == Some("globex"))
            .map(|entity| entity.id)
            .expect("foreign row");

        let bundle = SnapshotBundle {
            snapshot: None,
            auth: Some(("alice".to_string(), Role::Write)),
            auth_principal: Some(crate::auth::UserId::scoped("acme", "alice")),
            tenant: Some("acme".to_string()),
        };
        with_snapshot_bundle(&bundle, || {
            let first = runtime
                .scan_collection("docs", None, 1)
                .expect("first visible page");
            assert_eq!(first.total, 2);
            assert_eq!(first.items.len(), 1);
            assert_eq!(text_field(&first.items[0], "tenant_id"), Some("acme"));
            assert_eq!(first.next, Some(ScanCursor { offset: 1 }));

            let second = runtime
                .scan_collection("docs", first.next, 1)
                .expect("second visible page");
            assert_eq!(second.total, 2);
            assert_eq!(second.items.len(), 1);
            assert_eq!(text_field(&second.items[0], "tenant_id"), Some("acme"));
            assert_eq!(second.next, None);

            assert!(runtime
                .get_collection_entity("docs", foreign_id)
                .expect("typed get")
                .is_none());
            assert!(matches!(
                runtime.patch_entity(PatchEntityInput {
                    collection: "docs".to_string(),
                    id: foreign_id,
                    payload: crate::json::Value::Object(Default::default()),
                    operations: Vec::new(),
                }),
                Err(crate::RedDBError::NotFound(_))
            ));
            let deleted = runtime
                .delete_entity(DeleteEntityInput {
                    collection: "docs".to_string(),
                    id: foreign_id,
                })
                .expect("typed delete");
            assert!(!deleted.deleted);
        });

        assert!(runtime.db().store().get("docs", foreign_id).is_some());
    }

    #[test]
    fn typed_entity_boundary_uses_model_specific_rls_policies() {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        for query in [
            "CREATE GRAPH links",
            "INSERT INTO links NODE (label,node_type,visible) VALUES ('shown','Item',true)",
            "INSERT INTO links NODE (label,node_type,visible) VALUES ('hidden','Item',false)",
            "CREATE POLICY visible_nodes ON NODES OF links FOR SELECT USING (properties.visible=true)",
            "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
            "CREATE DOCUMENT papers",
            "INSERT INTO papers DOCUMENT VALUES ({name:'shown',visible:true})",
            "INSERT INTO papers DOCUMENT VALUES ({name:'hidden',visible:false})",
            "CREATE POLICY visible_documents ON DOCUMENTS OF papers FOR SELECT USING (visible=true)",
            "ALTER TABLE papers ENABLE ROW LEVEL SECURITY",
        ] {
            runtime.execute_query(query).expect(query);
        }

        let bundle = SnapshotBundle {
            snapshot: None,
            auth: Some(("alice".to_string(), Role::Read)),
            auth_principal: Some(crate::auth::UserId::platform("alice")),
            tenant: None,
        };
        with_snapshot_bundle(&bundle, || {
            let nodes = runtime
                .scan_collection("links", None, 10)
                .expect("typed graph scan");
            assert_eq!(nodes.total, 1);
            assert_eq!(nodes.items.len(), 1);

            let documents = runtime
                .scan_collection("papers", None, 10)
                .expect("typed document scan");
            assert_eq!(documents.total, 1);
            assert_eq!(documents.items.len(), 1);
        });
    }
}
