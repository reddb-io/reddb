//! Canonical ORDER BY and top-K over already-admitted runtime records.
use super::{
    field_resolution::resolve_runtime_field,
    values::{runtime_partial_cmp, text_abbrev_key},
};
use crate::query::unified::UnifiedRecord;
use reddb_rql::ast::{Expr, OrderByClause};
use reddb_types::Value;
use std::cmp::Ordering;

/// Hosts retain expression effects, secret policy and identity-key semantics.
/// As on the native path, an expression miss is an absent sort key. Hosts own
/// any separate validation/error policy before calling these ordering helpers.
pub trait OrderingHost {
    fn evaluate_expression(&self, expression: &Expr, record: &UnifiedRecord) -> Option<Value>;
    fn secret_query_input(&self, value: Value) -> Option<Value>;
    fn record_identity_key(&self, record: &UnifiedRecord) -> String;
    fn topk_disabled(&self) -> bool;
}

/// Hosts with operating failures distinguish unreadable keys from missing values.
/// Keys are extracted before the sort or top-K selection mutates the records.
pub trait FallibleOrderingHost: OrderingHost {
    type Error;
    fn try_evaluate_expression(
        &self,
        expression: &Expr,
        record: &UnifiedRecord,
    ) -> Result<Option<Value>, Self::Error>;
}

struct OptionalOrderingHost<'a, H>(&'a H);
impl<H: OrderingHost> OrderingHost for OptionalOrderingHost<'_, H> {
    fn evaluate_expression(&self, expression: &Expr, record: &UnifiedRecord) -> Option<Value> {
        self.0.evaluate_expression(expression, record)
    }
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        self.0.secret_query_input(value)
    }
    fn record_identity_key(&self, record: &UnifiedRecord) -> String {
        self.0.record_identity_key(record)
    }
    fn topk_disabled(&self) -> bool {
        self.0.topk_disabled()
    }
}
impl<H: OrderingHost> FallibleOrderingHost for OptionalOrderingHost<'_, H> {
    type Error = std::convert::Infallible;
    fn try_evaluate_expression(
        &self,
        expression: &Expr,
        record: &UnifiedRecord,
    ) -> Result<Option<Value>, Self::Error> {
        Ok(self.0.evaluate_expression(expression, record))
    }
}
fn optional_ordering_result<T>(result: Result<T, std::convert::Infallible>) -> T {
    match result {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

pub fn compare_runtime_order_with_host<H: OrderingHost>(
    host: &H,
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Ordering {
    optional_ordering_result(try_compare_runtime_order_with_host(
        &OptionalOrderingHost(host),
        left,
        right,
        clauses,
        table_name,
        table_alias,
    ))
}

pub fn try_compare_runtime_order_with_host<H: FallibleOrderingHost>(
    host: &H,
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Result<Ordering, H::Error> {
    for clause in clauses {
        // Fase 1.6: when the ORDER BY item is an expression (CAST,
        // arithmetic, CASE, etc.), evaluate it against each record
        // and compare the resulting Values. Bare-column clauses fall
        // back to the direct field resolver which is cheaper for the
        // common case.
        let (left_value, right_value) = if let Some(ref expr) = clause.expr {
            (
                host.try_evaluate_expression(expr, left)?,
                host.try_evaluate_expression(expr, right)?,
            )
        } else {
            (
                resolve_runtime_field(left, &clause.field, table_name, table_alias),
                resolve_runtime_field(right, &clause.field, table_name, table_alias),
            )
        };
        let ordering = compare_runtime_optional_values(
            &|value| host.secret_query_input(value),
            left_value.as_ref(),
            right_value.as_ref(),
            clause.nulls_first,
        );

        if ordering != Ordering::Equal {
            return Ok(if clause.ascending {
                ordering
            } else {
                ordering.reverse()
            });
        }
    }

    Ok(host
        .record_identity_key(left)
        .cmp(&host.record_identity_key(right)))
}

/// Sort `records` by `order_by` using the Schwartzian transform:
/// extract sort keys once per record (O(n)), sort by the extracted keys
/// (O(n log n) value comparisons, no HashMap lookups), then reorder.
///
/// For a naive `sort_by(compare_runtime_order)`, the sort calls
/// `resolve_runtime_field` O(n log n) times — once per comparison.
/// With pre-extraction, field resolution is O(n) regardless of sort depth.
/// A single sort key, carrying the full `Value` plus an optional `u64` abbreviated key
/// for `Text` values so the comparator can skip full string comparisons in the common case.
struct SortKey {
    value: Option<Value>,
    abbrev: Option<u64>,
}

impl SortKey {
    fn new(value: Option<Value>) -> Self {
        let abbrev = match &value {
            Some(Value::Text(s)) => Some(text_abbrev_key(s)),
            _ => None,
        };
        SortKey { value, abbrev }
    }
}

pub fn sort_records_by_order_by_with_host<H: OrderingHost>(
    host: &H,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    optional_ordering_result(try_sort_records_by_order_by_with_host(
        &OptionalOrderingHost(host),
        records,
        order_by,
        table_name,
        table_alias,
    ))
}

pub fn try_sort_records_by_order_by_with_host<H: FallibleOrderingHost>(
    host: &H,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Result<(), H::Error> {
    if order_by.is_empty() || records.len() < 2 {
        return Ok(());
    }

    // Extract sort keys once per record — O(n × k) where k = ORDER BY clauses.
    // Layout: a single flat `Vec<SortKey>` of length n*k where row i's
    // keys live at `keys_flat[i*k .. (i+1)*k]`. One allocation total
    // instead of one per row, and the index permutation in `idxs`
    // stays a contiguous `Vec<usize>` — both vectors are sized exactly
    // up front so the inner sort never reallocates.
    let n = records.len();
    let k = order_by.len();
    let mut keys_flat: Vec<SortKey> = Vec::with_capacity(n * k);
    for rec in records.iter() {
        for clause in order_by.iter() {
            let v = if let Some(ref expr) = clause.expr {
                host.try_evaluate_expression(expr, rec)?
            } else {
                resolve_runtime_field(rec, &clause.field, table_name, table_alias)
            };
            keys_flat.push(SortKey::new(v));
        }
    }
    let mut idxs: Vec<usize> = (0..n).collect();

    // Sort by extracted keys — O(n log n).
    // Text: compare abbreviated u64 key first; only fall through to full str::cmp on tie.
    // Non-text: delegate to the existing value comparator as before.
    let cmp_keys = |a: usize, b: usize| -> Ordering {
        let la = a * k;
        let lb = b * k;
        for (j, clause) in order_by.iter().enumerate() {
            let lk = &keys_flat[la + j];
            let rk = &keys_flat[lb + j];
            let ord = match (&lk.abbrev, &rk.abbrev, &lk.value, &rk.value) {
                (Some(la_a), Some(ra), Some(Value::Text(ls)), Some(Value::Text(rs))) => {
                    match la_a.cmp(ra) {
                        Ordering::Equal => ls.as_ref().cmp(rs.as_ref()),
                        other => other,
                    }
                }
                _ => compare_runtime_optional_values(
                    &|value| host.secret_query_input(value),
                    lk.value.as_ref(),
                    rk.value.as_ref(),
                    clause.nulls_first,
                ),
            };
            if ord != Ordering::Equal {
                return if clause.ascending { ord } else { ord.reverse() };
            }
        }
        Ordering::Equal
    };
    idxs.sort_by(|&a, &b| cmp_keys(a, b));

    // Reorder records in-place using the sorted index permutation
    let orig: Vec<_> = std::mem::take(records);
    let mut out = Vec::with_capacity(n);
    for i in idxs {
        out.push(orig[i].clone());
    }
    *records = out;
    Ok(())
}

/// Chooses between full sort and quickselect-based top-k based on
/// `limit`, `offset`, and the kill-switch. When `limit` is unset or
/// `records.len()` is already close to `offset + limit`, the heap
/// overhead of top-k doesn't pay off and we fall through to the
/// existing sort path. Output is identical in every branch.
pub fn sort_or_top_k_records_with_host<H: OrderingHost>(
    host: &H,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    offset: Option<u64>,
    limit: Option<u64>,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    optional_ordering_result(try_sort_or_top_k_records_with_host(
        &OptionalOrderingHost(host),
        records,
        order_by,
        offset,
        limit,
        table_name,
        table_alias,
    ))
}

pub fn try_sort_or_top_k_records_with_host<H: FallibleOrderingHost>(
    host: &H,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    offset: Option<u64>,
    limit: Option<u64>,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Result<(), H::Error> {
    if order_by.is_empty() {
        return Ok(());
    }
    let effective_k = match (offset, limit) {
        (_, None) => None,
        (off, Some(lim)) => {
            Some(usize::try_from(off.unwrap_or(0).saturating_add(lim)).unwrap_or(usize::MAX))
        }
    };
    if !host.topk_disabled() {
        if let Some(k) = effective_k {
            // Only take the top-k path when it actually saves work:
            // n > 2k means quickselect + sort-of-k beats full sort.
            if k > 0 && records.len() > k.saturating_mul(2) {
                try_top_k_records_by_order_by_with_host(
                    host,
                    records,
                    order_by,
                    k,
                    table_name,
                    table_alias,
                )?;
                return Ok(());
            }
        }
    }
    try_sort_records_by_order_by_with_host(host, records, order_by, table_name, table_alias)?;
    Ok(())
}

/// Top-K variant of `sort_records_by_order_by_with_host`. For ORDER BY + LIMIT k
/// with n > k, full O(n log n) sort is wasteful — quickselect picks the
/// k smallest in O(n) average, then sort only those k. Mirrors the
/// `select_nth_unstable_by` pattern used by `query_direct`'s
/// `parse_simple_ordered_complex_select` fast path so semantics stay
/// identical across code paths.
///
/// Output is bit-identical to `sort_records_by_order_by_with_host(...) +
/// records.truncate(k)` because the comparator used here is a **strict
/// total order** — ties are broken by original index, so no two records
/// ever compare Equal and the unstable partition degenerates to stable
/// semantics.
pub fn top_k_records_by_order_by_with_host<H: OrderingHost>(
    host: &H,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    k: usize,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    optional_ordering_result(try_top_k_records_by_order_by_with_host(
        &OptionalOrderingHost(host),
        records,
        order_by,
        k,
        table_name,
        table_alias,
    ))
}

pub fn try_top_k_records_by_order_by_with_host<H: FallibleOrderingHost>(
    host: &H,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    k: usize,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Result<(), H::Error> {
    if k == 0 {
        records.clear();
        return Ok(());
    }
    if order_by.is_empty() {
        records.truncate(k);
        return Ok(());
    }
    let n = records.len();
    if n <= k {
        try_sort_records_by_order_by_with_host(host, records, order_by, table_name, table_alias)?;
        return Ok(());
    }

    // Flat key buffer — see `sort_records_by_order_by_with_host` for the
    // layout rationale. One allocation of n*kc keys instead of n.
    let kc = order_by.len();
    let mut keys_flat: Vec<SortKey> = Vec::with_capacity(n * kc);
    for rec in records.iter() {
        for clause in order_by.iter() {
            let v = if let Some(ref expr) = clause.expr {
                host.try_evaluate_expression(expr, rec)?
            } else {
                resolve_runtime_field(rec, &clause.field, table_name, table_alias)
            };
            keys_flat.push(SortKey::new(v));
        }
    }

    // Returns Ordering::Less when `lhs` should come before `rhs` in the
    // final sorted output. Byte-for-byte identical to the loop inside
    // `sort_records_by_order_by_with_db`.
    let row_cmp = |a: usize, b: usize| -> Ordering {
        let la = a * kc;
        let lb = b * kc;
        for (j, clause) in order_by.iter().enumerate() {
            let lk = &keys_flat[la + j];
            let rk = &keys_flat[lb + j];
            let ord = match (&lk.abbrev, &rk.abbrev, &lk.value, &rk.value) {
                (Some(la_a), Some(ra), Some(Value::Text(ls)), Some(Value::Text(rs))) => {
                    match la_a.cmp(ra) {
                        Ordering::Equal => ls.as_ref().cmp(rs.as_ref()),
                        other => other,
                    }
                }
                _ => compare_runtime_optional_values(
                    &|value| host.secret_query_input(value),
                    lk.value.as_ref(),
                    rk.value.as_ref(),
                    clause.nulls_first,
                ),
            };
            if ord != Ordering::Equal {
                return if clause.ascending { ord } else { ord.reverse() };
            }
        }
        Ordering::Equal
    };

    let mut idxs: Vec<usize> = (0..n).collect();
    // Partition k smallest by (row_cmp, idx) — strict total order makes
    // the unstable partition deterministic and equivalent to stable sort
    // followed by truncate(k).
    idxs.select_nth_unstable_by(k - 1, |&a, &b| row_cmp(a, b).then_with(|| a.cmp(&b)));
    idxs.truncate(k);
    idxs.sort_by(|&a, &b| row_cmp(a, b).then_with(|| a.cmp(&b)));

    let orig: Vec<_> = std::mem::take(records);
    let mut out = Vec::with_capacity(k);
    for i in idxs {
        out.push(orig[i].clone());
    }
    *records = out;
    Ok(())
}

pub fn compare_runtime_optional_values(
    secret_input: &impl Fn(Value) -> Option<Value>,
    left: Option<&Value>,
    right: Option<&Value>,
    nulls_first: bool,
) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(_), None) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
        (Some(Value::Null), Some(_)) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(_), Some(Value::Null)) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(left), Some(right)) => {
            runtime_partial_cmp(left, right, secret_input).unwrap_or(Ordering::Equal)
        }
    }
}
