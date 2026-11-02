//! Canonical window phase over admitted records, preserving native Value variants.
//! Hosts retain expression evaluation, materialization admission and secret output policy.
use std::cmp::Ordering;
use std::collections::HashMap;

use crate::query::unified::UnifiedRecord;
use reddb_rql::ast::{Projection, WindowFrame, WindowFrameBound, WindowFrameUnit, WindowSpec};
use reddb_rql::sql_lowering::projection_to_expr;
use reddb_types::{value_to_canonical_key, CanonicalKey, Value};

use super::projection::{projection_name, projection_uses_secret};

/// Inputs must already have passed parser/AST admission and row visibility checks.
/// Storage-backed hosts override the fallible evaluator; existing optional hosts
/// retain their typed/legacy expression policy through the default adapter.
pub trait WindowHost {
    type Error;
    fn guard_materialization(&self, rows: usize) -> Result<(), Self::Error>;
    fn evaluate_expression(
        &self,
        expression: &reddb_rql::ast::Expr,
        source: &UnifiedRecord,
    ) -> Option<Value>;
    fn try_evaluate_expression(
        &self,
        expression: &reddb_rql::ast::Expr,
        source: &UnifiedRecord,
    ) -> Result<Option<Value>, Self::Error> {
        Ok(self.evaluate_expression(expression, source))
    }
    fn secret_query_output(&self, value: Value) -> Result<Value, Self::Error>;
    fn query_error(&self, message: String) -> Self::Error;
}

/// Evaluate every `Projection::Window` in `projections` and write
/// the result back onto each record under the projection's output
/// label. After this call, the projection node can resolve window
/// outputs via `record.get(label)` exactly like any other column.
pub fn apply_with_host<H: WindowHost>(
    host: &H,
    records: &mut [UnifiedRecord],
    projections: &[Projection],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Result<(), H::Error> {
    if records.is_empty() {
        return Ok(());
    }
    let has_window = projections
        .iter()
        .any(|p| matches!(p, Projection::Window { .. }));
    // Issue #769 — window functions buffer every row to partition and
    // rank them, so the row set must be materialized in full. Cap it
    // before doing the per-partition work. Only relevant when a window
    // projection is actually present; plain projections route through
    // here untouched.
    if has_window {
        host.guard_materialization(records.len())?;
    }
    for projection in projections {
        let Projection::Window {
            name, args, window, ..
        } = projection
        else {
            continue;
        };
        if !window_projection_depth_allowed(projection) {
            return Err(host.query_error("window projection exceeds nesting limit".into()));
        }
        let label = projection_name(projection);
        // A window result may depend on another row (LAG/LEAD or a frame
        // aggregate). A NULL input on the destination row does not make
        // the value copied from a secret input public.
        let sensitive = records
            .iter()
            .any(|record| projection_uses_secret(projection, record, table_name, table_alias));
        compute_window_column(host, records, name, args, window, &label)?;
        if sensitive {
            for record in records.iter_mut() {
                if let Some(value) = record.get(&label).cloned() {
                    record.set(&label, host.secret_query_output(value)?);
                }
            }
        }
    }
    Ok(())
}

/// Evaluate a projection that represents a per-call constant (the
/// offset / default arguments to LAG / LEAD). Handles the
/// `Projection::Column("LIT:…")` shape emitted by `expr_to_projection`
/// for literals, the `SUB(LIT:0, LIT:n)` shape emitted for unary
/// negation, and falls back to the typed `evaluator::evaluate` for
/// everything else.
fn eval_projection_constant<H: WindowHost>(
    host: &H,
    depth: u32,
    proj: &Projection,
    record: &UnifiedRecord,
) -> Result<Option<Value>, H::Error> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return Ok(None);
    }
    if let Projection::Column(s) = proj {
        if let Some(lit) = s.strip_prefix("LIT:") {
            if lit.is_empty() {
                return Ok(Some(Value::Null));
            }
            if let Ok(v) = lit.parse::<i64>() {
                return Ok(Some(Value::Integer(v)));
            }
            if let Ok(v) = lit.parse::<f64>() {
                return Ok(Some(Value::Float(v)));
            }
            return Ok(Some(Value::text(lit.to_string())));
        }
    }
    // Preserve the lowered unary-minus form used by LAG/LEAD defaults.
    if let Projection::Function(name, sub_args) = proj {
        let base = name.split(':').next().unwrap_or(name);
        if base.eq_ignore_ascii_case("SUB") && sub_args.len() == 2 {
            let Some(lhs) = eval_projection_constant(host, depth + 1, &sub_args[0], record)? else {
                return Ok(None);
            };
            let Some(rhs) = eval_projection_constant(host, depth + 1, &sub_args[1], record)? else {
                return Ok(None);
            };
            if let (Value::Integer(a), Value::Integer(b)) = (&lhs, &rhs) {
                return Ok(a.checked_sub(*b).map(Value::Integer));
            }
            if let (Some(a), Some(b)) = (value_as_f64(&lhs), value_as_f64(&rhs)) {
                return Ok(Some(Value::Float(a - b)));
            }
        }
    }
    let Some((expr, _)) = projection_to_expr(proj) else {
        return Ok(None);
    };
    host.try_evaluate_expression(&expr, record)
}

// Bound projection recursion before the lowering routine walks it. Embedded Expr /
// Filter trees and window specs retain their existing parser/AST admission limits.
fn window_projection_depth_allowed(projection: &Projection) -> bool {
    let mut pending = vec![(projection, 0)];
    while let Some((projection, depth)) = pending.pop() {
        if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
            return false;
        }
        if let Projection::Function(_, args) | Projection::Window { args, .. } = projection {
            pending.extend(args.iter().map(|argument| (argument, depth + 1)));
        }
    }
    true
}

fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(v) | Value::BigInt(v) => Some(*v as f64),
        Value::UnsignedInteger(v) => Some(*v as f64),
        Value::Float(v) => Some(*v),
        _ => None,
    }
}

fn compute_window_column<H: WindowHost>(
    host: &H,
    records: &mut [UnifiedRecord],
    func_name: &str,
    args: &[Projection],
    window: &WindowSpec,
    out_col: &str,
) -> Result<(), H::Error> {
    let upper = func_name.to_uppercase();
    let row_count = records.len();

    // Materialise partition + order keys for every row up front so the
    // per-partition sort comparator is a pure integer comparison.
    let partition_keys: Vec<Vec<CanonicalKey>> = records
        .iter()
        .map(|rec| {
            window
                .partition_by
                .iter()
                .map(|expr| {
                    Ok(value_to_canonical_key(
                        &host
                            .try_evaluate_expression(expr, rec)?
                            .unwrap_or(Value::Null),
                    )
                    .unwrap_or(CanonicalKey::Null))
                })
                .collect::<Result<Vec<_>, H::Error>>()
        })
        .collect::<Result<Vec<_>, H::Error>>()?;

    let order_keys: Vec<Vec<CanonicalKey>> = records
        .iter()
        .map(|rec| {
            window
                .order_by
                .iter()
                .map(|item| {
                    Ok(value_to_canonical_key(
                        &host
                            .try_evaluate_expression(&item.expr, rec)?
                            .unwrap_or(Value::Null),
                    )
                    .unwrap_or(CanonicalKey::Null))
                })
                .collect::<Result<Vec<_>, H::Error>>()
        })
        .collect::<Result<Vec<_>, H::Error>>()?;

    // Group row indices by partition key, preserving first-seen order
    // so functions without a partition_by still see deterministic
    // partition iteration.
    let mut groups: HashMap<Vec<CanonicalKey>, Vec<usize>> = HashMap::new();
    let mut group_order: Vec<Vec<CanonicalKey>> = Vec::new();
    for (i, key) in partition_keys.iter().enumerate() {
        let key = key.clone();
        if !groups.contains_key(&key) {
            group_order.push(key.clone());
        }
        groups.entry(key).or_default().push(i);
    }

    let order_dirs: Vec<(bool, bool)> = window
        .order_by
        .iter()
        .map(|o| (o.ascending, o.nulls_first))
        .collect();

    for indices in groups.values_mut() {
        indices.sort_by(|&a, &b| {
            for (dim, (asc, nulls_first)) in order_dirs.iter().enumerate() {
                let ka = &order_keys[a][dim];
                let kb = &order_keys[b][dim];
                let ord = compare_with_nulls(ka, kb, *nulls_first);
                let ord = if *asc { ord } else { ord.reverse() };
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            Ordering::Equal
        });
    }

    let mut results: Vec<Value> = vec![Value::Null; row_count];

    // Pre-evaluate the source-column expression for the five aggregate
    // OVER functions (slice 7c). COUNT(*) — encoded as `Projection::All`
    // — has no source expression, so we leave the vector empty and the
    // aggregator interprets that as "count every row in the frame".
    let agg_src_values: Vec<Value> =
        if matches!(upper.as_str(), "SUM" | "AVG" | "MIN" | "MAX" | "COUNT") {
            match args.first() {
                None | Some(Projection::All) => Vec::new(),
                Some(arg_proj) => {
                    let (expr, _) = projection_to_expr(arg_proj).ok_or_else(|| {
                        host.query_error(format!(
                            "{upper} OVER: argument is not a supported expression"
                        ))
                    })?;
                    records
                        .iter()
                        .map(|rec| {
                            host.try_evaluate_expression(&expr, rec)
                                .map(|value| value.unwrap_or(Value::Null))
                        })
                        .collect::<Result<Vec<_>, H::Error>>()?
                }
            }
        } else {
            Vec::new()
        };
    let agg_counts_all_rows = matches!(args.first(), None | Some(Projection::All));

    let (lag_lead_offset, lag_lead_default, lag_lead_src_values) =
        if matches!(upper.as_str(), "LAG" | "LEAD") {
            let src_proj = args.first().ok_or_else(|| {
                host.query_error(format!(
                    "{upper} requires at least one argument (source column)"
                ))
            })?;
            let (src_expr, _) = projection_to_expr(src_proj).ok_or_else(|| {
                host.query_error(format!(
                    "{upper} source argument is not a supported expression"
                ))
            })?;

            let offset = if let Some(arg) = args.get(1) {
                match eval_projection_constant(host, 0, arg, &records[0])? {
                    Some(Value::Integer(v) | Value::BigInt(v)) if v < 0 => {
                        return Err(host
                            .query_error(format!("{upper} offset must be non-negative, got {v}")));
                    }
                    Some(Value::Integer(v) | Value::BigInt(v)) => v as u64,
                    Some(Value::UnsignedInteger(v)) => v,
                    Some(Value::Null) | None => 1,
                    Some(other) => {
                        return Err(host.query_error(format!(
                            "{upper} offset must evaluate to an integer, got {other:?}"
                        )))
                    }
                }
            } else {
                1
            };

            let default = match args.get(2) {
                Some(arg) => eval_projection_constant(host, 0, arg, &records[0])?,
                None => None,
            };

            let src_values: Vec<Value> = records
                .iter()
                .map(|rec| {
                    host.try_evaluate_expression(&src_expr, rec)
                        .map(|value| value.unwrap_or(Value::Null))
                })
                .collect::<Result<Vec<_>, H::Error>>()?;

            (offset, default, src_values)
        } else {
            (0, None, Vec::new())
        };

    for key in &group_order {
        let partition_indices = groups.get(key).expect("partition exists");
        match upper.as_str() {
            "ROW_NUMBER" => {
                for (pos, &idx) in partition_indices.iter().enumerate() {
                    results[idx] = Value::Integer((pos + 1) as i64);
                }
            }
            "RANK" => {
                let mut prev: Option<&[CanonicalKey]> = None;
                let mut current_rank: i64 = 0;
                for (pos, &idx) in partition_indices.iter().enumerate() {
                    let here = order_keys[idx].as_slice();
                    let same_as_prev = prev.is_some_and(|p| p == here);
                    if !same_as_prev {
                        current_rank = (pos as i64) + 1;
                    }
                    results[idx] = Value::Integer(current_rank);
                    prev = Some(here);
                }
            }
            "DENSE_RANK" => {
                let mut prev: Option<&[CanonicalKey]> = None;
                let mut current_rank: i64 = 0;
                for &idx in partition_indices.iter() {
                    let here = order_keys[idx].as_slice();
                    let same_as_prev = prev.is_some_and(|p| p == here);
                    if !same_as_prev {
                        current_rank += 1;
                    }
                    results[idx] = Value::Integer(current_rank);
                    prev = Some(here);
                }
            }
            "LAG" | "LEAD" => {
                let offset = usize::try_from(lag_lead_offset).ok();
                for (pos, &idx) in partition_indices.iter().enumerate() {
                    let target = offset.and_then(|offset| {
                        if upper == "LAG" {
                            pos.checked_sub(offset)
                        } else {
                            pos.checked_add(offset)
                        }
                    });
                    if let Some(&src_idx) = target.and_then(|target| partition_indices.get(target))
                    {
                        results[idx] = lag_lead_src_values[src_idx].clone();
                    } else {
                        results[idx] = lag_lead_default.clone().unwrap_or(Value::Null);
                    }
                }
            }
            "SUM" | "COUNT" | "AVG" | "MIN" | "MAX" => {
                let has_order = !window.order_by.is_empty();
                for (pos, &idx) in partition_indices.iter().enumerate() {
                    let (start, end) = frame_bounds(
                        host,
                        pos,
                        partition_indices,
                        &order_keys,
                        window.frame.as_ref(),
                        has_order,
                    )?;
                    let frame_slice = &partition_indices[start..=end];
                    let value = match upper.as_str() {
                        "COUNT" => {
                            let n = if agg_counts_all_rows {
                                frame_slice.len() as i64
                            } else {
                                frame_slice
                                    .iter()
                                    .filter(|&&row_idx| {
                                        !matches!(agg_src_values[row_idx], Value::Null)
                                    })
                                    .count() as i64
                            };
                            Value::Integer(n)
                        }
                        "SUM" => sum_over_frame(&agg_src_values, frame_slice),
                        "AVG" => avg_over_frame(&agg_src_values, frame_slice),
                        "MIN" => min_over_frame(&agg_src_values, frame_slice, true),
                        "MAX" => min_over_frame(&agg_src_values, frame_slice, false),
                        _ => unreachable!(),
                    };
                    results[idx] = value;
                }
            }
            other => {
                return Err(host.query_error(format!(
                    "window function {other} is not supported — \
                     wired functions are ROW_NUMBER / RANK / DENSE_RANK / LAG / LEAD \
                     and aggregate OVER for SUM / COUNT / AVG / MIN / MAX"
                )));
            }
        }
    }

    for (idx, value) in results.into_iter().enumerate() {
        records[idx].set(out_col, value);
    }
    Ok(())
}

/// Resolve the frame's `[start, end]` row positions (inclusive,
/// indexed into the sorted partition slice) for the row at `pos`.
///
/// Slice 7c handles three explicit frame variants plus the SQL
/// defaults:
/// - No ORDER BY → frame = the whole partition (unordered aggregate).
/// - ORDER BY present and `frame: None` → `RANGE UNBOUNDED PRECEDING
///   AND CURRENT ROW` (the SQL default for ordered windows). "CURRENT
///   ROW" under RANGE means *peers* — every row whose ORDER BY keys
///   equal the current row's keys, so ties accumulate together.
/// - `ROWS UNBOUNDED PRECEDING AND CURRENT ROW` → `[0, pos]`.
/// - `ROWS BETWEEN N PRECEDING AND CURRENT ROW` → `[max(0, pos-N), pos]`.
///
/// Anything else (FOLLOWING bounds, RANGE arithmetic, etc.) is out of
/// scope for slice 7c and returns an explicit host query error.
fn frame_bounds<H: WindowHost>(
    host: &H,
    pos: usize,
    partition_indices: &[usize],
    order_keys: &[Vec<CanonicalKey>],
    frame: Option<&WindowFrame>,
    has_order: bool,
) -> Result<(usize, usize), H::Error> {
    let last = partition_indices.len() - 1;
    let frame = match frame {
        None => {
            return if has_order {
                Ok((0, range_current_row_end(pos, partition_indices, order_keys)))
            } else {
                Ok((0, last))
            };
        }
        Some(f) => f,
    };

    let end_bound = frame.end.clone().unwrap_or(WindowFrameBound::CurrentRow);

    match (frame.unit, &frame.start, &end_bound) {
        (
            WindowFrameUnit::Range,
            WindowFrameBound::UnboundedPreceding,
            WindowFrameBound::CurrentRow,
        ) => {
            if has_order {
                Ok((0, range_current_row_end(pos, partition_indices, order_keys)))
            } else {
                Ok((0, last))
            }
        }
        (
            WindowFrameUnit::Rows,
            WindowFrameBound::UnboundedPreceding,
            WindowFrameBound::CurrentRow,
        ) => Ok((0, pos)),
        (
            WindowFrameUnit::Rows,
            WindowFrameBound::Preceding(offset_expr),
            WindowFrameBound::CurrentRow,
        ) => {
            let n = preceding_offset_value(host, offset_expr)?;
            let start = pos.saturating_sub(n);
            Ok((start, pos))
        }
        _ => Err(host.query_error(
            "window frame variant not supported in slice 7c — \
             supported: RANGE UNBOUNDED PRECEDING AND CURRENT ROW, \
             ROWS UNBOUNDED PRECEDING AND CURRENT ROW, \
             ROWS N PRECEDING AND CURRENT ROW"
                .to_string(),
        )),
    }
}

/// Under RANGE, CURRENT ROW extends through every peer row — i.e.
/// every subsequent row sharing the current row's ORDER BY keys. The
/// partition is already sorted, so peers are contiguous; walk forward
/// from `pos` while the keys still match.
fn range_current_row_end(
    pos: usize,
    partition_indices: &[usize],
    order_keys: &[Vec<CanonicalKey>],
) -> usize {
    let here = order_keys[partition_indices[pos]].as_slice();
    let mut end = pos;
    while end + 1 < partition_indices.len()
        && order_keys[partition_indices[end + 1]].as_slice() == here
    {
        end += 1;
    }
    end
}

/// `ROWS N PRECEDING` — N must be a non-negative integer literal.
fn preceding_offset_value<H: WindowHost>(
    host: &H,
    expr: &reddb_rql::ast::Expr,
) -> Result<usize, H::Error> {
    use reddb_rql::ast::Expr;
    match expr {
        Expr::Literal { value, .. } => match value {
            Value::Integer(v) | Value::BigInt(v) if *v >= 0 => {
                Ok(usize::try_from(*v).unwrap_or(usize::MAX))
            }
            Value::Integer(v) | Value::BigInt(v) => Err(host.query_error(format!(
                "ROWS PRECEDING offset must be non-negative, got {v}"
            ))),
            Value::UnsignedInteger(v) => Ok(usize::try_from(*v).unwrap_or(usize::MAX)),
            other => Err(host.query_error(format!(
                "ROWS PRECEDING offset must be an integer literal, got {other:?}"
            ))),
        },
        other => Err(host.query_error(format!(
            "ROWS PRECEDING offset must be an integer literal, got {other:?}"
        ))),
    }
}

fn sum_over_frame(src: &[Value], indices: &[usize]) -> Value {
    let mut i_sum: i64 = 0;
    let mut f_sum: f64 = 0.0;
    let mut any_float = false;
    let mut any_nonnull = false;
    for &i in indices {
        match &src[i] {
            Value::Null => {}
            Value::Integer(v) | Value::BigInt(v) => {
                any_nonnull = true;
                i_sum = i_sum.saturating_add(*v);
                f_sum += *v as f64;
            }
            Value::UnsignedInteger(v) => {
                any_nonnull = true;
                i_sum = i_sum.saturating_add(*v as i64);
                f_sum += *v as f64;
            }
            Value::Float(v) => {
                any_nonnull = true;
                any_float = true;
                f_sum += *v;
            }
            _ => {}
        }
    }
    if !any_nonnull {
        Value::Null
    } else if any_float {
        Value::Float(f_sum)
    } else {
        Value::Integer(i_sum)
    }
}

fn avg_over_frame(src: &[Value], indices: &[usize]) -> Value {
    let mut sum: f64 = 0.0;
    let mut count: u64 = 0;
    for &i in indices {
        if let Some(v) = value_as_f64(&src[i]) {
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        Value::Null
    } else {
        Value::Float(sum / count as f64)
    }
}

fn min_over_frame(src: &[Value], indices: &[usize], pick_min: bool) -> Value {
    let mut best: Option<(CanonicalKey, Value)> = None;
    for &i in indices {
        if matches!(src[i], Value::Null) {
            continue;
        }
        let Some(key) = value_to_canonical_key(&src[i]) else {
            continue;
        };
        let take = match &best {
            None => true,
            Some((b_key, _)) => {
                if pick_min {
                    key < *b_key
                } else {
                    key > *b_key
                }
            }
        };
        if take {
            best = Some((key, src[i].clone()));
        }
    }
    best.map(|(_, v)| v).unwrap_or(Value::Null)
}

fn compare_with_nulls(a: &CanonicalKey, b: &CanonicalKey, nulls_first: bool) -> Ordering {
    let a_null = matches!(a, CanonicalKey::Null);
    let b_null = matches!(b, CanonicalKey::Null);
    match (a_null, b_null) {
        (true, true) => Ordering::Equal,
        (true, false) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (false, true) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (false, false) => a.cmp(b),
    }
}
