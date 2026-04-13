//! Statistics providers for the cost-based planner.
//!
//! Today [`super::cost::CostEstimator`] uses hardcoded constants —
//! `default_row_count = 1000`, equality selectivity `0.01`, range `0.3` —
//! and completely ignores real statistics from the storage engines. Every
//! query plan is the same shape regardless of whether a table has 10 rows
//! or 10 million.
//!
//! This module introduces [`StatsProvider`] — a trait the planner can
//! consult to substitute default constants with real, up-to-date numbers.
//! Storage components implement it to publish:
//!
//! - row counts (from table segments)
//! - column-level distinct counts / null counts (from zone maps / HLL)
//! - per-column [`crate::storage::index::IndexStats`] when an index exists
//!
//! Two implementations ship out of the box:
//!
//! - [`NullProvider`] — returns `None` for everything. The planner falls
//!   back to its heuristic defaults. Used when no stats are plumbed.
//! - [`StaticProvider`] — HashMap-backed, used by tests and by callers
//!   that gather stats once per plan (e.g. from the segment catalog).
//!
//! The planner never *requires* stats. Missing data is always a safe
//! fallback to the old heuristic path — so adding new stats is strictly
//! additive.

use std::collections::HashMap;

use super::cost::{ColumnStats, TableStats};
use crate::storage::index::IndexStats;

/// Read-only interface the planner uses to look up storage statistics.
///
/// Implementations must be cheap (O(1) or O(log n)) — the planner calls
/// these during plan construction and must not block on I/O. Pre-aggregate
/// expensive data into memory before exposing a provider.
pub trait StatsProvider: Send + Sync {
    /// Return row-count / page-count / column metadata for `table`, or
    /// `None` when stats are not available.
    fn table_stats(&self, table: &str) -> Option<TableStats>;

    /// Return per-column statistics (distinct count, null count, min/max)
    /// when available. Default implementation derives from
    /// [`StatsProvider::table_stats`] when present.
    fn column_stats(&self, table: &str, column: &str) -> Option<ColumnStats> {
        self.table_stats(table)?
            .columns
            .into_iter()
            .find(|c| c.name == column)
    }

    /// Return the [`IndexStats`] backing a secondary index on
    /// `(table, column)`, if one exists. The planner uses
    /// [`IndexStats::point_selectivity`] to derive equality selectivity
    /// instead of the `0.01` constant.
    fn index_stats(&self, table: &str, column: &str) -> Option<IndexStats>;

    /// Convenience: does a usable index exist on `(table, column)`?
    fn has_index(&self, table: &str, column: &str) -> bool {
        self.index_stats(table, column).is_some()
    }

    /// Convenience: distinct-value count for a column, via column stats or
    /// an index on that column, whichever is available.
    fn distinct_values(&self, table: &str, column: &str) -> Option<u64> {
        if let Some(cs) = self.column_stats(table, column) {
            if cs.distinct_count > 0 {
                return Some(cs.distinct_count);
            }
        }
        self.index_stats(table, column)
            .map(|s| s.distinct_keys as u64)
    }
}

/// Provider that returns `None` for everything. Planner uses its built-in
/// heuristic constants.
#[derive(Debug, Clone, Default)]
pub struct NullProvider;

impl StatsProvider for NullProvider {
    fn table_stats(&self, _table: &str) -> Option<TableStats> {
        None
    }

    fn index_stats(&self, _table: &str, _column: &str) -> Option<IndexStats> {
        None
    }
}

/// HashMap-backed provider suitable for tests and for callers who gather
/// stats once per plan.
#[derive(Debug, Clone, Default)]
pub struct StaticProvider {
    tables: HashMap<String, TableStats>,
    /// Indexes keyed by `(table, column)`.
    indexes: HashMap<(String, String), IndexStats>,
}

impl StaticProvider {
    /// Build an empty provider.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace table-level stats.
    pub fn with_table(mut self, table: impl Into<String>, stats: TableStats) -> Self {
        self.tables.insert(table.into(), stats);
        self
    }

    /// Register or replace an index on `(table, column)`.
    pub fn with_index(
        mut self,
        table: impl Into<String>,
        column: impl Into<String>,
        stats: IndexStats,
    ) -> Self {
        self.indexes.insert((table.into(), column.into()), stats);
        self
    }

    /// Mutable table insert for iterative builds.
    pub fn insert_table(&mut self, table: impl Into<String>, stats: TableStats) {
        self.tables.insert(table.into(), stats);
    }

    /// Mutable index insert.
    pub fn insert_index(
        &mut self,
        table: impl Into<String>,
        column: impl Into<String>,
        stats: IndexStats,
    ) {
        self.indexes.insert((table.into(), column.into()), stats);
    }
}

impl StatsProvider for StaticProvider {
    fn table_stats(&self, table: &str) -> Option<TableStats> {
        self.tables.get(table).cloned()
    }

    fn index_stats(&self, table: &str, column: &str) -> Option<IndexStats> {
        self.indexes
            .get(&(table.to_string(), column.to_string()))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::index::IndexKind;

    fn sample_stats(rows: u64) -> TableStats {
        TableStats {
            row_count: rows,
            avg_row_size: 128,
            page_count: rows / 32,
            columns: vec![ColumnStats {
                name: "id".to_string(),
                distinct_count: rows,
                null_count: 0,
                min_value: None,
                max_value: None,
                has_index: true,
            }],
        }
    }

    #[test]
    fn null_provider_returns_none() {
        let p = NullProvider;
        assert!(p.table_stats("anything").is_none());
        assert!(p.index_stats("t", "c").is_none());
        assert!(!p.has_index("t", "c"));
        assert!(p.distinct_values("t", "c").is_none());
    }

    #[test]
    fn static_provider_roundtrip() {
        let p = StaticProvider::new()
            .with_table("users", sample_stats(1_000_000))
            .with_index(
                "users",
                "email",
                IndexStats {
                    entries: 1_000_000,
                    distinct_keys: 1_000_000,
                    approx_bytes: 32_000_000,
                    kind: IndexKind::Hash,
                    has_bloom: true,
                },
            );

        let t = p.table_stats("users").unwrap();
        assert_eq!(t.row_count, 1_000_000);
        assert_eq!(t.columns.len(), 1);

        assert!(p.has_index("users", "email"));
        assert!(!p.has_index("users", "display_name"));

        let idx = p.index_stats("users", "email").unwrap();
        assert_eq!(idx.distinct_keys, 1_000_000);
        // 1 / 1M == very selective
        assert!(idx.point_selectivity() < 1e-5);
    }

    #[test]
    fn column_stats_default_derives_from_table() {
        let p = StaticProvider::new().with_table("users", sample_stats(100));
        let cs = p.column_stats("users", "id").unwrap();
        assert_eq!(cs.distinct_count, 100);
        assert!(cs.has_index);
    }

    #[test]
    fn distinct_values_prefers_column_then_index() {
        // Column stats present → use them.
        let p = StaticProvider::new().with_table("t", sample_stats(500));
        assert_eq!(p.distinct_values("t", "id"), Some(500));

        // Column stats absent → fall back to index stats.
        let p = StaticProvider::new().with_index(
            "t",
            "name",
            IndexStats {
                entries: 10,
                distinct_keys: 7,
                approx_bytes: 0,
                kind: IndexKind::BTree,
                has_bloom: false,
            },
        );
        assert_eq!(p.distinct_values("t", "name"), Some(7));

        // Neither → None.
        assert_eq!(NullProvider.distinct_values("t", "name"), None);
    }
}
