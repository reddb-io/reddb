//! Filter Ranking
//!
//! Optimizes filter execution order based on selectivity and cost.
//!
//! The key insight is that we want to execute filters that:
//! 1. Eliminate the most rows (low selectivity)
//! 2. Are cheap to evaluate (low cost)
//!
//! We rank by `selectivity / cost` - lower is better.

use super::stats::TableStats;
use std::cmp::Ordering;
use std::fmt::Debug;

/// Filter expression (simplified for ranking)
#[derive(Debug, Clone)]
pub enum FilterExpr {
    /// Equality: column = value
    Eq { column: String, value: FilterValue },
    /// Not equal: column != value
    Ne { column: String, value: FilterValue },
    /// Less than: column < value
    Lt { column: String, value: FilterValue },
    /// Less than or equal: column <= value
    Le { column: String, value: FilterValue },
    /// Greater than: column > value
    Gt { column: String, value: FilterValue },
    /// Greater than or equal: column >= value
    Ge { column: String, value: FilterValue },
    /// Range: lower <= column <= upper
    Between {
        column: String,
        lower: FilterValue,
        upper: FilterValue,
    },
    /// IN list: column IN (v1, v2, ...)
    In {
        column: String,
        values: Vec<FilterValue>,
    },
    /// LIKE pattern: column LIKE 'pattern%'
    Like { column: String, pattern: String },
    /// IS NULL: column IS NULL
    IsNull { column: String },
    /// IS NOT NULL: column IS NOT NULL
    IsNotNull { column: String },
    /// AND: expr1 AND expr2
    And(Box<FilterExpr>, Box<FilterExpr>),
    /// OR: expr1 OR expr2
    Or(Box<FilterExpr>, Box<FilterExpr>),
    /// NOT: NOT expr
    Not(Box<FilterExpr>),
    /// Function call (expensive)
    Function { name: String, args: Vec<FilterExpr> },
    /// Subquery (very expensive)
    Subquery { id: u64 },
    /// True literal (passes all)
    True,
    /// False literal (passes none)
    False,
}

/// Filter value for comparison
#[derive(Debug, Clone, PartialEq)]
pub enum FilterValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl FilterValue {
    /// Get numeric value for range estimation
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            FilterValue::Int(i) => Some(*i as f64),
            FilterValue::Float(f) => Some(*f),
            _ => None,
        }
    }
}

/// Cost estimates for different operations
#[derive(Debug, Clone)]
pub struct CostModel {
    /// Base cost of any comparison
    pub base_compare: f64,
    /// Cost per byte for string comparison
    pub string_per_byte: f64,
    /// Cost of function call
    pub function_call: f64,
    /// Cost of subquery execution
    pub subquery: f64,
    /// Cost of regex/LIKE pattern matching
    pub pattern_match: f64,
    /// Cost of NULL check
    pub null_check: f64,
    /// Cost of IN list (per element)
    pub in_per_element: f64,
}

impl Default for CostModel {
    fn default() -> Self {
        Self {
            base_compare: 1.0,
            string_per_byte: 0.01,
            function_call: 10.0,
            subquery: 1000.0,
            pattern_match: 5.0,
            null_check: 0.5,
            in_per_element: 1.0,
        }
    }
}

/// Selectivity estimates
#[derive(Debug, Clone)]
pub struct SelectivityModel {
    /// Default selectivity for equality
    pub default_equality: f64,
    /// Default selectivity for inequality
    pub default_inequality: f64,
    /// Default selectivity for range
    pub default_range: f64,
    /// Default selectivity for LIKE
    pub default_like: f64,
    /// Default selectivity for IS NULL
    pub default_is_null: f64,
}

impl Default for SelectivityModel {
    fn default() -> Self {
        Self {
            default_equality: 0.01,   // 1% of rows match
            default_inequality: 0.99, // 99% of rows match
            default_range: 0.25,      // 25% of rows match
            default_like: 0.10,       // 10% of rows match
            default_is_null: 0.05,    // 5% of rows are NULL
        }
    }
}

/// Ranking configuration
#[derive(Debug, Clone)]
pub struct RankingConfig {
    /// Cost model
    pub cost_model: CostModel,
    /// Selectivity model
    pub selectivity_model: SelectivityModel,
    /// Use column statistics if available
    pub use_statistics: bool,
    /// Minimum selectivity (prevent division issues)
    pub min_selectivity: f64,
    /// Maximum cost (cap outliers)
    pub max_cost: f64,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            cost_model: CostModel::default(),
            selectivity_model: SelectivityModel::default(),
            use_statistics: true,
            min_selectivity: 0.0001,
            max_cost: 10000.0,
        }
    }
}

/// A filter with its ranking score
#[derive(Debug, Clone)]
pub struct RankedFilter {
    /// The filter expression
    pub filter: FilterExpr,
    /// Estimated selectivity (0.0 - 1.0)
    pub selectivity: f64,
    /// Estimated evaluation cost
    pub cost: f64,
    /// Ranking score (lower is better)
    pub score: f64,
    /// Original position in input
    pub original_index: usize,
}

impl RankedFilter {
    /// Create new ranked filter
    pub fn new(filter: FilterExpr, selectivity: f64, cost: f64, original_index: usize) -> Self {
        let score = if cost > 0.0 {
            selectivity / cost
        } else {
            selectivity
        };

        Self {
            filter,
            selectivity,
            cost,
            score,
            original_index,
        }
    }
}

impl PartialEq for RankedFilter {
    fn eq(&self, other: &Self) -> bool {
        self.score == other.score
    }
}

impl Eq for RankedFilter {}

impl PartialOrd for RankedFilter {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedFilter {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(Ordering::Equal)
    }
}

/// Filter ranker
pub struct FilterRanker {
    /// Configuration
    config: RankingConfig,
    /// Table statistics (optional)
    table_stats: Option<TableStats>,
}

impl FilterRanker {
    /// Create new filter ranker
    pub fn new(config: RankingConfig) -> Self {
        Self {
            config,
            table_stats: None,
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(RankingConfig::default())
    }

    /// Set table statistics
    pub fn with_stats(mut self, stats: TableStats) -> Self {
        self.table_stats = Some(stats);
        self
    }

    /// Estimate selectivity of a filter
    pub fn estimate_selectivity(&self, filter: &FilterExpr) -> f64 {
        let sel = match filter {
            FilterExpr::Eq { column, value } => self.estimate_equality_selectivity(column, value),
            FilterExpr::Ne { column, value } => {
                1.0 - self.estimate_equality_selectivity(column, value)
            }
            FilterExpr::Lt { column, value } | FilterExpr::Le { column, value } => {
                self.estimate_range_selectivity(column, None, Some(value))
            }
            FilterExpr::Gt { column, value } | FilterExpr::Ge { column, value } => {
                self.estimate_range_selectivity(column, Some(value), None)
            }
            FilterExpr::Between {
                column,
                lower,
                upper,
            } => self.estimate_range_selectivity(column, Some(lower), Some(upper)),
            FilterExpr::In { column, values } => self.estimate_in_selectivity(column, values),
            FilterExpr::Like { column, pattern } => self.estimate_like_selectivity(column, pattern),
            FilterExpr::IsNull { column } => self.estimate_null_selectivity(column, true),
            FilterExpr::IsNotNull { column } => self.estimate_null_selectivity(column, false),
            FilterExpr::And(left, right) => {
                // Assume independence
                self.estimate_selectivity(left) * self.estimate_selectivity(right)
            }
            FilterExpr::Or(left, right) => {
                // P(A or B) = P(A) + P(B) - P(A and B)
                let sel_a = self.estimate_selectivity(left);
                let sel_b = self.estimate_selectivity(right);
                sel_a + sel_b - (sel_a * sel_b)
            }
            FilterExpr::Not(inner) => 1.0 - self.estimate_selectivity(inner),
            FilterExpr::Function { .. } => self.config.selectivity_model.default_equality,
            FilterExpr::Subquery { .. } => self.config.selectivity_model.default_equality,
            FilterExpr::True => 1.0,
            FilterExpr::False => 0.0,
        };

        sel.max(self.config.min_selectivity)
    }

    /// Estimate cost of evaluating a filter
    pub fn estimate_cost(&self, filter: &FilterExpr) -> f64 {
        let cost = match filter {
            FilterExpr::Eq { value, .. } | FilterExpr::Ne { value, .. } => {
                self.value_compare_cost(value)
            }
            FilterExpr::Lt { value, .. }
            | FilterExpr::Le { value, .. }
            | FilterExpr::Gt { value, .. }
            | FilterExpr::Ge { value, .. } => self.value_compare_cost(value),
            FilterExpr::Between { lower, upper, .. } => {
                self.value_compare_cost(lower) + self.value_compare_cost(upper)
            }
            FilterExpr::In { values, .. } => {
                self.config.cost_model.in_per_element * values.len() as f64
            }
            FilterExpr::Like { pattern, .. } => {
                self.config.cost_model.pattern_match
                    + self.config.cost_model.string_per_byte * pattern.len() as f64
            }
            FilterExpr::IsNull { .. } | FilterExpr::IsNotNull { .. } => {
                self.config.cost_model.null_check
            }
            FilterExpr::And(left, right) | FilterExpr::Or(left, right) => {
                self.estimate_cost(left) + self.estimate_cost(right)
            }
            FilterExpr::Not(inner) => self.estimate_cost(inner),
            FilterExpr::Function { args, .. } => {
                let arg_cost: f64 = args.iter().map(|a| self.estimate_cost(a)).sum();
                self.config.cost_model.function_call + arg_cost
            }
            FilterExpr::Subquery { .. } => self.config.cost_model.subquery,
            FilterExpr::True | FilterExpr::False => 0.0,
        };

        cost.min(self.config.max_cost)
    }

    /// Rank a list of filters
    pub fn rank(&self, filters: Vec<FilterExpr>) -> Vec<RankedFilter> {
        let mut ranked: Vec<RankedFilter> = filters
            .into_iter()
            .enumerate()
            .map(|(i, f)| {
                let selectivity = self.estimate_selectivity(&f);
                let cost = self.estimate_cost(&f);
                RankedFilter::new(f, selectivity, cost, i)
            })
            .collect();

        // Sort by score (lower is better)
        ranked.sort();

        ranked
    }

    /// Reorder filters for optimal execution
    pub fn reorder(&self, filters: Vec<FilterExpr>) -> Vec<FilterExpr> {
        self.rank(filters).into_iter().map(|r| r.filter).collect()
    }

    /// Estimate equality selectivity
    fn estimate_equality_selectivity(&self, column: &str, _value: &FilterValue) -> f64 {
        if self.config.use_statistics {
            if let Some(ref stats) = self.table_stats {
                if let Some(col_stats) = stats.get_column(column) {
                    // Use NDV (number of distinct values) for selectivity
                    if col_stats.ndv > 0 {
                        return 1.0 / col_stats.ndv as f64;
                    }
                }
            }
        }

        self.config.selectivity_model.default_equality
    }

    /// Estimate range selectivity
    fn estimate_range_selectivity(
        &self,
        column: &str,
        lower: Option<&FilterValue>,
        upper: Option<&FilterValue>,
    ) -> f64 {
        if self.config.use_statistics {
            if let Some(ref stats) = self.table_stats {
                if let Some(col_stats) = stats.get_column(column) {
                    // Use min/max for range estimation
                    if let (Some(min), Some(max)) = (col_stats.min_value, col_stats.max_value) {
                        if max > min {
                            let range = max - min;
                            let lower_bound = lower.and_then(|v| v.as_f64()).unwrap_or(min);
                            let upper_bound = upper.and_then(|v| v.as_f64()).unwrap_or(max);

                            let fraction = (upper_bound - lower_bound) / range;
                            return fraction.clamp(0.0, 1.0);
                        }
                    }
                }
            }
        }

        self.config.selectivity_model.default_range
    }

    /// Estimate IN selectivity
    fn estimate_in_selectivity(&self, column: &str, values: &[FilterValue]) -> f64 {
        let single_sel = self.estimate_equality_selectivity(column, &values[0]);
        // Assume values are distinct, so selectivity is additive
        (single_sel * values.len() as f64).min(1.0)
    }

    /// Estimate LIKE selectivity
    fn estimate_like_selectivity(&self, _column: &str, pattern: &str) -> f64 {
        // Prefix matching is more selective
        if !pattern.starts_with('%') && pattern.ends_with('%') {
            // Prefix match: fairly selective
            self.config.selectivity_model.default_like * 0.5
        } else if pattern.starts_with('%') && pattern.ends_with('%') {
            // Contains: less selective
            self.config.selectivity_model.default_like * 2.0
        } else {
            self.config.selectivity_model.default_like
        }
    }

    /// Estimate NULL selectivity
    fn estimate_null_selectivity(&self, column: &str, is_null: bool) -> f64 {
        if self.config.use_statistics {
            if let Some(ref stats) = self.table_stats {
                if let Some(col_stats) = stats.get_column(column) {
                    let null_fraction = col_stats.null_fraction;
                    return if is_null {
                        null_fraction
                    } else {
                        1.0 - null_fraction
                    };
                }
            }
        }

        if is_null {
            self.config.selectivity_model.default_is_null
        } else {
            1.0 - self.config.selectivity_model.default_is_null
        }
    }

    /// Calculate value comparison cost
    fn value_compare_cost(&self, value: &FilterValue) -> f64 {
        match value {
            FilterValue::String(s) => {
                self.config.cost_model.base_compare
                    + self.config.cost_model.string_per_byte * s.len() as f64
            }
            FilterValue::Bytes(b) => {
                self.config.cost_model.base_compare
                    + self.config.cost_model.string_per_byte * b.len() as f64
            }
            _ => self.config.cost_model.base_compare,
        }
    }

    /// Get configuration
    pub fn config(&self) -> &RankingConfig {
        &self.config
    }
}

impl Default for FilterRanker {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::optimizer::stats::ColumnStats;

    #[test]
    fn test_simple_ranking() {
        let ranker = FilterRanker::with_defaults();

        let filters = vec![
            FilterExpr::Eq {
                column: "id".to_string(),
                value: FilterValue::Int(42),
            },
            FilterExpr::Like {
                column: "name".to_string(),
                pattern: "%test%".to_string(),
            },
            FilterExpr::IsNull {
                column: "deleted_at".to_string(),
            },
        ];

        let ranked = ranker.rank(filters);

        // Ranking is by score = selectivity/cost (lower is better)
        // Eq: 0.01/1.0 = 0.01 (first - lowest selectivity per cost)
        // Like: ~0.2/5.06 ≈ 0.04 (second)
        // IsNull: 0.05/0.5 = 0.1 (third - highest score)
        assert!(ranked[0].score < ranked[1].score);
        assert!(ranked[1].score < ranked[2].score);
        assert!(matches!(ranked[0].filter, FilterExpr::Eq { .. }));
    }

    #[test]
    fn test_selectivity_estimation() {
        let ranker = FilterRanker::with_defaults();

        let eq_sel = ranker.estimate_selectivity(&FilterExpr::Eq {
            column: "x".to_string(),
            value: FilterValue::Int(1),
        });

        let ne_sel = ranker.estimate_selectivity(&FilterExpr::Ne {
            column: "x".to_string(),
            value: FilterValue::Int(1),
        });

        // ne should be 1 - eq
        assert!((eq_sel + ne_sel - 1.0).abs() < 0.0001);

        // AND should multiply
        let and_sel = ranker.estimate_selectivity(&FilterExpr::And(
            Box::new(FilterExpr::Eq {
                column: "x".to_string(),
                value: FilterValue::Int(1),
            }),
            Box::new(FilterExpr::Eq {
                column: "y".to_string(),
                value: FilterValue::Int(2),
            }),
        ));

        assert!((and_sel - eq_sel * eq_sel).abs() < 0.0001);
    }

    #[test]
    fn test_cost_estimation() {
        let ranker = FilterRanker::with_defaults();

        // NULL check should be cheap
        let null_cost = ranker.estimate_cost(&FilterExpr::IsNull {
            column: "x".to_string(),
        });

        // String comparison should be more expensive
        let string_cost = ranker.estimate_cost(&FilterExpr::Eq {
            column: "x".to_string(),
            value: FilterValue::String("a long string value".to_string()),
        });

        // Subquery should be very expensive
        let subquery_cost = ranker.estimate_cost(&FilterExpr::Subquery { id: 1 });

        assert!(null_cost < string_cost);
        assert!(string_cost < subquery_cost);
    }

    #[test]
    fn test_with_statistics() {
        let mut stats = TableStats::new("test".to_string(), 10000);
        stats.add_column(ColumnStats {
            name: "status".to_string(),
            ndv: 5, // 5 distinct values
            null_fraction: 0.0,
            min_value: None,
            max_value: None,
        });

        let ranker = FilterRanker::with_defaults().with_stats(stats);

        let sel = ranker.estimate_selectivity(&FilterExpr::Eq {
            column: "status".to_string(),
            value: FilterValue::String("active".to_string()),
        });

        // With NDV=5, selectivity should be ~0.2
        assert!((sel - 0.2).abs() < 0.01);
    }

    #[test]
    fn test_reorder() {
        let ranker = FilterRanker::with_defaults();

        let filters = vec![
            FilterExpr::Subquery { id: 1 }, // Very expensive but very selective
            FilterExpr::IsNull {
                column: "x".to_string(),
            }, // Cheap but less selective
            FilterExpr::Function {
                name: "expensive_fn".to_string(),
                args: vec![],
            }, // Expensive
        ];

        let reordered = ranker.reorder(filters);

        // Ranking by score = selectivity/cost (lower is better):
        // Subquery: 0.01/1000 = 0.00001 (first - best score due to high cost denominator)
        // Function: 0.01/10 = 0.001 (second)
        // IsNull: 0.05/0.5 = 0.1 (last - worst score)
        assert!(matches!(reordered[0], FilterExpr::Subquery { .. }));
        assert!(matches!(reordered[1], FilterExpr::Function { .. }));
        assert!(matches!(reordered[2], FilterExpr::IsNull { .. }));
    }

    #[test]
    fn test_in_selectivity() {
        let ranker = FilterRanker::with_defaults();

        let in_1 = ranker.estimate_selectivity(&FilterExpr::In {
            column: "x".to_string(),
            values: vec![FilterValue::Int(1)],
        });

        let in_5 = ranker.estimate_selectivity(&FilterExpr::In {
            column: "x".to_string(),
            values: vec![
                FilterValue::Int(1),
                FilterValue::Int(2),
                FilterValue::Int(3),
                FilterValue::Int(4),
                FilterValue::Int(5),
            ],
        });

        // IN with more values should have higher selectivity
        assert!(in_5 > in_1);
    }

    #[test]
    fn test_like_patterns() {
        let ranker = FilterRanker::with_defaults();

        let prefix = ranker.estimate_selectivity(&FilterExpr::Like {
            column: "x".to_string(),
            pattern: "test%".to_string(),
        });

        let contains = ranker.estimate_selectivity(&FilterExpr::Like {
            column: "x".to_string(),
            pattern: "%test%".to_string(),
        });

        // Prefix match should be more selective (lower)
        assert!(prefix < contains);
    }
}
