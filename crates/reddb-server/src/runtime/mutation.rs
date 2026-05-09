//! Unified row mutation engine.
//!
//! All frontends (SQL, HTTP, gRPC, stdio) converge here so that index
//! maintenance, CDC emission, and result-cache invalidation happen exactly
//! once per batch regardless of the call site.
//!
//! # Kernel selection
//!
//! The engine picks a physical write kernel by batch cardinality:
//!
//! * `append_one`        — single row; uses `insert_auto` (preprocessors run).
//! * `append_micro_batch`— 2–128 rows; uses `bulk_insert` (one segment lock).
//! * `append_bulk`       — >128 rows; same path, batched CDC.
//!
//! All kernels share the same semantic steps post-write:
//! context index → secondary indexes → CDC → one `invalidate_result_cache`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::json::{Map as JsonMap, Value as JsonValue};
use crate::presentation::entity_json::storage_value_to_json;
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, VectorRef};
use crate::storage::unified::{
    entity::{CrossRef, EntityData, EntityId, EntityKind, RefType, RowData, UnifiedEntity},
    Metadata, MetadataValue, UnifiedStore,
};
use crate::{RedDBError, RedDBResult};

/// One row queued for insertion, already schema-normalised and
/// validated by the application layer.
pub(crate) struct MutationRow {
    pub fields: Vec<(String, Value)>,
    pub metadata: Vec<(String, MetadataValue)>,
    pub node_links: Vec<NodeRef>,
    pub vector_links: Vec<VectorRef>,
}

/// Output of a `MutationEngine::apply` call.
pub(crate) struct MutationResult {
    pub ids: Vec<EntityId>,
}

impl MutationResult {
    fn empty() -> Self {
        Self { ids: Vec::new() }
    }
}

/// Central row mutation engine.
///
/// Created per-call via `RedDBRuntime::mutation_engine()` and dropped
/// immediately after `apply` returns. It borrows the runtime rather
/// than cloning Arcs so there is no allocation overhead on construction.
pub(crate) struct MutationEngine<'rt> {
    runtime: &'rt crate::RedDBRuntime,
    store: Arc<UnifiedStore>,
    suppress_events: bool,
}

impl<'rt> MutationEngine<'rt> {
    pub(crate) fn new(runtime: &'rt crate::RedDBRuntime) -> Self {
        let store = runtime.db().store();
        Self { runtime, store, suppress_events: false }
    }

    pub(crate) fn with_suppress_events(mut self) -> Self {
        self.suppress_events = true;
        self
    }

    /// Dispatch to the right kernel by batch size.
    pub(crate) fn apply(
        &self,
        collection: String,
        mut rows: Vec<MutationRow>,
    ) -> RedDBResult<MutationResult> {
        // Public-mutation gate (PLAN.md W1). Every public path that
        // creates rows funnels through `apply`, so a single check here
        // covers SQL `INSERT`, gRPC `Insert`/`BulkInsert`, HTTP
        // `POST /collections/X`, and the native-wire equivalent. The
        // replica internal apply path uses `LogicalChangeApplier`,
        // which talks to the store directly and never enters this
        // method, so legitimate replica catch-up is unaffected.
        self.runtime
            .check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        match rows.len() {
            0 => Ok(MutationResult::empty()),
            1 => self.append_one(collection, rows.remove(0)),
            _ => self.append_batch(collection, rows),
        }
    }

    // ── Kernel: single row ────────────────────────────────────────────────

    fn append_one(&self, collection: String, row: MutationRow) -> RedDBResult<MutationResult> {
        let db = self.runtime.db();

        // Build entity — same logic as BatchBuilder::add_row
        let mut entity = build_table_entity(
            self.store.as_ref(),
            &collection,
            row.fields.clone(),
            &row.node_links,
            &row.vector_links,
        );

        // MVCC xmin stamping (#30).
        //
        // Inside a BEGIN-wrapped transaction the writer reuses its
        // tx xid; outside (autocommit) we draw a "born committed" xid
        // from the snapshot manager's pool. Pool xids skip the
        // active-set insert/remove pair entirely — a pool refill costs
        // one `fetch_add(BATCH)` per `AUTOCOMMIT_POOL_BATCH` writes
        // instead of two `state.write()` lock acquisitions per write.
        // Visibility is identical: the legacy `begin()/commit()` pair
        // also leaves the xid out of `active` and `aborted` by the
        // time `insert_auto` runs, so concurrent readers see the same
        // row state either way (see `SnapshotManager::allocate_committed_xid`).
        // xmin=0 is no longer emitted for freshly-written rows.
        let writer_xid = match self.runtime.current_xid() {
            Some(xid) => xid,
            None => self.runtime.snapshot_manager().allocate_committed_xid(),
        };
        entity.set_xmin(writer_xid);

        // Write. `insert_auto` internally runs preprocessors, context index,
        // and cross-ref indexing — no need to repeat them here.
        let id = self
            .store
            .insert_auto(&collection, entity)
            .map_err(|e| RedDBError::Internal(format!("{e:?}")))?;

        // Metadata
        if !row.metadata.is_empty() {
            let _ = self.store.set_metadata(
                &collection,
                id,
                Metadata::with_fields(row.metadata.into_iter().collect()),
            );
        }

        // First-insert hook: if this row carries a column named `id` and
        // no index covers `id` yet on this collection, build a HASH index
        // implicitly so subsequent `WHERE id = N` lookups hit the
        // hash-index fast path instead of the O(N) zone scan. See #112
        // and `docs/perf/delete-sequential-2026-05-06.md`.
        self.maybe_auto_index_id(&collection, &row.fields);

        // Secondary indexes (not handled by insert_auto)
        self.runtime
            .index_store_ref()
            .index_entity_insert(&collection, id, &row.fields)
            .map_err(RedDBError::Internal)?;

        // CDC + cache invalidation (once)
        let lsn = self.runtime.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &collection,
            id.raw(),
            "table",
        );
        if !self.suppress_events {
            self.runtime
                .emit_insert_events_for_collection(&collection, &[id], &[lsn])?;
        }

        Ok(MutationResult { ids: vec![id] })
    }

    // ── Kernel: batch (micro and bulk share the same code path) ──────────

    fn append_batch(
        &self,
        collection: String,
        rows: Vec<MutationRow>,
    ) -> RedDBResult<MutationResult> {
        let n = rows.len();

        // First-insert hook for #112: if any row in the batch carries an
        // `id` column and no index covers `id` yet, register a HASH
        // index up front so the `index_entity_insert_batch` pass below
        // populates it. Must run BEFORE the `has_secondary_indexes`
        // probe — otherwise the field_snapshots clone is skipped and
        // the brand-new index never sees the rows.
        if let Some(first) = rows.first() {
            self.maybe_auto_index_id(&collection, &first.fields);
        }

        // If the collection has no registered secondary indexes the whole
        // `field_snapshots.push(row.fields.clone())` work is pure waste:
        // `index_entity_insert_batch` walks the registry and would find
        // nothing to index. For a 15-column typed_insert that's 375K
        // String clones per batch → a dominant CPU line. Skip entirely
        // when we know the downstream indexer has nothing to do.
        let has_secondary_indexes = !self
            .runtime
            .index_store_ref()
            .list_indices(&collection)
            .is_empty();

        let mut field_snapshots: Vec<Vec<(String, Value)>> = if has_secondary_indexes {
            Vec::with_capacity(n)
        } else {
            Vec::new()
        };
        // Pre-scan: if no row has metadata, skip the per-row
        // `metadata_batch` vec (a Vec<(String, MetadataValue)> push +
        // index accounting for nothing). For OLTP bulk inserts the
        // common case is "rows without metadata" — the branch skips
        // 25K zero-length Vec pushes per bench typed_insert batch.
        let any_metadata = rows.iter().any(|r| !r.metadata.is_empty());
        let mut metadata_batch: Vec<Vec<(String, MetadataValue)>> = if any_metadata {
            Vec::with_capacity(n)
        } else {
            Vec::new()
        };
        let mut entities: Vec<UnifiedEntity> = Vec::with_capacity(n);

        // Resolve the writer xid once — every entity in this batch
        // shares it. Inside a tx we reuse the active xid; in
        // autocommit we draw a pool-backed pre-committed xid so every
        // row in the batch lands with a single coherent xmin (#30
        // single-statement single-xid invariant). Pool semantics match
        // the previous `begin()/commit()` pair — see `append_one`.
        let current_xid = match self.runtime.current_xid() {
            Some(xid) => xid,
            None => self.runtime.snapshot_manager().allocate_committed_xid(),
        };

        // If this collection has no context index enabled AND no row
        // carries node/vector links, the per-row `store.get(id)` in the
        // post-write maintenance loop below is wasted work — both
        // downstream indexers would no-op on the entity. Hoisting this
        // check out of the loop lets us skip N lookups on the default
        // OLTP insert path.
        let ci_enabled = self
            .store
            .context_index()
            .is_collection_enabled(&collection);
        let any_xrefs = rows
            .iter()
            .any(|r| !r.node_links.is_empty() || !r.vector_links.is_empty());
        let needs_entity_fetch = ci_enabled || any_xrefs;

        // Build the `Arc<str>` for the collection name once and clone
        // it cheaply per row. Before this the per-row path did
        // `Arc::from(collection: &str)` which allocates each time —
        // 25 000 heap allocations on a bench bulk.
        let table_arc: Arc<str> = Arc::from(collection.as_str());

        for row in rows {
            if has_secondary_indexes {
                field_snapshots.push(row.fields.clone());
            }
            if any_metadata {
                metadata_batch.push(row.metadata);
            }
            let mut entity = build_table_entity_shared(
                self.store.as_ref(),
                Arc::clone(&table_arc),
                row.fields,
                &row.node_links,
                &row.vector_links,
            );
            entity.set_xmin(current_xid);
            entities.push(entity);
        }

        // Single lock acquisition for the entire batch.
        let ids = self
            .store
            .bulk_insert(&collection, entities)
            .map_err(|e| RedDBError::Internal(format!("{e:?}")))?;

        if ids.len() != n {
            return Err(RedDBError::Internal(format!(
                "bulk_insert returned {} ids for {} rows",
                ids.len(),
                n
            )));
        }

        // Post-write maintenance — per row but without extra lock round-trips.
        // When neither metadata nor entity-fetch applies (the
        // common OLTP bulk-insert shape with no TTLs, no links, no
        // context index) the whole loop disappears, taking the
        // `ids.iter()` iteration cost with it.
        if any_metadata || needs_entity_fetch {
            for (i, &id) in ids.iter().enumerate() {
                if any_metadata && !metadata_batch[i].is_empty() {
                    let meta = Metadata::with_fields(
                        std::mem::take(&mut metadata_batch[i]).into_iter().collect(),
                    );
                    let _ = self.store.set_metadata(&collection, id, meta);
                }

                if needs_entity_fetch {
                    if let Some(entity) = self.store.get(&collection, id) {
                        if ci_enabled {
                            self.store
                                .context_index()
                                .index_entity(&collection, &entity);
                        }
                        if any_xrefs {
                            let _ = self.store.index_cross_refs(&entity, &collection);
                        }
                    }
                }
            }
        }

        // Secondary indexes: fused pass — one registry-lock acquisition
        // for the whole batch. When the collection has no registered
        // secondary indexes, we've already skipped the field_snapshots
        // clone loop above, so there's nothing to hand over.
        if has_secondary_indexes {
            let index_rows: Vec<(EntityId, Vec<(String, Value)>)> = ids
                .iter()
                .zip(field_snapshots)
                .map(|(id, fields)| (*id, fields))
                .collect();
            self.runtime
                .index_store_ref()
                .index_entity_insert_batch(&collection, &index_rows)
                .map_err(RedDBError::Internal)?;
        }

        // CDC: emit once per entity but only ONE cache invalidation for the batch.
        // Previous code called cdc_emit() per row which triggered
        // invalidate_result_cache() N times — one write-lock acquisition per row.
        self.runtime.invalidate_result_cache();
        let lsns =
            self.runtime
                .cdc_emit_insert_batch_no_cache_invalidate(&collection, &ids, "table");
        if !self.suppress_events {
            self.runtime
                .emit_insert_events_for_collection(&collection, &ids, &lsns)?;
        }

        Ok(MutationResult { ids })
    }

    // ── Implicit primary-key index (#112) ────────────────────────────────
    //
    // When the first insert into a fresh collection carries a column
    // named `id`, register a HASH index on it transparently. PG and
    // Mongo both auto-index `id` (PG via `PRIMARY KEY`, Mongo via
    // `_id`); RedDB previously did not, so `WHERE id = N` fell through
    // to a full segment scan. Per the perf research in
    // `docs/perf/delete-sequential-2026-05-06.md`, this opens a 4× gap
    // on `delete_sequential` at 10k items.
    //
    // Match is **case-sensitive `id`** to start (conservative — does
    // not auto-index `Id`, `ID`, or `red_entity_id`). The hook is a
    // no-op once any index already covers the `id` column on the
    // collection (whether auto-created here, or via explicit
    // `CREATE INDEX ... USING HASH/BTREE`).
    //
    // Opt-out: set `UnifiedStoreConfig::auto_index_id = false`.
    //
    // Build cost is zero entities here — the hook fires before the
    // first row is indexed, so the standard
    // `index_entity_insert{,_batch}` call that follows populates it.
    fn maybe_auto_index_id(&self, collection: &str, fields: &[(String, Value)]) {
        if !self.store.config().auto_index_id {
            return;
        }
        if !fields.iter().any(|(name, _)| name == "id") {
            return;
        }
        let index_store = self.runtime.index_store_ref();
        if index_store
            .find_index_for_column(collection, "id")
            .is_some()
        {
            return;
        }

        let columns = vec!["id".to_string()];
        // Build with no entities — the caller's per-row index pass
        // (`index_entity_insert` for single, `index_entity_insert_batch`
        // for batch) is what populates the index. We cannot back-fill
        // here because the rows haven't reached the segment yet.
        if let Err(err) = index_store.create_index(
            "idx_id",
            collection,
            &columns,
            super::index_store::IndexMethodKind::Hash,
            /* unique */ false,
            &[],
        ) {
            // Surface the error in tracing but don't fail the insert —
            // the row still lands; subsequent reads merely fall back to
            // the scan path. Likely cause: the underlying hash index
            // already exists from a previous registration that wasn't
            // cleaned up.
            tracing::debug!(
                target: "reddb::runtime::auto_index_id",
                collection = %collection,
                error = %err,
                "auto_index_id: failed to create implicit hash index on `id`"
            );
            return;
        }
        index_store.register(super::index_store::RegisteredIndex {
            name: "idx_id".to_string(),
            collection: collection.to_string(),
            columns,
            method: super::index_store::IndexMethodKind::Hash,
            unique: false,
        });
        // Plan-cache invalidation so subsequent SELECT/UPDATE/DELETE
        // pickers see the new index when planning. Mirrors the explicit
        // `CREATE INDEX` path in `impl_ddl::execute_create_index`.
        self.runtime.invalidate_plan_cache();
    }
}

impl crate::RedDBRuntime {
    pub(crate) fn emit_insert_events_for_collection(
        &self,
        collection: &str,
        ids: &[EntityId],
        lsns: &[u64],
    ) -> RedDBResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        if ids.len() != lsns.len() {
            return Err(RedDBError::Internal(format!(
                "insert event emission expected {} LSNs, got {}",
                ids.len(),
                lsns.len()
            )));
        }

        let Some(contract) = self.db().collection_contract_arc(collection) else {
            return Ok(());
        };
        let subscriptions = contract
            .subscriptions
            .iter()
            .filter(|subscription| {
                subscription.enabled
                    && (subscription.ops_filter.is_empty()
                        || subscription
                            .ops_filter
                            .contains(&crate::catalog::SubscriptionOperation::Insert))
            })
            .cloned()
            .collect::<Vec<_>>();
        if subscriptions.is_empty() {
            return Ok(());
        }

        let store = self.db().store();
        for (&id, &lsn) in ids.iter().zip(lsns) {
            let Some(entity) = store.get(collection, id) else {
                continue;
            };
            let after = table_row_after_json(&entity);
            for subscription in &subscriptions {
                if !entity_passes_where_filter(subscription, &entity, collection) {
                    continue;
                }
                let payload = insert_event_payload(
                    collection,
                    id.raw(),
                    lsn,
                    &after,
                    subscription.redact_fields.as_slice(),
                )?;
                self.enqueue_event_payload(&effective_queue_name(subscription), Value::Json(payload))?;
            }
        }
        Ok(())
    }

    /// Returns true when `collection` has at least one enabled DELETE subscription.
    /// Used to decide whether pre-delete snapshots need to be captured.
    pub(crate) fn collection_has_delete_subscriptions(&self, collection: &str) -> bool {
        let Some(contract) = self.db().collection_contract_arc(collection) else {
            return false;
        };
        contract.subscriptions.iter().any(|s| {
            s.enabled
                && (s.ops_filter.is_empty()
                    || s.ops_filter
                        .contains(&crate::catalog::SubscriptionOperation::Delete))
        })
    }

    /// Emit one UPDATE event per applied mutation into each matching subscription queue.
    /// `before` comes from `applied.pre_mutation_fields` and `after` from the updated entity.
    /// Only changed fields (`modified_columns`) appear in the payload.
    pub(crate) fn emit_update_events_for_collection(
        &self,
        collection: &str,
        applied: &[crate::application::entity::AppliedEntityMutation],
        lsns: &[u64],
    ) -> RedDBResult<()> {
        if applied.is_empty() {
            return Ok(());
        }

        let Some(contract) = self.db().collection_contract_arc(collection) else {
            return Ok(());
        };
        let subscriptions = contract
            .subscriptions
            .iter()
            .filter(|s| {
                s.enabled
                    && (s.ops_filter.is_empty()
                        || s.ops_filter
                            .contains(&crate::catalog::SubscriptionOperation::Update))
            })
            .cloned()
            .collect::<Vec<_>>();
        if subscriptions.is_empty() {
            return Ok(());
        }

        for (mutation, &lsn) in applied.iter().zip(lsns) {
            let before = build_update_before_json(mutation);
            let after = build_update_after_json(mutation);
            for subscription in &subscriptions {
                if !entity_passes_where_filter(subscription, &mutation.entity, collection) {
                    continue;
                }
                let payload = update_event_payload(
                    collection,
                    mutation.id.raw(),
                    lsn,
                    &before,
                    &after,
                    subscription.redact_fields.as_slice(),
                )?;
                self.enqueue_event_payload(&effective_queue_name(subscription), Value::Json(payload))?;
            }
        }
        Ok(())
    }

    /// Emit one DELETE event per deleted entity into each matching subscription queue.
    /// `pre_images` maps entity_id → the row snapshot captured before deletion.
    pub(crate) fn emit_delete_events_for_collection(
        &self,
        collection: &str,
        deleted_ids: &[EntityId],
        lsns: &[u64],
        pre_images: &std::collections::HashMap<u64, crate::json::Value>,
    ) -> RedDBResult<()> {
        if deleted_ids.is_empty() {
            return Ok(());
        }

        let Some(contract) = self.db().collection_contract_arc(collection) else {
            return Ok(());
        };
        let subscriptions = contract
            .subscriptions
            .iter()
            .filter(|s| {
                s.enabled
                    && (s.ops_filter.is_empty()
                        || s.ops_filter
                            .contains(&crate::catalog::SubscriptionOperation::Delete))
            })
            .cloned()
            .collect::<Vec<_>>();
        if subscriptions.is_empty() {
            return Ok(());
        }

        for (&id, &lsn) in deleted_ids.iter().zip(lsns) {
            let before = pre_images
                .get(&id.raw())
                .cloned()
                .unwrap_or(crate::json::Value::Null);
            for subscription in &subscriptions {
                if !json_passes_where_filter(subscription, &before) {
                    continue;
                }
                let payload = delete_event_payload(
                    collection,
                    id.raw(),
                    lsn,
                    &before,
                    subscription.redact_fields.as_slice(),
                )?;
                self.enqueue_event_payload(&effective_queue_name(subscription), Value::Json(payload))?;
            }
        }
        Ok(())
    }
}

/// Returns the effective queue name for event routing.
/// Per-tenant subscriptions are namespaced as `{tenant}__{target_queue}` when a
/// tenant context is active; cluster-wide (`all_tenants`) subscriptions always
/// use the bare queue name.
fn effective_queue_name(subscription: &crate::catalog::SubscriptionDescriptor) -> String {
    if !subscription.all_tenants {
        if let Some(tenant) = crate::runtime::impl_core::current_tenant() {
            return format!("{tenant}__{}", subscription.target_queue);
        }
    }
    subscription.target_queue.clone()
}

fn insert_event_payload(
    collection: &str,
    id: u64,
    lsn: u64,
    after: &JsonValue,
    redact_fields: &[String],
) -> RedDBResult<Vec<u8>> {
    let mut object = JsonMap::new();
    let subject_id = after
        .get("id")
        .cloned()
        .unwrap_or(JsonValue::Number(id as f64));
    let subject_id_for_hash = json_id_for_hash(&subject_id);
    object.insert(
        "event_id".to_string(),
        JsonValue::String(deterministic_event_id(
            collection,
            &subject_id_for_hash,
            lsn,
            "insert",
        )),
    );
    object.insert("op".to_string(), JsonValue::String("insert".to_string()));
    object.insert(
        "collection".to_string(),
        JsonValue::String(collection.to_string()),
    );
    object.insert("id".to_string(), subject_id);
    object.insert(
        "ts".to_string(),
        JsonValue::Number(current_unix_ms() as f64),
    );
    object.insert("lsn".to_string(), JsonValue::Number(lsn as f64));
    object.insert(
        "tenant".to_string(),
        crate::runtime::impl_core::current_tenant()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert("before".to_string(), JsonValue::Null);
    object.insert(
        "after".to_string(),
        redact_json_object(after.clone(), redact_fields),
    );
    crate::json::to_vec(&JsonValue::Object(object))
        .map_err(|err| RedDBError::Internal(format!("encode insert event payload: {err}")))
}

fn deterministic_event_id(collection: &str, id: &str, lsn: u64, op: &str) -> String {
    let mut hasher = crate::crypto::sha256::Sha256::new();
    hasher.update(collection.as_bytes());
    hasher.update(&[0]);
    hasher.update(id.as_bytes());
    hasher.update(&[0]);
    hasher.update(&lsn.to_le_bytes());
    hasher.update(op.as_bytes());
    hex::encode(hasher.finalize())
}

fn json_id_for_hash(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) => value.clone(),
        JsonValue::Number(value) => value.to_string(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Null => "null".to_string(),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            crate::json::to_string(value).unwrap_or_else(|_| "structured".to_string())
        }
    }
}

fn table_row_after_json(entity: &UnifiedEntity) -> JsonValue {
    let mut object = JsonMap::new();
    if let EntityData::Row(row) = &entity.data {
        if let Some(named) = &row.named {
            for (key, value) in named {
                object.insert(key.to_string(), storage_value_to_json(value));
            }
        } else if let Some(schema) = &row.schema {
            for (idx, column) in schema.iter().enumerate() {
                if let Some(value) = row.columns.get(idx) {
                    object.insert(column.clone(), storage_value_to_json(value));
                }
            }
        }
    }
    JsonValue::Object(object)
}

fn redact_json_object(mut value: JsonValue, redact_fields: &[String]) -> JsonValue {
    for field in redact_fields {
        redact_path(&mut value, field);
    }
    value
}

fn redact_path(value: &mut JsonValue, path: &str) {
    let (key, rest) = match path.split_once('.') {
        Some((k, r)) => (k, Some(r)),
        None => (path, None),
    };
    let JsonValue::Object(obj) = value else {
        return;
    };
    match rest {
        None => {
            // Terminal: redact this key (insert even if absent, preserving original behavior)
            obj.insert(key.to_string(), JsonValue::String("[REDACTED]".to_string()));
        }
        Some(rest) if key == "*" => {
            for child in obj.values_mut() {
                redact_path(child, rest);
            }
        }
        Some(rest) => {
            if let Some(child) = obj.get_mut(key) {
                redact_path(child, rest);
            }
        }
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ── Helper ────────────────────────────────────────────────────────────────

/// Build a `UnifiedEntity` from validated fields, mirroring `BatchBuilder::add_row`.
fn build_table_entity(
    store: &UnifiedStore,
    collection: &str,
    fields: Vec<(String, Value)>,
    node_links: &[NodeRef],
    vector_links: &[VectorRef],
) -> UnifiedEntity {
    build_table_entity_shared(
        store,
        Arc::from(collection),
        fields,
        node_links,
        vector_links,
    )
}

/// Variant that takes an already-built `Arc<str>` for the collection
/// name. Callers that batch-insert share ONE Arc across every row in
/// the batch rather than paying 25k `Arc::from(&str)` allocations.
fn build_table_entity_shared(
    store: &UnifiedStore,
    table: Arc<str>,
    fields: Vec<(String, Value)>,
    node_links: &[NodeRef],
    vector_links: &[VectorRef],
) -> UnifiedEntity {
    let id = store.next_entity_id();
    let kind = EntityKind::TableRow {
        table,
        row_id: 0, // assigned by SegmentManager::bulk_insert
    };

    // Only populate the `named` HashMap — `SegmentManager::bulk_insert`
    // converts named→columnar for the entire batch under a single
    // shared schema. The earlier path also cloned every value into
    // `columns` here, which was 15 Value clones per 15-col row =
    // 375 000 wasted clones on a 25k typed_insert bulk. Verified
    // with a follow-up bench that this dedup does NOT regress any
    // read scenario — the fake "9k select_point" was a stale-binary
    // illusion, not a real regression caused by this change.
    let mut row = RowData::new(Vec::new());
    row.named = Some(fields.into_iter().collect());

    let mut entity = UnifiedEntity::new(id, kind, EntityData::Row(row));

    for node_ref in node_links {
        entity.add_cross_ref(CrossRef::new(
            id,
            node_ref.node_id,
            node_ref.collection.clone(),
            RefType::RowToNode,
        ));
    }
    for vector_ref in vector_links {
        entity.add_cross_ref(CrossRef::new(
            id,
            vector_ref.vector_id,
            vector_ref.collection.clone(),
            RefType::RowToVector,
        ));
    }

    entity
}

// ── #293: UPDATE / DELETE event helpers ──────────────────────────────────────

/// Convert any entity's row data to a JSON object. Used for DELETE pre-images
/// and is public within the crate so `execute_delete_inner` can call it.
pub(crate) fn entity_row_json(entity: &UnifiedEntity) -> JsonValue {
    table_row_after_json(entity)
}

/// Build the `before` object for an UPDATE event: only columns listed in
/// `modified_columns` from the pre-mutation snapshot.
fn build_update_before_json(
    mutation: &crate::application::entity::AppliedEntityMutation,
) -> JsonValue {
    if mutation.modified_columns.is_empty() {
        return JsonValue::Object(JsonMap::new());
    }
    let changed: std::collections::HashSet<&str> =
        mutation.modified_columns.iter().map(String::as_str).collect();
    let mut object = JsonMap::new();
    for (key, value) in &mutation.pre_mutation_fields {
        if changed.contains(key.as_str()) {
            object.insert(key.clone(), storage_value_to_json(value));
        }
    }
    JsonValue::Object(object)
}

/// Build the `after` object for an UPDATE event: only changed columns from
/// the post-mutation entity.
fn build_update_after_json(
    mutation: &crate::application::entity::AppliedEntityMutation,
) -> JsonValue {
    if mutation.modified_columns.is_empty() {
        return JsonValue::Object(JsonMap::new());
    }
    let changed: std::collections::HashSet<&str> =
        mutation.modified_columns.iter().map(String::as_str).collect();
    let mut object = JsonMap::new();
    if let EntityData::Row(row) = &mutation.entity.data {
        if let Some(named) = &row.named {
            for (key, value) in named {
                if changed.contains(key.as_str()) {
                    object.insert(key.clone(), storage_value_to_json(value));
                }
            }
        } else if let Some(schema) = &row.schema {
            for (idx, column) in schema.iter().enumerate() {
                if changed.contains(column.as_str()) {
                    if let Some(value) = row.columns.get(idx) {
                        object.insert(column.clone(), storage_value_to_json(value));
                    }
                }
            }
        }
    }
    JsonValue::Object(object)
}

fn update_event_payload(
    collection: &str,
    id: u64,
    lsn: u64,
    before: &JsonValue,
    after: &JsonValue,
    redact_fields: &[String],
) -> RedDBResult<Vec<u8>> {
    let mut object = JsonMap::new();
    let id_json = JsonValue::Number(id as f64);
    let subject_id_for_hash = json_id_for_hash(&id_json);
    object.insert(
        "event_id".to_string(),
        JsonValue::String(deterministic_event_id(
            collection,
            &subject_id_for_hash,
            lsn,
            "update",
        )),
    );
    object.insert("op".to_string(), JsonValue::String("update".to_string()));
    object.insert(
        "collection".to_string(),
        JsonValue::String(collection.to_string()),
    );
    object.insert("id".to_string(), id_json);
    object.insert(
        "ts".to_string(),
        JsonValue::Number(current_unix_ms() as f64),
    );
    object.insert("lsn".to_string(), JsonValue::Number(lsn as f64));
    object.insert(
        "tenant".to_string(),
        crate::runtime::impl_core::current_tenant()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "before".to_string(),
        redact_json_object(before.clone(), redact_fields),
    );
    object.insert(
        "after".to_string(),
        redact_json_object(after.clone(), redact_fields),
    );
    crate::json::to_vec(&JsonValue::Object(object))
        .map_err(|err| RedDBError::Internal(format!("encode update event payload: {err}")))
}

/// Emit exactly one `truncate` event per enabled subscription.
///
/// Called from `execute_truncate` before the rows are wiped so the count is
/// still accurate. Collections without any enabled subscription are a no-op.
pub(crate) fn emit_truncate_event_for_collection(
    runtime: &crate::RedDBRuntime,
    collection: &str,
    entities_count: u64,
) -> crate::RedDBResult<()> {
    let Some(contract) = runtime.db().collection_contract_arc(collection) else {
        return Ok(());
    };
    let subscriptions: Vec<_> = contract
        .subscriptions
        .iter()
        .filter(|s| s.enabled)
        .cloned()
        .collect();
    if subscriptions.is_empty() {
        return Ok(());
    }
    let lsn = runtime.cdc_emit(
        crate::replication::cdc::ChangeOperation::Delete,
        collection,
        0,
        "truncate",
    );
    for subscription in &subscriptions {
        let payload = truncate_event_payload(collection, lsn, entities_count)?;
        runtime.enqueue_event_payload(&effective_queue_name(subscription), Value::Json(payload))?;
    }
    Ok(())
}

/// Emit exactly one `collection_dropped` event per enabled subscription.
///
/// Called from the DROP path **before** the collection storage and contract
/// are removed. The subscription entries are then stripped from the contract
/// that will be persisted (they cease to be relevant once the source is
/// gone), but the target queue is preserved so consumers can drain.
pub(crate) fn emit_collection_dropped_event_for_collection(
    runtime: &crate::RedDBRuntime,
    collection: &str,
    final_entities_count: u64,
) -> crate::RedDBResult<()> {
    let Some(contract) = runtime.db().collection_contract_arc(collection) else {
        return Ok(());
    };
    let subscriptions: Vec<_> = contract
        .subscriptions
        .iter()
        .filter(|s| s.enabled)
        .cloned()
        .collect();
    if subscriptions.is_empty() {
        return Ok(());
    }
    let lsn = runtime.cdc_emit(
        crate::replication::cdc::ChangeOperation::Delete,
        collection,
        0,
        "collection_dropped",
    );
    for subscription in &subscriptions {
        let payload = collection_dropped_event_payload(collection, lsn, final_entities_count)?;
        runtime.enqueue_event_payload(&effective_queue_name(subscription), Value::Json(payload))?;
    }
    Ok(())
}

fn truncate_event_payload(
    collection: &str,
    lsn: u64,
    entities_count: u64,
) -> RedDBResult<Vec<u8>> {
    let mut object = JsonMap::new();
    object.insert(
        "event_id".to_string(),
        JsonValue::String(deterministic_event_id(collection, "truncate", lsn, "truncate")),
    );
    object.insert("op".to_string(), JsonValue::String("truncate".to_string()));
    object.insert(
        "collection".to_string(),
        JsonValue::String(collection.to_string()),
    );
    object.insert(
        "ts".to_string(),
        JsonValue::Number(current_unix_ms() as f64),
    );
    object.insert("lsn".to_string(), JsonValue::Number(lsn as f64));
    object.insert(
        "tenant".to_string(),
        crate::runtime::impl_core::current_tenant()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "entities_count".to_string(),
        JsonValue::Number(entities_count as f64),
    );
    crate::json::to_vec(&JsonValue::Object(object))
        .map_err(|err| RedDBError::Internal(format!("encode truncate event payload: {err}")))
}

fn collection_dropped_event_payload(
    collection: &str,
    lsn: u64,
    final_entities_count: u64,
) -> RedDBResult<Vec<u8>> {
    let mut object = JsonMap::new();
    object.insert(
        "event_id".to_string(),
        JsonValue::String(deterministic_event_id(
            collection,
            "collection_dropped",
            lsn,
            "collection_dropped",
        )),
    );
    object.insert(
        "op".to_string(),
        JsonValue::String("collection_dropped".to_string()),
    );
    object.insert(
        "collection".to_string(),
        JsonValue::String(collection.to_string()),
    );
    object.insert(
        "ts".to_string(),
        JsonValue::Number(current_unix_ms() as f64),
    );
    object.insert("lsn".to_string(), JsonValue::Number(lsn as f64));
    object.insert(
        "tenant".to_string(),
        crate::runtime::impl_core::current_tenant()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "final_entities_count".to_string(),
        JsonValue::Number(final_entities_count as f64),
    );
    crate::json::to_vec(&JsonValue::Object(object))
        .map_err(|err| RedDBError::Internal(format!("encode collection_dropped event payload: {err}")))
}

// ── #297: WHERE filter evaluation ────────────────────────────────────────────

/// Parse a raw SQL WHERE predicate string into an AST `Filter`.
/// Returns `None` on parse error — callers treat that as "pass all rows".
fn parse_where_filter(sql: &str) -> Option<crate::storage::query::AstFilter> {
    crate::storage::query::Parser::new(sql)
        .ok()
        .and_then(|mut p| p.parse_filter().ok())
}

/// `true` when `entity` satisfies the subscription's `where_filter`.
/// Always `true` when no filter is configured.
fn entity_passes_where_filter(
    sub: &crate::catalog::SubscriptionDescriptor,
    entity: &crate::storage::unified::entity::UnifiedEntity,
    collection: &str,
) -> bool {
    let Some(sql) = &sub.where_filter else { return true };
    let Some(filter) = parse_where_filter(sql) else { return true };
    crate::runtime::query_exec::evaluate_entity_filter(entity, &filter, collection, collection)
}

/// `true` when the JSON pre-image satisfies the subscription's `where_filter`.
/// Used for DELETE events where we only have the serialised row snapshot.
/// Always `true` when no filter is configured.
fn json_passes_where_filter(
    sub: &crate::catalog::SubscriptionDescriptor,
    json: &JsonValue,
) -> bool {
    let Some(sql) = &sub.where_filter else { return true };
    let Some(filter) = parse_where_filter(sql) else { return true };
    eval_filter_against_json(json, &filter)
}

fn eval_filter_against_json(
    json: &JsonValue,
    filter: &crate::storage::query::AstFilter,
) -> bool {
    use crate::storage::query::AstFilter as F;
    match filter {
        F::Compare { field, op, value } => {
            let col = ast_field_col_name(field);
            let json_val = json_object_field(json, col);
            compare_json_to_store_value(json_val, value, *op)
        }
        F::And(a, b) => eval_filter_against_json(json, a) && eval_filter_against_json(json, b),
        F::Or(a, b) => eval_filter_against_json(json, a) || eval_filter_against_json(json, b),
        F::Not(f) => !eval_filter_against_json(json, f),
        F::IsNull(field) => {
            json_object_field(json, ast_field_col_name(field))
                .map_or(true, |jv| matches!(jv, JsonValue::Null))
        }
        F::IsNotNull(field) => {
            json_object_field(json, ast_field_col_name(field))
                .map_or(false, |jv| !matches!(jv, JsonValue::Null))
        }
        // CompareFields, CompareExpr, In, Between, Like, StartsWith, EndsWith,
        // Contains — permissive (let the row through).
        _ => true,
    }
}

fn ast_field_col_name(field: &crate::storage::query::FieldRef) -> &str {
    use crate::storage::query::FieldRef;
    match field {
        FieldRef::TableColumn { column, .. } => column.as_str(),
        FieldRef::NodeProperty { property, .. } => property.as_str(),
        FieldRef::EdgeProperty { property, .. } => property.as_str(),
        FieldRef::NodeId { alias } => alias.as_str(),
    }
}

fn json_object_field<'a>(json: &'a JsonValue, col: &str) -> Option<&'a JsonValue> {
    if let JsonValue::Object(map) = json {
        map.get(col)
    } else {
        None
    }
}

/// Compare a JSON field value against a storage `Value` using `op`.
/// `json` is `None` when the field is absent from the object (treated as Null).
fn compare_json_to_store_value(
    json: Option<&JsonValue>,
    rhs: &Value,
    op: crate::storage::query::CompareOp,
) -> bool {
    use crate::storage::query::CompareOp;
    use std::cmp::Ordering;

    let lhs: Value = match json {
        None | Some(JsonValue::Null) => Value::Null,
        Some(JsonValue::Bool(b)) => Value::Boolean(*b),
        Some(JsonValue::Number(n)) => {
            let n = *n;
            if n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64 {
                Value::Integer(n as i64)
            } else {
                Value::Float(n)
            }
        }
        Some(JsonValue::String(s)) => Value::text(s.clone()),
        Some(JsonValue::Array(_) | JsonValue::Object(_)) => return false,
    };

    match op {
        CompareOp::Eq => lhs == *rhs,
        CompareOp::Ne => lhs != *rhs,
        _ => {
            fn val_to_f64(v: &Value) -> Option<f64> {
                match v {
                    Value::Integer(i) => Some(*i as f64),
                    Value::UnsignedInteger(u) => Some(*u as f64),
                    Value::Float(f) => Some(*f),
                    _ => None,
                }
            }
            let Some(lf) = val_to_f64(&lhs) else { return false };
            let Some(rf) = val_to_f64(rhs) else { return false };
            let ord = lf.partial_cmp(&rf).unwrap_or(Ordering::Equal);
            match op {
                CompareOp::Lt => ord == Ordering::Less,
                CompareOp::Le => ord != Ordering::Greater,
                CompareOp::Gt => ord == Ordering::Greater,
                CompareOp::Ge => ord != Ordering::Less,
                _ => false,
            }
        }
    }
}

fn delete_event_payload(
    collection: &str,
    id: u64,
    lsn: u64,
    before: &JsonValue,
    redact_fields: &[String],
) -> RedDBResult<Vec<u8>> {
    let mut object = JsonMap::new();
    let id_json = JsonValue::Number(id as f64);
    let subject_id_for_hash = json_id_for_hash(&id_json);
    object.insert(
        "event_id".to_string(),
        JsonValue::String(deterministic_event_id(
            collection,
            &subject_id_for_hash,
            lsn,
            "delete",
        )),
    );
    object.insert("op".to_string(), JsonValue::String("delete".to_string()));
    object.insert(
        "collection".to_string(),
        JsonValue::String(collection.to_string()),
    );
    object.insert("id".to_string(), id_json);
    object.insert(
        "ts".to_string(),
        JsonValue::Number(current_unix_ms() as f64),
    );
    object.insert("lsn".to_string(), JsonValue::Number(lsn as f64));
    object.insert(
        "tenant".to_string(),
        crate::runtime::impl_core::current_tenant()
            .map(JsonValue::String)
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "before".to_string(),
        redact_json_object(before.clone(), redact_fields),
    );
    object.insert("after".to_string(), JsonValue::Null);
    crate::json::to_vec(&JsonValue::Object(object))
        .map_err(|err| RedDBError::Internal(format!("encode delete event payload: {err}")))
}
