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
use crate::storage::query::ast::{FieldRef, Projection};
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
        for proj in &query.columns {
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

        // Add aggregate results
        for proj in &query.columns {
            if let Projection::Function(func, args) = proj {
                let func_name = base_function_name(func);
                if !is_aggregate_function(func_name) {
                    continue;
                }

                let col_name = match aggregate_argument_key(args) {
                    Some(col_name) => col_name,
                    None => continue,
                };
                let result_name = aggregate_output_name(proj, func_name, &col_name);

                if !columns.contains(&result_name) {
                    columns.push(result_name.clone());
                }

                let result_val = match func_name {
                    "COUNT" => {
                        if col_name == "*" {
                            Value::Integer(group.state.count as i64)
                        } else {
                            Value::Integer(
                                group.state.agg_counts.get(&col_name).copied().unwrap_or(0) as i64,
                            )
                        }
                    }
                    "SUM" => match group.state.sums.get(&col_name).copied() {
                        Some(s) => Value::Float(s),
                        None => Value::Null,
                    },
                    "AVG" => {
                        let s = group.state.sums.get(&col_name).copied().unwrap_or(0.0);
                        let n = group.state.agg_counts.get(&col_name).copied().unwrap_or(0);
                        if n > 0 {
                            Value::Float(s / n as f64)
                        } else {
                            Value::Null
                        }
                    }
                    "MIN" => group
                        .state
                        .mins
                        .get(&col_name)
                        .cloned()
                        .unwrap_or(Value::Null),
                    "MAX" => group
                        .state
                        .maxs
                        .get(&col_name)
                        .cloned()
                        .unwrap_or(Value::Null),
                    "VARIANCE" => {
                        let n = group.state.agg_counts.get(&col_name).copied().unwrap_or(0) as f64;
                        if n > 0.0 {
                            let sum = group.state.sums.get(&col_name).copied().unwrap_or(0.0);
                            let sum_sq = group
                                .state
                                .sum_squares
                                .get(&col_name)
                                .copied()
                                .unwrap_or(0.0);
                            Value::Float(sum_sq / n - (sum / n).powi(2))
                        } else {
                            Value::Null
                        }
                    }
                    "STDDEV" => {
                        let n = group.state.agg_counts.get(&col_name).copied().unwrap_or(0) as f64;
                        if n > 0.0 {
                            let sum = group.state.sums.get(&col_name).copied().unwrap_or(0.0);
                            let sum_sq = group
                                .state
                                .sum_squares
                                .get(&col_name)
                                .copied()
                                .unwrap_or(0.0);
                            let variance = sum_sq / n - (sum / n).powi(2);
                            Value::Float(variance.max(0.0).sqrt())
                        } else {
                            Value::Null
                        }
                    }
                    "MEDIAN" => {
                        let mut vals = group
                            .state
                            .all_values
                            .get(&col_name)
                            .cloned()
                            .unwrap_or_default();
                        if vals.is_empty() {
                            Value::Null
                        } else {
                            vals.sort_by(|a, b| {
                                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            let mid = vals.len() / 2;
                            if vals.len() % 2 == 0 {
                                Value::Float((vals[mid - 1] + vals[mid]) / 2.0)
                            } else {
                                Value::Float(vals[mid])
                            }
                        }
                    }
                    "PERCENTILE" => {
                        let pct = resolve_static_projection_number(args.get(1))
                            .unwrap_or(0.5)
                            .clamp(0.0, 1.0);
                        let mut vals = group
                            .state
                            .all_values
                            .get(&col_name)
                            .cloned()
                            .unwrap_or_default();
                        if vals.is_empty() {
                            Value::Null
                        } else {
                            vals.sort_by(|a, b| {
                                a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                            });
                            let idx = ((pct * (vals.len() as f64 - 1.0)).round() as usize)
                                .min(vals.len() - 1);
                            Value::Float(vals[idx])
                        }
                    }
                    "GROUP_CONCAT" | "STRING_AGG" => {
                        let vals = group
                            .state
                            .concat_values
                            .get(&col_name)
                            .cloned()
                            .unwrap_or_default();
                        if vals.is_empty() {
                            Value::Null
                        } else {
                            let separator = resolve_static_projection_text(args.get(1))
                                .unwrap_or_else(|| ", ".to_string());
                            Value::Text(vals.join(separator.as_str()))
                        }
                    }
                    "FIRST" => group
                        .state
                        .first_values
                        .get(&col_name)
                        .cloned()
                        .unwrap_or(Value::Null),
                    "LAST" => group
                        .state
                        .last_values
                        .get(&col_name)
                        .cloned()
                        .unwrap_or(Value::Null),
                    "ARRAY_AGG" => {
                        let vals = group
                            .state
                            .array_values
                            .get(&col_name)
                            .cloned()
                            .unwrap_or_default();
                        if vals.is_empty() {
                            Value::Null
                        } else {
                            Value::Array(vals)
                        }
                    }
                    "COUNT_DISTINCT" => {
                        let set = group
                            .state
                            .distinct_sets
                            .get(&col_name)
                            .map(|s| s.len())
                            .unwrap_or(0);
                        Value::Integer(set as i64)
                    }
                    _ => Value::Null,
                };
                record.set(&result_name, result_val);
            }
        }

        if having_matches(query.having.as_ref(), &record) {
            records.push(record);
        }
    }

    // If no input rows matched, return a single aggregate row.
    if groups.is_empty() && !has_group_by {
        let mut record = UnifiedRecord::new();
        for proj in &query.columns {
            if let Projection::Function(func, args) = proj {
                let func_name = base_function_name(func);
                if !is_aggregate_function(func_name) {
                    continue;
                }
                let col_name = match aggregate_argument_key(args) {
                    Some(col_name) => col_name,
                    None => continue,
                };
                let name = aggregate_output_name(proj, func_name, &col_name);
                if !columns.contains(&name) {
                    columns.push(name.clone());
                }
                record.set(
                    &name,
                    match func_name {
                        "COUNT" | "COUNT_DISTINCT" => Value::Integer(0),
                        _ => Value::Null,
                    },
                );
            }
        }
        if having_matches(query.having.as_ref(), &record) {
            records.push(record);
        }
    }

    if !query.order_by.is_empty() {
        sort_records_by_order_by(&mut records, &query.order_by, None, None);
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
                query.group_by
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
