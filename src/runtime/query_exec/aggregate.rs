//! Aggregate query executor.
//!
//! Handles SQL aggregates (`COUNT`, `SUM`, `AVG`, `MIN`, `MAX`, `STDDEV`,
//! `VARIANCE`, `MEDIAN`, `PERCENTILE`, `GROUP_CONCAT`, `STRING_AGG`, `FIRST`, `LAST`,
//! `ARRAY_AGG`, `COUNT_DISTINCT`) plus `GROUP BY` (including
//! `TIME_BUCKET` grouping for time-series rollups).
//!
//! Split out of `query_exec.rs` to keep the main executor file focused
//! on per-row scan paths. The entry point is
//! [`execute_aggregate_query`] which is dispatched to from
//! `execute_runtime_table_query` when any projection is an aggregate
//! function.

use super::filter_compiled::{classify_field, EntityColumnResolver};
use crate::api::{RedDBError, RedDBResult};
use crate::runtime::join_filter::{
    eval_projection_value_with_db, evaluate_runtime_filter_with_db, field_ref_name,
    projection_name, runtime_partial_cmp, sort_records_by_order_by_with_db,
};
use crate::runtime::runtime_table_record_from_entity_ref;
use crate::storage::query::ast::{
    BinOp, CompareOp, Expr, FieldRef, Filter, OrderByClause, Projection, Span, UnaryOp,
};
use crate::storage::query::sql_lowering::{
    effective_table_filter, effective_table_group_by_exprs, effective_table_having_filter,
    effective_table_projections, expr_to_projection as lower_expr_to_projection,
};
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::{value_to_canonical_key, CanonicalKey, Value};
use crate::RedDB;

use super::TableQuery;

/// Return `true` when any projection in the query is a known aggregate
/// function. Used by the executor to decide whether to dispatch to
/// [`execute_aggregate_query`].
pub(crate) fn has_aggregate_projections(projections: &[Projection]) -> bool {
    projections.iter().any(|p| {
        matches!(
            p,
            Projection::Function(name, _)
                if is_aggregate_function(base_function_name(name))
        )
    })
}

pub(crate) fn base_function_name(name: &str) -> &str {
    name.split(':').next().unwrap_or(name)
}

pub(crate) fn is_aggregate_function(name: &str) -> bool {
    matches!(
        name,
        "COUNT"
            | "AVG"
            | "SUM"
            | "MIN"
            | "MAX"
            | "STDDEV"
            | "VARIANCE"
            | "MEDIAN"
            | "PERCENTILE"
            | "GROUP_CONCAT"
            | "STRING_AGG"
            | "FIRST"
            | "LAST"
            | "ARRAY_AGG"
            | "COUNT_DISTINCT"
    )
}

/// Execute a query with aggregate functions (COUNT, AVG, SUM, MIN, MAX, GROUP BY).
pub(crate) fn execute_aggregate_query(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<UnifiedResult> {
    validate_aggregate_projection_shape(query)?;
    let effective_projections = effective_table_projections(query);
    let effective_filter = effective_table_filter(query);
    let effective_group_by = effective_table_group_by_exprs(query);
    let runtime_plan = prepare_aggregate_runtime_plan(query);
    let mut all_aggregate_projections = effective_projections
        .iter()
        .filter(|projection| {
            matches!(
                projection,
                Projection::Function(name, _)
                    if is_aggregate_function(base_function_name(name))
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    all_aggregate_projections.extend(runtime_plan.hidden_aggregates.iter().cloned());
    let mut seen_aggregate_signatures = std::collections::HashSet::new();
    all_aggregate_projections.retain(|projection| {
        let Projection::Function(name, args) = projection else {
            return false;
        };
        let func_name = base_function_name(name).to_uppercase();
        if !is_aggregate_function(&func_name) {
            return false;
        }
        seen_aggregate_signatures.insert(aggregate_projection_signature(&func_name, args))
    });

    let manager = db
        .store()
        .get_collection(query.table.as_str())
        .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let has_group_by = !effective_group_by.is_empty();

    // Compile the filter ONCE before the for_each_entity loop. The
    // compiled form pre-classifies every FieldRef into an
    // EntityFieldKind, so the per-row evaluator skips the ~6
    // system-field string compares + entity-kind cascade that
    // evaluate_entity_filter performs on every call.
    //
    // See `runtime/query_exec/filter_compiled.rs` for the algorithm
    // and `runtime/query_exec/table.rs` for the canonical scan-path
    // wire-up.
    let compiled_filter = effective_filter
        .as_ref()
        .map(|f| super::filter_compiled::CompiledEntityFilter::compile(f, table_name, table_alias));

    // ── GROUP BY fast path ───────────────────────────────────────────────
    // Pre-classify each GROUP BY expression once. When all expressions are
    // simple `Expr::Column` references (the common case), we can extract
    // values directly from the entity without materialising a full
    // `UnifiedRecord` — skipping the `entity.clone()` + field-map rebuild
    // that `runtime_table_record_from_entity` performs per row.
    //
    // TIME_BUCKET and non-column expressions fall back to record
    // materialisation (signalled by `None` in the parallel vec).
    let group_by_kinds: Vec<Option<EntityColumnResolver>> = if has_group_by {
        effective_group_by
            .iter()
            .map(|expr| {
                // TIME_BUCKET grouping requires a record (timestamp arithmetic).
                if parse_time_bucket_group_expr(&group_expr_key(expr).unwrap_or_default()).is_some()
                {
                    return None;
                }
                match expr {
                    Expr::Column { field, .. } => {
                        let col_name = field_ref_name(field);
                        let kind = classify_field(field, table_name, table_alias);
                        if matches!(
                            kind,
                            super::filter_compiled::EntityFieldKind::DocumentPath(_)
                                | super::filter_compiled::EntityFieldKind::Unknown
                        ) {
                            None
                        } else {
                            Some(EntityColumnResolver { kinds: vec![kind] })
                        }
                    }
                    _ => None,
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    // True iff every GROUP BY field can be read directly from the entity.
    let group_by_all_fast = has_group_by && group_by_kinds.iter().all(|k| k.is_some());

    // ── Aggregate argument fast path ─────────────────────────────────────
    // For projections like SUM(amount), COUNT(id), MIN(price) the argument
    // is a single simple column reference. Pre-classify once so the hot
    // loop can read the value from the entity without a record lookup.
    let agg_arg_kinds: Vec<Option<super::filter_compiled::EntityFieldKind>> =
        all_aggregate_projections
            .iter()
            .map(|proj| {
                let Projection::Function(_, args) = proj else {
                    return None;
                };
                match args.first() {
                    Some(Projection::Field(field, _)) => {
                        let kind = classify_field(field, table_name, table_alias);
                        if matches!(
                            kind,
                            super::filter_compiled::EntityFieldKind::DocumentPath(_)
                                | super::filter_compiled::EntityFieldKind::Unknown
                        ) {
                            None
                        } else {
                            Some(kind)
                        }
                    }
                    Some(Projection::Column(col)) if !col.starts_with("LIT:") && col != "*" => {
                        let field = FieldRef::TableColumn {
                            table: String::new(),
                            column: col.clone(),
                        };
                        let kind = classify_field(&field, table_name, table_alias);
                        if matches!(
                            kind,
                            super::filter_compiled::EntityFieldKind::DocumentPath(_)
                                | super::filter_compiled::EntityFieldKind::Unknown
                        ) {
                            None
                        } else {
                            Some(kind)
                        }
                    }
                    _ => None,
                }
            })
            .collect();

    // ── Compile the aggregation plan ─────────────────────────────────────────
    // Assigns a slot index to every projection in `all_aggregate_projections`
    // so the hot loop can use O(1) array writes instead of HashMap lookups.
    let agg_plan = CompiledAggPlan::compile(&all_aggregate_projections);

    // Work-mem cap: 64 MB mirrors PostgreSQL's work_mem GUC default.
    // When the in-memory HashMap exceeds `max_groups` entries, we flush the
    // current partial state to a SpilledHashAgg batch file on tmpfs and reset
    // the local HashMap.  The final drain() merges all on-disk batches back.
    const WORK_MEM_BYTES: usize = 64 * 1024 * 1024;
    // Approximate per-entry cost: 128 B for AggState + 64 B for group key + overhead
    const ESTIMATED_ENTRY_BYTES: usize = 256;
    let max_groups = WORK_MEM_BYTES / ESTIMATED_ENTRY_BYTES;

    // Hot accumulator: in-memory HashMap for per-row mutation.
    // Flushed to `spill_agg` when it exceeds max_groups.
    let mut groups: std::collections::HashMap<AggregateGroupKey, AggregateGroup> =
        std::collections::HashMap::new();

    // SpilledHashAgg receives flushed batches and performs the final merge.
    let spill_dir = {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let d = std::env::temp_dir().join(format!("reddb-agg-{pid}-{seq}"));
        std::fs::create_dir_all(&d)
            .map_err(|e| RedDBError::Query(format!("agg spill dir: {e}")))?;
        d
    };
    let mut spill_agg = crate::storage::query::executors::agg_spill::SpilledHashAgg::<
        AggregateGroupKey,
        AggregateGroup,
    >::new(spill_dir, WORK_MEM_BYTES, ESTIMATED_ENTRY_BYTES);
    let mut spill_err: Option<String> = None;

    manager.for_each_entity(|entity| {
        if !crate::runtime::impl_core::entity_visible_under_current_snapshot(entity) {
            return true;
        }
        if let Some(c) = compiled_filter.as_ref() {
            if !c.evaluate(entity) {
                return true;
            }
        }

        // ── Lazy record materialisation ──────────────────────────────────
        // We defer `runtime_table_record_from_entity` until we actually
        // need it (complex GROUP BY exprs or aggregate args that can't be
        // read directly from the entity).  For the common case — plain
        // column GROUP BY + single-column agg args — we never build it.
        let mut record_cache: Option<UnifiedRecord> = None;

        // Helper: materialise the record exactly once if not yet done.
        macro_rules! get_or_make_record {
            () => {{
                if record_cache.is_none() {
                    record_cache = runtime_table_record_from_entity_ref(entity);
                }
                record_cache.as_ref()
            }};
        }

        let group_values = if has_group_by {
            if group_by_all_fast {
                // Fast path: all GROUP BY are simple columns → read from entity.
                let mut values = Vec::with_capacity(effective_group_by.len());
                for (resolver_opt, expr) in group_by_kinds.iter().zip(&effective_group_by) {
                    let value = if let Some(resolver) = resolver_opt {
                        resolver.get_value(0, entity).map(|v| v.into_owned())
                    } else {
                        None
                    };
                    if let Some(v) = value {
                        values.push(v);
                    } else {
                        // Shouldn't happen (group_by_all_fast is true) but
                        // fall back gracefully.
                        let Some(rec) = get_or_make_record!() else {
                            return true;
                        };
                        let Some(v) = resolve_group_by_value(db, expr, rec) else {
                            return true;
                        };
                        values.push(v);
                    }
                }
                values
            } else {
                // Slow path: at least one complex GROUP BY expr.
                let Some(rec) = get_or_make_record!() else {
                    return true;
                };
                let mut values = Vec::with_capacity(effective_group_by.len());
                for group_expr in &effective_group_by {
                    let Some(value) = resolve_group_by_value(db, group_expr, rec) else {
                        return true;
                    };
                    values.push(value);
                }
                values
            }
        } else {
            Vec::new()
        };
        // Build the group-by key in a single String buffer instead
        // of `iter().map().collect::<Vec<_>>().join("|")`, which used
        // to pay N+1 String allocations per row. See sibling
        // `aggregation.rs::make_group_key` for the same optimisation
        // on the executor path.
        let group_key = if has_group_by {
            build_aggregate_group_key(&group_values)
        } else {
            Vec::new()
        };

        // One-probe entry dispatch. The old code did `contains_key`
        // (hash probe #1) + `entry()` (hash probe #2) on every row —
        // doubling the HashMap cost on the hot aggregate hit path.
        // Now: one probe, one match; the spill check only runs for
        // Vacant entries and only when the map is already at cap.
        use std::collections::hash_map::Entry;
        let need_spill_check = groups.len() >= max_groups;
        let group = match groups.entry(group_key) {
            Entry::Occupied(occ) => occ.into_mut(),
            Entry::Vacant(vac) => {
                if need_spill_check {
                    // Re-extract the key (consumed by the insert) and
                    // flush every existing group to the spill file,
                    // then start a fresh in-memory batch holding this
                    // new group.
                    let fresh_key = vac.key().clone();
                    drop(vac);
                    let batch = std::mem::take(&mut groups);
                    for (k, v) in batch {
                        if let Err(e) = spill_agg.accumulate(k, v) {
                            spill_err = Some(format!("agg spill error: {e}"));
                            return false; // stop iteration
                        }
                    }
                    groups.entry(fresh_key).or_insert_with(|| AggregateGroup {
                        group_values: group_values.clone(),
                        state: SlottedAggState::new(&agg_plan),
                    })
                } else {
                    vac.insert(AggregateGroup {
                        group_values: group_values.clone(),
                        state: SlottedAggState::new(&agg_plan),
                    })
                }
            }
        };
        let state = &mut group.state;
        state.count += 1;

        // Accumulate values — slot-indexed, zero HashMap/String overhead per row.
        for (proj_idx, proj) in all_aggregate_projections.iter().enumerate() {
            let Projection::Function(func, args) = proj else {
                continue;
            };
            let func_name = base_function_name(func);
            if !is_aggregate_function(func_name) {
                continue;
            }

            let slot = match agg_plan.proj_slots.get(proj_idx) {
                Some(s) => s,
                None => continue,
            };

            // COUNT(*) — already counted above.
            if matches!(slot, ProjSlot::CountStar) {
                continue;
            }

            // Resolve argument value: entity fast path first, then record.
            let val = if let Some(kind) = agg_arg_kinds.get(proj_idx).and_then(|k| k.as_ref()) {
                super::filter_compiled::resolve_kind(kind, entity)
                    .map(|v| v.into_owned())
                    .or_else(|| {
                        get_or_make_record!()
                            .and_then(|rec| resolve_aggregate_argument_value(db, args.first(), rec))
                    })
            } else {
                match get_or_make_record!() {
                    Some(rec) => resolve_aggregate_argument_value(db, args.first(), rec),
                    None => continue,
                }
            };
            let Some(val) = val else { continue };
            let num = value_to_f64(&val);

            match slot {
                ProjSlot::CountStar => {}
                ProjSlot::CountOnly(idx) => {
                    if !matches!(val, Value::Null) {
                        state.count_only[*idx] += 1;
                    }
                }
                ProjSlot::SumCount(idx) => {
                    if let Some(n) = num {
                        state.sums[*idx] += n;
                        state.sum_agg_counts[*idx] += 1;
                    }
                }
                ProjSlot::SumCountSq(idx) => {
                    if let Some(n) = num {
                        state.sums[*idx] += n;
                        state.sum_agg_counts[*idx] += 1;
                        state.sum_squares[*idx] += n * n;
                    }
                }
                ProjSlot::Min(idx) => {
                    update_extreme_value_slot(
                        &mut state.mins[*idx],
                        &val,
                        std::cmp::Ordering::Less,
                    );
                }
                ProjSlot::Max(idx) => {
                    update_extreme_value_slot(
                        &mut state.maxs[*idx],
                        &val,
                        std::cmp::Ordering::Greater,
                    );
                }
                ProjSlot::AllValues(idx) => {
                    if let Some(n) = num {
                        state.all_values[*idx].push(n);
                    }
                }
                ProjSlot::Concat(idx) => {
                    if !matches!(val, Value::Null) {
                        let text: String = match &val {
                            Value::Text(s) => s.to_string(),
                            other => other.display_string(),
                        };
                        state.concat_values[*idx].push(text);
                    }
                }
                ProjSlot::First(idx) => {
                    if state.first_values[*idx].is_none() {
                        state.first_values[*idx] = Some(val);
                    }
                }
                ProjSlot::Last(idx) => {
                    state.last_values[*idx] = Some(val);
                }
                ProjSlot::Array(idx) => {
                    state.array_values[*idx].push(val);
                }
                ProjSlot::Distinct(idx) => {
                    if !matches!(val, Value::Null) {
                        state.distinct_sets[*idx]
                            .get_or_insert_with(std::collections::HashSet::new)
                            .insert(group_value_key(&val));
                    }
                }
            }
        }
        true
    });

    // Propagate any spill I/O error from the iteration callback
    if let Some(e) = spill_err {
        return Err(RedDBError::Query(e));
    }

    // Flush the remaining in-memory groups to spill_agg, then drain all
    // on-disk batches back into a single merged HashMap.
    // When no spill occurred, spill_agg holds only the in-memory table.
    for (k, v) in groups {
        spill_agg
            .accumulate(k, v)
            .map_err(|e| RedDBError::Query(format!("agg spill flush: {e}")))?;
    }
    let groups = spill_agg
        .drain()
        .map_err(|e| RedDBError::Query(format!("agg spill drain: {e}")))?;

    // Build result records from accumulated groups
    let mut records = Vec::with_capacity(groups.len().max(1));
    let mut columns = Vec::new();

    for group in groups.values() {
        let mut record = UnifiedRecord::new();

        // Add GROUP BY columns
        if has_group_by {
            for (index, group_expr) in effective_group_by.iter().enumerate() {
                let Some(value) = group.group_values.get(index).cloned() else {
                    continue;
                };
                let label = group_output_label(query, group_expr);
                if !columns.contains(&label) {
                    columns.push(label.clone());
                }
                record.set(
                    &group_expr_key(group_expr).unwrap_or_else(|| label.clone()),
                    value.clone(),
                );
                record.set(&label, value);
            }
        }

        // Add visible aggregate results
        for proj in &effective_projections {
            if let Some((result_name, result_val)) =
                aggregate_projection_result_slotted(proj, &group.state, &agg_plan)
            {
                if !columns.contains(&result_name) {
                    columns.push(result_name.clone());
                }
                record.set(&result_name, result_val);
            }
        }

        for proj in &runtime_plan.hidden_aggregates {
            if let Some((result_name, result_val)) =
                aggregate_projection_result_slotted(proj, &group.state, &agg_plan)
            {
                record.set(&result_name, result_val);
            }
        }

        if having_matches(db, runtime_plan.having.as_ref(), &record) {
            records.push(record);
        }
    }

    // If no input rows matched, return a single aggregate row.
    let empty_state = SlottedAggState::new(&agg_plan);
    if groups.is_empty() && !has_group_by {
        let mut record = UnifiedRecord::new();
        for proj in &effective_projections {
            if let Some((result_name, result_val)) =
                empty_aggregate_projection_result_slotted(proj, &empty_state, &agg_plan)
            {
                if !columns.contains(&result_name) {
                    columns.push(result_name.clone());
                }
                record.set(&result_name, result_val);
            }
        }
        for proj in &runtime_plan.hidden_aggregates {
            if let Some((result_name, result_val)) =
                empty_aggregate_projection_result_slotted(proj, &empty_state, &agg_plan)
            {
                record.set(&result_name, result_val);
            }
        }
        if having_matches(db, runtime_plan.having.as_ref(), &record) {
            records.push(record);
        }
    }

    if !runtime_plan.order_by.is_empty() {
        sort_records_by_order_by_with_db(
            Some(db),
            &mut records,
            &runtime_plan.order_by,
            None,
            None,
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

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    })
}

#[derive(Default)]
struct AggregateRuntimePlan {
    hidden_aggregates: Vec<Projection>,
    having: Option<Filter>,
    order_by: Vec<OrderByClause>,
}

#[derive(Default)]
struct HiddenAggregateRegistry {
    projections: Vec<Projection>,
    outputs_by_signature: std::collections::HashMap<String, String>,
}

impl HiddenAggregateRegistry {
    fn ensure_output_name(&mut self, func_name: &str, args: &[Expr]) -> Option<String> {
        let signature = aggregate_expr_signature(func_name, args)?;
        if let Some(output_name) = self.outputs_by_signature.get(&signature) {
            return Some(output_name.clone());
        }

        let projection_args = args
            .iter()
            .map(projection_from_expr)
            .collect::<Option<Vec<_>>>()?;
        let col_name = aggregate_argument_key(&projection_args)?;
        let projection = Projection::Function(func_name.to_string(), projection_args);
        let output_name = aggregate_output_name(&projection, func_name, &col_name);

        self.outputs_by_signature
            .insert(signature, output_name.clone());
        self.projections.push(projection);
        Some(output_name)
    }
}

fn prepare_aggregate_runtime_plan(query: &TableQuery) -> AggregateRuntimePlan {
    let effective_projections = effective_table_projections(query);
    let visible_outputs = effective_projections
        .iter()
        .filter_map(visible_aggregate_output_name)
        .collect::<std::collections::HashMap<_, _>>();
    let mut hidden = HiddenAggregateRegistry::default();

    let having = effective_table_having_filter(query)
        .as_ref()
        .map(|filter| rewrite_aggregate_filter_refs(filter, &visible_outputs, &mut hidden));
    let order_by = query
        .order_by
        .iter()
        .map(|clause| rewrite_aggregate_order_by_refs(clause, &visible_outputs, &mut hidden))
        .collect();

    AggregateRuntimePlan {
        hidden_aggregates: hidden.projections,
        having,
        order_by,
    }
}

fn visible_aggregate_output_name(projection: &Projection) -> Option<(String, String)> {
    let Projection::Function(name, args) = projection else {
        return None;
    };
    let func_name = base_function_name(name).to_uppercase();
    if !is_aggregate_function(&func_name) {
        return None;
    }

    let signature = aggregate_projection_signature(&func_name, args);
    let col_name = aggregate_argument_key(args)?;
    Some((
        signature,
        aggregate_output_name(projection, &func_name, &col_name),
    ))
}

fn rewrite_aggregate_order_by_refs(
    clause: &OrderByClause,
    visible_outputs: &std::collections::HashMap<String, String>,
    hidden: &mut HiddenAggregateRegistry,
) -> OrderByClause {
    OrderByClause {
        field: clause.field.clone(),
        expr: clause
            .expr
            .as_ref()
            .map(|expr| rewrite_aggregate_expr_refs(expr, visible_outputs, hidden)),
        ascending: clause.ascending,
        nulls_first: clause.nulls_first,
    }
}

fn rewrite_aggregate_filter_refs(
    filter: &Filter,
    visible_outputs: &std::collections::HashMap<String, String>,
    hidden: &mut HiddenAggregateRegistry,
) -> Filter {
    match filter {
        Filter::CompareExpr { lhs, op, rhs } => Filter::CompareExpr {
            lhs: rewrite_aggregate_expr_refs(lhs, visible_outputs, hidden),
            op: *op,
            rhs: rewrite_aggregate_expr_refs(rhs, visible_outputs, hidden),
        },
        Filter::And(left, right) => Filter::And(
            Box::new(rewrite_aggregate_filter_refs(left, visible_outputs, hidden)),
            Box::new(rewrite_aggregate_filter_refs(
                right,
                visible_outputs,
                hidden,
            )),
        ),
        Filter::Or(left, right) => Filter::Or(
            Box::new(rewrite_aggregate_filter_refs(left, visible_outputs, hidden)),
            Box::new(rewrite_aggregate_filter_refs(
                right,
                visible_outputs,
                hidden,
            )),
        ),
        Filter::Not(inner) => Filter::Not(Box::new(rewrite_aggregate_filter_refs(
            inner,
            visible_outputs,
            hidden,
        ))),
        other => other.clone(),
    }
}

fn rewrite_aggregate_expr_refs(
    expr: &Expr,
    visible_outputs: &std::collections::HashMap<String, String>,
    hidden: &mut HiddenAggregateRegistry,
) -> Expr {
    match expr {
        Expr::FunctionCall { name, args, span } => {
            let func_name = name.to_uppercase();
            if is_aggregate_function(&func_name) {
                if let Some(signature) = aggregate_expr_signature(&func_name, args) {
                    if let Some(output_name) = visible_outputs.get(&signature) {
                        return aggregate_output_expr(output_name.clone(), *span);
                    }
                }
                if let Some(output_name) = hidden.ensure_output_name(&func_name, args) {
                    return aggregate_output_expr(output_name, *span);
                }
            }

            Expr::FunctionCall {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| rewrite_aggregate_expr_refs(arg, visible_outputs, hidden))
                    .collect(),
                span: *span,
            }
        }
        Expr::BinaryOp { op, lhs, rhs, span } => Expr::BinaryOp {
            op: *op,
            lhs: Box::new(rewrite_aggregate_expr_refs(lhs, visible_outputs, hidden)),
            rhs: Box::new(rewrite_aggregate_expr_refs(rhs, visible_outputs, hidden)),
            span: *span,
        },
        Expr::UnaryOp { op, operand, span } => Expr::UnaryOp {
            op: *op,
            operand: Box::new(rewrite_aggregate_expr_refs(
                operand,
                visible_outputs,
                hidden,
            )),
            span: *span,
        },
        Expr::Cast {
            inner,
            target,
            span,
        } => Expr::Cast {
            inner: Box::new(rewrite_aggregate_expr_refs(inner, visible_outputs, hidden)),
            target: *target,
            span: *span,
        },
        Expr::Case {
            branches,
            else_,
            span,
        } => Expr::Case {
            branches: branches
                .iter()
                .map(|(cond, value)| {
                    (
                        rewrite_aggregate_expr_refs(cond, visible_outputs, hidden),
                        rewrite_aggregate_expr_refs(value, visible_outputs, hidden),
                    )
                })
                .collect(),
            else_: else_
                .as_ref()
                .map(|expr| Box::new(rewrite_aggregate_expr_refs(expr, visible_outputs, hidden))),
            span: *span,
        },
        Expr::IsNull {
            operand,
            negated,
            span,
        } => Expr::IsNull {
            operand: Box::new(rewrite_aggregate_expr_refs(
                operand,
                visible_outputs,
                hidden,
            )),
            negated: *negated,
            span: *span,
        },
        Expr::InList {
            target,
            values,
            negated,
            span,
        } => Expr::InList {
            target: Box::new(rewrite_aggregate_expr_refs(target, visible_outputs, hidden)),
            values: values
                .iter()
                .map(|value| rewrite_aggregate_expr_refs(value, visible_outputs, hidden))
                .collect(),
            negated: *negated,
            span: *span,
        },
        Expr::Between {
            target,
            low,
            high,
            negated,
            span,
        } => Expr::Between {
            target: Box::new(rewrite_aggregate_expr_refs(target, visible_outputs, hidden)),
            low: Box::new(rewrite_aggregate_expr_refs(low, visible_outputs, hidden)),
            high: Box::new(rewrite_aggregate_expr_refs(high, visible_outputs, hidden)),
            negated: *negated,
            span: *span,
        },
        other => other.clone(),
    }
}

fn aggregate_output_expr(output_name: String, span: Span) -> Expr {
    Expr::Column {
        field: FieldRef::TableColumn {
            table: String::new(),
            column: output_name,
        },
        span,
    }
}

fn aggregate_projection_signature(func_name: &str, args: &[Projection]) -> String {
    let rendered = args
        .iter()
        .map(render_projection_signature)
        .collect::<Vec<_>>()
        .join(",");
    format!("{func_name}({rendered})")
}

fn aggregate_expr_signature(func_name: &str, args: &[Expr]) -> Option<String> {
    let rendered = args
        .iter()
        .map(render_expr_signature)
        .collect::<Option<Vec<_>>>()?
        .join(",");
    Some(format!("{func_name}({rendered})"))
}

fn render_projection_signature(projection: &Projection) -> String {
    match projection {
        Projection::All => "*".to_string(),
        Projection::Column(column) => column
            .strip_prefix("LIT:")
            .map(str::to_string)
            .unwrap_or_else(|| column.clone()),
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| field_ref_name(field)),
        Projection::Function(name, args) => format!(
            "{}({})",
            base_function_name(name),
            args.iter()
                .map(render_projection_signature)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Projection::Expression(filter, alias) => alias
            .clone()
            .unwrap_or_else(|| render_filter_signature(filter)),
    }
}

fn render_filter_signature(filter: &Filter) -> String {
    match filter {
        Filter::Compare { field, op, value } => format!(
            "({}{}{})",
            field_ref_name(field),
            op,
            render_value_signature(value)
        ),
        Filter::CompareFields { left, op, right } => {
            format!("({}{}{})", field_ref_name(left), op, field_ref_name(right))
        }
        Filter::CompareExpr { lhs, op, rhs } => format!(
            "({}{}{})",
            render_expr_signature(lhs).unwrap_or_else(|| "expr".to_string()),
            op,
            render_expr_signature(rhs).unwrap_or_else(|| "expr".to_string())
        ),
        Filter::And(left, right) => format!(
            "({}AND{})",
            render_filter_signature(left),
            render_filter_signature(right)
        ),
        Filter::Or(left, right) => format!(
            "({}OR{})",
            render_filter_signature(left),
            render_filter_signature(right)
        ),
        Filter::Not(inner) => format!("(NOT{})", render_filter_signature(inner)),
        Filter::IsNull(field) => format!("({}ISNULL)", field_ref_name(field)),
        Filter::IsNotNull(field) => format!("({}ISNOTNULL)", field_ref_name(field)),
        Filter::In { field, values } => format!(
            "{}IN({})",
            field_ref_name(field),
            values
                .iter()
                .map(render_value_signature)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Filter::Between { field, low, high } => format!(
            "{}BETWEEN({},{})",
            field_ref_name(field),
            render_value_signature(low),
            render_value_signature(high)
        ),
        Filter::Like { field, pattern } => format!("{}LIKE({pattern})", field_ref_name(field)),
        Filter::StartsWith { field, prefix } => {
            format!("{}STARTSWITH({prefix})", field_ref_name(field))
        }
        Filter::EndsWith { field, suffix } => {
            format!("{}ENDSWITH({suffix})", field_ref_name(field))
        }
        Filter::Contains { field, substring } => {
            format!("{}CONTAINS({substring})", field_ref_name(field))
        }
    }
}

fn render_expr_signature(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Literal { value, .. } => Some(render_value_signature(value)),
        Expr::Column { field, .. } => Some(field_ref_name(field)),
        Expr::Parameter { index, .. } => Some(format!("${index}")),
        Expr::BinaryOp { op, lhs, rhs, .. } => match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Concat => {
                Some(format!(
                    "{}({},{})",
                    render_binop_signature_name(*op),
                    render_expr_signature(lhs)?,
                    render_expr_signature(rhs)?
                ))
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Some(format!(
                "({}{}{})",
                render_expr_signature(lhs)?,
                render_binop_compare_symbol(*op),
                render_expr_signature(rhs)?
            )),
            BinOp::And => Some(format!(
                "({}AND{})",
                render_expr_signature(lhs)?,
                render_expr_signature(rhs)?
            )),
            BinOp::Or => Some(format!(
                "({}OR{})",
                render_expr_signature(lhs)?,
                render_expr_signature(rhs)?
            )),
        },
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Neg => Some(format!("NEG({})", render_expr_signature(operand)?)),
            UnaryOp::Not => Some(format!("NOT({})", render_expr_signature(operand)?)),
        },
        Expr::Cast { inner, target, .. } => Some(format!(
            "CAST({},TYPE:{target})",
            render_expr_signature(inner)?
        )),
        Expr::FunctionCall { name, args, .. } => Some(format!(
            "{}({})",
            name.to_uppercase(),
            args.iter()
                .map(render_expr_signature)
                .collect::<Option<Vec<_>>>()?
                .join(",")
        )),
        Expr::Case {
            branches, else_, ..
        } => {
            let mut parts = Vec::with_capacity(branches.len() * 2 + usize::from(else_.is_some()));
            for (cond, value) in branches {
                parts.push(render_expr_signature(cond)?);
                parts.push(render_expr_signature(value)?);
            }
            if let Some(else_expr) = else_ {
                parts.push(render_expr_signature(else_expr)?);
            }
            Some(format!("CASE({})", parts.join(",")))
        }
        Expr::IsNull {
            operand, negated, ..
        } => Some(format!(
            "{}({})",
            if *negated { "IS_NOT_NULL" } else { "IS_NULL" },
            render_expr_signature(operand)?
        )),
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => Some(format!(
            "{}({},{})",
            if *negated { "NOT_IN" } else { "IN" },
            render_expr_signature(target)?,
            values
                .iter()
                .map(render_expr_signature)
                .collect::<Option<Vec<_>>>()?
                .join(",")
        )),
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => Some(format!(
            "{}({},{},{})",
            if *negated { "NOT_BETWEEN" } else { "BETWEEN" },
            render_expr_signature(target)?,
            render_expr_signature(low)?,
            render_expr_signature(high)?
        )),
    }
}

fn render_binop_signature_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "ADD",
        BinOp::Sub => "SUB",
        BinOp::Mul => "MUL",
        BinOp::Div => "DIV",
        BinOp::Mod => "MOD",
        BinOp::Concat => "CONCAT",
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => "CMP",
        BinOp::And => "AND",
        BinOp::Or => "OR",
    }
}

fn render_binop_compare_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Eq => "=",
        BinOp::Ne => "<>",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        _ => "?",
    }
}

fn render_value_signature(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::UnsignedInteger(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::BigInt(value) => value.to_string(),
        Value::Decimal(value) => value.to_string(),
        Value::Text(value) => value.to_string(),
        other => other.display_string(),
    }
}

fn projection_from_expr(expr: &Expr) -> Option<Projection> {
    lower_expr_to_projection(expr)
}

fn aggregate_projection_result_slotted(
    projection: &Projection,
    state: &SlottedAggState,
    plan: &CompiledAggPlan,
) -> Option<(String, Value)> {
    let Projection::Function(func, args) = projection else {
        return None;
    };
    let func_name = base_function_name(func);
    if !is_aggregate_function(func_name) {
        return None;
    }

    let col_name = aggregate_argument_key(args)?;
    let result_name = aggregate_output_name(projection, func_name, &col_name);

    let result_value = match func_name {
        "COUNT" => {
            if col_name == "*" {
                Value::Integer(state.count as i64)
            } else {
                let idx = plan.slot_for(AggStorageGroup::Count, &col_name)?;
                Value::Integer(state.count_only[idx] as i64)
            }
        }
        "SUM" => {
            let idx = plan.slot_for(AggStorageGroup::SumCount, &col_name)?;
            if state.sum_agg_counts[idx] == 0 {
                Value::Null
            } else {
                Value::Float(state.sums[idx])
            }
        }
        "AVG" => {
            let idx = plan.slot_for(AggStorageGroup::SumCount, &col_name)?;
            let count = state.sum_agg_counts[idx];
            if count > 0 {
                Value::Float(state.sums[idx] / count as f64)
            } else {
                Value::Null
            }
        }
        "MIN" => {
            let idx = plan.slot_for(AggStorageGroup::Min, &col_name)?;
            state.mins[idx].clone().unwrap_or(Value::Null)
        }
        "MAX" => {
            let idx = plan.slot_for(AggStorageGroup::Max, &col_name)?;
            state.maxs[idx].clone().unwrap_or(Value::Null)
        }
        "VARIANCE" => {
            let idx = plan.slot_for(AggStorageGroup::SumCount, &col_name)?;
            let n = state.sum_agg_counts[idx] as f64;
            if n > 0.0 {
                let sum = state.sums[idx];
                let sum_sq = state.sum_squares[idx];
                Value::Float(sum_sq / n - (sum / n).powi(2))
            } else {
                Value::Null
            }
        }
        "STDDEV" => {
            let idx = plan.slot_for(AggStorageGroup::SumCount, &col_name)?;
            let n = state.sum_agg_counts[idx] as f64;
            if n > 0.0 {
                let sum = state.sums[idx];
                let sum_sq = state.sum_squares[idx];
                let variance = sum_sq / n - (sum / n).powi(2);
                Value::Float(variance.max(0.0).sqrt())
            } else {
                Value::Null
            }
        }
        "MEDIAN" => {
            let idx = plan.slot_for(AggStorageGroup::AllValues, &col_name)?;
            let mut values = state.all_values[idx].clone();
            if values.is_empty() {
                Value::Null
            } else {
                values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mid = values.len() / 2;
                if values.len() % 2 == 0 {
                    Value::Float((values[mid - 1] + values[mid]) / 2.0)
                } else {
                    Value::Float(values[mid])
                }
            }
        }
        "PERCENTILE" => {
            let pct = resolve_static_projection_number(args.get(1))
                .unwrap_or(0.5)
                .clamp(0.0, 1.0);
            let idx = plan.slot_for(AggStorageGroup::AllValues, &col_name)?;
            let mut values = state.all_values[idx].clone();
            if values.is_empty() {
                Value::Null
            } else {
                values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let index =
                    ((pct * (values.len() as f64 - 1.0)).round() as usize).min(values.len() - 1);
                Value::Float(values[index])
            }
        }
        "GROUP_CONCAT" | "STRING_AGG" => {
            let idx = plan.slot_for(AggStorageGroup::Concat, &col_name)?;
            let values = &state.concat_values[idx];
            if values.is_empty() {
                Value::Null
            } else {
                let separator =
                    resolve_static_projection_text(args.get(1)).unwrap_or_else(|| ", ".to_string());
                Value::text(values.join(separator.as_str()))
            }
        }
        "FIRST" => {
            let idx = plan.slot_for(AggStorageGroup::First, &col_name)?;
            state.first_values[idx].clone().unwrap_or(Value::Null)
        }
        "LAST" => {
            let idx = plan.slot_for(AggStorageGroup::Last, &col_name)?;
            state.last_values[idx].clone().unwrap_or(Value::Null)
        }
        "ARRAY_AGG" => {
            let idx = plan.slot_for(AggStorageGroup::Array, &col_name)?;
            let values = state.array_values[idx].clone();
            if values.is_empty() {
                Value::Null
            } else {
                Value::Array(values)
            }
        }
        "COUNT_DISTINCT" => {
            let idx = plan.slot_for(AggStorageGroup::Distinct, &col_name)?;
            Value::Integer(
                state.distinct_sets[idx]
                    .as_ref()
                    .map(|s| s.len())
                    .unwrap_or(0) as i64,
            )
        }
        _ => Value::Null,
    };

    Some((result_name, result_value))
}

fn empty_aggregate_projection_result_slotted(
    projection: &Projection,
    state: &SlottedAggState,
    plan: &CompiledAggPlan,
) -> Option<(String, Value)> {
    aggregate_projection_result_slotted(projection, state, plan)
}

fn aggregate_argument_key(args: &[Projection]) -> Option<String> {
    args.first().map(render_aggregate_argument_key)
}

fn having_matches(
    db: &RedDB,
    having: Option<&crate::storage::query::ast::Filter>,
    record: &UnifiedRecord,
) -> bool {
    match having {
        Some(filter) => evaluate_runtime_filter_with_db(Some(db), record, filter, None, None),
        None => true,
    }
}

fn resolve_aggregate_argument_value(
    db: &RedDB,
    arg: Option<&Projection>,
    record: &UnifiedRecord,
) -> Option<Value> {
    match arg {
        Some(Projection::All) => None,
        Some(arg) => eval_projection_value_with_db(Some(db), arg, record),
        _ => None,
    }
}

fn aggregate_output_name(projection: &Projection, func_name: &str, column_name: &str) -> String {
    if let Projection::Function(name, _) = projection {
        if let Some((_, alias)) = name.split_once(':') {
            return alias.to_string();
        }
    }

    if column_name == "*" {
        format!("{}(*)", func_name.to_lowercase())
    } else {
        format!("{}({})", func_name.to_lowercase(), column_name)
    }
}

fn validate_aggregate_projection_shape(query: &TableQuery) -> RedDBResult<()> {
    let effective_projections = effective_table_projections(query);
    let effective_group_by = effective_table_group_by_exprs(query);
    let has_group_by = !effective_group_by.is_empty();

    for projection in &effective_projections {
        if matches!(
            projection,
            Projection::Function(name, _)
                if is_aggregate_function(base_function_name(name))
        ) {
            continue;
        }

        if has_group_by
            && projection_group_key(projection).is_some_and(|group_key| {
                effective_group_by
                    .iter()
                    .filter_map(group_expr_key)
                    .any(|entry| entry.eq_ignore_ascii_case(&group_key))
            })
        {
            continue;
        }

        let label = projection_name(projection);
        let message = if has_group_by {
            format!("projection `{label}` must appear in GROUP BY or be an aggregate")
        } else {
            format!(
                "projection `{label}` must be an aggregate because the query contains aggregate functions"
            )
        };
        return Err(RedDBError::Query(message));
    }

    Ok(())
}

fn render_aggregate_argument_key(arg: &Projection) -> String {
    match arg {
        Projection::Column(column) => column
            .strip_prefix("LIT:")
            .map(str::to_string)
            .unwrap_or_else(|| column.clone()),
        Projection::All => "*".to_string(),
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| field_ref_name(field)),
        Projection::Function(name, args) => {
            let rendered = args
                .iter()
                .map(render_aggregate_argument_key)
                .collect::<Vec<_>>()
                .join(",");
            format!("{}({rendered})", base_function_name(name))
        }
        Projection::Expression(_, alias) => alias.clone().unwrap_or_else(|| "expr".to_string()),
    }
}

fn resolve_static_projection_number(arg: Option<&Projection>) -> Option<f64> {
    let record = UnifiedRecord::new();
    let value = eval_projection_value_with_db(None, arg?, &record)?;
    value_to_f64(&value)
}

fn resolve_static_projection_text(arg: Option<&Projection>) -> Option<String> {
    let record = UnifiedRecord::new();
    let value = eval_projection_value_with_db(None, arg?, &record)?;
    Some(match value {
        Value::Null => String::new(),
        Value::Text(text) => text.to_string(),
        other => other.display_string(),
    })
}

fn group_output_label(query: &TableQuery, group_expr: &Expr) -> String {
    effective_table_projections(query)
        .iter()
        .find_map(|projection| {
            let key = projection_group_key(projection)?;
            if group_expr_key(group_expr)
                .is_some_and(|group_key| key.eq_ignore_ascii_case(&group_key))
            {
                Some(projection_name(projection))
            } else {
                None
            }
        })
        .unwrap_or_else(|| group_expr_key(group_expr).unwrap_or_else(|| "group".to_string()))
}

fn projection_group_key(projection: &Projection) -> Option<String> {
    match projection {
        Projection::Column(column) => Some(column.clone()),
        Projection::Field(FieldRef::TableColumn { table, column }, _) if table.is_empty() => {
            Some(column.clone())
        }
        Projection::Function(name, args) if base_function_name(name) == "TIME_BUCKET" => {
            render_time_bucket_group_expr(args)
        }
        _ => None,
    }
}

fn render_time_bucket_group_expr(args: &[Projection]) -> Option<String> {
    let rendered = args
        .iter()
        .map(render_group_by_argument)
        .collect::<Option<Vec<_>>>()?;
    Some(format!("TIME_BUCKET({})", rendered.join(",")))
}

fn render_group_by_argument(arg: &Projection) -> Option<String> {
    match arg {
        Projection::Column(column) => Some(
            column
                .strip_prefix("LIT:")
                .map(str::to_string)
                .unwrap_or_else(|| column.clone()),
        ),
        Projection::All => Some("*".to_string()),
        _ => None,
    }
}

fn resolve_group_by_value(db: &RedDB, group_expr: &Expr, record: &UnifiedRecord) -> Option<Value> {
    if let Some((bucket_ns, timestamp_column)) =
        parse_time_bucket_group_expr(&group_expr_key(group_expr).unwrap_or_default())
    {
        let timestamp_ns = resolve_bucket_timestamp_ns(record, timestamp_column.as_deref())?;
        let bucket_start = if bucket_ns == 0 {
            timestamp_ns
        } else {
            (timestamp_ns / bucket_ns) * bucket_ns
        };
        return Some(Value::UnsignedInteger(bucket_start));
    }

    match group_expr {
        Expr::Column { field, .. } => record.get(&field_ref_name(field)).cloned(),
        _ => {
            let projection = projection_from_expr(group_expr)?;
            eval_projection_value_with_db(Some(db), &projection, record)
        }
    }
}

fn group_expr_key(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column { field, .. } => Some(field_ref_name(field)),
        _ => render_expr_signature(expr),
    }
}

fn parse_time_bucket_group_expr(expr: &str) -> Option<(u64, Option<String>)> {
    const PREFIX: &str = "TIME_BUCKET(";

    if expr.len() <= PREFIX.len()
        || !expr[..PREFIX.len()].eq_ignore_ascii_case(PREFIX)
        || !expr.ends_with(')')
    {
        return None;
    }

    let inner = &expr[PREFIX.len()..expr.len() - 1];
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.is_empty() || parts.len() > 2 {
        return None;
    }

    let bucket_ns = crate::storage::timeseries::retention::parse_duration_ns(parts[0])?;
    let timestamp_column = parts
        .get(1)
        .filter(|value| !value.is_empty())
        .map(|value| (*value).to_string());

    Some((bucket_ns, timestamp_column))
}

fn resolve_bucket_timestamp_ns(record: &UnifiedRecord, column: Option<&str>) -> Option<u64> {
    if let Some(column) = column {
        return record.get(column).and_then(value_to_bucket_timestamp_ns);
    }

    record
        .get("timestamp_ns")
        .and_then(value_to_bucket_timestamp_ns)
        .or_else(|| {
            record
                .get("timestamp_ms")
                .and_then(value_to_bucket_timestamp_ns)
        })
        .or_else(|| {
            record
                .get("timestamp")
                .and_then(value_to_bucket_timestamp_ns)
        })
}

fn value_to_bucket_timestamp_ns(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        Value::BigInt(v) if *v >= 0 => Some(*v as u64),
        Value::Float(v) if *v >= 0.0 => Some(*v as u64),
        Value::Timestamp(v) if *v >= 0 => Some((*v as u64) * 1_000_000_000),
        Value::TimestampMs(v) if *v >= 0 => Some((*v as u64) * 1_000_000),
        _ => None,
    }
}

fn build_aggregate_group_key(values: &[Value]) -> AggregateGroupKey {
    values
        .iter()
        .map(|value| {
            value_to_canonical_key(value)
                .map(GroupKeyPart::Canonical)
                .unwrap_or_else(|| GroupKeyPart::Rendered(group_value_key(value)))
        })
        .collect()
}

#[allow(dead_code)]
fn group_value_key(value: &Value) -> String {
    use std::fmt::Write;
    let mut buf = String::with_capacity(32);
    match value {
        Value::Null => buf.push_str("null"),
        Value::Boolean(v) => {
            buf.push_str("b:");
            let _ = write!(buf, "{v}");
        }
        Value::Integer(v) => {
            buf.push_str("i:");
            let _ = write!(buf, "{v}");
        }
        Value::UnsignedInteger(v) => {
            buf.push_str("u:");
            let _ = write!(buf, "{v}");
        }
        Value::Float(v) => {
            buf.push_str("f:");
            let _ = write!(buf, "{:016x}", v.to_bits());
        }
        Value::Text(v) => {
            buf.push_str("t:");
            buf.push_str(v);
        }
        other => {
            buf.push_str("o:");
            let _ = write!(buf, "{other:?}");
        }
    }
    buf
}

type AggregateGroupKey = Vec<GroupKeyPart>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GroupKeyPart {
    Canonical(CanonicalKey),
    Rendered(String),
}

// ── Slot-indexed aggregate state ─────────────────────────────────────────────
//
// Replaces the HashMap<String, T> fields in the old AggState with Vec<T>
// indexed by pre-assigned compile-time slot indices. The "plan" is compiled
// once from `all_aggregate_projections` before the hot loop; thereafter every
// accumulation step is a single array write — zero String allocation,
// zero hash lookup.
//
// Slot assignment is deduplicated by (storage_group, col_name): SUM(age) and
// AVG(age) share the same SumCount slot; MIN(price) and MAX(price) get
// separate Min and Max slots for the same column.

/// Which backing Vec within `SlottedAggState` stores a given function's data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
enum AggStorageGroup {
    SumCount = 0, // sums + sum_agg_counts (+sum_squares for STDDEV/VARIANCE)
    Count = 1,    // count_only (COUNT(col))
    Min = 2,
    Max = 3,
    AllValues = 4, // MEDIAN, PERCENTILE
    Concat = 5,    // GROUP_CONCAT, STRING_AGG
    First = 6,
    Last = 7,
    Array = 8,    // ARRAY_AGG
    Distinct = 9, // COUNT_DISTINCT
}

fn func_storage_group(func_name: &str) -> Option<AggStorageGroup> {
    match func_name {
        "SUM" | "AVG" | "STDDEV" | "VARIANCE" => Some(AggStorageGroup::SumCount),
        "COUNT" => Some(AggStorageGroup::Count),
        "MIN" => Some(AggStorageGroup::Min),
        "MAX" => Some(AggStorageGroup::Max),
        "MEDIAN" | "PERCENTILE" => Some(AggStorageGroup::AllValues),
        "GROUP_CONCAT" | "STRING_AGG" => Some(AggStorageGroup::Concat),
        "FIRST" => Some(AggStorageGroup::First),
        "LAST" => Some(AggStorageGroup::Last),
        "ARRAY_AGG" => Some(AggStorageGroup::Array),
        "COUNT_DISTINCT" => Some(AggStorageGroup::Distinct),
        _ => None,
    }
}

/// Per-projection slot reference: tells the hot loop exactly which Vec index
/// to write for each aggregate projection.
#[derive(Debug, Clone, Copy)]
enum ProjSlot {
    /// COUNT(*) — just increment the global `state.count`.
    CountStar,
    /// sums[idx] + sum_agg_counts[idx] — SUM, AVG.
    SumCount(usize),
    /// SumCount + sum_squares[idx] — STDDEV, VARIANCE.
    SumCountSq(usize),
    /// count_only[idx] — COUNT(col).
    CountOnly(usize),
    Min(usize),
    Max(usize),
    AllValues(usize),
    Concat(usize),
    First(usize),
    Last(usize),
    Array(usize),
    Distinct(usize),
}

/// Compiled per-query aggregation plan: slot assignments for all projections.
struct CompiledAggPlan {
    /// One slot per entry in `all_aggregate_projections`.
    proj_slots: Vec<ProjSlot>,
    /// Vec sizes for `SlottedAggState` allocation.
    n_sum_count: usize,
    n_count: usize,
    n_min: usize,
    n_max: usize,
    n_all_values: usize,
    n_concat: usize,
    n_first: usize,
    n_last: usize,
    n_array: usize,
    n_distinct: usize,
    /// Reverse lookup for result building: (group, col_name) → slot_idx.
    result_slot_map: std::collections::HashMap<(AggStorageGroup, String), usize>,
}

impl CompiledAggPlan {
    fn compile(projections: &[Projection]) -> Self {
        use std::collections::HashMap;
        let mut slot_key_to_idx: HashMap<(AggStorageGroup, String), usize> = HashMap::new();
        let mut counters = [0usize; 10];
        let mut proj_slots = Vec::with_capacity(projections.len());
        // Tracks whether each SumCount slot needs sum_squares.
        let mut sum_count_needs_sq: Vec<bool> = Vec::new();

        for proj in projections {
            let Projection::Function(func, args) = proj else {
                proj_slots.push(ProjSlot::CountStar);
                continue;
            };
            let func_name = base_function_name(func);
            let col_name = aggregate_argument_key(args).unwrap_or_default();

            if func_name == "COUNT" && col_name == "*" {
                proj_slots.push(ProjSlot::CountStar);
                continue;
            }

            let Some(group) = func_storage_group(func_name) else {
                proj_slots.push(ProjSlot::CountStar);
                continue;
            };

            let key = (group, col_name);
            let idx = *slot_key_to_idx.entry(key).or_insert_with(|| {
                let i = counters[group as usize];
                counters[group as usize] += 1;
                if group == AggStorageGroup::SumCount {
                    sum_count_needs_sq.push(false);
                }
                i
            });

            // STDDEV/VARIANCE need sum_squares for this slot.
            if group == AggStorageGroup::SumCount
                && (func_name == "STDDEV" || func_name == "VARIANCE")
                && idx < sum_count_needs_sq.len()
            {
                sum_count_needs_sq[idx] = true;
            }

            let ps = match group {
                AggStorageGroup::SumCount => {
                    if func_name == "STDDEV" || func_name == "VARIANCE" {
                        ProjSlot::SumCountSq(idx)
                    } else {
                        ProjSlot::SumCount(idx)
                    }
                }
                AggStorageGroup::Count => ProjSlot::CountOnly(idx),
                AggStorageGroup::Min => ProjSlot::Min(idx),
                AggStorageGroup::Max => ProjSlot::Max(idx),
                AggStorageGroup::AllValues => ProjSlot::AllValues(idx),
                AggStorageGroup::Concat => ProjSlot::Concat(idx),
                AggStorageGroup::First => ProjSlot::First(idx),
                AggStorageGroup::Last => ProjSlot::Last(idx),
                AggStorageGroup::Array => ProjSlot::Array(idx),
                AggStorageGroup::Distinct => ProjSlot::Distinct(idx),
            };
            proj_slots.push(ps);
        }

        CompiledAggPlan {
            proj_slots,
            n_sum_count: counters[0],
            n_count: counters[1],
            n_min: counters[2],
            n_max: counters[3],
            n_all_values: counters[4],
            n_concat: counters[5],
            n_first: counters[6],
            n_last: counters[7],
            n_array: counters[8],
            n_distinct: counters[9],
            result_slot_map: slot_key_to_idx,
        }
    }

    /// Look up the slot index for a result-building call.
    fn slot_for(&self, group: AggStorageGroup, col_name: &str) -> Option<usize> {
        self.result_slot_map
            .get(&(group, col_name.to_string()))
            .copied()
    }
}

/// Vec-indexed replacement for the old HashMap-based `AggState`.
/// Allocated once per group; hot-path writes are direct array assignments.
#[derive(Clone)]
struct SlottedAggState {
    count: u64,
    sums: Vec<f64>,
    sum_agg_counts: Vec<u64>,
    sum_squares: Vec<f64>,
    count_only: Vec<u64>,
    mins: Vec<Option<Value>>,
    maxs: Vec<Option<Value>>,
    all_values: Vec<Vec<f64>>,
    concat_values: Vec<Vec<String>>,
    first_values: Vec<Option<Value>>,
    last_values: Vec<Option<Value>>,
    array_values: Vec<Vec<Value>>,
    distinct_sets: Vec<Option<std::collections::HashSet<String>>>,
}

impl SlottedAggState {
    fn new(plan: &CompiledAggPlan) -> Self {
        Self {
            count: 0,
            sums: vec![0.0; plan.n_sum_count],
            sum_agg_counts: vec![0; plan.n_sum_count],
            sum_squares: vec![0.0; plan.n_sum_count],
            count_only: vec![0; plan.n_count],
            mins: vec![None; plan.n_min],
            maxs: vec![None; plan.n_max],
            all_values: vec![Vec::new(); plan.n_all_values],
            concat_values: vec![Vec::new(); plan.n_concat],
            first_values: vec![None; plan.n_first],
            last_values: vec![None; plan.n_last],
            array_values: vec![Vec::new(); plan.n_array],
            distinct_sets: vec![None; plan.n_distinct],
        }
    }
}

#[derive(Clone)]
struct AggregateGroup {
    group_values: Vec<Value>,
    state: SlottedAggState,
}

pub(super) fn update_extreme_value_slot(
    slot: &mut Option<Value>,
    candidate: &Value,
    ordering: std::cmp::Ordering,
) {
    if matches!(candidate, Value::Null) {
        return;
    }
    match slot {
        Some(current) => {
            if runtime_partial_cmp(candidate, current).is_some_and(|ord| ord == ordering) {
                *current = candidate.clone();
            }
        }
        None => {
            *slot = Some(candidate.clone());
        }
    }
}

fn value_to_f64(val: &Value) -> Option<f64> {
    match val {
        Value::Integer(n) => Some(*n as f64),
        Value::UnsignedInteger(n) => Some(*n as f64),
        Value::BigInt(n) => Some(*n as f64),
        Value::Float(f) => Some(*f),
        Value::Decimal(d) => Some(*d as f64 / 10_000.0),
        _ => None,
    }
}

// ── SpillCodec / Mergeable for SlottedAggState + AggregateGroup ─────────────
//
// Enables SpilledHashAgg<AggregateGroupKey, AggregateGroup> so GROUP BY
// queries that exceed work_mem spill to a tmpfs batch file rather than
// failing.  Encoding is manual little-endian (no serde dep) using the same
// style as the built-in impls in `agg_spill.rs`.
//
// SlottedAggState fields are encoded as length-prefixed Vec<T> sequences so
// that decode can reconstruct the Vec without the CompiledAggPlan.
mod agg_spill_codec {
    use std::collections::HashSet;
    use std::io::{Read, Write};

    use crate::storage::query::executors::agg_spill::{Mergeable, SpillCodec, SpillError};
    use crate::storage::schema::{CanonicalKey, CanonicalKeyFamily, Value};

    use super::{AggregateGroup, AggregateGroupKey, GroupKeyPart, SlottedAggState};

    // ── low-level helpers ────────────────────────────────────────────────────

    fn w_u64<W: Write>(w: &mut W, v: u64) -> std::io::Result<usize> {
        w.write_all(&v.to_le_bytes())?;
        Ok(8)
    }
    fn r_u64<R: Read>(r: &mut R) -> std::io::Result<u64> {
        let mut b = [0u8; 8];
        r.read_exact(&mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    fn w_f64<W: Write>(w: &mut W, v: f64) -> std::io::Result<usize> {
        w.write_all(&v.to_le_bytes())?;
        Ok(8)
    }
    fn r_f64<R: Read>(r: &mut R) -> std::io::Result<f64> {
        let mut b = [0u8; 8];
        r.read_exact(&mut b)?;
        Ok(f64::from_le_bytes(b))
    }
    fn w_u8<W: Write>(w: &mut W, v: u8) -> std::io::Result<usize> {
        w.write_all(&[v])?;
        Ok(1)
    }
    fn r_u8<R: Read>(r: &mut R) -> std::io::Result<u8> {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn w_str<W: Write>(w: &mut W, s: &str) -> std::io::Result<usize> {
        let b = s.as_bytes();
        w.write_all(&(b.len() as u32).to_le_bytes())?;
        w.write_all(b)?;
        Ok(4 + b.len())
    }
    fn r_str<R: Read>(r: &mut R) -> std::io::Result<String> {
        let mut nb = [0u8; 4];
        r.read_exact(&mut nb)?;
        let n = u32::from_le_bytes(nb) as usize;
        let mut buf = vec![0u8; n];
        r.read_exact(&mut buf)?;
        String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
    // Value codec: hot-path fast encoding for the 6 most common scalar types;
    // all other types delegate to Value::to_bytes() (length-prefixed, tag=255).
    // This preserves full type fidelity for MIN/MAX/FIRST/LAST on any column type.
    fn w_val<W: Write>(w: &mut W, v: &Value) -> std::io::Result<usize> {
        match v {
            Value::Null => {
                w.write_all(&[0u8])?;
                Ok(1)
            }
            Value::Boolean(b) => {
                w.write_all(&[1u8, *b as u8])?;
                Ok(2)
            }
            Value::Integer(n) => {
                w.write_all(&[2u8])?;
                w.write_all(&n.to_le_bytes())?;
                Ok(9)
            }
            Value::UnsignedInteger(n) => {
                w.write_all(&[3u8])?;
                w.write_all(&n.to_le_bytes())?;
                Ok(9)
            }
            Value::Float(f) => {
                w.write_all(&[4u8])?;
                w.write_all(&f.to_le_bytes())?;
                Ok(9)
            }
            Value::Text(s) => {
                w.write_all(&[5u8])?;
                Ok(1 + w_str(w, s)?)
            }
            other => {
                // Fallback: delegate to Value::to_bytes() for full type coverage.
                // Tag 255 + u32 length prefix + payload bytes.
                let payload = other.to_bytes();
                w.write_all(&[255u8])?;
                w.write_all(&(payload.len() as u32).to_le_bytes())?;
                w.write_all(&payload)?;
                Ok(1 + 4 + payload.len())
            }
        }
    }
    fn r_val<R: Read>(r: &mut R) -> std::io::Result<Value> {
        let mut tag = [0u8];
        r.read_exact(&mut tag)?;
        match tag[0] {
            0 => Ok(Value::Null),
            1 => {
                let mut b = [0u8];
                r.read_exact(&mut b)?;
                Ok(Value::Boolean(b[0] != 0))
            }
            2 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                Ok(Value::Integer(i64::from_le_bytes(b)))
            }
            3 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                Ok(Value::UnsignedInteger(u64::from_le_bytes(b)))
            }
            4 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                Ok(Value::Float(f64::from_le_bytes(b)))
            }
            5 => Ok(Value::text(r_str(r)?)),
            255 => {
                let mut nb = [0u8; 4];
                r.read_exact(&mut nb)?;
                let n = u32::from_le_bytes(nb) as usize;
                let mut buf = vec![0u8; n];
                r.read_exact(&mut buf)?;
                Value::from_bytes(&buf).map(|(v, _)| v).map_err(|e| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
                })
            }
            _ => Ok(Value::Null),
        }
    }

    fn family_to_tag(family: CanonicalKeyFamily) -> u8 {
        match family {
            CanonicalKeyFamily::Null => 0,
            CanonicalKeyFamily::Boolean => 1,
            CanonicalKeyFamily::Integer => 2,
            CanonicalKeyFamily::BigInt => 3,
            CanonicalKeyFamily::UnsignedInteger => 4,
            CanonicalKeyFamily::Float => 5,
            CanonicalKeyFamily::Text => 6,
            CanonicalKeyFamily::Blob => 7,
            CanonicalKeyFamily::Timestamp => 8,
            CanonicalKeyFamily::Duration => 9,
            CanonicalKeyFamily::IpAddr => 10,
            CanonicalKeyFamily::MacAddr => 11,
            CanonicalKeyFamily::Json => 12,
            CanonicalKeyFamily::Uuid => 13,
            CanonicalKeyFamily::NodeRef => 14,
            CanonicalKeyFamily::EdgeRef => 15,
            CanonicalKeyFamily::VectorRef => 16,
            CanonicalKeyFamily::RowRef => 17,
            CanonicalKeyFamily::Color => 18,
            CanonicalKeyFamily::Email => 19,
            CanonicalKeyFamily::Url => 20,
            CanonicalKeyFamily::Phone => 21,
            CanonicalKeyFamily::Semver => 22,
            CanonicalKeyFamily::Cidr => 23,
            CanonicalKeyFamily::Date => 24,
            CanonicalKeyFamily::Time => 25,
            CanonicalKeyFamily::Decimal => 26,
            CanonicalKeyFamily::EnumValue => 27,
            CanonicalKeyFamily::TimestampMs => 28,
            CanonicalKeyFamily::Ipv4 => 29,
            CanonicalKeyFamily::Ipv6 => 30,
            CanonicalKeyFamily::Subnet => 31,
            CanonicalKeyFamily::Port => 32,
            CanonicalKeyFamily::Latitude => 33,
            CanonicalKeyFamily::Longitude => 34,
            CanonicalKeyFamily::GeoPoint => 35,
            CanonicalKeyFamily::Country2 => 36,
            CanonicalKeyFamily::Country3 => 37,
            CanonicalKeyFamily::Lang2 => 38,
            CanonicalKeyFamily::Lang5 => 39,
            CanonicalKeyFamily::Currency => 40,
            CanonicalKeyFamily::ColorAlpha => 41,
            CanonicalKeyFamily::KeyRef => 42,
            CanonicalKeyFamily::DocRef => 43,
            CanonicalKeyFamily::TableRef => 44,
            CanonicalKeyFamily::PageRef => 45,
            CanonicalKeyFamily::Password => 46,
        }
    }

    fn tag_to_family(tag: u8) -> Result<CanonicalKeyFamily, SpillError> {
        match tag {
            0 => Ok(CanonicalKeyFamily::Null),
            1 => Ok(CanonicalKeyFamily::Boolean),
            2 => Ok(CanonicalKeyFamily::Integer),
            3 => Ok(CanonicalKeyFamily::BigInt),
            4 => Ok(CanonicalKeyFamily::UnsignedInteger),
            5 => Ok(CanonicalKeyFamily::Float),
            6 => Ok(CanonicalKeyFamily::Text),
            7 => Ok(CanonicalKeyFamily::Blob),
            8 => Ok(CanonicalKeyFamily::Timestamp),
            9 => Ok(CanonicalKeyFamily::Duration),
            10 => Ok(CanonicalKeyFamily::IpAddr),
            11 => Ok(CanonicalKeyFamily::MacAddr),
            12 => Ok(CanonicalKeyFamily::Json),
            13 => Ok(CanonicalKeyFamily::Uuid),
            14 => Ok(CanonicalKeyFamily::NodeRef),
            15 => Ok(CanonicalKeyFamily::EdgeRef),
            16 => Ok(CanonicalKeyFamily::VectorRef),
            17 => Ok(CanonicalKeyFamily::RowRef),
            18 => Ok(CanonicalKeyFamily::Color),
            19 => Ok(CanonicalKeyFamily::Email),
            20 => Ok(CanonicalKeyFamily::Url),
            21 => Ok(CanonicalKeyFamily::Phone),
            22 => Ok(CanonicalKeyFamily::Semver),
            23 => Ok(CanonicalKeyFamily::Cidr),
            24 => Ok(CanonicalKeyFamily::Date),
            25 => Ok(CanonicalKeyFamily::Time),
            26 => Ok(CanonicalKeyFamily::Decimal),
            27 => Ok(CanonicalKeyFamily::EnumValue),
            28 => Ok(CanonicalKeyFamily::TimestampMs),
            29 => Ok(CanonicalKeyFamily::Ipv4),
            30 => Ok(CanonicalKeyFamily::Ipv6),
            31 => Ok(CanonicalKeyFamily::Subnet),
            32 => Ok(CanonicalKeyFamily::Port),
            33 => Ok(CanonicalKeyFamily::Latitude),
            34 => Ok(CanonicalKeyFamily::Longitude),
            35 => Ok(CanonicalKeyFamily::GeoPoint),
            36 => Ok(CanonicalKeyFamily::Country2),
            37 => Ok(CanonicalKeyFamily::Country3),
            38 => Ok(CanonicalKeyFamily::Lang2),
            39 => Ok(CanonicalKeyFamily::Lang5),
            40 => Ok(CanonicalKeyFamily::Currency),
            41 => Ok(CanonicalKeyFamily::ColorAlpha),
            42 => Ok(CanonicalKeyFamily::KeyRef),
            43 => Ok(CanonicalKeyFamily::DocRef),
            44 => Ok(CanonicalKeyFamily::TableRef),
            45 => Ok(CanonicalKeyFamily::PageRef),
            46 => Ok(CanonicalKeyFamily::Password),
            other => Err(SpillError::Codec(format!(
                "unknown canonical key family tag {other}"
            ))),
        }
    }

    fn w_canonical_key<W: Write>(w: &mut W, key: &CanonicalKey) -> Result<usize, SpillError> {
        let mut t = 0;
        match key {
            CanonicalKey::Null => {
                t += w_u8(w, 0).map_err(SpillError::Io)?;
            }
            CanonicalKey::Boolean(value) => {
                t += w_u8(w, 1).map_err(SpillError::Io)?;
                t += w_u8(w, *value as u8).map_err(SpillError::Io)?;
            }
            CanonicalKey::Signed(family, value) => {
                t += w_u8(w, 2).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                w.write_all(&value.to_le_bytes()).map_err(SpillError::Io)?;
                t += 8;
            }
            CanonicalKey::Unsigned(family, value) => {
                t += w_u8(w, 3).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                w.write_all(&value.to_le_bytes()).map_err(SpillError::Io)?;
                t += 8;
            }
            CanonicalKey::Float(bits) => {
                t += w_u8(w, 4).map_err(SpillError::Io)?;
                w.write_all(&bits.to_le_bytes()).map_err(SpillError::Io)?;
                t += 8;
            }
            CanonicalKey::Text(family, value) => {
                t += w_u8(w, 5).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                t += w_str(w, value).map_err(SpillError::Io)?;
            }
            CanonicalKey::Bytes(family, value) => {
                t += w_u8(w, 6).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                w.write_all(&(value.len() as u32).to_le_bytes())
                    .map_err(SpillError::Io)?;
                w.write_all(value).map_err(SpillError::Io)?;
                t += 4 + value.len();
            }
            CanonicalKey::PairTextU64(family, left, right) => {
                t += w_u8(w, 7).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                t += w_str(w, left).map_err(SpillError::Io)?;
                t += w_u64(w, *right).map_err(SpillError::Io)?;
            }
            CanonicalKey::PairTextText(family, left, right) => {
                t += w_u8(w, 8).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                t += w_str(w, left).map_err(SpillError::Io)?;
                t += w_str(w, right).map_err(SpillError::Io)?;
            }
            CanonicalKey::PairU32U8(family, left, right) => {
                t += w_u8(w, 9).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                w.write_all(&left.to_le_bytes()).map_err(SpillError::Io)?;
                t += 4;
                t += w_u8(w, *right).map_err(SpillError::Io)?;
            }
            CanonicalKey::PairU32U32(family, left, right) => {
                t += w_u8(w, 10).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                w.write_all(&left.to_le_bytes()).map_err(SpillError::Io)?;
                w.write_all(&right.to_le_bytes()).map_err(SpillError::Io)?;
                t += 8;
            }
            CanonicalKey::PairI32I32(family, left, right) => {
                t += w_u8(w, 11).map_err(SpillError::Io)?;
                t += w_u8(w, family_to_tag(*family)).map_err(SpillError::Io)?;
                w.write_all(&left.to_le_bytes()).map_err(SpillError::Io)?;
                w.write_all(&right.to_le_bytes()).map_err(SpillError::Io)?;
                t += 8;
            }
        }
        Ok(t)
    }

    fn r_canonical_key<R: Read>(r: &mut R) -> Result<CanonicalKey, SpillError> {
        let tag = r_u8(r).map_err(SpillError::Io)?;
        match tag {
            0 => Ok(CanonicalKey::Null),
            1 => Ok(CanonicalKey::Boolean(r_u8(r).map_err(SpillError::Io)? != 0)),
            2 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let mut b = [0u8; 8];
                r.read_exact(&mut b).map_err(SpillError::Io)?;
                Ok(CanonicalKey::Signed(family, i64::from_le_bytes(b)))
            }
            3 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let mut b = [0u8; 8];
                r.read_exact(&mut b).map_err(SpillError::Io)?;
                Ok(CanonicalKey::Unsigned(family, u64::from_le_bytes(b)))
            }
            4 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b).map_err(SpillError::Io)?;
                Ok(CanonicalKey::Float(u64::from_le_bytes(b)))
            }
            5 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let s = r_str(r).map_err(SpillError::Io)?;
                Ok(CanonicalKey::Text(family, std::sync::Arc::from(s.as_str())))
            }
            6 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let mut len = [0u8; 4];
                r.read_exact(&mut len).map_err(SpillError::Io)?;
                let len = u32::from_le_bytes(len) as usize;
                let mut bytes = vec![0u8; len];
                r.read_exact(&mut bytes).map_err(SpillError::Io)?;
                Ok(CanonicalKey::Bytes(family, bytes))
            }
            7 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let left = r_str(r).map_err(SpillError::Io)?;
                let right = r_u64(r).map_err(SpillError::Io)?;
                Ok(CanonicalKey::PairTextU64(family, left, right))
            }
            8 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let left = r_str(r).map_err(SpillError::Io)?;
                let right = r_str(r).map_err(SpillError::Io)?;
                Ok(CanonicalKey::PairTextText(family, left, right))
            }
            9 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let mut left = [0u8; 4];
                r.read_exact(&mut left).map_err(SpillError::Io)?;
                let right = r_u8(r).map_err(SpillError::Io)?;
                Ok(CanonicalKey::PairU32U8(
                    family,
                    u32::from_le_bytes(left),
                    right,
                ))
            }
            10 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let mut left = [0u8; 4];
                let mut right = [0u8; 4];
                r.read_exact(&mut left).map_err(SpillError::Io)?;
                r.read_exact(&mut right).map_err(SpillError::Io)?;
                Ok(CanonicalKey::PairU32U32(
                    family,
                    u32::from_le_bytes(left),
                    u32::from_le_bytes(right),
                ))
            }
            11 => {
                let family = tag_to_family(r_u8(r).map_err(SpillError::Io)?)?;
                let mut left = [0u8; 4];
                let mut right = [0u8; 4];
                r.read_exact(&mut left).map_err(SpillError::Io)?;
                r.read_exact(&mut right).map_err(SpillError::Io)?;
                Ok(CanonicalKey::PairI32I32(
                    family,
                    i32::from_le_bytes(left),
                    i32::from_le_bytes(right),
                ))
            }
            other => Err(SpillError::Codec(format!(
                "unknown canonical key tag {other}"
            ))),
        }
    }

    // ── compound helpers: Vec<T> ─────────────────────────────────────────────

    fn w_vec_f64<W: Write>(w: &mut W, v: &[f64]) -> std::io::Result<usize> {
        w.write_all(&(v.len() as u32).to_le_bytes())?;
        let mut t = 4;
        for &f in v {
            t += w_f64(w, f)?;
        }
        Ok(t)
    }
    fn r_vec_f64<R: Read>(r: &mut R) -> std::io::Result<Vec<f64>> {
        let n = r_len(r)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(r_f64(r)?);
        }
        Ok(v)
    }
    fn w_vec_u64<W: Write>(w: &mut W, v: &[u64]) -> std::io::Result<usize> {
        w.write_all(&(v.len() as u32).to_le_bytes())?;
        let mut t = 4;
        for &n in v {
            t += w_u64(w, n)?;
        }
        Ok(t)
    }
    fn r_vec_u64<R: Read>(r: &mut R) -> std::io::Result<Vec<u64>> {
        let n = r_len(r)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(r_u64(r)?);
        }
        Ok(v)
    }
    fn w_vec_option_val<W: Write>(w: &mut W, v: &[Option<Value>]) -> std::io::Result<usize> {
        w.write_all(&(v.len() as u32).to_le_bytes())?;
        let mut t = 4;
        for opt in v {
            match opt {
                None => {
                    w.write_all(&[0u8])?;
                    t += 1;
                }
                Some(val) => {
                    w.write_all(&[1u8])?;
                    t += 1 + w_val(w, val)?;
                }
            }
        }
        Ok(t)
    }
    fn r_vec_option_val<R: Read>(r: &mut R) -> std::io::Result<Vec<Option<Value>>> {
        let n = r_len(r)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let tag = r_u8(r)?;
            v.push(if tag == 0 { None } else { Some(r_val(r)?) });
        }
        Ok(v)
    }
    fn w_vec_val<W: Write>(w: &mut W, v: &[Value]) -> std::io::Result<usize> {
        w.write_all(&(v.len() as u32).to_le_bytes())?;
        let mut t = 4;
        for val in v {
            t += w_val(w, val)?;
        }
        Ok(t)
    }
    fn r_vec_val<R: Read>(r: &mut R) -> std::io::Result<Vec<Value>> {
        let n = r_len(r)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(r_val(r)?);
        }
        Ok(v)
    }
    fn w_vec_vec_f64<W: Write>(w: &mut W, v: &[Vec<f64>]) -> std::io::Result<usize> {
        w.write_all(&(v.len() as u32).to_le_bytes())?;
        let mut t = 4;
        for inner in v {
            t += w_vec_f64(w, inner)?;
        }
        Ok(t)
    }
    fn r_vec_vec_f64<R: Read>(r: &mut R) -> std::io::Result<Vec<Vec<f64>>> {
        let n = r_len(r)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(r_vec_f64(r)?);
        }
        Ok(v)
    }
    fn w_vec_vec_str<W: Write>(w: &mut W, v: &[Vec<String>]) -> std::io::Result<usize> {
        w.write_all(&(v.len() as u32).to_le_bytes())?;
        let mut t = 4;
        for inner in v {
            w.write_all(&(inner.len() as u32).to_le_bytes())?;
            t += 4;
            for s in inner {
                t += w_str(w, s)?;
            }
        }
        Ok(t)
    }
    fn r_vec_vec_str<R: Read>(r: &mut R) -> std::io::Result<Vec<Vec<String>>> {
        let n = r_len(r)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let m = r_len(r)?;
            let mut inner = Vec::with_capacity(m);
            for _ in 0..m {
                inner.push(r_str(r)?);
            }
            v.push(inner);
        }
        Ok(v)
    }
    fn w_vec_vec_val<W: Write>(w: &mut W, v: &[Vec<Value>]) -> std::io::Result<usize> {
        w.write_all(&(v.len() as u32).to_le_bytes())?;
        let mut t = 4;
        for inner in v {
            t += w_vec_val(w, inner)?;
        }
        Ok(t)
    }
    fn r_vec_vec_val<R: Read>(r: &mut R) -> std::io::Result<Vec<Vec<Value>>> {
        let n = r_len(r)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(r_vec_val(r)?);
        }
        Ok(v)
    }
    fn w_vec_option_set_str<W: Write>(
        w: &mut W,
        v: &[Option<HashSet<String>>],
    ) -> std::io::Result<usize> {
        w.write_all(&(v.len() as u32).to_le_bytes())?;
        let mut t = 4;
        for opt in v {
            match opt {
                None => {
                    w.write_all(&[0u8])?;
                    t += 1;
                }
                Some(set) => {
                    w.write_all(&[1u8])?;
                    w.write_all(&(set.len() as u32).to_le_bytes())?;
                    t += 5;
                    for s in set {
                        t += w_str(w, s)?;
                    }
                }
            }
        }
        Ok(t)
    }
    fn r_vec_option_set_str<R: Read>(r: &mut R) -> std::io::Result<Vec<Option<HashSet<String>>>> {
        let n = r_len(r)?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            let tag = r_u8(r)?;
            if tag == 0 {
                v.push(None);
            } else {
                let m = r_len(r)?;
                let mut set = HashSet::with_capacity(m);
                for _ in 0..m {
                    set.insert(r_str(r)?);
                }
                v.push(Some(set));
            }
        }
        Ok(v)
    }
    fn r_len<R: Read>(r: &mut R) -> std::io::Result<usize> {
        let mut nb = [0u8; 4];
        r.read_exact(&mut nb)?;
        Ok(u32::from_le_bytes(nb) as usize)
    }

    // ── SpillCodec ───────────────────────────────────────────────────────────

    impl SpillCodec for GroupKeyPart {
        fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
            match self {
                GroupKeyPart::Canonical(key) => {
                    let mut t = w_u8(w, 0).map_err(SpillError::Io)?;
                    t += w_canonical_key(w, key)?;
                    Ok(t)
                }
                GroupKeyPart::Rendered(value) => {
                    let mut t = w_u8(w, 1).map_err(SpillError::Io)?;
                    t += w_str(w, value).map_err(SpillError::Io)?;
                    Ok(t)
                }
            }
        }

        fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
            let tag = match r_u8(r) {
                Ok(tag) => tag,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(SpillError::Io(e)),
            };
            match tag {
                0 => Ok(Some(GroupKeyPart::Canonical(r_canonical_key(r)?))),
                1 => Ok(Some(GroupKeyPart::Rendered(
                    r_str(r).map_err(SpillError::Io)?,
                ))),
                other => Err(SpillError::Codec(format!(
                    "unknown group key part tag {other}"
                ))),
            }
        }
    }

    impl SpillCodec for AggregateGroupKey {
        fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
            w.write_all(&(self.len() as u32).to_le_bytes())
                .map_err(SpillError::Io)?;
            let mut t = 4;
            for part in self {
                t += part.encode(w)?;
            }
            Ok(t)
        }

        fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
            let mut nb = [0u8; 4];
            match r.read_exact(&mut nb) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(SpillError::Io(e)),
            }
            let n = u32::from_le_bytes(nb) as usize;
            let mut parts = Vec::with_capacity(n);
            for _ in 0..n {
                let part = GroupKeyPart::decode(r)?
                    .ok_or_else(|| SpillError::Codec("truncated group key".to_string()))?;
                parts.push(part);
            }
            Ok(Some(parts))
        }
    }

    impl SpillCodec for AggregateGroup {
        fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
            let mut t = 0;
            t += w_vec_val(w, &self.group_values).map_err(SpillError::Io)?;
            let s = &self.state;
            t += w_u64(w, s.count).map_err(SpillError::Io)?;
            t += w_vec_f64(w, &s.sums).map_err(SpillError::Io)?;
            t += w_vec_u64(w, &s.sum_agg_counts).map_err(SpillError::Io)?;
            t += w_vec_f64(w, &s.sum_squares).map_err(SpillError::Io)?;
            t += w_vec_u64(w, &s.count_only).map_err(SpillError::Io)?;
            t += w_vec_option_val(w, &s.mins).map_err(SpillError::Io)?;
            t += w_vec_option_val(w, &s.maxs).map_err(SpillError::Io)?;
            t += w_vec_vec_f64(w, &s.all_values).map_err(SpillError::Io)?;
            t += w_vec_vec_str(w, &s.concat_values).map_err(SpillError::Io)?;
            t += w_vec_option_val(w, &s.first_values).map_err(SpillError::Io)?;
            t += w_vec_option_val(w, &s.last_values).map_err(SpillError::Io)?;
            t += w_vec_vec_val(w, &s.array_values).map_err(SpillError::Io)?;
            t += w_vec_option_set_str(w, &s.distinct_sets).map_err(SpillError::Io)?;
            Ok(t)
        }

        fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
            // Detect clean EOF on the first field's length prefix.
            let mut nb = [0u8; 4];
            match r.read_exact(&mut nb) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(e) => return Err(SpillError::Io(e)),
            }
            let gv_n = u32::from_le_bytes(nb) as usize;
            let mut group_values = Vec::with_capacity(gv_n);
            for _ in 0..gv_n {
                group_values.push(r_val(r).map_err(SpillError::Io)?);
            }
            Ok(Some(AggregateGroup {
                group_values,
                state: SlottedAggState {
                    count: r_u64(r).map_err(SpillError::Io)?,
                    sums: r_vec_f64(r).map_err(SpillError::Io)?,
                    sum_agg_counts: r_vec_u64(r).map_err(SpillError::Io)?,
                    sum_squares: r_vec_f64(r).map_err(SpillError::Io)?,
                    count_only: r_vec_u64(r).map_err(SpillError::Io)?,
                    mins: r_vec_option_val(r).map_err(SpillError::Io)?,
                    maxs: r_vec_option_val(r).map_err(SpillError::Io)?,
                    all_values: r_vec_vec_f64(r).map_err(SpillError::Io)?,
                    concat_values: r_vec_vec_str(r).map_err(SpillError::Io)?,
                    first_values: r_vec_option_val(r).map_err(SpillError::Io)?,
                    last_values: r_vec_option_val(r).map_err(SpillError::Io)?,
                    array_values: r_vec_vec_val(r).map_err(SpillError::Io)?,
                    distinct_sets: r_vec_option_set_str(r).map_err(SpillError::Io)?,
                },
            }))
        }
    }

    // ── Mergeable ────────────────────────────────────────────────────────────

    impl Mergeable for AggregateGroup {
        fn merge(&mut self, other: Self) {
            // group_values identical (same GROUP BY key) — keep self's copy.
            let s = &mut self.state;
            let o = other.state;
            s.count += o.count;
            for (i, v) in o.sums.into_iter().enumerate() {
                if i < s.sums.len() {
                    s.sums[i] += v;
                }
            }
            for (i, v) in o.sum_agg_counts.into_iter().enumerate() {
                if i < s.sum_agg_counts.len() {
                    s.sum_agg_counts[i] += v;
                }
            }
            for (i, v) in o.sum_squares.into_iter().enumerate() {
                if i < s.sum_squares.len() {
                    s.sum_squares[i] += v;
                }
            }
            for (i, v) in o.count_only.into_iter().enumerate() {
                if i < s.count_only.len() {
                    s.count_only[i] += v;
                }
            }
            for (i, candidate) in o.mins.into_iter().enumerate() {
                if i < s.mins.len() {
                    if let Some(c) = candidate {
                        super::update_extreme_value_slot(
                            &mut s.mins[i],
                            &c,
                            std::cmp::Ordering::Less,
                        );
                    }
                }
            }
            for (i, candidate) in o.maxs.into_iter().enumerate() {
                if i < s.maxs.len() {
                    if let Some(c) = candidate {
                        super::update_extreme_value_slot(
                            &mut s.maxs[i],
                            &c,
                            std::cmp::Ordering::Greater,
                        );
                    }
                }
            }
            for (i, v) in o.all_values.into_iter().enumerate() {
                if i < s.all_values.len() {
                    s.all_values[i].extend(v);
                }
            }
            for (i, v) in o.concat_values.into_iter().enumerate() {
                if i < s.concat_values.len() {
                    s.concat_values[i].extend(v);
                }
            }
            // FIRST: keep self (first batch wins)
            for (i, v) in o.first_values.into_iter().enumerate() {
                if i < s.first_values.len() && s.first_values[i].is_none() {
                    s.first_values[i] = v;
                }
            }
            // LAST: overwrite with other (later batch)
            for (i, v) in o.last_values.into_iter().enumerate() {
                if i < s.last_values.len() && v.is_some() {
                    s.last_values[i] = v;
                }
            }
            for (i, v) in o.array_values.into_iter().enumerate() {
                if i < s.array_values.len() {
                    s.array_values[i].extend(v);
                }
            }
            for (i, set_opt) in o.distinct_sets.into_iter().enumerate() {
                if i < s.distinct_sets.len() {
                    if let Some(set) = set_opt {
                        s.distinct_sets[i]
                            .get_or_insert_with(std::collections::HashSet::new)
                            .extend(set);
                    }
                }
            }
        }
    }
}
