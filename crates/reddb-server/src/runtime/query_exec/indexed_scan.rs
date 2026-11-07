//! Sorted-index lookup helper for the table executor.
//!
//! `try_sorted_index_lookup` turns range filters (`BETWEEN`, `<`, `<=`,
//! `>`, `>=`) into O(log N) probes against the sorted numeric index
//! instead of full table scans. It refuses to help when the filter is
//! not range-shaped or when the result set would be larger than the
//! break-even point (≈5000 rows) where full scan wins.
//!
//! Split out of `query_exec.rs` with its unit tests co-located so the
//! regression coverage for numeric boundaries (`i64::MIN`, `i64::MAX`,
//! `u64 > i64::MAX`) lives next to the logic it guards.

use super::super::index_store::IndexStore;
use super::*;

use crate::storage::unified::hash_index::HashCandidates;
use std::ops::Bound::{Excluded, Included, Unbounded};
type CandidateResult = crate::RedDBResult<Option<HashCandidates>>;

/// None permits a source scan for an unsupported shape. Admission failures are
/// authoritative: callers must not turn them into a second, unbudgeted attempt.
pub(crate) fn try_sorted_index_lookup(
    filter: &Filter,
    table: &str,
    idx_store: &IndexStore,
    limit: Option<usize>,
) -> CandidateResult {
    match filter {
        Filter::Between { .. }
        | Filter::Compare {
            op: CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge,
            ..
        } => {
            const BREAK_EVEN_CAP: usize = 200_000;
            let result = range_filter_candidates(
                filter,
                table,
                idx_store,
                limit.unwrap_or(BREAK_EVEN_CAP + 1),
                None,
            )?;
            Ok(result.filter(|ids| limit.is_some() || ids.len() <= BREAK_EVEN_CAP))
        }
        Filter::In {
            field: FieldRef::TableColumn { column, .. },
            values,
        } => {
            let keys: Vec<_> = values
                .iter()
                .filter_map(super::super::index_store::value_to_sorted_key)
                .collect();
            if keys.is_empty() {
                return Ok(None);
            }
            idx_store.sorted_in_candidates(table, column, keys, limit.unwrap_or(usize::MAX), None)
        }
        Filter::And(left, right) => {
            if let Some(ids) = try_composite_and_lookup(left, right, table, idx_store, limit)? {
                return Ok(Some(ids));
            }
            // Both complete leaf sets are required before applying the final
            // LIMIT; truncating either side can lose valid intersections.
            let left_ids = try_sorted_index_lookup_leaf(left, table, idx_store, None)?;
            let right_ids = try_sorted_index_lookup_leaf(right, table, idx_store, None)?;
            match (left_ids, right_ids) {
                (Some(left), Some(right)) => {
                    intersect_sorted_id_sets(idx_store, left, right, limit.unwrap_or(usize::MAX))
                        .map(Some)
                }
                (Some(ids), None) | (None, Some(ids)) => Ok(Some(ids)),
                (None, None) => match try_sorted_index_lookup(left, table, idx_store, limit)? {
                    Some(ids) => Ok(Some(ids)),
                    None => try_sorted_index_lookup(right, table, idx_store, limit),
                },
            }
        }
        _ => Ok(None),
    }
}

fn range_filter_candidates(
    filter: &Filter,
    table: &str,
    idx_store: &IndexStore,
    limit: usize,
    filter_set: Option<&std::collections::HashSet<u64>>,
) -> CandidateResult {
    use super::super::index_store::value_to_sorted_key;
    match filter {
        Filter::Between {
            field: FieldRef::TableColumn { column, .. },
            low,
            high,
        } => {
            if let (Some(low_key), Some(high_key)) =
                (value_to_sorted_key(low), value_to_sorted_key(high))
            {
                if let Some(ids) = idx_store.sorted_range_candidates(
                    table,
                    column,
                    (Included(low_key), Included(high_key)),
                    limit,
                    false,
                    filter_set,
                )? {
                    return Ok(Some(ids));
                }
            }
            if filter_set.is_some() {
                return Ok(None);
            }
            try_mixed_integral_between_lookup(table, column, low, high, idx_store, limit)
        }
        Filter::Compare {
            field: FieldRef::TableColumn { column, .. },
            op,
            value,
        } => {
            if let Some(key) = value_to_sorted_key(value) {
                let bounds = match op {
                    CompareOp::Lt => (Unbounded, Excluded(key)),
                    CompareOp::Le => (Unbounded, Included(key)),
                    CompareOp::Gt => (Excluded(key), Unbounded),
                    CompareOp::Ge => (Included(key), Unbounded),
                    _ => return Ok(None),
                };
                if let Some(ids) = idx_store
                    .sorted_range_candidates(table, column, bounds, limit, false, filter_set)?
                {
                    return Ok(Some(ids));
                }
            }
            if filter_set.is_some() {
                return Ok(None);
            }
            try_mixed_integral_compare_lookup(table, column, *op, value, idx_store, limit)
        }
        _ => Ok(None),
    }
}

fn try_composite_and_lookup(
    left: &Filter,
    right: &Filter,
    table: &str,
    idx_store: &IndexStore,
    limit: Option<usize>,
) -> CandidateResult {
    let Some((columns, prefix, low, high)) = composite_lookup_parts(left, right, table, idx_store)
    else {
        return Ok(None);
    };
    idx_store.composite_range_candidates(
        table,
        &columns,
        &[prefix],
        low,
        high,
        limit.unwrap_or(200_000),
    )
}

/// Detect `And(Eq(col_a), Range(col_b))` / `And(Range(col_b), Eq(col_a))`
/// and resolve via a composite sorted index on `(col_a, col_b)` when one
/// is registered. Returns `None` if the shape doesn't fit or no composite
/// covers both columns in order.
fn composite_lookup_parts(
    left: &Filter,
    right: &Filter,
    table: &str,
    idx_store: &IndexStore,
) -> Option<(
    Vec<String>,
    reddb_types::CanonicalKey,
    reddb_types::CanonicalKey,
    reddb_types::CanonicalKey,
)> {
    use reddb_rql::ast::FieldRef;
    use reddb_types::CanonicalKey;

    // Extract equality side (col, value) and range side (col, low, high).
    let extract_eq = |f: &Filter| -> Option<(String, CanonicalKey)> {
        let Filter::Compare {
            field,
            op: CompareOp::Eq,
            value,
        } = f
        else {
            return None;
        };
        let col = match field {
            FieldRef::TableColumn { column, .. } => column.clone(),
            _ => return None,
        };
        let key = super::super::index_store::value_to_sorted_key(value)?;
        Some((col, key))
    };
    let extract_range = |f: &Filter| -> Option<(String, CanonicalKey, CanonicalKey)> {
        match f {
            Filter::Between { field, low, high } => {
                let col = match field {
                    FieldRef::TableColumn { column, .. } => column.clone(),
                    _ => return None,
                };
                let lo = super::super::index_store::value_to_sorted_key(low)?;
                let hi = super::super::index_store::value_to_sorted_key(high)?;
                Some((col, lo, hi))
            }
            Filter::Compare { field, op, value } => {
                let col = match field {
                    FieldRef::TableColumn { column, .. } => column.clone(),
                    _ => return None,
                };
                let pivot = super::super::index_store::value_to_sorted_key(value)?;
                // Saturating bounds for the pivot's numeric family so we
                // can express `age > N` as `range(N-exclusive ..= MAX)`
                // against the composite BTreeMap.
                use reddb_types::{CanonicalKey, CanonicalKeyFamily};
                let (family, is_signed) = match &pivot {
                    CanonicalKey::Signed(f, _) => (*f, true),
                    CanonicalKey::Unsigned(f, _) => (*f, false),
                    _ => return None,
                };
                let min = if is_signed {
                    CanonicalKey::Signed(family, i64::MIN)
                } else {
                    CanonicalKey::Unsigned(family, 0)
                };
                let max = if is_signed {
                    CanonicalKey::Signed(family, i64::MAX)
                } else {
                    CanonicalKey::Unsigned(family, u64::MAX)
                };
                let (lo, hi) = match (op, &pivot) {
                    (CompareOp::Gt, CanonicalKey::Signed(_, v)) => {
                        (CanonicalKey::Signed(family, v.checked_add(1)?), max)
                    }
                    (CompareOp::Gt, CanonicalKey::Unsigned(_, v)) => {
                        (CanonicalKey::Unsigned(family, v.checked_add(1)?), max)
                    }
                    (CompareOp::Ge, _) => (pivot.clone(), max),
                    (CompareOp::Lt, CanonicalKey::Signed(_, v)) => {
                        (min, CanonicalKey::Signed(family, v.checked_sub(1)?))
                    }
                    (CompareOp::Lt, CanonicalKey::Unsigned(_, v)) => {
                        (min, CanonicalKey::Unsigned(family, v.checked_sub(1)?))
                    }
                    (CompareOp::Le, _) => (min, pivot.clone()),
                    _ => return None,
                };
                Some((col, lo, hi))
            }
            _ => None,
        }
    };

    let (eq_col, eq_key, rng_col, rng_low, rng_high) =
        match (extract_eq(left), extract_range(right)) {
            (Some((ec, ek)), Some((rc, rl, rh))) => (ec, ek, rc, rl, rh),
            _ => match (extract_eq(right), extract_range(left)) {
                (Some((ec, ek)), Some((rc, rl, rh))) => (ec, ek, rc, rl, rh),
                _ => return None,
            },
        };

    let cols = vec![eq_col, rng_col];
    if !idx_store.sorted.has_composite_index(table, &cols) {
        return None;
    }
    Some((cols, eq_key, rng_low, rng_high))
}

/// Like `try_sorted_index_lookup` but only matches leaf predicates (not AND/OR wrappers).
/// Used by Phase 6 AND-of-sorted to prevent double-counting nested ANDs.
///
/// Falls through to hash-eq for `Eq`/`In` leaves when the sorted path doesn't
/// apply — gives the `And` intersector a small candidate set from the equality
/// side (e.g. `city='NYC'`) that it can cheaply cross against the sorted
/// range side (e.g. `age > 30`). Without this path, an `Eq` on a hash-only
/// indexed column contributes nothing and the AND returns the full range
/// side's result, which can be 50k+ rows for low-selectivity ranges.
fn try_sorted_index_lookup_leaf(
    filter: &Filter,
    table: &str,
    idx_store: &IndexStore,
    limit: Option<usize>,
) -> CandidateResult {
    match filter {
        Filter::And(_, _) | Filter::Or(_, _) | Filter::Not(_) => Ok(None),
        Filter::Compare {
            op: CompareOp::Eq, ..
        }
        | Filter::In { .. } => match try_sorted_index_lookup(filter, table, idx_store, limit)? {
            Some(ids) => Ok(Some(ids)),
            None => super::helpers::try_resident_hash_eq_lookup(filter, table, idx_store),
        },
        other => try_sorted_index_lookup(other, table, idx_store, limit),
    }
}

/// Preserve the larger side's order and the existing hash-set fast path.
/// Both candidate owners stay charged while the set and final output coexist.
pub(crate) fn intersect_sorted_id_sets(
    idx_store: &IndexStore,
    a: HashCandidates,
    b: HashCandidates,
    limit: usize,
) -> crate::RedDBResult<HashCandidates> {
    if a.is_empty() || b.is_empty() || limit == 0 {
        return Ok(Vec::new().into());
    }
    let (larger, smaller) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let set = idx_store.candidate_set(smaller.iter().map(|id| id.raw()))?;
    idx_store.collect_candidates(
        larger
            .iter()
            .copied()
            .filter(|id| set.contains(&id.raw()))
            .take(limit),
    )
}

type IntegralBoundsResult<T> = Result<Option<(T, T)>, ()>;

fn try_mixed_integral_between_lookup(
    table: &str,
    column: &str,
    low: &Value,
    high: &Value,
    idx_store: &IndexStore,
    limit: usize,
) -> CandidateResult {
    if !idx_store
        .sorted
        .supports_mixed_integral_ranges(table, column)
    {
        return Ok(None);
    }

    let (Ok(signed_bounds), Ok(unsigned_bounds)) = (
        signed_between_bounds(low, high),
        unsigned_between_bounds(low, high),
    ) else {
        return Ok(None);
    };
    collect_integral_family_ranges(
        table,
        column,
        signed_bounds,
        unsigned_bounds,
        idx_store,
        limit,
    )
}

fn try_mixed_integral_compare_lookup(
    table: &str,
    column: &str,
    op: CompareOp,
    value: &Value,
    idx_store: &IndexStore,
    limit: usize,
) -> CandidateResult {
    if !idx_store
        .sorted
        .supports_mixed_integral_ranges(table, column)
    {
        return Ok(None);
    }

    let (Ok(signed_bounds), Ok(unsigned_bounds)) = (
        signed_compare_bounds(op, value),
        unsigned_compare_bounds(op, value),
    ) else {
        return Ok(None);
    };
    collect_integral_family_ranges(
        table,
        column,
        signed_bounds,
        unsigned_bounds,
        idx_store,
        limit,
    )
}

fn collect_integral_family_ranges(
    table: &str,
    column: &str,
    signed_bounds: Option<(i64, i64)>,
    unsigned_bounds: Option<(u64, u64)>,
    idx_store: &IndexStore,
    limit: usize,
) -> CandidateResult {
    let signed = if let Some((low, high)) = signed_bounds {
        idx_store.sorted_range_candidates(
            table,
            column,
            (
                Included(reddb_types::CanonicalKey::Signed(
                    reddb_types::CanonicalKeyFamily::Integer,
                    low,
                )),
                Included(reddb_types::CanonicalKey::Signed(
                    reddb_types::CanonicalKeyFamily::Integer,
                    high,
                )),
            ),
            limit,
            true,
            None,
        )?
    } else {
        Some(Vec::new().into())
    };
    let Some(signed) = signed else {
        return Ok(None);
    };
    let remaining = limit.saturating_sub(signed.len());
    let unsigned = if let Some((low, high)) = unsigned_bounds.filter(|_| remaining > 0) {
        idx_store.sorted_range_candidates(
            table,
            column,
            (
                Included(reddb_types::CanonicalKey::Unsigned(
                    reddb_types::CanonicalKeyFamily::UnsignedInteger,
                    low,
                )),
                Included(reddb_types::CanonicalKey::Unsigned(
                    reddb_types::CanonicalKeyFamily::UnsignedInteger,
                    high,
                )),
            ),
            remaining,
            true,
            None,
        )?
    } else {
        Some(Vec::new().into())
    };
    let Some(unsigned) = unsigned else {
        return Ok(None);
    };
    if unsigned.is_empty() {
        return Ok(Some(signed));
    }
    if signed.is_empty() {
        return Ok(Some(unsigned));
    }
    idx_store
        .collect_candidates(signed.iter().chain(unsigned.iter()).copied())
        .map(Some)
}

fn signed_between_bounds(low: &Value, high: &Value) -> IntegralBoundsResult<i64> {
    let lower = match low {
        Value::Integer(value) => *value,
        Value::UnsignedInteger(value) => match i64::try_from(*value) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        },
        _ => return Err(()),
    };
    let upper = match high {
        Value::Integer(value) => *value,
        Value::UnsignedInteger(value) => (*value).min(i64::MAX as u64) as i64,
        _ => return Err(()),
    };
    Ok((lower <= upper).then_some((lower, upper)))
}

fn unsigned_between_bounds(low: &Value, high: &Value) -> IntegralBoundsResult<u64> {
    let lower = match low {
        Value::Integer(value) if *value < 0 => 0,
        Value::Integer(value) => *value as u64,
        Value::UnsignedInteger(value) => *value,
        _ => return Err(()),
    };
    let upper = match high {
        Value::Integer(value) if *value < 0 => return Ok(None),
        Value::Integer(value) => *value as u64,
        Value::UnsignedInteger(value) => *value,
        _ => return Err(()),
    };
    Ok((lower <= upper).then_some((lower, upper)))
}

fn signed_compare_bounds(op: CompareOp, value: &Value) -> IntegralBoundsResult<i64> {
    match op {
        CompareOp::Lt => match value {
            Value::Integer(value) => match value.checked_sub(1) {
                Some(upper) => Ok(Some((i64::MIN, upper))),
                None => Ok(None),
            },
            Value::UnsignedInteger(0) => Ok(Some((i64::MIN, -1))),
            Value::UnsignedInteger(value) => {
                let upper = value.saturating_sub(1).min(i64::MAX as u64) as i64;
                Ok(Some((i64::MIN, upper)))
            }
            _ => Err(()),
        },
        CompareOp::Le => match value {
            Value::Integer(value) => Ok(Some((i64::MIN, *value))),
            Value::UnsignedInteger(value) => {
                Ok(Some((i64::MIN, (*value).min(i64::MAX as u64) as i64)))
            }
            _ => Err(()),
        },
        CompareOp::Gt => match value {
            Value::Integer(value) => match value.checked_add(1) {
                Some(lower) => Ok(Some((lower, i64::MAX))),
                None => Ok(None),
            },
            Value::UnsignedInteger(value) if *value >= i64::MAX as u64 => Ok(None),
            Value::UnsignedInteger(value) => Ok(Some(((*value as i64) + 1, i64::MAX))),
            _ => Err(()),
        },
        CompareOp::Ge => match value {
            Value::Integer(value) => Ok(Some((*value, i64::MAX))),
            Value::UnsignedInteger(value) if *value > i64::MAX as u64 => Ok(None),
            Value::UnsignedInteger(value) => Ok(Some((*value as i64, i64::MAX))),
            _ => Err(()),
        },
        _ => Err(()),
    }
}

fn unsigned_compare_bounds(op: CompareOp, value: &Value) -> IntegralBoundsResult<u64> {
    match op {
        CompareOp::Lt => match value {
            Value::Integer(value) if *value <= 0 => Ok(None),
            Value::Integer(value) => Ok(Some((0, (*value as u64) - 1))),
            Value::UnsignedInteger(0) => Ok(None),
            Value::UnsignedInteger(value) => Ok(Some((0, value - 1))),
            _ => Err(()),
        },
        CompareOp::Le => match value {
            Value::Integer(value) if *value < 0 => Ok(None),
            Value::Integer(value) => Ok(Some((0, *value as u64))),
            Value::UnsignedInteger(value) => Ok(Some((0, *value))),
            _ => Err(()),
        },
        CompareOp::Gt => match value {
            Value::Integer(value) if *value < 0 => Ok(Some((0, u64::MAX))),
            Value::Integer(value) if *value == i64::MAX => {
                Ok(Some((i64::MAX as u64 + 1, u64::MAX)))
            }
            Value::Integer(value) => Ok(Some(((*value as u64) + 1, u64::MAX))),
            Value::UnsignedInteger(value) if *value == u64::MAX => Ok(None),
            Value::UnsignedInteger(value) => Ok(Some((value + 1, u64::MAX))),
            _ => Err(()),
        },
        CompareOp::Ge => match value {
            Value::Integer(value) if *value < 0 => Ok(Some((0, u64::MAX))),
            Value::Integer(value) => Ok(Some((*value as u64, u64::MAX))),
            Value::UnsignedInteger(value) => Ok(Some((*value, u64::MAX))),
            _ => Err(()),
        },
        _ => Err(()),
    }
}

/// Covered-query optimization: when the filter is a simple range/IN predicate on a sorted
/// index column, and the projection only requests that column, return the BTree keys directly
/// as Values — no entity fetch needed.
///
/// Returns `Some(Vec<Value>)` when the query is covered; `None` to fall through to entity fetch.
pub(crate) fn try_covered_sorted_index_query(
    filter: &Filter,
    table: &str,
    idx_store: &IndexStore,
    explicit_cols: &[String],
    limit: usize,
) -> Option<Vec<reddb_types::Value>> {
    // Only covers single-column projections on the exact indexed column.
    if explicit_cols.len() != 1 {
        return None;
    }
    let proj_col = &explicit_cols[0];

    match filter {
        Filter::Between { field, low, high } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if col != proj_col.as_str() {
                return None;
            }
            let lo = super::super::index_store::value_to_sorted_key(low)?;
            let hi = super::super::index_store::value_to_sorted_key(high)?;
            let keys = idx_store
                .sorted
                .range_lookup_values(table, col, lo, hi, limit)?;
            Some(
                keys.into_iter()
                    .map(super::super::index_store::sorted_key_to_value)
                    .collect(),
            )
        }
        Filter::Compare { field, op, value }
            if matches!(
                *op,
                CompareOp::Gt | CompareOp::Ge | CompareOp::Lt | CompareOp::Le
            ) =>
        {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if col != proj_col.as_str() {
                return None;
            }
            let threshold = super::super::index_store::value_to_sorted_key(value)?;
            let keys = idx_store
                .sorted
                .compare_lookup_values(table, col, threshold, op, limit)?;
            Some(
                keys.into_iter()
                    .map(super::super::index_store::sorted_key_to_value)
                    .collect(),
            )
        }
        Filter::In { field, values } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if col != proj_col.as_str() {
                return None;
            }
            let keys: Vec<reddb_types::CanonicalKey> = values
                .iter()
                .filter_map(super::super::index_store::value_to_sorted_key)
                .collect();
            if keys.is_empty() {
                return None;
            }
            let keys = idx_store
                .sorted
                .in_lookup_values(table, col, &keys, limit)?;
            Some(
                keys.into_iter()
                    .map(super::super::index_store::sorted_key_to_value)
                    .collect(),
            )
        }
        _ => None,
    }
}

// ─── Cross-index AND intersection helpers ────────────────────────────────────

/// Check if `filter` is a range predicate (BETWEEN / Gt / Ge / Lt / Le) that
/// has a sorted index on the referenced column for the given table.
fn is_range_filter_with_sorted_index(filter: &Filter, table: &str, idx_store: &IndexStore) -> bool {
    match filter {
        Filter::Between { field, .. } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return false,
            };
            idx_store.sorted.has_index(table, col)
        }
        Filter::Compare { field, op, .. }
            if matches!(
                *op,
                CompareOp::Gt | CompareOp::Ge | CompareOp::Lt | CompareOp::Le
            ) =>
        {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return false,
            };
            idx_store.sorted.has_index(table, col)
        }
        _ => false,
    }
}

/// Extract cross-index predicates from a compound AND filter.
///
/// Returns `(eq_column, eq_value_bytes, range_filter)` when the filter tree
/// contains an equality predicate on a column with a hash index AND a range
/// predicate on a column with a sorted index, joined by AND.
///
/// Used by the bitmap-AND path to perform:
///   hash_lookup(eq_col = val) → HashSet  ∩  sorted_range(range_col op val2)
/// instead of fetching all hash candidates and filtering in-memory.
pub(crate) fn extract_cross_index_predicates<'a>(
    filter: &'a Filter,
    table: &str,
    idx_store: &IndexStore,
) -> Option<(String, Vec<u8>, &'a Filter)> {
    let Filter::And(left, right) = filter else {
        return None;
    };

    // Try left = equality (hash index), right = range (sorted index)
    if let Some((col, bytes)) = super::helpers::extract_index_candidate(left) {
        if idx_store.find_hash_index_for_column(table, &col).is_some()
            && is_range_filter_with_sorted_index(right, table, idx_store)
        {
            return Some((col, bytes, right.as_ref()));
        }
    }

    // Try right = equality (hash index), left = range (sorted index)
    if let Some((col, bytes)) = super::helpers::extract_index_candidate(right) {
        if idx_store.find_hash_index_for_column(table, &col).is_some()
            && is_range_filter_with_sorted_index(left, table, idx_store)
        {
            return Some((col, bytes, left.as_ref()));
        }
    }

    // Recurse into nested AND
    extract_cross_index_predicates(left, table, idx_store)
        .or_else(|| extract_cross_index_predicates(right, table, idx_store))
}

/// Find any range predicate in an AND-tree that has a sorted index on its column.
///
/// Unlike `extract_cross_index_predicates`, this does NOT require a paired equality
/// predicate — it is used by the TID bitmap path after the equality intersection is
/// already built, to further narrow via a sorted range scan.
pub(crate) fn find_range_predicate_with_sorted_index<'a>(
    filter: &'a Filter,
    table: &str,
    idx_store: &IndexStore,
) -> Option<&'a Filter> {
    if is_range_filter_with_sorted_index(filter, table, idx_store) {
        return Some(filter);
    }
    if let Filter::And(left, right) = filter {
        find_range_predicate_with_sorted_index(left, table, idx_store)
            .or_else(|| find_range_predicate_with_sorted_index(right, table, idx_store))
    } else {
        None
    }
}

/// Sorted-range scan filtered by a pre-built candidate `HashSet` (from hash index).
/// Implements PG-style bitmap AND: iterate the BTree range, only collect IDs in the
/// hash set. Stops after `limit` results.
/// Returns None when the range filter is unsupported or no sorted index exists.
pub(crate) fn try_sorted_index_filtered_by_set(
    range_filter: &Filter,
    table: &str,
    idx_store: &IndexStore,
    filter_set: &std::collections::HashSet<u64>,
    limit: usize,
) -> CandidateResult {
    if let Filter::In {
        field: FieldRef::TableColumn { column, .. },
        values,
    } = range_filter
    {
        let keys: Vec<_> = values
            .iter()
            .filter_map(super::super::index_store::value_to_sorted_key)
            .collect();
        if keys.is_empty() {
            return Ok(None);
        }
        return idx_store.sorted_in_candidates(table, column, keys, limit, Some(filter_set));
    }
    range_filter_candidates(range_filter, table, idx_store, limit, Some(filter_set))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sort_ids(ids: impl IntoIterator<Item = EntityId>) -> Vec<u64> {
        let mut ids: Vec<u64> = ids.into_iter().map(|id| id.raw()).collect();
        ids.sort_unstable();
        ids
    }

    fn value_for_column<'a>(fields: &'a [(String, Value)], column: &str) -> Option<&'a Value> {
        fields
            .iter()
            .find(|(field, _)| field == column)
            .map(|(_, value)| value)
    }

    fn expected_ids(
        entities: &[(EntityId, Vec<(String, Value)>)],
        filter: &Filter,
        column: &str,
    ) -> Vec<EntityId> {
        entities
            .iter()
            .filter_map(|(entity_id, fields)| {
                let candidate = value_for_column(fields, column)?;
                let matches = match filter {
                    Filter::Compare { op, value, .. } => {
                        compare_runtime_values(candidate, value, *op)
                    }
                    Filter::Between { low, high, .. } => {
                        compare_runtime_values(candidate, low, CompareOp::Ge)
                            && compare_runtime_values(candidate, high, CompareOp::Le)
                    }
                    _ => false,
                };
                matches.then_some(*entity_id)
            })
            .collect()
    }

    #[test]
    fn test_try_sorted_index_lookup_matches_full_scan_for_integral_boundaries() {
        let idx_store = IndexStore::new();
        let entities = vec![
            (
                EntityId::new(1),
                vec![("n".to_string(), Value::Integer(i64::MIN))],
            ),
            (
                EntityId::new(2),
                vec![("n".to_string(), Value::Integer(-1))],
            ),
            (
                EntityId::new(3),
                vec![("n".to_string(), Value::Integer(i64::MAX))],
            ),
            (
                EntityId::new(4),
                vec![("n".to_string(), Value::UnsignedInteger(i64::MAX as u64 + 1))],
            ),
            (
                EntityId::new(5),
                vec![("n".to_string(), Value::UnsignedInteger(u64::MAX))],
            ),
        ];
        idx_store.sorted.build_index("numbers", "n", &entities);

        let filters = vec![
            Filter::Compare {
                field: FieldRef::column("numbers", "n"),
                op: CompareOp::Le,
                value: Value::Integer(i64::MIN),
            },
            Filter::Compare {
                field: FieldRef::column("numbers", "n"),
                op: CompareOp::Lt,
                value: Value::UnsignedInteger(0),
            },
            Filter::Compare {
                field: FieldRef::column("numbers", "n"),
                op: CompareOp::Gt,
                value: Value::Integer(i64::MAX),
            },
            Filter::Compare {
                field: FieldRef::column("numbers", "n"),
                op: CompareOp::Ge,
                value: Value::UnsignedInteger(i64::MAX as u64 + 1),
            },
            Filter::Between {
                field: FieldRef::column("numbers", "n"),
                low: Value::Integer(i64::MAX),
                high: Value::UnsignedInteger(i64::MAX as u64 + 1),
            },
        ];

        for filter in filters {
            let indexed = try_sorted_index_lookup(&filter, "numbers", &idx_store, None)
                .expect("candidate admission")
                .expect("lookup should use sorted index");
            let expected = expected_ids(&entities, &filter, "n");
            assert_eq!(sort_ids(indexed), sort_ids(expected), "filter={filter:?}");
        }
    }

    #[test]
    fn test_try_sorted_index_lookup_falls_back_when_float_values_are_present() {
        let idx_store = IndexStore::new();
        let entities = vec![
            (
                EntityId::new(1),
                vec![("score".to_string(), Value::Integer(10))],
            ),
            (
                EntityId::new(2),
                vec![("score".to_string(), Value::Float(10.5))],
            ),
        ];
        idx_store.sorted.build_index("metrics", "score", &entities);

        let filter = Filter::Compare {
            field: FieldRef::column("metrics", "score"),
            op: CompareOp::Ge,
            value: Value::Integer(10),
        };

        assert!(
            try_sorted_index_lookup(&filter, "metrics", &idx_store, None)
                .expect("candidate admission")
                .is_none()
        );
    }

    #[test]
    fn test_composite_city_age_lookup_matches_filtered_shape() {
        let idx_store = IndexStore::new();
        let columns = vec!["city".to_string(), "age".to_string()];
        let entities = vec![
            (
                EntityId::new(1),
                vec![
                    ("city".to_string(), Value::text("NYC".to_string())),
                    ("age".to_string(), Value::Integer(25)),
                ],
            ),
            (
                EntityId::new(2),
                vec![
                    ("city".to_string(), Value::text("LA".to_string())),
                    ("age".to_string(), Value::Integer(40)),
                ],
            ),
            (
                EntityId::new(3),
                vec![
                    ("city".to_string(), Value::text("NYC".to_string())),
                    ("age".to_string(), Value::Integer(35)),
                ],
            ),
            (
                EntityId::new(4),
                vec![
                    ("city".to_string(), Value::text("NYC".to_string())),
                    ("age".to_string(), Value::Integer(45)),
                ],
            ),
            (
                EntityId::new(5),
                vec![
                    ("city".to_string(), Value::text("NYC".to_string())),
                    ("age".to_string(), Value::Integer(30)),
                ],
            ),
        ];
        idx_store
            .sorted
            .build_composite("users", &columns, &entities);

        let city_eq = Filter::Compare {
            field: FieldRef::column("users", "city"),
            op: CompareOp::Eq,
            value: Value::text("NYC".to_string()),
        };
        let age_gt = Filter::Compare {
            field: FieldRef::column("users", "age"),
            op: CompareOp::Gt,
            value: Value::Integer(30),
        };

        let filter = Filter::And(Box::new(city_eq.clone()), Box::new(age_gt.clone()));
        let ids = try_sorted_index_lookup(&filter, "users", &idx_store, None)
            .expect("candidate admission")
            .expect("composite index should resolve city equality + age range");
        assert_eq!(sort_ids(ids), vec![3, 4]);

        let reversed = Filter::And(Box::new(age_gt), Box::new(city_eq));
        let ids = try_sorted_index_lookup(&reversed, "users", &idx_store, None)
            .expect("candidate admission")
            .expect("composite lookup should accept either AND order");
        assert_eq!(sort_ids(ids), vec![3, 4]);
    }

    #[test]
    fn test_limit_aware_between_stops_early() {
        let idx_store = IndexStore::new();
        // 1000 entities with age 1..=1000
        let entities: Vec<(EntityId, Vec<(String, Value)>)> = (1u64..=1000)
            .map(|i| {
                (
                    EntityId::new(i),
                    vec![("age".to_string(), Value::Integer(i as i64))],
                )
            })
            .collect();
        idx_store.sorted.build_index("t", "age", &entities);

        let filter = Filter::Between {
            field: FieldRef::column("t", "age"),
            low: Value::Integer(1),
            high: Value::Integer(1000),
        };

        // Without limit: all 1000 results fit under the 5000 cap
        let all = try_sorted_index_lookup(&filter, "t", &idx_store, None)
            .expect("candidate admission")
            .expect("should use sorted index");
        assert_eq!(all.len(), 1000);

        // With limit=10: should return exactly 10 IDs (the lowest-valued ones)
        let limited = try_sorted_index_lookup(&filter, "t", &idx_store, Some(10))
            .expect("candidate admission")
            .expect("should use sorted index with limit");
        assert_eq!(limited.len(), 10);

        // Returned IDs must be a subset of valid IDs
        let all_set: std::collections::HashSet<u64> = all.iter().map(|id| id.raw()).collect();
        for id in limited.iter() {
            assert!(
                all_set.contains(&id.raw()),
                "limited ID {id:?} not in full result"
            );
        }
    }

    #[test]
    fn test_limit_bypasses_200k_cap_for_large_ranges() {
        let idx_store = IndexStore::new();
        // 210_000 entities — exceeds the 200K break-even cap
        let entities: Vec<(EntityId, Vec<(String, Value)>)> = (1u64..=210_000)
            .map(|i| {
                (
                    EntityId::new(i),
                    vec![("score".to_string(), Value::Integer(i as i64))],
                )
            })
            .collect();
        idx_store.sorted.build_index("t", "score", &entities);

        let filter = Filter::Between {
            field: FieldRef::column("t", "score"),
            low: Value::Integer(1),
            high: Value::Integer(210_000),
        };

        // Without limit: > 200K results → None (falls back to full scan)
        assert!(
            try_sorted_index_lookup(&filter, "t", &idx_store, None)
                .expect("candidate admission")
                .is_none(),
            "should fall back to full scan when > 200K results and no limit"
        );

        // With limit=100: should succeed and return exactly 100 IDs
        let limited = try_sorted_index_lookup(&filter, "t", &idx_store, Some(100))
            .expect("candidate admission")
            .expect("should use sorted index with limit even when total > 200K");
        assert_eq!(limited.len(), 100);
    }

    #[test]
    fn test_limit_aware_gt_stops_early() {
        let idx_store = IndexStore::new();
        let entities: Vec<(EntityId, Vec<(String, Value)>)> = (1u64..=500)
            .map(|i| {
                (
                    EntityId::new(i),
                    vec![("n".to_string(), Value::Integer(i as i64))],
                )
            })
            .collect();
        idx_store.sorted.build_index("t", "n", &entities);

        let filter = Filter::Compare {
            field: FieldRef::column("t", "n"),
            op: CompareOp::Gt,
            value: Value::Integer(0),
        };

        let limited = try_sorted_index_lookup(&filter, "t", &idx_store, Some(50))
            .expect("candidate admission")
            .expect("should use sorted index with limit");
        assert_eq!(limited.len(), 50);
    }
}
