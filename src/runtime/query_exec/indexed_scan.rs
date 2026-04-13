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
pub(crate) fn try_sorted_index_lookup(
    filter: &Filter,
    table: &str,
    idx_store: &IndexStore,
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
            let ids = idx_store.sorted.range_lookup(table, col, lo, hi)?;
            // If too many results, full scan is faster than N individual get() calls
            if ids.len() > 5000 {
                return None;
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
            let ids = match *op {
                CompareOp::Lt => idx_store.sorted.lt_lookup(table, col, threshold)?,
                CompareOp::Le => idx_store.sorted.le_lookup(table, col, threshold)?,
                CompareOp::Gt => idx_store.sorted.gt_lookup(table, col, threshold)?,
                CompareOp::Ge => idx_store.sorted.ge_lookup(table, col, threshold)?,
                _ => unreachable!("non-range compare op guarded above"),
            };
            if ids.len() > 5000 {
                return None;
            }
            Some(ids)
        }
        Filter::And(_left, _right) => {
            // For AND filters, don't use sorted index — the hash index path
            // handles the equality part, and the remaining filter is evaluated
            // on the candidates. Using sorted index here returns too many results.
            None
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
            let indexed = try_sorted_index_lookup(&filter, "numbers", &idx_store)
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

        assert!(try_sorted_index_lookup(&filter, "metrics", &idx_store).is_none());
    }
}
