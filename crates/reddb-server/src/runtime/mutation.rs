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
}

impl<'rt> MutationEngine<'rt> {
    pub(crate) fn new(runtime: &'rt crate::RedDBRuntime) -> Self {
        let store = runtime.db().store();
        Self { runtime, store }
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
        self.runtime
            .emit_insert_events_for_collection(&collection, &[id], &[lsn])?;

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
        self.runtime
            .emit_insert_events_for_collection(&collection, &ids, &lsns)?;

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
                let payload = insert_event_payload(
                    collection,
                    id.raw(),
                    lsn,
                    &after,
                    subscription.redact_fields.as_slice(),
                )?;
                self.enqueue_event_payload(&subscription.target_queue, Value::Json(payload))?;
            }
        }
        Ok(())
    }
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
    if let JsonValue::Object(object) = &mut value {
        for field in redact_fields {
            object.insert(field.clone(), JsonValue::String("[REDACTED]".to_string()));
        }
    }
    value
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
