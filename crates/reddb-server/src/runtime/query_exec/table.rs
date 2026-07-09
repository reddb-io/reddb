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
use super::indexed_scan::{
    extract_cross_index_predicates, find_range_predicate_with_sorted_index,
    try_sorted_index_filtered_by_set, try_sorted_index_lookup,
};
use super::*;
use crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver;
use crate::storage::query::sql_lowering::{
    effective_table_filter, effective_table_group_by_exprs, effective_table_having_filter,
    effective_table_projections,
};

/// Build the JSON result from a set of entity IDs (from index lookup).
/// Scan entities sequentially but only process those in the candidate set (from hash index).
/// Faster than individual store.get() because HashMap iteration is sequential/cache-friendly.
pub(crate) struct RuntimeTableExecutionContext<'a> {
    pub(crate) query: &'a TableQuery,
    pub(crate) table_name: &'a str,
    pub(crate) table_alias: &'a str,
}

fn resolve_table_row_by_logical_id(
    db: &RedDB,
    table: &str,
    logical_id: EntityId,
) -> Option<UnifiedEntity> {
    let store = db.store();
    TableRowMvccReadResolver::current_statement().resolve_logical_id(&store, table, logical_id)
}

fn projections_require_runtime_projection(projections: &[Projection]) -> bool {
    projections.iter().any(|projection| {
        matches!(
            projection,
            Projection::Function(_, _) | Projection::Expression(_, _) | Projection::Window { .. }
        ) || matches!(
            projection,
            Projection::Column(column) | Projection::Alias(column, _) if column.starts_with("LIT:")
        )
    })
}

#[derive(Clone, Copy)]
struct GeoDistancePredicate<'a> {
    column: &'a str,
    center_lat: f64,
    center_lon: f64,
    radius_km: f64,
}

struct GeoWithinPredicate<'a> {
    column: &'a str,
    vertices: Vec<(f64, f64)>,
}

fn execute_geo_h3_candidate_scan(
    db: &RedDB,
    query: &TableQuery,
    filter: &Filter,
    effective_projections: &[Projection],
    candidate_ids: &std::collections::HashSet<u64>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let manager = db
        .store()
        .get_collection(query.table.as_str())
        .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let compiled = crate::runtime::scalar_evaluator::compile_filter(
        filter,
        &crate::runtime::scalar_evaluator::PermissiveScope,
    );

    let table_row_resolver = TableRowMvccReadResolver::current_statement();
    let hydrate_store = db.store();
    let mut records = Vec::new();
    manager.for_each_entity(|entity| {
        if !candidate_ids.contains(&entity.id.raw()) {
            return true;
        }
        if table_row_resolver.resolve_read_candidate(entity).is_none() {
            return true;
        }
        if !db.replica_allows_entity_at_read(&query.table, entity) {
            return true;
        }
        let hydrated = super::super::impl_timeseries::hydrate_timeseries_entity(
            hydrate_store.as_ref(),
            entity,
        );
        let Some(record) = runtime_table_record_from_entity(hydrated) else {
            return true;
        };
        if crate::runtime::scalar_evaluator::evaluate_compiled_filter(
            Some(db),
            &compiled,
            &record,
            Some(table_name),
            Some(table_alias),
        ) {
            records.push(record);
        }
        true
    });

    crate::runtime::window_phase::apply(
        Some(db),
        &mut records,
        effective_projections,
        Some(table_name),
        Some(table_alias),
    )?;

    let mut records = records
        .iter()
        .map(|record| {
            project_runtime_record_with_db(
                Some(db),
                record,
                effective_projections,
                Some(table_name),
                Some(table_alias),
                false,
                false,
            )
        })
        .collect::<RedDBResult<Vec<_>>>()?;

    if !query.order_by.is_empty() {
        crate::runtime::materialization_limit::guard(db, "sort", records.len())?;
        super::super::join_filter::sort_records_by_order_by_with_db(
            Some(db),
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

    Ok(records)
}

fn geo_h3_candidate_ids(
    filter: &Filter,
    table: &str,
    idx_store: &super::index_store::IndexStore,
) -> Option<std::collections::HashSet<u64>> {
    match filter {
        Filter::CompareExpr { lhs, op, rhs } => {
            if let Some(predicate) = geo_distance_predicate(lhs, *op, rhs)
                .or_else(|| geo_distance_predicate(rhs, flipped_compare_op_for_geo(*op), lhs))
            {
                return geo_distance_predicate_candidate_ids(predicate, table, idx_store);
            }
            let predicate = geo_within_predicate(lhs, *op, rhs)
                .or_else(|| geo_within_predicate(rhs, flipped_compare_op_for_geo(*op), lhs))?;
            geo_within_predicate_candidate_ids(predicate, table, idx_store)
        }
        Filter::And(left, right) => {
            let left_ids = geo_h3_candidate_ids(left, table, idx_store);
            let right_ids = geo_h3_candidate_ids(right, table, idx_store);
            match (left_ids, right_ids) {
                (Some(mut left), Some(right)) => {
                    left.retain(|id| right.contains(id));
                    Some(left)
                }
                (Some(ids), None) | (None, Some(ids)) => Some(ids),
                (None, None) => None,
            }
        }
        Filter::Or(left, right) => {
            let mut left_ids = geo_h3_candidate_ids(left, table, idx_store)?;
            let right_ids = geo_h3_candidate_ids(right, table, idx_store)?;
            left_ids.extend(right_ids);
            Some(left_ids)
        }
        Filter::Not(_) => None,
        _ => None,
    }
}

fn geo_distance_predicate<'a>(
    lhs: &'a Expr,
    op: CompareOp,
    rhs: &'a Expr,
) -> Option<GeoDistancePredicate<'a>> {
    if !matches!(op, CompareOp::Lt | CompareOp::Le) {
        return None;
    }
    let radius_km = literal_f64(rhs)?;
    if radius_km.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return None;
    }
    let Expr::FunctionCall { name, args, .. } = lhs else {
        return None;
    };
    if !(name.eq_ignore_ascii_case("GEO_DISTANCE") || name.eq_ignore_ascii_case("HAVERSINE")) {
        return None;
    }
    let [Expr::Column { field, .. }, lat, lon] = args.as_slice() else {
        return None;
    };
    let column = match field {
        FieldRef::TableColumn { column, .. } => column.as_str(),
        _ => return None,
    };
    Some(GeoDistancePredicate {
        column,
        center_lat: literal_f64(lat)?,
        center_lon: literal_f64(lon)?,
        radius_km,
    })
}

fn geo_within_predicate<'a>(
    lhs: &'a Expr,
    op: CompareOp,
    rhs: &Expr,
) -> Option<GeoWithinPredicate<'a>> {
    if !matches!(op, CompareOp::Eq) || !literal_bool(rhs)? {
        return None;
    }
    let Expr::FunctionCall { name, args, .. } = lhs else {
        return None;
    };
    if !name.eq_ignore_ascii_case("GEO_WITHIN") {
        return None;
    }
    let [Expr::Column { field, .. }, polygon] = args.as_slice() else {
        return None;
    };
    let column = match field {
        FieldRef::TableColumn { column, .. } => column.as_str(),
        _ => return None,
    };
    Some(GeoWithinPredicate {
        column,
        vertices: polygon_vertices_from_expr(polygon)?,
    })
}

fn literal_f64(expr: &Expr) -> Option<f64> {
    match expr {
        Expr::Literal {
            value: Value::Float(value),
            ..
        } => Some(*value),
        Expr::Literal {
            value: Value::Integer(value),
            ..
        } => Some(*value as f64),
        Expr::Literal {
            value: Value::UnsignedInteger(value),
            ..
        } => Some(*value as f64),
        _ => None,
    }
}

fn literal_bool(expr: &Expr) -> Option<bool> {
    match expr {
        Expr::Literal {
            value: Value::Boolean(value),
            ..
        } => Some(*value),
        _ => None,
    }
}

fn polygon_vertices_from_expr(expr: &Expr) -> Option<Vec<(f64, f64)>> {
    let Expr::Literal {
        value: Value::Array(vertices),
        ..
    } = expr
    else {
        return None;
    };
    vertices
        .iter()
        .map(|vertex| {
            let Value::Array(pair) = vertex else {
                return None;
            };
            let [lat, lon] = pair.as_slice() else {
                return None;
            };
            Some((value_f64(lat)?, value_f64(lon)?))
        })
        .collect()
}

fn value_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Float(value) => Some(*value),
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        _ => None,
    }
}

fn flipped_compare_op_for_geo(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Eq => CompareOp::Eq,
        CompareOp::Ne => CompareOp::Ne,
        CompareOp::Lt => CompareOp::Gt,
        CompareOp::Le => CompareOp::Ge,
        CompareOp::Gt => CompareOp::Lt,
        CompareOp::Ge => CompareOp::Le,
    }
}

fn geo_distance_predicate_candidate_ids(
    predicate: GeoDistancePredicate<'_>,
    table: &str,
    idx_store: &super::index_store::IndexStore,
) -> Option<std::collections::HashSet<u64>> {
    let index = idx_store.find_index_for_column(table, predicate.column)?;
    let super::index_store::IndexMethodKind::H3 { resolution } = index.method else {
        return None;
    };
    let cells = h3_cover_cells_for_geo_predicate(
        predicate.center_lat,
        predicate.center_lon,
        predicate.radius_km,
        resolution,
    );
    if cells.is_empty() {
        return None;
    }
    let keys: Vec<_> = cells
        .iter()
        .filter_map(|cell| {
            crate::storage::schema::value_to_canonical_key(&Value::UnsignedInteger(*cell))
        })
        .collect();
    if keys.is_empty() {
        return None;
    }
    let ids = idx_store
        .sorted
        .in_lookup_limited(table, predicate.column, &keys, usize::MAX)?;
    Some(ids.into_iter().map(|id| id.raw()).collect())
}

fn geo_within_predicate_candidate_ids(
    predicate: GeoWithinPredicate<'_>,
    table: &str,
    idx_store: &super::index_store::IndexStore,
) -> Option<std::collections::HashSet<u64>> {
    let index = idx_store.find_index_for_column(table, predicate.column)?;
    let super::index_store::IndexMethodKind::H3 { resolution } = index.method else {
        return None;
    };
    const MAX_POLYGON_COVER_CELLS: usize = 50_000;
    let cells = crate::geo::h3::polygon_to_cover_cells(
        &predicate.vertices,
        resolution,
        MAX_POLYGON_COVER_CELLS,
    )?;
    if cells.is_empty() {
        return None;
    }
    let keys: Vec<_> = cells
        .iter()
        .filter_map(|cell| {
            crate::storage::schema::value_to_canonical_key(&Value::UnsignedInteger(*cell))
        })
        .collect();
    if keys.is_empty() {
        return None;
    }
    let ids = idx_store
        .sorted
        .in_lookup_limited(table, predicate.column, &keys, usize::MAX)?;
    Some(ids.into_iter().map(|id| id.raw()).collect())
}

fn h3_cover_cells_for_geo_predicate(
    lat: f64,
    lon: f64,
    radius_km: f64,
    resolution: u8,
) -> Vec<u64> {
    let cell = crate::geo::h3::lat_lng_to_cell(lat, lon, resolution);
    if cell == 0 {
        return Vec::new();
    }
    let edge_km = crate::geo::h3::edge_length_km(resolution).max(f64::MIN_POSITIVE);
    const MAX_COVER_RING: u32 = 128;
    let k_f = (radius_km / edge_km).ceil() + 1.0;
    if !k_f.is_finite() || k_f > f64::from(MAX_COVER_RING) {
        return Vec::new();
    }
    crate::geo::h3::grid_disk(cell, k_f as u32)
}

pub(crate) fn execute_runtime_canonical_table_query_indexed(
    db: &RedDB,
    query: &TableQuery,
    index_store: Option<&super::index_store::IndexStore>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let effective_projections = effective_table_projections(query);
    let effective_filter = effective_table_filter(query);
    let effective_group_by = effective_table_group_by_exprs(query);
    let effective_having = effective_table_having_filter(query);
    let requires_runtime_projection =
        projections_require_runtime_projection(&effective_projections);
    let uses_document_projection =
        runtime_projections_use_document_path(&effective_projections, query);

    // Hypertable chunk pruning (PRD #850, Phase 0): when this table is a
    // hypertable and the WHERE clause constrains the time column, consult
    // the per-chunk time bounds. If no chunk overlaps the predicate
    // window, the scan is skipped entirely — by the pruner's soundness
    // contract there is provably no matching row, so returning an empty
    // result is exact, not a heuristic. Non-temporal predicates and
    // non-hypertable collections fall through untouched: the pruner keeps
    // every chunk conservatively, so `kept` is non-empty and the scan
    // proceeds as before.
    if let Some(spec) = db.hypertables().get(&query.table) {
        let chunks = db.hypertables().show_chunks(&query.table);
        if !chunks.is_empty() {
            let kept = crate::storage::query::planner::hypertable_pruning::prune_hypertable_chunks(
                &spec,
                &chunks,
                effective_filter.as_ref(),
            );
            if kept.is_empty() {
                return Ok(Vec::new());
            }
        }
    }

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
                if let Some(ref outer_filter) = effective_filter {
                    records.retain(|record| {
                        super::super::join_filter::evaluate_runtime_filter_with_db(
                            Some(db),
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
                    // Issue #769 — the ORDER BY buffer is materialized in
                    // full before sorting; cap it.
                    crate::runtime::materialization_limit::guard(db, "sort", records.len())?;
                    super::super::join_filter::sort_or_top_k_records_with_db(
                        Some(db),
                        &mut records,
                        &query.order_by,
                        query.offset,
                        query.limit,
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

                crate::runtime::window_phase::apply(
                    Some(db),
                    &mut records,
                    &effective_projections,
                    None,
                    outer_alias,
                )?;

                return records
                    .iter()
                    .map(|record| {
                        project_runtime_record_with_db(
                            Some(db),
                            record,
                            &effective_projections,
                            None,
                            outer_alias,
                            false,
                            false,
                        )
                    })
                    .collect();
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
    if !requires_runtime_projection {
        if let Some(entity_id) = extract_entity_id_from_filter(&effective_filter) {
            let entity =
                resolve_table_row_by_logical_id(db, &query.table, EntityId::new(entity_id));
            if let Some(entity) = entity {
                return Ok(
                    super::super::record_search::runtime_table_record_lean_in_collection(
                        entity,
                        &query.table,
                    )
                    .into_iter()
                    .collect(),
                );
            }
            return Ok(Vec::new());
        }
    }

    let requires_mvcc_index_fallback =
        crate::runtime::impl_core::current_snapshot_requires_index_fallback();

    // ── GEO H3 CANDIDATE PATH ────────────────────────────────────────────────
    // Geo predicates over H3-indexed columns can reuse the same candidate
    // covers as SEARCH SPATIAL. The full WHERE expression is still evaluated
    // after candidate pruning, so the index remains a pure optimization.
    if let (false, Some(idx_store), Some(ref filter)) =
        (requires_mvcc_index_fallback, index_store, &effective_filter)
    {
        if !is_universal_query_source(&query.table) {
            if let Some(candidate_ids) = geo_h3_candidate_ids(filter, &query.table, idx_store) {
                return execute_geo_h3_candidate_scan(
                    db,
                    query,
                    filter,
                    &effective_projections,
                    &candidate_ids,
                );
            }
        }
    }

    // ── INDEX-ASSISTED PATH: sorted (BTREE) index for BETWEEN / >/>= ──
    //
    // Piggy-backs on `try_sorted_index_lookup`, which already knows how
    // to walk a `SortedIndexManager` for range predicates. Previously
    // the main execution path only looked at hash (equality) indices,
    // so `WHERE age BETWEEN 30 AND 40` always fell through to a full
    // scan even when a BTREE index on `age` existed.
    //
    // BUT: for `WHERE city='X' AND age > Y`, a bare sorted lookup on
    // `age` returns almost the entire table (e.g. `age > 18` = 99% of
    // rows). The real win is intersecting with the hash index on city
    // (5% of rows). Skip the sorted-only path when the filter has a
    // hash-indexed equality AND a sorted range predicate — the
    // cross-index bitmap block below does the right thing.
    let has_cross_index_candidate = index_store
        .zip(effective_filter.as_ref())
        .and_then(|(idx_store, filter)| {
            extract_cross_index_predicates(filter, &query.table, idx_store)
        })
        .is_some();
    if let (false, false, Some(idx_store), Some(ref filter), false, false) = (
        requires_mvcc_index_fallback,
        requires_runtime_projection,
        index_store,
        &effective_filter,
        has_cross_index_candidate,
        uses_document_projection,
    ) {
        let trace = std::env::var("REDDB_INDEX_TRACE").ok().as_deref() == Some("1");
        let sorted_res = try_sorted_index_lookup(
            filter,
            &query.table,
            idx_store,
            query.limit.map(|l| l as usize),
        );
        if trace {
            eprintln!(
                "sorted_index_lookup table={} filter={:?} result={:?}",
                query.table,
                filter,
                sorted_res.as_ref().map(|v| v.len())
            );
        }
        if let Some(entity_ids) = sorted_res {
            // Even covered projections must fetch the candidate row so MVCC
            // can reject stale or tombstoned index IDs before materialization.
            let explicit_cols = extract_select_column_names(&effective_projections);

            // Re-apply the full filter — when the filter is a compound AND, the
            // sorted lookup used only the range predicate to narrow candidates.
            // Residual predicates (equality, other ranges) must be checked here.
            //
            // But: when the filter is a *leaf* predicate (single Between /
            // Compare / In on a sorted-indexed column) the sorted lookup
            // already returned exactly the matching ids — every candidate
            // passes the residual check by construction. Skipping the
            // re-compile + re-evaluate shaves ~20% off range/filtered
            // scans on `SELECT * WHERE age BETWEEN X AND Y`.
            let table_name = query.table.as_str();
            let table_alias = query.alias.as_deref().unwrap_or(table_name);
            let filter_fully_covered = matches!(
                filter,
                crate::storage::query::ast::Filter::Between { .. }
                    | crate::storage::query::ast::Filter::Compare { .. }
                    | crate::storage::query::ast::Filter::In { .. }
            );
            let compiled_filter = if filter_fully_covered {
                None
            } else {
                let schema_arc = db
                    .store()
                    .get_collection(table_name)
                    .and_then(|m| m.column_schema());
                Some(match schema_arc.as_ref() {
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
                })
            };
            let store = db.store();
            // Use lean materialization (skip red_* system fields) when SELECT *
            // was requested.  Explicit projection columns still go through the full
            // path below so user-specified system fields (e.g. SELECT rid,
            // age FROM t) are not silently dropped.
            let lean = explicit_cols.is_empty(); // SELECT * → lean path
            let limit = query.limit.map(|l| l as usize).unwrap_or(usize::MAX);

            // Lean/SELECT-* path uses the borrow-based
            // `SegmentManager::for_each_id` → `runtime_table_record_lean_ref`
            // combination to skip the `UnifiedEntity::clone` inside
            // `get_batch` (~20% of scan CPU on `select_range`/`filtered`).
            // Projection paths keep the old owned-entity flow because
            // `runtime_table_record_from_entity_projected` consumes the
            // entity.
            if lean {
                let manager = match store.get_collection(&query.table) {
                    Some(m) => m,
                    None => return Ok(Vec::new()),
                };
                let mut records: Vec<UnifiedRecord> =
                    Vec::with_capacity(entity_ids.len().min(limit));
                let mut stop = false;
                let table_row_resolver = TableRowMvccReadResolver::current_statement();
                manager.for_each_id(&entity_ids, |_idx, entity| {
                    if stop {
                        return;
                    }
                    if records.len() >= limit {
                        stop = true;
                        return;
                    }
                    if table_row_resolver.resolve_read_candidate(entity).is_none() {
                        return;
                    }
                    if !db.replica_allows_entity_at_read(&query.table, entity) {
                        return;
                    }
                    if let Some(cf) = compiled_filter.as_ref() {
                        if !cf.evaluate(entity) {
                            return;
                        }
                    }
                    if let Some(record) =
                        super::super::record_search::runtime_table_record_lean_ref(entity)
                    {
                        records.push(record);
                    }
                });
                return Ok(records);
            }

            // Projection path (explicit columns): keep owned-entity flow.
            let entities = store.get_batch(&query.table, &entity_ids);
            let mut records = Vec::with_capacity(entity_ids.len().min(limit));
            let table_row_resolver = TableRowMvccReadResolver::current_statement();
            for entity_opt in entities.into_iter().flatten() {
                if records.len() >= limit {
                    break;
                }
                if table_row_resolver
                    .resolve_read_candidate(&entity_opt)
                    .is_none()
                {
                    continue;
                }
                if !db.replica_allows_entity_at_read(&query.table, &entity_opt) {
                    continue;
                }
                if let Some(cf) = compiled_filter.as_ref() {
                    if !cf.evaluate(&entity_opt) {
                        continue;
                    }
                }
                let record_opt =
                    runtime_table_record_from_entity_projected(entity_opt, &explicit_cols);
                if let Some(record) = record_opt {
                    records.push(record);
                }
            }
            return Ok(records);
        }
    }

    // ── CROSS-INDEX BITMAP AND: hash eq ∩ sorted range ──────────────────────────
    // Handles `WHERE city = 'X' AND age > 30` when city has a hash index and
    // age has a sorted index.
    //
    // Current single-index path: fetch ALL ~50K hash candidates → filter by age.
    // This path: iterate sorted range for age, check each ID against a HashSet
    // built from the hash candidates. Only fetch the ~1K intersection.
    //
    // Equivalent to PG's bitmap heap scan where two bitmap indexes are AND-ed
    // at word level before touching heap pages. Here we use HashSet instead of
    // actual bitmaps but the reduction in entity fetches is the same.
    if let (false, false, Some(idx_store), Some(ref filter), false) = (
        requires_mvcc_index_fallback,
        requires_runtime_projection,
        index_store,
        &effective_filter,
        uses_document_projection,
    ) {
        if let Some((eq_col, eq_bytes, range_filter)) =
            extract_cross_index_predicates(filter, &query.table, idx_store)
        {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, &eq_col) {
                if let Ok(hash_ids) =
                    idx_store.hash_lookup(&query.table, idx.hash_lookup_name().as_ref(), &eq_bytes)
                {
                    if !hash_ids.is_empty() {
                        let limit = query.limit.map(|l| l as usize).unwrap_or(usize::MAX);
                        // Build HashSet from the smaller (hash) candidate set
                        let hash_set: std::collections::HashSet<u64> =
                            hash_ids.iter().map(|id| id.raw()).collect();
                        // Stream sorted range, collect only IDs in hash_set
                        if let Some(intersection_ids) = try_sorted_index_filtered_by_set(
                            range_filter,
                            &query.table,
                            idx_store,
                            &hash_set,
                            limit,
                        ) {
                            let table_name = query.table.as_str();
                            let table_alias = query.alias.as_deref().unwrap_or(table_name);
                            let schema_arc = db
                                .store()
                                .get_collection(table_name)
                                .and_then(|m| m.column_schema());
                            let compiled_filter =
                                effective_filter.as_ref().map(|f| match schema_arc.as_ref() {
                                    Some(schema) => {
                                        super::filter_compiled::CompiledEntityFilter::compile_with_schema(
                                            f,
                                            table_name,
                                            table_alias,
                                            schema.as_ref(),
                                        )
                                    }
                                    None => super::filter_compiled::CompiledEntityFilter::compile(
                                        f,
                                        table_name,
                                        table_alias,
                                    ),
                                });
                            let explicit_cols = extract_select_column_names(&effective_projections);

                            let store = db.store();
                            let entities = store.get_batch(&query.table, &intersection_ids);
                            let lean = explicit_cols.is_empty();
                            let mut records = Vec::with_capacity(intersection_ids.len().min(limit));
                            let table_row_resolver = TableRowMvccReadResolver::current_statement();
                            for entity_opt in entities.into_iter().flatten() {
                                if records.len() >= limit {
                                    break;
                                }
                                if table_row_resolver
                                    .resolve_read_candidate(&entity_opt)
                                    .is_none()
                                {
                                    continue;
                                }
                                if !db.replica_allows_entity_at_read(&query.table, &entity_opt) {
                                    continue;
                                }
                                if compiled_filter
                                    .as_ref()
                                    .is_none_or(|cf| cf.evaluate(&entity_opt))
                                {
                                    let record_opt = if lean {
                                        super::super::record_search::runtime_table_record_lean_in_collection(
                                            entity_opt,
                                            &query.table,
                                        )
                                    } else {
                                        runtime_table_record_from_entity_projected(
                                            entity_opt,
                                            &explicit_cols,
                                        )
                                    };
                                    if let Some(record) = record_opt {
                                        records.push(record);
                                    }
                                }
                            }
                            return Ok(records);
                        }
                    }
                }
            }
        }
    }

    // ── TID BITMAP PATH: AND multiple hash indexes for multi-predicate queries ──
    // `WHERE a = 1 AND b = 2 [AND range_col op val]` with hash indexes on a, b
    // and optional sorted index on range_col:
    // - Look up each equality index → TidBitmap per column
    // - AND the bitmaps via word-level RoaringBitmap intersection (smallest-first)
    // - Optionally narrow further via sorted range scan filtered by the intersection set
    // - Fetch only the surviving rows; re-apply full compiled filter for residual predicates
    // Only fires when ≥2 indexed equality columns exist in the filter.
    if let (false, false, Some(idx_store), Some(ref filter), false) = (
        requires_mvcc_index_fallback,
        requires_runtime_projection,
        index_store,
        &effective_filter,
        uses_document_projection,
    ) {
        let mut eq_candidates: Vec<(String, Vec<u8>, crate::storage::schema::Value)> = Vec::new();
        extract_all_eq_candidates(filter, &mut eq_candidates);

        // Collect one TidBitmap per indexed equality column.
        // TidBitmap uses RoaringBitmap internally — intersection is word-level AND
        // (~32x faster than HashSet retain for 10K+ IDs, and far more cache-friendly).
        // Entity IDs are cast to u32; safe for any reasonable in-memory DB size (< 4 B).
        let mut bitmaps: Vec<crate::storage::index::tid_bitmap::TidBitmap> = Vec::new();
        for (col, val_bytes, _val) in &eq_candidates {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, col) {
                if let Ok(ids) =
                    idx_store.hash_lookup(&query.table, idx.hash_lookup_name().as_ref(), val_bytes)
                {
                    let mut bmp = crate::storage::index::tid_bitmap::TidBitmap::with_cap_bytes(0);
                    let _ = bmp.extend_from_iter(ids.iter().map(|e| e.raw() as u32));
                    bitmaps.push(bmp);
                }
            }
        }

        // Only use bitmap AND when we have ≥2 bitmaps (otherwise single-index path below)
        if bitmaps.len() >= 2 {
            // Intersect all bitmaps — start from the smallest for best performance
            bitmaps.sort_by_key(|b| b.len());
            let mut intersection = bitmaps.remove(0);
            for other in &bitmaps {
                intersection.intersect_with(other); // word-level AND — O(n/32)
            }
            if intersection.is_empty() {
                return Ok(Vec::new());
            }
            // Convert to HashSet<u64> for compatibility with try_sorted_index_filtered_by_set.
            // The intersection is already small (post-AND), so this conversion is cheap.
            let intersection: std::collections::HashSet<u64> =
                intersection.iter().map(|id| id as u64).collect();

            let table_name = query.table.as_str();
            let table_alias = query.alias.as_deref().unwrap_or(table_name);
            let schema_arc = db
                .store()
                .get_collection(table_name)
                .and_then(|m| m.column_schema());
            // Always re-apply the full filter — residual predicates (range, OR, etc.)
            // not captured by the eq-bitmap path must still be checked.
            let compiled_filter = match schema_arc.as_ref() {
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

            let limit = query.limit.map(|l| l as usize).unwrap_or(usize::MAX);
            let explicit_cols = extract_select_column_names(&effective_projections);
            let lean = explicit_cols.is_empty();

            // Optional: narrow via sorted range index filtered by the eq-intersection set.
            // Handles `WHERE city='NYC' AND status='active' AND age > 30` fully in-index.
            // Uses find_range_predicate_with_sorted_index (not extract_cross_index_predicates)
            // because the eq predicates are already handled by the bitmap intersection above.
            let entity_ids: Vec<EntityId> = if let Some(range_filter) =
                find_range_predicate_with_sorted_index(filter, table_name, idx_store)
            {
                // Sorted-range scan filtered by the eq-bitmap intersection set
                if let Some(range_ids) = try_sorted_index_filtered_by_set(
                    range_filter,
                    table_name,
                    idx_store,
                    &intersection,
                    limit,
                ) {
                    range_ids
                } else {
                    // Sorted range not applicable; use eq-intersection directly
                    let mut sorted: Vec<u64> = intersection.into_iter().collect();
                    sorted.sort_unstable();
                    sorted.into_iter().map(EntityId::new).collect()
                }
            } else {
                // No range predicate; just sort eq-intersection for sequential access
                let mut sorted: Vec<u64> = intersection.into_iter().collect();
                sorted.sort_unstable();
                sorted.into_iter().map(EntityId::new).collect()
            };

            let store = db.store();
            let entities = store.get_batch(&query.table, &entity_ids);
            let mut records = Vec::with_capacity(entity_ids.len().min(limit));
            let table_row_resolver = TableRowMvccReadResolver::current_statement();
            for entity_opt in entities.into_iter().flatten() {
                if records.len() >= limit {
                    break;
                }
                if table_row_resolver
                    .resolve_read_candidate(&entity_opt)
                    .is_none()
                {
                    continue;
                }
                if !db.replica_allows_entity_at_read(&query.table, &entity_opt) {
                    continue;
                }
                if compiled_filter.evaluate(&entity_opt) {
                    let record_opt = if lean {
                        super::super::record_search::runtime_table_record_lean_in_collection(
                            entity_opt,
                            &query.table,
                        )
                    } else {
                        runtime_table_record_from_entity_projected(entity_opt, &explicit_cols)
                    };
                    if let Some(record) = record_opt {
                        records.push(record);
                    }
                }
            }
            return Ok(records);
        }
    }

    // ── INDEX-ASSISTED PATH: use hash index for O(1) equality lookups ──
    if let (false, false, Some(idx_store), Some(ref filter), false) = (
        requires_mvcc_index_fallback,
        requires_runtime_projection,
        index_store,
        &effective_filter,
        uses_document_projection,
    ) {
        if let Some((column, value_bytes)) = extract_index_candidate(filter) {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, &column) {
                let mut entity_ids = idx_store
                    .hash_lookup(&query.table, idx.hash_lookup_name().as_ref(), &value_bytes)
                    .map_err(|err| {
                        RedDBError::Internal(format!("hash index lookup failed: {err}"))
                    })?;
                if !entity_ids.is_empty() {
                    let limit = query.limit.map(|l| l as usize).unwrap_or(usize::MAX);

                    // When the hash result is much larger than LIMIT and the filter has
                    // secondary predicates (e.g. AND(city='X', age>30)), fetching and
                    // cloning all N candidates is wasteful — a sequential scan with
                    // early termination visits ~limit/combined_selectivity entities and
                    // stops, vs N entity clones.  Fall through to the fast-scan path below.
                    //
                    // Threshold: N > limit * 20. For LIMIT=100, N must be >2000 before
                    // we skip; for LIMIT=usize::MAX (no LIMIT), we never skip.
                    //
                    // Simple equality filter (no secondary predicates): truncate candidate
                    // list to limit instead — any limit rows from the hash set are valid
                    // without fetching the rest.
                    let is_simple_eq = matches!(
                        effective_filter.as_ref(),
                        Some(Filter::Compare {
                            op: CompareOp::Eq,
                            ..
                        })
                    );
                    if is_simple_eq && limit < entity_ids.len() {
                        // No secondary predicate — any N rows are valid.
                        entity_ids.truncate(limit);
                    } else if !is_simple_eq
                        && limit < usize::MAX
                        && entity_ids.len() > limit.saturating_mul(20)
                    {
                        // Compound filter + large candidate set: let fast scan handle it.
                        // Drop entity_ids and fall through to for_each_entity_zoned below.
                    } else {
                        // Compile and re-apply the FULL filter.
                        // The hash lookup extracted only one equality predicate from the AND tree
                        // (e.g. city = 'X'); secondary predicates (e.g. age > 30) would be silently
                        // dropped without this step, producing wrong row counts.
                        //
                        // Use compile_with_schema when the collection schema is available so that
                        // user column accesses use O(1) index lookup (RowFieldFast) instead of
                        // O(n) linear search (RowField) — matters for 50K+ candidate sets.
                        let table_name = query.table.as_str();
                        let table_alias = query.alias.as_deref().unwrap_or(table_name);
                        let schema_arc = db
                            .store()
                            .get_collection(table_name)
                            .and_then(|m| m.column_schema());
                        let compiled_filter = effective_filter.as_ref().map(|f| {
                            match schema_arc
                        .as_ref()
                    {
                        Some(schema) => {
                            super::filter_compiled::CompiledEntityFilter::compile_with_schema(
                                f,
                                table_name,
                                table_alias,
                                schema.as_ref(),
                            )
                        }
                        None => super::filter_compiled::CompiledEntityFilter::compile(
                            f,
                            table_name,
                            table_alias,
                        ),
                    }
                        });
                        let store = db.store();
                        // Batch fetch: single lock acquisition for the entire candidate set
                        let entities = store.get_batch(&query.table, &entity_ids);
                        let explicit_cols = extract_select_column_names(&effective_projections);
                        let lean = explicit_cols.is_empty();
                        let mut records = Vec::with_capacity(entity_ids.len().min(limit));
                        let table_row_resolver = TableRowMvccReadResolver::current_statement();
                        for entity_opt in entities.into_iter().flatten() {
                            if records.len() >= limit {
                                break;
                            }
                            if table_row_resolver
                                .resolve_read_candidate(&entity_opt)
                                .is_none()
                            {
                                continue;
                            }
                            if !db.replica_allows_entity_at_read(&query.table, &entity_opt) {
                                continue;
                            }
                            if compiled_filter
                                .as_ref()
                                .is_none_or(|cf| cf.evaluate(&entity_opt))
                            {
                                let record_opt = if lean {
                                    super::super::record_search::runtime_table_record_lean_in_collection(
                                        entity_opt,
                                        &query.table,
                                    )
                                } else {
                                    runtime_table_record_from_entity_projected(
                                        entity_opt,
                                        &explicit_cols,
                                    )
                                };
                                if let Some(record) = record_opt {
                                    records.push(record);
                                }
                            }
                        }
                        return Ok(records);
                    } // end else (hash batch path)
                }
            }
        }
    }

    // ── GLOBAL ATTRIBUTE PATH: `SELECT * WHERE passport = '...'` ──
    //
    // SELECT without FROM parses as the universal source (`table = "any"`).
    // Before falling back to the canonical universal scan, narrow the
    // collection set using declared collection contracts / column schemas when
    // the WHERE clause references plain attributes. Collections with unknown
    // shape stay in the candidate set so dynamic graph/document collections
    // can still match.
    if effective_filter.is_some()
        && is_universal_query_source(&query.table)
        && effective_group_by.is_empty()
        && effective_having.is_none()
        && query.expand.is_none()
        && !effective_projections.iter().any(|p| {
            matches!(
                p,
                Projection::Function(_, _) | Projection::Expression(_, _) | Projection::Window { .. }
            ) || matches!(p, Projection::Column(column) | Projection::Alias(column, _) if column.starts_with("LIT:"))
        })
    {
        let filter = effective_filter.as_ref().ok_or_else(|| {
            RedDBError::Internal(
                "global attribute scan selected without a WHERE clause".into(),
            )
        })?;
        if let Some(candidate_collections) =
            universal_candidate_collections_for_filter(db, query, filter)
        {
            let table_name = query.table.as_str();
            let table_alias = query.alias.as_deref().unwrap_or(table_name);
            let mut records = scan_runtime_universal_source_records_limited(
                db,
                Some(candidate_collections.as_slice()),
                None,
            )?;
            let compiled = crate::runtime::scalar_evaluator::compile_filter(
                filter,
                &crate::runtime::scalar_evaluator::PermissiveScope,
            );
            records.retain(|record| {
                crate::runtime::scalar_evaluator::evaluate_compiled_filter(
                    Some(db),
                    &compiled,
                    record,
                    Some(table_name),
                    Some(table_alias),
                )
            });

            if !query.order_by.is_empty() {
                crate::runtime::materialization_limit::guard(db, "sort", records.len())?;
                super::super::join_filter::sort_records_by_order_by_with_db(
                    Some(db),
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

            if !matches!(effective_projections.as_slice(), [Projection::All]) {
                records = records
                    .iter()
                    .map(|record| {
                        project_runtime_record_with_db(
                            Some(db),
                            record,
                            &effective_projections,
                            Some(table_name),
                            Some(table_alias),
                            false,
                            false,
                        )
                    })
                    .collect::<crate::RedDBResult<Vec<_>>>()?;
            }

            return Ok(records);
        }
    }

    // ── FAST PATH: Simple filtered scan — bypass planner for basic WHERE queries ──
    // Evaluates the filter directly on raw entity data to avoid materializing
    // UnifiedRecord for every entity in the collection.
    // Excludes universal entity sources (e.g. "any") which span all collections.
    if effective_filter.is_some()
        && !effective_filter
            .as_ref()
            .is_some_and(|filter| runtime_filter_uses_document_path(filter, query))
        && effective_group_by.is_empty()
        && effective_having.is_none()
        && query.expand.is_none()
        && !requires_runtime_projection
        && !uses_document_projection
        && !is_universal_query_source(&query.table)
    {
        let manager = db
            .store()
            .get_collection(query.table.as_str())
            .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

        let filter = effective_filter.as_ref().ok_or_else(|| {
            RedDBError::Internal("filtered runtime scan selected without a WHERE clause".into())
        })?;
        let table_name = query.table.as_str();
        let table_alias = query.alias.as_deref().unwrap_or(table_name);
        let explicit_limit = query.limit;
        // Issue #550 — the sequential scan terminates once `records.len()
        // >= limit` so the cap must cover the rows OFFSET will skip later;
        // otherwise `WHERE … LIMIT N OFFSET M` returns `N - M` rows instead
        // of `N`. ORDER BY is applied after the scan, so when a sort key is
        // present the cap is invalidated (capping at `off + lim` would
        // surface whichever rows the storage layer happens to yield first,
        // not the sort-ordered slice). Matches the unfiltered fast path's
        // `scan_cap` shape further down in this file.
        let limit = match (query.offset, explicit_limit) {
            _ if !query.order_by.is_empty() => 10000,
            (Some(off), Some(lim)) => (off as usize).saturating_add(lim as usize),
            (None, Some(lim)) => lim as usize,
            _ => 10000,
        };

        // Bloom filter: extract PK key for segment pruning
        let bloom_key = extract_bloom_key_for_pk(filter);
        if let Some(ref key) = bloom_key {
            if !manager.bloom_may_contain_key(key) {
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
        let select_cols = extract_select_column_names(&effective_projections);

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
        let requires_filter_recheck = compiled.has_fallback();

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

        // A5 — parallel scan: when there's no explicit LIMIT and the collection
        // is large enough, use query_all_zoned which parallelises filter eval
        // across sealed segments using std::thread::scope. Sequential path kept
        // for LIMIT queries so the early-exit optimisation still works.
        let entity_count = manager.count();
        let use_parallel = explicit_limit.is_none()
            && entity_count >= crate::storage::query::executors::parallel_scan::MIN_PARALLEL_ROWS;

        // SELECT * with lean materialization: skip the 6 heavy red_* system fields
        // (collection, kind, type, capabilities, sequence_id, row_id) while keeping
        // the two timestamp fields that external adapters commonly parse.
        let lean_select_star = select_cols.is_empty();

        let mut records: Vec<UnifiedRecord> = Vec::new();
        let hydrate_store = db.store();
        if use_parallel {
            // Parallel scan spawns worker threads that don't inherit the
            // main thread's CURRENT_SNAPSHOT thread-local. Capture the
            // context here so each closure invocation (on any thread) runs
            // the same MVCC visibility gate.
            let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
            let table_row_resolver = TableRowMvccReadResolver::captured(snap_ctx);
            let matching = manager.query_all_zoned(&zone_preds, |entity| {
                let hydrated = super::super::impl_timeseries::hydrate_timeseries_entity(
                    hydrate_store.as_ref(),
                    entity,
                );
                table_row_resolver.resolve_read_candidate(entity).is_some()
                    && db.replica_allows_entity_at_read(&query.table, entity)
                    && compiled.evaluate(&hydrated)
            });
            for entity in &matching {
                let hydrated = super::super::impl_timeseries::hydrate_timeseries_entity(
                    hydrate_store.as_ref(),
                    entity,
                );
                let record = if !select_cols.is_empty() {
                    if let Some(ref idx_map) = schema_col_indices {
                        super::super::record_search::runtime_table_record_with_col_indices(
                            &hydrated,
                            &select_cols,
                            idx_map,
                        )
                        .or_else(|| {
                            super::super::record_search::runtime_table_record_from_entity_ref_projected(
                                &hydrated, &select_cols,
                            )
                        })
                        .or_else(|| {
                            runtime_table_record_from_entity_projected(hydrated.clone(), &select_cols)
                        })
                    } else {
                        super::super::record_search::runtime_table_record_from_entity_ref_projected(
                            &hydrated,
                            &select_cols,
                        )
                        .or_else(|| {
                            runtime_table_record_from_entity_projected(
                                hydrated.clone(),
                                &select_cols,
                            )
                        })
                    }
                } else if lean_select_star {
                    super::super::record_search::runtime_table_record_lean_in_collection(
                        hydrated.clone(),
                        &query.table,
                    )
                } else {
                    runtime_table_record_from_entity(hydrated.clone())
                };
                if let Some(record) = record {
                    if requires_filter_recheck {
                        let Some(filter_record) =
                            runtime_table_record_from_entity(hydrated.clone())
                        else {
                            continue;
                        };
                        if super::super::join_filter::evaluate_runtime_filter_with_db(
                            Some(db),
                            &filter_record,
                            filter,
                            Some(table_name),
                            Some(table_alias),
                        ) {
                            records.push(record);
                        }
                    } else {
                        records.push(record);
                    }
                }
            }
        } else {
            let table_row_resolver = TableRowMvccReadResolver::current_statement();
            manager.for_each_entity_zoned(&zone_preds, |entity| {
                if records.len() >= limit {
                    return false; // stop iteration
                }
                if table_row_resolver.resolve_read_candidate(entity).is_none() {
                    return true; // skip hidden tuple, keep scanning
                }
                if !db.replica_allows_entity_at_read(&query.table, entity) {
                    return true;
                }
                let hydrated = super::super::impl_timeseries::hydrate_timeseries_entity(
                    hydrate_store.as_ref(),
                    entity,
                );
                if compiled.evaluate(&hydrated) {
                    let record = if !select_cols.is_empty() {
                        // Fast columnar path: use pre-computed schema indices when available.
                        if let Some(ref idx_map) = schema_col_indices {
                            super::super::record_search::runtime_table_record_with_col_indices(
                                &hydrated,
                                &select_cols,
                                idx_map,
                            )
                            .or_else(|| {
                                super::super::record_search::runtime_table_record_from_entity_ref_projected(
                                    &hydrated, &select_cols,
                                )
                            })
                            .or_else(|| {
                                runtime_table_record_from_entity_projected(hydrated.clone(), &select_cols)
                            })
                        } else {
                            super::super::record_search::runtime_table_record_from_entity_ref_projected(
                                &hydrated,
                                &select_cols,
                            )
                            .or_else(|| {
                                runtime_table_record_from_entity_projected(hydrated.clone(), &select_cols)
                            })
                        }
                    } else if lean_select_star {
                        super::super::record_search::runtime_table_record_lean_in_collection(
                            hydrated.clone(),
                            &query.table,
                        )
                        } else {
                            runtime_table_record_from_entity(hydrated.clone())
                        };
                    if let Some(record) = record {
                        if requires_filter_recheck {
                            let Some(filter_record) =
                                runtime_table_record_from_entity(hydrated.clone())
                            else {
                                return true;
                            };
                            if super::super::join_filter::evaluate_runtime_filter_with_db(
                                Some(db),
                                &filter_record,
                                filter,
                                Some(table_name),
                                Some(table_alias),
                            ) {
                                records.push(record);
                            }
                        } else {
                            records.push(record);
                        }
                    }
                }
                true // continue
            });
        }

        // Apply ORDER BY — Schwartzian transform extracts keys once (O(n))
        // instead of per-comparison (O(n log n) HashMap lookups).
        if !query.order_by.is_empty() {
            // Issue #769 — cap the materialized sort buffer.
            crate::runtime::materialization_limit::guard(db, "sort", records.len())?;
            super::super::join_filter::sort_records_by_order_by_with_db(
                Some(db),
                &mut records,
                &query.order_by,
                Some(table_name),
                Some(table_alias),
            );
        }

        // Apply OFFSET, then LIMIT. Order matters: SQL semantics drop the
        // first `OFFSET` filtered rows, then keep the next `LIMIT`. The
        // scan above terminates at `offset + limit` records (when there is
        // no ORDER BY) so we still get the right slice; with ORDER BY the
        // scan is uncapped above and the sort runs before OFFSET/LIMIT.
        if let Some(offset) = query.offset {
            let offset = offset as usize;
            if offset < records.len() {
                records = records.into_iter().skip(offset).collect();
            } else {
                records.clear();
            }
        }
        if let Some(limit) = explicit_limit {
            records.truncate(limit as usize);
        }

        return Ok(records);
    }

    // ── FAST PATH: Unfiltered scan — bypass planner for simple SELECT * ──
    // Skipped when the projection list contains scalar function calls
    // (e.g. VERIFY_PASSWORD(...), UPPER(...)), since the fast path
    // returns raw records without running project_runtime_record.
    if effective_filter.is_none()
        && effective_group_by.is_empty()
        && effective_having.is_none()
        && query.expand.is_none()
        && !requires_runtime_projection
        && !uses_document_projection
    {
        // LIMIT + OFFSET pushdown: pre-scan cap is `offset + limit` so
        // the scan loop stops once we have enough to skip + keep. ORDER
        // BY changes the row order downstream, so when a sort key is
        // present the cap is invalidated — capping at `off + lim` would
        // yield whichever rows the storage layer happens to surface
        // first, not the alphabetically-smallest ones. The unbounded-
        // scan-no-LIMIT case keeps reading the whole table as before.
        let scan_cap = match (query.offset, query.limit) {
            _ if !query.order_by.is_empty() => None,
            (Some(off), Some(lim)) => Some(off as usize + lim as usize),
            (None, Some(lim)) => Some(lim as usize),
            _ => None,
        };
        let mut records =
            scan_runtime_table_source_records_limited(db, query.table.as_str(), scan_cap)?;
        let table_name = query.table.as_str();
        let table_alias = query.alias.as_deref().unwrap_or(table_name);

        if !query.order_by.is_empty() {
            // Issue #769 — cap the materialized sort buffer.
            crate::runtime::materialization_limit::guard(db, "sort", records.len())?;
            super::super::join_filter::sort_or_top_k_records_with_db(
                Some(db),
                &mut records,
                &query.order_by,
                query.offset,
                query.limit,
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
    let effective_filter = effective_table_filter(context.query);
    let effective_projections = effective_table_projections(context.query);
    let uses_document_projection =
        runtime_projections_use_document_path(&effective_projections, context.query);
    match node.operator.as_str() {
        "table_scan" | "index_seek" | "entity_scan" | "document_path_index_seek" => {
            // ── FAST PATH 1: Direct entity_id lookup (O(1) instead of full scan) ──
            if !projections_require_runtime_projection(&effective_projections)
                && !uses_document_projection
            {
                if let Some(entity_id) = extract_entity_id_from_filter(&effective_filter) {
                    let entity = resolve_table_row_by_logical_id(
                        db,
                        &context.query.table,
                        EntityId::new(entity_id),
                    );
                    if let Some(entity) = entity {
                        if !db.replica_allows_entity_at_read(&context.query.table, &entity) {
                            return Ok(Vec::new());
                        }
                        return Ok(
                            super::super::record_search::runtime_table_record_lean_in_collection(
                                entity,
                                &context.query.table,
                            )
                            .into_iter()
                            .collect(),
                        );
                    }
                    return Ok(Vec::new());
                }
            }

            // ── FAST PATH 2: Filtered scan with entity-level pre-filter ──
            // Evaluates the WHERE clause directly on raw entity data, only
            // creating UnifiedRecord for entities that match the filter.
            // Skip for universal sources ("any") which need cross-collection scanning.
            if effective_filter.is_some()
                && !effective_filter.as_ref().is_some_and(|filter| {
                    runtime_filter_uses_document_path(filter, context.query)
                })
                && !effective_projections.iter().any(|p| {
                        matches!(p, Projection::Function(_, _) | Projection::Expression(_, _))
                            || matches!(p, Projection::Column(column) | Projection::Alias(column, _) if column.starts_with("LIT:"))
                    })
                && !uses_document_projection
                && !is_universal_query_source(context.query.table.as_str())
            {
                let manager = db
                    .store()
                    .get_collection(context.query.table.as_str())
                    .ok_or_else(|| RedDBError::NotFound(context.query.table.clone()))?;

                let filter = effective_filter.as_ref().ok_or_else(|| {
                    RedDBError::Internal(
                        "canonical filtered scan selected without a WHERE clause".into(),
                    )
                })?;
                let table_name = context.table_name;
                let table_alias = context.table_alias;
                let limit = context.query.limit.unwrap_or(10000) as usize;

                let select_cols = extract_select_column_names(&effective_projections);
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
                let requires_filter_recheck = compiled.has_fallback();
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
                let table_row_resolver = TableRowMvccReadResolver::current_statement();
                manager.for_each_entity(|entity| {
                    if records.len() >= limit {
                        return false;
                    }
                    if table_row_resolver.resolve_read_candidate(entity).is_none() {
                        return true;
                    }
                    if !db.replica_allows_entity_at_read(&context.query.table, entity) {
                        return true;
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
                            if requires_filter_recheck {
                                let Some(filter_record) =
                                    runtime_table_record_from_entity(entity.clone())
                                else {
                                    return true;
                                };
                                if evaluate_runtime_filter_with_db(
                                    Some(db),
                                    &filter_record,
                                    filter,
                                    Some(table_name),
                                    Some(table_alias),
                                ) {
                                    records.push(record);
                                }
                            } else {
                                records.push(record);
                            }
                        }
                    }
                    true
                });
                return Ok(records);
            }

            // ── DEFAULT: Full scan with LIMIT pushdown ──
            let scan_cap = match (context.query.offset, context.query.limit) {
                _ if !context.query.order_by.is_empty() => None,
                (Some(off), Some(lim)) => Some(off as usize + lim as usize),
                (None, Some(lim)) => Some(lim as usize),
                _ => None,
            };
            scan_runtime_table_source_records_limited(db, context.query.table.as_str(), scan_cap)
        }
        "filter" | "entity_filter" => {
            // ── FAST PATH: Direct entity_id lookup (O(1)) ──
            if !projections_require_runtime_projection(&effective_projections)
                && !uses_document_projection
            {
                if let Some(entity_id) = extract_entity_id_from_filter(&effective_filter) {
                    let entity = resolve_table_row_by_logical_id(
                        db,
                        &context.query.table,
                        EntityId::new(entity_id),
                    );
                    if let Some(entity) = entity {
                        return Ok(
                            super::super::record_search::runtime_table_record_lean_in_collection(
                                entity,
                                &context.query.table,
                            )
                            .into_iter()
                            .collect(),
                        );
                    }
                    return Ok(Vec::new());
                }
            }

            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = effective_filter.as_ref() {
                // Compile the filter through the ScalarEvaluator
                // interface ONCE before the per-row loop. Every
                // `Filter::CompareExpr` arm has its operator / cast /
                // function entries resolved here; the per-row
                // dispatch below only walks the resolved IR. Other
                // Filter variants stay on the legacy walker via the
                // `Legacy` arm of `CompiledFilter`.
                let compiled = crate::runtime::scalar_evaluator::compile_filter(
                    filter,
                    &crate::runtime::scalar_evaluator::PermissiveScope,
                );
                records.retain(|record| {
                    crate::runtime::scalar_evaluator::evaluate_compiled_filter(
                        Some(db),
                        &compiled,
                        record,
                        Some(context.table_name),
                        Some(context.table_alias),
                    )
                });
            }
            Ok(records)
        }
        "document_path_filter" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = effective_filter.as_ref() {
                // The capability gate exists so non-document rows in a
                // document collection don't survive a `body.field`
                // predicate (#551). But a dotted *tenant* path like
                // `meta.tenant` on a plain table targets a real JSON
                // column the row actually carries — those rows have no
                // "document" capability, so the bare gate would drop
                // every one of them and the tenant policy would match
                // nothing (#638). Let a row through when it owns the
                // root column the document path traverses; the resolver
                // already parses JSON-in-TEXT, and an unresolvable path
                // is excluded by the predicate itself.
                records.retain(|record| {
                    (runtime_record_has_document_capability(record)
                        || runtime_record_carries_filter_document_roots(
                            record,
                            filter,
                            context.query,
                        ))
                        && evaluate_runtime_filter_with_db(
                            Some(db),
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
                // Issue #769 — cap the materialized sort buffer.
                crate::runtime::materialization_limit::guard(db, "sort", records.len())?;
                super::super::join_filter::sort_records_by_order_by_with_db(
                    Some(db),
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
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            let document_projection = node.operator == "document_projection";
            let entity_projection = node.operator == "entity_projection";
            let effective_projections = effective_table_projections(context.query);
            // Issue #590 slice 7b — apply the window phase (ROW_NUMBER,
            // RANK, DENSE_RANK, LAG, LEAD) between filter/sort and the
            // final per-row projection. The window phase materialises a
            // virtual column on each record under the projection's
            // alias; the `Projection::Window` arm in
            // `project_runtime_record_with_db` then reads that column
            // back like any other field.
            crate::runtime::window_phase::apply(
                Some(db),
                &mut records,
                &effective_projections,
                Some(context.table_name),
                Some(context.table_alias),
            )?;
            records
                .iter()
                .map(|record| {
                    project_runtime_record_with_db(
                        Some(db),
                        record,
                        &effective_projections,
                        Some(context.table_name),
                        Some(context.table_alias),
                        document_projection,
                        entity_projection,
                    )
                })
                .collect()
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

/// Does `record` carry the root column of every document path the
/// filter traverses? Used to relax the `document_path_filter`
/// capability gate for dotted tenant/JSON-column predicates on plain
/// tables (#638): `meta.tenant` resolves against the row's real `meta`
/// column, so the row should be evaluated even without a "document"
/// capability. Returns `false` when the filter references no resolvable
/// dotted root (e.g. a foreign-table qualifier with no dotted path), so
/// genuine non-document rows in a document collection stay gated.
fn runtime_record_carries_filter_document_roots(
    record: &UnifiedRecord,
    filter: &Filter,
    query: &TableQuery,
) -> bool {
    let mut roots: Vec<Option<String>> = Vec::new();
    collect_filter_document_path_roots(filter, query, &mut roots);
    !roots.is_empty()
        && roots.iter().all(|root| {
            root.as_ref()
                .is_some_and(|root| record.get(root.as_str()).is_some())
        })
}

/// Collect, for each document-path field the filter references, the
/// root column its path traverses (`meta` for `meta.tenant`). A
/// document-path field with no dotted root (a bare foreign-table
/// qualifier) contributes `None` so the caller treats it as
/// unresolvable. Mirrors the shape of `runtime_filter_uses_document_path`.
fn collect_filter_document_path_roots(
    filter: &Filter,
    query: &TableQuery,
    out: &mut Vec<Option<String>>,
) {
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => {
            push_field_document_path_root(field, query, out);
        }
        Filter::CompareFields { left, right, .. } => {
            push_field_document_path_root(left, query, out);
            push_field_document_path_root(right, query, out);
        }
        Filter::CompareExpr { lhs, rhs, .. } => {
            collect_expr_document_path_roots(lhs, query, out);
            collect_expr_document_path_roots(rhs, query, out);
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            collect_filter_document_path_roots(left, query, out);
            collect_filter_document_path_roots(right, query, out);
        }
        Filter::Not(inner) => collect_filter_document_path_roots(inner, query, out),
    }
}

fn collect_expr_document_path_roots(
    expr: &crate::storage::query::ast::Expr,
    query: &TableQuery,
    out: &mut Vec<Option<String>>,
) {
    use crate::storage::query::ast::Expr;
    match expr {
        Expr::Literal { .. } | Expr::Parameter { .. } => {}
        Expr::Column { field, .. } => push_field_document_path_root(field, query, out),
        Expr::BinaryOp { lhs, rhs, .. } => {
            collect_expr_document_path_roots(lhs, query, out);
            collect_expr_document_path_roots(rhs, query, out);
        }
        Expr::UnaryOp { operand, .. } => collect_expr_document_path_roots(operand, query, out),
        Expr::Cast { inner, .. } => collect_expr_document_path_roots(inner, query, out),
        Expr::FunctionCall { args, .. } => {
            for arg in args {
                collect_expr_document_path_roots(arg, query, out);
            }
        }
        Expr::Case {
            branches, else_, ..
        } => {
            for (cond, val) in branches {
                collect_expr_document_path_roots(cond, query, out);
                collect_expr_document_path_roots(val, query, out);
            }
            if let Some(e) = else_ {
                collect_expr_document_path_roots(e, query, out);
            }
        }
        Expr::IsNull { operand, .. } => collect_expr_document_path_roots(operand, query, out),
        Expr::InList { target, values, .. } => {
            collect_expr_document_path_roots(target, query, out);
            for v in values {
                collect_expr_document_path_roots(v, query, out);
            }
        }
        Expr::Between {
            target, low, high, ..
        } => {
            collect_expr_document_path_roots(target, query, out);
            collect_expr_document_path_roots(low, query, out);
            collect_expr_document_path_roots(high, query, out);
        }
        // Opaque shapes can't be proven to traverse a known root —
        // contribute an unresolvable marker so the gate stays strict.
        Expr::Subquery { .. } | Expr::WindowFunctionCall { .. } => out.push(None),
    }
}

fn runtime_projections_use_document_path(projections: &[Projection], query: &TableQuery) -> bool {
    projections
        .iter()
        .any(|projection| runtime_projection_uses_document_path(projection, query))
}

fn runtime_projection_uses_document_path(projection: &Projection, query: &TableQuery) -> bool {
    match projection {
        Projection::Column(name) | Projection::Alias(name, _) => {
            name.split_once('.').is_some_and(|(head, tail)| {
                tail.contains('.')
                    || (head != query.table.as_str() && query.alias.as_deref() != Some(head))
            })
        }
        Projection::Field(field, _) => runtime_field_ref_uses_document_path(field, query),
        Projection::Expression(filter, _) => runtime_filter_uses_document_path(filter, query),
        Projection::Function(_, _) | Projection::All | Projection::Window { .. } => false,
    }
}

/// Push the resolvable document-path root for `field`, if it is one.
/// A dotted column (`meta.tenant`) yields `Some("meta")`. A bare
/// foreign-table qualifier (document-path by virtue of the qualifier,
/// not a dotted name) yields `None` — unresolvable for presence checks.
/// Flat same-table columns are not document paths and contribute nothing.
fn push_field_document_path_root(
    field: &FieldRef,
    query: &TableQuery,
    out: &mut Vec<Option<String>>,
) {
    if !runtime_field_ref_uses_document_path(field, query) {
        return;
    }
    match field {
        FieldRef::TableColumn { column, .. } => match column.split_once('.') {
            Some((root, _)) => out.push(Some(root.to_string())),
            None => out.push(None),
        },
        _ => out.push(None),
    }
}

fn runtime_filter_uses_document_path(filter: &Filter, query: &TableQuery) -> bool {
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => runtime_field_ref_uses_document_path(field, query),
        Filter::CompareFields { left, right, .. } => {
            runtime_field_ref_uses_document_path(left, query)
                || runtime_field_ref_uses_document_path(right, query)
        }
        // Mirror the planner's `filter_uses_document_path`: only route
        // a CompareExpr to the document-path path when an operand
        // actually traverses a document path. Flat predicates such as
        // the tenant-iso policy `col = CURRENT_TENANT()` stay on the
        // regular filter path.
        Filter::CompareExpr { lhs, rhs, .. } => {
            runtime_expr_uses_document_path(lhs, query)
                || runtime_expr_uses_document_path(rhs, query)
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            runtime_filter_uses_document_path(left, query)
                || runtime_filter_uses_document_path(right, query)
        }
        Filter::Not(inner) => runtime_filter_uses_document_path(inner, query),
    }
}

/// Runtime twin of the planner's `expr_uses_document_path` — must stay
/// in sync so FAST PATH 2 gating agrees with the operator the planner
/// chose. See `planner::logical_helpers::expr_uses_document_path`.
fn runtime_expr_uses_document_path(
    expr: &crate::storage::query::ast::Expr,
    query: &TableQuery,
) -> bool {
    use crate::storage::query::ast::Expr;
    match expr {
        Expr::Literal { .. } | Expr::Parameter { .. } => false,
        Expr::Column { field, .. } => runtime_field_ref_uses_document_path(field, query),
        Expr::BinaryOp { lhs, rhs, .. } => {
            runtime_expr_uses_document_path(lhs, query)
                || runtime_expr_uses_document_path(rhs, query)
        }
        Expr::UnaryOp { operand, .. } => runtime_expr_uses_document_path(operand, query),
        Expr::Cast { inner, .. } => runtime_expr_uses_document_path(inner, query),
        Expr::FunctionCall { args, .. } => args
            .iter()
            .any(|a| runtime_expr_uses_document_path(a, query)),
        Expr::Case {
            branches, else_, ..
        } => {
            branches.iter().any(|(cond, val)| {
                runtime_expr_uses_document_path(cond, query)
                    || runtime_expr_uses_document_path(val, query)
            }) || else_
                .as_ref()
                .is_some_and(|e| runtime_expr_uses_document_path(e, query))
        }
        Expr::IsNull { operand, .. } => runtime_expr_uses_document_path(operand, query),
        Expr::InList { target, values, .. } => {
            runtime_expr_uses_document_path(target, query)
                || values
                    .iter()
                    .any(|v| runtime_expr_uses_document_path(v, query))
        }
        Expr::Between {
            target, low, high, ..
        } => {
            runtime_expr_uses_document_path(target, query)
                || runtime_expr_uses_document_path(low, query)
                || runtime_expr_uses_document_path(high, query)
        }
        Expr::Subquery { .. } | Expr::WindowFunctionCall { .. } => true,
    }
}

fn runtime_field_ref_uses_document_path(field: &FieldRef, query: &TableQuery) -> bool {
    match field {
        FieldRef::TableColumn { table, column } => {
            column.contains('.')
                || (!table.is_empty()
                    && table != &query.table
                    && query.alias.as_deref() != Some(table.as_str()))
        }
        _ => false,
    }
}

fn universal_candidate_collections_for_filter(
    db: &RedDB,
    query: &TableQuery,
    filter: &Filter,
) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    if !collect_universal_filter_field_roots(filter, query, &mut fields) || fields.is_empty() {
        return None;
    }
    fields.sort();
    fields.dedup();

    Some(
        db.store()
            .list_collections()
            .into_iter()
            .filter(|collection| universal_collection_may_have_fields(db, collection, &fields))
            .collect(),
    )
}

fn collect_universal_filter_field_roots(
    filter: &Filter,
    query: &TableQuery,
    out: &mut Vec<String>,
) -> bool {
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => push_universal_filter_field_root(field, query, out),
        Filter::CompareFields { left, right, .. } => {
            push_universal_filter_field_root(left, query, out)
                && push_universal_filter_field_root(right, query, out)
        }
        Filter::And(left, right) => {
            collect_universal_filter_field_roots(left, query, out)
                && collect_universal_filter_field_roots(right, query, out)
        }
        // OR/NOT and expression filters can be soundly executed by the
        // existing universal plan. Keep them there until we have a candidate
        // algebra that can prove union/complement collection sets.
        Filter::Or(_, _) | Filter::Not(_) | Filter::CompareExpr { .. } => false,
    }
}

fn push_universal_filter_field_root(
    field: &FieldRef,
    query: &TableQuery,
    out: &mut Vec<String>,
) -> bool {
    let FieldRef::TableColumn { table, column } = field else {
        return false;
    };
    if !table.is_empty()
        && !is_universal_query_source(table)
        && query.alias.as_deref() != Some(table.as_str())
    {
        return false;
    }
    let root = column
        .split_once('.')
        .map_or(column.as_str(), |(root, _)| root);
    if is_universal_runtime_field(root) {
        return false;
    }
    out.push(root.to_string());
    true
}

fn is_universal_runtime_field(field: &str) -> bool {
    matches!(
        field,
        "rid"
            | "row_id"
            | "entity_id"
            | "collection"
            | "red_collection"
            | "kind"
            | "red_kind"
            | "tenant"
            | "created_at"
            | "updated_at"
            | "red_entity_type"
            | "red_capabilities"
            | "red_sequence_id"
    )
}

fn universal_collection_may_have_fields(db: &RedDB, collection: &str, fields: &[String]) -> bool {
    let contract = db.collection_contract(collection);
    if let Some(contract) = contract.as_ref() {
        let declared_match = fields.iter().all(|field| {
            contract
                .declared_columns
                .iter()
                .any(|column| column.name.eq_ignore_ascii_case(field))
        });
        if declared_match && !contract.declared_columns.is_empty() {
            return true;
        }
        if matches!(contract.schema_mode, crate::catalog::SchemaMode::Strict)
            && !contract.declared_columns.is_empty()
        {
            return false;
        }
    }

    let Some(manager) = db.store().get_collection(collection) else {
        return false;
    };
    if let Some(schema) = manager.column_schema() {
        if !schema.is_empty() {
            return fields.iter().all(|field| {
                schema
                    .iter()
                    .any(|column| column.eq_ignore_ascii_case(field))
            });
        }
    }

    if let Some(contract) = contract.as_ref() {
        if !universal_contract_may_have_dynamic_fields(contract) {
            return universal_contract_has_static_fields(contract, fields);
        }
        if universal_contract_has_static_fields(contract, fields) {
            return true;
        }
        return universal_collection_has_observed_fields(&manager, fields);
    }

    universal_collection_has_observed_fields(&manager, fields)
}

fn universal_contract_has_static_fields(
    contract: &crate::physical::CollectionContract,
    fields: &[String],
) -> bool {
    match contract.declared_model {
        crate::catalog::CollectionModel::Graph => fields_match_any(
            fields,
            &["label", "node_type", "from_rid", "to_rid", "weight"],
        ),
        crate::catalog::CollectionModel::Vector => {
            fields_match_any(fields, &["dimension", "content"])
        }
        crate::catalog::CollectionModel::TimeSeries | crate::catalog::CollectionModel::Metrics => {
            fields_match_any(
                fields,
                &[
                    "metric",
                    "timestamp_ns",
                    "timestamp",
                    "time",
                    "value",
                    "tags",
                ],
            )
        }
        crate::catalog::CollectionModel::Queue => fields_match_any(
            fields,
            &["position", "payload", "attempts", "acked", "priority"],
        ),
        crate::catalog::CollectionModel::Kv
        | crate::catalog::CollectionModel::Config
        | crate::catalog::CollectionModel::Vault => fields_match_any(fields, &["key", "value"]),
        _ => false,
    }
}

fn universal_contract_may_have_dynamic_fields(
    contract: &crate::physical::CollectionContract,
) -> bool {
    matches!(
        contract.declared_model,
        crate::catalog::CollectionModel::Document
            | crate::catalog::CollectionModel::Graph
            | crate::catalog::CollectionModel::Hll
            | crate::catalog::CollectionModel::Sketch
            | crate::catalog::CollectionModel::Filter
            | crate::catalog::CollectionModel::Mixed
    ) || matches!(
        contract.declared_model,
        crate::catalog::CollectionModel::Table
    ) && !matches!(contract.schema_mode, crate::catalog::SchemaMode::Strict)
}

fn universal_collection_has_observed_fields(
    manager: &crate::storage::unified::manager::SegmentManager,
    fields: &[String],
) -> bool {
    if fields.is_empty() {
        return false;
    }
    let mut seen = vec![false; fields.len()];
    manager.for_each_entity(|entity| {
        for (idx, field) in fields.iter().enumerate() {
            if !seen[idx] && universal_entity_has_observed_field(entity, field) {
                seen[idx] = true;
            }
        }
        !seen.iter().all(|value| *value)
    });
    seen.into_iter().all(|value| value)
}

fn universal_entity_has_observed_field(
    entity: &crate::storage::unified::UnifiedEntity,
    field: &str,
) -> bool {
    match &entity.data {
        crate::storage::unified::EntityData::Row(row) => row
            .iter_fields()
            .any(|(name, _)| name.eq_ignore_ascii_case(field)),
        crate::storage::unified::EntityData::Node(node) => node
            .properties
            .keys()
            .any(|name| name.eq_ignore_ascii_case(field)),
        crate::storage::unified::EntityData::Edge(edge) => edge
            .properties
            .keys()
            .any(|name| name.eq_ignore_ascii_case(field)),
        _ => false,
    }
}

fn fields_match_any(fields: &[String], candidates: &[&str]) -> bool {
    fields.iter().all(|field| {
        candidates
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(field))
    })
}

#[cfg(test)]
mod tests {
    use crate::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    use crate::storage::query::ast::{CompareOp, FieldRef, Filter, QueryExpr};
    use crate::storage::schema::Value;
    use crate::storage::unified::EntityId;
    use crate::{RedDBOptions, RedDBRuntime};

    fn rt() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
    }

    fn exec(rt: &RedDBRuntime, sql: &str) {
        rt.execute_query(sql)
            .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    }

    fn rid(rt: &RedDBRuntime, table: &str, id: i64) -> u64 {
        let result = rt
            .execute_query(&format!("SELECT rid FROM {table} WHERE id = {id}"))
            .unwrap_or_else(|err| panic!("select rid from {table}: {err:?}"));
        match result.result.records[0].get("rid") {
            Some(Value::UnsignedInteger(id)) => *id,
            Some(Value::Integer(id)) => *id as u64,
            other => panic!("expected rid, got {other:?}"),
        }
    }

    fn text_values(rt: &RedDBRuntime, sql: &str, column: &str) -> Vec<String> {
        let result = rt
            .execute_query(sql)
            .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
        result
            .result
            .records
            .iter()
            .map(|record| match record.get(column) {
                Some(Value::Text(value)) => value.to_string(),
                other => panic!("expected text column {column}, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn covered_sorted_index_rejects_tombstoned_stale_candidate() {
        let rt = rt();
        set_current_connection_id(51001);
        exec(
            &rt,
            "CREATE TABLE mvcc_idx_stale (id INT, status TEXT, marker TEXT)",
        );
        exec(
            &rt,
            "INSERT INTO mvcc_idx_stale (id, status, marker) VALUES (1, 'gone', 'stable')",
        );
        exec(
            &rt,
            "CREATE INDEX idx_mvcc_stale_status ON mvcc_idx_stale (status) USING BTREE",
        );
        let stale_id = rid(&rt, "mvcc_idx_stale", 1);

        exec(&rt, "DELETE FROM mvcc_idx_stale WHERE id = 1");
        rt.index_store_ref()
            .index_entity_insert(
                "mvcc_idx_stale",
                EntityId::new(stale_id),
                &[("status".to_string(), Value::text("gone".to_string()))],
            )
            .expect("force stale index entry");

        let indexed = text_values(
            &rt,
            "SELECT status FROM mvcc_idx_stale WHERE status IN ('gone')",
            "status",
        );
        let scanned = text_values(
            &rt,
            "SELECT status FROM mvcc_idx_stale WHERE marker = 'stable'",
            "status",
        );
        assert_eq!(indexed, scanned);
        assert!(indexed.is_empty());
        clear_current_connection_id();
    }

    #[test]
    fn global_attribute_candidates_use_declared_columns_but_keep_dynamic_collections() {
        let rt = rt();
        exec(&rt, "CREATE TABLE travelers (passport TEXT, name TEXT)");
        exec(&rt, "CREATE TABLE pets (tag TEXT, name TEXT)");
        exec(&rt, "CREATE VECTOR embeddings DIM 2 METRIC cosine");
        exec(&rt, "CREATE TIMESERIES metrics RETENTION 7 d");
        exec(&rt, "CREATE QUEUE jobs");
        exec(
            &rt,
            "INSERT INTO social NODE (label, node_type, passport) \
             VALUES ('person', 'Person', 'ABC123123')",
        );
        exec(
            &rt,
            "INSERT INTO places NODE (label, node_type, name) \
             VALUES ('city', 'Place', 'Paris')",
        );

        let query =
            match crate::storage::query::parser::parse("SELECT * WHERE passport = 'ABC123123'")
                .expect("parse")
                .query
            {
                QueryExpr::Table(query) => query,
                other => panic!("expected table query, got {other:?}"),
            };
        let filter = Filter::Compare {
            field: FieldRef::column("", "passport"),
            op: CompareOp::Eq,
            value: Value::text("ABC123123"),
        };
        let candidates =
            super::universal_candidate_collections_for_filter(&rt.db(), &query, &filter)
                .expect("plain attribute filter should produce candidates");

        assert!(candidates.iter().any(|name| name == "travelers"));
        assert!(candidates.iter().any(|name| name == "social"));
        assert!(!candidates.iter().any(|name| name == "pets"));
        assert!(!candidates.iter().any(|name| name == "places"));

        let content_fields = vec!["content".to_string()];
        assert!(super::universal_collection_may_have_fields(
            &rt.db(),
            "embeddings",
            &content_fields
        ));
        assert!(!super::universal_collection_may_have_fields(
            &rt.db(),
            "metrics",
            &content_fields
        ));

        let tags_fields = vec!["tags".to_string()];
        assert!(super::universal_collection_may_have_fields(
            &rt.db(),
            "metrics",
            &tags_fields
        ));
        assert!(!super::universal_collection_may_have_fields(
            &rt.db(),
            "embeddings",
            &tags_fields
        ));

        let payload_fields = vec!["payload".to_string()];
        assert!(super::universal_collection_may_have_fields(
            &rt.db(),
            "jobs",
            &payload_fields
        ));
        assert!(!super::universal_collection_may_have_fields(
            &rt.db(),
            "travelers",
            &payload_fields
        ));
    }
}
