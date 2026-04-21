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

        // MVCC xmin stamping (Phase 2.3.2a PG parity).
        //
        // When this INSERT runs inside a BEGIN-wrapped transaction, stamp
        // the tuple with the current xid so other snapshots hide it until
        // the transaction commits. Outside a transaction `current_xid()`
        // returns `None` and xmin stays at 0 — the pre-MVCC "always
        // visible" default preserved for backwards compatibility.
        if let Some(xid) = self.runtime.current_xid() {
            entity.set_xmin(xid);
        }

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

        // Secondary indexes (not handled by insert_auto)
        self.runtime
            .index_store_ref()
            .index_entity_insert(&collection, id, &row.fields)
            .map_err(RedDBError::Internal)?;

        // CDC + cache invalidation (once)
        self.runtime.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &collection,
            id.raw(),
            "table",
        );

        Ok(MutationResult { ids: vec![id] })
    }

    // ── Kernel: batch (micro and bulk share the same code path) ──────────

    fn append_batch(
        &self,
        collection: String,
        rows: Vec<MutationRow>,
    ) -> RedDBResult<MutationResult> {
        let n = rows.len();

        // Separate fields for index maintenance before moving into entities.
        let mut field_snapshots: Vec<Vec<(String, Value)>> = Vec::with_capacity(n);
        let mut metadata_batch: Vec<Vec<(String, MetadataValue)>> = Vec::with_capacity(n);
        let mut entities: Vec<UnifiedEntity> = Vec::with_capacity(n);

        // Resolve the current transaction xid once — every entity in this
        // batch shares it. `None` (autocommit) leaves xmin at 0, which
        // the visibility checker treats as "always visible" (pre-MVCC).
        let current_xid = self.runtime.current_xid();

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

        for row in rows {
            field_snapshots.push(row.fields.clone());
            metadata_batch.push(row.metadata);
            let mut entity = build_table_entity(
                self.store.as_ref(),
                &collection,
                row.fields,
                &row.node_links,
                &row.vector_links,
            );
            if let Some(xid) = current_xid {
                entity.set_xmin(xid);
            }
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
        for (i, &id) in ids.iter().enumerate() {
            // Metadata
            if !metadata_batch[i].is_empty() {
                let meta = Metadata::with_fields(
                    std::mem::take(&mut metadata_batch[i]).into_iter().collect(),
                );
                let _ = self.store.set_metadata(&collection, id, meta);
            }

            // Context index + cross-refs (still per-row; these APIs
            // don't expose a batch form yet). Skip the lookup entirely
            // when the hoisted check says neither path has work to do —
            // the default OLTP insert_bulk shape (no links, no context
            // index) avoids N `store.get` walks this way.
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

        // Secondary indexes: fused pass — one registry-lock acquisition
        // for the whole batch instead of N (see `index_entity_insert_batch`
        // in `runtime::index_store`). This is the P4.T2 win.
        let index_rows: Vec<(EntityId, Vec<(String, Value)>)> = ids
            .iter()
            .zip(field_snapshots.iter().cloned())
            .map(|(id, fields)| (*id, fields))
            .collect();
        self.runtime
            .index_store_ref()
            .index_entity_insert_batch(&collection, &index_rows)
            .map_err(RedDBError::Internal)?;

        // CDC: emit once per entity but only ONE cache invalidation for the batch.
        // Previous code called cdc_emit() per row which triggered
        // invalidate_result_cache() N times — one write-lock acquisition per row.
        self.runtime.invalidate_result_cache();
        for &id in &ids {
            self.runtime.cdc_emit_no_cache_invalidate(
                crate::replication::cdc::ChangeOperation::Insert,
                &collection,
                id.raw(),
                "table",
            );
        }

        Ok(MutationResult { ids })
    }
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
    let id = store.next_entity_id();
    let kind = EntityKind::TableRow {
        table: Arc::from(collection),
        row_id: 0, // assigned by SegmentManager::bulk_insert
    };

    let mut row = RowData::new(fields.iter().map(|(_, v)| v.clone()).collect());
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
