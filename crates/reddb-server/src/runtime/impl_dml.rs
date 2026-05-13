//! DML execution: INSERT, UPDATE, DELETE via SQL AST
//!
//! Implements `execute_insert`, `execute_update`, and `execute_delete` on
//! `RedDBRuntime`.  Each method translates the parsed AST into entity-level
//! operations through the existing `RuntimeEntityPort` trait so that all
//! cross-cutting concerns (WAL, indexing, replication) are automatically
//! applied.

use crate::application::entity::{
    metadata_from_json, AppliedEntityMutation, CreateDocumentInput, CreateEdgeInput,
    CreateKvInput, CreateNodeInput, CreateRowInput, CreateRowsBatchInput, CreateVectorInput,
    DeleteEntityInput,
    PatchEntityOperation, PatchEntityOperationType, RowUpdateColumnRule, RowUpdateContractPlan,
};
use crate::application::ports::{
    build_row_update_contract_plan, normalize_row_update_assignment_with_plan,
    normalize_row_update_value_for_rule, RuntimeEntityPort,
};
use crate::application::ttl_payload::has_internal_ttl_metadata;
use crate::presentation::entity_json::storage_value_to_json;
use crate::storage::query::ast::{Expr, ReturningItem};
use crate::storage::query::sql_lowering::{
    effective_delete_filter, effective_insert_rows, effective_update_filter, fold_expr_to_value,
};
use crate::storage::query::unified::{sys_key_red_entity_id, UnifiedRecord, UnifiedResult};
use crate::storage::unified::MetadataValue;
use crate::storage::Metadata;
use std::collections::HashMap;
use std::sync::Arc;

use super::*;

const UPDATE_APPLY_CHUNK_SIZE: usize = 2048;
const TREE_CHILD_EDGE_LABEL: &str = "TREE_CHILD";
const TREE_METADATA_PREFIX: &str = "red.tree.";

#[derive(Clone)]
struct CompiledUpdateAssignment {
    column: String,
    expr: Expr,
    metadata_key: Option<&'static str>,
    row_rule: Option<RowUpdateColumnRule>,
}

struct CompiledUpdatePlan {
    static_field_assignments: Vec<(String, Value)>,
    static_metadata_assignments: Vec<(String, MetadataValue)>,
    dynamic_assignments: Vec<CompiledUpdateAssignment>,
    row_contract_plan: Option<RowUpdateContractPlan>,
    row_modified_columns: Vec<String>,
    row_touches_unique_columns: bool,
}

#[derive(Default)]
struct MaterializedUpdateAssignments {
    dynamic_field_assignments: Vec<(String, Value)>,
    dynamic_metadata_assignments: Vec<(String, MetadataValue)>,
}

impl RedDBRuntime {
    /// Phase 2.5.4: inject `CURRENT_TENANT()` into an INSERT when the
    /// target table is tenant-scoped and the user's column list does
    /// not already name the tenant column.
    ///
    /// Returns:
    /// * `Ok(None)` — no injection needed (non-tenant table, or user
    ///   supplied the column explicitly). Caller uses the original
    ///   query unchanged.
    /// * `Ok(Some(augmented))` — a cloned query with the tenant column
    ///   + literal value appended to every row.
    /// * `Err(..)` — table is tenant-scoped but no tenant is bound to
    ///   the current session. Fails loudly so callers don't produce
    ///   rows that RLS would then hide on read.
    fn maybe_inject_tenant_column(&self, query: &InsertQuery) -> RedDBResult<Option<InsertQuery>> {
        let Some(tenant_col) = self.tenant_column(&query.table) else {
            return Ok(None);
        };
        // User already named the column (literal match) — trust them.
        if query
            .columns
            .iter()
            .any(|c| c.eq_ignore_ascii_case(&tenant_col))
        {
            return Ok(None);
        }

        // Phase 2 PG parity: dotted-path tenancy. When `tenant_col` is a
        // nested key like `headers.tenant` we operate on the root
        // column (`headers`) and set / add the nested path inside its
        // JSON value. If the user named the root column we mutate in
        // place; otherwise we create a fresh JSON column for every row.
        if let Some(dot_pos) = tenant_col.find('.') {
            let (root, tail) = tenant_col.split_at(dot_pos);
            let tail = &tail[1..]; // drop leading '.'
            return self.inject_dotted_tenant(query, root, tail);
        }

        let Some(tenant_id) = crate::runtime::impl_core::current_tenant() else {
            return Err(RedDBError::Query(format!(
                "INSERT into tenant-scoped table '{}' requires an active tenant — \
                 run SET TENANT '<id>' first or name column '{}' explicitly",
                query.table, tenant_col
            )));
        };

        let mut augmented = query.clone();
        augmented.columns.push(tenant_col);
        let lit = Value::text(tenant_id.clone());
        for row in augmented.values.iter_mut() {
            row.push(lit.clone());
        }
        for row in augmented.value_exprs.iter_mut() {
            row.push(crate::storage::query::ast::Expr::Literal {
                value: lit.clone(),
                span: crate::storage::query::ast::Span::synthetic(),
            });
        }
        Ok(Some(augmented))
    }

    /// Dotted-path auto-fill — set `root.tail` to `CURRENT_TENANT()` on
    /// every row. Mirrors `maybe_inject_tenant_column` but mutates
    /// nested JSON instead of appending a flat column.
    ///
    /// Cases:
    /// * Root column already in the INSERT list → mutate per-row JSON
    ///   (parse, set path, re-serialize).
    /// * Root column absent → create a fresh `{tail: tenant}` JSON
    ///   object and append the root column to the INSERT.
    fn inject_dotted_tenant(
        &self,
        query: &InsertQuery,
        root: &str,
        tail: &str,
    ) -> RedDBResult<Option<InsertQuery>> {
        let active_tenant = crate::runtime::impl_core::current_tenant();
        let mut augmented = query.clone();
        let root_idx = augmented
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(root));

        if let Some(idx) = root_idx {
            // User supplied the root column. Per-row: if the dotted
            // tail is already present we trust the user (admin / bulk
            // loader scenario); otherwise fill from the active
            // tenant. An unbound tenant is only an error when some
            // row actually needs filling.
            for row in augmented.values.iter_mut() {
                let Some(slot) = row.get_mut(idx) else {
                    continue;
                };
                if dotted_tail_already_set(slot, tail) {
                    continue;
                }
                let Some(tenant_id) = &active_tenant else {
                    return Err(RedDBError::Query(format!(
                        "INSERT into tenant-scoped table '{}' requires an active tenant — \
                         run SET TENANT '<id>' first or set '{}.{}' explicitly in each row",
                        query.table, root, tail
                    )));
                };
                *slot = merge_dotted_tenant(slot.clone(), tail, tenant_id)?;
            }
            // Expression row is kept in sync by re-wrapping the
            // mutated literal; the canonical path will re-evaluate
            // against the same JSON shape.
            for (row_idx, row) in augmented.value_exprs.iter_mut().enumerate() {
                if let Some(slot) = row.get_mut(idx) {
                    let new_value = augmented
                        .values
                        .get(row_idx)
                        .and_then(|v| v.get(idx))
                        .cloned()
                        .unwrap_or(Value::Null);
                    *slot = crate::storage::query::ast::Expr::Literal {
                        value: new_value,
                        span: crate::storage::query::ast::Span::synthetic(),
                    };
                }
            }
        } else {
            // No root column in the INSERT list — auto-fill needs a
            // bound tenant to synthesise one. Error loud so we never
            // create a tenant-less row that RLS would then hide.
            let Some(tenant_id) = &active_tenant else {
                return Err(RedDBError::Query(format!(
                    "INSERT into tenant-scoped table '{}' requires an active tenant — \
                     run SET TENANT '<id>' first or name path '{}.{}' explicitly",
                    query.table, root, tail
                )));
            };
            // Create a fresh JSON column with only the tenant path set.
            augmented.columns.push(root.to_string());
            let fresh = merge_dotted_tenant(Value::Null, tail, tenant_id)?;
            for row in augmented.values.iter_mut() {
                row.push(fresh.clone());
            }
            for row in augmented.value_exprs.iter_mut() {
                row.push(crate::storage::query::ast::Expr::Literal {
                    value: fresh.clone(),
                    span: crate::storage::query::ast::Span::synthetic(),
                });
            }
        }

        Ok(Some(augmented))
    }

    /// Returns `(affected_count, lsns)`. For the txn (xmax-stamp) path,
    /// `lsns` is empty because events fire at commit time.
    fn delete_entities_batch(
        &self,
        collection: &str,
        ids: &[EntityId],
    ) -> RedDBResult<(u64, Vec<u64>)> {
        if ids.is_empty() {
            return Ok((0, vec![]));
        }

        let store = self.db().store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok((0, vec![]));
        };

        let active_xid = self.current_xid();
        let conn_id = crate::runtime::impl_core::current_connection_id();
        let mut autocommit_xid = None;
        let mut tombstoned_ids = Vec::new();
        let mut tombstoned_entities = Vec::new();
        let mut physical_delete_ids = Vec::new();

        for &id in ids {
            let Some(mut entity) = manager.get(id) else {
                continue;
            };
            if matches!(entity.data, EntityData::Row(_)) {
                let previous_xmax = entity.xmax;
                // Skip if this tuple was already tombstoned by a prior
                // statement in the same txn — idempotent DELETE.
                if entity.xmax != 0 {
                    continue;
                }

                let xid = match active_xid {
                    Some(xid) => xid,
                    None => match autocommit_xid {
                        Some(xid) => xid,
                        None => {
                            let mgr = self.snapshot_manager();
                            let xid = mgr.begin();
                            autocommit_xid = Some(xid);
                            xid
                        }
                    },
                };
                entity.set_xmax(xid);
                if manager.update(entity.clone()).is_ok() {
                    if active_xid.is_some() {
                        self.record_pending_tombstone(conn_id, collection, id, xid, previous_xmax);
                    }
                    tombstoned_entities.push(entity);
                    tombstoned_ids.push(id);
                }
            } else {
                physical_delete_ids.push(id);
            }
        }

        if let Some(xid) = autocommit_xid {
            self.snapshot_manager().commit(xid);
        }

        let mut affected = tombstoned_ids.len() as u64;
        let mut lsns = Vec::with_capacity(tombstoned_ids.len() + physical_delete_ids.len());
        if active_xid.is_some() {
            store
                .persist_entities_to_pager(collection, &tombstoned_entities)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        } else {
            store
                .persist_entities_to_pager(collection, &tombstoned_entities)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            for id in &tombstoned_ids {
                store.context_index().remove_entity(*id);
                let lsn = self.cdc_emit(
                    crate::replication::cdc::ChangeOperation::Delete,
                    collection,
                    id.raw(),
                    "entity",
                );
                lsns.push(lsn);
            }
        }

        let deleted_ids = store
            .delete_batch(collection, &physical_delete_ids)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        affected += deleted_ids.len() as u64;
        for id in &deleted_ids {
            store.context_index().remove_entity(*id);
            let lsn = self.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                collection,
                id.raw(),
                "entity",
            );
            lsns.push(lsn);
        }

        Ok((affected, lsns))
    }

    /// Flushes context-index updates and CDC for each applied mutation.
    /// Returns one LSN per entity in the same order as `applied`.
    fn flush_update_chunk(&self, applied: &[AppliedEntityMutation]) -> Vec<u64> {
        if applied.is_empty() {
            return Vec::new();
        }

        let store = self.db().store();
        if applied.iter().any(|item| item.context_index_dirty) {
            store.context_index().index_entities(
                &applied[0].collection,
                applied
                    .iter()
                    .filter(|item| item.context_index_dirty)
                    .map(|item| &item.entity),
            );
        }

        let mut lsns = Vec::with_capacity(applied.len());
        for item in applied {
            let lsn = self.cdc_emit_prebuilt(
                crate::replication::cdc::ChangeOperation::Update,
                &item.collection,
                &item.entity,
                "entity",
                item.metadata.as_ref(),
                false,
            );
            lsns.push(lsn);
        }
        lsns
    }

    fn persist_update_chunk(&self, applied: &[AppliedEntityMutation]) -> RedDBResult<()> {
        self.persist_applied_entity_mutations(applied)
    }

    /// Execute INSERT INTO table [entity_type] (cols) VALUES (vals), ...
    ///
    /// Each row in `query.values` is zipped with `query.columns` to produce a
    /// set of named fields, which is then dispatched based on entity_type.
    pub fn execute_insert(
        &self,
        raw_query: &str,
        query: &InsertQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        // CollectionContract gate (#49): single entry point for the
        // operator's collection-level write rules. Today this is a
        // no-op for INSERT (APPEND ONLY permits insert); routing
        // through the gate now means future contract bits — versioned,
        // vault-only writes — plug in once instead of per verb.
        crate::runtime::collection_contract::CollectionContractGate::check(
            self,
            &query.table,
            crate::runtime::collection_contract::MutationKind::Insert,
        )?;
        // Phase 2.5.4 table-scoped tenancy: if the target table is
        // tenant-scoped and the user didn't name the tenant column,
        // auto-inject it with the thread-local `CURRENT_TENANT()`
        // value. When the column is named explicitly we trust the
        // caller (useful for admin tooling that writes on behalf of
        // specific tenants). An unbound tenant on an implicit-fill
        // path errors up front rather than producing a row the RLS
        // policy would silently hide.
        let augmented_owned;
        let query = match self.maybe_inject_tenant_column(query)? {
            Some(new_q) => {
                augmented_owned = new_q;
                &augmented_owned
            }
            None => query,
        };
        self.check_insert_column_policy(query)?;

        let mut inserted_count: u64 = 0;
        let effective_rows =
            effective_insert_rows(query).map_err(|msg| RedDBError::Query(msg.to_string()))?;

        // Ensure the collection exists (auto-create on first insert).
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(&query.table);
        let declared_model = self
            .db()
            .collection_contract_arc(&query.table)
            .map(|contract| contract.declared_model);

        let mut returning_snapshots: Option<Vec<Vec<(String, Value)>>> =
            if query.returning.is_some() {
                Some(Vec::with_capacity(effective_rows.len()))
            } else {
                None
            };
        let mut returning_result: Option<UnifiedResult> = None;

        if matches!(query.entity_type, InsertEntityType::Row)
            && !matches!(
                declared_model,
                Some(crate::catalog::CollectionModel::TimeSeries)
            )
        {
            let mut rows = Vec::with_capacity(effective_rows.len());
            for row_values in &effective_rows {
                if row_values.len() != query.columns.len() {
                    return Err(RedDBError::Query(format!(
                        "INSERT column count ({}) does not match value count ({})",
                        query.columns.len(),
                        row_values.len()
                    )));
                }
                let (fields, mut metadata) =
                    split_insert_metadata(self, &query.columns, row_values)?;
                merge_with_clauses(
                    &mut metadata,
                    query.ttl_ms,
                    query.expires_at_ms,
                    &query.with_metadata,
                );
                if let Some(snaps) = returning_snapshots.as_mut() {
                    snaps.push(fields.clone());
                }
                rows.push(CreateRowInput {
                    collection: query.table.clone(),
                    fields,
                    metadata,
                    node_links: Vec::new(),
                    vector_links: Vec::new(),
                });
            }
            let outputs = self.create_rows_batch(CreateRowsBatchInput {
                collection: query.table.clone(),
                rows,
                suppress_events: query.suppress_events,
            })?;
            inserted_count = outputs.len() as u64;

            // Hypertable chunk routing: if this table was declared via
            // CREATE HYPERTABLE, register each row's time-column value
            // with the registry so chunk metadata (bounds, row counts,
            // TTL eligibility) stays current. This is what lets
            // HYPERTABLE_PRUNE_CHUNKS answer real questions + lets the
            // retention daemon sweep expired chunks without scanning
            // every row.
            if let Some(spec) = self.inner.db.hypertables().get(&query.table) {
                let time_col = &spec.time_column;
                // Find the column's index in the INSERT column list.
                if let Some(idx) = query.columns.iter().position(|c| c == time_col) {
                    for row in &effective_rows {
                        if let Some(Value::Integer(n) | Value::BigInt(n)) = row.get(idx) {
                            if *n >= 0 {
                                let _ = self.inner.db.hypertables().route(&query.table, *n as u64);
                            }
                        } else if let Some(Value::UnsignedInteger(n)) = row.get(idx) {
                            let _ = self.inner.db.hypertables().route(&query.table, *n);
                        }
                    }
                }
            }

            if let (Some(items), Some(snaps)) =
                (query.returning.as_ref(), returning_snapshots.take())
            {
                returning_result = Some(build_returning_result(items, &snaps, Some(&outputs)));
            }
        } else {
            // Issue #419: surface the inserted entity id on every INSERT path.
            // For Node/Edge/Vector/Document/Kv we now keep each CreateEntityOutput
            // so a RETURNING clause (and the unconditional inserted_ids list,
            // below) can expose the engine-assigned id. TimeSeries (the row
            // branch in this else) still returns the not-supported error
            // because create_timeseries_point isn't plumbed through this fn.
            let mut entity_outputs: Vec<crate::application::entity::CreateEntityOutput> =
                Vec::with_capacity(effective_rows.len());
            let mut returning_field_snaps: Vec<Vec<(String, Value)>> = if query.returning.is_some()
            {
                Vec::with_capacity(effective_rows.len())
            } else {
                Vec::new()
            };
            if matches!(
                query.entity_type,
                InsertEntityType::Node | InsertEntityType::Edge
            ) {
                enum PreparedGraphInsert {
                    Node {
                        fields: Vec<(String, Value)>,
                        input: CreateNodeInput,
                    },
                    Edge {
                        fields: Vec<(String, Value)>,
                        input: CreateEdgeInput,
                    },
                }

                let mut prepared = Vec::with_capacity(effective_rows.len());
                for row_values in &effective_rows {
                    if row_values.len() != query.columns.len() {
                        return Err(RedDBError::Query(format!(
                            "INSERT column count ({}) does not match value count ({})",
                            query.columns.len(),
                            row_values.len()
                        )));
                    }

                    match query.entity_type {
                        InsertEntityType::Node => {
                            let (node_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            ensure_non_tree_reserved_metadata_entries(&metadata)?;
                            apply_collection_default_ttl_metadata(
                                self,
                                &query.table,
                                &mut metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&node_values);
                            let label = find_column_value_string(&columns, &values, "label")?;
                            let node_type =
                                find_column_value_opt_string(&columns, &values, "node_type");
                            let properties = extract_remaining_properties(
                                &columns,
                                &values,
                                &["label", "node_type"],
                            );
                            prepared.push(PreparedGraphInsert::Node {
                                fields: node_values,
                                input: CreateNodeInput {
                                    collection: query.table.clone(),
                                    label,
                                    node_type,
                                    properties,
                                    metadata,
                                    embeddings: Vec::new(),
                                    table_links: Vec::new(),
                                    node_links: Vec::new(),
                                },
                            });
                        }
                        InsertEntityType::Edge => {
                            let (edge_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            ensure_non_tree_reserved_metadata_entries(&metadata)?;
                            apply_collection_default_ttl_metadata(
                                self,
                                &query.table,
                                &mut metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&edge_values);
                            let label = find_column_value_string(&columns, &values, "label")?;
                            ensure_non_tree_structural_edge_label(&label)?;
                            let from_id = resolve_edge_endpoint(
                                self.inner.db.store().as_ref(),
                                &query.table,
                                &columns,
                                &values,
                                "from",
                            )?;
                            let to_id = resolve_edge_endpoint(
                                self.inner.db.store().as_ref(),
                                &query.table,
                                &columns,
                                &values,
                                "to",
                            )?;
                            let weight = find_column_value_f32_opt(&columns, &values, "weight");
                            let properties = extract_remaining_properties(
                                &columns,
                                &values,
                                &["label", "from", "to", "weight"],
                            );
                            prepared.push(PreparedGraphInsert::Edge {
                                fields: edge_values,
                                input: CreateEdgeInput {
                                    collection: query.table.clone(),
                                    label,
                                    from: EntityId::new(from_id),
                                    to: EntityId::new(to_id),
                                    weight,
                                    properties,
                                    metadata,
                                },
                            });
                        }
                        _ => unreachable!("prepared graph insert only handles NODE and EDGE"),
                    }
                }

                ensure_graph_insert_contract(self, &query.table)?;
                let mut batch = self.inner.db.batch();
                for item in prepared {
                    match item {
                        PreparedGraphInsert::Node { fields, input } => {
                            if query.returning.is_some() {
                                returning_field_snaps.push(fields);
                            }
                            let node_type = input.node_type.unwrap_or_else(|| input.label.clone());
                            batch = batch.add_node_with_type(
                                input.collection,
                                input.label,
                                node_type,
                                input.properties.into_iter().collect(),
                                input.metadata.into_iter().collect(),
                            );
                        }
                        PreparedGraphInsert::Edge { fields, input } => {
                            if query.returning.is_some() {
                                returning_field_snaps.push(fields);
                            }
                            batch = batch.add_edge(
                                input.collection,
                                input.label,
                                input.from,
                                input.to,
                                input.weight.unwrap_or(1.0),
                                input.properties.into_iter().collect(),
                                input.metadata.into_iter().collect(),
                            );
                        }
                    }
                }
                let batch_result = batch
                    .execute()
                    .map_err(|err| RedDBError::Internal(format!("{err:?}")))?;
                let (ids, entity_kind) = match query.entity_type {
                    InsertEntityType::Node => (batch_result.nodes, "graph_node"),
                    InsertEntityType::Edge => (batch_result.edges, "graph_edge"),
                    _ => unreachable!("prepared graph insert only handles NODE and EDGE"),
                };
                for id in &ids {
                    self.stamp_xmin_if_in_txn(&query.table, *id);
                }
                self.cdc_emit_insert_batch_no_cache_invalidate(&query.table, &ids, entity_kind);
                entity_outputs.extend(ids.iter().map(|id| {
                    crate::application::entity::CreateEntityOutput {
                        id: *id,
                        entity: None,
                    }
                }));
                inserted_count = ids.len() as u64;
            } else {
                for row_values in &effective_rows {
                    if row_values.len() != query.columns.len() {
                        return Err(RedDBError::Query(format!(
                            "INSERT column count ({}) does not match value count ({})",
                            query.columns.len(),
                            row_values.len()
                        )));
                    }

                    match query.entity_type {
                        InsertEntityType::Row => {
                            if query.returning.is_some() {
                                return Err(RedDBError::Query(
                                "RETURNING is not yet supported for this INSERT path (TimeSeries)"
                                    .to_string(),
                            ));
                            }
                            let (fields, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            self.insert_timeseries_point(&query.table, fields, metadata)?;
                        }
                        InsertEntityType::Node | InsertEntityType::Edge => {
                            unreachable!("NODE and EDGE are handled by the prepared graph path")
                        }
                        InsertEntityType::Vector => {
                            let (vector_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&vector_values);
                            let dense = find_column_value_vec_f32_any(
                                &columns,
                                &values,
                                &["dense", "embedding"],
                            )?;
                            merge_vector_metadata_column(&mut metadata, &columns, &values)?;
                            let content =
                                find_column_value_opt_string(&columns, &values, "content");
                            if query.returning.is_some() {
                                returning_field_snaps.push(vector_values.clone());
                            }
                            let input = CreateVectorInput {
                                collection: query.table.clone(),
                                dense,
                                content,
                                metadata,
                                link_row: None,
                                link_node: None,
                            };
                            entity_outputs.push(self.create_vector(input)?);
                        }
                        InsertEntityType::Document => {
                            let (document_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&document_values);
                            let body_str = find_column_value_string(&columns, &values, "body")?;
                            let body: crate::json::Value = crate::json::from_str(&body_str)
                                .map_err(|e| {
                                    RedDBError::Query(format!("invalid JSON body: {e}"))
                                })?;
                            if query.returning.is_some() {
                                returning_field_snaps.push(document_values.clone());
                            }
                            let input = CreateDocumentInput {
                                collection: query.table.clone(),
                                body,
                                metadata,
                                node_links: Vec::new(),
                                vector_links: Vec::new(),
                            };
                            entity_outputs.push(self.create_document(input)?);
                        }
                        InsertEntityType::Kv => {
                            let (kv_values, mut metadata) =
                                split_insert_metadata(self, &query.columns, row_values)?;
                            merge_with_clauses(
                                &mut metadata,
                                query.ttl_ms,
                                query.expires_at_ms,
                                &query.with_metadata,
                            );
                            let (columns, values) = pairwise_columns_values(&kv_values);
                            let key = find_column_value_string(&columns, &values, "key")?;
                            let value = find_column_value(&columns, &values, "value")?;
                            if query.returning.is_some() {
                                returning_field_snaps.push(kv_values.clone());
                            }
                            let input = CreateKvInput {
                                collection: query.table.clone(),
                                key,
                                value,
                                metadata,
                            };
                            entity_outputs.push(self.create_kv(input)?);
                        }
                    }

                    inserted_count += 1;
                }
            }

            if let Some(items) = query.returning.as_ref() {
                if !entity_outputs.is_empty() {
                    returning_result = Some(build_returning_result(
                        items,
                        &returning_field_snaps,
                        Some(&entity_outputs),
                    ));
                }
            }
        }

        // Auto-embed pipeline: batch-embed fields across all inserted rows via AiBatchClient.
        if let Some(ref embed_config) = query.auto_embed {
            let store = self.inner.db.store();
            let provider = crate::ai::parse_provider(&embed_config.provider)?;
            let api_key = crate::ai::resolve_api_key_from_runtime(&provider, None, self)?;
            let model = embed_config.model.clone().unwrap_or_else(|| {
                std::env::var("REDDB_OPENAI_EMBEDDING_MODEL")
                    .ok()
                    .unwrap_or_else(|| crate::ai::DEFAULT_OPENAI_EMBEDDING_MODEL.to_string())
            });

            // Collect the just-inserted rows (most-recently appended, reversed back to insert order).
            let manager = store
                .get_collection(&query.table)
                .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
            let entities = manager.query_all(|_| true);
            let recent: Vec<_> = entities
                .into_iter()
                .rev()
                .take(effective_rows.len())
                .collect();

            // Collector phase: (entity_index, combined_text) for rows that have non-empty fields.
            let entity_combos: Vec<(usize, String)> = recent
                .iter()
                .enumerate()
                .filter_map(|(i, entity)| {
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            let texts: Vec<String> = embed_config
                                .fields
                                .iter()
                                .filter_map(|field| match named.get(field) {
                                    Some(Value::Text(t)) if !t.is_empty() => Some(t.to_string()),
                                    _ => None,
                                })
                                .collect();
                            if !texts.is_empty() {
                                return Some((i, texts.join(" ")));
                            }
                        }
                    }
                    None
                })
                .collect();

            if !entity_combos.is_empty() {
                // Batch phase: single provider round-trip for all rows.
                let batch_texts: Vec<String> =
                    entity_combos.iter().map(|(_, t)| t.clone()).collect();

                let batch_client =
                    crate::runtime::ai::batch_client::AiBatchClient::from_runtime(self);

                let embeddings = match tokio::runtime::Handle::try_current() {
                    Ok(handle) => tokio::task::block_in_place(|| {
                        handle.block_on(batch_client.embed_batch(
                            &provider,
                            &model,
                            &api_key,
                            batch_texts,
                        ))
                    }),
                    Err(_) => {
                        return Err(RedDBError::Query(
                            "AUTO EMBED requires a Tokio runtime context".to_string(),
                        ));
                    }
                }
                .map_err(|e| RedDBError::Query(e.to_string()))?;

                // Distribute phase: persist one vector per non-empty embedding.
                for ((_, combined), dense) in entity_combos.iter().zip(embeddings) {
                    if dense.is_empty() {
                        continue;
                    }
                    self.create_vector(CreateVectorInput {
                        collection: query.table.clone(),
                        dense,
                        content: Some(combined.clone()),
                        metadata: Vec::new(),
                        link_row: None,
                        link_node: None,
                    })?;
                }
            }
        }

        if inserted_count > 0 {
            self.note_table_write(&query.table);
        }

        let mut result = RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            inserted_count,
            "insert",
            "runtime-dml",
        );
        if let Some(returning) = returning_result {
            result.result = returning;
        }
        Ok(result)
    }

    fn check_insert_column_policy(&self, query: &InsertQuery) -> RedDBResult<()> {
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let Some((username, role)) = crate::runtime::impl_core::current_auth_identity() else {
            return Ok(());
        };

        let tenant = crate::runtime::impl_core::current_tenant();
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
        let request = crate::auth::ColumnAccessRequest {
            action: "insert".to_string(),
            schema: None,
            table: query.table.clone(),
            columns: query.columns.clone(),
        };
        let ctx = crate::auth::policies::EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant,
            peer_ip: None,
            mfa_present: false,
            now_ms: crate::auth::now_ms(),
            principal_is_admin_role: role == crate::auth::Role::Admin,
        };

        let outcome = auth_store.check_column_projection_authz(&principal, &request, &ctx);
        let table_allowed = matches!(
            outcome.table_decision,
            crate::auth::policies::Decision::Allow { .. }
                | crate::auth::policies::Decision::AdminBypass
        );
        if !table_allowed {
            return Err(RedDBError::Query(format!(
                "principal=`{username}` action=`insert` resource=`{}:{}` denied by IAM policy",
                outcome.table_resource.kind, outcome.table_resource.name
            )));
        }
        if let Some(denied) = outcome.first_denied_column() {
            return Err(RedDBError::Query(format!(
                "principal=`{username}` action=`insert` resource=`{}:{}` denied by IAM policy",
                denied.resource.kind, denied.resource.name
            )));
        }

        Ok(())
    }

    pub(crate) fn insert_timeseries_point(
        &self,
        collection: &str,
        fields: Vec<(String, Value)>,
        mut metadata: Vec<(String, MetadataValue)>,
    ) -> RedDBResult<EntityId> {
        apply_collection_default_ttl_metadata(self, collection, &mut metadata);

        let (columns, values) = pairwise_columns_values(&fields);
        validate_timeseries_insert_columns(&columns)?;

        let metric = find_column_value_string(&columns, &values, "metric")?;
        let value = find_column_value_f64(&columns, &values, "value")?;
        let timestamp_ns =
            find_timeseries_timestamp_ns(&columns, &values)?.unwrap_or_else(current_unix_ns);
        let tags = find_timeseries_tags(&columns, &values)?;

        let mut entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TimeSeriesPoint(Box::new(crate::storage::TimeSeriesPointKind {
                series: collection.to_string(),
                metric: metric.clone(),
            })),
            EntityData::TimeSeries(crate::storage::TimeSeriesData {
                metric,
                timestamp_ns,
                value,
                tags,
            }),
        );
        // MVCC #30: stamp xmin with the active tx xid (inside a tx)
        // or an autocommit xid (allocated and committed up-front so
        // future snapshots see the row as soon as it lands).
        let writer_xid = match self.current_xid() {
            Some(xid) => xid,
            None => {
                let mgr = self.snapshot_manager();
                let xid = mgr.begin();
                mgr.commit(xid);
                xid
            }
        };
        entity.set_xmin(writer_xid);

        let store = self.inner.db.store();
        let id = store
            .insert_auto(collection, entity)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;

        if !metadata.is_empty() {
            let _ = store.set_metadata(
                collection,
                id,
                Metadata::with_fields(metadata.into_iter().collect()),
            );
        }

        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            collection,
            id.raw(),
            "timeseries",
        );

        Ok(id)
    }

    /// Execute UPDATE table SET col=val, ... WHERE filter
    ///
    /// Scans the target collection, evaluates the WHERE filter against each
    /// record, and patches every matching entity.
    pub fn execute_update(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        // CollectionContract gate (#50): runs the APPEND ONLY guard
        // (and any future contract bits) before RLS / RETURNING work
        // so the operator's immutability declaration is honoured
        // uniformly and the error message points at the DDL rather
        // than at a downstream symptom.
        crate::runtime::collection_contract::CollectionContractGate::check(
            self,
            &query.table,
            crate::runtime::collection_contract::MutationKind::Update,
        )?;

        // Apply RLS augmentation first so every downstream path — plain
        // UPDATE, UPDATE...RETURNING, the inner scan — observes the
        // same policy-filtered target set. This prevents RETURNING
        // from ever exposing rows the UPDATE policy would have
        // denied.
        let rls_gated = crate::runtime::impl_core::rls_is_enabled(self, &query.table);
        let augmented_query: UpdateQuery;
        let effective_query: &UpdateQuery = if rls_gated {
            let rls_filter = crate::runtime::impl_core::rls_policy_filter(
                self,
                &query.table,
                crate::storage::query::ast::PolicyAction::Update,
            );
            let Some(policy) = rls_filter else {
                // No admitting policy: zero rows affected, empty
                // RETURNING (never leak rows the caller can't touch).
                let mut response = RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    0,
                    "update",
                    "runtime-dml-rls",
                );
                if let Some(items) = query.returning.clone() {
                    response.result = build_returning_result(&items, &[], None);
                }
                return Ok(response);
            };
            let mut augmented = query.clone();
            augmented.filter = Some(match augmented.filter.take() {
                Some(existing) => {
                    crate::storage::query::ast::Filter::And(Box::new(existing), Box::new(policy))
                }
                None => policy,
            });
            augmented_query = augmented;
            &augmented_query
        } else {
            query
        };

        // RETURNING wraps the inner executor and uses the touched-id
        // list the inner reports so the post-image reflects exactly
        // the rows the UPDATE actually mutated (not whatever a
        // separate SELECT might have observed).
        if let Some(items) = effective_query.returning.clone() {
            let mut inner_query = effective_query.clone();
            inner_query.returning = None;
            let (mut response, touched_ids) =
                self.execute_update_inner_tracked(raw_query, &inner_query)?;

            let snapshots = super::dml_target_scan::DmlTargetScan::new(
                self,
                &effective_query.table,
                None,
                None,
            )
            .row_snapshots(&touched_ids);

            response.result = build_returning_result(&items, &snapshots, None);
            response.engine = "runtime-dml-returning";
            return Ok(response);
        }

        self.execute_update_inner(raw_query, effective_query)
    }

    /// Back-compat shim: the older entry point ignored touched ids.
    fn execute_update_inner(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.execute_update_inner_tracked(raw_query, query)
            .map(|(res, _)| res)
    }

    fn execute_update_inner_tracked(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
    ) -> RedDBResult<(RuntimeQueryResult, Vec<EntityId>)> {
        let store = self.inner.db.store();
        let effective_filter = effective_update_filter(query);
        let compiled_plan = self.compile_update_plan(query)?;
        let mut touched_ids: Vec<EntityId> = Vec::new();
        let limit_cap = query.limit.map(|l| l as usize);
        let manager = store
            .get_collection(&query.table)
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
        let ids_to_update = super::dml_target_scan::DmlTargetScan::new(
            self,
            &query.table,
            effective_filter.as_ref(),
            limit_cap,
        )
        .find_target_ids()?;

        let mut affected: u64 = 0;
        for chunk in ids_to_update.chunks(UPDATE_APPLY_CHUNK_SIZE) {
            let mut applied_chunk = Vec::with_capacity(chunk.len());
            for entity in manager.get_many(chunk).into_iter().flatten() {
                let assignments =
                    self.materialize_update_assignments_for_entity(query, &entity, &compiled_plan)?;
                let applied = self.apply_materialized_update_for_entity(
                    query.table.clone(),
                    entity,
                    &compiled_plan,
                    assignments,
                )?;
                touched_ids.push(applied.id);
                applied_chunk.push(applied);
            }
            self.persist_update_chunk(&applied_chunk)?;
            affected += applied_chunk.len() as u64;
            let lsns = self.flush_update_chunk(&applied_chunk);
            if !query.suppress_events {
                self.emit_update_events_for_collection(&query.table, &applied_chunk, &lsns)?;
            }
        }

        if affected > 0 {
            self.note_table_write(&query.table);
        }

        Ok((
            RuntimeQueryResult::dml_result(
                raw_query.to_string(),
                affected,
                "update",
                "runtime-dml",
            ),
            touched_ids,
        ))
    }

    fn compile_update_plan(&self, query: &UpdateQuery) -> RedDBResult<CompiledUpdatePlan> {
        let mut static_field_assignments = Vec::new();
        let mut static_metadata_assignments = Vec::new();
        let mut dynamic_assignments = Vec::new();
        let row_contract_plan = build_row_update_contract_plan(&self.db(), &query.table)?;
        let mut row_modified_columns = Vec::new();

        for (column, expr) in &query.assignment_exprs {
            let metadata_key = resolve_sql_ttl_metadata_key(column);
            if let Ok(value) = fold_expr_to_value(expr.clone()) {
                if let Some(metadata_key) = metadata_key {
                    let raw_value = sql_literal_to_metadata_value(metadata_key, &value)?;
                    let (canonical_key, canonical_value) =
                        canonicalize_sql_ttl_metadata(metadata_key, raw_value);
                    static_metadata_assignments.push((canonical_key.to_string(), canonical_value));
                } else {
                    let value = self.resolve_crypto_sentinel(value)?;
                    static_field_assignments.push((
                        column.clone(),
                        normalize_row_update_assignment_with_plan(
                            &query.table,
                            column,
                            value,
                            row_contract_plan.as_ref(),
                        )?,
                    ));
                    row_modified_columns.push(column.clone());
                }
                continue;
            }

            dynamic_assignments.push(CompiledUpdateAssignment {
                column: column.clone(),
                expr: expr.clone(),
                metadata_key,
                row_rule: if metadata_key.is_none() {
                    if let Some(plan) = row_contract_plan.as_ref() {
                        if plan.timestamps_enabled
                            && (column == "created_at" || column == "updated_at")
                        {
                            return Err(RedDBError::Query(format!(
                                "collection '{}' manages '{}' automatically — do not set it in UPDATE",
                                query.table, column
                            )));
                        }
                        if let Some(rule) = plan.declared_rules.get(column) {
                            Some(rule.clone())
                        } else if plan.strict_schema {
                            return Err(RedDBError::Query(format!(
                                "collection '{}' is strict and does not allow undeclared fields: {}",
                                query.table, column
                            )));
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                },
            });
            if metadata_key.is_none() {
                row_modified_columns.push(column.clone());
            }
        }

        let row_modified_columns = dedupe_update_columns(row_modified_columns);
        let row_touches_unique_columns = row_contract_plan.as_ref().is_some_and(|plan| {
            row_modified_columns.iter().any(|column| {
                plan.unique_columns
                    .keys()
                    .any(|unique| unique.eq_ignore_ascii_case(column))
            })
        });

        if let Some(ttl_ms) = query.ttl_ms {
            static_metadata_assignments
                .push(("_ttl_ms".to_string(), metadata_u64_to_value(ttl_ms)));
        }
        if let Some(expires_at_ms) = query.expires_at_ms {
            static_metadata_assignments.push((
                "_expires_at".to_string(),
                metadata_u64_to_value(expires_at_ms),
            ));
        }
        for (key, val) in &query.with_metadata {
            static_metadata_assignments.push((key.clone(), storage_value_to_metadata_value(val)));
        }

        Ok(CompiledUpdatePlan {
            static_field_assignments,
            static_metadata_assignments,
            dynamic_assignments,
            row_contract_plan,
            row_modified_columns,
            row_touches_unique_columns,
        })
    }

    fn materialize_update_assignments_for_entity(
        &self,
        query: &UpdateQuery,
        entity: &UnifiedEntity,
        compiled_plan: &CompiledUpdatePlan,
    ) -> RedDBResult<MaterializedUpdateAssignments> {
        let mut assignments = MaterializedUpdateAssignments::default();
        let mut record: Option<UnifiedRecord> = None;

        for assignment in &compiled_plan.dynamic_assignments {
            if record.is_none() {
                record = runtime_any_record_from_entity_ref(entity);
            }
            let Some(record) = record.as_ref() else {
                return Err(RedDBError::Query(format!(
                    "UPDATE could not materialize runtime record for entity {} in '{}'",
                    entity.id.raw(),
                    query.table
                )));
            };
            let value = super::expr_eval::evaluate_runtime_expr_with_db(
                Some(self.inner.db.as_ref()),
                &assignment.expr,
                record,
                Some(query.table.as_str()),
                Some(query.table.as_str()),
            )
            .ok_or_else(|| {
                RedDBError::Query(format!(
                    "failed to evaluate UPDATE expression for column '{}'",
                    assignment.column
                ))
            })?;

            if let Some(metadata_key) = assignment.metadata_key {
                let raw_value = sql_literal_to_metadata_value(metadata_key, &value)?;
                let (canonical_key, canonical_value) =
                    canonicalize_sql_ttl_metadata(metadata_key, raw_value);
                assignments
                    .dynamic_metadata_assignments
                    .push((canonical_key.to_string(), canonical_value));
            } else {
                assignments.dynamic_field_assignments.push((
                    assignment.column.clone(),
                    normalize_row_update_value_for_rule(
                        &query.table,
                        self.resolve_crypto_sentinel(value)?,
                        assignment.row_rule.as_ref(),
                    )?,
                ));
            }
        }

        Ok(assignments)
    }

    fn apply_materialized_update_for_entity(
        &self,
        collection: String,
        entity: UnifiedEntity,
        compiled_plan: &CompiledUpdatePlan,
        assignments: MaterializedUpdateAssignments,
    ) -> RedDBResult<AppliedEntityMutation> {
        if matches!(entity.data, EntityData::Row(_)) {
            return self.apply_loaded_sql_update_row_core(
                collection,
                entity,
                &compiled_plan.static_field_assignments,
                assignments.dynamic_field_assignments,
                &compiled_plan.static_metadata_assignments,
                assignments.dynamic_metadata_assignments,
                compiled_plan.row_contract_plan.as_ref(),
                &compiled_plan.row_modified_columns,
                compiled_plan.row_touches_unique_columns,
            );
        }

        self.apply_loaded_patch_entity_core(
            collection,
            entity,
            crate::json::Value::Null,
            build_patch_operations_from_materialized_assignments(compiled_plan, assignments),
        )
    }

    /// Execute DELETE FROM table WHERE filter
    pub fn execute_delete(
        &self,
        raw_query: &str,
        query: &DeleteQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        // CollectionContract gate (#50) — see execute_update for
        // rationale. The gate handles APPEND ONLY rejection and is
        // the single point where future contract bits land.
        crate::runtime::collection_contract::CollectionContractGate::check(
            self,
            &query.table,
            crate::runtime::collection_contract::MutationKind::Delete,
        )?;

        // RETURNING on DELETE: capture the pre-image via an internal
        // SELECT that reuses the same WHERE, then run the delete with
        // the RETURNING clause stripped, then project the captured
        // rows through the requested items. The extra SELECT is a
        // pragmatic MVP — a future pass can fuse the scan with the
        // delete to avoid the second pass over the heap.
        if let Some(items) = query.returning.clone() {
            let select_sql = delete_to_select_sql(raw_query).ok_or_else(|| {
                RedDBError::Query(
                    "DELETE ... RETURNING: cannot rewrite query for pre-image scan".to_string(),
                )
            })?;
            let captured = self.execute_query(&select_sql)?;

            let mut inner_query = query.clone();
            inner_query.returning = None;
            let _ = self.execute_delete(raw_query, &inner_query)?;

            let snapshots: Vec<Vec<(String, Value)>> = captured
                .result
                .records
                .iter()
                .map(|rec| {
                    rec.iter_fields()
                        .map(|(k, v)| (k.as_ref().to_string(), v.clone()))
                        .collect()
                })
                .collect();
            let affected = snapshots.len() as u64;
            let result = build_returning_result(&items, &snapshots, None);

            let mut response = RuntimeQueryResult::dml_result(
                raw_query.to_string(),
                affected,
                "delete",
                "runtime-dml-returning",
            );
            response.result = result;
            return Ok(response);
        }
        // Row-Level Security enforcement (Phase 2.5.2 PG parity).
        //
        // When the table has RLS enabled, gate the DELETE by the
        // per-role policy set: mutations only touch rows that *every*
        // matching `FOR DELETE` policy would accept. No policies =>
        // zero rows affected (PG restrictive-default).
        if crate::runtime::impl_core::rls_is_enabled(self, &query.table) {
            let rls_filter = crate::runtime::impl_core::rls_policy_filter(
                self,
                &query.table,
                crate::storage::query::ast::PolicyAction::Delete,
            );
            let Some(policy) = rls_filter else {
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    0,
                    "delete",
                    "runtime-dml-rls",
                ));
            };
            // Fold the policy predicate into the user's WHERE before
            // dispatching — the remainder of this function reads the
            // filter from `query` via `effective_delete_filter`, which
            // respects the updated value.
            let mut augmented = query.clone();
            augmented.filter = Some(match augmented.filter.take() {
                Some(existing) => {
                    crate::storage::query::ast::Filter::And(Box::new(existing), Box::new(policy))
                }
                None => policy,
            });
            return self.execute_delete_inner(raw_query, &augmented);
        }
        self.execute_delete_inner(raw_query, query)
    }

    fn execute_delete_inner(
        &self,
        raw_query: &str,
        query: &DeleteQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let effective_filter = effective_delete_filter(query);

        // Find the rows that match the WHERE clause. The "find target
        // rows" loop lives in DmlTargetScan so UPDATE (#52) can reuse
        // the same scan strategy.
        let scan = super::dml_target_scan::DmlTargetScan::new(
            self,
            &query.table,
            effective_filter.as_ref(),
            None,
        );
        let ids_to_delete = scan.find_target_ids()?;

        // For event-enabled collections, snapshot the pre-delete state
        // before rows are physically removed.
        let needs_delete_events =
            !query.suppress_events && self.collection_has_delete_subscriptions(&query.table);
        let mut pre_images: HashMap<u64, crate::json::Value> = if needs_delete_events {
            scan.row_json_pre_images(&ids_to_delete)
        } else {
            HashMap::new()
        };

        let mut affected: u64 = 0;
        for chunk in ids_to_delete.chunks(UPDATE_APPLY_CHUNK_SIZE) {
            let (count, lsns) = self.delete_entities_batch(&query.table, chunk)?;
            affected += count;
            if needs_delete_events && !lsns.is_empty() {
                // lsns.len() == actually-deleted entities; align with chunk ids.
                // `delete_batch` may skip missing entities, so we correlate by
                // the number returned (they're emitted in chunk order).
                let deleted_chunk = &chunk[..lsns.len().min(chunk.len())];
                self.emit_delete_events_for_collection(
                    &query.table,
                    deleted_chunk,
                    &lsns,
                    &pre_images,
                )?;
            }
        }
        pre_images.clear();

        if affected > 0 {
            self.note_table_write(&query.table);
        }

        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            affected,
            "delete",
            "runtime-dml",
        ))
    }
}

fn build_patch_operations_from_materialized_assignments(
    compiled_plan: &CompiledUpdatePlan,
    assignments: MaterializedUpdateAssignments,
) -> Vec<PatchEntityOperation> {
    let mut operations = Vec::with_capacity(
        compiled_plan.static_field_assignments.len()
            + compiled_plan.static_metadata_assignments.len()
            + assignments.dynamic_field_assignments.len()
            + assignments.dynamic_metadata_assignments.len(),
    );

    for (column, value) in &compiled_plan.static_field_assignments {
        operations.push(PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["fields".to_string(), column.clone()],
            value: Some(storage_value_to_json(value)),
        });
    }

    for (column, value) in assignments.dynamic_field_assignments {
        operations.push(PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["fields".to_string(), column],
            value: Some(storage_value_to_json(&value)),
        });
    }

    for (key, value) in &compiled_plan.static_metadata_assignments {
        operations.push(PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["metadata".to_string(), key.clone()],
            value: Some(metadata_value_to_json(value)),
        });
    }

    for (key, value) in assignments.dynamic_metadata_assignments {
        operations.push(PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["metadata".to_string(), key],
            value: Some(metadata_value_to_json(&value)),
        });
    }

    operations
}

/// Rewrite `DELETE FROM <table> [WHERE …] [RETURNING …]` as
/// `SELECT * FROM <table> [WHERE …]` so the delete executor can
/// capture the pre-image before actually removing the rows. Returns
/// `None` when the input does not start with `DELETE`.
///
/// Case-insensitive on the keywords. Preserves everything between
/// the table name and the RETURNING clause, so WHERE / ORDER BY /
/// LIMIT survive untouched. The RETURNING tail — if present — is
/// truncated at the first top-level `RETURNING` token.
fn delete_to_select_sql(sql: &str) -> Option<String> {
    let trimmed = sql.trim_start();
    let lowered = trimmed.to_ascii_lowercase();
    if !lowered.starts_with("delete ") && !lowered.starts_with("delete\t") {
        return None;
    }
    // Find `FROM` after DELETE.
    let from_idx = lowered.find(" from ")?;
    let after_from = &trimmed[from_idx + " from ".len()..];
    let after_from_lc = &lowered[from_idx + " from ".len()..];

    // Cut off the RETURNING tail (a naive search — the RETURNING
    // clause only appears once per statement at top level in our
    // grammar). Matches whitespace-bounded tokens to avoid clipping
    // `RETURNING` inside a string literal.
    let mut body = after_from.to_string();
    if let Some(pos) = find_top_level_keyword(after_from_lc, "returning") {
        body.truncate(pos);
    }
    Some(format!("SELECT * FROM {}", body.trim_end()))
}

/// Find the byte offset of a whitespace-bounded keyword in a
/// lowercased haystack, skipping matches inside single-quoted
/// string literals. Naive — no escape handling — but enough for
/// the shapes the DML parser emits.
fn find_top_level_keyword(haystack: &str, needle: &str) -> Option<usize> {
    let bytes = haystack.as_bytes();
    let nlen = needle.len();
    let mut i = 0usize;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\'' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if !in_string
            && i + nlen <= bytes.len()
            && &bytes[i..i + nlen] == needle.as_bytes()
            && (i == 0 || bytes[i - 1].is_ascii_whitespace())
            && (i + nlen == bytes.len() || bytes[i + nlen].is_ascii_whitespace())
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Build a `UnifiedResult` from the rows affected by a DML statement plus
/// its `RETURNING` clause. Each snapshot is a list of (column, value) pairs
/// for one affected row; `outputs`, when provided, supplies the engine-
/// assigned entity id for the same row (INSERT path). Projection honours
/// the RETURNING items: `*` expands to every snapshot column plus
/// `red_entity_id` when available.
fn build_returning_result(
    items: &[ReturningItem],
    snapshots: &[Vec<(String, Value)>],
    outputs: Option<&[crate::application::entity::CreateEntityOutput]>,
) -> UnifiedResult {
    let project_all = items.iter().any(|it| matches!(it, ReturningItem::All));

    let mut columns: Vec<String> = if project_all {
        let mut cols: Vec<String> = Vec::new();
        if outputs.is_some() {
            cols.push("red_entity_id".to_string());
        }
        if let Some(first) = snapshots.first() {
            for (name, _) in first {
                cols.push(name.clone());
            }
        }
        cols
    } else {
        items
            .iter()
            .filter_map(|it| match it {
                ReturningItem::Column(c) => Some(c.clone()),
                ReturningItem::All => None,
            })
            .collect()
    };
    // Guarantee unique order-preserving column list.
    {
        let mut seen = std::collections::HashSet::new();
        columns.retain(|c| seen.insert(c.clone()));
    }

    let id_key = sys_key_red_entity_id();
    let mut records: Vec<UnifiedRecord> = Vec::with_capacity(snapshots.len());
    for (idx, snap) in snapshots.iter().enumerate() {
        let mut values: HashMap<Arc<str>, Value> = HashMap::with_capacity(columns.len());
        if let Some(outs) = outputs {
            if let Some(out) = outs.get(idx) {
                values.insert(Arc::clone(&id_key), Value::Integer(out.id.raw() as i64));
            }
        }
        for (name, val) in snap {
            values.insert(Arc::from(name.as_str()), val.clone());
        }
        let mut rec = UnifiedRecord::default();
        // Only keep projected columns on the record.
        for col in &columns {
            if let Some(v) = values.get(col.as_str()) {
                rec.set_arc(Arc::from(col.as_str()), v.clone());
            }
        }
        records.push(rec);
    }

    UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    }
}

fn ensure_graph_insert_contract(runtime: &RedDBRuntime, collection: &str) -> RedDBResult<()> {
    let db = runtime.db();
    if let Some(contract) = db.collection_contract(collection) {
        let advisory_implicit_dynamic = matches!(
            (&contract.origin, &contract.schema_mode),
            (
                crate::physical::ContractOrigin::Implicit,
                crate::catalog::SchemaMode::Dynamic,
            )
        );
        if advisory_implicit_dynamic
            || matches!(
                contract.declared_model,
                crate::catalog::CollectionModel::Graph | crate::catalog::CollectionModel::Mixed
            )
        {
            return Ok(());
        }
        return Err(RedDBError::InvalidOperation(format!(
            "collection '{}' is declared as '{:?}' and does not allow 'Graph' writes",
            collection, contract.declared_model
        )));
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    db.save_collection_contract(crate::physical::CollectionContract {
        name: collection.to_string(),
        declared_model: crate::catalog::CollectionModel::Graph,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Implicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: db.collection_default_ttl_ms(collection),
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        append_only: false,
        subscriptions: Vec::new(),
    })
    .map(|_| ())
    .map_err(|err| RedDBError::Internal(err.to_string()))
}

fn dedupe_update_columns(mut columns: Vec<String>) -> Vec<String> {
    if columns.is_empty() {
        return columns;
    }

    let mut unique = Vec::with_capacity(columns.len());
    for column in columns.drain(..) {
        if !unique
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&column))
        {
            unique.push(column);
        }
    }
    unique
}

// =============================================================================
// Helper functions for extracting typed values from column/value pairs
// =============================================================================

const SQL_TTL_METADATA_COLUMNS: [&str; 3] = ["_ttl", "_ttl_ms", "_expires_at"];

fn resolve_sql_ttl_metadata_key(column: &str) -> Option<&'static str> {
    if column.eq_ignore_ascii_case("_ttl") {
        Some(SQL_TTL_METADATA_COLUMNS[0])
    } else if column.eq_ignore_ascii_case("_ttl_ms") {
        Some(SQL_TTL_METADATA_COLUMNS[1])
    } else if column.eq_ignore_ascii_case("_expires_at") {
        Some(SQL_TTL_METADATA_COLUMNS[2])
    } else {
        None
    }
}

/// Canonicalize a SQL TTL metadata `(key, value)` pair so the retention
/// sweeper sees a single key (`_ttl_ms`) regardless of which legacy form
/// the operator wrote. `_ttl` is scaled from seconds to milliseconds;
/// `_ttl_ms` and `_expires_at` are passed through.
fn canonicalize_sql_ttl_metadata(
    key: &'static str,
    value: MetadataValue,
) -> (&'static str, MetadataValue) {
    if key != "_ttl" {
        return (key, value);
    }
    let scaled = match value {
        MetadataValue::Int(s) => MetadataValue::Int(s.saturating_mul(1_000)),
        MetadataValue::Timestamp(ms_or_s) => {
            // Timestamp is already chosen for very large values; treat as
            // already-ms to avoid silent overflow.
            MetadataValue::Timestamp(ms_or_s)
        }
        MetadataValue::Float(f) => MetadataValue::Float(f * 1_000.0),
        other => other,
    };
    ("_ttl_ms", scaled)
}

/// Sentinel prefix produced by the parser for `PASSWORD('...')` and
/// `SECRET('...')` literals. The runtime strips this marker and
/// applies the actual crypto transform during INSERT execution.
pub(crate) const PLAINTEXT_SENTINEL: &str = "@@plain@@";

impl RedDBRuntime {
    /// Strip the plaintext sentinel from a `Value::Password` or
    /// `Value::Secret` produced by the parser and apply the real
    /// crypto transform. `Password` is always hashed with argon2id.
    /// `Secret` is encrypted with AES-256-GCM keyed by the vault
    /// when `red.config.secret.auto_encrypt = true` (default).
    pub(crate) fn resolve_crypto_sentinel(&self, value: Value) -> RedDBResult<Value> {
        match value {
            Value::Password(marked) => {
                if let Some(plain) = marked.strip_prefix(PLAINTEXT_SENTINEL) {
                    Ok(Value::Password(crate::auth::store::hash_password(plain)))
                } else {
                    Ok(Value::Password(marked))
                }
            }
            Value::Secret(bytes) => {
                if bytes.starts_with(PLAINTEXT_SENTINEL.as_bytes()) {
                    if !self.secret_auto_encrypt() {
                        return Err(RedDBError::Query(
                            "SECRET() literal rejected: red.config.secret.auto_encrypt \
                             is false. Insert pre-encrypted bytes directly instead."
                                .to_string(),
                        ));
                    }
                    let key = self.secret_aes_key().ok_or_else(|| {
                        RedDBError::Query(
                            "SECRET() column encryption requires a bootstrapped \
                             vault (red.secret.aes_key is missing). Start the server \
                             with --vault to enable."
                                .to_string(),
                        )
                    })?;
                    let plain = &bytes[PLAINTEXT_SENTINEL.len()..];
                    Ok(Value::Secret(encrypt_secret_payload(&key, plain)))
                } else {
                    Ok(Value::Secret(bytes))
                }
            }
            other => Ok(other),
        }
    }
}

/// Encode an AES-256-GCM ciphertext as `[12-byte nonce][ciphertext||tag]`.
/// This is the on-disk representation of `Value::Secret`.
fn encrypt_secret_payload(key: &[u8; 32], plaintext: &[u8]) -> Vec<u8> {
    let nonce_bytes = crate::auth::store::random_bytes(12);
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&nonce_bytes[..12]);
    let ct = crate::crypto::aes_gcm::aes256_gcm_encrypt(key, &nonce, b"reddb.secret", plaintext);
    let mut out = Vec::with_capacity(12 + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Decode a `Value::Secret` payload back to plaintext. Returns
/// `None` when the payload is too short or AES-GCM authentication
/// fails (tampered or wrong key).
pub(crate) fn decrypt_secret_payload(key: &[u8; 32], payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() < 12 {
        return None;
    }
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&payload[..12]);
    crate::crypto::aes_gcm::aes256_gcm_decrypt(key, &nonce, b"reddb.secret", &payload[12..]).ok()
}

fn split_insert_metadata(
    runtime: &RedDBRuntime,
    columns: &[String],
    values: &[Value],
) -> RedDBResult<(Vec<(String, Value)>, Vec<(String, MetadataValue)>)> {
    let mut fields = Vec::new();
    let mut metadata = Vec::new();

    for (column, value) in columns.iter().zip(values.iter()) {
        // Still support legacy _ttl columns for backward compat
        if let Some(metadata_key) = resolve_sql_ttl_metadata_key(column) {
            let raw_value = sql_literal_to_metadata_value(metadata_key, value)?;
            let (canonical_key, canonical_value) =
                canonicalize_sql_ttl_metadata(metadata_key, raw_value);
            metadata.push((canonical_key.to_string(), canonical_value));
            continue;
        }
        fields.push((
            column.clone(),
            runtime.resolve_crypto_sentinel(value.clone())?,
        ));
    }

    Ok((fields, metadata))
}

/// Merge structured WITH TTL, WITH EXPIRES AT, and WITH METADATA clauses into metadata entries.
fn merge_with_clauses(
    metadata: &mut Vec<(String, MetadataValue)>,
    ttl_ms: Option<u64>,
    expires_at_ms: Option<u64>,
    with_metadata: &[(String, Value)],
) {
    if let Some(ms) = ttl_ms {
        metadata.push((
            "_ttl_ms".to_string(),
            if ms <= i64::MAX as u64 {
                MetadataValue::Int(ms as i64)
            } else {
                MetadataValue::Timestamp(ms)
            },
        ));
    }
    if let Some(ms) = expires_at_ms {
        metadata.push(("_expires_at".to_string(), MetadataValue::Timestamp(ms)));
    }
    for (key, value) in with_metadata {
        let meta_value = match value {
            Value::Text(s) => MetadataValue::String(s.to_string()),
            Value::Integer(n) => MetadataValue::Int(*n),
            Value::Float(n) => MetadataValue::Float(*n),
            Value::Boolean(b) => MetadataValue::Bool(*b),
            _ => MetadataValue::String(value.to_string()),
        };
        metadata.push((key.clone(), meta_value));
    }
}

fn merge_vector_metadata_column(
    metadata: &mut Vec<(String, MetadataValue)>,
    columns: &[String],
    values: &[Value],
) -> RedDBResult<()> {
    let Some(value) = columns
        .iter()
        .position(|column| column.eq_ignore_ascii_case("metadata"))
        .map(|index| &values[index])
    else {
        return Ok(());
    };
    let json = match value {
        Value::Null => return Ok(()),
        Value::Json(bytes) => crate::json::from_slice(bytes).map_err(|err| {
            RedDBError::Query(format!("column 'metadata' invalid JSON object: {err}"))
        })?,
        Value::Text(text) => crate::json::from_str(text).map_err(|err| {
            RedDBError::Query(format!("column 'metadata' invalid JSON object: {err}"))
        })?,
        other => {
            return Err(RedDBError::Query(format!(
                "column 'metadata' expected JSON object, got {other:?}"
            )))
        }
    };
    let parsed = metadata_from_json(&json)?;
    for (key, value) in parsed.iter() {
        metadata.push((key.clone(), value.clone()));
    }
    Ok(())
}

fn apply_collection_default_ttl_metadata(
    runtime: &RedDBRuntime,
    collection: &str,
    metadata: &mut Vec<(String, MetadataValue)>,
) {
    if has_internal_ttl_metadata(metadata) {
        return;
    }

    let Some(default_ttl_ms) = runtime.db().collection_default_ttl_ms(collection) else {
        return;
    };

    metadata.push((
        "_ttl_ms".to_string(),
        if default_ttl_ms <= i64::MAX as u64 {
            MetadataValue::Int(default_ttl_ms as i64)
        } else {
            MetadataValue::Timestamp(default_ttl_ms)
        },
    ));
}

fn ensure_non_tree_reserved_metadata_entries(
    metadata: &[(String, MetadataValue)],
) -> RedDBResult<()> {
    for (key, _) in metadata {
        ensure_non_tree_reserved_metadata_key(key)?;
    }
    Ok(())
}

fn ensure_non_tree_reserved_metadata_key(key: &str) -> RedDBResult<()> {
    if key.starts_with(TREE_METADATA_PREFIX) {
        return Err(RedDBError::Query(format!(
            "metadata key '{}' is reserved for managed trees",
            key
        )));
    }
    Ok(())
}

fn ensure_non_tree_structural_edge_label(label: &str) -> RedDBResult<()> {
    if label.eq_ignore_ascii_case(TREE_CHILD_EDGE_LABEL) {
        return Err(RedDBError::Query(format!(
            "edge label '{}' is reserved for managed trees",
            TREE_CHILD_EDGE_LABEL
        )));
    }
    Ok(())
}

fn pairwise_columns_values(pairs: &[(String, Value)]) -> (Vec<String>, Vec<Value>) {
    let mut columns = Vec::with_capacity(pairs.len());
    let mut values = Vec::with_capacity(pairs.len());

    for (column, value) in pairs {
        columns.push(column.clone());
        values.push(value.clone());
    }

    (columns, values)
}

/// Find a required column value and return it as-is.
fn find_column_value(columns: &[String], values: &[Value], name: &str) -> RedDBResult<Value> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return Ok(values[i].clone());
        }
    }
    Err(RedDBError::Query(format!(
        "required column '{name}' not found in INSERT"
    )))
}

/// Find a required column value and coerce to String.
fn find_column_value_string(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<String> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Text(s) => Ok(s.to_string()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Float(n) => Ok(n.to_string()),
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected text, got {other:?}"
        ))),
    }
}

fn find_column_value_f64(columns: &[String], values: &[Value], name: &str) -> RedDBResult<f64> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Float(n) => Ok(n),
        Value::Integer(n) => Ok(n as f64),
        Value::UnsignedInteger(n) => Ok(n as f64),
        Value::Text(s) => s
            .parse::<f64>()
            .map_err(|_| RedDBError::Query(format!("column '{name}' expected number, got '{s}'"))),
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected number, got {other:?}"
        ))),
    }
}

/// Find an optional column value as String.
fn find_column_value_opt_string(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> Option<String> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return match &values[i] {
                Value::Null => None,
                Value::Text(s) => Some(s.to_string()),
                Value::Integer(n) => Some(n.to_string()),
                Value::Float(n) => Some(n.to_string()),
                _ => None,
            };
        }
    }
    None
}

/// Resolve an EDGE endpoint (`from`/`to`) to a numeric entity id.
///
/// Accepts integer literals, decimal strings, and node labels resolved via
/// the per-collection graph label index (same source of truth that
/// `GRAPH NEIGHBORHOOD` / `GRAPH TRAVERSE` use at query time). Ambiguous
/// labels error so callers can fall back to the numeric id form.
fn resolve_edge_endpoint(
    store: &crate::storage::unified::UnifiedStore,
    collection: &str,
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<u64> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Integer(n) => Ok(n as u64),
        Value::UnsignedInteger(n) => Ok(n),
        Value::Text(s) => {
            if let Ok(n) = s.parse::<u64>() {
                return Ok(n);
            }
            let matches = store.lookup_graph_nodes_by_label_in(collection, &s);
            match matches.len() {
                0 => Err(RedDBError::Query(format!(
                    "column '{name}': no graph node with label '{s}' in collection '{collection}'"
                ))),
                1 => Ok(matches[0].raw()),
                n => Err(RedDBError::Query(format!(
                    "column '{name}': ambiguous label '{s}' matches {n} nodes in collection '{collection}'; use the numeric id"
                ))),
            }
        }
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected integer or node label, got {other:?}"
        ))),
    }
}

/// Find a required column value and coerce to u64.
fn find_column_value_u64(columns: &[String], values: &[Value], name: &str) -> RedDBResult<u64> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Integer(n) => Ok(n as u64),
        Value::UnsignedInteger(n) => Ok(n),
        Value::Text(s) => s
            .parse::<u64>()
            .map_err(|_| RedDBError::Query(format!("column '{name}' expected integer, got '{s}'"))),
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected integer, got {other:?}"
        ))),
    }
}

/// Find an optional column value as f32.
fn find_column_value_f32_opt(columns: &[String], values: &[Value], name: &str) -> Option<f32> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return match &values[i] {
                Value::Float(n) => Some(*n as f32),
                Value::Integer(n) => Some(*n as f32),
                Value::Null => None,
                _ => None,
            };
        }
    }
    None
}

/// Find a required column value and coerce to Vec<f32> (from Value::Vector).
fn find_column_value_vec_f32(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<Vec<f32>> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Vector(v) => Ok(v),
        Value::Json(bytes) => {
            // Try to parse as JSON array of numbers
            let s = std::str::from_utf8(&bytes).map_err(|_| {
                RedDBError::Query(format!("column '{name}' contains invalid UTF-8"))
            })?;
            let arr: Vec<f32> = crate::json::from_str(s).map_err(|e| {
                RedDBError::Query(format!("column '{name}' invalid vector JSON: {e}"))
            })?;
            Ok(arr)
        }
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected vector, got {other:?}"
        ))),
    }
}

fn find_column_value_vec_f32_any(
    columns: &[String],
    values: &[Value],
    names: &[&str],
) -> RedDBResult<Vec<f32>> {
    for name in names {
        if columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(name))
        {
            return find_column_value_vec_f32(columns, values, name);
        }
    }
    Err(RedDBError::Query(format!(
        "required vector column '{}' not found in INSERT",
        names.join("' or '")
    )))
}

/// Extract remaining properties (all columns not in the exclusion list).
fn extract_remaining_properties(
    columns: &[String],
    values: &[Value],
    exclude: &[&str],
) -> Vec<(String, Value)> {
    columns
        .iter()
        .zip(values.iter())
        .filter(|(col, _)| !exclude.iter().any(|e| col.eq_ignore_ascii_case(e)))
        .map(|(col, val)| (col.clone(), val.clone()))
        .collect()
}

fn validate_timeseries_insert_columns(columns: &[String]) -> RedDBResult<()> {
    let mut invalid = Vec::new();
    for column in columns {
        if !is_timeseries_insert_column(column) && resolve_sql_ttl_metadata_key(column).is_none() {
            invalid.push(column.clone());
        }
    }

    if invalid.is_empty() {
        Ok(())
    } else {
        Err(RedDBError::Query(format!(
            "timeseries INSERT only accepts metric, value, tags, timestamp, timestamp_ns, or time columns; got {}",
            invalid.join(", ")
        )))
    }
}

fn is_timeseries_insert_column(column: &str) -> bool {
    matches!(
        column.to_ascii_lowercase().as_str(),
        "metric" | "value" | "tags" | "timestamp" | "timestamp_ns" | "time"
    )
}

fn find_timeseries_timestamp_ns(columns: &[String], values: &[Value]) -> RedDBResult<Option<u64>> {
    let mut found = None;

    for alias in ["timestamp_ns", "timestamp", "time"] {
        for (index, column) in columns.iter().enumerate() {
            if !column.eq_ignore_ascii_case(alias) {
                continue;
            }

            if found.is_some() {
                return Err(RedDBError::Query(
                    "timeseries INSERT accepts only one timestamp column".to_string(),
                ));
            }

            found = Some(coerce_value_to_non_negative_u64(&values[index], alias)?);
        }
    }

    Ok(found)
}

fn find_timeseries_tags(
    columns: &[String],
    values: &[Value],
) -> RedDBResult<std::collections::HashMap<String, String>> {
    for (index, column) in columns.iter().enumerate() {
        if column.eq_ignore_ascii_case("tags") {
            return parse_timeseries_tags(&values[index]);
        }
    }
    Ok(std::collections::HashMap::new())
}

fn parse_timeseries_tags(value: &Value) -> RedDBResult<std::collections::HashMap<String, String>> {
    match value {
        Value::Null => Ok(std::collections::HashMap::new()),
        Value::Json(bytes) => parse_timeseries_tags_json(bytes),
        Value::Text(text) => parse_timeseries_tags_json(text.as_bytes()),
        other => Err(RedDBError::Query(format!(
            "timeseries tags must be a JSON object or JSON text, got {other:?}"
        ))),
    }
}

fn parse_timeseries_tags_json(
    bytes: &[u8],
) -> RedDBResult<std::collections::HashMap<String, String>> {
    let json: crate::json::Value = crate::json::from_slice(bytes)
        .map_err(|err| RedDBError::Query(format!("timeseries tags must be valid JSON: {err}")))?;

    let object = match json {
        crate::json::Value::Object(object) => object,
        other => {
            return Err(RedDBError::Query(format!(
                "timeseries tags must be a JSON object, got {other:?}"
            )))
        }
    };

    let mut tags = std::collections::HashMap::with_capacity(object.len());
    for (key, value) in object {
        tags.insert(key, json_tag_value_to_string(&value));
    }
    Ok(tags)
}

fn json_tag_value_to_string(value: &crate::json::Value) -> String {
    match value {
        crate::json::Value::Null => "null".to_string(),
        crate::json::Value::Bool(value) => value.to_string(),
        crate::json::Value::Number(value) => value.to_string(),
        crate::json::Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn coerce_value_to_non_negative_u64(value: &Value, column: &str) -> RedDBResult<u64> {
    match value {
        Value::UnsignedInteger(value) => Ok(*value),
        Value::Integer(value) if *value >= 0 => Ok(*value as u64),
        Value::Float(value) if *value >= 0.0 => Ok(*value as u64),
        Value::Text(value) => value.parse::<u64>().map_err(|_| {
            RedDBError::Query(format!(
                "column '{column}' expected a non-negative integer timestamp, got '{value}'"
            ))
        }),
        other => Err(RedDBError::Query(format!(
            "column '{column}' expected a non-negative integer timestamp, got {other:?}"
        ))),
    }
}

fn current_unix_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

fn metadata_value_to_json(value: &MetadataValue) -> crate::json::Value {
    use crate::json::{Map, Value as JV};
    match value {
        MetadataValue::Null => JV::Null,
        MetadataValue::Bool(value) => JV::Bool(*value),
        MetadataValue::Int(value) => JV::Number(*value as f64),
        MetadataValue::Float(value) => JV::Number(*value),
        MetadataValue::String(value) => JV::String(value.clone()),
        MetadataValue::Bytes(value) => JV::Array(
            value
                .iter()
                .map(|value| JV::Number(*value as f64))
                .collect(),
        ),
        MetadataValue::Timestamp(value) => JV::Number(*value as f64),
        MetadataValue::Array(values) => {
            JV::Array(values.iter().map(metadata_value_to_json).collect())
        }
        MetadataValue::Object(object) => {
            let entries = object
                .iter()
                .map(|(key, value)| (key.clone(), metadata_value_to_json(value)))
                .collect();
            JV::Object(entries)
        }
        MetadataValue::Geo { lat, lon } => {
            let mut object = Map::new();
            object.insert("lat".to_string(), JV::Number(*lat));
            object.insert("lon".to_string(), JV::Number(*lon));
            JV::Object(object)
        }
        MetadataValue::Reference(target) => {
            let mut object = Map::new();
            object.insert(
                "collection".to_string(),
                JV::String(target.collection().to_string()),
            );
            object.insert(
                "entity_id".to_string(),
                JV::Number(target.entity_id().raw() as f64),
            );
            JV::Object(object)
        }
        MetadataValue::References(values) => {
            let refs = values
                .iter()
                .map(|target| {
                    let mut object = Map::new();
                    object.insert(
                        "collection".to_string(),
                        JV::String(target.collection().to_string()),
                    );
                    object.insert(
                        "entity_id".to_string(),
                        JV::Number(target.entity_id().raw() as f64),
                    );
                    JV::Object(object)
                })
                .collect();
            JV::Array(refs)
        }
    }
}

fn storage_value_to_metadata_value(value: &Value) -> MetadataValue {
    match value {
        Value::Null => MetadataValue::Null,
        Value::Boolean(value) => MetadataValue::Bool(*value),
        Value::Integer(value) => MetadataValue::Int(*value),
        Value::UnsignedInteger(value) => metadata_u64_to_value(*value),
        Value::Float(value) => MetadataValue::Float(*value),
        Value::Text(value) => MetadataValue::String(value.to_string()),
        Value::Blob(value) => MetadataValue::Bytes(value.clone()),
        Value::Timestamp(value) => {
            if *value >= 0 {
                metadata_u64_to_value(*value as u64)
            } else {
                MetadataValue::Int(*value)
            }
        }
        Value::TimestampMs(value) => {
            if *value >= 0 {
                metadata_u64_to_value(*value as u64)
            } else {
                MetadataValue::Int(*value)
            }
        }
        Value::Json(value) => MetadataValue::String(String::from_utf8_lossy(value).into_owned()),
        Value::Uuid(value) => MetadataValue::String(format!("{value:?}")),
        Value::Date(value) => MetadataValue::String(value.to_string()),
        Value::Time(value) => MetadataValue::String(value.to_string()),
        Value::Decimal(value) => MetadataValue::String(value.to_string()),
        Value::Ipv4(value) => MetadataValue::String(format!(
            "{}.{}.{}.{}",
            (value >> 24) & 0xFF,
            (value >> 16) & 0xFF,
            (value >> 8) & 0xFF,
            value & 0xFF
        )),
        Value::Port(value) => MetadataValue::Int(i64::from(*value)),
        Value::Latitude(value) => MetadataValue::Float(*value as f64 / 1_000_000.0),
        Value::Longitude(value) => MetadataValue::Float(*value as f64 / 1_000_000.0),
        Value::GeoPoint(lat, lon) => MetadataValue::Geo {
            lat: *lat as f64 / 1_000_000.0,
            lon: *lon as f64 / 1_000_000.0,
        },
        Value::BigInt(value) => MetadataValue::String(value.to_string()),
        Value::TableRef(value) => MetadataValue::String(value.clone()),
        Value::PageRef(value) => MetadataValue::Int(*value as i64),
        Value::Password(value) => MetadataValue::String(value.clone()),
        Value::Array(values) => {
            MetadataValue::Array(values.iter().map(storage_value_to_metadata_value).collect())
        }
        _ => MetadataValue::String(value.to_string()),
    }
}

fn sql_literal_to_metadata_value(field: &str, value: &Value) -> RedDBResult<MetadataValue> {
    match value {
        Value::Null => Ok(MetadataValue::Null),
        Value::Integer(value) if *value >= 0 => Ok(metadata_u64_to_value(*value as u64)),
        Value::Integer(_) => Err(RedDBError::Query(format!(
            "column '{field}' must be non-negative for TTL metadata"
        ))),
        Value::UnsignedInteger(value) => Ok(metadata_u64_to_value(*value)),
        Value::Float(value) if value.is_finite() => {
            if value.fract().abs() >= f64::EPSILON {
                return Err(RedDBError::Query(format!(
                    "column '{field}' must be an integer (TTL metadata must be an integer)"
                )));
            }
            if *value < 0.0 {
                return Err(RedDBError::Query(format!(
                    "column '{field}' must be non-negative for TTL metadata"
                )));
            }
            if *value > u64::MAX as f64 {
                return Err(RedDBError::Query(format!(
                    "column '{field}' value is too large"
                )));
            }
            Ok(metadata_u64_to_value(*value as u64))
        }
        Value::Float(_) => Err(RedDBError::Query(format!(
            "column '{field}' must be a finite number"
        ))),
        Value::Text(value) => {
            let value = value.trim();
            if let Ok(value) = value.parse::<u64>() {
                Ok(metadata_u64_to_value(value))
            } else if let Ok(value) = value.parse::<i64>() {
                if value < 0 {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' must be non-negative for TTL metadata"
                    )));
                }
                Ok(metadata_u64_to_value(value as u64))
            } else if let Ok(value) = value.parse::<f64>() {
                if !value.is_finite() {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' must be a finite number"
                    )));
                }
                if value.fract().abs() >= f64::EPSILON {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' must be an integer (TTL metadata must be an integer)"
                    )));
                }
                if value < 0.0 {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' must be non-negative for TTL metadata"
                    )));
                }
                if value > u64::MAX as f64 {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' value is too large"
                    )));
                }
                Ok(metadata_u64_to_value(value as u64))
            } else {
                Err(RedDBError::Query(format!(
                    "column '{field}' expects a numeric value for TTL metadata"
                )))
            }
        }
        _ => Err(RedDBError::Query(format!(
            "column '{field}' expects a numeric value for TTL metadata"
        ))),
    }
}

fn metadata_u64_to_value(value: u64) -> MetadataValue {
    if value <= i64::MAX as u64 {
        MetadataValue::Int(value as i64)
    } else {
        MetadataValue::Timestamp(value)
    }
}

/// Phase 2 PG parity: inspect a column value and return `true` when
/// the dotted `tail` path is already present under it. Used by the
/// tenant auto-fill so rows that already carry an explicit value
/// (bulk import, admin insert on behalf of a tenant) are not
/// double-stamped with the session's current_tenant().
fn dotted_tail_already_set(value: &Value, tail: &str) -> bool {
    let json = match value {
        Value::Null => return false,
        Value::Json(bytes) | Value::Blob(bytes) => {
            match crate::json::from_slice::<crate::json::Value>(bytes) {
                Ok(v) => v,
                Err(_) => return false,
            }
        }
        Value::Text(s) => {
            let trimmed = s.trim_start();
            if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
                return false;
            }
            match crate::json::from_str::<crate::json::Value>(s) {
                Ok(v) => v,
                Err(_) => return false,
            }
        }
        _ => return false,
    };
    let mut cursor = &json;
    for seg in tail.split('.') {
        match cursor {
            crate::json::Value::Object(map) => match map.iter().find(|(k, _)| *k == seg) {
                Some((_, v)) => cursor = v,
                None => return false,
            },
            _ => return false,
        }
    }
    !matches!(cursor, crate::json::Value::Null)
}

/// Phase 2 PG parity: take a column value (possibly Null / Text /
/// Json) and return a `Value::Json` with the dotted `tail` path set
/// to `tenant_id`. Preserves every pre-existing key.
///
/// Accepts:
/// * `Value::Null`  → fresh `{tail: tenant_id}` object
/// * `Value::Json(bytes)` → parse, navigate / create path, re-serialize
/// * `Value::text(s)` if `s` is valid JSON → same as Json
/// * anything else → error (user supplied a scalar where we need
///   a JSON container)
fn merge_dotted_tenant(current: Value, tail: &str, tenant_id: &str) -> RedDBResult<Value> {
    let mut root = match current {
        Value::Null => crate::json::Value::Object(Default::default()),
        Value::Json(bytes) | Value::Blob(bytes) => {
            crate::json::from_slice(&bytes).map_err(|err| {
                RedDBError::Query(format!(
                    "tenant auto-fill: root column is not valid JSON ({err})"
                ))
            })?
        }
        Value::Text(s) => {
            if s.trim().is_empty() {
                crate::json::Value::Object(Default::default())
            } else {
                crate::json::from_str::<crate::json::Value>(&s).map_err(|err| {
                    RedDBError::Query(format!(
                        "tenant auto-fill: text root is not valid JSON ({err})"
                    ))
                })?
            }
        }
        other => {
            return Err(RedDBError::Query(format!(
                "tenant auto-fill: root column must be JSON / NULL, got {other:?}"
            )));
        }
    };

    // Navigate path segments, creating intermediate objects on demand.
    let segments: Vec<&str> = tail.split('.').collect();
    let mut cursor: &mut crate::json::Value = &mut root;
    for (i, seg) in segments.iter().enumerate() {
        let is_last = i + 1 == segments.len();
        let map = match cursor {
            crate::json::Value::Object(m) => m,
            _ => {
                return Err(RedDBError::Query(format!(
                    "tenant auto-fill: segment '{seg}' is not inside an object"
                )));
            }
        };
        if is_last {
            map.insert(
                seg.to_string(),
                crate::json::Value::String(tenant_id.to_string()),
            );
            break;
        }
        cursor = map
            .entry(seg.to_string())
            .or_insert_with(|| crate::json::Value::Object(Default::default()));
    }

    let bytes = crate::json::to_vec(&root).map_err(|err| {
        RedDBError::Query(format!(
            "tenant auto-fill: failed to re-serialize JSON ({err})"
        ))
    })?;
    Ok(Value::Json(bytes))
}

#[cfg(test)]
mod tests {
    use crate::storage::schema::Value;
    use crate::{RedDBOptions, RedDBRuntime};

    #[test]
    fn update_where_id_in_with_hash_index_updates_expected_rows() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, score INT)")
            .unwrap();
        for id in 0..5 {
            rt.execute_query(&format!("INSERT INTO users (id, score) VALUES ({id}, 0)"))
                .unwrap();
        }
        rt.execute_query("CREATE INDEX idx_id ON users (id) USING HASH")
            .unwrap();

        let updated = rt
            .execute_query("UPDATE users SET score = 42 WHERE id IN (1,3,4)")
            .unwrap();
        assert_eq!(updated.affected_rows, 3);

        let selected = rt
            .execute_query("SELECT id, score FROM users ORDER BY id")
            .unwrap();
        let scores: Vec<(i64, i64)> = selected
            .result
            .records
            .iter()
            .map(|record| {
                let id = match record.get("id").unwrap() {
                    Value::Integer(value) => *value,
                    other => panic!("expected integer id, got {other:?}"),
                };
                let score = match record.get("score").unwrap() {
                    Value::Integer(value) => *value,
                    other => panic!("expected integer score, got {other:?}"),
                };
                (id, score)
            })
            .collect();
        assert_eq!(scores, vec![(0, 0), (1, 42), (2, 0), (3, 42), (4, 42)]);
    }

    /// Drives UPDATE through the shared `DmlTargetScan` module — the
    /// same code path DELETE uses (#51, #52). Exercises the indexed
    /// equality fast-path (WHERE id = N with a HASH index), the
    /// unindexed range scan (WHERE score > N), and the no-WHERE
    /// full-scan branch to confirm the extracted "find target rows"
    /// loop preserves affected-row counts and the resulting row state.
    #[test]
    fn update_routes_through_dml_target_scan_for_indexed_and_scan_paths() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE items (id INT, score INT)")
            .unwrap();
        for id in 0..5 {
            rt.execute_query(&format!(
                "INSERT INTO items (id, score) VALUES ({id}, {})",
                id * 10
            ))
            .unwrap();
        }
        rt.execute_query("CREATE INDEX idx_items_id ON items (id) USING HASH")
            .unwrap();

        // Indexed equality UPDATE — hits the hash fast-path inside
        // DmlTargetScan::find_target_ids. id=2 has score=20, drop it
        // below the score>25 cutoff so the next assertion stays clean.
        let updated_one = rt
            .execute_query("UPDATE items SET score = 5 WHERE id = 2")
            .unwrap();
        assert_eq!(updated_one.affected_rows, 1);

        // Unindexed scan UPDATE — bumps everyone with score > 25,
        // i.e. ids 3 and 4 (scores 30, 40). Goes through the
        // zoned/full-scan branch.
        let updated_many = rt
            .execute_query("UPDATE items SET score = 7 WHERE score > 25")
            .unwrap();
        assert_eq!(updated_many.affected_rows, 2);

        let snapshot = rt
            .execute_query("SELECT id, score FROM items ORDER BY id")
            .unwrap();
        let pairs: Vec<(i64, i64)> = snapshot
            .result
            .records
            .iter()
            .map(|record| {
                let id = match record.get("id").unwrap() {
                    Value::Integer(value) => *value,
                    other => panic!("expected integer id, got {other:?}"),
                };
                let score = match record.get("score").unwrap() {
                    Value::Integer(value) => *value,
                    other => panic!("expected integer score, got {other:?}"),
                };
                (id, score)
            })
            .collect();
        assert_eq!(pairs, vec![(0, 0), (1, 10), (2, 5), (3, 7), (4, 7)]);

        // Full-scan UPDATE with no WHERE rewrites every remaining row.
        let updated_all = rt.execute_query("UPDATE items SET score = 1").unwrap();
        assert_eq!(updated_all.affected_rows, 5);
        let after = rt
            .execute_query("SELECT score FROM items ORDER BY id")
            .unwrap();
        let scores: Vec<i64> = after
            .result
            .records
            .iter()
            .map(|record| match record.get("score").unwrap() {
                Value::Integer(value) => *value,
                other => panic!("expected integer score, got {other:?}"),
            })
            .collect();
        assert_eq!(scores, vec![1, 1, 1, 1, 1]);
    }

    /// Drives DELETE through the new `DmlTargetScan` module. Exercises
    /// both the index fast-path (WHERE id = N with a HASH index) and
    /// the unindexed scan path (WHERE score > N) to confirm the
    /// extracted "find target rows" loop preserves the affected-row
    /// count and which rows survive.
    #[test]
    fn delete_routes_through_dml_target_scan_for_indexed_and_scan_paths() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE items (id INT, score INT)")
            .unwrap();
        for id in 0..5 {
            rt.execute_query(&format!(
                "INSERT INTO items (id, score) VALUES ({id}, {})",
                id * 10
            ))
            .unwrap();
        }
        rt.execute_query("CREATE INDEX idx_items_id ON items (id) USING HASH")
            .unwrap();

        // Indexed equality DELETE — hits the hash fast-path inside
        // DmlTargetScan::find_target_ids.
        let deleted_one = rt.execute_query("DELETE FROM items WHERE id = 2").unwrap();
        assert_eq!(deleted_one.affected_rows, 1);

        // Unindexed scan DELETE — drops everyone with score > 25,
        // i.e. ids 3 and 4 (scores 30, 40). Goes through the
        // zoned/full-scan branch.
        let deleted_many = rt
            .execute_query("DELETE FROM items WHERE score > 25")
            .unwrap();
        assert_eq!(deleted_many.affected_rows, 2);

        let surviving = rt
            .execute_query("SELECT id FROM items ORDER BY id")
            .unwrap();
        let ids: Vec<i64> = surviving
            .result
            .records
            .iter()
            .map(|record| match record.get("id").unwrap() {
                Value::Integer(value) => *value,
                other => panic!("expected integer id, got {other:?}"),
            })
            .collect();
        assert_eq!(ids, vec![0, 1]);

        // Sanity: full-scan DELETE with no WHERE clears the rest.
        let deleted_rest = rt.execute_query("DELETE FROM items").unwrap();
        assert_eq!(deleted_rest.affected_rows, 2);
        let empty = rt.execute_query("SELECT id FROM items").unwrap();
        assert!(empty.result.records.is_empty());
    }

    /// CollectionContract gate (#49 + #50): APPEND ONLY tables accept
    /// INSERT but reject UPDATE and DELETE with the documented
    /// operator-facing error strings. Drives all three DML verbs so
    /// the centralized gate is exercised end-to-end.
    #[test]
    fn collection_contract_gate_blocks_update_and_delete_on_append_only() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE events (id INT, payload TEXT) APPEND ONLY")
            .unwrap();

        // INSERT must succeed — APPEND ONLY exists precisely to allow
        // appends. The gate should be a no-op for INSERT.
        let inserted = rt
            .execute_query("INSERT INTO events (id, payload) VALUES (1, 'hello')")
            .unwrap();
        assert_eq!(inserted.affected_rows, 1);

        // UPDATE is rejected with the gate's UPDATE-specific message.
        let update_err = rt
            .execute_query("UPDATE events SET payload = 'mut' WHERE id = 1")
            .unwrap_err();
        let msg = format!("{update_err}");
        assert!(
            msg.contains("APPEND ONLY") && msg.contains("UPDATE is rejected"),
            "expected UPDATE rejection message, got: {msg}"
        );

        // DELETE is rejected with the gate's DELETE-specific message.
        let delete_err = rt
            .execute_query("DELETE FROM events WHERE id = 1")
            .unwrap_err();
        let msg = format!("{delete_err}");
        assert!(
            msg.contains("APPEND ONLY") && msg.contains("DELETE is rejected"),
            "expected DELETE rejection message, got: {msg}"
        );

        // Row should still be present — neither rejected mutation
        // touched storage.
        let surviving = rt.execute_query("SELECT id FROM events").unwrap();
        assert_eq!(surviving.result.records.len(), 1);
    }

    /// CollectionContract gate: tables without an APPEND ONLY contract
    /// permit INSERT, UPDATE, and DELETE — the gate's default branch
    /// is a true pass-through, not an accidental block.
    #[test]
    fn collection_contract_gate_allows_all_verbs_on_unrestricted_table() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE notes (id INT, body TEXT)")
            .unwrap();

        rt.execute_query("INSERT INTO notes (id, body) VALUES (1, 'a')")
            .unwrap();
        let updated = rt
            .execute_query("UPDATE notes SET body = 'b' WHERE id = 1")
            .unwrap();
        assert_eq!(updated.affected_rows, 1);
        let deleted = rt.execute_query("DELETE FROM notes WHERE id = 1").unwrap();
        assert_eq!(deleted.affected_rows, 1);
    }

    #[test]
    fn insert_into_event_enabled_table_emits_event_to_configured_queue() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, email TEXT) WITH EVENTS (INSERT) TO audit_log",
        )
        .unwrap();

        let inserted = rt
            .execute_query("INSERT INTO users (id, email) VALUES (7, 'a@example.com')")
            .unwrap();
        assert_eq!(inserted.affected_rows, 1);

        let events = queue_payloads(&rt, "audit_log");
        assert_eq!(events.len(), 1);
        let event = events[0].as_object().expect("event payload object");
        assert!(event
            .get("event_id")
            .and_then(crate::json::Value::as_str)
            .is_some_and(|value| !value.is_empty()));
        assert_eq!(
            event.get("op").and_then(crate::json::Value::as_str),
            Some("insert")
        );
        assert_eq!(
            event.get("collection").and_then(crate::json::Value::as_str),
            Some("users")
        );
        assert_eq!(
            event.get("id").and_then(crate::json::Value::as_u64),
            Some(7)
        );
        assert!(event
            .get("ts")
            .and_then(crate::json::Value::as_u64)
            .is_some());
        assert!(event
            .get("lsn")
            .and_then(crate::json::Value::as_u64)
            .is_some());
        assert!(matches!(
            event.get("tenant"),
            Some(crate::json::Value::Null)
        ));
        assert!(matches!(
            event.get("before"),
            Some(crate::json::Value::Null)
        ));
        let after = event
            .get("after")
            .and_then(crate::json::Value::as_object)
            .expect("after object");
        assert_eq!(
            after.get("id").and_then(crate::json::Value::as_u64),
            Some(7)
        );
        assert_eq!(
            after.get("email").and_then(crate::json::Value::as_str),
            Some("a@example.com")
        );
    }

    #[test]
    fn multi_row_insert_emits_one_insert_event_per_row_in_order() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, email TEXT) WITH EVENTS")
            .unwrap();

        rt.execute_query(
            "INSERT INTO users (id, email) VALUES (1, 'a@example.com'), (2, 'b@example.com')",
        )
        .unwrap();

        let events = queue_payloads(&rt, "users_events");
        assert_eq!(events.len(), 2);
        let mut previous_lsn = 0;
        for (event, expected_id) in events.iter().zip([1_u64, 2]) {
            let object = event.as_object().expect("event payload object");
            assert_eq!(
                object.get("op").and_then(crate::json::Value::as_str),
                Some("insert")
            );
            assert_eq!(
                object.get("id").and_then(crate::json::Value::as_u64),
                Some(expected_id)
            );
            let lsn = object
                .get("lsn")
                .and_then(crate::json::Value::as_u64)
                .expect("event lsn");
            assert!(
                lsn > previous_lsn,
                "event LSNs should increase in row order"
            );
            previous_lsn = lsn;
            let after = object
                .get("after")
                .and_then(crate::json::Value::as_object)
                .expect("after object");
            assert_eq!(
                after.get("id").and_then(crate::json::Value::as_u64),
                Some(expected_id)
            );
        }
    }

    fn queue_payloads(rt: &RedDBRuntime, queue: &str) -> Vec<crate::json::Value> {
        let result = rt
            .execute_query(&format!("QUEUE PEEK {queue} 10"))
            .expect("peek queue");
        result
            .result
            .records
            .iter()
            .map(
                |record| match record.get("payload").expect("payload column") {
                    Value::Json(bytes) => crate::json::from_slice(bytes).expect("json payload"),
                    other => panic!("expected JSON queue payload, got {other:?}"),
                },
            )
            .collect()
    }

    // ── #112: auto-index user `id` on first insert ─────────────────────

    /// First insert into a fresh collection that carries a column named
    /// `id` registers an implicit HASH index on `id`. Subsequent inserts
    /// populate it transparently, and `WHERE id = N` lookups exercise
    /// the hash-index fast path in `DmlTargetScan::find_target_ids`.
    ///
    /// This is the load-bearing acceptance test for #112 — without the
    /// hook, `find_index_for_column` returns `None` and DELETE/UPDATE
    /// fall through to a full segment scan (the 4× perf gap documented
    /// in `docs/perf/delete-sequential-2026-05-06.md`).
    #[test]
    fn auto_index_id_fires_on_first_insert() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE bench_users (id INT, score INT)")
            .unwrap();

        // Pre-condition: no index on `id` yet.
        assert!(
            rt.index_store_ref()
                .find_index_for_column("bench_users", "id")
                .is_none(),
            "freshly created collection should not have an `id` index"
        );

        // Single-row INSERT — drives `MutationEngine::append_one`.
        rt.execute_query("INSERT INTO bench_users (id, score) VALUES (1, 10)")
            .unwrap();

        // Post-condition: hash index registered on `id`.
        let registered = rt
            .index_store_ref()
            .find_index_for_column("bench_users", "id")
            .expect("auto-index hook should have registered idx_id on first insert");
        assert_eq!(registered.name, "idx_id");
        assert_eq!(registered.collection, "bench_users");
        assert_eq!(registered.columns, vec!["id".to_string()]);
        assert!(matches!(
            registered.method,
            super::super::index_store::IndexMethodKind::Hash
        ));

        // Subsequent inserts populate the index; `WHERE id = N` should
        // resolve via the hash fast path and round-trip every row.
        for id in 2..=5 {
            rt.execute_query(&format!(
                "INSERT INTO bench_users (id, score) VALUES ({id}, {})",
                id * 10
            ))
            .unwrap();
        }
        for id in 1..=5 {
            let result = rt
                .execute_query(&format!("SELECT score FROM bench_users WHERE id = {id}"))
                .unwrap();
            assert_eq!(
                result.result.records.len(),
                1,
                "id={id} should match one row"
            );
        }

        // Delete via the hash fast-path — exactly the bench scenario the
        // perf doc identified as the 4× regression. With the index
        // present, `find_target_ids` short-circuits before
        // `for_each_entity_zoned` runs.
        let deleted = rt
            .execute_query("DELETE FROM bench_users WHERE id = 3")
            .unwrap();
        assert_eq!(deleted.affected_rows, 1);
    }

    /// Bulk INSERT (the multi-row VALUES path) drives
    /// `MutationEngine::append_batch`. The hook must fire there too —
    /// otherwise the batch entry points (gRPC binary bulk, HTTP bulk,
    /// wire bulk INSERT) skip auto-indexing entirely.
    #[test]
    fn auto_index_id_fires_on_first_bulk_insert() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE bench_bulk (id INT, score INT)")
            .unwrap();

        rt.execute_query("INSERT INTO bench_bulk (id, score) VALUES (1, 10), (2, 20), (3, 30)")
            .unwrap();

        let registered = rt
            .index_store_ref()
            .find_index_for_column("bench_bulk", "id")
            .expect("auto-index hook should fire on first bulk insert");
        assert_eq!(registered.name, "idx_id");

        // Every row populated via `index_entity_insert_batch`.
        for id in 1..=3 {
            let result = rt
                .execute_query(&format!("SELECT score FROM bench_bulk WHERE id = {id}"))
                .unwrap();
            assert_eq!(result.result.records.len(), 1);
        }
    }

    /// Hook is a no-op when the row carries no `id` column. Conservative
    /// match (case-sensitive `id`) — `Id`, `ID`, and `red_entity_id`
    /// don't trigger it.
    #[test]
    fn auto_index_id_skips_when_no_id_column() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE plain (uid INT, label TEXT)")
            .unwrap();
        rt.execute_query("INSERT INTO plain (uid, label) VALUES (1, 'a')")
            .unwrap();

        assert!(rt
            .index_store_ref()
            .find_index_for_column("plain", "id")
            .is_none());
        assert!(rt
            .index_store_ref()
            .find_index_for_column("plain", "uid")
            .is_none());
    }

    /// Hook only fires once per collection. If an explicit
    /// `CREATE INDEX ... USING BTREE` already covers `id`, the hook
    /// detects it via `find_index_for_column` and does NOT clobber it
    /// with a HASH index on the next insert.
    #[test]
    fn auto_index_id_skips_when_index_already_exists() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE pre (id INT, score INT)")
            .unwrap();
        // User-declared BTREE index on `id` before any insert.
        rt.execute_query("CREATE INDEX user_idx ON pre (id) USING BTREE")
            .unwrap();
        rt.execute_query("INSERT INTO pre (id, score) VALUES (1, 10)")
            .unwrap();

        let registered = rt
            .index_store_ref()
            .find_index_for_column("pre", "id")
            .expect("user index should still be there");
        assert_eq!(
            registered.name, "user_idx",
            "auto-index hook must not overwrite an existing index"
        );
    }

    /// Implicit `idx_id` is reaped when the collection drops. The
    /// existing `execute_drop_table` walks `list_indices` and drops every
    /// entry — confirm the auto-created index participates.
    #[test]
    fn auto_index_id_dropped_with_collection() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE ephemeral (id INT, score INT)")
            .unwrap();
        rt.execute_query("INSERT INTO ephemeral (id, score) VALUES (1, 10)")
            .unwrap();
        assert!(rt
            .index_store_ref()
            .find_index_for_column("ephemeral", "id")
            .is_some());

        rt.execute_query("DROP TABLE ephemeral").unwrap();

        assert!(
            rt.index_store_ref()
                .find_index_for_column("ephemeral", "id")
                .is_none(),
            "implicit `idx_id` must be reaped when its collection drops"
        );
    }

    /// Opt-out via `RedDBOptions::with_auto_index_id(false)` (which
    /// forwards to `UnifiedStoreConfig::auto_index_id`). With the knob
    /// off, first insert leaves the collection without an `id` index —
    /// DELETE/UPDATE fall back to the scan path.
    #[test]
    fn auto_index_id_disabled_by_config() {
        let opts = RedDBOptions::in_memory().with_auto_index_id(false);
        let rt = RedDBRuntime::with_options(opts).unwrap();

        rt.execute_query("CREATE TABLE off (id INT, score INT)")
            .unwrap();
        rt.execute_query("INSERT INTO off (id, score) VALUES (1, 10)")
            .unwrap();

        assert!(
            rt.index_store_ref()
                .find_index_for_column("off", "id")
                .is_none(),
            "with auto_index_id=false, no implicit index should be created"
        );
    }

    // ── #293: UPDATE / DELETE events ─────────────────────────────────────

    #[test]
    fn update_single_row_emits_update_event() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, name TEXT) WITH EVENTS (UPDATE) TO audit_log",
        )
        .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
            .unwrap();

        rt.execute_query("UPDATE users SET name = 'Bob' WHERE id = 1")
            .unwrap();

        let events = queue_payloads(&rt, "audit_log");
        assert_eq!(events.len(), 1, "expected exactly 1 update event");
        let event = events[0].as_object().expect("event payload object");
        assert_eq!(
            event.get("op").and_then(crate::json::Value::as_str),
            Some("update")
        );
        assert_eq!(
            event.get("collection").and_then(crate::json::Value::as_str),
            Some("users")
        );
        assert!(event
            .get("event_id")
            .and_then(crate::json::Value::as_str)
            .is_some_and(|v| !v.is_empty()));
        let before = event
            .get("before")
            .and_then(crate::json::Value::as_object)
            .expect("before must be an object");
        let after = event
            .get("after")
            .and_then(crate::json::Value::as_object)
            .expect("after must be an object");
        assert_eq!(
            before.get("name").and_then(crate::json::Value::as_str),
            Some("Alice"),
            "before.name should be the old value"
        );
        assert_eq!(
            after.get("name").and_then(crate::json::Value::as_str),
            Some("Bob"),
            "after.name should be the new value"
        );
    }

    #[test]
    fn update_event_only_includes_changed_fields() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, name TEXT, email TEXT) WITH EVENTS (UPDATE) TO evts",
        )
        .unwrap();
        rt.execute_query("INSERT INTO users (id, name, email) VALUES (1, 'Alice', 'a@x.com')")
            .unwrap();

        rt.execute_query("UPDATE users SET name = 'Bob' WHERE id = 1")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert_eq!(events.len(), 1);
        let event = events[0].as_object().unwrap();
        let before = event
            .get("before")
            .and_then(crate::json::Value::as_object)
            .unwrap();
        let after = event
            .get("after")
            .and_then(crate::json::Value::as_object)
            .unwrap();
        // Only changed field included.
        assert!(
            before.contains_key("name"),
            "before must include changed field"
        );
        assert!(
            after.contains_key("name"),
            "after must include changed field"
        );
        // Unchanged fields must not appear.
        assert!(
            !before.contains_key("email"),
            "before must not include unchanged email"
        );
        assert!(
            !after.contains_key("email"),
            "after must not include unchanged email"
        );
    }

    #[test]
    fn multi_row_update_emits_one_event_per_row() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE items (id INT, status TEXT) WITH EVENTS (UPDATE) TO evts")
            .unwrap();
        rt.execute_query(
            "INSERT INTO items (id, status) VALUES (1, 'new'), (2, 'new'), (3, 'new')",
        )
        .unwrap();

        rt.execute_query("UPDATE items SET status = 'done'")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert_eq!(events.len(), 3, "expected one update event per row");
        for event in &events {
            let obj = event.as_object().unwrap();
            assert_eq!(
                obj.get("op").and_then(crate::json::Value::as_str),
                Some("update")
            );
        }
    }

    #[test]
    fn delete_single_row_emits_delete_event() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS (DELETE) TO del_log")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (42, 'Alice')")
            .unwrap();

        rt.execute_query("DELETE FROM users WHERE id = 42").unwrap();

        let events = queue_payloads(&rt, "del_log");
        assert_eq!(events.len(), 1);
        let event = events[0].as_object().expect("event payload object");
        assert_eq!(
            event.get("op").and_then(crate::json::Value::as_str),
            Some("delete")
        );
        assert_eq!(
            event.get("collection").and_then(crate::json::Value::as_str),
            Some("users")
        );
        assert!(event
            .get("event_id")
            .and_then(crate::json::Value::as_str)
            .is_some_and(|v| !v.is_empty()));
        let before = event
            .get("before")
            .and_then(crate::json::Value::as_object)
            .expect("before must be an object for delete");
        assert_eq!(
            before.get("id").and_then(crate::json::Value::as_u64),
            Some(42)
        );
        assert_eq!(
            before.get("name").and_then(crate::json::Value::as_str),
            Some("Alice")
        );
        assert!(matches!(event.get("after"), Some(crate::json::Value::Null)));
    }

    #[test]
    fn multi_row_delete_emits_one_event_per_row() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE items (id INT, val INT) WITH EVENTS (DELETE) TO del_log")
            .unwrap();
        rt.execute_query("INSERT INTO items (id, val) VALUES (1, 10), (2, 20), (3, 30)")
            .unwrap();

        rt.execute_query("DELETE FROM items").unwrap();

        let events = queue_payloads(&rt, "del_log");
        assert_eq!(events.len(), 3, "expected one delete event per deleted row");
        for event in &events {
            let obj = event.as_object().unwrap();
            assert_eq!(
                obj.get("op").and_then(crate::json::Value::as_str),
                Some("delete")
            );
            assert!(matches!(obj.get("after"), Some(crate::json::Value::Null)));
        }
    }

    #[test]
    fn ops_filter_update_does_not_emit_on_insert_or_delete() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS (UPDATE) TO evts")
            .unwrap();

        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
            .unwrap();
        rt.execute_query("DELETE FROM users WHERE id = 1").unwrap();

        let events = queue_payloads(&rt, "evts");
        assert!(
            events.is_empty(),
            "UPDATE-only filter must not emit INSERT or DELETE events"
        );
    }

    // ── SUPPRESS EVENTS ────────────────────────────────────────────────────

    #[test]
    fn suppress_events_on_insert_emits_no_events() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO evts")
            .unwrap();

        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice') SUPPRESS EVENTS")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert!(
            events.is_empty(),
            "SUPPRESS EVENTS must prevent INSERT events"
        );
    }

    #[test]
    fn suppress_events_on_update_emits_no_events() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO evts")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice')")
            .unwrap();
        // drain the INSERT event
        let _ = queue_payloads(&rt, "evts");
        // Force pop to drain; simpler: just check new count after UPDATE
        rt.execute_query("QUEUE PURGE evts").unwrap();

        rt.execute_query("UPDATE users SET name = 'Bob' WHERE id = 1 SUPPRESS EVENTS")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert!(
            events.is_empty(),
            "SUPPRESS EVENTS must prevent UPDATE events"
        );
    }

    #[test]
    fn suppress_events_on_delete_emits_no_events() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query(
            "CREATE TABLE users (id INT, name TEXT) WITH EVENTS (INSERT, DELETE) TO evts",
        )
        .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice') SUPPRESS EVENTS")
            .unwrap();

        rt.execute_query("DELETE FROM users WHERE id = 1 SUPPRESS EVENTS")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert!(
            events.is_empty(),
            "SUPPRESS EVENTS must prevent DELETE events"
        );
    }

    #[test]
    fn normal_insert_after_suppress_still_emits() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, name TEXT) WITH EVENTS TO evts")
            .unwrap();

        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Alice') SUPPRESS EVENTS")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, name) VALUES (2, 'Bob')")
            .unwrap();

        let events = queue_payloads(&rt, "evts");
        assert_eq!(
            events.len(),
            1,
            "only the non-suppressed INSERT should emit"
        );
        assert_eq!(
            events[0].get("id").and_then(crate::json::Value::as_u64),
            Some(2)
        );
    }
}
