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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::cost::{ColumnStats, TableStats};
use super::histogram::{Histogram, MostCommonValues};
use super::stats_catalog::load_persisted_stats;
use crate::storage::index::{IndexRegistry, IndexScope, IndexStats};

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

    /// Optional equi-depth histogram for the column. Defaults to
    /// `None`, in which case the planner falls back to its uniform
    /// 0.3 range heuristic.
    ///
    /// Implementations should sample once and cache — this is called
    /// during plan construction and must not block on I/O.
    fn column_histogram(&self, _table: &str, _column: &str) -> Option<Histogram> {
        None
    }

    /// Optional most-common-values list for the column. Defaults to
    /// `None`, in which case the planner falls back to its uniform
    /// 0.01 equality heuristic. Use for skewed columns where one or
    /// two values dominate the table.
    fn column_mcv(&self, _table: &str, _column: &str) -> Option<MostCommonValues> {
        None
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
    /// Optional histograms keyed by `(table, column)`.
    histograms: HashMap<(String, String), Histogram>,
    /// Optional MCV lists keyed by `(table, column)`.
    mcvs: HashMap<(String, String), MostCommonValues>,
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

    /// Register or replace an equi-depth histogram on `(table, column)`.
    pub fn with_histogram(
        mut self,
        table: impl Into<String>,
        column: impl Into<String>,
        histogram: Histogram,
    ) -> Self {
        self.histograms
            .insert((table.into(), column.into()), histogram);
        self
    }

    /// Register or replace an MCV list on `(table, column)`.
    pub fn with_mcv(
        mut self,
        table: impl Into<String>,
        column: impl Into<String>,
        mcv: MostCommonValues,
    ) -> Self {
        self.mcvs.insert((table.into(), column.into()), mcv);
        self
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

    fn column_histogram(&self, table: &str, column: &str) -> Option<Histogram> {
        self.histograms
            .get(&(table.to_string(), column.to_string()))
            .cloned()
    }

    fn column_mcv(&self, table: &str, column: &str) -> Option<MostCommonValues> {
        self.mcvs
            .get(&(table.to_string(), column.to_string()))
            .cloned()
    }
}

/// [`StatsProvider`] backed by a [`crate::api::CatalogSnapshot`].
///
/// Provides real row counts to the cost estimator without requiring a
/// live `RedDB` reference in the planner's inner loops. Build once per
/// query from `db.catalog_snapshot()` and wrap in `Arc`.
pub struct CatalogStatsProvider {
    tables: HashMap<String, TableStats>,
    histograms: HashMap<(String, String), Histogram>,
    mcvs: HashMap<(String, String), MostCommonValues>,
}

impl CatalogStatsProvider {
    /// Build a provider from a catalog snapshot.  Only `row_count` and a
    /// minimal `page_count` estimate are populated — index stats still come
    /// from [`RegistryProvider`] or remain `None`.
    pub fn from_catalog(snapshot: &crate::api::CatalogSnapshot) -> Self {
        const AVG_ROW_SIZE: u32 = 128;
        const ROWS_PER_PAGE: u64 = 100;
        let tables = snapshot
            .stats_by_collection
            .iter()
            .map(|(name, cstats)| {
                let row_count = cstats.entities as u64;
                let stats = TableStats {
                    row_count,
                    avg_row_size: AVG_ROW_SIZE,
                    page_count: (row_count / ROWS_PER_PAGE).max(1),
                    columns: vec![],
                };
                (name.clone(), stats)
            })
            .collect();
        Self {
            tables,
            histograms: HashMap::new(),
            mcvs: HashMap::new(),
        }
    }

    /// Build a provider from the live database, overlaying persisted
    /// `red_stats` column statistics onto the catalog's fresh row counts.
    pub fn from_db(db: &crate::storage::RedDB) -> Self {
        let mut provider = Self::from_catalog(&db.catalog_snapshot());
        let persisted = load_persisted_stats(&db.store(), &db.catalog_snapshot());
        for (table, stats) in persisted.tables {
            provider
                .tables
                .entry(table)
                .and_modify(|current| {
                    current.avg_row_size = stats.avg_row_size;
                    current.page_count = stats.page_count.max(current.page_count);
                    current.columns = merge_persisted_columns(&current.columns, &stats.columns);
                })
                .or_insert(stats);
        }
        provider.histograms = persisted.histograms;
        provider.mcvs = persisted.mcvs;
        provider
    }
}

fn merge_persisted_columns(current: &[ColumnStats], persisted: &[ColumnStats]) -> Vec<ColumnStats> {
    let current_by_name: HashMap<&str, &ColumnStats> = current
        .iter()
        .map(|column| (column.name.as_str(), column))
        .collect();
    let mut merged = Vec::with_capacity(current.len().max(persisted.len()));
    let mut seen = HashSet::new();

    for column in persisted {
        let mut merged_column = column.clone();
        if let Some(current_column) = current_by_name.get(merged_column.name.as_str()) {
            merged_column.has_index = current_column.has_index;
        }
        seen.insert(merged_column.name.clone());
        merged.push(merged_column);
    }

    for column in current {
        if seen.insert(column.name.clone()) {
            merged.push(column.clone());
        }
    }

    merged
}

impl StatsProvider for CatalogStatsProvider {
    fn table_stats(&self, table: &str) -> Option<TableStats> {
        self.tables.get(table).cloned()
    }

    fn index_stats(&self, _table: &str, _column: &str) -> Option<IndexStats> {
        None
    }

    fn column_histogram(&self, table: &str, column: &str) -> Option<Histogram> {
        self.histograms
            .get(&(table.to_string(), column.to_string()))
            .cloned()
    }

    fn column_mcv(&self, table: &str, column: &str) -> Option<MostCommonValues> {
        self.mcvs
            .get(&(table.to_string(), column.to_string()))
            .cloned()
    }
}

/// [`StatsProvider`] backed by an [`IndexRegistry`].
///
/// Closes the loop between the index trait layer and the planner stats
/// surface: storage components publish their indexes into an
/// `IndexRegistry`, and this adapter surfaces those statistics to the cost
/// estimator through the trait it already consumes.
///
/// Table-level statistics (row counts, page counts) still need an external
/// source — the registry only knows about *indexes*, not base-table
/// cardinality. Callers can chain a [`StaticProvider`] via
/// [`RegistryProvider::with_table_fallback`] when they want both.
pub struct RegistryProvider {
    registry: Arc<IndexRegistry>,
    table_fallback: Option<Arc<dyn StatsProvider>>,
}

impl RegistryProvider {
    /// Wrap an existing registry. Without a fallback, `table_stats` always
    /// returns `None` — only index-level stats are served.
    pub fn new(registry: Arc<IndexRegistry>) -> Self {
        Self {
            registry,
            table_fallback: None,
        }
    }

    /// Attach a secondary provider consulted for table-level stats the
    /// registry cannot answer.
    pub fn with_table_fallback(mut self, fallback: Arc<dyn StatsProvider>) -> Self {
        self.table_fallback = Some(fallback);
        self
    }
}

impl StatsProvider for RegistryProvider {
    fn table_stats(&self, table: &str) -> Option<TableStats> {
        self.table_fallback
            .as_ref()
            .and_then(|f| f.table_stats(table))
    }

    fn index_stats(&self, table: &str, column: &str) -> Option<IndexStats> {
        self.registry
            .get(&IndexScope::table(table, column))
            .map(|idx| idx.stats())
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
                    index_correlation: 0.0,
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
    fn null_provider_returns_no_histogram_or_mcv() {
        let p = NullProvider;
        assert!(p.column_histogram("users", "email").is_none());
        assert!(p.column_mcv("users", "email").is_none());
    }

    #[test]
    fn static_provider_serves_histograms() {
        use super::super::histogram::{ColumnValue, Histogram};
        let h = Histogram::equi_depth_from_sample((0..100i64).map(ColumnValue::Int).collect(), 10);
        let p = StaticProvider::new().with_histogram("orders", "amount", h);
        let got = p.column_histogram("orders", "amount").unwrap();
        assert_eq!(got.bucket_count(), 10);
        assert_eq!(got.total_count, 100);
        // Other columns unaffected.
        assert!(p.column_histogram("orders", "missing").is_none());
    }

    #[test]
    fn static_provider_serves_mcv_lists() {
        use super::super::histogram::{ColumnValue, MostCommonValues};
        let mcv = MostCommonValues::new(vec![
            (ColumnValue::text("admin".to_string()), 0.4),
            (ColumnValue::text("user".to_string()), 0.5),
        ]);
        let p = StaticProvider::new().with_mcv("users", "role", mcv);
        let got = p.column_mcv("users", "role").unwrap();
        assert_eq!(got.len(), 2);
        // Sorted descending by frequency on construction.
        assert_eq!(got.values[0].1, 0.5);
        assert!(p.column_mcv("users", "missing").is_none());
    }

    #[test]
    fn registry_provider_default_no_histogram() {
        // RegistryProvider doesn't have a histogram path yet — falls
        // through to None like NullProvider.
        use crate::storage::index::IndexRegistry;
        use std::sync::Arc;
        let p = RegistryProvider::new(Arc::new(IndexRegistry::new()));
        assert!(p.column_histogram("any", "any").is_none());
        assert!(p.column_mcv("any", "any").is_none());
    }

    #[test]
    fn registry_provider_serves_index_stats() {
        use crate::storage::index::{IndexBase, IndexKind, IndexRegistry, IndexScope};
        use std::sync::Arc;

        struct StubIndex(IndexStats);
        impl IndexBase for StubIndex {
            fn name(&self) -> &str {
                "stub"
            }
            fn kind(&self) -> IndexKind {
                self.0.kind
            }
            fn stats(&self) -> IndexStats {
                self.0.clone()
            }
        }

        let registry = Arc::new(IndexRegistry::new());
        registry.register(
            IndexScope::table("users", "email"),
            Arc::new(StubIndex(IndexStats {
                entries: 500_000,
                distinct_keys: 500_000,
                approx_bytes: 0,
                kind: IndexKind::Hash,
                has_bloom: true,
                index_correlation: 0.0,
            })),
        );

        let provider = RegistryProvider::new(Arc::clone(&registry));
        let stats = provider.index_stats("users", "email").unwrap();
        assert_eq!(stats.distinct_keys, 500_000);
        assert_eq!(stats.kind, IndexKind::Hash);
        // No table fallback registered.
        assert!(provider.table_stats("users").is_none());
    }

    #[test]
    fn registry_provider_chains_fallback_for_table_stats() {
        use crate::storage::index::IndexRegistry;
        use std::sync::Arc;

        let fallback: Arc<dyn StatsProvider> = Arc::new(StaticProvider::new().with_table(
            "orders",
            TableStats {
                row_count: 25_000,
                avg_row_size: 512,
                page_count: 50,
                columns: vec![],
            },
        ));

        let registry = Arc::new(IndexRegistry::new());
        let provider = RegistryProvider::new(registry).with_table_fallback(fallback);
        let t = provider.table_stats("orders").unwrap();
        assert_eq!(t.row_count, 25_000);
        // Registry has no index for this table — None is correct.
        assert!(provider.index_stats("orders", "id").is_none());
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
                index_correlation: 0.0,
            },
        );
        assert_eq!(p.distinct_values("t", "name"), Some(7));

        // Neither → None.
        assert_eq!(NullProvider.distinct_values("t", "name"), None);
    }
}
