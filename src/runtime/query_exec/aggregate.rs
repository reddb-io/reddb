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

use crate::api::{RedDBError, RedDBResult};
use crate::runtime::join_filter::{
    eval_projection_value, evaluate_runtime_filter, field_ref_name, projection_name,
    runtime_partial_cmp, sort_records_by_order_by,
};
use crate::runtime::runtime_table_record_from_entity;
use crate::storage::query::ast::{
    BinOp, CompareOp, Expr, FieldRef, Filter, OrderByClause, Projection, Span, UnaryOp,
};
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;
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
    let runtime_plan = prepare_aggregate_runtime_plan(query);
    let mut all_aggregate_projections = query
        .columns
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

    let filter = query.filter.as_ref();
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let has_group_by = !query.group_by.is_empty();

    // Compile the filter ONCE before the for_each_entity loop. The
    // compiled form pre-classifies every FieldRef into an
    // EntityFieldKind, so the per-row evaluator skips the ~6
    // system-field string compares + entity-kind cascade that
    // evaluate_entity_filter performs on every call.
    //
    // See `runtime/query_exec/filter_compiled.rs` for the algorithm
    // and `runtime/query_exec/table.rs` for the canonical scan-path
    // wire-up.
    let compiled_filter = filter
        .map(|f| super::filter_compiled::CompiledEntityFilter::compile(f, table_name, table_alias));

    // Work-mem cap: prevent OOM on runaway GROUP BY aggregations.
    // 64 MB default mirrors PostgreSQL's work_mem GUC default.
    // When exceeded the query fails cleanly instead of OOMing the process.
    // TODO(A8-full): replace with SpilledHashAgg drain when the full
    // agg_spill serialization layer is wired in.
    const WORK_MEM_BYTES: usize = 64 * 1024 * 1024;
    // Approximate per-entry cost: 128 B for AggState + 64 B for group key + HashMap overhead
    const ESTIMATED_ENTRY_BYTES: usize = 256;
    let max_groups = WORK_MEM_BYTES / ESTIMATED_ENTRY_BYTES; // ~256k groups

    // Accumulators per group (empty string key = no grouping)
    let mut groups: std::collections::HashMap<String, AggregateGroup> =
        std::collections::HashMap::new();
    let mut work_mem_exceeded = false;

    manager.for_each_entity(|entity| {
        if let Some(c) = compiled_filter.as_ref() {
            if !c.evaluate(entity) {
                return true;
            }
        }

        let record = match runtime_table_record_from_entity(entity.clone()) {
            Some(record) => record,
            None => return true,
        };

        let group_values = if has_group_by {
            let mut values = Vec::with_capacity(query.group_by.len());
            for group_expr in &query.group_by {
                let Some(value) = resolve_group_by_value(group_expr, &record) else {
                    return true;
                };
                values.push(value);
            }
            values
        } else {
            Vec::new()
        };
        // Build the group-by key in a single String buffer instead
        // of `iter().map().collect::<Vec<_>>().join("|")`, which used
        // to pay N+1 String allocations per row. See sibling
        // `aggregation.rs::make_group_key` for the same optimisation
        // on the executor path.
        let group_key = if has_group_by {
            let mut key = String::with_capacity(64);
            for (i, v) in group_values.iter().enumerate() {
                if i > 0 {
                    key.push('|');
                }
                append_group_value_key(&mut key, v);
            }
            key
        } else {
            String::new()
        };

        // Work-mem guard: if we'd exceed the cap on a new group key, stop.
        // Existing group keys are always allowed (they don't grow the map).
        if !groups.contains_key(&group_key) && groups.len() >= max_groups {
            work_mem_exceeded = true;
            return false; // stop iteration
        }

        let group = groups.entry(group_key).or_insert_with(|| AggregateGroup {
            group_values: group_values.clone(),
            state: AggState::default(),
        });
        let state = &mut group.state;
        state.count += 1;

        // Accumulate values for each aggregate projection
        for proj in &all_aggregate_projections {
            if let Projection::Function(func, args) = proj {
                let func_name = base_function_name(func);
                if !is_aggregate_function(func_name) {
                    continue;
                }

                let col_name = match aggregate_argument_key(args) {
                    Some(col) => col,
                    None => continue,
                };
                if func_name == "COUNT" && col_name == "*" {
                    continue;
                }

                let val = match resolve_aggregate_argument_value(args.first(), &record) {
                    Some(v) => v,
                    None => continue,
                };
                let num = value_to_f64(&val);

                match func_name {
                    "COUNT" => {
                        if !matches!(val, Value::Null) {
                            *state.agg_counts.entry(col_name.clone()).or_insert(0) += 1;
                        }
                    }
                    "SUM" | "AVG" => {
                        if let Some(n) = num {
                            *state.sums.entry(col_name.clone()).or_insert(0.0) += n;
                            *state.agg_counts.entry(col_name.clone()).or_insert(0) += 1;
                        }
                    }
                    "MIN" => {
                        update_extreme_value(
                            &mut state.mins,
                            &col_name,
                            &val,
                            std::cmp::Ordering::Less,
                        );
                    }
                    "MAX" => {
                        update_extreme_value(
                            &mut state.maxs,
                            &col_name,
                            &val,
                            std::cmp::Ordering::Greater,
                        );
                    }
                    "STDDEV" | "VARIANCE" => {
                        if let Some(n) = num {
                            *state.sums.entry(col_name.clone()).or_insert(0.0) += n;
                            *state.sum_squares.entry(col_name.clone()).or_insert(0.0) += n * n;
                            *state.agg_counts.entry(col_name.clone()).or_insert(0) += 1;
                        }
                    }
                    "MEDIAN" | "PERCENTILE" => {
                        if let Some(n) = num {
                            state
                                .all_values
                                .entry(col_name.clone())
                                .or_default()
                                .push(n);
                        }
                    }
                    "GROUP_CONCAT" | "STRING_AGG" => {
                        if !matches!(val, Value::Null) {
                            let text = match &val {
                                Value::Text(s) => s.clone(),
                                other => other.display_string(),
                            };
                            state
                                .concat_values
                                .entry(col_name.clone())
                                .or_default()
                                .push(text);
                        }
                    }
                    "FIRST" => {
                        state
                            .first_values
                            .entry(col_name.clone())
                            .or_insert_with(|| val.clone());
                    }
                    "LAST" => {
                        state.last_values.insert(col_name.clone(), val.clone());
                    }
                    "ARRAY_AGG" => {
                        state
                            .array_values
                            .entry(col_name.clone())
                            .or_default()
                            .push(val.clone());
                    }
                    "COUNT_DISTINCT" => {
                        if !matches!(val, Value::Null) {
                            state
                                .distinct_sets
                                .entry(col_name.clone())
                                .or_default()
                                .insert(group_value_key(&val));
                        }
                    }
                    _ => {}
                }
            }
        }
        true
    });

    // Work-mem exceeded: return an informative error rather than silently
    // producing a partial or wrong result. Full spill-to-disk lives in
    // agg_spill.rs and wires in when the Codec/Mergeable traits are impl'd
    // for AggregateGroup (A8-full).
    if work_mem_exceeded {
        return Err(RedDBError::Query(format!(
            "GROUP BY aggregation exceeded work_mem ({} MB, ~{} groups). \
             Reduce cardinality or increase work_mem.",
            WORK_MEM_BYTES / (1024 * 1024),
            max_groups,
        )));
    }

    // Build result records from accumulated groups
    let mut records = Vec::with_capacity(groups.len().max(1));
    let mut columns = Vec::new();

    for group in groups.values() {
        let mut record = UnifiedRecord::new();

        // Add GROUP BY columns
        if has_group_by {
            for (index, group_expr) in query.group_by.iter().enumerate() {
                let Some(value) = group.group_values.get(index).cloned() else {
                    continue;
                };
                let label = group_output_label(query, group_expr);
                if !columns.contains(&label) {
                    columns.push(label.clone());
                }
                record.set(group_expr, value.clone());
                record.set(&label, value);
            }
        }

        // Add visible aggregate results
        for proj in &query.columns {
            if let Some((result_name, result_val)) = aggregate_projection_result(proj, &group.state)
            {
                if !columns.contains(&result_name) {
                    columns.push(result_name.clone());
                }
                record.set(&result_name, result_val);
            }
        }

        for proj in &runtime_plan.hidden_aggregates {
            if let Some((result_name, result_val)) = aggregate_projection_result(proj, &group.state)
            {
                record.set(&result_name, result_val);
            }
        }

        if having_matches(runtime_plan.having.as_ref(), &record) {
            records.push(record);
        }
    }

    // If no input rows matched, return a single aggregate row.
    if groups.is_empty() && !has_group_by {
        let mut record = UnifiedRecord::new();
        for proj in &query.columns {
            if let Some((result_name, result_val)) =
                empty_aggregate_projection_result(proj, &AggState::default())
            {
                if !columns.contains(&result_name) {
                    columns.push(result_name.clone());
                }
                record.set(&result_name, result_val);
            }
        }
        for proj in &runtime_plan.hidden_aggregates {
            if let Some((result_name, result_val)) =
                empty_aggregate_projection_result(proj, &AggState::default())
            {
                record.set(&result_name, result_val);
            }
        }
        if having_matches(runtime_plan.having.as_ref(), &record) {
            records.push(record);
        }
    }

    if !runtime_plan.order_by.is_empty() {
        sort_records_by_order_by(&mut records, &runtime_plan.order_by, None, None);
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
    let visible_outputs = query
        .columns
        .iter()
        .filter_map(visible_aggregate_output_name)
        .collect::<std::collections::HashMap<_, _>>();
    let mut hidden = HiddenAggregateRegistry::default();

    let having = query
        .having
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
        Value::Text(value) => value.clone(),
        other => other.display_string(),
    }
}

fn projection_from_expr(expr: &Expr) -> Option<Projection> {
    match expr {
        Expr::Literal { value, .. } => projection_from_literal(value),
        Expr::Column { field, .. } => {
            if matches!(
                field,
                FieldRef::TableColumn { table, column } if table.is_empty() && column == "*"
            ) {
                Some(Projection::All)
            } else {
                Some(Projection::Field(field.clone(), None))
            }
        }
        Expr::Parameter { .. } => None,
        Expr::BinaryOp { op, lhs, rhs, .. } => match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Concat => {
                Some(Projection::Function(
                    render_binop_signature_name(*op).to_string(),
                    vec![projection_from_expr(lhs)?, projection_from_expr(rhs)?],
                ))
            }
            _ => Some(boolean_expr_projection(expr.clone())),
        },
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Neg => Some(Projection::Function(
                "SUB".to_string(),
                vec![
                    Projection::Column("LIT:0".to_string()),
                    projection_from_expr(operand)?,
                ],
            )),
            UnaryOp::Not => Some(boolean_expr_projection(expr.clone())),
        },
        Expr::Cast { inner, target, .. } => Some(Projection::Function(
            "CAST".to_string(),
            vec![
                projection_from_expr(inner)?,
                Projection::Column(format!("TYPE:{target}")),
            ],
        )),
        Expr::FunctionCall { name, args, .. } => Some(Projection::Function(
            name.to_uppercase(),
            args.iter()
                .map(projection_from_expr)
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::Case {
            branches, else_, ..
        } => {
            let mut args = Vec::with_capacity(branches.len() * 2 + usize::from(else_.is_some()));
            for (cond, value) in branches {
                args.push(case_condition_projection(cond.clone()));
                args.push(projection_from_expr(value)?);
            }
            if let Some(else_expr) = else_ {
                args.push(projection_from_expr(else_expr)?);
            }
            Some(Projection::Function("CASE".to_string(), args))
        }
        Expr::IsNull { .. } | Expr::InList { .. } | Expr::Between { .. } => {
            Some(boolean_expr_projection(expr.clone()))
        }
    }
}

fn projection_from_literal(value: &Value) -> Option<Projection> {
    match value {
        Value::Boolean(_) => Some(boolean_expr_projection(Expr::Literal {
            value: value.clone(),
            span: Span::synthetic(),
        })),
        _ => Some(Projection::Column(format!(
            "LIT:{}",
            render_value_signature(value)
        ))),
    }
}

fn boolean_expr_projection(expr: Expr) -> Projection {
    Projection::Expression(
        Box::new(Filter::CompareExpr {
            lhs: expr,
            op: CompareOp::Eq,
            rhs: Expr::Literal {
                value: Value::Boolean(true),
                span: Span::synthetic(),
            },
        }),
        None,
    )
}

fn case_condition_projection(condition: Expr) -> Projection {
    Projection::Expression(
        Box::new(Filter::CompareExpr {
            lhs: condition,
            op: CompareOp::Eq,
            rhs: Expr::Literal {
                value: Value::Boolean(true),
                span: Span::synthetic(),
            },
        }),
        None,
    )
}

fn aggregate_projection_result(
    projection: &Projection,
    state: &AggState,
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
                Value::Integer(state.agg_counts.get(&col_name).copied().unwrap_or(0) as i64)
            }
        }
        "SUM" => state
            .sums
            .get(&col_name)
            .copied()
            .map(Value::Float)
            .unwrap_or(Value::Null),
        "AVG" => {
            let sum = state.sums.get(&col_name).copied().unwrap_or(0.0);
            let count = state.agg_counts.get(&col_name).copied().unwrap_or(0);
            if count > 0 {
                Value::Float(sum / count as f64)
            } else {
                Value::Null
            }
        }
        "MIN" => state.mins.get(&col_name).cloned().unwrap_or(Value::Null),
        "MAX" => state.maxs.get(&col_name).cloned().unwrap_or(Value::Null),
        "VARIANCE" => {
            let n = state.agg_counts.get(&col_name).copied().unwrap_or(0) as f64;
            if n > 0.0 {
                let sum = state.sums.get(&col_name).copied().unwrap_or(0.0);
                let sum_sq = state.sum_squares.get(&col_name).copied().unwrap_or(0.0);
                Value::Float(sum_sq / n - (sum / n).powi(2))
            } else {
                Value::Null
            }
        }
        "STDDEV" => {
            let n = state.agg_counts.get(&col_name).copied().unwrap_or(0) as f64;
            if n > 0.0 {
                let sum = state.sums.get(&col_name).copied().unwrap_or(0.0);
                let sum_sq = state.sum_squares.get(&col_name).copied().unwrap_or(0.0);
                let variance = sum_sq / n - (sum / n).powi(2);
                Value::Float(variance.max(0.0).sqrt())
            } else {
                Value::Null
            }
        }
        "MEDIAN" => {
            let mut values = state.all_values.get(&col_name).cloned().unwrap_or_default();
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
            let mut values = state.all_values.get(&col_name).cloned().unwrap_or_default();
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
            let values = state
                .concat_values
                .get(&col_name)
                .cloned()
                .unwrap_or_default();
            if values.is_empty() {
                Value::Null
            } else {
                let separator =
                    resolve_static_projection_text(args.get(1)).unwrap_or_else(|| ", ".to_string());
                Value::Text(values.join(separator.as_str()))
            }
        }
        "FIRST" => state
            .first_values
            .get(&col_name)
            .cloned()
            .unwrap_or(Value::Null),
        "LAST" => state
            .last_values
            .get(&col_name)
            .cloned()
            .unwrap_or(Value::Null),
        "ARRAY_AGG" => {
            let values = state
                .array_values
                .get(&col_name)
                .cloned()
                .unwrap_or_default();
            if values.is_empty() {
                Value::Null
            } else {
                Value::Array(values)
            }
        }
        "COUNT_DISTINCT" => Value::Integer(
            state
                .distinct_sets
                .get(&col_name)
                .map(|set| set.len())
                .unwrap_or(0) as i64,
        ),
        _ => Value::Null,
    };

    Some((result_name, result_value))
}

fn empty_aggregate_projection_result(
    projection: &Projection,
    state: &AggState,
) -> Option<(String, Value)> {
    aggregate_projection_result(projection, state)
}

fn aggregate_argument_key(args: &[Projection]) -> Option<String> {
    args.first().map(render_aggregate_argument_key)
}

fn having_matches(
    having: Option<&crate::storage::query::ast::Filter>,
    record: &UnifiedRecord,
) -> bool {
    match having {
        Some(filter) => evaluate_runtime_filter(record, filter, None, None),
        None => true,
    }
}

fn resolve_aggregate_argument_value(
    arg: Option<&Projection>,
    record: &UnifiedRecord,
) -> Option<Value> {
    match arg {
        Some(Projection::All) => None,
        Some(arg) => eval_projection_value(arg, record),
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
    let has_group_by = !query.group_by.is_empty();

    for projection in &query.columns {
        if matches!(
            projection,
            Projection::Function(name, _)
                if is_aggregate_function(base_function_name(name))
        ) {
            continue;
        }

        if has_group_by
            && projection_group_key(projection).is_some_and(|group_key| {
                query
                    .group_by
                    .iter()
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
    let value = eval_projection_value(arg?, &record)?;
    value_to_f64(&value)
}

fn resolve_static_projection_text(arg: Option<&Projection>) -> Option<String> {
    let record = UnifiedRecord::new();
    let value = eval_projection_value(arg?, &record)?;
    Some(match value {
        Value::Null => String::new(),
        Value::Text(text) => text,
        other => other.display_string(),
    })
}

fn update_extreme_value(
    map: &mut std::collections::HashMap<String, Value>,
    key: &str,
    candidate: &Value,
    ordering: std::cmp::Ordering,
) {
    if matches!(candidate, Value::Null) {
        return;
    }

    match map.get_mut(key) {
        Some(current) => {
            if runtime_partial_cmp(candidate, current).is_some_and(|ord| ord == ordering) {
                *current = candidate.clone();
            }
        }
        None => {
            map.insert(key.to_string(), candidate.clone());
        }
    }
}

fn group_output_label(query: &TableQuery, group_expr: &str) -> String {
    query
        .columns
        .iter()
        .find_map(|projection| {
            let key = projection_group_key(projection)?;
            if key.eq_ignore_ascii_case(group_expr) {
                Some(projection_name(projection))
            } else {
                None
            }
        })
        .unwrap_or_else(|| group_expr.to_string())
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

fn resolve_group_by_value(group_expr: &str, record: &UnifiedRecord) -> Option<Value> {
    if let Some((bucket_ns, timestamp_column)) = parse_time_bucket_group_expr(group_expr) {
        let timestamp_ns = resolve_bucket_timestamp_ns(record, timestamp_column.as_deref())?;
        let bucket_start = if bucket_ns == 0 {
            timestamp_ns
        } else {
            (timestamp_ns / bucket_ns) * bucket_ns
        };
        return Some(Value::UnsignedInteger(bucket_start));
    }

    record.get(group_expr).cloned()
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

/// Append a single group-by `Value` to a shared key buffer.
///
/// **Hot path** — called once per group-by column per row in
/// `execute_aggregate_query`. Writes directly into the caller's
/// `String` buffer to avoid the per-value `format!` allocation
/// the previous `group_value_key` paid.
fn append_group_value_key(buf: &mut String, value: &Value) {
    use std::fmt::Write;
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
}

#[allow(dead_code)]
fn group_value_key(value: &Value) -> String {
    let mut buf = String::with_capacity(32);
    append_group_value_key(&mut buf, value);
    buf
}

#[derive(Default)]
struct AggregateGroup {
    group_values: Vec<Value>,
    state: AggState,
}

#[derive(Default)]
struct AggState {
    count: u64,
    sums: std::collections::HashMap<String, f64>,
    mins: std::collections::HashMap<String, Value>,
    maxs: std::collections::HashMap<String, Value>,
    // For STDDEV/VARIANCE: collect sum of squares
    sum_squares: std::collections::HashMap<String, f64>,
    agg_counts: std::collections::HashMap<String, u64>,
    // For MEDIAN/PERCENTILE: collect all values
    all_values: std::collections::HashMap<String, Vec<f64>>,
    // For GROUP_CONCAT: collect strings
    concat_values: std::collections::HashMap<String, Vec<String>>,
    // For FIRST/LAST: track first and last seen values
    first_values: std::collections::HashMap<String, Value>,
    last_values: std::collections::HashMap<String, Value>,
    // For ARRAY_AGG: collect all values
    array_values: std::collections::HashMap<String, Vec<Value>>,
    // For COUNT(DISTINCT): collect unique values
    distinct_sets: std::collections::HashMap<String, std::collections::HashSet<String>>,
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
