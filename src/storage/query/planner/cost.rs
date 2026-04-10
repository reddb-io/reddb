//! Cost Estimation
//!
//! Cost-based query plan selection with cardinality estimation.
//!
//! # Cost Model
//!
//! - **CPU cost**: Computation overhead
//! - **IO cost**: Disk/memory access
//! - **Network cost**: For distributed queries
//! - **Memory cost**: Working memory required

use crate::storage::query::ast::{
    CompareOp, Filter as AstFilter, GraphQuery, HybridQuery, JoinQuery, JoinType, PathQuery,
    QueryExpr, TableQuery, VectorQuery,
};

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

/// Cost of executing a query plan
#[derive(Debug, Clone, Default)]
pub struct PlanCost {
    /// CPU computation cost
    pub cpu: f64,
    /// IO access cost
    pub io: f64,
    /// Network transfer cost (for distributed)
    pub network: f64,
    /// Memory requirement
    pub memory: f64,
    /// Total combined cost
    pub total: f64,
}

impl PlanCost {
    /// Create a new cost estimate
    pub fn new(cpu: f64, io: f64, memory: f64) -> Self {
        let total = cpu + io * 10.0 + memory * 0.1; // IO is expensive
        Self {
            cpu,
            io,
            network: 0.0,
            memory,
            total,
        }
    }

    /// Combine two costs (for joins, etc.)
    pub fn combine(&self, other: &PlanCost) -> PlanCost {
        PlanCost {
            cpu: self.cpu + other.cpu,
            io: self.io + other.io,
            network: self.network + other.network,
            memory: self.memory.max(other.memory), // Peak memory
            total: self.total + other.total,
        }
    }

    /// Scale cost by a factor
    pub fn scale(&self, factor: f64) -> PlanCost {
        PlanCost {
            cpu: self.cpu * factor,
            io: self.io * factor,
            network: self.network * factor,
            memory: self.memory, // Memory doesn't scale linearly
            total: self.total * factor,
        }
    }
}

/// Statistics about a table or graph
#[derive(Debug, Clone, Default)]
pub struct TableStats {
    /// Total row count
    pub row_count: u64,
    /// Average row size in bytes
    pub avg_row_size: u32,
    /// Number of pages
    pub page_count: u64,
    /// Column statistics
    pub columns: Vec<ColumnStats>,
}

/// Statistics about a column
#[derive(Debug, Clone, Default)]
pub struct ColumnStats {
    /// Column name
    pub name: String,
    /// Number of distinct values
    pub distinct_count: u64,
    /// Null count
    pub null_count: u64,
    /// Minimum value (if orderable)
    pub min_value: Option<String>,
    /// Maximum value (if orderable)
    pub max_value: Option<String>,
    /// Has index
    pub has_index: bool,
}

/// Cost estimator for query plans
pub struct CostEstimator {
    /// Default table row count estimate
    default_row_count: f64,
    /// Cost per row scan
    row_scan_cost: f64,
    /// Cost per index lookup
    index_lookup_cost: f64,
    /// Cost per hash join probe
    hash_probe_cost: f64,
    /// Cost per nested loop iteration
    nested_loop_cost: f64,
    /// Cost per graph edge traversal
    edge_traversal_cost: f64,
}

impl CostEstimator {
    /// Create a new cost estimator with default parameters
    pub fn new() -> Self {
        Self {
            default_row_count: 1000.0,
            row_scan_cost: 1.0,
            index_lookup_cost: 0.1,
            hash_probe_cost: 0.5,
            nested_loop_cost: 2.0,
            edge_traversal_cost: 1.5,
        }
    }

    /// Estimate cost of a query expression
    pub fn estimate(&self, query: &QueryExpr) -> PlanCost {
        match query {
            QueryExpr::Table(tq) => self.estimate_table(tq),
            QueryExpr::Graph(gq) => self.estimate_graph(gq),
            QueryExpr::Join(jq) => self.estimate_join(jq),
            QueryExpr::Path(pq) => self.estimate_path(pq),
            QueryExpr::Vector(vq) => self.estimate_vector(vq),
            QueryExpr::Hybrid(hq) => self.estimate_hybrid(hq),
            // DML/DDL statements have minimal query cost
            QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)
            | QueryExpr::Ask(_) => PlanCost::new(1.0, 1.0, 0.0),
        }
    }

    /// Estimate cardinality of a query result
    pub fn estimate_cardinality(&self, query: &QueryExpr) -> CardinalityEstimate {
        match query {
            QueryExpr::Table(tq) => self.estimate_table_cardinality(tq),
            QueryExpr::Graph(gq) => self.estimate_graph_cardinality(gq),
            QueryExpr::Join(jq) => self.estimate_join_cardinality(jq),
            QueryExpr::Path(pq) => self.estimate_path_cardinality(pq),
            QueryExpr::Vector(vq) => self.estimate_vector_cardinality(vq),
            QueryExpr::Hybrid(hq) => self.estimate_hybrid_cardinality(hq),
            // DML/DDL/Command statements return affected-row count or nothing
            QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)
            | QueryExpr::Ask(_) => CardinalityEstimate::new(1.0, 1.0),
        }
    }

    // =========================================================================
    // Table Query Estimation
    // =========================================================================

    fn estimate_table(&self, query: &TableQuery) -> PlanCost {
        let cardinality = self.estimate_table_cardinality(query);

        let cpu = cardinality.rows * self.row_scan_cost;
        let io = (cardinality.rows / 100.0).ceil(); // Assume 100 rows per page
        let memory = cardinality.rows * 100.0; // 100 bytes per row estimate

        PlanCost::new(cpu, io, memory)
    }

    fn estimate_table_cardinality(&self, query: &TableQuery) -> CardinalityEstimate {
        let mut estimate = CardinalityEstimate::full_scan(self.default_row_count);

        // Apply filter selectivity
        if let Some(ref filter) = query.filter {
            let selectivity = Self::estimate_filter_selectivity(filter);
            estimate = estimate.with_filter(selectivity);
        }

        // Apply limit
        if let Some(limit) = query.limit {
            estimate.rows = estimate.rows.min(limit as f64);
        }

        estimate
    }

    // =========================================================================
    // Graph Query Estimation
    // =========================================================================

    fn estimate_graph(&self, query: &GraphQuery) -> PlanCost {
        let cardinality = self.estimate_graph_cardinality(query);

        // Graph queries are more expensive due to pointer chasing
        let nodes = query.pattern.nodes.len() as f64;
        let edges = query.pattern.edges.len() as f64;

        let cpu = cardinality.rows * self.edge_traversal_cost * (nodes + edges);
        let io = cardinality.rows * 0.1; // More random IO
        let memory = cardinality.rows * 200.0; // Larger due to paths

        PlanCost::new(cpu, io, memory)
    }

    fn estimate_graph_cardinality(&self, query: &GraphQuery) -> CardinalityEstimate {
        let nodes = query.pattern.nodes.len() as f64;
        let edges = query.pattern.edges.len() as f64;

        // Each edge reduces cardinality
        let base_rows = self.default_row_count;
        let edge_factor = 0.1_f64.powf(edges); // Each edge is highly selective

        let mut estimate = CardinalityEstimate::new(base_rows * nodes * edge_factor, edge_factor);
        estimate.confidence = 0.5; // Graph estimates are less accurate

        // Apply filter
        if let Some(ref filter) = query.filter {
            let selectivity = Self::estimate_filter_selectivity(filter);
            estimate = estimate.with_filter(selectivity);
        }

        estimate
    }

    // =========================================================================
    // Join Query Estimation
    // =========================================================================

    fn estimate_join(&self, query: &JoinQuery) -> PlanCost {
        let left_cost = self.estimate(&query.left);
        let right_cost = self.estimate(&query.right);

        let left_card = self.estimate_cardinality(&query.left);
        let right_card = self.estimate_cardinality(&query.right);

        // Hash join cost model
        let build_cost = left_card.rows * self.hash_probe_cost;
        let probe_cost = right_card.rows * self.hash_probe_cost;

        let join_cpu = build_cost + probe_cost;
        let join_io = 0.0; // Hash join is in-memory
        let join_memory = left_card.rows * 100.0; // Hash table

        let join_cost = PlanCost::new(join_cpu, join_io, join_memory);

        left_cost.combine(&right_cost).combine(&join_cost)
    }

    fn estimate_join_cardinality(&self, query: &JoinQuery) -> CardinalityEstimate {
        let left = self.estimate_cardinality(&query.left);
        let right = self.estimate_cardinality(&query.right);

        // Join selectivity based on join type
        let selectivity = match query.join_type {
            JoinType::Inner => 0.1,      // Inner join is selective
            JoinType::LeftOuter => 1.0,  // Left join preserves left side
            JoinType::RightOuter => 1.0, // Right join preserves right side
        };

        CardinalityEstimate::new(
            left.rows * right.rows * selectivity,
            left.selectivity * right.selectivity * selectivity,
        )
    }

    // =========================================================================
    // Path Query Estimation
    // =========================================================================

    fn estimate_path(&self, query: &PathQuery) -> PlanCost {
        let cardinality = self.estimate_path_cardinality(query);

        // BFS/DFS cost
        let max_hops = query.max_length;
        let branching_factor: f64 = 5.0; // Average edges per node

        let nodes_visited = branching_factor.powf(max_hops as f64).min(10000.0);
        let cpu = nodes_visited * self.edge_traversal_cost;
        let io = nodes_visited * 0.1;
        let memory = nodes_visited * 50.0; // Visited set

        PlanCost::new(cpu, io, memory)
    }

    fn estimate_path_cardinality(&self, query: &PathQuery) -> CardinalityEstimate {
        // Path queries typically return few results
        let max_paths = 10.0;
        CardinalityEstimate::new(max_paths, 0.001)
    }

    // =========================================================================
    // Vector Query Estimation
    // =========================================================================

    fn estimate_vector(&self, query: &VectorQuery) -> PlanCost {
        // HNSW search is O(log n) with relatively low constant
        // Typical search visits ~100-500 nodes for 1M vectors
        let k = query.k as f64;

        // Base cost from HNSW traversal
        let hnsw_cost = 100.0 * (1.0 + k.ln()); // ~100-300 node visits

        // Metadata filtering adds cost if present
        let filter_cost = if query.filter.is_some() { 50.0 } else { 0.0 };

        let cpu = hnsw_cost + filter_cost;
        let io = 20.0; // HNSW layers are cached
        let memory = k * 32.0 + 1000.0; // k results + working set

        PlanCost::new(cpu, io, memory)
    }

    fn estimate_vector_cardinality(&self, query: &VectorQuery) -> CardinalityEstimate {
        // Vector search returns exactly k results (or fewer if not enough vectors)
        let k = query.k as f64;
        CardinalityEstimate::new(k, 0.1)
    }

    // =========================================================================
    // Hybrid Query Estimation
    // =========================================================================

    fn estimate_hybrid(&self, query: &HybridQuery) -> PlanCost {
        // Hybrid cost = structured + vector + fusion overhead
        let structured_cost = self.estimate(&query.structured);
        let vector_cost = self.estimate_vector(&query.vector);

        // Fusion overhead depends on strategy
        let fusion_overhead = match &query.fusion {
            crate::storage::query::ast::FusionStrategy::Rerank { .. } => 50.0,
            crate::storage::query::ast::FusionStrategy::FilterThenSearch => 10.0,
            crate::storage::query::ast::FusionStrategy::SearchThenFilter => 10.0,
            crate::storage::query::ast::FusionStrategy::RRF { .. } => 30.0,
            crate::storage::query::ast::FusionStrategy::Intersection => 20.0,
            crate::storage::query::ast::FusionStrategy::Union { .. } => 40.0,
        };

        PlanCost::new(
            structured_cost.cpu + vector_cost.cpu + fusion_overhead,
            structured_cost.io + vector_cost.io,
            structured_cost.memory + vector_cost.memory,
        )
    }

    fn estimate_hybrid_cardinality(&self, query: &HybridQuery) -> CardinalityEstimate {
        let structured_card = self.estimate_cardinality(&query.structured);
        let vector_card = self.estimate_vector_cardinality(&query.vector);

        // Result size depends on fusion strategy
        let rows = match &query.fusion {
            crate::storage::query::ast::FusionStrategy::Intersection => {
                structured_card.rows.min(vector_card.rows)
            }
            crate::storage::query::ast::FusionStrategy::Union { .. } => {
                structured_card.rows + vector_card.rows
            }
            _ => vector_card.rows, // Rerank and filter strategies return vector k
        };

        CardinalityEstimate::new(rows, 0.2)
    }

    // =========================================================================
    // Filter Selectivity
    // =========================================================================

    fn estimate_filter_selectivity(filter: &AstFilter) -> f64 {
        match filter {
            AstFilter::Compare { op, .. } => {
                match op {
                    CompareOp::Eq => 0.01, // Equality is very selective
                    CompareOp::Ne => 0.99, // Inequality is not selective
                    CompareOp::Lt | CompareOp::Le => 0.3,
                    CompareOp::Gt | CompareOp::Ge => 0.3,
                }
            }
            AstFilter::Between { .. } => 0.25,
            AstFilter::In { values, .. } => {
                // Each value adds 1% selectivity
                (values.len() as f64 * 0.01).min(0.5)
            }
            AstFilter::Like { .. } => 0.1,
            AstFilter::StartsWith { .. } => 0.15,
            AstFilter::EndsWith { .. } => 0.15,
            AstFilter::Contains { .. } => 0.1,
            AstFilter::IsNull { .. } => 0.01,
            AstFilter::IsNotNull { .. } => 0.99,
            AstFilter::And(left, right) => {
                Self::estimate_filter_selectivity(left) * Self::estimate_filter_selectivity(right)
            }
            AstFilter::Or(left, right) => {
                let s1 = Self::estimate_filter_selectivity(left);
                let s2 = Self::estimate_filter_selectivity(right);
                s1 + s2 - (s1 * s2) // Inclusion-exclusion
            }
            AstFilter::Not(inner) => 1.0 - Self::estimate_filter_selectivity(inner),
        }
    }
}

impl Default for CostEstimator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{FieldRef, Projection};
    use crate::storage::schema::Value;

    #[test]
    fn test_table_cost_estimation() {
        let estimator = CostEstimator::new();

        let query = QueryExpr::Table(TableQuery {
            table: "hosts".to_string(),
            alias: None,
            columns: vec![Projection::All],
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: vec![],
            limit: None,
            offset: None,
            expand: None,
        });

        let cost = estimator.estimate(&query);
        assert!(cost.cpu > 0.0);
        assert!(cost.total > 0.0);
    }

    #[test]
    fn test_filter_selectivity() {
        let estimator = CostEstimator::new();

        let eq_filter = AstFilter::Compare {
            field: FieldRef::column("hosts", "id"),
            op: CompareOp::Eq,
            value: Value::Integer(1),
        };
        assert!(CostEstimator::estimate_filter_selectivity(&eq_filter) < 0.1);

        let ne_filter = AstFilter::Compare {
            field: FieldRef::column("hosts", "id"),
            op: CompareOp::Ne,
            value: Value::Integer(1),
        };
        assert!(CostEstimator::estimate_filter_selectivity(&ne_filter) > 0.9);
    }

    #[test]
    fn test_and_selectivity() {
        let estimator = CostEstimator::new();

        let and_filter = AstFilter::And(
            Box::new(AstFilter::Compare {
                field: FieldRef::column("hosts", "a"),
                op: CompareOp::Eq,
                value: Value::Integer(1),
            }),
            Box::new(AstFilter::Compare {
                field: FieldRef::column("hosts", "b"),
                op: CompareOp::Eq,
                value: Value::Integer(2),
            }),
        );

        let selectivity = CostEstimator::estimate_filter_selectivity(&and_filter);
        assert!(selectivity < 0.01); // AND should be very selective
    }

    #[test]
    fn test_cardinality_with_limit() {
        let estimator = CostEstimator::new();

        let query = TableQuery {
            table: "hosts".to_string(),
            alias: None,
            columns: vec![Projection::All],
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: vec![],
            limit: Some(10),
            offset: None,
            expand: None,
        };

        let card = estimator.estimate_table_cardinality(&query);
        assert!(card.rows <= 10.0);
    }
}
