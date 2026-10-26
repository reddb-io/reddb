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
