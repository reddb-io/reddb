//! Runtime ordering: ORDER BY comparison, sort, and top-K.
//!
//! Extracted from `join_filter.rs` as part of the join_filter
//! directory refactor (parent re-exports the whole module).
use super::*;

pub(crate) fn compare_runtime_order(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Ordering {
    compare_runtime_order_with_db(None, left, right, clauses, table_name, table_alias)
}

pub(crate) fn compare_runtime_order_with_db(
    db: Option<&RedDB>,
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Ordering {
    for clause in clauses {
        // Fase 1.6: when the ORDER BY item is an expression (CAST,
        // arithmetic, CASE, etc.), evaluate it against each record
        // and compare the resulting Values. Bare-column clauses fall
        // back to the direct field resolver which is cheaper for the
        // common case.
        let (left_value, right_value) = if let Some(ref expr) = clause.expr {
            (
                super::expr_eval::evaluate_runtime_expr_with_db(
                    db,
                    expr,
                    left,
                    table_name,
                    table_alias,
                ),
                super::expr_eval::evaluate_runtime_expr_with_db(
                    db,
                    expr,
                    right,
                    table_name,
                    table_alias,
                ),
            )
        } else {
            (
                resolve_runtime_field(left, &clause.field, table_name, table_alias),
                resolve_runtime_field(right, &clause.field, table_name, table_alias),
            )
        };
        let ordering = compare_runtime_optional_values(
            left_value.as_ref(),
            right_value.as_ref(),
            clause.nulls_first,
        );

        if ordering != Ordering::Equal {
            return if clause.ascending {
                ordering
            } else {
                ordering.reverse()
            };
        }
    }

    runtime_record_identity_key(left).cmp(&runtime_record_identity_key(right))
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

pub(crate) fn sort_records_by_order_by(
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    sort_records_by_order_by_with_db(None, records, order_by, table_name, table_alias)
}

pub(crate) fn sort_records_by_order_by_with_db(
    db: Option<&RedDB>,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    if order_by.is_empty() || records.len() < 2 {
        return;
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
                super::expr_eval::evaluate_runtime_expr_with_db(
                    db,
                    expr,
                    rec,
                    table_name,
                    table_alias,
                )
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
}

/// Whether the `REDDB_DISABLE_TOPK` kill-switch is set. Cached so a
/// `std::env::var` lookup doesn't land in the per-query hot path once
/// the binary is warm.
pub(crate) fn topk_disabled() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("REDDB_DISABLE_TOPK")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "on" | "yes"))
            .unwrap_or(false)
    })
}

/// Chooses between full sort and quickselect-based top-k based on
/// `limit`, `offset`, and the kill-switch. When `limit` is unset or
/// `records.len()` is already close to `offset + limit`, the heap
/// overhead of top-k doesn't pay off and we fall through to the
/// existing sort path. Output is identical in every branch.
pub(crate) fn sort_or_top_k_records_with_db(
    db: Option<&RedDB>,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    offset: Option<u64>,
    limit: Option<u64>,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    if order_by.is_empty() {
        return;
    }
    let effective_k = match (offset, limit) {
        (_, None) => None,
        (off, Some(lim)) => Some(off.unwrap_or(0).saturating_add(lim) as usize),
    };
    if !topk_disabled() {
        if let Some(k) = effective_k {
            // Only take the top-k path when it actually saves work:
            // n > 2k means quickselect + sort-of-k beats full sort.
            if k > 0 && records.len() > k.saturating_mul(2) {
                top_k_records_by_order_by_with_db(
                    db,
                    records,
                    order_by,
                    k,
                    table_name,
                    table_alias,
                );
                return;
            }
        }
    }
    sort_records_by_order_by_with_db(db, records, order_by, table_name, table_alias);
}

/// Top-K variant of `sort_records_by_order_by_with_db`. For ORDER BY + LIMIT k
/// with n > k, full O(n log n) sort is wasteful — quickselect picks the
/// k smallest in O(n) average, then sort only those k. Mirrors the
/// `select_nth_unstable_by` pattern used by `query_direct`'s
/// `parse_simple_ordered_complex_select` fast path so semantics stay
/// identical across code paths.
///
/// Output is bit-identical to `sort_records_by_order_by_with_db(...) +
/// records.truncate(k)` because the comparator used here is a **strict
/// total order** — ties are broken by original index, so no two records
/// ever compare Equal and the unstable partition degenerates to stable
/// semantics.
pub(crate) fn top_k_records_by_order_by_with_db(
    db: Option<&RedDB>,
    records: &mut Vec<UnifiedRecord>,
    order_by: &[OrderByClause],
    k: usize,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) {
    if k == 0 {
        records.clear();
        return;
    }
    if order_by.is_empty() {
        records.truncate(k);
        return;
    }
    let n = records.len();
    if n <= k {
        sort_records_by_order_by_with_db(db, records, order_by, table_name, table_alias);
        return;
    }

    // Flat key buffer — see `sort_records_by_order_by_with_db` for the
    // layout rationale. One allocation of n*kc keys instead of n.
    let kc = order_by.len();
    let mut keys_flat: Vec<SortKey> = Vec::with_capacity(n * kc);
    for rec in records.iter() {
        for clause in order_by.iter() {
            let v = if let Some(ref expr) = clause.expr {
                super::expr_eval::evaluate_runtime_expr_with_db(
                    db,
                    expr,
                    rec,
                    table_name,
                    table_alias,
                )
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
}

pub(crate) fn compare_runtime_optional_values(
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
        (Some(left), Some(right)) => runtime_partial_cmp(left, right).unwrap_or(Ordering::Equal),
    }
}
