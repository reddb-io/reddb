//! Aggregation Cache
//!
//! Precomputed aggregation values for fast query responses.
//! Inspired by Neo4j's statistics layer and Turso's precomputed counts.
//!
//! # Features
//!
//! - **Count Cache**: Precomputed COUNT(*) per table/filter
//! - **Sum/Avg Cache**: Numeric aggregations by column
//! - **Cardinality Cache**: Distinct value counts for query planning
//! - **Incremental Updates**: Delta updates instead of full recalculation
//!
//! # Example
//!
//! ```ignore
//! let mut agg = AggregationCache::new();
//!
//! // Register tables to track
//! agg.register_table("hosts", &["status", "os_family", "criticality"]);
//!
//! // Update on inserts
//! agg.on_insert("hosts", &row);
//!
//! // Fast aggregation queries
//! let count = agg.count("hosts", Some("status = 'active'")); // O(1)
//! let avg = agg.avg("hosts", "criticality"); // O(1)
//! let distinct = agg.distinct_count("hosts", "os_family"); // O(1)
//! ```

use std::collections::{HashMap, HashSet};
use std::time::Instant;

// ============================================================================
// Aggregation Types
// ============================================================================

/// Numeric aggregation value
#[derive(Debug, Clone, Default)]
pub struct NumericAgg {
    /// Sum of values
    pub sum: f64,
    /// Count of values
    pub count: u64,
    /// Minimum value
    pub min: Option<f64>,
    /// Maximum value
    pub max: Option<f64>,
    /// Sum of squares (for variance/stddev)
    pub sum_sq: f64,
}

impl NumericAgg {
    /// Add a value
    pub fn add(&mut self, value: f64) {
        self.sum += value;
        self.count += 1;
        self.sum_sq += value * value;

        self.min = Some(match self.min {
            Some(m) => m.min(value),
            None => value,
        });

        self.max = Some(match self.max {
            Some(m) => m.max(value),
            None => value,
        });
    }

    /// Remove a value (for updates/deletes)
    pub fn remove(&mut self, value: f64) {
        if self.count > 0 {
            self.sum -= value;
            self.count -= 1;
            self.sum_sq -= value * value;
            // Min/max become invalid - need recompute or track
        }
    }

    /// Get average
    pub fn avg(&self) -> Option<f64> {
        if self.count == 0 {
            None
        } else {
            Some(self.sum / self.count as f64)
        }
    }

    /// Get variance
    pub fn variance(&self) -> Option<f64> {
        if self.count < 2 {
            None
        } else {
            let mean = self.sum / self.count as f64;
            Some(self.sum_sq / self.count as f64 - mean * mean)
        }
    }

    /// Get standard deviation
    pub fn stddev(&self) -> Option<f64> {
        self.variance().map(|v| v.sqrt())
    }
}

/// Cardinality estimator using HyperLogLog-style counting
#[derive(Debug, Clone)]
pub struct CardinalityEstimate {
    /// Distinct values seen (exact for small sets)
    distinct_values: HashSet<u64>,
    /// Threshold for switching to approximate
    exact_threshold: usize,
    /// Approximate count if over threshold
    approximate: Option<u64>,
    /// Last update time
    updated_at: Instant,
}

impl CardinalityEstimate {
    pub fn new(exact_threshold: usize) -> Self {
        Self {
            distinct_values: HashSet::new(),
            exact_threshold,
            approximate: None,
            updated_at: Instant::now(),
        }
    }

    /// Add a value (hash of the actual value)
    pub fn add(&mut self, hash: u64) {
        if self.approximate.is_none() {
            self.distinct_values.insert(hash);
            if self.distinct_values.len() > self.exact_threshold {
                // Switch to approximate mode
                self.approximate = Some(self.distinct_values.len() as u64);
                self.distinct_values.clear();
            }
        } else {
            // Approximate mode: use probabilistic estimation
            // Simple: just increment if hash is "rare enough"
            if hash % 1000 == 0 {
                if let Some(ref mut count) = self.approximate {
                    *count += 1;
                }
            }
        }
        self.updated_at = Instant::now();
    }

    /// Get cardinality estimate
    pub fn estimate(&self) -> u64 {
        if let Some(approx) = self.approximate {
            approx
        } else {
            self.distinct_values.len() as u64
        }
    }
}

impl Default for CardinalityEstimate {
    fn default() -> Self {
        Self::new(10000)
    }
}

// ============================================================================
// Table Aggregates
// ============================================================================

/// Aggregations for a single table
#[derive(Debug)]
struct TableAggregates {
    /// Total row count
    row_count: u64,
    /// Count by filter predicate (e.g., "status=active" -> count)
    filtered_counts: HashMap<String, u64>,
    /// Numeric aggregations by column
    numeric_aggs: HashMap<String, NumericAgg>,
    /// Cardinality estimates by column
    cardinalities: HashMap<String, CardinalityEstimate>,
    /// Columns being tracked
    tracked_columns: Vec<String>,
    /// When aggregates were last refreshed
    last_refresh: Instant,
    /// Whether aggregates are stale
    stale: bool,
}

impl TableAggregates {
    fn new(tracked_columns: Vec<String>) -> Self {
        Self {
            row_count: 0,
            filtered_counts: HashMap::new(),
            numeric_aggs: HashMap::new(),
            cardinalities: tracked_columns
                .iter()
                .map(|c| (c.clone(), CardinalityEstimate::default()))
                .collect(),
            tracked_columns,
            last_refresh: Instant::now(),
            stale: false,
        }
    }
}

// ============================================================================
// Aggregation Cache
// ============================================================================

/// Cache for precomputed aggregations
pub struct AggregationCache {
    /// Aggregations per table
    tables: HashMap<String, TableAggregates>,
    /// Global row count across all tables
    global_row_count: u64,
}

impl AggregationCache {
    /// Create a new aggregation cache
    pub fn new() -> Self {
        Self {
            tables: HashMap::new(),
            global_row_count: 0,
        }
    }

    /// Register a table for aggregation tracking
    pub fn register_table(&mut self, table: &str, tracked_columns: &[&str]) {
        let columns = tracked_columns.iter().map(|s| s.to_string()).collect();
        self.tables
            .insert(table.to_string(), TableAggregates::new(columns));
    }

    /// Get total row count for a table
    pub fn count(&self, table: &str) -> Option<u64> {
        self.tables.get(table).map(|t| t.row_count)
    }

    /// Get filtered count (if cached)
    pub fn count_filtered(&self, table: &str, filter_key: &str) -> Option<u64> {
        self.tables
            .get(table)
            .and_then(|t| t.filtered_counts.get(filter_key).copied())
    }

    /// Set a filtered count (precomputed)
    pub fn set_filtered_count(&mut self, table: &str, filter_key: &str, count: u64) {
        if let Some(aggs) = self.tables.get_mut(table) {
            aggs.filtered_counts.insert(filter_key.to_string(), count);
        }
    }

    /// Get numeric aggregation for a column
    pub fn numeric_agg(&self, table: &str, column: &str) -> Option<&NumericAgg> {
        self.tables
            .get(table)
            .and_then(|t| t.numeric_aggs.get(column))
    }

    /// Get average for a column
    pub fn avg(&self, table: &str, column: &str) -> Option<f64> {
        self.numeric_agg(table, column).and_then(|a| a.avg())
    }

    /// Get sum for a column
    pub fn sum(&self, table: &str, column: &str) -> Option<f64> {
        self.numeric_agg(table, column).map(|a| a.sum)
    }

    /// Get min for a column
    pub fn min(&self, table: &str, column: &str) -> Option<f64> {
        self.numeric_agg(table, column).and_then(|a| a.min)
    }

    /// Get max for a column
    pub fn max(&self, table: &str, column: &str) -> Option<f64> {
        self.numeric_agg(table, column).and_then(|a| a.max)
    }

    /// Get distinct count estimate for a column
    pub fn distinct_count(&self, table: &str, column: &str) -> Option<u64> {
        self.tables
            .get(table)
            .and_then(|t| t.cardinalities.get(column))
            .map(|c| c.estimate())
    }

    /// Record an insert operation
    pub fn on_insert(&mut self, table: &str, values: &HashMap<String, AggValue>) {
        if let Some(aggs) = self.tables.get_mut(table) {
            aggs.row_count += 1;
            self.global_row_count += 1;

            for (col, value) in values {
                // Update numeric aggregations
                if let AggValue::Number(n) = value {
                    aggs.numeric_aggs
                        .entry(col.clone())
                        .or_insert_with(NumericAgg::default)
                        .add(*n);
                }

                // Update cardinality
                if let Some(card) = aggs.cardinalities.get_mut(col) {
                    card.add(value.hash());
                }
            }

            // Invalidate filtered counts (need recompute)
            aggs.filtered_counts.clear();
        }
    }

    /// Record a delete operation
    pub fn on_delete(&mut self, table: &str, values: &HashMap<String, AggValue>) {
        if let Some(aggs) = self.tables.get_mut(table) {
            aggs.row_count = aggs.row_count.saturating_sub(1);
            self.global_row_count = self.global_row_count.saturating_sub(1);

            for (col, value) in values {
                if let AggValue::Number(n) = value {
                    if let Some(num_agg) = aggs.numeric_aggs.get_mut(col) {
                        num_agg.remove(*n);
                    }
                }
            }

            // Mark as needing refresh for min/max
            aggs.stale = true;
            aggs.filtered_counts.clear();
        }
    }

    /// Full refresh for a table (recompute all aggregates)
    pub fn refresh<I>(&mut self, table: &str, rows: I)
    where
        I: Iterator<Item = HashMap<String, AggValue>>,
    {
        if let Some(aggs) = self.tables.get_mut(table) {
            // Reset aggregates
            aggs.row_count = 0;
            aggs.numeric_aggs.clear();
            for card in aggs.cardinalities.values_mut() {
                *card = CardinalityEstimate::default();
            }

            // Rebuild from rows
            for row in rows {
                aggs.row_count += 1;

                for (col, value) in &row {
                    if let AggValue::Number(n) = value {
                        aggs.numeric_aggs
                            .entry(col.clone())
                            .or_insert_with(NumericAgg::default)
                            .add(*n);
                    }

                    if let Some(card) = aggs.cardinalities.get_mut(col) {
                        card.add(value.hash());
                    }
                }
            }

            aggs.stale = false;
            aggs.last_refresh = Instant::now();
        }
    }

    /// Get global row count
    pub fn global_count(&self) -> u64 {
        self.global_row_count
    }

    /// Check if aggregates are stale
    pub fn is_stale(&self, table: &str) -> bool {
        self.tables.get(table).map(|t| t.stale).unwrap_or(true)
    }

    /// Get statistics summary
    pub fn stats(&self) -> AggCacheStats {
        AggCacheStats {
            tables: self.tables.len(),
            total_rows: self.global_row_count,
            tracked_columns: self.tables.values().map(|t| t.tracked_columns.len()).sum(),
        }
    }
}

impl Default for AggregationCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Value type for aggregation operations
#[derive(Debug, Clone)]
pub enum AggValue {
    Number(f64),
    String(String),
    Bool(bool),
    Null,
}

impl AggValue {
    /// Get a hash of the value for cardinality estimation
    pub fn hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        match self {
            AggValue::Number(n) => n.to_bits().hash(&mut hasher),
            AggValue::String(s) => s.hash(&mut hasher),
            AggValue::Bool(b) => b.hash(&mut hasher),
            AggValue::Null => 0u64.hash(&mut hasher),
        }
        hasher.finish()
    }
}

/// Aggregation cache statistics
#[derive(Debug, Clone)]
pub struct AggCacheStats {
    /// Number of tables tracked
    pub tables: usize,
    /// Total rows across all tables
    pub total_rows: u64,
    /// Total tracked columns
    pub tracked_columns: usize,
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_numeric_agg() {
        let mut agg = NumericAgg::default();
        agg.add(10.0);
        agg.add(20.0);
        agg.add(30.0);

        assert_eq!(agg.count, 3);
        assert_eq!(agg.sum, 60.0);
        assert_eq!(agg.avg(), Some(20.0));
        assert_eq!(agg.min, Some(10.0));
        assert_eq!(agg.max, Some(30.0));
    }

    #[test]
    fn test_aggregation_cache() {
        let mut cache = AggregationCache::new();
        cache.register_table("hosts", &["criticality", "status"]);

        // Insert some rows
        let mut row1 = HashMap::new();
        row1.insert("criticality".to_string(), AggValue::Number(5.0));
        row1.insert("status".to_string(), AggValue::String("active".to_string()));
        cache.on_insert("hosts", &row1);

        let mut row2 = HashMap::new();
        row2.insert("criticality".to_string(), AggValue::Number(8.0));
        row2.insert("status".to_string(), AggValue::String("active".to_string()));
        cache.on_insert("hosts", &row2);

        let mut row3 = HashMap::new();
        row3.insert("criticality".to_string(), AggValue::Number(2.0));
        row3.insert(
            "status".to_string(),
            AggValue::String("inactive".to_string()),
        );
        cache.on_insert("hosts", &row3);

        assert_eq!(cache.count("hosts"), Some(3));
        assert_eq!(cache.avg("hosts", "criticality"), Some(5.0));
        assert_eq!(cache.sum("hosts", "criticality"), Some(15.0));
        assert_eq!(cache.min("hosts", "criticality"), Some(2.0));
        assert_eq!(cache.max("hosts", "criticality"), Some(8.0));
    }

    #[test]
    fn test_cardinality() {
        let mut card = CardinalityEstimate::new(100);

        // Add distinct values
        for i in 0..50 {
            card.add(i);
        }

        assert_eq!(card.estimate(), 50);

        // Add duplicates
        for i in 0..50 {
            card.add(i);
        }

        // Should still be ~50 (duplicates don't count)
        assert_eq!(card.estimate(), 50);
    }
}
