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
    try_covered_sorted_index_query, try_sorted_index_filtered_by_set, try_sorted_index_lookup,
};
use super::*;
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

pub(crate) fn execute_runtime_canonical_table_query_indexed(
    db: &RedDB,
    query: &TableQuery,
    index_store: Option<&super::index_store::IndexStore>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let effective_projections = effective_table_projections(query);
    let effective_filter = effective_table_filter(query);
    let effective_group_by = effective_table_group_by_exprs(query);
    let effective_having = effective_table_having_filter(query);

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
    if let Some(entity_id) = extract_entity_id_from_filter(&effective_filter) {
        let store = db.store();
        if let Some(entity) = store.get(&query.table, EntityId::new(entity_id)) {
            if !crate::runtime::impl_core::entity_visible_under_current_snapshot(&entity) {
                return Ok(Vec::new());
            }
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
    if let (Some(idx_store), Some(ref filter), false) =
        (index_store, &effective_filter, has_cross_index_candidate)
    {
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
            // ── COVERED QUERY: skip heap fetch when projection ⊆ indexed column ──
            //
            // `SELECT age FROM t WHERE age > 30` — the BTree key IS the value.
            // Return directly from index without touching flat_entities.
            // Only valid when: single column projection matching the indexed column,
            // no GROUP BY, no HAVING, no ORDER BY (those need entity data).
            let explicit_cols = extract_select_column_names(&effective_projections);
            let is_covered_candidate = !explicit_cols.is_empty()
                && effective_group_by.is_empty()
                && effective_having.is_none()
                && query.order_by.is_empty();

            if is_covered_candidate {
                let limit = query.limit.map(|l| l as usize).unwrap_or(usize::MAX);
                if let Some(values) = super::indexed_scan::try_covered_sorted_index_query(
                    filter,
                    &query.table,
                    idx_store,
                    &explicit_cols,
                    limit,
                ) {
                    // Each value becomes a single-field UnifiedRecord
                    let col_name = &explicit_cols[0];
                    let records = values
                        .into_iter()
                        .map(|v| {
                            let mut rec = UnifiedRecord::new();
                            rec.set(col_name, v);
                            rec
                        })
                        .collect();
                    return Ok(records);
                }
            }

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
            // path below so user-specified system fields (e.g. SELECT red_entity_id,
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
                manager.for_each_id(&entity_ids, |_idx, entity| {
                    if stop {
                        return;
                    }
                    if records.len() >= limit {
                        stop = true;
                        return;
                    }
                    if !crate::runtime::impl_core::entity_visible_under_current_snapshot(entity) {
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
            for entity_opt in entities.into_iter().flatten() {
                if records.len() >= limit {
                    break;
                }
                if !crate::runtime::impl_core::entity_visible_under_current_snapshot(&entity_opt) {
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
    if let (Some(idx_store), Some(ref filter)) = (index_store, &effective_filter) {
        if let Some((eq_col, eq_bytes, range_filter)) =
            extract_cross_index_predicates(filter, &query.table, idx_store)
        {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, &eq_col) {
                if let Ok(hash_ids) = idx_store.hash_lookup(&query.table, &idx.name, &eq_bytes) {
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
                            // ── COVERED QUERY: cross-index bitmap path ───────────
                            // If every projected column is the eq_col whose value we
                            // already have from the filter predicate, skip entity fetch.
                            let is_covered = !explicit_cols.is_empty()
                                && effective_group_by.is_empty()
                                && effective_having.is_none()
                                && query.order_by.is_empty()
                                && explicit_cols.iter().all(|c| c == &eq_col);
                            if is_covered {
                                use crate::storage::query::ast::Filter;
                                let eq_value = match filter {
                                    Filter::And(_, _) => {
                                        // Find the eq_col value in the original filter
                                        let mut v = None;
                                        let f = filter;
                                        loop {
                                            match f {
                                                Filter::Compare {
                                                    field: crate::storage::query::ast::FieldRef::TableColumn { column, .. },
                                                    op: crate::storage::query::ast::CompareOp::Eq,
                                                    value,
                                                } if column == &eq_col => { v = Some(value.clone()); break; }
                                                Filter::And(l, r) => {
                                                    // Walk left first, then right
                                                    let mut stk = vec![l.as_ref(), r.as_ref()];
                                                    while let Some(node) = stk.pop() {
                                                        match node {
                                                            Filter::Compare {
                                                                field: crate::storage::query::ast::FieldRef::TableColumn { column, .. },
                                                                op: crate::storage::query::ast::CompareOp::Eq,
                                                                value,
                                                            } if column == &eq_col => { v = Some(value.clone()); break; }
                                                            Filter::And(a, b) => { stk.push(a); stk.push(b); }
                                                            _ => {}
                                                        }
                                                    }
                                                    break;
                                                }
                                                _ => break,
                                            }
                                        }
                                        v
                                    }
                                    _ => None,
                                };
                                if let Some(val) = eq_value {
                                    let records = intersection_ids
                                        .iter()
                                        .take(limit)
                                        .map(|_| {
                                            let mut rec = UnifiedRecord::with_capacity(1);
                                            rec.set(&eq_col, val.clone());
                                            rec
                                        })
                                        .collect();
                                    return Ok(records);
                                }
                            }

                            let store = db.store();
                            let entities = store.get_batch(&query.table, &intersection_ids);
                            let lean = explicit_cols.is_empty();
                            let mut records = Vec::with_capacity(intersection_ids.len().min(limit));
                            for entity_opt in entities.into_iter().flatten() {
                                if records.len() >= limit {
                                    break;
                                }
                                if !crate::runtime::impl_core::entity_visible_under_current_snapshot(
                                    &entity_opt,
                                ) {
                                    continue;
                                }
                                if compiled_filter
                                    .as_ref()
                                    .map_or(true, |cf| cf.evaluate(&entity_opt))
                                {
                                    let record_opt = if lean {
                                        super::super::record_search::runtime_table_record_lean(
                                            entity_opt,
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
    if let (Some(idx_store), Some(ref filter)) = (index_store, &effective_filter) {
        let mut eq_candidates: Vec<(String, Vec<u8>, crate::storage::schema::Value)> = Vec::new();
        extract_all_eq_candidates(filter, &mut eq_candidates);

        // Collect one TidBitmap per indexed equality column.
        // TidBitmap uses RoaringBitmap internally — intersection is word-level AND
        // (~32x faster than HashSet retain for 10K+ IDs, and far more cache-friendly).
        // Entity IDs are cast to u32; safe for any reasonable in-memory DB size (< 4 B).
        let mut bitmaps: Vec<crate::storage::index::tid_bitmap::TidBitmap> = Vec::new();
        for (col, val_bytes, _val) in &eq_candidates {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, col) {
                if let Ok(ids) = idx_store.hash_lookup(&query.table, &idx.name, val_bytes) {
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

            // ── COVERED QUERY: TID bitmap path ───────────────────────────────
            // When every projected column is an equality column whose value
            // is available from the filter predicate, skip entity fetch entirely.
            // e.g. `SELECT city FROM t WHERE city='NYC' AND status='active'`
            // returns {city:'NYC'} for every matching entity ID without a heap read.
            let is_covered = !explicit_cols.is_empty()
                && effective_group_by.is_empty()
                && effective_having.is_none()
                && query.order_by.is_empty()
                && explicit_cols.iter().all(|proj_col| {
                    eq_candidates
                        .iter()
                        .any(|(eq_col, _, _)| eq_col == proj_col)
                });
            if is_covered {
                // Build a single template record from eq_candidates values.
                // All result rows are identical (same equality predicates).
                let mut template = UnifiedRecord::with_capacity(explicit_cols.len());
                for proj_col in &explicit_cols {
                    if let Some((_, _, val)) = eq_candidates.iter().find(|(c, _, _)| c == proj_col)
                    {
                        template.set(proj_col, val.clone());
                    }
                }
                // Count surviving IDs after optional range narrowing — we still need
                // to know how many rows match, but we don't touch the entity heap.
                let count = intersection.len().min(limit);
                return Ok(vec![template; count]);
            }

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
            for entity_opt in entities.into_iter().flatten() {
                if records.len() >= limit {
                    break;
                }
                if !crate::runtime::impl_core::entity_visible_under_current_snapshot(&entity_opt) {
                    continue;
                }
                if compiled_filter.evaluate(&entity_opt) {
                    let record_opt = if lean {
                        super::super::record_search::runtime_table_record_lean(entity_opt)
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
    if let (Some(idx_store), Some(ref filter)) = (index_store, &effective_filter) {
        if let Some((column, value_bytes)) = extract_index_candidate(filter) {
            if let Some(idx) = idx_store.find_index_for_column(&query.table, &column) {
                // ── INDEX-ONLY SCAN CHECK ──────────────────────────────────────
                // When the query projects only the indexed column (SELECT col FROM t WHERE col = ?)
                // and the collection has sealed (all-visible) data, skip the entity heap fetch.
                // The value we need is the equality predicate value itself — no entity scan needed.
                let projected_cols = extract_select_column_names(&effective_projections);
                let index_only_eligible = !projected_cols.is_empty()
                    && projected_cols.iter().all(|c| c == &column)
                    && effective_group_by.is_empty()
                    && effective_having.is_none()
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

                let mut entity_ids = idx_store
                    .hash_lookup(&query.table, &idx.name, &value_bytes)
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
                        for entity_opt in entities.into_iter().flatten() {
                            if records.len() >= limit {
                                break;
                            }
                            if !crate::runtime::impl_core::entity_visible_under_current_snapshot(
                                &entity_opt,
                            ) {
                                continue;
                            }
                            if compiled_filter
                                .as_ref()
                                .map_or(true, |cf| cf.evaluate(&entity_opt))
                            {
                                let record_opt = if lean {
                                    super::super::record_search::runtime_table_record_lean(
                                        entity_opt,
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

    // ── FAST PATH: Simple filtered scan — bypass planner for basic WHERE queries ──
    // Evaluates the filter directly on raw entity data to avoid materializing
    // UnifiedRecord for every entity in the collection.
    // Excludes universal entity sources (e.g. "any") which span all collections.
    if effective_filter.is_some()
        && effective_group_by.is_empty()
        && effective_having.is_none()
        && query.expand.is_none()
        && !effective_projections
            .iter()
            .any(|p| matches!(p, Projection::Function(_, _) | Projection::Expression(_, _)))
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
        let limit = explicit_limit.unwrap_or(10000) as usize;

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
        if use_parallel {
            // Parallel scan spawns worker threads that don't inherit the
            // main thread's CURRENT_SNAPSHOT thread-local. Capture the
            // context here so each closure invocation (on any thread) runs
            // the same MVCC visibility gate.
            let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
            let matching = manager.query_all_zoned(&zone_preds, |entity| {
                crate::runtime::impl_core::entity_visible_with_context(snap_ctx.as_ref(), entity)
                    && compiled.evaluate(entity)
            });
            for entity in &matching {
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
                } else if lean_select_star {
                    super::super::record_search::runtime_table_record_lean(entity.clone())
                } else {
                    runtime_table_record_from_entity(entity.clone())
                };
                if let Some(record) = record {
                    if requires_filter_recheck {
                        let Some(filter_record) = runtime_table_record_from_entity(entity.clone())
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
            manager.for_each_entity_zoned(&zone_preds, |entity| {
                if records.len() >= limit {
                    return false; // stop iteration
                }
                if !crate::runtime::impl_core::entity_visible_under_current_snapshot(entity) {
                    return true; // skip hidden tuple, keep scanning
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
                    } else if lean_select_star {
                        super::super::record_search::runtime_table_record_lean(entity.clone())
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
            super::super::join_filter::sort_records_by_order_by_with_db(
                Some(db),
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
    let has_scalar_function = effective_projections
        .iter()
        .any(|p| matches!(p, Projection::Function(_, _) | Projection::Expression(_, _)));
    if effective_filter.is_none()
        && effective_group_by.is_empty()
        && effective_having.is_none()
        && query.expand.is_none()
        && !has_scalar_function
    {
        // LIMIT + OFFSET pushdown: pre-scan cap is `offset + limit` so
        // the scan loop stops once we have enough to skip + keep. An
        // unbounded scan (no LIMIT) still reads the full table, same
        // as before.
        let scan_cap = match (query.offset, query.limit) {
            (_, None) if !query.order_by.is_empty() => None, // ORDER BY needs full set
            (Some(off), Some(lim)) => Some(off as usize + lim as usize),
            (None, Some(lim)) if query.order_by.is_empty() => Some(lim as usize),
            _ => None,
        };
        let mut records =
            scan_runtime_table_source_records_limited(db, query.table.as_str(), scan_cap)?;
        let table_name = query.table.as_str();
        let table_alias = query.alias.as_deref().unwrap_or(table_name);

        if !query.order_by.is_empty() {
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
    match node.operator.as_str() {
        "table_scan" | "index_seek" | "entity_scan" | "document_path_index_seek" => {
            // ── FAST PATH 1: Direct entity_id lookup (O(1) instead of full scan) ──
            if let Some(entity_id) = extract_entity_id_from_filter(&effective_filter) {
                let store = db.store();
                if let Some(entity) = store.get(&context.query.table, EntityId::new(entity_id)) {
                    if !crate::runtime::impl_core::entity_visible_under_current_snapshot(&entity) {
                        return Ok(Vec::new());
                    }
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
            if effective_filter.is_some()
                && !effective_table_projections(context.query)
                    .iter()
                    .any(|p| matches!(p, Projection::Function(_, _) | Projection::Expression(_, _)))
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

                let effective_projections = effective_table_projections(context.query);
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
                manager.for_each_entity(|entity| {
                    if records.len() >= limit {
                        return false;
                    }
                    if !crate::runtime::impl_core::entity_visible_under_current_snapshot(entity) {
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
            scan_runtime_table_source_records_limited(
                db,
                context.query.table.as_str(),
                scan_cap,
            )
        }
        "filter" | "entity_filter" => {
            // ── FAST PATH: Direct entity_id lookup (O(1)) ──
            if let Some(entity_id) = extract_entity_id_from_filter(&effective_filter) {
                let store = db.store();
                if let Some(entity) = store.get(&context.query.table, EntityId::new(entity_id)) {
                    if !crate::runtime::impl_core::entity_visible_under_current_snapshot(&entity) {
                        return Ok(Vec::new());
                    }
                    return Ok(runtime_table_record_from_entity(entity)
                        .into_iter()
                        .collect());
                }
                return Ok(Vec::new());
            }

            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = effective_filter.as_ref() {
                records.retain(|record| {
                    evaluate_runtime_filter_with_db(
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
        "document_path_filter" => {
            let mut records = execute_runtime_canonical_table_child(db, node, context)?;
            if let Some(filter) = effective_filter.as_ref() {
                records.retain(|record| {
                    runtime_record_has_document_capability(record)
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
            let records = execute_runtime_canonical_table_child(db, node, context)?;
            let document_projection = node.operator == "document_projection";
            let entity_projection = node.operator == "entity_projection";
            Ok(records
                .iter()
                .map(|record| {
                    project_runtime_record_with_db(
                        Some(db),
                        record,
                        &effective_table_projections(context.query),
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
