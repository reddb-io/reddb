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
                .zip(field_snapshots.into_iter())
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
        self.runtime
            .cdc_emit_insert_batch_no_cache_invalidate(&collection, &ids, "table");

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
