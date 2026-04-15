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

/// Attempt to resolve a range/between filter to a list of entity ids via
/// the sorted numeric index. Returns `None` when the filter is not
/// applicable (different shape, unsupported type, missing index, or too
/// many results) — the caller is expected to fall through to a full
/// scan in that case.
///
/// When `limit` is `Some(n)`, the scan stops after collecting `n` IDs
/// (matching PG's sorted-index + LIMIT pushdown behaviour). The 5 000-row
/// break-even cap is only enforced when `limit` is `None`.
pub(crate) fn try_sorted_index_lookup(
    filter: &Filter,
    table: &str,
    idx_store: &IndexStore,
    limit: Option<usize>,
) -> Option<Vec<EntityId>> {
    match filter {
        Filter::Between { field, low, high } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if !idx_store.sorted.has_index(table, col) {
                return None;
            }
            let lo = super::super::index_store::value_to_sorted_numeric_key(low)?;
            let hi = super::super::index_store::value_to_sorted_numeric_key(high)?;
            // Use the effective cap: query LIMIT if present, otherwise a static break-even
            // cap. For BETWEEN on a 1M-row table with ~16% selectivity (~160K matches),
            // the sorted-index path (160K BTree traversal + 160K HashMap lookups) outperforms
            // a full scan of 1M rows. The cap is set to 200_001 (one above the threshold)
            // so we can detect "too many" without collecting the entire result set.
            // At >200K matches the parallel full-scan wins on cache locality.
            const BREAK_EVEN_CAP: usize = 200_000;
            let cap = limit.unwrap_or(BREAK_EVEN_CAP + 1);
            let ids = idx_store
                .sorted
                .range_lookup_limited(table, col, lo, hi, cap)?;
            if limit.is_none() && ids.len() > BREAK_EVEN_CAP {
                return None; // Full scan cheaper for very large result sets without LIMIT
            }
            Some(ids)
        }
        Filter::Compare { field, op, value }
            if matches!(
                *op,
                CompareOp::Lt | CompareOp::Le | CompareOp::Gt | CompareOp::Ge
            ) =>
        {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            if !idx_store.sorted.has_index(table, col) {
                return None;
            }
            let threshold = super::super::index_store::value_to_sorted_numeric_key(value)?;
            // Same cap logic as BETWEEN above.
            const BREAK_EVEN_CAP: usize = 200_000;
            let cap = limit.unwrap_or(BREAK_EVEN_CAP + 1);
            let ids = match *op {
                CompareOp::Lt => idx_store
                    .sorted
                    .lt_lookup_limited(table, col, threshold, cap)?,
                CompareOp::Le => idx_store
                    .sorted
                    .le_lookup_limited(table, col, threshold, cap)?,
                CompareOp::Gt => idx_store
                    .sorted
                    .gt_lookup_limited(table, col, threshold, cap)?,
                CompareOp::Ge => idx_store
                    .sorted
                    .ge_lookup_limited(table, col, threshold, cap)?,
                _ => unreachable!("non-range compare op guarded above"),
            };
            if limit.is_none() && ids.len() > BREAK_EVEN_CAP {
                return None; // Full scan cheaper for very large result sets without LIMIT
            }
            Some(ids)
        }
        Filter::And(left, right) => {
            // Extract a range predicate from either side of the AND.
            // The sorted index narrows the candidate set; the caller applies the
            // full filter on the surviving rows, so it's safe to return a superset here.
            try_sorted_index_lookup(left, table, idx_store, limit)
                .or_else(|| try_sorted_index_lookup(right, table, idx_store, limit))
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
        if idx_store.find_index_for_column(table, &col).is_some()
            && is_range_filter_with_sorted_index(right, table, idx_store)
        {
            return Some((col, bytes, right.as_ref()));
        }
    }

    // Try right = equality (hash index), left = range (sorted index)
    if let Some((col, bytes)) = super::helpers::extract_index_candidate(right) {
        if idx_store.find_index_for_column(table, &col).is_some()
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
) -> Option<Vec<EntityId>> {
    match range_filter {
        Filter::Between { field, low, high } => {
            let col = match field {
                FieldRef::TableColumn { column, .. } => column.as_str(),
                _ => return None,
            };
            let lo = super::super::index_store::value_to_sorted_numeric_key(low)?;
            let hi = super::super::index_store::value_to_sorted_numeric_key(high)?;
            idx_store
                .sorted
                .range_filtered_by_set(table, col, lo, hi, filter_set, limit)
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
            let threshold = super::super::index_store::value_to_sorted_numeric_key(value)?;
            match *op {
                CompareOp::Gt => idx_store
                    .sorted
                    .gt_filtered_by_set(table, col, threshold, filter_set, limit),
                CompareOp::Ge => idx_store
                    .sorted
                    .ge_filtered_by_set(table, col, threshold, filter_set, limit),
                CompareOp::Lt => idx_store
                    .sorted
                    .lt_filtered_by_set(table, col, threshold, filter_set, limit),
                CompareOp::Le => idx_store
                    .sorted
                    .le_filtered_by_set(table, col, threshold, filter_set, limit),
                _ => unreachable!(),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sort_ids(ids: Vec<EntityId>) -> Vec<u64> {
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

        assert!(try_sorted_index_lookup(&filter, "metrics", &idx_store, None).is_none());
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
            .expect("should use sorted index");
        assert_eq!(all.len(), 1000);

        // With limit=10: should return exactly 10 IDs (the lowest-valued ones)
        let limited = try_sorted_index_lookup(&filter, "t", &idx_store, Some(10))
            .expect("should use sorted index with limit");
        assert_eq!(limited.len(), 10);

        // Returned IDs must be a subset of valid IDs
        let all_set: std::collections::HashSet<u64> = all.iter().map(|id| id.raw()).collect();
        for id in &limited {
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
            try_sorted_index_lookup(&filter, "t", &idx_store, None).is_none(),
            "should fall back to full scan when > 200K results and no limit"
        );

        // With limit=100: should succeed and return exactly 100 IDs
        let limited = try_sorted_index_lookup(&filter, "t", &idx_store, Some(100))
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
            .expect("should use sorted index with limit");
        assert_eq!(limited.len(), 50);
    }
}
