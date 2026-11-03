use super::*;
use crate::storage::query::unified::{sys_key_collection, sys_key_created_at, sys_key_updated_at};
pub(super) use reddb_engine::query::records::{
    apply_runtime_identity_hints, fill_document_body_fields, runtime_any_record_from_entity,
    runtime_any_record_from_entity_ref, runtime_documentish_value,
    runtime_entity_type_and_capabilities, runtime_record_capability_list, runtime_row_capabilities,
    runtime_row_capabilities_str, runtime_row_entity_type, runtime_row_has_document_capability,
    runtime_table_record_from_entity, runtime_table_record_from_entity_projected,
    runtime_table_record_from_entity_ref, runtime_table_record_from_entity_ref_projected,
    runtime_table_record_from_entity_ref_with_schema, runtime_table_record_lean,
    runtime_table_record_lean_in_collection, runtime_table_record_lean_ref,
    runtime_table_record_with_col_indices, set_runtime_entity_metadata,
};
use reddb_rql::sql_lowering::effective_table_projections;

fn set_source_collection(record: &mut UnifiedRecord, collection: &str) {
    record.set_arc(sys_key_collection(), Value::text(collection.to_string()));
}

#[inline(never)]
pub(super) fn scan_runtime_table_source_records(
    db: &RedDB,
    table: &str,
) -> RedDBResult<Vec<UnifiedRecord>> {
    scan_runtime_table_source_records_limited(db, table, None)
}

/// Like `scan_runtime_table_source_records` but stops after `limit`
/// visible rows when one is supplied. Pushes LIMIT down so that
/// `SELECT * FROM t LIMIT N` does not materialise the full table
/// before truncating — the hot case for dashboards and `\d`-style
/// exploration queries.
pub(super) fn scan_runtime_table_source_records_limited(
    db: &RedDB,
    table: &str,
    limit: Option<usize>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if is_universal_entity_source(table) {
        return scan_runtime_universal_source_records_limited(db, None, limit);
    }
    if let Some(limit) = limit {
        return scan_runtime_table_source_records_window(db, table, 0, limit);
    }

    let manager = db
        .store()
        .get_collection(table)
        .ok_or_else(|| RedDBError::NotFound(table.to_string()))?;
    if manager.has_backing() {
        return scan_runtime_table_source_records_window(db, table, 0, usize::MAX);
    }
    let snapshot = crate::runtime::impl_core::capture_current_snapshot();

    // A5 — parallel scan: for large unfiltered tables, collect entities once
    // then convert to records in parallel using the thread-pool coordinator.
    // The threshold guards against spawn overhead dominating on small tables.
    // Finite windows have already taken the bounded sequential path above.
    use crate::storage::query::executors::parallel_scan::MIN_PARALLEL_ROWS;
    let entity_count = manager.count();
    let go_parallel =
        !crate::runtime::function_budget::active() && entity_count >= MIN_PARALLEL_ROWS;
    if go_parallel {
        let schema = manager.column_schema();
        let table_name = table.to_string();
        let hydrate_store = db.store();
        let mut entities: Vec<crate::storage::unified::entity::UnifiedEntity> =
            Vec::with_capacity(entity_count);
        crate::runtime::function_budget::try_scan(
            |visit| manager.try_scan_for_each(snapshot.as_ref(), usize::MAX, visit),
            |e| {
                if db.replica_allows_entity_at_read(table, e) {
                    entities.push(e.clone());
                }
                true
            },
        )?;
        let records = crate::storage::query::executors::parallel_scan::parallel_scan_default(
            &entities,
            move |chunk| {
                chunk
                    .iter()
                    .filter_map(|e| {
                        let hydrated = crate::runtime::impl_timeseries::hydrate_timeseries_entity(
                            hydrate_store.as_ref(),
                            e,
                        );
                        let mut record = runtime_table_record_from_entity_ref_with_schema(
                            &hydrated,
                            schema.as_ref(),
                        )?;
                        set_source_collection(&mut record, &table_name);
                        Some(record)
                    })
                    .collect()
            },
        );
        return Ok(records);
    }

    // Issue #806 — the unbounded sequential scan flows through the
    // bounded-memory streaming channel. Collecting the stream reproduces
    // the previous `Vec` exactly (same materializer, same scan order)
    // while giving streaming consumers an O(chunk) resident set. The
    // limited path above keeps its early-stop loop so `SELECT * … LIMIT N`
    // still stops scanning as soon as it has N rows.
    stream_runtime_table_source_scan(
        db,
        table,
        crate::runtime::query_exec::DEFAULT_HIGH_WATER_MARK,
    )?
    .collect_records()
}

/// Materialize only the requested visible window. Skipped rows are visited in
/// storage scan order and released before the next row, without buffering a
/// prefix proportional to OFFSET or reserving the caller's entire LIMIT.
pub(super) fn scan_runtime_table_source_records_window(
    db: &RedDB,
    table: &str,
    offset: usize,
    limit: usize,
) -> RedDBResult<Vec<UnifiedRecord>> {
    if is_universal_entity_source(table) {
        return Ok(scan_runtime_universal_source_records_limited(
            db,
            None,
            Some(offset.saturating_add(limit)),
        )?
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect());
    }

    let store = db.store();
    let manager = store
        .get_collection(table)
        .ok_or_else(|| RedDBError::NotFound(table.to_string()))?;
    let mut records =
        Vec::with_capacity(limit.min(crate::runtime::query_exec::DEFAULT_HIGH_WATER_MARK));
    if limit == 0 {
        return Ok(records);
    }
    let snapshot = crate::runtime::impl_core::capture_current_snapshot();
    let schema = manager.column_schema();
    let mut remaining_offset = offset;
    crate::runtime::function_budget::try_scan(
        |visit| manager.try_scan_for_each(snapshot.as_ref(), usize::MAX, visit),
        |entity| {
            if !db.replica_allows_entity_at_read(table, entity) {
                return true;
            }
            let hydrated =
                crate::runtime::impl_timeseries::hydrate_timeseries_entity(store.as_ref(), entity);
            if let Some(mut record) =
                runtime_table_record_from_entity_ref_with_schema(&hydrated, schema.as_ref())
            {
                if remaining_offset > 0 {
                    remaining_offset -= 1;
                    return true;
                }
                set_source_collection(&mut record, table);
                records.push(record);
                if records.len() >= limit {
                    return false;
                }
            }
            true
        },
    )?;
    Ok(records)
}

pub(super) fn scan_runtime_universal_source_records_limited(
    db: &RedDB,
    candidate_collections: Option<&[String]>,
    limit: Option<usize>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    use crate::runtime::impl_core::capture_current_snapshot;

    // Cross-collection scans may run inside std::thread::scope — capture
    // the snapshot so worker threads see the same MVCC view instead of
    // defaulting to "no snapshot" (every row visible).
    let snapshot = capture_current_snapshot();
    let budget_collections = (crate::runtime::function_budget::active()
        || db.store().config().resident_memory_bytes_max.is_some())
    .then(|| db.store().list_collections());
    if let Some(collections) = candidate_collections.or(budget_collections.as_deref()) {
        let store = db.store();
        let mut records = match limit {
            Some(n) => {
                Vec::with_capacity(n.min(crate::runtime::query_exec::DEFAULT_HIGH_WATER_MARK))
            }
            None => Vec::new(),
        };
        for collection in collections {
            let Some(manager) = store.get_collection(collection) else {
                continue;
            };
            crate::runtime::function_budget::try_scan(
                |visit| manager.try_scan_for_each(snapshot.as_ref(), usize::MAX, visit),
                |entity| {
                    if records.len() >= limit.unwrap_or(usize::MAX) {
                        return false;
                    }
                    if !db.replica_allows_entity_at_read(collection, entity) {
                        return true;
                    }
                    let hydrated = crate::runtime::impl_timeseries::hydrate_timeseries_entity(
                        store.as_ref(),
                        entity,
                    );
                    if let Some(mut record) = runtime_any_record_from_entity_ref(&hydrated) {
                        set_source_collection(&mut record, collection);
                        records.push(record);
                    }
                    true
                },
            )?;
        }
        return Ok(records);
    }

    let records: Vec<UnifiedRecord> = db
        .store()
        .scan(snapshot.as_ref(), |_| true)
        .into_iter()
        .filter_map(|(collection, entity)| {
            if !db.replica_allows_entity_at_read(&collection, &entity) {
                return None;
            }
            let hydrated = crate::runtime::impl_timeseries::hydrate_timeseries_entity(
                db.store().as_ref(),
                &entity,
            );
            let mut record = runtime_any_record_from_entity(hydrated)?;
            set_source_collection(&mut record, &collection);
            Some(record)
        })
        .collect();
    let records = match limit {
        Some(n) if records.len() > n => records.into_iter().take(n).collect(),
        _ => records,
    };
    Ok(records)
}

/// Issue #806 — bounded-memory streaming scan for the unfiltered
/// full-table `SELECT *`.
///
/// Collects the visible entity *handles* once (the same cheap working
/// set the parallel scan path already builds), then converts each to a
/// [`UnifiedRecord`] lazily, so a streaming consumer holds only one
/// chunk at a time rather than the whole table. The rows are produced by
/// the same materializer and in the same scan order as the sequential
/// [`scan_runtime_table_source_records_limited`] path, so collecting the
/// stream reproduces the previous materialized result byte-for-byte.
/// Non-row entities that the materializer cannot render are skipped,
/// matching the sequential path's `if let Some(record)` filter.
pub(crate) fn stream_runtime_table_source_scan(
    db: &RedDB,
    table: &str,
    high_water_mark: usize,
) -> RedDBResult<super::query_exec::RowStream> {
    let manager = db
        .store()
        .get_collection(table)
        .ok_or_else(|| RedDBError::NotFound(table.to_string()))?;
    let schema = manager.column_schema();
    let table_name = table.to_string();

    let snapshot = crate::runtime::impl_core::capture_current_snapshot();
    if manager.has_backing() && !crate::runtime::function_budget::active() {
        let store = db.store();
        let cursor = {
            let _publication = store.vault_publication_lock.lock();
            manager.try_paged_entity_cursor(snapshot.clone())?
        };
        if let Some(cursor) = cursor {
            let is_replica = db.is_replica_role();
            let schema = schema.clone();
            let table = table_name.clone();
            let mut source = cursor.filter_map(move |entry| {
                let (entity, metadata) = match entry {
                    Ok(record) => record,
                    Err(error) => return Some(Err(RedDBError::from(error))),
                };
                if let Err(error) = crate::runtime::function_budget::charge(1) {
                    return Some(Err(error));
                }
                if is_replica
                    && metadata.as_ref().is_some_and(|metadata| {
                        RedDB::entity_metadata_expired_at(
                            &entity,
                            metadata,
                            crate::utils::now_unix_millis(),
                        )
                    })
                {
                    return None;
                }
                let hydrated = crate::runtime::impl_timeseries::hydrate_timeseries_entity(
                    store.as_ref(),
                    &entity,
                );
                let mut record =
                    runtime_table_record_from_entity_ref_with_schema(&hydrated, schema.as_ref())?;
                set_source_collection(&mut record, &table);
                Some(Ok(record))
            });
            let first = source.next().transpose()?;
            let columns = first
                .as_ref()
                .map(|record| {
                    record
                        .column_names()
                        .into_iter()
                        .map(|name| name.to_string())
                        .collect()
                })
                .unwrap_or_default();
            return Ok(super::query_exec::RowStream::from_lazy(
                columns,
                crate::storage::query::unified::QueryStats::default(),
                high_water_mark,
                Box::new(first.into_iter().map(Ok).chain(source)),
            ));
        }
    }
    let mut entities: Vec<crate::storage::unified::entity::UnifiedEntity> = Vec::new();
    crate::runtime::function_budget::try_scan(
        |visit| manager.try_scan_for_each(snapshot.as_ref(), usize::MAX, visit),
        |entity| {
            if db.replica_allows_entity_at_read(table, entity) {
                entities.push(entity.clone());
            }
            true
        },
    )?;

    // Columns mirror `collect_visible_columns`'s uniform-schema fast path:
    // sample the first materialized row's column set.
    let columns: Vec<String> = entities
        .first()
        .and_then(|entity| {
            let hydrated = crate::runtime::impl_timeseries::hydrate_timeseries_entity(
                db.store().as_ref(),
                entity,
            );
            runtime_table_record_from_entity_ref_with_schema(&hydrated, schema.as_ref())
        })
        .map(|mut record| {
            set_source_collection(&mut record, &table_name);
            record
                .column_names()
                .into_iter()
                .map(|name| name.to_string())
                .collect()
        })
        .unwrap_or_default();

    let map_schema = schema.clone();
    let map_table = table_name.clone();
    let hydrate_store = db.store();
    let source = entities.into_iter().filter_map(move |entity| {
        let hydrated = crate::runtime::impl_timeseries::hydrate_timeseries_entity(
            hydrate_store.as_ref(),
            &entity,
        );
        let mut record =
            runtime_table_record_from_entity_ref_with_schema(&hydrated, map_schema.as_ref())?;
        set_source_collection(&mut record, &map_table);
        Some(Ok(record))
    });

    Ok(super::query_exec::RowStream::from_lazy(
        columns,
        crate::storage::query::unified::QueryStats::default(),
        high_water_mark,
        Box::new(source),
    ))
}

pub(super) fn is_universal_entity_source(table: &str) -> bool {
    is_universal_query_source(table)
}

pub(super) fn runtime_search_collections(
    db: &RedDB,
    collections: Option<Vec<String>>,
) -> Option<Vec<String>> {
    match collections {
        Some(collections) if !collections.is_empty() => Some(collections),
        _ => Some(db.store().list_collections()),
    }
}

pub(super) fn runtime_filter_dsl_result(
    result: &mut DslQueryResult,
    entity_types: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
) {
    let entity_types = entity_types
        .map(|items| {
            items
                .into_iter()
                .map(|item| item.trim().to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .filter(|items| !items.is_empty());
    let capabilities = capabilities
        .map(|items| {
            items
                .into_iter()
                .map(|item| item.trim().to_ascii_lowercase())
                .filter(|item| !item.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .filter(|items| !items.is_empty());

    if entity_types.is_none() && capabilities.is_none() {
        return;
    }

    result.matches.retain(|item| {
        let (entity_type, item_capabilities) = runtime_entity_type_and_capabilities(&item.entity);
        let type_ok = entity_types
            .as_ref()
            .is_none_or(|accepted| accepted.contains(entity_type));
        let capability_ok = capabilities.as_ref().is_none_or(|accepted| {
            item_capabilities
                .iter()
                .any(|capability| accepted.contains(capability))
        });
        type_ok && capability_ok
    });

    normalize_runtime_dsl_result_scores(result);
}

pub(super) fn normalize_runtime_dsl_result_scores(result: &mut DslQueryResult) {
    for item in &mut result.matches {
        if let Some(final_score) = item
            .components
            .final_score
            .filter(|score| score.is_finite())
        {
            item.score = final_score;
        } else {
            item.components.final_score = Some(item.score);
        }
    }

    result.matches.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.entity.id.raw().cmp(&right.entity.id.raw()))
    });
}

pub(super) fn resolve_runtime_vector_source(
    runtime: &RedDBRuntime,
    source: &VectorSource,
) -> RedDBResult<Vec<f32>> {
    let db = &runtime.inner.db;
    match source {
        VectorSource::Literal(vector) => Ok(vector.clone()),
        VectorSource::Reference {
            collection,
            vector_id,
        } => {
            let entity = db
                .get(EntityId::new(*vector_id))
                .ok_or_else(|| RedDBError::NotFound(format!("vector:{vector_id}")))?;
            if entity.kind.collection() != collection
                || !super::impl_core::entity_visible_under_current_snapshot(&entity)
                || !runtime.search_entity_rls_allowed(collection, &entity, &mut HashMap::new())
            {
                return Err(RedDBError::NotFound(format!("vector:{vector_id}")));
            }
            match entity.data {
                EntityData::Vector(data) => Ok(data.dense),
                _ => Err(RedDBError::Query(format!(
                    "entity {vector_id} is not a vector source"
                ))),
            }
        }
        VectorSource::Text(text) => {
            eprintln!("DEBUG resolve_runtime_vector_source Text({text:?})");
            embed_runtime_vector_text(runtime, text)
        }
        VectorSource::Subquery(expr) => resolve_runtime_vector_subquery(runtime, expr.as_ref()),
    }
}

fn embed_runtime_vector_text(runtime: &RedDBRuntime, text: &str) -> RedDBResult<Vec<f32>> {
    let db = &runtime.inner.db;
    let kv_getter = |key: &str| -> RedDBResult<Option<String>> {
        match db.get_kv("red_config", key) {
            Some((Value::Text(value), _)) => Ok(Some(value.to_string())),
            Some(_) => Ok(None),
            None => Ok(None),
        }
    };

    // Embeddings follow the embeddings task pointer (ADR-0068 §5), not the
    // inference/ASK provider — a modality-incapable pointer fails loudly.
    let provider = crate::ai::resolve_embeddings_provider(&kv_getter)?;
    let model = crate::ai::resolve_embeddings_model(&provider, &kv_getter);

    // Issue #681 — when the default provider is `local`, route through
    // the registered local-model backend (registry from #678, cache from
    // #679, backend dispatch from #680). The resolved embedding flows
    // into the existing vector-search path; the dimension contract on
    // the target collection is validated downstream in
    // `validate_vector_query_shape`. We never silently fall back to a
    // remote provider on failure — propagating the error keeps the
    // operator-safe contract documented for `embed_local`.
    if matches!(provider, crate::ai::AiProvider::Local) {
        let response = crate::runtime::ai::local_embedding::embed_local_with_db(
            db,
            &model,
            vec![text.to_string()],
        )?;
        return response.embeddings.into_iter().next().ok_or_else(|| {
            RedDBError::Query("local embedding backend returned no vectors".to_string())
        });
    }

    let crate::ai::AiConnection { api_key, api_base } =
        crate::ai::resolve_connection_from_runtime(&provider, None, runtime)?;
    let transport = crate::runtime::ai::transport::AiTransport::new(
        crate::runtime::ai::transport::AiTransportConfig::default(),
    );
    let request = crate::ai::OpenAiEmbeddingRequest {
        api_key,
        model,
        inputs: vec![text.to_string()],
        dimensions: None,
        api_base,
    };
    let response = crate::runtime::ai::block_on_ai(async move {
        crate::ai::openai_embeddings_async(&transport, request).await
    })
    .and_then(|result| result)?;

    response
        .embeddings
        .into_iter()
        .next()
        .ok_or_else(|| RedDBError::Query("embedding API returned no vectors".to_string()))
}

fn resolve_runtime_vector_subquery(
    runtime: &RedDBRuntime,
    expr: &QueryExpr,
) -> RedDBResult<Vec<f32>> {
    let records = execute_runtime_vector_subquery_records(runtime, expr)?;
    let record = records
        .first()
        .ok_or_else(|| RedDBError::Query("vector source subquery returned no rows".to_string()))?;

    extract_runtime_vector_from_record(runtime, record)?.ok_or_else(|| {
        RedDBError::Query(
            "vector source subquery must return a vector value, vector reference, or vector entity id"
                .to_string(),
        )
    })
}

fn execute_runtime_vector_subquery_records(
    runtime: &RedDBRuntime,
    expr: &QueryExpr,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match expr {
        QueryExpr::Graph(_) | QueryExpr::Path(_) => {
            let plan = CanonicalPlanner::new(&runtime.inner.db).build(expr);
            execute_runtime_canonical_expr_node(runtime, &plan.root, expr)
        }
        QueryExpr::Table(_) | QueryExpr::Join(_) | QueryExpr::Vector(_) | QueryExpr::Hybrid(_) => {
            Ok(runtime.execute_query_expr(expr.clone())?.result.records)
        }
        other => Err(RedDBError::Query(format!(
            "vector source subqueries do not support {} statements",
            query_expr_name(other)
        ))),
    }
}

fn extract_runtime_vector_from_record(
    runtime: &RedDBRuntime,
    record: &UnifiedRecord,
) -> RedDBResult<Option<Vec<f32>>> {
    for key in ["dense", "vector", "embedding", "query_vector"] {
        if let Some(value) = record.get(key) {
            if let Some(vector) = resolve_runtime_vector_value(runtime, value)? {
                return Ok(Some(vector));
            }
        }
    }

    for key in ["rid", "entity_id", "vector_id", "id"] {
        if let Some(value) = record.get(key) {
            if let Some(vector) = resolve_runtime_vector_entity_value(runtime, value)? {
                return Ok(Some(vector));
            }
        }
    }

    if record.field_count() == 1 {
        if let Some((_, value)) = record.iter_fields().next() {
            if let Some(vector) = resolve_runtime_vector_value(runtime, value)? {
                return Ok(Some(vector));
            }
        }
    }

    for (_, value) in record.iter_fields() {
        match value {
            Value::Vector(vector) => return Ok(Some(vector.clone())),
            Value::VectorRef(_, vector_id) => {
                if let Some(vector) = runtime_vector_entity_by_id(runtime, *vector_id)? {
                    return Ok(Some(vector));
                }
            }
            _ => {}
        }
    }

    Ok(None)
}

fn resolve_runtime_vector_value(
    runtime: &RedDBRuntime,
    value: &Value,
) -> RedDBResult<Option<Vec<f32>>> {
    match value {
        Value::Vector(vector) => Ok(Some(vector.clone())),
        Value::Array(values) => Ok(Some(runtime_value_array_to_vector(values)?)),
        Value::Json(bytes) => Ok(Some(runtime_json_bytes_to_vector(bytes)?)),
        Value::VectorRef(_, vector_id) => runtime_vector_entity_by_id(runtime, *vector_id),
        Value::UnsignedInteger(vector_id) => runtime_vector_entity_by_id(runtime, *vector_id),
        Value::Integer(vector_id) if *vector_id >= 0 => {
            runtime_vector_entity_by_id(runtime, *vector_id as u64)
        }
        _ => Ok(None),
    }
}

fn resolve_runtime_vector_entity_value(
    runtime: &RedDBRuntime,
    value: &Value,
) -> RedDBResult<Option<Vec<f32>>> {
    match value {
        Value::UnsignedInteger(vector_id) => runtime_vector_entity_by_id(runtime, *vector_id),
        Value::Integer(vector_id) if *vector_id >= 0 => {
            runtime_vector_entity_by_id(runtime, *vector_id as u64)
        }
        Value::VectorRef(_, vector_id) => runtime_vector_entity_by_id(runtime, *vector_id),
        _ => Ok(None),
    }
}

fn runtime_vector_entity_by_id(
    runtime: &RedDBRuntime,
    vector_id: u64,
) -> RedDBResult<Option<Vec<f32>>> {
    let db = &runtime.inner.db;
    let Some(entity) = db.get(EntityId::new(vector_id)) else {
        return Ok(None);
    };
    if !super::impl_core::entity_visible_under_current_snapshot(&entity)
        || !runtime.search_entity_rls_allowed(
            entity.kind.collection(),
            &entity,
            &mut HashMap::new(),
        )
    {
        return Ok(None);
    }

    match entity.data {
        EntityData::Vector(vector) => Ok(Some(vector.dense)),
        _ => Ok(None),
    }
}

fn runtime_value_array_to_vector(values: &[Value]) -> RedDBResult<Vec<f32>> {
    values
        .iter()
        .map(|value| match value {
            Value::Float(number) => Ok(*number as f32),
            Value::Integer(number) => Ok(*number as f32),
            Value::UnsignedInteger(number) => Ok(*number as f32),
            other => Err(RedDBError::Query(format!(
                "vector arrays accept only numeric values, got {other:?}"
            ))),
        })
        .collect()
}

fn runtime_json_bytes_to_vector(bytes: &[u8]) -> RedDBResult<Vec<f32>> {
    crate::json::from_slice(bytes).map_err(|err| {
        RedDBError::Query(format!("vector JSON source must be a numeric array: {err}"))
    })
}

pub(super) fn runtime_vector_record_from_match(item: SimilarResult) -> UnifiedRecord {
    let mut record = UnifiedRecord::new();
    let (entity_type, capabilities) = runtime_entity_type_and_capabilities(&item.entity);
    record.set("entity_id", Value::UnsignedInteger(item.entity_id.raw()));
    record.set("rid", Value::UnsignedInteger(item.entity_id.raw()));
    record.set("score", Value::Float(item.score as f64));
    record.set("_score", Value::Float(item.score as f64));
    record.set("final_score", Value::Float(item.score as f64));
    record.set("distance", Value::Float(item.distance as f64));
    record.set("_distance", Value::Float(item.distance as f64));
    record.set("vector_distance", Value::Float(item.distance as f64));
    record.set("vector_score", Value::Float(item.score as f64));
    record.set("vector_similarity", Value::Float(item.score as f64));
    record.set(
        "collection",
        Value::text(item.entity.kind.collection().to_string()),
    );
    record.set(
        "red_collection",
        Value::text(item.entity.kind.collection().to_string()),
    );
    record.set(
        "red_kind",
        Value::text(item.entity.kind.storage_type().to_string()),
    );
    record.set_arc(
        sys_key_created_at(),
        Value::UnsignedInteger(item.entity.created_at),
    );
    record.set_arc(
        sys_key_updated_at(),
        Value::UnsignedInteger(item.entity.updated_at),
    );
    record.set(
        "red_sequence_id",
        Value::UnsignedInteger(item.entity.sequence_id),
    );
    set_runtime_entity_metadata(&mut record, entity_type, capabilities);
    apply_runtime_identity_hints(&mut record, &item.entity);

    match item.entity.data {
        EntityData::Vector(data) => {
            record.set("dimension", Value::UnsignedInteger(data.dense.len() as u64));
            if let Some(content) = data.content {
                record.set("content", Value::text(content));
            } else {
                record.set("content", Value::Null);
            }
        }
        EntityData::Row(row) => {
            record.set("dimension", Value::Null);
            if let Some(named) = row.named {
                for (key, value) in named {
                    record.set(&key, value);
                }
            }
        }
        EntityData::Node(node) => {
            record.set("dimension", Value::Null);
            for (key, value) in node.properties {
                record.set(&key, value);
            }
        }
        EntityData::Edge(edge) => {
            record.set("dimension", Value::Null);
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in edge.properties {
                record.set(&key, value);
            }
        }
        EntityData::TimeSeries(ts) => {
            record.set("dimension", Value::Null);
            record.set("metric", Value::text(ts.metric));
            record.set("timestamp_ns", Value::UnsignedInteger(ts.timestamp_ns));
            record.set("value", Value::Float(ts.value));
        }
        EntityData::QueueMessage(msg) => {
            record.set("dimension", Value::Null);
            record.set("payload", msg.payload);
            record.set("attempts", Value::UnsignedInteger(msg.attempts as u64));
            record.set("acked", Value::Boolean(msg.acked));
        }
    }

    record
}

pub(super) fn hybrid_candidate_keys(
    structured: &HashMap<String, UnifiedRecord>,
    vector: &HashMap<String, UnifiedRecord>,
    fusion: &FusionStrategy,
) -> Vec<String> {
    let structured_keys: BTreeSet<String> = structured.keys().cloned().collect();
    let vector_keys: BTreeSet<String> = vector.keys().cloned().collect();

    match fusion {
        FusionStrategy::Rerank { .. } => structured_keys.into_iter().collect(),
        FusionStrategy::FilterThenSearch
        | FusionStrategy::SearchThenFilter
        | FusionStrategy::Intersection => structured_keys
            .intersection(&vector_keys)
            .cloned()
            .collect(),
        FusionStrategy::Union { .. } | FusionStrategy::RRF { .. } => {
            structured_keys.union(&vector_keys).cloned().collect()
        }
    }
}

pub(super) fn runtime_record_identity_key(record: &UnifiedRecord) -> String {
    for key in [
        "_source_row",
        "_source_node",
        "_source_edge",
        "_source_entity",
        "_linked_identity",
    ] {
        if let Some(value) = record.get(key) {
            if let Some(fragment) = runtime_identity_fragment(value) {
                return format!("link:{fragment}");
            }
        }
    }

    if let Some(value) = record.get("entity_id").or_else(|| record.get("rid")) {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("entity:{fragment}");
        }
    }

    if let (Some(collection), Some(row_id)) = (
        record.get("red_collection").and_then(runtime_value_text),
        record.get("row_id").or_else(|| record.get("id")),
    ) {
        if let Some(fragment) = runtime_identity_fragment(row_id) {
            return format!("row:{collection}:{fragment}");
        }
    }

    if let Some((alias, node)) = record.nodes.iter().next() {
        return format!("node:{alias}:{}", node.id);
    }

    if let Some(value) = record
        .iter_fields()
        .find_map(|(key, value)| key.ends_with(".id").then_some(value))
    {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("ref:{fragment}");
        }
    }

    if let Some(value) = record.get("id") {
        if let Some(fragment) = runtime_identity_fragment(value) {
            return format!("id:{fragment}");
        }
    }

    if let Some(node) = record.paths.first().and_then(|path| path.nodes.first()) {
        return format!("path:{node}");
    }

    format!(
        "fingerprint:{:016x}",
        runtime_record_identity_fingerprint(record)
    )
}

fn runtime_record_identity_fingerprint(record: &UnifiedRecord) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    let mix = |hash: &mut u64, bytes: &[u8]| {
        for &byte in bytes {
            *hash ^= u64::from(byte);
            *hash = hash.wrapping_mul(0x100000001b3);
        }
    };

    let mut value_keys: Vec<_> = record.iter_fields().collect();
    value_keys.sort_by(|left, right| left.0.cmp(right.0));
    for (key, value) in value_keys {
        mix(&mut hash, key.as_bytes());
        mix(&mut hash, b"\x00");
        let bytes = value.to_bytes();
        mix(&mut hash, &bytes);
        mix(&mut hash, b"|");
    }

    let mut nodes: Vec<_> = record.nodes.iter().collect();
    nodes.sort_by(|left, right| left.0.cmp(right.0));
    for (alias, node) in nodes {
        mix(&mut hash, alias.as_bytes());
        mix(&mut hash, b"\x1f");
        mix(&mut hash, node.id.as_bytes());
        mix(&mut hash, node.label.as_bytes());
        mix(&mut hash, node.node_label.as_bytes());
        mix(&mut hash, b"|");
    }

    let mut edges: Vec<_> = record.edges.iter().collect();
    edges.sort_by(|left, right| left.0.cmp(right.0));
    for (alias, edge) in edges {
        mix(&mut hash, alias.as_bytes());
        mix(&mut hash, b"\x1f");
        mix(&mut hash, edge.from.as_bytes());
        mix(&mut hash, b"->");
        mix(&mut hash, edge.to.as_bytes());
        mix(&mut hash, edge.edge_label.as_bytes());
        mix(&mut hash, b"::");
        mix(&mut hash, format!("{:.8}", edge.weight).as_bytes());
        mix(&mut hash, b"|");
    }

    let mut paths: Vec<_> = record.paths.iter().collect();
    paths.sort_by(|left, right| {
        let left_node = left.nodes.first().map(|node| node.as_str()).unwrap_or("");
        let right_node = right.nodes.first().map(|node| node.as_str()).unwrap_or("");
        left_node.cmp(right_node)
    });
    for path in paths {
        for node in &path.nodes {
            mix(&mut hash, node.as_bytes());
            mix(&mut hash, b",");
        }
        mix(&mut hash, b"|");
        for edge in &path.edges {
            mix(&mut hash, edge.from.as_bytes());
            mix(&mut hash, b"->");
            mix(&mut hash, edge.to.as_bytes());
            mix(&mut hash, b"::");
            mix(&mut hash, edge.edge_label.as_bytes());
            mix(&mut hash, b":");
            mix(&mut hash, format!("{:.8}", edge.weight).as_bytes());
            mix(&mut hash, b",");
        }
        mix(&mut hash, b"|");
    }

    let mut vector_results: Vec<_> = record.vector_results.iter().collect();
    vector_results.sort_by(|left, right| {
        (left.collection.as_str(), left.id).cmp(&(right.collection.as_str(), right.id))
    });
    for result in vector_results {
        mix(&mut hash, result.collection.as_bytes());
        mix(&mut hash, b"#");
        mix(&mut hash, result.id.to_string().as_bytes());
        mix(&mut hash, b"::");
        mix(&mut hash, format!("{:.8}", result.distance).as_bytes());
    }

    hash
}

pub(super) fn runtime_identity_fragment(value: &Value) -> Option<String> {
    match value {
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::Float(value) => Some(value.to_string()),
        Value::Text(value) => Some(value.to_string()),
        Value::NodeRef(value) => Some(value.clone()),
        Value::EdgeRef(value) => Some(value.clone()),
        Value::RowRef(table, row_id) => Some(format!("{table}:{row_id}")),
        Value::VectorRef(collection, vector_id) => Some(format!("{collection}:{vector_id}")),
        _ => runtime_value_text(value),
    }
}

pub(super) fn runtime_metadata_entry(metadata: &Metadata) -> MetadataEntry {
    let mut entry = MetadataEntry::new();
    for (key, value) in metadata.iter() {
        if let Some(converted) = runtime_vector_metadata_value(value) {
            entry.insert(key.clone(), converted);
        }
    }
    entry
}

pub(super) fn runtime_vector_metadata_value(
    value: &UnifiedMetadataValue,
) -> Option<VectorMetadataValue> {
    match value {
        UnifiedMetadataValue::Null => Some(VectorMetadataValue::Null),
        UnifiedMetadataValue::Bool(value) => Some(VectorMetadataValue::Bool(*value)),
        UnifiedMetadataValue::Int(value) => Some(VectorMetadataValue::Integer(*value)),
        UnifiedMetadataValue::Float(value) => Some(VectorMetadataValue::Float(*value)),
        UnifiedMetadataValue::String(value) => Some(VectorMetadataValue::String(value.clone())),
        UnifiedMetadataValue::Timestamp(value) => Some(VectorMetadataValue::Integer(*value as i64)),
        UnifiedMetadataValue::Reference(target) => Some(VectorMetadataValue::String(
            runtime_ref_target_string(target),
        )),
        UnifiedMetadataValue::References(targets) => Some(VectorMetadataValue::String(
            targets
                .iter()
                .map(runtime_ref_target_string)
                .collect::<Vec<_>>()
                .join(","),
        )),
        UnifiedMetadataValue::Array(values) => Some(VectorMetadataValue::String(
            values
                .iter()
                .filter_map(runtime_vector_metadata_value)
                .map(|value| match value {
                    VectorMetadataValue::String(value) => value,
                    VectorMetadataValue::Integer(value) => value.to_string(),
                    VectorMetadataValue::Float(value) => value.to_string(),
                    VectorMetadataValue::Bool(value) => value.to_string(),
                    VectorMetadataValue::Null => "null".to_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
        )),
        UnifiedMetadataValue::Object(_)
        | UnifiedMetadataValue::Bytes(_)
        | UnifiedMetadataValue::Geo { .. } => None,
    }
}

pub(super) fn runtime_ref_target_string(target: &RefTarget) -> String {
    match target {
        RefTarget::TableRow { table, row_id } => format!("{table}:{row_id}"),
        RefTarget::Node {
            collection,
            node_id,
        } => format!("{collection}:{node_id}"),
        RefTarget::Edge {
            collection,
            edge_id,
        } => format!("{collection}:{edge_id}"),
        RefTarget::Vector {
            collection,
            vector_id,
        } => format!("{collection}:{vector_id}"),
        RefTarget::Entity {
            collection,
            entity_id,
        } => format!("{collection}:{entity_id}"),
    }
}

pub(super) fn runtime_entity_vector_similarity(entity: &UnifiedEntity, query: &[f32]) -> f32 {
    let mut best_similarity = 0.0f32;

    for emb in entity.embeddings() {
        best_similarity = best_similarity.max(cosine_similarity(query, &emb.vector));
    }

    if let EntityData::Vector(vec_data) = &entity.data {
        best_similarity = best_similarity.max(cosine_similarity(query, &vec_data.dense));
    }

    best_similarity
}

pub(super) fn runtime_structured_score(record: &UnifiedRecord, rank: Option<usize>) -> f64 {
    if let Some(value) = record
        .get("_score")
        .or_else(|| record.get("final_score"))
        .or_else(|| record.get("score"))
        .or_else(|| record.get("hybrid_score"))
        .or_else(|| record.get("graph_score"))
        .or_else(|| record.get("table_score"))
        .or_else(|| record.get("graph_match"))
        .or_else(|| record.get("vector_similarity"))
        .or_else(|| record.get("structured_match"))
        .or_else(|| record.get("text_relevance"))
    {
        if let Some(number) = runtime_value_number(value) {
            return number;
        }
    }

    rank.map(|value| 1.0 / (value as f64 + 1.0)).unwrap_or(0.0)
}

pub(super) fn runtime_vector_score(record: &UnifiedRecord) -> f64 {
    record
        .get("_score")
        .or_else(|| record.get("final_score"))
        .or_else(|| record.get("score"))
        .or_else(|| record.get("vector_similarity"))
        .or_else(|| record.get("graph_score"))
        .or_else(|| record.get("table_score"))
        .and_then(runtime_value_number)
        .unwrap_or(0.0)
}

pub(super) fn merge_hybrid_records(
    structured: Option<&UnifiedRecord>,
    vector: Option<&UnifiedRecord>,
) -> UnifiedRecord {
    let mut merged = structured.cloned().unwrap_or_default();

    if let Some(vector_record) = vector {
        // Collect first to avoid borrowing `merged` while we're
        // iterating its sibling-record fields.
        let pairs: Vec<(std::sync::Arc<str>, Value)> = vector_record
            .iter_fields()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (key, value) in pairs {
            let key_str: &str = &key;
            if let Some(existing) = merged.get(key_str) {
                if existing != &value {
                    merged.set_arc(std::sync::Arc::from(format!("vector.{key_str}")), value);
                }
            } else {
                merged.set_arc(key, value);
            }
        }

        for (alias, node) in &vector_record.nodes {
            merged
                .nodes
                .entry(alias.clone())
                .or_insert_with(|| node.clone());
        }
        for (alias, edge) in &vector_record.edges {
            merged
                .edges
                .entry(alias.clone())
                .or_insert_with(|| edge.clone());
        }
        merged.paths.extend(vector_record.paths.clone());
        merged
            .vector_results
            .extend(vector_record.vector_results.clone());
    }

    merged
}

pub(super) fn merge_join_records(
    left: Option<&UnifiedRecord>,
    right: Option<&UnifiedRecord>,
    left_query: &TableQuery,
    right_prefix: Option<&str>,
) -> UnifiedRecord {
    let left_table_name = left_query.table.as_str();
    let left_table_alias = left_query.alias.as_deref().unwrap_or(left_table_name);
    let mut merged = UnifiedRecord::new();

    if let Some(left_record) = left {
        merged = project_runtime_record(
            left_record,
            &effective_table_projections(left_query),
            Some(left_table_name),
            Some(left_table_alias),
            false,
            false,
        );
    }

    if let Some(right_record) = right {
        let pairs: Vec<(std::sync::Arc<str>, Value)> = right_record
            .iter_fields()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (key, value) in pairs {
            let key_str: &str = &key;
            if merged.contains_column(key_str) {
                if let Some(prefix) = right_prefix {
                    merged.set_arc(std::sync::Arc::from(format!("{prefix}.{key_str}")), value);
                }
            } else {
                merged.set_arc(key, value);
            }
        }

        for (alias, node) in &right_record.nodes {
            merged.nodes.insert(alias.clone(), node.clone());
        }
        for (alias, edge) in &right_record.edges {
            merged.edges.insert(alias.clone(), edge.clone());
        }
        merged.paths.extend(right_record.paths.clone());
        merged
            .vector_results
            .extend(right_record.vector_results.clone());
    }

    merged
}

pub(super) fn join_condition_matches(
    left_record: &UnifiedRecord,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    left_field: &FieldRef,
    right_record: &UnifiedRecord,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    right_field: &FieldRef,
) -> bool {
    let left_value =
        resolve_runtime_field(left_record, left_field, left_table_name, left_table_alias);
    let right_value = resolve_runtime_field(
        right_record,
        right_field,
        right_table_name,
        right_table_alias,
    );

    match (left_value.as_ref(), right_value.as_ref()) {
        (Some(left), Some(right)) => compare_runtime_values(left, right, CompareOp::Eq),
        _ => false,
    }
}

pub(super) fn canonical_join_type(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
) -> RedDBResult<JoinType> {
    match node.details.get("join_type").map(String::as_str) {
        Some("inner") => Ok(JoinType::Inner),
        Some("left_outer") => Ok(JoinType::LeftOuter),
        Some("right_outer") => Ok(JoinType::RightOuter),
        Some("full_outer") => Ok(JoinType::FullOuter),
        Some("cross") => Ok(JoinType::Cross),
        Some(other) => Err(RedDBError::Query(format!(
            "unsupported canonical join type {other}"
        ))),
        None => Err(RedDBError::Query(
            "canonical join operator is missing join_type".to_string(),
        )),
    }
}

pub(super) fn canonical_join_field(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    key: &str,
) -> RedDBResult<FieldRef> {
    let value = node
        .details
        .get(key)
        .ok_or_else(|| RedDBError::Query(format!("canonical join operator is missing {key}")))?;
    parse_canonical_field_ref(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CanonicalJoinStrategy {
    IndexedNestedLoop,
    GraphLookupJoin,
    HashJoin,
    NestedLoop,
}

pub(super) fn canonical_join_strategy(
    node: &crate::storage::query::planner::CanonicalLogicalNode,
) -> RedDBResult<CanonicalJoinStrategy> {
    match node.details.get("join_strategy").map(String::as_str) {
        Some("indexed_nested_loop") => Ok(CanonicalJoinStrategy::IndexedNestedLoop),
        Some("graph_lookup_join") => Ok(CanonicalJoinStrategy::GraphLookupJoin),
        Some("hash_join") => Ok(CanonicalJoinStrategy::HashJoin),
        Some("nested_loop") => Ok(CanonicalJoinStrategy::NestedLoop),
        Some(other) => Err(RedDBError::Query(format!(
            "unsupported canonical join strategy {other}"
        ))),
        None => Err(RedDBError::Query(
            "canonical join operator is missing join_strategy".to_string(),
        )),
    }
}
