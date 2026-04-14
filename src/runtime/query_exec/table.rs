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
    extract_all_eq_candidates, extract_bloom_key_for_pk, extract_entity_id_from_filter,
    extract_index_candidate, extract_select_column_names, extract_zone_predicates,
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
                    super::super::join_filter::sort_records_by_order_by(
                        &mut records,
                        &query.order_by,
                        outer_alias,
                        outer_alias,
                    );
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

    // ── TID BITMAP PATH: AND multiple hash indexes for multi-predicate queries ──
    // `WHERE a = 1 AND b = 2` with separate indexes on a and b:
    // - Look up each index → TidBitmap
    // - AND the bitmaps (in-CPU, no heap I/O)
    // - Fetch only the rows that survived both predicates
    // Only fires when ≥2 indexed equality columns exist in the filter.
    if let (Some(idx_store), Some(ref filter)) = (index_store, &query.filter) {
        let mut eq_candidates: Vec<(String, Vec<u8>)> = Vec::new();
        extract_all_eq_candidates(filter, &mut eq_candidates);

        // Collect EntityId sets for each indexed column
        let mut indexed_id_sets: Vec<std::collections::HashSet<u64>> = Vec::new();
        for (col, val_bytes) in &eq_candidates {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, col) {
                if let Ok(ids) = idx_store.hash_lookup(&query.table, &idx.name, val_bytes) {
                    let id_set: std::collections::HashSet<u64> =
                        ids.iter().map(|e| e.raw()).collect();
                    indexed_id_sets.push(id_set);
                }
            }
        }

        // Only use bitmap AND when we have ≥2 indexed sets (otherwise single-index path below)
        if indexed_id_sets.len() >= 2 {
            // Intersect all sets — start from the smallest for best performance
            indexed_id_sets.sort_by_key(|s| s.len());
            let mut intersection = indexed_id_sets.remove(0);
            for other in &indexed_id_sets {
                intersection.retain(|id| other.contains(id));
                if intersection.is_empty() {
                    break;
                }
            }
            if intersection.is_empty() {
                return Ok(Vec::new());
            }
            // Fetch matching entities in sorted order (ascending EntityId for sequential access)
            let store = db.store();
            let mut sorted_ids: Vec<u64> = intersection.into_iter().collect();
            sorted_ids.sort_unstable();
            let mut records = Vec::new();
            for raw_id in sorted_ids {
                if let Some(entity) = store.get(&query.table, EntityId::new(raw_id)) {
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
                // ── INDEX-ONLY SCAN CHECK ──────────────────────────────────────
                // When the query projects only the indexed column (SELECT col FROM t WHERE col = ?)
                // and the collection has sealed (all-visible) data, skip the entity heap fetch.
                // The value we need is the equality predicate value itself — no entity scan needed.
                let projected_cols = extract_select_column_names(&query.columns);
                let index_only_eligible = !projected_cols.is_empty()
                    && projected_cols.iter().all(|c| c == &column)
                    && query.group_by.is_empty()
                    && query.having.is_none()
                    && query.order_by.is_empty();

                if index_only_eligible {
                    let manager_opt = db.store().get_collection(query.table.as_str());
                    let vis_fraction = manager_opt
                        .as_ref()
                        .map(|m| m.all_visible_fraction())
                        .unwrap_or(0.0);

                    use crate::storage::query::planner::index_only::{
                        decide, CoveringIndex, IndexOnlyDecision,
                    };
                    let covering = CoveringIndex {
                        name: idx.name.clone(),
                        covered_columns: vec![column.clone()],
                    };
                    let filter_cols = vec![column.clone()];
                    let decision = decide(&projected_cols, &filter_cols, &covering, vis_fraction);

                    if decision == IndexOnlyDecision::FullCover {
                        // Return the equality value directly — no entity fetch
                        use crate::storage::schema::Value as SchemaValue;
                        let value = match filter {
                            crate::storage::query::ast::Filter::Compare { value, .. } => {
                                value.clone()
                            }
                            _ => SchemaValue::Null,
                        };
                        // Each matching entity_id = one result row with {column: value}
                        let entity_ids = idx_store
                            .hash_lookup(&query.table, &idx.name, &value_bytes)
                            .map_err(|err| {
                                RedDBError::Internal(format!("hash index lookup failed: {err}"))
                            })?;
                        let records = entity_ids
                            .iter()
                            .map(|_eid| {
                                let mut rec = UnifiedRecord::with_capacity(1);
                                rec.set(&column, value.clone());
                                rec
                            })
                            .collect::<Vec<_>>();
                        return Ok(records);
                    }
                }

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

        // Zone map: extract range/equality predicates for segment-level pruning.
        // Sealed segments whose column min/max proves no row can match are skipped.
        let mut zone_raw: Vec<(
            String,
            crate::storage::schema::Value,
            crate::storage::unified::segment::ZoneColPredKind,
        )> = Vec::new();
        extract_zone_predicates(filter, &mut zone_raw);
        // Reconstruct lifetime-bound ZoneColPred refs from the owned Vec.
        let zone_preds: Vec<(&str, crate::storage::unified::segment::ZoneColPred<'_>)> = zone_raw
            .iter()
            .map(|(col, val, kind)| {
                use crate::storage::unified::segment::{ZoneColPred, ZoneColPredKind};
                let pred = match kind {
                    ZoneColPredKind::Eq => ZoneColPred::Eq(val),
                    ZoneColPredKind::Gt => ZoneColPred::Gt(val),
                    ZoneColPredKind::Gte => ZoneColPred::Gte(val),
                    ZoneColPredKind::Lt => ZoneColPred::Lt(val),
                    ZoneColPredKind::Lte => ZoneColPred::Lte(val),
                };
                (col.as_str(), pred)
            })
            .collect();

        // Extract explicit column names for projection pushdown
        let select_cols = extract_select_column_names(&query.columns);

        // Compile the filter ONCE before iterating. When the collection
        // schema is available, use compile_with_schema to pre-resolve
        // user column names to positional indices — O(1) access per field
        // per row instead of O(n schema search) for bulk-inserted entities.
        let schema_arc = manager.column_schema();
        let compiled = match schema_arc.as_ref() {
            Some(schema) => super::filter_compiled::CompiledEntityFilter::compile_with_schema(
                filter,
                table_name,
                table_alias,
                schema.as_ref(),
            ),
            None => super::filter_compiled::CompiledEntityFilter::compile(
                filter,
                table_name,
                table_alias,
            ),
        };

        // Pre-filter at entity level, only materialize records that pass.
        // Uses zone-map-aware iteration: sealed segments whose column zones
        // prove no row can match the predicate are skipped entirely.
        //
        // B3 optimisation: when select_cols is non-empty, use the ref-based
        // projected materialiser — avoids cloning the whole entity, only
        // clones the K selected field values instead of all N.
        //
        // Schema-index precomputation: for bulk-inserted (columnar) entities,
        // resolve projected column names → schema positions once before the
        // scan loop. Each row then does O(1) indexed access instead of
        // O(schema_len) linear search per (row, column) pair.
        let schema_col_indices: Option<Vec<(usize, usize)>> = if !select_cols.is_empty() {
            schema_arc.as_ref().map(|schema| {
                select_cols
                    .iter()
                    .enumerate()
                    .filter_map(|(ci, col)| schema.iter().position(|s| s == col).map(|si| (ci, si)))
                    .collect()
            })
        } else {
            None
        };

        let mut records: Vec<UnifiedRecord> = Vec::new();
        manager.for_each_entity_zoned(&zone_preds, |entity| {
            if records.len() >= limit {
                return false; // stop iteration
            }
            if compiled.evaluate(entity) {
                let record = if !select_cols.is_empty() {
                    // Fast columnar path: use pre-computed schema indices when available.
                    if let Some(ref idx_map) = schema_col_indices {
                        super::super::record_search::runtime_table_record_with_col_indices(
                            entity, &select_cols, idx_map,
                        )
                        .or_else(|| {
                            super::super::record_search::runtime_table_record_from_entity_ref_projected(
                                entity, &select_cols,
                            )
                        })
                        .or_else(|| {
                            runtime_table_record_from_entity_projected(entity.clone(), &select_cols)
                        })
                    } else {
                        super::super::record_search::runtime_table_record_from_entity_ref_projected(
                            entity,
                            &select_cols,
                        )
                        .or_else(|| {
                            runtime_table_record_from_entity_projected(entity.clone(), &select_cols)
                        })
                    }
                } else {
                    runtime_table_record_from_entity(entity.clone())
                };
                if let Some(record) = record {
                    records.push(record);
                }
            }
            true // continue
        });

        // Apply ORDER BY — Schwartzian transform extracts keys once (O(n))
        // instead of per-comparison (O(n log n) HashMap lookups).
        if !query.order_by.is_empty() {
            super::super::join_filter::sort_records_by_order_by(
                &mut records,
                &query.order_by,
                Some(table_name),
                Some(table_alias),
            );
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
            super::super::join_filter::sort_records_by_order_by(
                &mut records,
                &query.order_by,
                Some(table_name),
                Some(table_alias),
            );
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
                let schema_arc = manager.column_schema();
                let compiled = match schema_arc.as_ref() {
                    Some(schema) => {
                        super::filter_compiled::CompiledEntityFilter::compile_with_schema(
                            filter,
                            table_name,
                            table_alias,
                            schema.as_ref(),
                        )
                    }
                    None => super::filter_compiled::CompiledEntityFilter::compile(
                        filter,
                        table_name,
                        table_alias,
                    ),
                };
                // Schema-index precomputation: same optimisation as the indexed scan path.
                // Resolve projected column names → schema positions once before the loop.
                let schema_col_indices: Option<Vec<(usize, usize)>> = if !select_cols.is_empty() {
                    schema_arc.as_ref().map(|schema| {
                        select_cols
                            .iter()
                            .enumerate()
                            .filter_map(|(ci, col)| {
                                schema.iter().position(|s| s == col).map(|si| (ci, si))
                            })
                            .collect()
                    })
                } else {
                    None
                };

                let mut records: Vec<UnifiedRecord> = Vec::new();
                manager.for_each_entity(|entity| {
                    if records.len() >= limit {
                        return false;
                    }
                    if compiled.evaluate(entity) {
                        let record = if !select_cols.is_empty() {
                            if let Some(ref idx_map) = schema_col_indices {
                                super::super::record_search::runtime_table_record_with_col_indices(
                                    entity, &select_cols, idx_map,
                                )
                                .or_else(|| {
                                    super::super::record_search::runtime_table_record_from_entity_ref_projected(
                                        entity, &select_cols,
                                    )
                                })
                                .or_else(|| {
                                    runtime_table_record_from_entity_projected(
                                        entity.clone(),
                                        &select_cols,
                                    )
                                })
                            } else {
                                super::super::record_search::runtime_table_record_from_entity_ref_projected(
                                    entity,
                                    &select_cols,
                                )
                                .or_else(|| {
                                    runtime_table_record_from_entity_projected(
                                        entity.clone(),
                                        &select_cols,
                                    )
                                })
                            }
                        } else {
                            runtime_table_record_from_entity(entity.clone())
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
                super::super::join_filter::sort_records_by_order_by(
                    &mut records,
                    &context.query.order_by,
                    Some(context.table_name),
                    Some(context.table_alias),
                );
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
