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

use reddb_engine::query::planning::aggregation::{
    prepare_aggregate_runtime_plan, validate_aggregate_projection_shape_with_host,
    AggregateGroupBinding, AggregateShapeHost,
};
use reddb_engine::query::runtime::aggregation::{
    aggregate_argument_key, aggregate_output_name, build_aggregate_group_key, sum_f64_to_value,
    AggregateGroupKey, AggregateHost, CompiledAggPlan, GroupKeyPart, ProjSlot, SlottedAggState,
};
pub(crate) use reddb_engine::query::runtime::aggregation::{
    base_function_name, has_aggregate_projections, is_aggregate_function,
};

use super::filter_compiled::{classify_field, EntityColumnResolver};
use crate::api::{RedDBError, RedDBResult};
use crate::runtime::join_filter::{
    eval_projection_value_with_db, evaluate_runtime_filter_with_db, field_ref_name,
    projection_name, resolve_runtime_field, sort_records_by_order_by_with_db,
};
use crate::runtime::runtime_table_record_from_entity_ref;
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::RedDB;
use reddb_rql::ast::{Expr, FieldRef, Filter, OrderByClause, Projection};
use reddb_rql::sql_lowering::{
    effective_table_filter, effective_table_group_by_exprs, effective_table_having_filter,
    effective_table_projections, expr_to_projection as lower_expr_to_projection,
};
use reddb_types::Value;

use super::TableQuery;

struct NativeAggregateShapeHost;
impl AggregateShapeHost for NativeAggregateShapeHost {
    type Error = RedDBError;
    fn validate_projection(&self, projection: &Projection) -> RedDBResult<()> {
        validate_h3_projection_resolutions(projection)
    }
    fn validate_group_expression(&self, expression: &Expr) -> RedDBResult<()> {
        validate_h3_expr_resolutions(expression)
    }
    fn query_error(&self, message: String) -> RedDBError {
        RedDBError::Query(message)
    }
}

struct NativeAggregateHost;
impl AggregateHost for NativeAggregateHost {
    fn compare_values(&self, left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
        crate::runtime::join_filter::runtime_partial_cmp(left, right)
    }
    fn evaluate_static_projection(&self, projection: &Projection) -> Option<Value> {
        eval_projection_value_with_db(None, projection, &UnifiedRecord::new())
    }
}
struct NativeAggregateResultHost<'a> {
    db: &'a RedDB,
}
impl AggregateHost for NativeAggregateResultHost<'_> {
    fn compare_values(&self, left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
        NativeAggregateHost.compare_values(left, right)
    }
    fn evaluate_static_projection(&self, projection: &Projection) -> Option<Value> {
        NativeAggregateHost.evaluate_static_projection(projection)
    }
}
impl reddb_engine::query::runtime::aggregate_results::AggregateResultHost
    for NativeAggregateResultHost<'_>
{
    type Error = RedDBError;
    fn guard_materialization(&self, groups: usize) -> RedDBResult<()> {
        crate::runtime::materialization_limit::guard(self.db, "aggregation", groups)
    }
    fn having_matches(&self, filter: &Filter, record: &UnifiedRecord) -> bool {
        evaluate_runtime_filter_with_db(Some(self.db), record, filter, None, None)
    }
    fn sort_records(&self, records: &mut Vec<UnifiedRecord>, clauses: &[OrderByClause]) {
        sort_records_by_order_by_with_db(Some(self.db), records, clauses, None, None)
    }
    fn secret_query_output(&self, value: Value) -> RedDBResult<Value> {
        crate::runtime::execution_context::secret_query_output(value)
    }
}

struct QueryTempDir {
    path: std::path::PathBuf,
}

impl QueryTempDir {
    fn new(prefix: &str) -> std::io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);

        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let unique = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "{prefix}{}-{now_nanos}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for QueryTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod query_temp_dir_tests {
    use super::QueryTempDir;

    #[test]
    fn query_temp_dir_new_creates_directory() {
        let dir = QueryTempDir::new("reddb-agg-test-").expect("temp dir should be created");

        assert!(dir.path().is_dir());
    }

    #[test]
    fn query_temp_dir_drop_removes_directory() {
        let path = {
            let dir = QueryTempDir::new("reddb-agg-test-").expect("temp dir should be created");
            let path = dir.path().to_path_buf();
            assert!(path.is_dir());
            path
        };

        assert!(!path.exists());
    }
}

/// Execute a query with aggregate functions (COUNT, AVG, SUM, MIN, MAX, GROUP BY).
pub(crate) fn execute_aggregate_query(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<UnifiedResult> {
    validate_aggregate_projection_shape_with_host(&NativeAggregateShapeHost, query)?;
    let mut sensitivity_source = UnifiedRecord::new();
    let contract = db.collection_contract(&query.table);
    let declared_schema = contract
        .as_ref()
        .is_some_and(|contract| !contract.declared_columns.is_empty());
    if let Some(contract) = contract {
        for column in &contract.declared_columns {
            if column.data_type.eq_ignore_ascii_case("secret") {
                sensitivity_source.set(&column.name, Value::Secret(Vec::new()));
            }
        }
    }
    let projections = effective_table_projections(query);
    let uses_secrets = |sensitivity_source: &UnifiedRecord| {
        effective_table_filter(query)
            .as_ref()
            .is_some_and(|filter| {
                crate::runtime::join_filter::filter_uses_secret(
                    filter,
                    sensitivity_source,
                    Some(&query.table),
                    query.alias.as_deref(),
                )
            })
            || effective_table_having_filter(query)
                .as_ref()
                .is_some_and(|filter| {
                    crate::runtime::join_filter::filter_uses_secret(
                        filter,
                        sensitivity_source,
                        Some(&query.table),
                        query.alias.as_deref(),
                    )
                })
            || projections.iter().any(|projection| {
                crate::runtime::join_filter::projection_uses_secret(
                    projection,
                    sensitivity_source,
                    Some(&query.table),
                    query.alias.as_deref(),
                )
            })
            || effective_table_group_by_exprs(query)
                .iter()
                .any(|expression| {
                    crate::runtime::join_filter::expression_uses_secret(
                        expression,
                        sensitivity_source,
                        Some(&query.table),
                        query.alias.as_deref(),
                    )
                })
            || query.order_by.iter().any(|order| {
                order.expr.as_ref().is_some_and(|expression| {
                    crate::runtime::join_filter::expression_uses_secret(
                        expression,
                        sensitivity_source,
                        Some(&query.table),
                        query.alias.as_deref(),
                    )
                }) || crate::runtime::join_filter::resolve_runtime_field(
                    sensitivity_source,
                    &order.field,
                    Some(&query.table),
                    query.alias.as_deref(),
                )
                .is_some_and(|value| matches!(value, Value::Secret(_)))
            })
    };

    let mut sensitive_query = uses_secrets(&sensitivity_source);

    // Untyped collections discover sensitive fields while scanning. Their
    // aggregate values must stay on this path, which tracks those fields.
    // ── Push-down GROUP BY planner — issue #161 ───────────────────────
    // The new `AggregateQueryPlanner` deep module accumulates per-group
    // state directly in the scan loop, materialising one row per group
    // instead of one per scanned input. Eligible: single-column GROUP
    // BY by simple column ref, aggregates limited to COUNT(*) /
    // COUNT(col) / SUM / AVG / MIN / MAX over simple column refs.
    // Unsupported shapes return None and fall through to the legacy
    // path below — this is a strict subset, so behaviour is identical
    // for queries it accepts.
    if declared_schema && !sensitive_query {
        if let Some(result) =
            super::aggregate_pushdown_dispatch::try_execute_pushdown_aggregate(db, query)?
        {
            return Ok(result);
        }
    }

    // Fast path — SELECT <col>, COUNT/SUM/AVG(...) FROM t GROUP BY <col>
    // parallelised across segments via rayon. This is the mini-duel
    // aggregate_group shape and avoids the generic Vec<GroupKeyPart> +
    // spill-capable accumulator for low-cardinality group scans.
    if declared_schema && !sensitive_query && !crate::runtime::function_budget::active() {
        if let Some(result) = try_execute_parallel_single_col_numeric_aggs(db, query)? {
            return Ok(result);
        }
    }

    let effective_projections = effective_table_projections(query);
    let effective_filter = effective_table_filter(query);
    let runtime_plan = prepare_aggregate_runtime_plan(query)
        .map_err(|message| RedDBError::Query(message.into()))?;
    let all_aggregate_projections = &runtime_plan.aggregate_projections;
    let effective_group_by = &runtime_plan.group_bindings;

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
                if parse_time_bucket_group_expr(expr.key.as_deref().unwrap_or_default()).is_some() {
                    return None;
                }
                match &expr.expression {
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
    let agg_plan = CompiledAggPlan::compile(all_aggregate_projections)
        .ok_or_else(|| RedDBError::Query("aggregate projection exceeds nesting limit".into()))?;

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
    let spill_dir = QueryTempDir::new("reddb-agg-")
        .map_err(|e| RedDBError::Query(format!("agg spill dir: {e}")))?;
    let mut spill_agg = crate::storage::query::executors::agg_spill::SpilledHashAgg::<
        AggregateGroupKey,
        AggregateGroup,
    >::new(spill_dir.path(), WORK_MEM_BYTES, ESTIMATED_ENTRY_BYTES);
    let mut spill_err: Option<String> = None;

    let snapshot = crate::runtime::impl_core::capture_current_snapshot();
    crate::runtime::function_budget::scan(
        |visit| manager.scan_for_each(snapshot.as_ref(), visit),
        |entity| {
            // ── Lazy record materialisation ──────────────────────────────────
            // We defer `runtime_table_record_from_entity` until we actually
            // need it (complex filters, GROUP BY exprs or aggregate args that
            // can't be read directly from the entity).
            let mut record_cache: Option<UnifiedRecord> = None;

            macro_rules! get_or_make_record {
                () => {{
                    if record_cache.is_none() {
                        record_cache = runtime_table_record_from_entity_ref(entity);
                    }
                    record_cache.as_ref()
                }};
            }

            if !declared_schema {
                if let Some(record) = get_or_make_record!() {
                    let mut discovered = false;
                    for column in record.column_names() {
                        if record
                            .get(&column)
                            .is_some_and(|value| matches!(value, Value::Secret(_)))
                            && sensitivity_source.get(&column).is_none()
                        {
                            sensitivity_source.set(&column, Value::Secret(Vec::new()));
                            discovered = true;
                        }
                    }
                    if discovered {
                        sensitive_query = uses_secrets(&sensitivity_source);
                    }
                }
            }

            if let Some(c) = compiled_filter.as_ref() {
                let matches = c.evaluate(entity).resolve_with_fallback(|| {
                    let Some(record) = get_or_make_record!() else {
                        return false;
                    };
                    effective_filter.as_ref().is_some_and(|filter| {
                        evaluate_runtime_filter_with_db(
                            Some(db),
                            record,
                            filter,
                            Some(table_name),
                            Some(table_alias),
                        )
                    })
                });
                if !matches {
                    return true;
                }
            }

            let group_values = if has_group_by {
                if group_by_all_fast {
                    // Fast path: all GROUP BY are simple columns → read from entity.
                    let mut values = Vec::with_capacity(effective_group_by.len());
                    for (resolver_opt, expr) in group_by_kinds.iter().zip(effective_group_by) {
                        let value = if let Some(resolver) = resolver_opt {
                            resolver
                                .get_value(0, entity)
                                .map(|v| v.into_owned())
                                .and_then(crate::runtime::execution_context::secret_query_input)
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
                    for group_expr in effective_group_by {
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
                        if sensitive_query {
                            spill_err = Some(
                                "secret aggregation exceeded its in-memory group limit".into(),
                            );
                            return false;
                        }
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

                // COUNT(*) is already counted; shared slots are updated by their owner.
                if matches!(slot, ProjSlot::CountStar | ProjSlot::Shared) {
                    continue;
                }

                // Resolve argument value: entity fast path first, then record.
                let val = if let Some(kind) = agg_arg_kinds.get(proj_idx).and_then(|k| k.as_ref()) {
                    super::filter_compiled::resolve_kind(kind, entity)
                        .map(|v| v.into_owned())
                        .or_else(|| {
                            get_or_make_record!().and_then(|rec| {
                                resolve_aggregate_argument_value(db, args.first(), rec)
                            })
                        })
                } else {
                    match get_or_make_record!() {
                        Some(rec) => resolve_aggregate_argument_value(db, args.first(), rec),
                        None => continue,
                    }
                };
                let val = val.and_then(crate::runtime::execution_context::secret_query_input);
                let Some(val) = val else { continue };
                state.accumulate(&NativeAggregateHost, slot, val);
            }

            true
        },
    )?;

    // Propagate any spill I/O error from the iteration callback
    if let Some(e) = spill_err {
        return Err(RedDBError::Query(e));
    }

    // Flush the remaining in-memory groups to spill_agg, then drain all
    // on-disk batches back into a single merged HashMap.
    // When no spill occurred, spill_agg holds only the in-memory table.
    let groups = if sensitive_query {
        groups
    } else {
        for (k, v) in groups {
            spill_agg
                .accumulate(k, v)
                .map_err(|e| RedDBError::Query(format!("agg spill flush: {e}")))?;
        }
        spill_agg
            .drain()
            .map_err(|e| RedDBError::Query(format!("agg spill drain: {e}")))?
    };

    reddb_engine::query::runtime::aggregate_results::finish_aggregate_results_with_host(
        &NativeAggregateResultHost { db },
        reddb_engine::query::runtime::aggregate_results::AggregateResultContext {
            query,
            visible_projections: &effective_projections,
            runtime_plan: &runtime_plan,
            slots: &agg_plan,
            sensitivity_source: sensitive_query.then_some(&sensitivity_source),
        },
        groups.values().map(|group| {
            reddb_engine::query::runtime::aggregate_results::AggregateGroupRef {
                group_values: &group.group_values,
                state: &group.state,
            }
        }),
    )
}

fn projection_from_expr(expr: &Expr) -> Option<Projection> {
    lower_expr_to_projection(expr)
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

fn validate_h3_projection_resolutions(projection: &Projection) -> RedDBResult<()> {
    match projection {
        Projection::Function(name, args) => {
            validate_h3_function_projection_resolution(base_function_name(name), args)?;
            for arg in args {
                validate_h3_projection_resolutions(arg)?;
            }
        }
        Projection::Expression(filter, _) => validate_h3_filter_resolutions(filter)?,
        _ => {}
    }
    Ok(())
}

fn validate_h3_filter_resolutions(filter: &Filter) -> RedDBResult<()> {
    match filter {
        Filter::And(left, right) | Filter::Or(left, right) => {
            validate_h3_filter_resolutions(left)?;
            validate_h3_filter_resolutions(right)?;
        }
        Filter::Not(inner) => validate_h3_filter_resolutions(inner)?,
        _ => {}
    }
    Ok(())
}

fn validate_h3_expr_resolutions(expr: &Expr) -> RedDBResult<()> {
    match expr {
        Expr::FunctionCall { name, args, .. } => {
            validate_h3_function_expr_resolution(name, args)?;
            for arg in args {
                validate_h3_expr_resolutions(arg)?;
            }
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            validate_h3_expr_resolutions(lhs)?;
            validate_h3_expr_resolutions(rhs)?;
        }
        Expr::UnaryOp { operand, .. } | Expr::Cast { inner: operand, .. } => {
            validate_h3_expr_resolutions(operand)?;
        }
        Expr::Case {
            branches, else_, ..
        } => {
            for (cond, value) in branches {
                validate_h3_expr_resolutions(cond)?;
                validate_h3_expr_resolutions(value)?;
            }
            if let Some(else_expr) = else_ {
                validate_h3_expr_resolutions(else_expr)?;
            }
        }
        Expr::IsNull { operand, .. } => validate_h3_expr_resolutions(operand)?,
        Expr::InList { target, values, .. } => {
            validate_h3_expr_resolutions(target)?;
            for value in values {
                validate_h3_expr_resolutions(value)?;
            }
        }
        Expr::Between {
            target, low, high, ..
        } => {
            validate_h3_expr_resolutions(target)?;
            validate_h3_expr_resolutions(low)?;
            validate_h3_expr_resolutions(high)?;
        }
        Expr::Literal { .. }
        | Expr::Column { .. }
        | Expr::Parameter { .. }
        | Expr::Subquery { .. }
        | Expr::WindowFunctionCall { .. } => {}
    }
    Ok(())
}

fn validate_h3_function_projection_resolution(name: &str, args: &[Projection]) -> RedDBResult<()> {
    let Some(res_arg) = h3_resolution_projection_arg(name, args) else {
        return Ok(());
    };
    let Some(res) = literal_projection_i64(res_arg) else {
        return Ok(());
    };
    validate_h3_resolution_value(name, res)
}

fn validate_h3_function_expr_resolution(name: &str, args: &[Expr]) -> RedDBResult<()> {
    let upper = name.to_ascii_uppercase();
    let res_arg = match upper.as_str() {
        "H3_CELL" | "H3_PARENT" => args.get(1),
        _ => None,
    };
    let Some(Expr::Literal { value, .. }) = res_arg else {
        return Ok(());
    };
    let Some(res) = value_i64(value) else {
        return Ok(());
    };
    validate_h3_resolution_value(&upper, res)
}

fn h3_resolution_projection_arg<'a>(name: &str, args: &'a [Projection]) -> Option<&'a Projection> {
    match name.to_ascii_uppercase().as_str() {
        "H3_CELL" | "H3_PARENT" => args.get(1),
        _ => None,
    }
}

fn literal_projection_i64(projection: &Projection) -> Option<i64> {
    let record = UnifiedRecord::new();
    let value = eval_projection_value_with_db(None, projection, &record)?;
    value_i64(&value).or_else(|| match value {
        Value::Float(value) if value.fract() == 0.0 => Some(value as i64),
        _ => None,
    })
}

fn value_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) | Value::BigInt(value) => Some(*value),
        Value::UnsignedInteger(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

fn validate_h3_resolution_value(function_name: &str, resolution: i64) -> RedDBResult<()> {
    if crate::geo::h3::valid_resolution(resolution).is_some() {
        return Ok(());
    }
    Err(RedDBError::Query(format!(
        "{function_name} H3 resolution {resolution} is invalid; expected H3 resolution in range 0..=15"
    )))
}

fn resolve_group_by_value(
    db: &RedDB,
    binding: &AggregateGroupBinding,
    record: &UnifiedRecord,
) -> Option<Value> {
    let group_expr = &binding.expression;
    if let Some((bucket_ns, timestamp_column)) =
        parse_time_bucket_group_expr(binding.key.as_deref().unwrap_or_default())
    {
        let timestamp_ns = resolve_bucket_timestamp_ns(record, timestamp_column.as_deref())?;
        let bucket_start = timestamp_ns
            .checked_div(bucket_ns)
            .map(|bucket| bucket * bucket_ns)
            .unwrap_or(timestamp_ns);
        return Some(Value::UnsignedInteger(bucket_start));
    }

    match group_expr {
        Expr::Column { field, .. } => resolve_runtime_field(record, field, None, None)
            .and_then(crate::runtime::execution_context::secret_query_input),
        _ => {
            let projection = projection_from_expr(group_expr)?;
            let value = eval_projection_value_with_db(Some(db), &projection, record)?;
            if h3_group_expr_yields_null_drop(group_expr) && matches!(value, Value::Null) {
                None
            } else {
                Some(value)
            }
        }
    }
}

fn h3_group_expr_yields_null_drop(group_expr: &Expr) -> bool {
    matches!(
        group_expr,
        Expr::FunctionCall { name, .. }
            if matches!(name.to_ascii_uppercase().as_str(), "H3_CELL" | "H3_PARENT")
    )
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

#[derive(Clone)]
struct AggregateGroup {
    group_values: Vec<Value>,
    state: SlottedAggState,
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
    use reddb_types::{CanonicalKey, CanonicalKeyFamily, Value};

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
            s.merge_with_host(&super::NativeAggregateHost, o);
        }
    }
}

// ── Fast path — parallel single-column numeric GROUP BY ───────────────────
//
// Serves queries shaped as:
//   SELECT <col>, COUNT(*), AVG(<num_col>), SUM(<num_col>)
//   FROM <table> GROUP BY <col> [ORDER BY <col>]
// with no WHERE, HAVING, LIMIT, OFFSET, or extra projections.
//
// Wins three ways versus the generic loop in `execute_aggregate_query`:
//   1. No `Vec<Value>` / `Vec<GroupKeyPart>` allocation per row — each worker
//      groups by `SingleGroupKey` directly.
//   2. Numeric aggregate slots are plain Vec<f64>/Vec<u64>, not the full
//      general-purpose slotted state with spill codecs.
//   3. Segments are folded in parallel via `SegmentManager::fold_entities_parallel`.
//
// Returns `Ok(None)` when the query doesn't fit or runtime values need the
// generic key/value semantics.
#[derive(Clone, PartialEq, Eq, Hash)]
enum SingleGroupKey {
    Null,
    Bool(bool),
    Int(i64),
    UInt(u64),
    Text(std::sync::Arc<str>),
}

impl SingleGroupKey {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Null => Some(Self::Null),
            Value::Boolean(b) => Some(Self::Bool(*b)),
            Value::Integer(n) => Some(Self::Int(*n)),
            Value::UnsignedInteger(n) => Some(Self::UInt(*n)),
            Value::Text(s) => Some(Self::Text(s.clone())),
            _ => None,
        }
    }

    fn into_value(self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Boolean(b),
            Self::Int(n) => Value::Integer(n),
            Self::UInt(n) => Value::UnsignedInteger(n),
            Self::Text(s) => Value::text(s),
        }
    }
}

struct FastEntityAccessor {
    name: String,
    schema_idx: Option<u16>,
    fallback: super::filter_compiled::EntityFieldKind,
}

impl FastEntityAccessor {
    fn get_value<'a>(
        &'a self,
        entity: &'a crate::storage::unified::entity::UnifiedEntity,
    ) -> Option<std::borrow::Cow<'a, Value>> {
        if let Some(idx) = self.schema_idx {
            if let Some(row) = entity.data.as_row() {
                if row.named.is_none()
                    && row
                        .schema
                        .as_ref()
                        .and_then(|schema| schema.get(idx as usize))
                        .is_some_and(|name| name == &self.name)
                {
                    if let Some(value) = row.columns.get(idx as usize) {
                        return Some(std::borrow::Cow::Borrowed(value));
                    }
                }
            }
        }

        super::filter_compiled::resolve_kind(&self.fallback, entity)
    }
}

enum FastAggOutput {
    Group {
        output_name: String,
    },
    CountStar {
        output_name: String,
    },
    Sum {
        output_name: String,
        slot: usize,
        accessor: FastEntityAccessor,
    },
    Avg {
        output_name: String,
        slot: usize,
        accessor: FastEntityAccessor,
    },
}

impl FastAggOutput {
    fn output_name(&self) -> &str {
        match self {
            Self::Group { output_name }
            | Self::CountStar { output_name }
            | Self::Sum { output_name, .. }
            | Self::Avg { output_name, .. } => output_name,
        }
    }
}

struct FastNumericGroupState {
    rows: u64,
    sums: Vec<f64>,
    counts: Vec<u64>,
}

impl FastNumericGroupState {
    fn new(numeric_slots: usize) -> Self {
        Self {
            rows: 0,
            sums: vec![0.0; numeric_slots],
            counts: vec![0; numeric_slots],
        }
    }

    fn merge(&mut self, other: Self) {
        self.rows += other.rows;
        for (idx, value) in other.sums.into_iter().enumerate() {
            if let Some(sum) = self.sums.get_mut(idx) {
                *sum += value;
            }
        }
        for (idx, value) in other.counts.into_iter().enumerate() {
            if let Some(count) = self.counts.get_mut(idx) {
                *count += value;
            }
        }
    }
}

struct FastNumericGroupAccumulator {
    groups: std::collections::HashMap<SingleGroupKey, FastNumericGroupState>,
    unsupported_value: bool,
}

fn try_execute_parallel_single_col_numeric_aggs(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<Option<UnifiedResult>> {
    if query.limit.is_some()
        || query.offset.is_some()
        || query.filter.is_some()
        || query.where_expr.is_some()
        || query.having.is_some()
        || query.having_expr.is_some()
        || query.expand.is_some()
    {
        return Ok(None);
    }
    let group_exprs = effective_table_group_by_exprs(query);
    if group_exprs.len() != 1 {
        return Ok(None);
    }
    let projections = effective_table_projections(query);
    if projections.len() < 2 {
        return Ok(None);
    }

    let group_col = match &group_exprs[0] {
        Expr::Column {
            field: FieldRef::TableColumn { column, .. },
            ..
        } => column.clone(),
        _ => return Ok(None),
    };

    let manager = db
        .store()
        .get_collection(query.table.as_str())
        .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let schema_cols = manager.column_schema();

    let mut outputs = Vec::with_capacity(projections.len());
    let mut saw_group_projection = false;
    let mut saw_aggregate_projection = false;
    let mut numeric_slots = 0usize;
    for projection in &projections {
        if projection_matches_group_col(projection, &group_col) {
            saw_group_projection = true;
            outputs.push(FastAggOutput::Group {
                output_name: projection_name(projection),
            });
            continue;
        }

        let Some(output) = build_fast_numeric_agg_output(
            projection,
            table_name,
            table_alias,
            schema_cols.as_deref().map(|cols| cols.as_slice()),
            &mut numeric_slots,
        ) else {
            return Ok(None);
        };
        saw_aggregate_projection = true;
        outputs.push(output);
    }
    if !saw_group_projection || !saw_aggregate_projection {
        return Ok(None);
    }

    let order_by = match fast_group_order_by(query, &group_col) {
        Some(order_by) => order_by,
        None => return Ok(None),
    };

    let field = FieldRef::TableColumn {
        table: String::new(),
        column: group_col.clone(),
    };
    let Some(group_accessor) = build_fast_entity_accessor(
        &field,
        table_name,
        table_alias,
        schema_cols.as_deref().map(|cols| cols.as_slice()),
    ) else {
        return Ok(None);
    };

    let snapshot = crate::runtime::impl_core::capture_current_snapshot();
    let acc = manager.scan_fold_parallel(
        snapshot.as_ref(),
        || FastNumericGroupAccumulator {
            groups: std::collections::HashMap::new(),
            unsupported_value: false,
        },
        |mut local, entity| {
            if local.unsupported_value {
                return local;
            }
            let Some(value_cow) = group_accessor.get_value(entity) else {
                local.unsupported_value = true;
                return local;
            };
            let value = value_cow.into_owned();
            let Some(key) = SingleGroupKey::from_value(&value) else {
                local.unsupported_value = true;
                return local;
            };
            let group = local
                .groups
                .entry(key)
                .or_insert_with(|| FastNumericGroupState::new(numeric_slots));
            group.rows += 1;

            for output in &outputs {
                match output {
                    FastAggOutput::Sum { slot, accessor, .. }
                    | FastAggOutput::Avg { slot, accessor, .. } => {
                        let Some(value) = accessor.get_value(entity) else {
                            continue;
                        };
                        let Some(num) = super::aggregate_value_to_f64(value.as_ref()) else {
                            continue;
                        };
                        if let Some(sum) = group.sums.get_mut(*slot) {
                            *sum += num;
                        }
                        if let Some(count) = group.counts.get_mut(*slot) {
                            *count += 1;
                        }
                    }
                    FastAggOutput::Group { .. } | FastAggOutput::CountStar { .. } => {}
                }
            }

            local
        },
        |mut a, b| {
            a.unsupported_value |= b.unsupported_value;
            for (key, state) in b.groups {
                match a.groups.get_mut(&key) {
                    Some(existing) => existing.merge(state),
                    None => {
                        a.groups.insert(key, state);
                    }
                }
            }
            a
        },
    );

    if acc.unsupported_value {
        return Ok(None);
    }

    let mut groups: Vec<_> = acc.groups.into_iter().collect();
    if let Some((ascending, nulls_first)) = order_by {
        groups.sort_by(|(left, _), (right, _)| {
            let ord = compare_single_group_key(left, right, nulls_first);
            if ascending {
                ord
            } else {
                ord.reverse()
            }
        });
    }

    // Issue #769 — same materialization ceiling as the generic path,
    // enforced on the parallel single-column fast path before we
    // project the per-group records.
    crate::runtime::materialization_limit::guard(db, "aggregation", groups.len())?;

    let mut records = Vec::with_capacity(groups.len().max(1));
    for (key, state) in groups {
        let group_value = key.clone().into_value();
        let mut record = UnifiedRecord::new();
        record.set(&group_col, group_value.clone());
        for output in &outputs {
            match output {
                FastAggOutput::Group { output_name } => {
                    record.set(output_name, group_value.clone());
                }
                FastAggOutput::CountStar { output_name } => {
                    record.set(output_name, Value::Integer(state.rows as i64));
                }
                FastAggOutput::Sum {
                    output_name, slot, ..
                } => {
                    let value = if state.counts.get(*slot).copied().unwrap_or(0) == 0 {
                        Value::Null
                    } else {
                        sum_f64_to_value(state.sums.get(*slot).copied().unwrap_or_default())
                    };
                    record.set(output_name, value);
                }
                FastAggOutput::Avg {
                    output_name, slot, ..
                } => {
                    let count = state.counts.get(*slot).copied().unwrap_or(0);
                    let value = if count == 0 {
                        Value::Null
                    } else {
                        Value::Float(
                            state.sums.get(*slot).copied().unwrap_or_default() / count as f64,
                        )
                    };
                    record.set(output_name, value);
                }
            }
        }
        records.push(record);
    }

    Ok(Some(UnifiedResult {
        columns: outputs
            .iter()
            .map(|output| output.output_name().to_string())
            .collect(),
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    }))
}

fn projection_matches_group_col(projection: &Projection, group_col: &str) -> bool {
    match projection {
        Projection::Column(name) => name == group_col,
        Projection::Alias(name, _) => name == group_col,
        Projection::Field(FieldRef::TableColumn { column, .. }, _) => column == group_col,
        _ => false,
    }
}

fn build_fast_numeric_agg_output(
    projection: &Projection,
    table_name: &str,
    table_alias: &str,
    schema_cols: Option<&[String]>,
    next_numeric_slot: &mut usize,
) -> Option<FastAggOutput> {
    let Projection::Function(name, args) = projection else {
        return None;
    };
    let func_name = base_function_name(name).to_uppercase();
    if func_name == "COUNT" && projection_is_count_star(args) {
        return Some(FastAggOutput::CountStar {
            output_name: projection_name(projection),
        });
    }
    if func_name != "SUM" && func_name != "AVG" {
        return None;
    }

    let (field, col_name) = projection_simple_field_arg(args)?;
    let accessor = build_fast_entity_accessor(&field, table_name, table_alias, schema_cols)?;
    let slot = *next_numeric_slot;
    *next_numeric_slot += 1;
    let output_name = aggregate_output_name(projection, &func_name, &col_name);
    match func_name.as_str() {
        "SUM" => Some(FastAggOutput::Sum {
            output_name,
            slot,
            accessor,
        }),
        "AVG" => Some(FastAggOutput::Avg {
            output_name,
            slot,
            accessor,
        }),
        _ => None,
    }
}

fn projection_is_count_star(args: &[Projection]) -> bool {
    if args.len() != 1 {
        return false;
    }
    match &args[0] {
        Projection::All => true,
        Projection::Column(value) => value == "*" || value == "LIT:*",
        _ => false,
    }
}

fn projection_simple_field_arg(args: &[Projection]) -> Option<(FieldRef, String)> {
    if args.len() != 1 {
        return None;
    }
    match &args[0] {
        Projection::Column(column) if !column.starts_with("LIT:") && column != "*" => Some((
            FieldRef::TableColumn {
                table: String::new(),
                column: column.clone(),
            },
            column.clone(),
        )),
        Projection::Field(field @ FieldRef::TableColumn { column, .. }, _) => {
            Some((field.clone(), column.clone()))
        }
        _ => None,
    }
}

fn build_fast_entity_accessor(
    field: &FieldRef,
    table_name: &str,
    table_alias: &str,
    schema_cols: Option<&[String]>,
) -> Option<FastEntityAccessor> {
    let kind = super::filter_compiled::classify_field(field, table_name, table_alias);
    if matches!(
        kind,
        super::filter_compiled::EntityFieldKind::DocumentPath(_)
            | super::filter_compiled::EntityFieldKind::Unknown
    ) {
        return None;
    }

    let schema_idx = match (&kind, field) {
        (
            super::filter_compiled::EntityFieldKind::RowField(name),
            FieldRef::TableColumn { table, column },
        ) if column == name
            && (table.is_empty() || table == table_name || table == table_alias) =>
        {
            schema_cols
                .and_then(|cols| cols.iter().position(|candidate| candidate == name))
                .and_then(|idx| u16::try_from(idx).ok())
        }
        _ => None,
    };

    let name = match &kind {
        super::filter_compiled::EntityFieldKind::RowField(name)
        | super::filter_compiled::EntityFieldKind::RowFieldFast { name, .. } => name.clone(),
        _ => field_ref_name(field),
    };

    Some(FastEntityAccessor {
        name,
        schema_idx,
        fallback: kind,
    })
}

fn fast_group_order_by(query: &TableQuery, group_col: &str) -> Option<Option<(bool, bool)>> {
    if query.order_by.is_empty() {
        return Some(None);
    }
    if query.order_by.len() != 1 {
        return None;
    }
    let clause = &query.order_by[0];
    if let Some(expr) = &clause.expr {
        match expr {
            Expr::Column { field, .. } if field_ref_name(field) == group_col => {}
            _ => return None,
        }
    }
    if field_ref_name(&clause.field) != group_col {
        return None;
    }
    Some(Some((clause.ascending, clause.nulls_first)))
}

fn compare_single_group_key(
    left: &SingleGroupKey,
    right: &SingleGroupKey,
    nulls_first: bool,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (left, right) {
        (SingleGroupKey::Null, SingleGroupKey::Null) => Ordering::Equal,
        (SingleGroupKey::Null, _) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (_, SingleGroupKey::Null) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (SingleGroupKey::Bool(a), SingleGroupKey::Bool(b)) => a.cmp(b),
        (SingleGroupKey::Int(a), SingleGroupKey::Int(b)) => a.cmp(b),
        (SingleGroupKey::UInt(a), SingleGroupKey::UInt(b)) => a.cmp(b),
        (SingleGroupKey::Text(a), SingleGroupKey::Text(b)) => a.cmp(b),
        (a, b) => single_group_key_rank(a).cmp(&single_group_key_rank(b)),
    }
}

fn single_group_key_rank(key: &SingleGroupKey) -> u8 {
    match key {
        SingleGroupKey::Null => 0,
        SingleGroupKey::Bool(_) => 1,
        SingleGroupKey::Int(_) => 2,
        SingleGroupKey::UInt(_) => 3,
        SingleGroupKey::Text(_) => 4,
    }
}

#[cfg(test)]
mod parallel_group_by_tests {
    use crate::{RedDBOptions, RedDBRuntime};
    use reddb_types::Value;

    fn mk_runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory())
            .expect("in-memory runtime should open")
    }

    fn seed_cities(rt: &RedDBRuntime, total: usize, cities: &[&str]) {
        rt.execute_query("CREATE TABLE users (id INT, name TEXT, city TEXT, age INT)")
            .unwrap();
        for i in 0..total {
            let city = cities[i % cities.len()];
            rt.execute_query(&format!(
                "INSERT INTO users (id, name, city, age) VALUES ({i}, 'u{i}', '{city}', {})",
                20 + (i % 40)
            ))
            .unwrap();
        }
    }

    fn count_by_city(rt: &RedDBRuntime) -> Vec<(String, u64)> {
        let r = rt
            .execute_query("SELECT city, COUNT(*) FROM users GROUP BY city")
            .expect("group by ok");
        let mut out: Vec<(String, u64)> = r
            .result
            .records
            .iter()
            .filter_map(|rec| {
                let city = match rec.get("city")? {
                    reddb_types::Value::Text(s) => s.to_string(),
                    _ => return None,
                };
                let count = match rec
                    .get("COUNT")
                    .or_else(|| rec.get("COUNT(*)"))
                    .or_else(|| rec.get("count"))?
                {
                    reddb_types::Value::UnsignedInteger(n) => *n,
                    reddb_types::Value::Integer(n) => *n as u64,
                    _ => return None,
                };
                Some((city, count))
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn number(value: &Value) -> f64 {
        match value {
            Value::Integer(n) => *n as f64,
            Value::UnsignedInteger(n) => *n as f64,
            Value::Float(n) => *n,
            other => panic!("expected numeric value, got {other:?}"),
        }
    }

    #[test]
    fn single_col_count_returns_correct_counts() {
        let rt = mk_runtime();
        seed_cities(&rt, 300, &["NYC", "LA", "CHI"]);
        let counts = count_by_city(&rt);
        assert_eq!(counts.len(), 3);
        for (_, n) in &counts {
            assert_eq!(*n, 100, "each city should have 100 rows (got {n})");
        }
    }

    #[test]
    fn single_col_count_many_cities() {
        let rt = mk_runtime();
        seed_cities(&rt, 1000, &["A", "B", "C", "D", "E"]);
        let counts = count_by_city(&rt);
        assert_eq!(counts.len(), 5);
        for (_, n) in &counts {
            assert_eq!(*n, 200);
        }
    }

    #[test]
    fn single_col_count_single_group() {
        let rt = mk_runtime();
        seed_cities(&rt, 50, &["NYC"]);
        let counts = count_by_city(&rt);
        assert_eq!(counts, vec![("NYC".to_string(), 50)]);
    }

    #[test]
    fn single_col_count_avg_sum_ordered_by_group_col() {
        let rt = mk_runtime();
        rt.execute_query("CREATE TABLE users (id INT, city TEXT, age INT, score INT)")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, city, age, score) VALUES (1, 'NYC', 20, 10)")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, city, age, score) VALUES (2, 'LA', 40, 5)")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, city, age, score) VALUES (3, 'NYC', 30, 30)")
            .unwrap();
        rt.execute_query("INSERT INTO users (id, city, age, score) VALUES (4, 'LA', 20, 15)")
            .unwrap();

        let r = rt
            .execute_query(
                "SELECT city, COUNT(*) AS cnt, AVG(age) AS avg_age, SUM(score) AS sum_score \
                 FROM users GROUP BY city ORDER BY city",
            )
            .expect("aggregate group fast path should return results");
        assert_eq!(
            r.result.columns,
            vec!["city", "cnt", "avg_age", "sum_score"]
        );
        assert_eq!(r.result.records.len(), 2);

        let first = &r.result.records[0];
        assert_eq!(first.get("city"), Some(&Value::text("LA")));
        assert_eq!(number(first.get("cnt").unwrap()), 2.0);
        assert_eq!(number(first.get("avg_age").unwrap()), 30.0);
        assert_eq!(number(first.get("sum_score").unwrap()), 20.0);

        let second = &r.result.records[1];
        assert_eq!(second.get("city"), Some(&Value::text("NYC")));
        assert_eq!(number(second.get("cnt").unwrap()), 2.0);
        assert_eq!(number(second.get("avg_age").unwrap()), 25.0);
        assert_eq!(number(second.get("sum_score").unwrap()), 40.0);
    }

    #[test]
    fn single_col_count_empty_table() {
        let rt = mk_runtime();
        rt.execute_query("CREATE TABLE users (id INT, city TEXT)")
            .unwrap();
        let r = rt
            .execute_query("SELECT city, COUNT(*) FROM users GROUP BY city")
            .unwrap();
        assert_eq!(r.result.records.len(), 0);
    }

    #[test]
    fn fallback_when_where_clause_present() {
        // WHERE isn't supported by the fast path — it must defer to the
        // generic aggregate loop. Correctness guard: the query still
        // returns valid results (filtered groups).
        let rt = mk_runtime();
        seed_cities(&rt, 300, &["NYC", "LA", "CHI"]);
        let r = rt
            .execute_query("SELECT city, COUNT(*) FROM users WHERE age > 40 GROUP BY city")
            .expect("with WHERE ok via generic path");
        // Filter is age > 40; ages cycle 20..59; 19/40 values match; 300×19/40≈142.
        // The generic path may label the count column differently; accept
        // any numeric column that isn't the group key as the count.
        let total: u64 = r
            .result
            .records
            .iter()
            .filter_map(|rec| {
                for (k, v) in rec.iter_fields() {
                    if k.as_ref() == "city" {
                        continue;
                    }
                    match v {
                        reddb_types::Value::UnsignedInteger(n) => return Some(*n),
                        reddb_types::Value::Integer(n) => return Some(*n as u64),
                        _ => {}
                    }
                }
                None
            })
            .sum();
        assert!(
            !r.result.records.is_empty(),
            "expected at least one group record"
        );
        assert!(total > 0, "expected some rows past filter");
    }
}

#[cfg(test)]
mod aggregate_numeric_route_parity_tests {
    use crate::storage::unified::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
    use crate::{RedDBOptions, RedDBRuntime};
    use reddb_types::Value;
    use std::collections::HashMap;

    const PLANNER_SUM_QUERY: &str = "SELECT bucket, SUM(value) FROM samples GROUP BY bucket";
    const PLANNER_AVG_QUERY: &str = "SELECT bucket, AVG(value) FROM samples GROUP BY bucket";
    // HAVING is deliberately outside the push-down and parallel fast-path
    // envelopes, so these otherwise equivalent queries force the legacy route.
    const LEGACY_SUM_QUERY: &str =
        "SELECT bucket, SUM(value) FROM samples GROUP BY bucket HAVING COUNT(*) > 0";
    const LEGACY_AVG_QUERY: &str =
        "SELECT bucket, AVG(value) FROM samples GROUP BY bucket HAVING COUNT(*) > 0";

    fn runtime_with_table(value_type: &str) -> RedDBRuntime {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory())
            .expect("in-memory runtime should open");
        runtime
            .execute_query(&format!(
                "CREATE TABLE samples (bucket TEXT, value {value_type})"
            ))
            .expect("table should be created");
        runtime
    }

    fn insert(runtime: &RedDBRuntime, bucket: &str, value: Value) {
        let named = HashMap::from([
            ("bucket".to_string(), Value::text(bucket)),
            ("value".to_string(), value),
        ]);
        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: "samples".into(),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );
        runtime
            .db()
            .store()
            .insert("samples", entity)
            .expect("typed aggregate input should be inserted");
    }

    fn aggregate_rows(runtime: &RedDBRuntime, query: &str) -> Vec<(String, f64)> {
        let result = runtime
            .execute_query(query)
            .expect("aggregate query should run");
        let aggregate_column = result
            .result
            .columns
            .iter()
            .find(|column| column.as_str() != "bucket")
            .expect("aggregate column label")
            .clone();
        let mut rows = result
            .result
            .records
            .iter()
            .map(|record| {
                let Value::Text(bucket) = record.get("bucket").expect("bucket output") else {
                    panic!("bucket should be text")
                };
                (
                    bucket.to_string(),
                    number(record.get(&aggregate_column).expect("aggregate output")),
                )
            })
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        rows
    }

    fn number(value: &Value) -> f64 {
        match value {
            Value::Integer(value) => *value as f64,
            Value::UnsignedInteger(value) => *value as f64,
            Value::BigInt(value) => *value as f64,
            Value::Float(value) => *value,
            other => panic!("expected numeric aggregate, got {other:?}"),
        }
    }

    #[test]
    fn shared_group_bindings_preserve_aliases_and_output_columns() {
        let runtime = runtime_with_table("BIGINT");
        for (bucket, value) in [("red", -10), ("red", 10), ("blue", 20)] {
            insert(&runtime, bucket, Value::BigInt(value));
        }
        let grouped = runtime.execute_query("SELECT bucket AS tenant, SUM(value) AS amount FROM samples GROUP BY TENANT HAVING COUNT(*) > 0 ORDER BY tenant ASC").expect("case-insensitive SELECT alias grouping");
        assert_eq!(grouped.result.columns, ["tenant", "amount"]);
        assert_eq!(grouped.result.records.len(), 2);
        for (record, (tenant, amount)) in grouped
            .result
            .records
            .iter()
            .zip([("blue", 20.0), ("red", 0.0)])
        {
            assert_eq!(record.get("tenant"), Some(&Value::text(tenant)));
            assert_eq!(record.get("bucket"), Some(&Value::text(tenant)));
            assert_eq!(number(record.get("amount").expect("sum alias")), amount);
        }
        let grouped = runtime.execute_query("SELECT SUM(value) AS amount, bucket AS tenant, ABS(value) AS magnitude FROM samples GROUP BY tenant, magnitude HAVING COUNT(*) > 0 ORDER BY tenant ASC").expect("multiple group aliases including a function");
        assert_eq!(grouped.result.columns, ["tenant", "magnitude", "amount"]);
        assert_eq!(grouped.result.records.len(), 2);
        for (record, (tenant, magnitude, amount)) in grouped
            .result
            .records
            .iter()
            .zip([("blue", 20.0, 20.0), ("red", 10.0, 0.0)])
        {
            assert_eq!(record.get("tenant"), Some(&Value::text(tenant)));
            assert_eq!(
                number(record.get("magnitude").expect("function alias")),
                magnitude
            );
            assert_eq!(number(record.get("amount").expect("sum alias")), amount);
        }
        let error = runtime.execute_query("SELECT bucket AS tenant, SUM(value) FROM samples AS s GROUP BY s.tenant HAVING COUNT(*) > 0").expect_err("qualified name cannot resolve to SELECT alias");
        assert!(error.to_string().contains("must appear in GROUP BY"));
    }

    #[test]
    fn shared_shape_validation_retains_native_h3_admission() {
        let runtime = runtime_with_table("BIGINT");
        for sql in [
            "SELECT H3_PARENT(bucket, 16) AS cell, COUNT(*) FROM samples GROUP BY cell",
            "SELECT COUNT(*) FROM samples GROUP BY H3_PARENT(bucket, 16)",
        ] {
            let error = runtime
                .execute_query(sql)
                .expect_err("invalid native H3 resolution");
            assert!(error
                .to_string()
                .contains("H3_PARENT H3 resolution 16 is invalid"));
        }
    }

    #[test]
    fn hidden_aggregate_parameters_and_visible_aliases_do_not_collide() {
        let runtime = runtime_with_table("BIGINT");
        for (bucket, values) in [("red", [10, 10, 20, 90]), ("blue", [10, 10, 50, 60])] {
            for value in values {
                insert(&runtime, bucket, Value::BigInt(value));
            }
        }
        let percentiles = runtime.execute_query("SELECT bucket, COUNT(*) AS n FROM samples GROUP BY bucket HAVING PERCENTILE(value, 0.25) = 10 ORDER BY PERCENTILE(value, 0.75) DESC, bucket ASC LIMIT 1 OFFSET 1").expect("independent hidden percentile outputs");
        assert_eq!(percentiles.result.columns, vec!["bucket", "n"]);
        assert_eq!(percentiles.result.records.len(), 1);
        assert_eq!(
            percentiles.result.records[0].get("bucket"),
            Some(&Value::text("red"))
        );
        assert_eq!(
            number(
                percentiles.result.records[0]
                    .get("n")
                    .expect("visible count")
            ),
            4.0
        );
        let alias = runtime.execute_query(r#"SELECT bucket, COUNT(*) AS "sum(value)" FROM samples GROUP BY bucket HAVING SUM(value) >= 15 ORDER BY SUM(value) DESC, bucket ASC LIMIT 1 OFFSET 1"#).expect("hidden sum must not overwrite visible count alias");
        assert_eq!(alias.result.columns, vec!["bucket", "sum(value)"]);
        assert_eq!(alias.result.records.len(), 1);
        assert_eq!(
            alias.result.records[0].get("bucket"),
            Some(&Value::text("red"))
        );
        assert_eq!(
            number(
                alias.result.records[0]
                    .get("sum(value)")
                    .expect("visible count alias")
            ),
            4.0
        );
    }

    #[test]
    fn shared_result_phase_preserves_empty_aggregates_and_wide_pages() {
        let runtime = runtime_with_table("BIGINT");
        for (bucket, value) in [("red", 10), ("red", 20), ("blue", 5), ("blue", 10)] {
            insert(&runtime, bucket, Value::BigInt(value));
        }
        for limit in [1u64 << 32, (1u64 << 32) + 1, i64::MAX as u64] {
            let sql = format!("SELECT bucket, SUM(value) AS amount FROM samples GROUP BY bucket HAVING MAX(value) > 0 ORDER BY SUM(value) DESC LIMIT {limit}");
            let result = runtime.execute_query(&sql).expect("wide aggregate page");
            assert_eq!(result.result.columns, vec!["bucket", "amount"]);
            assert_eq!(result.result.records.len(), 2);
            assert_eq!(
                result.result.records[0].get("bucket"),
                Some(&Value::text("red"))
            );
        }
        let result = runtime.execute_query("SELECT bucket, SUM(value) AS amount FROM samples GROUP BY bucket HAVING MAX(value) > 0 ORDER BY SUM(value) DESC LIMIT 1 OFFSET 4294967296").expect("wide aggregate offset");
        assert!(result.result.records.is_empty());
        // MEDIAN keeps this outside the basic count/sum pushdown envelope.
        let empty = runtime.execute_query("SELECT COUNT(*) AS n, SUM(value) AS amount, MEDIAN(value) AS middle FROM samples WHERE value > 1000").expect("empty scalar aggregate result");
        assert_eq!(empty.result.records.len(), 1);
        assert_eq!(
            number(empty.result.records[0].get("n").expect("empty count")),
            0.0
        );
        assert_eq!(empty.result.records[0].get("amount"), Some(&Value::Null));
        assert_eq!(empty.result.records[0].get("middle"), Some(&Value::Null));
        let empty = runtime.execute_query("SELECT bucket, COUNT(*) AS n FROM samples WHERE value > 1000 GROUP BY bucket HAVING MAX(value) > 0").expect("empty grouped aggregate result");
        assert!(empty.result.records.is_empty());
    }

    #[test]
    fn shared_planning_reuses_aliases_and_hidden_aggregates() {
        let runtime = runtime_with_table("BIGINT");
        for (bucket, value) in [("red", 10), ("red", 20), ("blue", 5), ("blue", 10)] {
            insert(&runtime, bucket, Value::BigInt(value));
        }
        let visible = runtime.execute_query("SELECT bucket, SUM(value) AS amount FROM samples GROUP BY bucket HAVING SUM(value) >= 10 ORDER BY SUM(value) DESC, bucket ASC").expect("visible aggregate alias references");
        assert_eq!(visible.result.columns, vec!["bucket", "amount"]);
        assert_eq!(visible.result.records.len(), 2);
        for (record, (bucket, amount)) in visible
            .result
            .records
            .iter()
            .zip([("red", 30.0), ("blue", 15.0)])
        {
            assert_eq!(record.get("bucket"), Some(&Value::text(bucket)));
            assert_eq!(
                number(record.get("amount").expect("aggregate alias")),
                amount
            );
        }
        let hidden = runtime.execute_query("SELECT bucket, COUNT(*) AS n FROM samples GROUP BY bucket HAVING SUM(value) >= 10 ORDER BY SUM(value) DESC, bucket ASC LIMIT 1 OFFSET 1").expect("hidden HAVING/ORDER BY aggregate references");
        assert_eq!(hidden.result.columns, vec!["bucket", "n"]);
        assert_eq!(hidden.result.records.len(), 1);
        assert_eq!(
            hidden.result.records[0].get("bucket"),
            Some(&Value::text("blue"))
        );
        assert_eq!(
            number(hidden.result.records[0].get("n").expect("count")),
            2.0
        );
    }

    #[test]
    fn shared_numeric_slots_accumulate_each_row_once() {
        let runtime = runtime_with_table("BIGINT");
        insert(&runtime, "all", Value::BigInt(10));
        insert(&runtime, "all", Value::BigInt(20));
        for select in [
            "SUM(value) AS total, AVG(value) AS mean, VARIANCE(value) AS variance, STDDEV(value) AS deviation",
            "STDDEV(value) AS deviation, VARIANCE(value) AS variance, AVG(value) AS mean, SUM(value) AS total",
        ] {
            let sql = format!("SELECT bucket, {select} FROM samples GROUP BY bucket HAVING COUNT(*) > 0");
            let result = runtime.execute_query(&sql).expect("generic shared aggregate slots");
            assert_eq!(result.result.records.len(), 1);
            let record = &result.result.records[0];
            for (name, expected) in [("total", 30.0), ("mean", 15.0), ("variance", 25.0), ("deviation", 5.0)] {
                assert_eq!(number(record.get(name).expect("aggregate output")), expected);
            }
        }
    }

    #[test]
    fn sum_and_avg_bigint_agree_on_planner_and_legacy_routes() {
        let runtime = runtime_with_table("BIGINT");
        insert(&runtime, "all", Value::BigInt(10));
        insert(&runtime, "all", Value::BigInt(20));

        let expected_sum = vec![("all".to_string(), 30.0)];
        let expected_avg = vec![("all".to_string(), 15.0)];
        assert_eq!(aggregate_rows(&runtime, PLANNER_SUM_QUERY), expected_sum);
        assert_eq!(aggregate_rows(&runtime, LEGACY_SUM_QUERY), expected_sum);
        assert_eq!(aggregate_rows(&runtime, PLANNER_AVG_QUERY), expected_avg);
        assert_eq!(aggregate_rows(&runtime, LEGACY_AVG_QUERY), expected_avg);
    }

    #[test]
    fn sum_boolean_counts_true_values_on_planner_and_legacy_routes() {
        let runtime = runtime_with_table("BOOLEAN");
        insert(&runtime, "all", Value::Boolean(true));
        insert(&runtime, "all", Value::Boolean(false));
        insert(&runtime, "all", Value::Boolean(true));

        let planner = aggregate_rows(&runtime, PLANNER_SUM_QUERY);
        let legacy = aggregate_rows(&runtime, LEGACY_SUM_QUERY);
        assert_eq!(planner[0].1, 2.0);
        assert_eq!(legacy[0].1, 2.0);
    }

    #[test]
    fn nonfinite_float_propagates_on_planner_and_legacy_routes() {
        let runtime = runtime_with_table("FLOAT");
        insert(&runtime, "infinity", Value::Float(1.0));
        insert(&runtime, "infinity", Value::Float(f64::INFINITY));
        insert(&runtime, "nan", Value::Float(1.0));
        insert(&runtime, "nan", Value::Float(f64::NAN));

        for query in [PLANNER_SUM_QUERY, LEGACY_SUM_QUERY] {
            let rows = aggregate_rows(&runtime, query);
            assert_eq!(rows[0].0, "infinity");
            assert_eq!(rows[0].1, f64::INFINITY);
            assert_eq!(rows[1].0, "nan");
            assert!(rows[1].1.is_nan());
        }
    }
}

/// Issue #769 (S10) — `stream.executor.max_materialized_rows` ceiling
/// on the aggregating executors. These drive real SQL through the
/// runtime so they cover whichever aggregation sub-path (push-down,
/// parallel, generic+spill) the planner picks for the shape.
#[cfg(test)]
mod materialization_limit_tests {
    use crate::api::RedDBError;
    use crate::{RedDBOptions, RedDBRuntime};

    fn mk_runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory())
            .expect("in-memory runtime should open")
    }

    /// Seed `distinct` rows, each with a unique city, so a `GROUP BY
    /// city` materializes exactly `distinct` groups and a `SELECT *`
    /// materializes exactly `distinct` rows.
    fn seed_distinct(rt: &RedDBRuntime, distinct: usize) {
        rt.execute_query("CREATE TABLE big_table (id INT, name TEXT, city TEXT)")
            .unwrap();
        for i in 0..distinct {
            rt.execute_query(&format!(
                "INSERT INTO big_table (id, name, city) VALUES ({i}, 'n{i}', 'c{i}')"
            ))
            .unwrap();
        }
    }

    fn set_limit(rt: &RedDBRuntime, limit: u64) {
        rt.execute_query(&format!(
            "SET CONFIG stream.executor.max_materialized_rows = {limit}"
        ))
        .expect("set config ok");
    }

    #[test]
    fn aggregation_over_limit_terminates_with_named_error() {
        let rt = mk_runtime();
        seed_distinct(&rt, 25);
        set_limit(&rt, 10);
        let err = rt
            .execute_query("SELECT COUNT(*), city FROM big_table GROUP BY city")
            .expect_err("aggregation past the ceiling must terminate");
        match err {
            RedDBError::MaterializationLimitExceeded {
                executor,
                limit,
                current,
            } => {
                assert_eq!(executor, "aggregation");
                assert_eq!(limit, 10);
                assert!(current > 10, "current ({current}) should exceed the limit");
            }
            other => panic!("expected MaterializationLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn sort_over_limit_terminates_with_named_error() {
        let rt = mk_runtime();
        seed_distinct(&rt, 25);
        set_limit(&rt, 10);
        let err = rt
            .execute_query("SELECT * FROM big_table ORDER BY name")
            .expect_err("sort past the ceiling must terminate");
        match err {
            RedDBError::MaterializationLimitExceeded {
                executor,
                limit,
                current,
            } => {
                assert_eq!(executor, "sort");
                assert_eq!(limit, 10);
                assert!(current > 10, "current ({current}) should exceed the limit");
            }
            other => panic!("expected MaterializationLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn aggregation_within_limit_returns_full_result() {
        let rt = mk_runtime();
        seed_distinct(&rt, 25);
        set_limit(&rt, 1000);
        let result = rt
            .execute_query("SELECT COUNT(*), city FROM big_table GROUP BY city")
            .expect("within-limit aggregation should succeed");
        assert_eq!(
            result.result.records.len(),
            25,
            "every distinct group should be present when under the ceiling"
        );
    }

    #[test]
    fn sort_within_limit_returns_full_result() {
        let rt = mk_runtime();
        seed_distinct(&rt, 25);
        set_limit(&rt, 1000);
        let result = rt
            .execute_query("SELECT * FROM big_table ORDER BY name")
            .expect("within-limit sort should succeed");
        assert_eq!(result.result.records.len(), 25);
    }

    #[test]
    fn default_ceiling_leaves_small_queries_untouched() {
        // No SET CONFIG — the 10⁶ default must not perturb ordinary
        // queries (acceptance: within-limit results identical).
        let rt = mk_runtime();
        seed_distinct(&rt, 25);
        let agg = rt
            .execute_query("SELECT COUNT(*), city FROM big_table GROUP BY city")
            .expect("group by under default ceiling");
        assert_eq!(agg.result.records.len(), 25);
        let sorted = rt
            .execute_query("SELECT * FROM big_table ORDER BY name")
            .expect("sort under default ceiling");
        assert_eq!(sorted.result.records.len(), 25);
    }

    #[test]
    fn window_over_limit_terminates_with_named_error() {
        let rt = mk_runtime();
        seed_distinct(&rt, 25);
        set_limit(&rt, 10);
        let err = rt
            .execute_query("SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM big_table")
            .expect_err("window past the ceiling must terminate");
        match err {
            RedDBError::MaterializationLimitExceeded {
                executor,
                limit,
                current,
            } => {
                assert_eq!(executor, "window");
                assert_eq!(limit, 10);
                assert!(current > 10, "current ({current}) should exceed the limit");
            }
            other => panic!("expected MaterializationLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn window_within_limit_returns_full_result() {
        let rt = mk_runtime();
        seed_distinct(&rt, 25);
        set_limit(&rt, 1000);
        let result = rt
            .execute_query("SELECT id, ROW_NUMBER() OVER (ORDER BY id) AS rn FROM big_table")
            .expect("within-limit window should succeed");
        assert_eq!(result.result.records.len(), 25);
    }

    #[test]
    fn config_hot_reload_affects_new_queries_only() {
        let rt = mk_runtime();
        seed_distinct(&rt, 25);

        // Tight ceiling — the GROUP BY now blows it.
        set_limit(&rt, 5);
        assert!(matches!(
            rt.execute_query("SELECT COUNT(*), city FROM big_table GROUP BY city"),
            Err(RedDBError::MaterializationLimitExceeded { .. })
        ));

        // Raise it — the very next query reads the fresh value and
        // succeeds. The limit is consulted per query, so the change
        // takes effect for new queries without a restart.
        set_limit(&rt, 1000);
        let after = rt
            .execute_query("SELECT COUNT(*), city FROM big_table GROUP BY city")
            .expect("raised ceiling should let the same query through");
        assert_eq!(after.result.records.len(), 25);
    }
}

#[cfg(test)]
mod shared_kernel_spill_tests {
    use super::*;
    use crate::storage::query::executors::agg_spill::{Mergeable, SpillCodec};

    #[test]
    fn shared_state_round_trips_the_native_spill_codec_and_merges() {
        let projections = [
            "SUM",
            "AVG",
            "VARIANCE",
            "STDDEV",
            "FIRST",
            "LAST",
            "ARRAY_AGG",
            "COUNT_DISTINCT",
        ]
        .map(|name| Projection::Function(name.into(), vec![Projection::Column("value".into())]));
        let plan = CompiledAggPlan::compile(&projections).expect("bounded spill plan");
        let group = |values: &[Value]| {
            let mut state = SlottedAggState::new(&plan);
            for value in values {
                state.count += 1;
                for slot in &plan.proj_slots {
                    state.accumulate(&NativeAggregateHost, slot, value.clone());
                }
            }
            AggregateGroup {
                group_values: vec![Value::text("fixture")],
                state,
            }
        };
        let source = [
            Value::Null,
            Value::Integer(10),
            Value::Integer(20),
            Value::Integer(30),
        ];
        let expected = group(&source);
        let first = group(&source[..2]);
        let mut encoded = Vec::new();
        first.encode(&mut encoded).expect("native spill encode");
        let mut input = encoded.as_slice();
        let mut decoded = AggregateGroup::decode(&mut input)
            .expect("native spill decode")
            .expect("one native group");
        assert!(input.is_empty(), "codec consumes the complete native group");
        decoded.merge(group(&source[2..]));
        assert_eq!(decoded.group_values, expected.group_values);
        assert_eq!(decoded.state.count, expected.state.count);
        for projection in &projections {
            assert_eq!(
                reddb_engine::query::runtime::aggregation::aggregate_projection_result_with_host(
                    &NativeAggregateHost,
                    projection,
                    &decoded.state,
                    &plan
                ),
                reddb_engine::query::runtime::aggregation::aggregate_projection_result_with_host(
                    &NativeAggregateHost,
                    projection,
                    &expected.state,
                    &plan
                )
            );
        }
    }
}
