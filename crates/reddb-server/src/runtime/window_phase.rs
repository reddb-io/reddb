//! Issue #590 — Analytics slice 7b: planner+runtime wiring for the
//! five window-only functions (ROW_NUMBER, RANK, DENSE_RANK, LAG,
//! LEAD).
//!
//! Sits between the canonical scan/filter/sort and the final
//! projection. For each `Projection::Window` in the projection list,
//! partitions the row set by the window's `PARTITION BY`, sorts each
//! partition by `ORDER BY`, computes the function value per row, and
//! writes it back onto the record under the projection's alias (or
//! function name when no alias is given) so the projection node can
//! pick it up as if it were a plain column.
//!
//! The existing `storage::query::executors::window::WindowExecutor`
//! is a fuller reference implementation operating over the
//! `engine::binding::Value` enum, which loses information for the
//! richer `schema::Value` variants (Timestamp, UUID, Decimal, etc.).
//! This module avoids that lossy conversion by operating directly on
//! `UnifiedRecord` for the five slice-7b functions; subsequent
//! slices that need full aggregate-OVER coverage can revisit that
//! choice.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::api::RedDBError;
use crate::storage::query::ast::{
    Projection, WindowFrame, WindowFrameBound, WindowFrameUnit, WindowSpec,
};
use crate::storage::query::evaluator;
use crate::storage::query::sql_lowering::projection_to_expr;
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::{value_to_canonical_key, CanonicalKey, Value};

use super::join_filter::projection_name;

/// Evaluate every `Projection::Window` in `projections` and write
/// the result back onto each record under the projection's output
/// label. After this call, the projection node can resolve window
/// outputs via `record.get(label)` exactly like any other column.
pub(crate) fn apply(
    records: &mut [UnifiedRecord],
    projections: &[Projection],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Result<(), RedDBError> {
    if records.is_empty() {
        return Ok(());
    }
    for projection in projections {
        let Projection::Window {
            name,
            args,
            window,
            ..
        } = projection
        else {
            continue;
        };
        let label = projection_name(projection);
        compute_window_column(
            records,
            name,
            args,
            window,
            &label,
            table_name,
            table_alias,
        )?;
    }
    Ok(())
}

/// Evaluate a projection that represents a per-call constant (the
/// offset / default arguments to LAG / LEAD). Handles the
/// `Projection::Column("LIT:…")` shape emitted by `expr_to_projection`
/// for literals, the `SUB(LIT:0, LIT:n)` shape emitted for unary
/// negation, and falls back to the typed `evaluator::evaluate` for
/// everything else.
fn eval_projection_constant(
    proj: &Projection,
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    // Direct literal — `expr_to_projection` lowers `Expr::Literal` to
    // `Projection::Column("LIT:<rendered>")`.
    if let Projection::Column(s) = proj {
        if let Some(lit) = s.strip_prefix("LIT:") {
            if lit.is_empty() {
                return Some(Value::Null);
            }
            if let Ok(v) = lit.parse::<i64>() {
                return Some(Value::Integer(v));
            }
            if let Ok(v) = lit.parse::<f64>() {
                return Some(Value::Float(v));
            }
            return Some(Value::text(lit.to_string()));
        }
    }
    // Unary minus is lowered to `SUB(LIT:0, <operand>)` by
    // `expr_to_projection`. The evaluator does not know "SUB" as a
    // function — recognise the pattern here so `LAG(ts, 1, -1)`
    // surfaces the right default instead of a typed
    // UnknownFunction → Null.
    if let Projection::Function(name, sub_args) = proj {
        let base = name.split(':').next().unwrap_or(name);
        if base.eq_ignore_ascii_case("SUB") && sub_args.len() == 2 {
            let lhs = eval_projection_constant(&sub_args[0], record, table_name, table_alias)?;
            let rhs = eval_projection_constant(&sub_args[1], record, table_name, table_alias)?;
            if let (Value::Integer(a), Value::Integer(b)) = (&lhs, &rhs) {
                return Some(Value::Integer(a - b));
            }
            if let (Some(a), Some(b)) = (value_as_f64(&lhs), value_as_f64(&rhs)) {
                return Some(Value::Float(a - b));
            }
        }
    }
    let (expr, _) = projection_to_expr(proj)?;
    let row_closure = |field: &crate::storage::query::ast::FieldRef| -> Option<Value> {
        super::join_filter::resolve_runtime_field(record, field, table_name, table_alias)
    };
    evaluator::evaluate(&expr, &row_closure).ok()
}

fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(v) | Value::BigInt(v) => Some(*v as f64),
        Value::UnsignedInteger(v) => Some(*v as f64),
        Value::Float(v) => Some(*v),
        _ => None,
    }
}

fn eval_expr_on_record(
    expr: &crate::storage::query::ast::Expr,
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Value {
    let row_closure = |field: &crate::storage::query::ast::FieldRef| -> Option<Value> {
        super::join_filter::resolve_runtime_field(record, field, table_name, table_alias)
    };
    evaluator::evaluate(expr, &row_closure).unwrap_or(Value::Null)
}

fn compute_window_column(
    records: &mut [UnifiedRecord],
    func_name: &str,
    args: &[Projection],
    window: &WindowSpec,
    out_col: &str,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Result<(), RedDBError> {
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
                    value_to_canonical_key(&eval_expr_on_record(
                        expr,
                        rec,
                        table_name,
                        table_alias,
                    ))
                    .unwrap_or(CanonicalKey::Null)
                })
                .collect()
        })
        .collect();

    let order_keys: Vec<Vec<CanonicalKey>> = records
        .iter()
        .map(|rec| {
            window
                .order_by
                .iter()
                .map(|item| {
                    value_to_canonical_key(&eval_expr_on_record(
                        &item.expr,
                        rec,
                        table_name,
                        table_alias,
                    ))
                    .unwrap_or(CanonicalKey::Null)
                })
                .collect()
        })
        .collect();

    // Group row indices by partition key, preserving first-seen order
    // so functions without a partition_by still see deterministic
    // partition iteration.
    let mut groups: HashMap<Vec<CanonicalKey>, Vec<usize>> = HashMap::new();
    let mut group_order: Vec<Vec<CanonicalKey>> = Vec::new();
    for i in 0..row_count {
        let key = partition_keys[i].clone();
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
    let agg_src_values: Vec<Value> = if matches!(
        upper.as_str(),
        "SUM" | "AVG" | "MIN" | "MAX" | "COUNT"
    ) {
        match args.first() {
            None | Some(Projection::All) => Vec::new(),
            Some(arg_proj) => {
                let (expr, _) = projection_to_expr(arg_proj).ok_or_else(|| {
                    RedDBError::Query(format!(
                        "{upper} OVER: argument is not a supported expression"
                    ))
                })?;
                records
                    .iter()
                    .map(|rec| eval_expr_on_record(&expr, rec, table_name, table_alias))
                    .collect()
            }
        }
    } else {
        Vec::new()
    };
    let agg_counts_all_rows = matches!(args.first(), None | Some(Projection::All));

    let (lag_lead_offset, lag_lead_default, lag_lead_src_values) = if matches!(
        upper.as_str(),
        "LAG" | "LEAD"
    ) {
        let src_proj = args.first().ok_or_else(|| {
            RedDBError::Query(format!(
                "{upper} requires at least one argument (source column)"
            ))
        })?;
        let (src_expr, _) = projection_to_expr(src_proj).ok_or_else(|| {
            RedDBError::Query(format!("{upper} source argument is not a supported expression"))
        })?;

        let offset = if let Some(arg) = args.get(1) {
            match eval_projection_constant(arg, &records[0], table_name, table_alias) {
                Some(Value::Integer(v)) => v,
                Some(Value::BigInt(v)) => v,
                Some(Value::UnsignedInteger(v)) => v as i64,
                Some(Value::Null) | None => 1,
                Some(other) => {
                    return Err(RedDBError::Query(format!(
                        "{upper} offset must evaluate to an integer, got {other:?}"
                    )))
                }
            }
        } else {
            1
        };
        if offset < 0 {
            return Err(RedDBError::Query(format!(
                "{upper} offset must be non-negative, got {offset}"
            )));
        }

        let default = args
            .get(2)
            .and_then(|arg| eval_projection_constant(arg, &records[0], table_name, table_alias));

        let src_values: Vec<Value> = records
            .iter()
            .map(|rec| eval_expr_on_record(&src_expr, rec, table_name, table_alias))
            .collect();

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
                let direction: i64 = if upper == "LAG" { -1 } else { 1 };
                let partition_len = partition_indices.len() as i64;
                for (pos, &idx) in partition_indices.iter().enumerate() {
                    let target = (pos as i64) + direction * lag_lead_offset;
                    if target >= 0 && target < partition_len {
                        let src_idx = partition_indices[target as usize];
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
                return Err(RedDBError::Query(format!(
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
/// scope for slice 7c and returns an explicit `RedDBError::Query`.
fn frame_bounds(
    pos: usize,
    partition_indices: &[usize],
    order_keys: &[Vec<CanonicalKey>],
    frame: Option<&WindowFrame>,
    has_order: bool,
) -> Result<(usize, usize), RedDBError> {
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

    let end_bound = frame
        .end
        .clone()
        .unwrap_or(WindowFrameBound::CurrentRow);

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
        (WindowFrameUnit::Rows, WindowFrameBound::Preceding(offset_expr), WindowFrameBound::CurrentRow) => {
            let n = preceding_offset_value(offset_expr)?;
            let start = pos.saturating_sub(n);
            Ok((start, pos))
        }
        _ => Err(RedDBError::Query(
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
fn preceding_offset_value(
    expr: &crate::storage::query::ast::Expr,
) -> Result<usize, RedDBError> {
    use crate::storage::query::ast::Expr;
    match expr {
        Expr::Literal { value, .. } => match value {
            Value::Integer(v) | Value::BigInt(v) if *v >= 0 => Ok(*v as usize),
            Value::Integer(v) | Value::BigInt(v) => Err(RedDBError::Query(format!(
                "ROWS PRECEDING offset must be non-negative, got {v}"
            ))),
            Value::UnsignedInteger(v) => Ok(*v as usize),
            other => Err(RedDBError::Query(format!(
                "ROWS PRECEDING offset must be an integer literal, got {other:?}"
            ))),
        },
        other => Err(RedDBError::Query(format!(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{Expr, FieldRef, Span, WindowOrderItem};
    use crate::storage::schema::Value;

    fn rec(id: i64, user: &str, ts: i64) -> UnifiedRecord {
        let mut r = UnifiedRecord::new();
        r.set("id", Value::Integer(id));
        r.set("user_id", Value::text(user.to_string()));
        r.set("ts", Value::Integer(ts));
        r
    }

    fn col_field(name: &str) -> FieldRef {
        FieldRef::TableColumn {
            table: String::new(),
            column: name.to_string(),
        }
    }

    fn col_expr(name: &str) -> Expr {
        Expr::Column {
            field: col_field(name),
            span: Span::synthetic(),
        }
    }

    fn window_proj(name: &str, args: Vec<Projection>, spec: WindowSpec, alias: &str) -> Projection {
        Projection::Window {
            name: name.to_string(),
            args,
            window: Box::new(spec),
            alias: Some(alias.to_string()),
        }
    }

    #[test]
    fn row_number_partitioned_by_user_ordered_by_ts() {
        let mut rows = vec![
            rec(1, "u1", 100),
            rec(2, "u1", 200),
            rec(3, "u2", 50),
            rec(4, "u1", 150),
            rec(5, "u2", 75),
        ];
        let spec = WindowSpec {
            partition_by: vec![col_expr("user_id")],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            &mut rows,
            &[window_proj("ROW_NUMBER", vec![], spec, "rn")],
            None,
            None,
        )
        .expect("apply");

        let by_id: HashMap<i64, i64> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!("id"),
                };
                let rn = match r.get("rn").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!("rn"),
                };
                (id, rn)
            })
            .collect();
        // u1 by ts: id=1 (100) → 1, id=4 (150) → 2, id=2 (200) → 3
        // u2 by ts: id=3 (50) → 1, id=5 (75) → 2
        assert_eq!(by_id[&1], 1);
        assert_eq!(by_id[&4], 2);
        assert_eq!(by_id[&2], 3);
        assert_eq!(by_id[&3], 1);
        assert_eq!(by_id[&5], 2);
    }

    #[test]
    fn rank_and_dense_rank_treat_ties_differently() {
        let mut rows = vec![
            rec(1, "u1", 100),
            rec(2, "u1", 100), // tied with id=1
            rec(3, "u1", 200),
        ];
        let spec = || WindowSpec {
            partition_by: vec![col_expr("user_id")],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            &mut rows,
            &[
                window_proj("RANK", vec![], spec(), "rk"),
                window_proj("DENSE_RANK", vec![], spec(), "drk"),
            ],
            None,
            None,
        )
        .expect("apply");

        let result: HashMap<i64, (i64, i64)> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                let rk = match r.get("rk").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                let drk = match r.get("drk").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                (id, (rk, drk))
            })
            .collect();
        // RANK: id=1 → 1, id=2 → 1 (tie), id=3 → 3 (gap)
        // DENSE_RANK: id=1 → 1, id=2 → 1, id=3 → 2 (no gap)
        assert_eq!(result[&1], (1, 1));
        assert_eq!(result[&2], (1, 1));
        assert_eq!(result[&3], (3, 2));
    }

    #[test]
    fn lag_returns_prior_value_or_null_on_first_row() {
        let mut rows = vec![
            rec(1, "u1", 100),
            rec(2, "u1", 200),
            rec(3, "u1", 300),
        ];
        let spec = WindowSpec {
            partition_by: vec![col_expr("user_id")],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            &mut rows,
            &[window_proj(
                "LAG",
                vec![Projection::Field(col_field("ts"), None)],
                spec,
                "prev_ts",
            )],
            None,
            None,
        )
        .expect("apply");

        let by_id: HashMap<i64, Value> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                (id, r.get("prev_ts").cloned().unwrap_or(Value::Null))
            })
            .collect();
        assert!(matches!(by_id[&1], Value::Null));
        assert_eq!(by_id[&2], Value::Integer(100));
        assert_eq!(by_id[&3], Value::Integer(200));
    }

    #[test]
    fn lead_returns_next_value_or_null_on_last_row() {
        let mut rows = vec![
            rec(1, "u1", 100),
            rec(2, "u1", 200),
            rec(3, "u1", 300),
        ];
        let spec = WindowSpec {
            partition_by: vec![col_expr("user_id")],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            &mut rows,
            &[window_proj(
                "LEAD",
                vec![Projection::Field(col_field("ts"), None)],
                spec,
                "next_ts",
            )],
            None,
            None,
        )
        .expect("apply");

        let by_id: HashMap<i64, Value> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                (id, r.get("next_ts").cloned().unwrap_or(Value::Null))
            })
            .collect();
        assert_eq!(by_id[&1], Value::Integer(200));
        assert_eq!(by_id[&2], Value::Integer(300));
        assert!(matches!(by_id[&3], Value::Null));
    }

    #[test]
    fn lag_with_offset_and_default() {
        let mut rows = vec![
            rec(1, "u1", 100),
            rec(2, "u1", 200),
            rec(3, "u1", 300),
            rec(4, "u1", 400),
        ];
        let spec = WindowSpec {
            partition_by: vec![],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            &mut rows,
            &[window_proj(
                "LAG",
                vec![
                    Projection::Field(col_field("ts"), None),
                    Projection::Column("LIT:2".to_string()),
                    Projection::Column("LIT:-1".to_string()),
                ],
                spec,
                "lag2",
            )],
            None,
            None,
        )
        .expect("apply");

        let by_id: HashMap<i64, Value> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                (id, r.get("lag2").cloned().unwrap_or(Value::Null))
            })
            .collect();
        // offset=2: positions [1..]=100, [2..]=200 ... so lag2:
        // pos 0 → default -1
        // pos 1 → default -1
        // pos 2 → ts of pos 0 = 100
        // pos 3 → ts of pos 1 = 200
        assert_eq!(by_id[&1], Value::Integer(-1));
        assert_eq!(by_id[&2], Value::Integer(-1));
        assert_eq!(by_id[&3], Value::Integer(100));
        assert_eq!(by_id[&4], Value::Integer(200));
    }
}
