//! DML execution: INSERT, UPDATE, DELETE via SQL AST
//!
//! Implements `execute_insert`, `execute_update`, and `execute_delete` on
//! `RedDBRuntime`.  Each method translates the parsed AST into entity-level
//! operations through the existing `RuntimeEntityPort` trait so that all
//! cross-cutting concerns (WAL, indexing, replication) are automatically
//! applied.

use crate::application::entity::{
    AppliedEntityMutation, CreateDocumentInput, CreateEdgeInput, CreateKvInput, CreateNodeInput,
    CreateRowInput, CreateRowsBatchInput, CreateVectorInput, DeleteEntityInput,
    PatchEntityOperation, PatchEntityOperationType, RowUpdateColumnRule, RowUpdateContractPlan,
};
use crate::application::ports::{
    build_row_update_contract_plan, normalize_row_update_assignment_with_plan,
    normalize_row_update_value_for_rule, RuntimeEntityPort,
};
use crate::application::ttl_payload::has_internal_ttl_metadata;
use crate::presentation::entity_json::storage_value_to_json;
use crate::storage::query::ast::Expr;
use crate::storage::query::sql_lowering::{
    effective_delete_filter, effective_insert_rows, effective_update_filter, fold_expr_to_value,
};
use crate::storage::unified::MetadataValue;
use crate::storage::Metadata;

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
    fn delete_entities_batch(&self, collection: &str, ids: &[EntityId]) -> RedDBResult<u64> {
        if ids.is_empty() {
            return Ok(0);
        }

        let store = self.db().store();
        let deleted_ids = store
            .delete_batch(collection, ids)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if deleted_ids.is_empty() {
            return Ok(0);
        }

        for id in &deleted_ids {
            store.context_index().remove_entity(*id);
            self.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                collection,
                id.raw(),
                "entity",
            );
        }

        Ok(deleted_ids.len() as u64)
    }

    fn flush_update_chunk(&self, applied: &[AppliedEntityMutation]) {
        if applied.is_empty() {
            return;
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

        self.cdc_emit_prebuilt_batch(
            crate::replication::cdc::ChangeOperation::Update,
            "entity",
            applied.iter().map(|item| {
                (
                    item.collection.as_str(),
                    &item.entity,
                    item.metadata.as_ref(),
                )
            }),
            false,
        );
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
        let mut inserted_count: u64 = 0;
        let effective_rows =
            effective_insert_rows(query).map_err(|msg| RedDBError::Query(msg.to_string()))?;

        // Ensure the collection exists (auto-create on first insert).
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(&query.table);
        let declared_model = self
            .db()
            .collection_contract(&query.table)
            .map(|contract| contract.declared_model);

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
                rows.push(CreateRowInput {
                    collection: query.table.clone(),
                    fields,
                    metadata,
                    node_links: Vec::new(),
                    vector_links: Vec::new(),
                });
            }
            inserted_count = self
                .create_rows_batch(CreateRowsBatchInput {
                    collection: query.table.clone(),
                    rows,
                })?
                .len() as u64;
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
                        let (columns, values) = pairwise_columns_values(&node_values);
                        let label = find_column_value_string(&columns, &values, "label")?;
                        let node_type =
                            find_column_value_opt_string(&columns, &values, "node_type");
                        let properties = extract_remaining_properties(
                            &columns,
                            &values,
                            &["label", "node_type"],
                        );
                        let input = CreateNodeInput {
                            collection: query.table.clone(),
                            label,
                            node_type,
                            properties,
                            metadata,
                            embeddings: Vec::new(),
                            table_links: Vec::new(),
                            node_links: Vec::new(),
                        };
                        self.create_node(input)?;
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
                        let (columns, values) = pairwise_columns_values(&edge_values);
                        let label = find_column_value_string(&columns, &values, "label")?;
                        ensure_non_tree_structural_edge_label(&label)?;
                        let from_id = find_column_value_u64(&columns, &values, "from")?;
                        let to_id = find_column_value_u64(&columns, &values, "to")?;
                        let weight = find_column_value_f32_opt(&columns, &values, "weight");
                        let properties = extract_remaining_properties(
                            &columns,
                            &values,
                            &["label", "from", "to", "weight"],
                        );
                        let input = CreateEdgeInput {
                            collection: query.table.clone(),
                            label,
                            from: EntityId::new(from_id),
                            to: EntityId::new(to_id),
                            weight,
                            properties,
                            metadata,
                        };
                        self.create_edge(input)?;
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
                        let dense = find_column_value_vec_f32(&columns, &values, "dense")?;
                        let content = find_column_value_opt_string(&columns, &values, "content");
                        let input = CreateVectorInput {
                            collection: query.table.clone(),
                            dense,
                            content,
                            metadata,
                            link_row: None,
                            link_node: None,
                        };
                        self.create_vector(input)?;
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
                            .map_err(|e| RedDBError::Query(format!("invalid JSON body: {e}")))?;
                        let input = CreateDocumentInput {
                            collection: query.table.clone(),
                            body,
                            metadata,
                            node_links: Vec::new(),
                            vector_links: Vec::new(),
                        };
                        self.create_document(input)?;
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
                        let input = CreateKvInput {
                            collection: query.table.clone(),
                            key,
                            value,
                            metadata,
                        };
                        self.create_kv(input)?;
                    }
                }

                inserted_count += 1;
            }
        }

        // Auto-embed pipeline: generate embeddings for specified fields
        if let Some(ref embed_config) = query.auto_embed {
            let store = self.inner.db.store();
            let provider = crate::ai::parse_provider(&embed_config.provider)?;
            let api_key = crate::ai::resolve_api_key_from_runtime(&provider, None, self)?;
            let model = embed_config.model.clone().unwrap_or_else(|| {
                std::env::var("REDDB_OPENAI_EMBEDDING_MODEL")
                    .ok()
                    .unwrap_or_else(|| crate::ai::DEFAULT_OPENAI_EMBEDDING_MODEL.to_string())
            });
            let api_base = provider.resolve_api_base();

            // Collect texts from the last inserted rows
            let manager = store
                .get_collection(&query.table)
                .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
            let entities = manager.query_all(|_| true);
            let recent: Vec<_> = entities
                .into_iter()
                .rev()
                .take(effective_rows.len())
                .collect();

            for entity in &recent {
                let mut texts = Vec::new();
                if let EntityData::Row(ref row) = entity.data {
                    if let Some(ref named) = row.named {
                        for field in &embed_config.fields {
                            if let Some(Value::Text(text)) = named.get(field) {
                                if !text.is_empty() {
                                    texts.push(text.clone());
                                }
                            }
                        }
                    }
                }
                if texts.is_empty() {
                    continue;
                }

                let combined = texts.join(" ");
                let response = crate::ai::openai_embeddings(crate::ai::OpenAiEmbeddingRequest {
                    api_key: api_key.clone(),
                    model: model.clone(),
                    inputs: vec![combined.clone()],
                    dimensions: None,
                    api_base: api_base.clone(),
                })?;

                if let Some(dense) = response.embeddings.into_iter().next() {
                    self.create_vector(CreateVectorInput {
                        collection: query.table.clone(),
                        dense,
                        content: Some(combined),
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

        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            inserted_count,
            "insert",
            "runtime-dml",
        ))
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

        let entity = UnifiedEntity::new(
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
        let db = self.db();
        let store = self.inner.db.store();
        let effective_filter = effective_update_filter(query);
        let compiled_plan = self.compile_update_plan(query)?;

        // ── FAST PATH: UPDATE ... WHERE _entity_id = N ──
        // Direct entity lookup instead of full collection scan.
        if let Some(entity_id) = query_exec::extract_entity_id_from_filter(&effective_filter) {
            let manager = store
                .get_collection(&query.table)
                .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
            if let Some(entity) = manager.get(EntityId::new(entity_id)) {
                let assignments =
                    self.materialize_update_assignments_for_entity(query, &entity, &compiled_plan)?;
                let applied = self.apply_materialized_update_for_entity(
                    query.table.clone(),
                    entity,
                    &compiled_plan,
                    assignments,
                )?;
                self.persist_update_chunk(std::slice::from_ref(&applied))?;
                self.flush_update_chunk(&[applied]);
                self.note_table_write(&query.table);
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    1,
                    "update",
                    "runtime-dml",
                ));
            }
            return Ok(RuntimeQueryResult::dml_result(
                raw_query.to_string(),
                0,
                "update",
                "runtime-dml",
            ));
        }

        // ── FAST PATH: UPDATE ... WHERE <indexed_col> = <value> ──
        // When the filter is a single equality predicate on a hash-indexed column,
        // use the index to find the matching entity IDs in O(log N) instead of
        // scanning the full collection.
        if let Some(ref filter) = effective_filter {
            let idx_store = self.index_store_ref();
            if let Some(entity_ids) =
                query_exec::try_hash_eq_lookup(filter, &query.table, idx_store)
            {
                if entity_ids.is_empty() {
                    return Ok(RuntimeQueryResult::dml_result(
                        raw_query.to_string(),
                        0,
                        "update",
                        "runtime-dml",
                    ));
                }
                let manager = store
                    .get_collection(&query.table)
                    .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
                let mut affected: u64 = 0;
                let table_name = query.table.as_str();
                for chunk in entity_ids.chunks(UPDATE_APPLY_CHUNK_SIZE) {
                    let mut applied_chunk = Vec::with_capacity(chunk.len());
                    for entity in manager.get_many(chunk).into_iter().flatten() {
                        if !query_exec::evaluate_entity_filter_with_db(
                            Some(db.as_ref()),
                            &entity,
                            filter,
                            table_name,
                            table_name,
                        ) {
                            continue;
                        }
                        let assignments = self.materialize_update_assignments_for_entity(
                            query,
                            &entity,
                            &compiled_plan,
                        )?;
                        applied_chunk.push(self.apply_materialized_update_for_entity(
                            query.table.clone(),
                            entity,
                            &compiled_plan,
                            assignments,
                        )?);
                    }
                    self.persist_update_chunk(&applied_chunk)?;
                    affected += applied_chunk.len() as u64;
                    self.flush_update_chunk(&applied_chunk);
                }
                if affected > 0 {
                    self.note_table_write(&query.table);
                }
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    affected,
                    "update",
                    "runtime-dml",
                ));
            }
        }

        // ── SORTED-INDEX PATH: UPDATE ... WHERE range/between/in predicate ──
        // Reuses the same sorted-index candidate generation as SELECT, then
        // rechecks the full filter before applying the update so compound
        // predicates remain correct.
        if let Some(ref filter) = effective_filter {
            let idx_store = self.index_store_ref();
            if let Some(entity_ids) =
                query_exec::try_sorted_index_lookup(filter, &query.table, idx_store, None)
            {
                if entity_ids.is_empty() {
                    return Ok(RuntimeQueryResult::dml_result(
                        raw_query.to_string(),
                        0,
                        "update",
                        "runtime-dml",
                    ));
                }
                let manager = store
                    .get_collection(&query.table)
                    .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
                let mut affected: u64 = 0;
                let table_name = query.table.as_str();
                for chunk in entity_ids.chunks(UPDATE_APPLY_CHUNK_SIZE) {
                    let mut applied_chunk = Vec::with_capacity(chunk.len());
                    for entity in manager.get_many(chunk).into_iter().flatten() {
                        if !query_exec::evaluate_entity_filter_with_db(
                            Some(db.as_ref()),
                            &entity,
                            filter,
                            table_name,
                            table_name,
                        ) {
                            continue;
                        }
                        let assignments = self.materialize_update_assignments_for_entity(
                            query,
                            &entity,
                            &compiled_plan,
                        )?;
                        applied_chunk.push(self.apply_materialized_update_for_entity(
                            query.table.clone(),
                            entity,
                            &compiled_plan,
                            assignments,
                        )?);
                    }
                    self.persist_update_chunk(&applied_chunk)?;
                    affected += applied_chunk.len() as u64;
                    self.flush_update_chunk(&applied_chunk);
                }
                if affected > 0 {
                    self.note_table_write(&query.table);
                }
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    affected,
                    "update",
                    "runtime-dml",
                ));
            }
        }

        let manager = store
            .get_collection(&query.table)
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

        // Collect matching entity IDs first while holding only read locks, then
        // fetch/apply in bounded chunks so bulk UPDATEs don't clone the whole
        // matching set at once.
        let mut ids_to_update = Vec::new();
        let table_name = query.table.as_str();
        if let Some(ref filter) = effective_filter {
            let mut owned_zone_preds = Vec::new();
            query_exec::extract_zone_predicates(filter, &mut owned_zone_preds);
            let zone_preds: Vec<_> = owned_zone_preds
                .iter()
                .map(|(column, value, kind)| {
                    (
                        column.as_str(),
                        match kind {
                            crate::storage::unified::segment::ZoneColPredKind::Eq => {
                                crate::storage::unified::segment::ZoneColPred::Eq(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Gt => {
                                crate::storage::unified::segment::ZoneColPred::Gt(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Gte => {
                                crate::storage::unified::segment::ZoneColPred::Gte(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Lt => {
                                crate::storage::unified::segment::ZoneColPred::Lt(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Lte => {
                                crate::storage::unified::segment::ZoneColPred::Lte(value)
                            }
                        },
                    )
                })
                .collect();

            manager.for_each_entity_zoned(&zone_preds, |entity| {
                if query_exec::evaluate_entity_filter_with_db(
                    Some(db.as_ref()),
                    entity,
                    filter,
                    table_name,
                    table_name,
                ) {
                    ids_to_update.push(entity.id);
                }
                true
            });
        } else {
            manager.for_each_entity(|entity| {
                ids_to_update.push(entity.id);
                true
            });
        }

        let mut affected: u64 = 0;
        for chunk in ids_to_update.chunks(UPDATE_APPLY_CHUNK_SIZE) {
            let mut applied_chunk = Vec::with_capacity(chunk.len());
            for entity in manager.get_many(chunk).into_iter().flatten() {
                let assignments =
                    self.materialize_update_assignments_for_entity(query, &entity, &compiled_plan)?;
                applied_chunk.push(self.apply_materialized_update_for_entity(
                    query.table.clone(),
                    entity,
                    &compiled_plan,
                    assignments,
                )?);
            }
            self.persist_update_chunk(&applied_chunk)?;
            affected += applied_chunk.len() as u64;
            self.flush_update_chunk(&applied_chunk);
        }

        if affected > 0 {
            self.note_table_write(&query.table);
        }

        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            affected,
            "update",
            "runtime-dml",
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
                    static_metadata_assignments.push((
                        metadata_key.to_string(),
                        sql_literal_to_metadata_value(metadata_key, &value)?,
                    ));
                } else {
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
                assignments.dynamic_metadata_assignments.push((
                    metadata_key.to_string(),
                    sql_literal_to_metadata_value(metadata_key, &value)?,
                ));
            } else {
                assignments.dynamic_field_assignments.push((
                    assignment.column.clone(),
                    normalize_row_update_value_for_rule(
                        &query.table,
                        value,
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
        let db = self.db();
        let store = self.inner.db.store();
        let effective_filter = effective_delete_filter(query);

        // ── FAST PATH: DELETE ... WHERE _entity_id = N ──
        if let Some(entity_id) = query_exec::extract_entity_id_from_filter(&effective_filter) {
            let manager = store
                .get_collection(&query.table)
                .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
            if manager.get(EntityId::new(entity_id)).is_some() {
                self.delete_entity(DeleteEntityInput {
                    collection: query.table.clone(),
                    id: EntityId::new(entity_id),
                })?;
                self.note_table_write(&query.table);
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    1,
                    "delete",
                    "runtime-dml",
                ));
            }
            return Ok(RuntimeQueryResult::dml_result(
                raw_query.to_string(),
                0,
                "delete",
                "runtime-dml",
            ));
        }

        // ── FAST PATH: DELETE ... WHERE <indexed_col> = <value> ──
        if let Some(ref filter) = effective_filter {
            let idx_store = self.index_store_ref();
            if let Some(entity_ids) =
                query_exec::try_hash_eq_lookup(filter, &query.table, idx_store)
            {
                let manager = store
                    .get_collection(&query.table)
                    .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
                let table_name = query.table.as_str();
                let mut affected: u64 = 0;
                let mut batch_ids = Vec::with_capacity(entity_ids.len());
                for entity in manager.get_many(&entity_ids).into_iter().flatten() {
                    if !query_exec::evaluate_entity_filter_with_db(
                        Some(db.as_ref()),
                        &entity,
                        filter,
                        table_name,
                        table_name,
                    ) {
                        continue;
                    }
                    batch_ids.push(entity.id);
                }
                affected += self.delete_entities_batch(&query.table, &batch_ids)?;
                if affected > 0 {
                    self.note_table_write(&query.table);
                }
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    affected,
                    "delete",
                    "runtime-dml",
                ));
            }
        }

        if let Some(ref filter) = effective_filter {
            let idx_store = self.index_store_ref();
            if let Some(entity_ids) =
                query_exec::try_sorted_index_lookup(filter, &query.table, idx_store, None)
            {
                if entity_ids.is_empty() {
                    return Ok(RuntimeQueryResult::dml_result(
                        raw_query.to_string(),
                        0,
                        "delete",
                        "runtime-dml",
                    ));
                }
                let manager = store
                    .get_collection(&query.table)
                    .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
                let table_name = query.table.as_str();
                let mut affected: u64 = 0;
                let mut batch_ids = Vec::with_capacity(entity_ids.len());
                for entity in manager.get_many(&entity_ids).into_iter().flatten() {
                    if !query_exec::evaluate_entity_filter_with_db(
                        Some(db.as_ref()),
                        &entity,
                        filter,
                        table_name,
                        table_name,
                    ) {
                        continue;
                    }
                    batch_ids.push(entity.id);
                }
                affected += self.delete_entities_batch(&query.table, &batch_ids)?;
                if affected > 0 {
                    self.note_table_write(&query.table);
                }
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    affected,
                    "delete",
                    "runtime-dml",
                ));
            }
        }

        let manager = store
            .get_collection(&query.table)
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

        let mut ids_to_delete = Vec::new();

        let table_name = query.table.as_str();
        if let Some(ref filter) = effective_filter {
            let mut owned_zone_preds = Vec::new();
            query_exec::extract_zone_predicates(filter, &mut owned_zone_preds);
            let zone_preds: Vec<_> = owned_zone_preds
                .iter()
                .map(|(column, value, kind)| {
                    (
                        column.as_str(),
                        match kind {
                            crate::storage::unified::segment::ZoneColPredKind::Eq => {
                                crate::storage::unified::segment::ZoneColPred::Eq(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Gt => {
                                crate::storage::unified::segment::ZoneColPred::Gt(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Gte => {
                                crate::storage::unified::segment::ZoneColPred::Gte(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Lt => {
                                crate::storage::unified::segment::ZoneColPred::Lt(value)
                            }
                            crate::storage::unified::segment::ZoneColPredKind::Lte => {
                                crate::storage::unified::segment::ZoneColPred::Lte(value)
                            }
                        },
                    )
                })
                .collect();

            manager.for_each_entity_zoned(&zone_preds, |entity| {
                if query_exec::evaluate_entity_filter_with_db(
                    Some(db.as_ref()),
                    entity,
                    filter,
                    table_name,
                    table_name,
                ) {
                    ids_to_delete.push(entity.id);
                }
                true
            });
        } else {
            manager.for_each_entity(|entity| {
                ids_to_delete.push(entity.id);
                true
            });
        }

        let mut affected: u64 = 0;
        for chunk in ids_to_delete.chunks(UPDATE_APPLY_CHUNK_SIZE) {
            affected += self.delete_entities_batch(&query.table, chunk)?;
        }

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
            value: Some(storage_value_to_json(&value)),
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
            value: Some(metadata_value_to_json(&value)),
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
            metadata.push((
                metadata_key.to_string(),
                sql_literal_to_metadata_value(metadata_key, value)?,
            ));
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
            Value::Text(s) => MetadataValue::String(s.clone()),
            Value::Integer(n) => MetadataValue::Int(*n),
            Value::Float(n) => MetadataValue::Float(*n),
            Value::Boolean(b) => MetadataValue::Bool(*b),
            _ => MetadataValue::String(value.to_string()),
        };
        metadata.push((key.clone(), meta_value));
    }
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
        Value::Text(s) => Ok(s),
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
                Value::Text(s) => Some(s.clone()),
                Value::Integer(n) => Some(n.to_string()),
                Value::Float(n) => Some(n.to_string()),
                _ => None,
            };
        }
    }
    None
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
        Value::Text(value) => MetadataValue::String(value.clone()),
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
