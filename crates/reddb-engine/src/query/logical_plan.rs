//! Canonical logical plan data shared with native planning and browser execution.
use std::collections::BTreeMap;

/// Canonical logical plan used for explain/introspection during the planner transition.
#[derive(Debug, Clone, Default)]
pub struct CanonicalLogicalPlan {
    pub root: CanonicalLogicalNode,
}

/// Canonical logical operator node.
#[derive(Debug, Clone, Default)]
pub struct CanonicalLogicalNode {
    pub operator: String,
    pub source: Option<String>,
    pub details: BTreeMap<String, String>,
    pub estimated_rows: f64,
    pub estimated_selectivity: f64,
    pub estimated_confidence: f64,
    pub operator_cost: f64,
    pub children: Vec<CanonicalLogicalNode>,
}

/// Cardinality estimate for a query result
#[derive(Debug, Clone, Default)]
pub struct CardinalityEstimate {
    /// Estimated row/record count
    pub rows: f64,
    /// Selectivity factor (0.0 - 1.0)
    pub selectivity: f64,
    /// Confidence in the estimate (0.0 - 1.0)
    pub confidence: f64,
}

impl CardinalityEstimate {
    /// Create a new cardinality estimate
    pub fn new(rows: f64, selectivity: f64) -> Self {
        Self {
            rows,
            selectivity,
            confidence: 1.0,
        }
    }

    /// Full table scan estimate
    pub fn full_scan(table_size: f64) -> Self {
        Self {
            rows: table_size,
            selectivity: 1.0,
            confidence: 1.0,
        }
    }

    /// Apply a filter to reduce cardinality
    pub fn with_filter(mut self, filter_selectivity: f64) -> Self {
        self.rows *= filter_selectivity;
        self.selectivity *= filter_selectivity;
        self.confidence *= 0.9; // Reduce confidence with each estimate
        self
    }
}

#[derive(Debug, Clone)]
pub struct AccessPathDecision {
    pub path: &'static str,
    pub index_hint: Option<String>,
    pub reason: String,
    pub warning: Option<String>,
}
