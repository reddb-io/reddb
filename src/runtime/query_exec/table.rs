//! Canonical table query executor.
//!
//! Houses the path for plain `FROM table` queries: the
//! [`RuntimeTableExecutionContext`] struct and the three dispatch
//! functions (`execute_runtime_canonical_table_query_indexed`,
//! `execute_runtime_canonical_table_node`,
//! `execute_runtime_canonical_table_child`) that walk the canonical
//! plan and produce `UnifiedRecord`s.
//!
//! Split out of `query_exec.rs` to isolate the ~400 lines of table
//! scan logic from the entry point and the expression router. Uses
//! `use super::*;` to inherit the parent executor's imports, plus
//! explicit imports from sibling helpers/indexed_scan submodules.

use super::helpers::{
    extract_bloom_key_for_pk, extract_entity_id_from_filter, extract_index_candidate,
    extract_select_column_names,
};
use super::indexed_scan::try_sorted_index_lookup;
use super::*;

/// Build the JSON result from a set of entity IDs (from index lookup).
/// Scan entities sequentially but only process those in the candidate set (from hash index).
/// Faster than individual store.get() because HashMap iteration is sequential/cache-friendly.
pub(crate) struct RuntimeTableExecutionContext<'a> {
    pub(crate) query: &'a TableQuery,
    pub(crate) table_name: &'a str,
    pub(crate) table_alias: &'a str,
}

pub(crate) fn execute_runtime_canonical_table_query_indexed(
    db: &RedDB,
    query: &TableQuery,
    index_store: Option<&super::index_store::IndexStore>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    // ── FROM SUBQUERY PATH (Fase 1.7 / W4 rebind): when the query's
    // source is a `(SELECT …) AS alias`, execute the inner query
    // recursively to get its records, then apply the outer query's
    // WHERE / ORDER BY / OFFSET / LIMIT on top of those records so
    // the user sees the canonical SQL semantics.
    //
    // Column scope: the outer sees the inner's projection aliases
    // verbatim because UnifiedRecord keys are string column names.
    // If the user writes `SELECT score FROM (SELECT a + b AS score
    // FROM t) AS s WHERE score > 10 ORDER BY score DESC LIMIT 5`,
    // the inner emits records keyed by `score` and the outer's
    // filter / sort resolve against that key directly.
    //
    // Only QueryExpr::Table nested shapes are supported here —
    // joins / unions / CTEs in FROM-subquery position error loudly.
    if let Some(crate::storage::query::ast::TableSource::Subquery(inner)) = &query.source {
        match inner.as_ref() {
            crate::storage::query::ast::QueryExpr::Table(inner_table) => {
                let mut records =
                    execute_runtime_canonical_table_query_indexed(db, inner_table, index_store)?;

                // Outer WHERE: re-evaluate the legacy filter walker
                // against each materialised record. The alias is the
                // outer query's alias (or the synthetic sentinel if
                // unaliased) so qualified column references resolve
                // back onto the inner projection keys.
                let outer_alias = query.alias.as_deref();
                if let Some(ref outer_filter) = query.filter {
                    records.retain(|record| {
                        super::super::join_filter::evaluate_runtime_filter(
                            record,
                            outer_filter,
                            outer_alias,
                            outer_alias,
                        )
                    });
                }

                // Outer ORDER BY: sort the materialised records
                // using the same comparator as the normal table
                // path. Expression-shaped sort keys run through
                // expr_eval, bare columns through resolve_field.
                if !query.order_by.is_empty() {
                    records.sort_by(|a, b| {
                        super::super::join_filter::compare_runtime_order(
                            a,
                            b,
                            &query.order_by,
                            outer_alias,
                            outer_alias,
                        )
                    });
                }

                // Outer OFFSET / LIMIT.
                if let Some(offset) = query.offset {
                    let offset = offset as usize;
                    if offset >= records.len() {
                        records.clear();
                    } else {
                        records.drain(..offset);
                    }
                }
                if let Some(limit) = query.limit {
                    records.truncate(limit as usize);
                }

                return Ok(records);
            }
            other => {
                return Err(RedDBError::Query(format!(
                    "FROM subquery of kind {} is not yet supported — \
                     only nested SELECT lands in Fase 2 Week 4",
                    super::super::join_filter::query_expr_name(other)
                )));
            }
        }
    }

    // ── ULTRA-FAST PATH: entity_id lookup bypasses planner entirely ──
    if let Some(entity_id) = extract_entity_id_from_filter(&query.filter) {
        let store = db.store();
        if let Some(entity) = store.get(&query.table, EntityId::new(entity_id)) {
            return Ok(runtime_table_record_from_entity(entity)
                .into_iter()
                .collect());
        }
        return Ok(Vec::new());
    }

    // ── INDEX-ASSISTED PATH: sorted (BTREE) index for BETWEEN / >/>= ──
    //
    // Piggy-backs on `try_sorted_index_lookup`, which already knows how
    // to walk a `SortedIndexManager` for range predicates. Previously
    // the main execution path only looked at hash (equality) indices,
    // so `WHERE age BETWEEN 30 AND 40` always fell through to a full
    // scan even when a BTREE index on `age` existed.
    if let (Some(idx_store), Some(ref filter)) = (index_store, &query.filter) {
        let trace = std::env::var("REDDB_INDEX_TRACE").ok().as_deref() == Some("1");
        let sorted_res = try_sorted_index_lookup(filter, &query.table, idx_store);
        if trace {
            eprintln!(
                "sorted_index_lookup table={} filter={:?} result={:?}",
                query.table,
                filter,
                sorted_res.as_ref().map(|v| v.len())
            );
        }
        if let Some(entity_ids) = sorted_res {
            let store = db.store();
            let mut records = Vec::with_capacity(entity_ids.len());
            for eid in entity_ids {
                if let Some(entity) = store.get(&query.table, eid) {
                    if let Some(record) = runtime_table_record_from_entity(entity) {
                        records.push(record);
                    }
                }
            }
            return Ok(records);
        }
    }

    // ── INDEX-ASSISTED PATH: use hash index for O(1) equality lookups ──
    if let (Some(idx_store), Some(ref filter)) = (index_store, &query.filter) {
        if let Some((column, value_bytes)) = extract_index_candidate(filter) {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, &column) {
                let entity_ids = idx_store
                    .hash_lookup(&query.table, &idx.name, &value_bytes)
                    .map_err(|err| {
                        RedDBError::Internal(format!("hash index lookup failed: {err}"))
                    })?;
                if !entity_ids.is_empty() {
                    let store = db.store();
                    let mut records = Vec::new();
                    for eid in entity_ids {
                        if let Some(entity) = store.get(&query.table, eid) {
                            if let Some(record) = runtime_table_record_from_entity(entity) {
                                records.push(record);
                            }
                        }
                    }
                    return Ok(records);
                }
            }
        }
    }

    // ── FAST PATH: Simple filtered scan — bypass planner for basic WHERE queries ──
    // Evaluates the filter directly on raw entity data to avoid materializing
    // UnifiedRecord for every entity in the collection.
    // Excludes universal entity sources (e.g. "any") which span all collections.
    if query.filter.is_some()
        && query.group_by.is_empty()
        && query.having.is_none()
        && query.expand.is_none()
        && !query
            .columns
            .iter()
            .any(|p| matches!(p, Projection::Function(_, _) | Projection::Expression(_, _)))
        && !is_universal_query_source(&query.table)
    {
        let manager = db
            .store()
            .get_collection(query.table.as_str())
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

        let filter = query.filter.as_ref().ok_or_else(|| {
            RedDBError::Internal("filtered runtime scan selected without a WHERE clause".into())
        })?;
        let table_name = query.table.as_str();
        let table_alias = query.alias.as_deref().unwrap_or(table_name);
        let limit = query.limit.unwrap_or(10000) as usize;

        // Bloom filter: extract PK key for segment pruning
        let bloom_key = extract_bloom_key_for_pk(filter);
        if let Some(ref key) = bloom_key {
            let (entities, _pruned) = manager.query_with_bloom_hint(Some(key), |_| true);
            if entities.is_empty() {
                return Ok(Vec::new());
            }
        }

        // Extract explicit column names for projection pushdown
        let select_cols = extract_select_column_names(&query.columns);

        // Compile the filter ONCE before iterating. The compiled
        // form pre-classifies every FieldRef into an EntityFieldKind
        // so the per-row evaluator skips the ~6 system-field string
        // compares + entity-kind cascade that the legacy walker
        // performs on every call. See
        // `runtime/query_exec/filter_compiled.rs` for the algorithm.
        let compiled =
            super::filter_compiled::CompiledEntityFilter::compile(filter, table_name, table_alias);

        // Pre-filter at entity level, only materialize records that pass
        let mut records: Vec<UnifiedRecord> = Vec::new();
        manager.for_each_entity(|entity| {
            if records.len() >= limit {
                return false; // stop iteration
            }
            if compiled.evaluate(entity) {
                let record = if select_cols.is_empty() {
                    runtime_table_record_from_entity(entity.clone())
                } else {
                    runtime_table_record_from_entity_projected(entity.clone(), &select_cols)
                };
                if let Some(record) = record {
                    records.push(record);
                }
            }
            true // continue
        });

        // Apply ORDER BY if present
        if !query.order_by.is_empty() {
            let order_by = &query.order_by;
            records.sort_by(|left, right| {
                compare_runtime_order(left, right, order_by, Some(table_name), Some(table_alias))
            });
        }

        // Apply OFFSET
        if let Some(offset) = query.offset {
            let offset = offset as usize;
            if offset < records.len() {
                records = records.into_iter().skip(offset).collect();
            } else {
                records.clear();
            }
        }

        return Ok(records);
    }

    // ── FAST PATH: Unfiltered scan — bypass planner for simple SELECT * ──
    // Skipped when the projection list contains scalar function calls
    // (e.g. VERIFY_PASSWORD(...), UPPER(...)), since the fast path
    // returns raw records without running project_runtime_record.
    let has_scalar_function = query
        .columns
        .iter()
        .any(|p| matches!(p, Projection::Function(_, _) | Projection::Expression(_, _)));
    if query.filter.is_none()
        && query.group_by.is_empty()
        && query.having.is_none()
        && query.expand.is_none()
        && !has_scalar_function
    {
        let mut records = scan_runtime_table_source_records(db, query.table.as_str())?;
        let table_name = query.table.as_str();
        let table_alias = query.alias.as_deref().unwrap_or(table_name);

        if !query.order_by.is_empty() {
            records.sort_by(|left, right| {
                compare_runtime_order(
                    left,
                    right,
                    &query.order_by,
                    Some(table_name),
                    Some(table_alias),
                )
            });
        }

        if let Some(offset) = query.offset {
            let offset = offset as usize;
            if offset < records.len() {
                records = records.into_iter().skip(offset).collect();
            } else {
                records.clear();
            }
        }

        if let Some(limit) = query.limit {
            records.truncate(limit as usize);
        }

        return Ok(records);
    }

    let plan = CanonicalPlanner::new(db).build(&QueryExpr::Table(query.clone()));
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let context = RuntimeTableExecutionContext {
        query,
        table_name,
        table_alias,
    };
    execute_runtime_canonical_table_node(db, &plan.root, &context)
}

pub(crate) fn execute_runtime_canonical_table_node(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    context: &RuntimeTableExecutionContext<'_>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    match node.operator.as_str() {
        "table_scan" | "index_seek" | "entity_scan" | "document_path_index_seek" => {
            // ── FAST PATH 1: Direct entity_id lookup (O(1) instead of full scan) ──
            if let Some(entity_id) = extract_entity_id_from_filter(&context.query.filter) {
                let store = db.store();
                if let Some(entity) = store.get(&context.query.table, EntityId::new(entity_id)) {
                    return Ok(runtime_table_record_from_entity(entity)
                        .into_iter()
                        .collect());
                }
                return Ok(Vec::new());
            }

            // ── FAST PATH 2: Filtered scan with entity-level pre-filter ──
            // Evaluates the WHERE clause directly on raw entity data, only
            // creating UnifiedRecord for entities that match the filter.
            // Skip for universal sources ("any") which need cross-collection scanning.
            if context.query.filter.is_some()
                && !context
                    .query
                    .columns
                    .iter()
                    .any(|p| matches!(p, Projection::Function(_, _) | Projection::Expression(_, _)))
                && !is_universal_query_source(context.query.table.as_str())
            {
                let manager = db
                    .store()
                    .get_collection(context.query.table.as_str())
                    .ok_or_else(|| RedDBError::NotFound(context.query.table.clone()))?;

                let filter = context.query.filter.as_ref().ok_or_else(|| {
                    RedDBError::Internal(
                        "canonical filtered scan selected without a WHERE clause".into(),
                    )
                })?;
                let table_name = context.table_name;
                let table_alias = context.table_alias;
                let limit = context.query.limit.unwrap_or(10000) as usize;

                let select_cols = extract_select_column_names(&context.query.columns);
                let compiled = super::filter_compiled::CompiledEntityFilter::compile(
                    filter,
                    table_name,
                    table_alias,
                );
                let mut records: Vec<UnifiedRecord> = Vec::new();
                manager.for_each_entity(|entity| {
                    if records.len() >= limit {
                        return false;
                    }
                    if compiled.evaluate(entity) {
                        let record = if select_cols.is_empty() {
                            runtime_table_record_from_entity(entity.clone())
                        } else {
                            runtime_table_record_from_entity_projected(entity.clone(), &select_cols)
                        };
                        if let Some(record) = record {
                            records.push(record);
                        }
                    }
                    true
                });
                return Ok(records);
            }

            // ── DEFAULT: Full scan ──
            scan_runtime_table_source_records(db, context.query.table.as_str())
        }
        "filter" | "entity_filter" => {
            // ── FAST PATH: Direct entity_id lookup (O(1)) ──
            if let Some(entity_id) = extract_entity_id_from_filter(&context.query.filter) {
                let store = db.store();
                if let Some(entity) = store.get(&context.query.table, EntityId::new(entity_id)) {
                    return Ok(runtime_table_record_from_entity(entity)
                        .into_iter()
                        .collect());
                }
                return Ok(Vec::new());
            }

            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = context.query.filter.as_ref() {
                records.retain(|record| {
                    evaluate_runtime_filter(
                        record,
                        filter,
                        Some(context.table_name),
                        Some(context.table_alias),
                    )
                });
            }
            Ok(records)
        }
        "document_path_filter" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = context.query.filter.as_ref() {
                records.retain(|record| {
                    runtime_record_has_document_capability(record)
                        && evaluate_runtime_document_filter(
                            record,
                            filter,
                            Some(context.table_name),
                            Some(context.table_alias),
                        )
                });
            }
            Ok(records)
        }
        "sort" | "entity_sort" | "document_sort" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if !context.query.order_by.is_empty() {
                records.sort_by(|left, right| {
                    compare_runtime_order(
                        left,
                        right,
                        &context.query.order_by,
                        Some(context.table_name),
                        Some(context.table_alias),
                    )
                });
            } else if node.operator == "entity_sort" {
                records.sort_by(compare_runtime_ranked_records);
            }
            Ok(records)
        }
        "offset" | "entity_offset" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let offset = context.query.offset.unwrap_or(0) as usize;
            Ok(records.into_iter().skip(offset).collect())
        }
        "limit" | "entity_limit" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let limit = context.query.limit.map(|value| value as usize);
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "entity_search" => execute_runtime_canonical_table_child(db, node, context),
        "entity_topk" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            records.sort_by(compare_runtime_ranked_records);
            let limit = node
                .details
                .get("k")
                .and_then(|value| value.parse::<usize>().ok())
                .or_else(|| context.query.limit.map(|value| value as usize));
            Ok(match limit {
                Some(limit) => records.into_iter().take(limit).collect(),
                None => records,
            })
        }
        "projection" | "document_projection" | "entity_projection" => {
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let document_projection = node.operator == "document_projection";
            let entity_projection = node.operator == "entity_projection";
            Ok(records
                .iter()
                .map(|record| {
                    project_runtime_record(
                        record,
                        &context.query.columns,
                        Some(context.table_name),
                        Some(context.table_alias),
                        document_projection,
                        entity_projection,
                    )
                })
                .collect())
        }
        other => Err(RedDBError::Query(format!(
            "unsupported canonical table operator {other}"
        ))),
    }
}

pub(crate) fn execute_runtime_canonical_table_child(
    db: &RedDB,
    node: &crate::storage::query::planner::CanonicalLogicalNode,
    context: &RuntimeTableExecutionContext<'_>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let child = node.children.first().ok_or_else(|| {
        RedDBError::Query(format!(
            "canonical table operator {} is missing its child plan",
            node.operator
        ))
    })?;
    execute_runtime_canonical_table_node(db, child, context)
}
