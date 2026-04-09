//! Statistics Collection
//!
//! Collects and maintains statistics for query optimization.

use std::collections::HashMap;
use std::sync::RwLock;

/// Column statistics
#[derive(Debug, Clone)]
pub struct ColumnStats {
    /// Column name
    pub name: String,
    /// Number of distinct values (NDV)
    pub ndv: u64,
    /// Fraction of NULL values
    pub null_fraction: f64,
    /// Minimum value (for numeric columns)
    pub min_value: Option<f64>,
    /// Maximum value (for numeric columns)
    pub max_value: Option<f64>,
}

impl ColumnStats {
    /// Create new column stats
    pub fn new(name: String) -> Self {
        Self {
            name,
            ndv: 0,
            null_fraction: 0.0,
            min_value: None,
            max_value: None,
        }
    }

    /// Set NDV
    pub fn with_ndv(mut self, ndv: u64) -> Self {
        self.ndv = ndv;
        self
    }

    /// Set null fraction
    pub fn with_null_fraction(mut self, fraction: f64) -> Self {
        self.null_fraction = fraction.clamp(0.0, 1.0);
        self
    }

    /// Set min/max values
    pub fn with_range(mut self, min: f64, max: f64) -> Self {
        self.min_value = Some(min);
        self.max_value = Some(max);
        self
    }

    /// Estimate selectivity for equality predicate
    pub fn equality_selectivity(&self) -> f64 {
        if self.ndv > 0 {
            1.0 / self.ndv as f64
        } else {
            0.01 // Default
        }
    }

    /// Estimate selectivity for range predicate
    pub fn range_selectivity(&self, lower: Option<f64>, upper: Option<f64>) -> f64 {
        match (self.min_value, self.max_value) {
            (Some(min), Some(max)) if max > min => {
                let range = max - min;
                let low = lower.unwrap_or(min);
                let high = upper.unwrap_or(max);
                ((high - low) / range).clamp(0.0, 1.0)
            }
            _ => 0.25, // Default
        }
    }
}

/// Table statistics
#[derive(Debug, Clone)]
pub struct TableStats {
    /// Table name
    pub name: String,
    /// Row count
    pub row_count: u64,
    /// Column statistics
    columns: HashMap<String, ColumnStats>,
    /// Average row size in bytes
    pub avg_row_size: Option<usize>,
    /// Last updated timestamp
    pub last_updated: Option<u64>,
}

impl TableStats {
    /// Create new table stats
    pub fn new(name: String, row_count: u64) -> Self {
        Self {
            name,
            row_count,
            columns: HashMap::new(),
            avg_row_size: None,
            last_updated: None,
        }
    }

    /// Add column statistics
    pub fn add_column(&mut self, stats: ColumnStats) {
        self.columns.insert(stats.name.clone(), stats);
    }

    /// Get column statistics
    pub fn get_column(&self, name: &str) -> Option<&ColumnStats> {
        self.columns.get(name)
    }

    /// Get all column names
    pub fn column_names(&self) -> Vec<&str> {
        self.columns.keys().map(|s| s.as_str()).collect()
    }

    /// Set average row size
    pub fn with_avg_row_size(mut self, size: usize) -> Self {
        self.avg_row_size = Some(size);
        self
    }

    /// Estimate table size in bytes
    pub fn estimated_size(&self) -> Option<u64> {
        self.avg_row_size.map(|size| self.row_count * size as u64)
    }
}

/// Statistics collector for building table stats
pub struct StatsCollector {
    /// Column collectors
    columns: HashMap<String, ColumnCollector>,
    /// Total rows seen
    row_count: u64,
    /// Total size seen
    total_size: usize,
}

impl StatsCollector {
    /// Create new collector
    pub fn new() -> Self {
        Self {
            columns: HashMap::new(),
            row_count: 0,
            total_size: 0,
        }
    }

    /// Start collecting for a column
    pub fn add_column(&mut self, name: &str) {
        self.columns
            .insert(name.to_string(), ColumnCollector::new(name.to_string()));
    }

    /// Observe a row
    pub fn observe_row(&mut self, row_size: usize) {
        self.row_count += 1;
        self.total_size += row_size;
    }

    /// Observe a value
    pub fn observe_value(&mut self, column: &str, value: Option<&ObservedValue>) {
        if let Some(collector) = self.columns.get_mut(column) {
            collector.observe(value);
        }
    }

    /// Build final statistics
    pub fn build(self, table_name: String) -> TableStats {
        let mut stats = TableStats::new(table_name, self.row_count);

        if self.row_count > 0 {
            stats.avg_row_size = Some(self.total_size / self.row_count as usize);
        }

        for (_, collector) in self.columns {
            stats.add_column(collector.build(self.row_count));
        }

        stats
    }
}

impl Default for StatsCollector {
    fn default() -> Self {
        Self::new()
    }
}

/// Value type for observation
#[derive(Debug, Clone)]
pub enum ObservedValue {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Bytes(Vec<u8>),
}

impl ObservedValue {
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            ObservedValue::Int(i) => Some(*i as f64),
            ObservedValue::Float(f) => Some(*f),
            _ => None,
        }
    }
}

/// Per-column statistics collector
struct ColumnCollector {
    name: String,
    /// Distinct values (using HyperLogLog would be better for large datasets)
    distinct: std::collections::HashSet<u64>,
    /// NULL count
    null_count: u64,
    /// Min value
    min_value: Option<f64>,
    /// Max value
    max_value: Option<f64>,
}

impl ColumnCollector {
    fn new(name: String) -> Self {
        Self {
            name,
            distinct: std::collections::HashSet::new(),
            null_count: 0,
            min_value: None,
            max_value: None,
        }
    }

    fn observe(&mut self, value: Option<&ObservedValue>) {
        match value {
            None => {
                self.null_count += 1;
            }
            Some(v) => {
                // Hash for distinct counting
                let hash = Self::hash_value(v);
                self.distinct.insert(hash);

                // Track min/max for numeric values
                if let Some(f) = v.as_f64() {
                    self.min_value = Some(match self.min_value {
                        Some(min) => min.min(f),
                        None => f,
                    });
                    self.max_value = Some(match self.max_value {
                        Some(max) => max.max(f),
                        None => f,
                    });
                }
            }
        }
    }

    fn hash_value(value: &ObservedValue) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();

        match value {
            ObservedValue::Int(i) => i.hash(&mut hasher),
            ObservedValue::Float(f) => f.to_bits().hash(&mut hasher),
            ObservedValue::String(s) => s.hash(&mut hasher),
            ObservedValue::Bool(b) => b.hash(&mut hasher),
            ObservedValue::Bytes(b) => b.hash(&mut hasher),
        }

        hasher.finish()
    }

    fn build(self, row_count: u64) -> ColumnStats {
        let null_fraction = if row_count > 0 {
            self.null_count as f64 / row_count as f64
        } else {
            0.0
        };

        ColumnStats {
            name: self.name,
            ndv: self.distinct.len() as u64,
            null_fraction,
            min_value: self.min_value,
            max_value: self.max_value,
        }
    }
}

/// Global statistics registry
pub struct StatsRegistry {
    /// Table statistics
    tables: RwLock<HashMap<String, TableStats>>,
}

impl StatsRegistry {
    /// Create new registry
    pub fn new() -> Self {
        Self {
            tables: RwLock::new(HashMap::new()),
        }
    }

    /// Register table statistics
    pub fn register(&self, stats: TableStats) {
        let mut tables = self.tables.write().unwrap();
        tables.insert(stats.name.clone(), stats);
    }

    /// Get table statistics
    pub fn get(&self, table_name: &str) -> Option<TableStats> {
        let tables = self.tables.read().unwrap();
        tables.get(table_name).cloned()
    }

    /// Remove table statistics
    pub fn remove(&self, table_name: &str) -> Option<TableStats> {
        let mut tables = self.tables.write().unwrap();
        tables.remove(table_name)
    }

    /// List all tables with statistics
    pub fn list(&self) -> Vec<String> {
        let tables = self.tables.read().unwrap();
        tables.keys().cloned().collect()
    }

    /// Clear all statistics
    pub fn clear(&self) {
        let mut tables = self.tables.write().unwrap();
        tables.clear();
    }
}

impl Default for StatsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_column_stats() {
        let stats = ColumnStats::new("status".to_string())
            .with_ndv(5)
            .with_null_fraction(0.1);

        assert_eq!(stats.ndv, 5);
        assert!((stats.null_fraction - 0.1).abs() < 0.001);
        assert!((stats.equality_selectivity() - 0.2).abs() < 0.001);
    }

    #[test]
    fn test_range_selectivity() {
        let stats = ColumnStats::new("age".to_string())
            .with_ndv(100)
            .with_range(0.0, 100.0);

        // Half the range
        let sel = stats.range_selectivity(Some(0.0), Some(50.0));
        assert!((sel - 0.5).abs() < 0.001);

        // Quarter of the range
        let sel = stats.range_selectivity(Some(25.0), Some(50.0));
        assert!((sel - 0.25).abs() < 0.001);
    }

    #[test]
    fn test_table_stats() {
        let mut stats = TableStats::new("users".to_string(), 10000);

        stats.add_column(
            ColumnStats::new("id".to_string())
                .with_ndv(10000)
                .with_null_fraction(0.0),
        );

        stats.add_column(
            ColumnStats::new("status".to_string())
                .with_ndv(5)
                .with_null_fraction(0.02),
        );

        assert_eq!(stats.row_count, 10000);
        assert!(stats.get_column("id").is_some());
        assert!(stats.get_column("status").is_some());
        assert!(stats.get_column("unknown").is_none());
    }

    #[test]
    fn test_stats_collector() {
        let mut collector = StatsCollector::new();
        collector.add_column("value");

        // Observe some values
        for i in 0..100 {
            collector.observe_row(100);
            if i % 10 == 0 {
                collector.observe_value("value", None); // NULL
            } else {
                collector.observe_value("value", Some(&ObservedValue::Int(i % 5)));
            }
        }

        let stats = collector.build("test".to_string());

        assert_eq!(stats.row_count, 100);
        assert_eq!(stats.avg_row_size, Some(100));

        let col = stats.get_column("value").unwrap();
        assert_eq!(col.ndv, 5); // 0, 1, 2, 3, 4
        assert!((col.null_fraction - 0.1).abs() < 0.01);
    }

    #[test]
    fn test_stats_registry() {
        let registry = StatsRegistry::new();

        let stats = TableStats::new("users".to_string(), 1000);
        registry.register(stats);

        assert!(registry.get("users").is_some());
        assert!(registry.get("orders").is_none());

        assert_eq!(registry.list().len(), 1);

        registry.remove("users");
        assert!(registry.get("users").is_none());
    }

    #[test]
    fn test_observed_value_hashing() {
        let mut collector = StatsCollector::new();
        collector.add_column("mixed");

        // Different types should hash differently
        collector.observe_value("mixed", Some(&ObservedValue::Int(42)));
        collector.observe_value("mixed", Some(&ObservedValue::String("42".to_string())));
        collector.observe_value("mixed", Some(&ObservedValue::Float(42.0)));

        let stats = collector.build("test".to_string());
        let col = stats.get_column("mixed").unwrap();

        // All three should be distinct
        assert_eq!(col.ndv, 3);
    }
}
