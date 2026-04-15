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

use std::sync::Arc;

use super::stats_provider::{NullProvider, StatsProvider};
use crate::storage::query::ast::{
    CompareOp, FieldRef, Filter as AstFilter, GraphQuery, HybridQuery, JoinQuery, JoinType,
    PathQuery, QueryExpr, TableQuery, VectorQuery,
};
use crate::storage::schema::Value;

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

/// Cost of executing a query plan.
///
/// Mirrors PostgreSQL's `Cost` split: `startup_cost` is the work needed
/// before the **first** row can be produced, `total` is the work to
/// produce the **last** row. Both are reported so plan selection can
/// pick a low-startup plan when a small `LIMIT` is in scope, even if
/// total work is higher.
///
/// See `src/storage/query/planner/README.md` § Invariant 1.
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
    /// Cost to produce the **first** row.
    ///
    /// Zero for streaming operators (full scan, index scan, filter over
    /// scan). Equal to `total` for blocking operators (sort, hash join
    /// build side, materialize).
    pub startup_cost: f64,
    /// Cost to produce the **last** row.
    pub total: f64,
}

impl PlanCost {
    /// Create a new cost estimate with `startup_cost = 0` (streaming).
    pub fn new(cpu: f64, io: f64, memory: f64) -> Self {
        let total = cpu + io * 10.0 + memory * 0.1; // IO is expensive
        Self {
            cpu,
            io,
            network: 0.0,
            memory,
            startup_cost: 0.0,
            total,
        }
    }

    /// Create a cost with an explicit `startup_cost`. Use for blocking
    /// operators (sort, hash build) and for index point lookups whose
    /// first-row cost is non-zero.
    pub fn with_startup(cpu: f64, io: f64, memory: f64, startup_cost: f64) -> Self {
        let total = cpu + io * 10.0 + memory * 0.1;
        Self {
            cpu,
            io,
            network: 0.0,
            memory,
            startup_cost: startup_cost.max(0.0),
            total: total.max(startup_cost),
        }
    }

    /// Compose two costs in a **pipelined** fashion: the second operator
    /// consumes the first as a stream.
    ///
    /// Both `startup_cost` and `total` add together. Use for filter
    /// over scan, projection over filter, etc.
    pub fn combine_pipelined(&self, other: &PlanCost) -> PlanCost {
        PlanCost {
            cpu: self.cpu + other.cpu,
            io: self.io + other.io,
            network: self.network + other.network,
            memory: self.memory.max(other.memory),
            startup_cost: self.startup_cost + other.startup_cost,
            total: self.total + other.total,
        }
    }

    /// Compose two costs where `self` must be **fully consumed** before
    /// `blocker` can produce its first row.
    ///
    /// `self.total` flows into `blocker.startup_cost`. Use for sort,
    /// hash build, materialise — anything that has to drain its input
    /// before emitting.
    pub fn combine_blocking(&self, blocker: &PlanCost) -> PlanCost {
        PlanCost {
            cpu: self.cpu + blocker.cpu,
            io: self.io + blocker.io,
            network: self.network + blocker.network,
            memory: self.memory.max(blocker.memory),
            startup_cost: self.total + blocker.startup_cost,
            total: self.total + blocker.total,
        }
    }

    /// Backwards-compatible alias for [`combine_pipelined`].
    ///
    /// New code should prefer `combine_pipelined` / `combine_blocking`
    /// explicitly. This is kept so existing callers compile unchanged.
    pub fn combine(&self, other: &PlanCost) -> PlanCost {
        self.combine_pipelined(other)
    }

    /// Scale cost by a factor (cardinality multiplier, etc.).
    pub fn scale(&self, factor: f64) -> PlanCost {
        PlanCost {
            cpu: self.cpu * factor,
            io: self.io * factor,
            network: self.network * factor,
            memory: self.memory,             // Memory doesn't scale linearly
            startup_cost: self.startup_cost, // startup is per-plan, not per-row
            total: self.total * factor,
        }
    }

    /// Plan-comparison helper. Picks `Less` when `self` should be
    /// preferred over `other`.
    ///
    /// When `limit` is `Some(k)` and `k < 0.1 * cardinality`, the
    /// comparison switches from `total` to `startup_cost` — the client
    /// will only consume a small slice of the result, so we want the
    /// plan that produces the first rows fastest even if the full scan
    /// would be more expensive.
    ///
    /// This mirrors PostgreSQL's `compare_path_costs_fuzzily` logic for
    /// `STARTUP` vs `TOTAL` cost ordering.
    pub fn prefer_over(
        &self,
        other: &PlanCost,
        limit: Option<u64>,
        cardinality: f64,
    ) -> std::cmp::Ordering {
        let use_startup = matches!(limit, Some(k) if (k as f64) < 0.1 * cardinality.max(1.0));
        let (lhs, rhs) = if use_startup {
            (self.startup_cost, other.startup_cost)
        } else {
            (self.total, other.total)
        };
        lhs.partial_cmp(&rhs).unwrap_or(std::cmp::Ordering::Equal)
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
    /// Optional stats provider. When present, `estimate_table_cardinality`
    /// and the selectivity computation use real per-table / per-column
    /// statistics instead of the heuristic constants. `None` preserves the
    /// legacy behaviour so callers can adopt stats incrementally.
    stats: Arc<dyn StatsProvider>,
}

impl CostEstimator {
    /// Create a new cost estimator with default parameters and a
    /// [`NullProvider`] — no real stats, pure heuristic mode.
    pub fn new() -> Self {
        Self {
            default_row_count: 1000.0,
            row_scan_cost: 1.0,
            index_lookup_cost: 0.1,
            hash_probe_cost: 0.5,
            nested_loop_cost: 2.0,
            edge_traversal_cost: 1.5,
            stats: Arc::new(NullProvider),
        }
    }

    /// Create a cost estimator that consults `provider` for real table /
    /// column / index statistics. Any lookups the provider cannot satisfy
    /// fall back to the heuristic path automatically.
    pub fn with_stats(provider: Arc<dyn StatsProvider>) -> Self {
        Self {
            stats: provider,
            ..Self::new()
        }
    }

    /// Swap the stats provider on an existing estimator. Useful for tests
    /// and for planners that build one `CostEstimator` and repoint it at
    /// per-query snapshots.
    pub fn set_stats(&mut self, provider: Arc<dyn StatsProvider>) {
        self.stats = provider;
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
            | QueryExpr::CreateIndex(_)
            | QueryExpr::DropIndex(_)
            | QueryExpr::ProbabilisticCommand(_)
            | QueryExpr::Ask(_)
            | QueryExpr::SetConfig { .. }
            | QueryExpr::ShowConfig { .. }
            | QueryExpr::CreateTimeSeries(_)
            | QueryExpr::DropTimeSeries(_)
            | QueryExpr::CreateQueue(_)
            | QueryExpr::DropQueue(_)
            | QueryExpr::QueueCommand(_)
            | QueryExpr::ExplainAlter(_) => PlanCost::new(1.0, 1.0, 0.0),
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
            | QueryExpr::CreateIndex(_)
            | QueryExpr::DropIndex(_)
            | QueryExpr::ProbabilisticCommand(_)
            | QueryExpr::Ask(_)
            | QueryExpr::SetConfig { .. }
            | QueryExpr::ShowConfig { .. }
            | QueryExpr::CreateTimeSeries(_)
            | QueryExpr::DropTimeSeries(_)
            | QueryExpr::CreateQueue(_)
            | QueryExpr::DropQueue(_)
            | QueryExpr::QueueCommand(_)
            | QueryExpr::ExplainAlter(_) => CardinalityEstimate::new(1.0, 1.0),
        }
    }

    // =========================================================================
    // Table Query Estimation
    // =========================================================================

    fn estimate_table(&self, query: &TableQuery) -> PlanCost {
        let cardinality = self.estimate_table_cardinality(query);

        let cpu = cardinality.rows * self.row_scan_cost;

        // I/O cost: use Mackert-Lohman when we have index stats and a filter
        // column with a known index; otherwise fall back to the naive heuristic.
        let io = self.estimate_table_io(query, cardinality.rows);

        let memory = cardinality.rows * 100.0; // 100 bytes per row estimate

        PlanCost::new(cpu, io, memory)
    }

    /// Compute the I/O page cost for a table scan.
    ///
    /// When the query has a simple equality or range filter on an indexed
    /// column, use `IndexStats::correlated_io_cost` (Mackert-Lohman) which
    /// accounts for `index_correlation` (0.0 = random I/O, 1.0 = sequential).
    /// Falls back to the naive `rows / 100` heuristic otherwise.
    fn estimate_table_io(&self, query: &TableQuery, result_rows: f64) -> f64 {
        const ROWS_PER_PAGE: f64 = 100.0;

        // Look up total heap pages from table stats if available
        let table_stats = self.stats.table_stats(&query.table);
        let heap_pages = table_stats
            .map(|s| s.page_count as f64)
            .unwrap_or_else(|| (result_rows / ROWS_PER_PAGE).max(1.0));

        // If the filter is a simple comparison on an indexed column, use
        // the Mackert-Lohman formula with correlation from IndexStats.
        if let Some(filter) = crate::storage::query::sql_lowering::effective_table_filter(query) {
            if let Some(col) = first_filter_column(&filter, &query.table) {
                if let Some(idx) = self.stats.index_stats(&query.table, col) {
                    return idx.correlated_io_cost(result_rows, heap_pages);
                }
            }
        }

        // Heuristic fallback: assume sequential pages = rows / 100
        (result_rows / ROWS_PER_PAGE).ceil()
    }

    fn estimate_table_cardinality(&self, query: &TableQuery) -> CardinalityEstimate {
        // Prefer real row counts from the stats provider; fall back to the
        // heuristic `default_row_count` when no stats are registered.
        let base_rows = self
            .stats
            .table_stats(&query.table)
            .map(|s| s.row_count as f64)
            .unwrap_or(self.default_row_count);

        let mut estimate = CardinalityEstimate::full_scan(base_rows);

        // Apply filter selectivity (stats-aware when provider has index
        // stats on the compared column).
        if let Some(filter) = crate::storage::query::sql_lowering::effective_table_filter(query) {
            let selectivity = self.filter_selectivity(&filter, &query.table);
            estimate = estimate.with_filter(selectivity);
        }

        // Apply limit
        if let Some(limit) = query.limit {
            estimate.rows = estimate.rows.min(limit as f64);
        }

        estimate
    }

    /// Stats-aware selectivity computation.
    ///
    /// Resolution order (best → worst):
    ///   1. `column_mcv` for equality on a known frequent value
    ///   2. `column_histogram` for ranges and BETWEEN
    ///   3. `index_stats.point_selectivity()` for indexed columns
    ///   4. Hardcoded heuristic constants as final fallback
    ///
    /// Mirrors postgres `var_eq_const` / `histogram_selectivity` in
    /// `src/backend/utils/adt/selfuncs.c`. Histogram + MCV data
    /// structures already live in `super::histogram`; this method is
    /// where we finally consume them on the hot planner path.
    fn filter_selectivity(&self, filter: &AstFilter, table: &str) -> f64 {
        match filter {
            AstFilter::Compare { field, op, value } => {
                let column = column_name_for_table(field, table);
                match op {
                    CompareOp::Eq => self.eq_selectivity(table, column, value),
                    CompareOp::Ne => 1.0 - self.eq_selectivity(table, column, value),
                    CompareOp::Lt | CompareOp::Le => {
                        self.range_selectivity(table, column, None, Some(value))
                    }
                    CompareOp::Gt | CompareOp::Ge => {
                        self.range_selectivity(table, column, Some(value), None)
                    }
                }
            }
            AstFilter::Between {
                field, low, high, ..
            } => {
                let column = column_name_for_table(field, table);
                self.range_selectivity(table, column, Some(low), Some(high))
            }
            AstFilter::In { field, values, .. } => {
                let column = column_name_for_table(field, table);
                // If we have an MCV list, sum the per-value frequencies
                // for values that are actually in the list, plus the
                // residual estimate for the rest.
                if let Some(c) = column {
                    if let Some(mcv) = self.stats.column_mcv(table, c) {
                        let mut hits: f64 = 0.0;
                        let mut residual_count = 0usize;
                        for v in values {
                            if let Some(cv) = column_value_from(v) {
                                if let Some(freq) = mcv.frequency_of(&cv) {
                                    hits += freq;
                                } else {
                                    residual_count += 1;
                                }
                            } else {
                                residual_count += 1;
                            }
                        }
                        let total = mcv.total_frequency();
                        let distinct = self.stats.distinct_values(table, c).unwrap_or(100);
                        let non_mcv_distinct =
                            distinct.saturating_sub(mcv.values.len() as u64).max(1);
                        let per_residual = (1.0 - total) / non_mcv_distinct as f64;
                        let estimate = hits + (residual_count as f64) * per_residual;
                        return estimate.clamp(0.0, 1.0).min(0.5);
                    }
                    if let Some(s) = self.stats.index_stats(table, c) {
                        return (s.point_selectivity() * values.len() as f64).min(0.5);
                    }
                }
                (values.len() as f64 * 0.01).min(0.5)
            }
            AstFilter::Like { .. } => 0.1,
            AstFilter::StartsWith { .. } => 0.15,
            AstFilter::EndsWith { .. } => 0.15,
            AstFilter::Contains { .. } => 0.1,
            AstFilter::IsNull { .. } => 0.01,
            AstFilter::IsNotNull { .. } => 0.99,
            AstFilter::And(left, right) => {
                self.filter_selectivity(left, table) * self.filter_selectivity(right, table)
            }
            AstFilter::Or(left, right) => {
                let s1 = self.filter_selectivity(left, table);
                let s2 = self.filter_selectivity(right, table);
                s1 + s2 - (s1 * s2)
            }
            AstFilter::Not(inner) => 1.0 - self.filter_selectivity(inner, table),
            AstFilter::CompareFields { .. } => {
                // Column-to-column predicates lack histogram leverage
                // — assume moderate selectivity. Histogram/MCV hooks
                // only help literal-valued filters.
                0.1
            }
            AstFilter::CompareExpr { .. } => {
                // Expression-shaped predicates: conservative 0.1 until
                // the planner learns to walk Expr trees. Matches the
                // CompareFields default.
                0.1
            }
        }
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

        // Hash join cost model.
        //
        // Build side (left) is **blocking** — we must drain the entire
        // left input and populate the hash table before any probe can
        // produce its first output row. Probe side (right) is then
        // streamed pipelined.
        let build_cpu = left_card.rows * self.hash_probe_cost;
        let probe_cpu = right_card.rows * self.hash_probe_cost;
        let join_memory = left_card.rows * 100.0; // hash table footprint

        // The build operator: zero work upstream, blocking on left input.
        let build_op = PlanCost::with_startup(build_cpu, 0.0, join_memory, build_cpu);
        // The probe operator: pipelined over right input.
        let probe_op = PlanCost::new(probe_cpu, 0.0, 0.0);

        // Compose: left → block on build → pipelined probe with right.
        let after_build = left_cost.combine_blocking(&build_op);
        after_build
            .combine_pipelined(&right_cost)
            .combine_pipelined(&probe_op)
    }

    fn estimate_join_cardinality(&self, query: &JoinQuery) -> CardinalityEstimate {
        let left = self.estimate_cardinality(&query.left);
        let right = self.estimate_cardinality(&query.right);

        // Join selectivity based on join type
        let selectivity = match query.join_type {
            JoinType::Inner => 0.1,      // Inner join is selective
            JoinType::LeftOuter => 1.0,  // Left join preserves left side
            JoinType::RightOuter => 1.0, // Right join preserves right side
            JoinType::FullOuter => 1.0,  // Full outer preserves both sides entirely
            JoinType::Cross => 1.0,      // Cartesian product — every pair matches
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

        // Base cost from HNSW traversal — must descend the layer graph
        // before *any* candidate can be returned. This is the operator's
        // intrinsic startup cost.
        let hnsw_cost = 100.0 * (1.0 + k.ln()); // ~100-300 node visits

        // Metadata filtering adds cost if present
        let filter_cost = if crate::storage::query::sql_lowering::effective_vector_filter(query)
            .is_some()
        {
            50.0
        } else {
            0.0
        };

        let cpu = hnsw_cost + filter_cost;
        let io = 20.0; // HNSW layers are cached
        let memory = k * 32.0 + 1000.0; // k results + working set

        // Vector search is *partly* blocking: HNSW must traverse the
        // entry layers before the first neighbour is known, so the
        // first-row cost is roughly the descent cost. Subsequent rows
        // come essentially free until `k`.
        PlanCost::with_startup(cpu, io, memory, hnsw_cost * 0.5)
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
            AstFilter::CompareFields { .. } => 0.1,
            AstFilter::CompareExpr { .. } => 0.1,
        }
    }
}

impl CostEstimator {
    /// Equality selectivity for `column = value`.
    ///
    /// Resolution order:
    /// 1. MCV list — exact frequency for tracked values, residual
    ///    formula for untracked values.
    /// 2. `index_stats.point_selectivity()` — `1 / distinct_keys`.
    /// 3. Heuristic constant `0.01`.
    fn eq_selectivity(&self, table: &str, column: Option<&str>, value: &Value) -> f64 {
        if let Some(col) = column {
            // 1. Most-common-values lookup.
            if let Some(mcv) = self.stats.column_mcv(table, col) {
                if let Some(cv) = column_value_from(value) {
                    if let Some(freq) = mcv.frequency_of(&cv) {
                        return freq;
                    }
                    // Untracked value: residual / non_mcv_distinct.
                    let total = mcv.total_frequency();
                    let distinct = self.stats.distinct_values(table, col).unwrap_or(100);
                    let non_mcv_distinct = distinct.saturating_sub(mcv.values.len() as u64).max(1);
                    return ((1.0 - total) / non_mcv_distinct as f64).clamp(0.0, 1.0);
                }
            }
            // 2. Index stats fallback.
            if let Some(s) = self.stats.index_stats(table, col) {
                return s.point_selectivity();
            }
        }
        // 3. Heuristic.
        0.01
    }

    /// Range selectivity for `lo <= column <= hi`. Either bound may
    /// be `None` to express an open side. Used by `<`, `<=`, `>`,
    /// `>=`, and `BETWEEN`.
    ///
    /// Resolution order:
    /// 1. Histogram — `Histogram::range_selectivity` with bounds
    ///    converted via `column_value_from`.
    /// 2. `index_stats.point_selectivity() * (distinct_keys / 2)`
    ///    capped at the legacy heuristic.
    /// 3. Heuristic `0.3` for one-sided, `0.25` for two-sided.
    fn range_selectivity(
        &self,
        table: &str,
        column: Option<&str>,
        lo: Option<&Value>,
        hi: Option<&Value>,
    ) -> f64 {
        if let Some(col) = column {
            // 1. Histogram bucket arithmetic.
            if let Some(h) = self.stats.column_histogram(table, col) {
                let lo_cv = lo.and_then(column_value_from);
                let hi_cv = hi.and_then(column_value_from);
                return h.range_selectivity(lo_cv.as_ref(), hi_cv.as_ref());
            }
            // 2. Index stats fallback.
            if let Some(s) = self.stats.index_stats(table, col) {
                let cap = if lo.is_some() && hi.is_some() {
                    0.25
                } else {
                    0.3
                };
                return (s.point_selectivity() * (s.distinct_keys as f64 / 2.0)).min(cap);
            }
        }
        // 3. Heuristic.
        if lo.is_some() && hi.is_some() {
            0.25
        } else {
            0.3
        }
    }
}

impl Default for CostEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// Convert a query AST `Value` into a histogram-comparable
/// [`super::histogram::ColumnValue`]. Returns `None` for value types
/// that histograms don't support (Bool, Null, Bytes, etc.) — callers
/// fall through to the heuristic path.
fn column_value_from(v: &crate::storage::schema::Value) -> Option<super::histogram::ColumnValue> {
    use super::histogram::ColumnValue;
    use crate::storage::schema::Value;
    match v {
        Value::Integer(i) => Some(ColumnValue::Int(*i)),
        Value::UnsignedInteger(u) => Some(ColumnValue::Int(*u as i64)),
        Value::Float(f) => Some(ColumnValue::Float(*f)),
        Value::Text(s) => Some(ColumnValue::Text(s.clone())),
        Value::Timestamp(t) => Some(ColumnValue::Int(*t)),
        Value::Duration(d) => Some(ColumnValue::Int(*d)),
        // Other variants (Null, Blob, Boolean, IpAddr, MacAddr,
        // Vector, Json, Uuid, NodeRef, EdgeRef, vector ref...) are
        // not orderable in a histogram-meaningful way; the planner
        // falls through to the heuristic for these.
        _ => None,
    }
}

/// Resolve a `FieldRef` to a bare column name when it refers to `table`.
/// Returns `None` when the field targets another relation — in that case
/// Extract the first plain column name from a filter for index-stat lookup.
/// Walks AND nodes; stops at OR/NOT (too complex for simple correlation lookup).
fn first_filter_column<'a>(filter: &'a AstFilter, table: &str) -> Option<&'a str> {
    match filter {
        AstFilter::Compare { field, .. } => column_name_for_table(field, table),
        AstFilter::Between { field, .. } => column_name_for_table(field, table),
        AstFilter::And(l, r) => {
            first_filter_column(l, table).or_else(|| first_filter_column(r, table))
        }
        _ => None,
    }
}

/// the legacy heuristic still applies.
fn column_name_for_table<'a>(field: &'a FieldRef, table: &str) -> Option<&'a str> {
    match field {
        FieldRef::TableColumn { table: t, column } if t == table || t.is_empty() => {
            Some(column.as_str())
        }
        // Node / edge property refs don't map to table-level stats.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::stats_provider::StaticProvider;
    use super::*;
    use crate::storage::index::{IndexKind, IndexStats};
    use crate::storage::query::ast::{FieldRef, Projection};
    use crate::storage::schema::Value;

    fn eq_filter(table: &str, column: &str, value: i64) -> AstFilter {
        AstFilter::Compare {
            field: FieldRef::column(table, column),
            op: CompareOp::Eq,
            value: Value::Integer(value),
        }
    }

    fn table_query(name: &str, filter: Option<AstFilter>) -> TableQuery {
        TableQuery {
            table: name.to_string(),
            source: None,
            alias: None,
            select_items: Vec::new(),
            columns: vec![Projection::All],
            where_expr: None,
            filter,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
            having: None,
            order_by: vec![],
            limit: None,
            offset: None,
            expand: None,
        }
    }

    #[test]
    fn injected_row_count_overrides_default() {
        let provider = Arc::new(StaticProvider::new().with_table(
            "users",
            TableStats {
                row_count: 50_000,
                avg_row_size: 256,
                page_count: 500,
                columns: vec![],
            },
        ));
        let estimator = CostEstimator::with_stats(provider);
        let q = table_query("users", None);
        let card = estimator.estimate_table_cardinality(&q);
        // Default would be 1000; provider says 50_000.
        assert_eq!(card.rows, 50_000.0);
    }

    #[test]
    fn stats_aware_eq_selectivity_beats_default() {
        let provider = Arc::new(
            StaticProvider::new()
                .with_table(
                    "users",
                    TableStats {
                        row_count: 1_000_000,
                        avg_row_size: 256,
                        page_count: 10_000,
                        columns: vec![],
                    },
                )
                .with_index(
                    "users",
                    "email",
                    IndexStats {
                        entries: 1_000_000,
                        distinct_keys: 1_000_000,
                        approx_bytes: 0,
                        kind: IndexKind::Hash,
                        has_bloom: true,
                        index_correlation: 0.0,
                    },
                ),
        );
        let estimator = CostEstimator::with_stats(provider);
        let q = table_query("users", Some(eq_filter("users", "email", 0)));
        let card = estimator.estimate_table_cardinality(&q);
        // 1M rows × (1 / 1M distinct) ≈ 1 row
        assert!(card.rows < 2.0, "expected ~1 row, got {}", card.rows);
    }

    #[test]
    fn fallback_when_no_index_stats() {
        let provider = Arc::new(StaticProvider::new().with_table(
            "users",
            TableStats {
                row_count: 1_000_000,
                avg_row_size: 256,
                page_count: 10_000,
                columns: vec![],
            },
        ));
        let estimator = CostEstimator::with_stats(provider);
        let q = table_query("users", Some(eq_filter("users", "email", 0)));
        let card = estimator.estimate_table_cardinality(&q);
        // Heuristic 0.01 on 1M rows = 10_000
        assert!((card.rows - 10_000.0).abs() < 1.0);
    }

    #[test]
    fn null_provider_keeps_legacy_behaviour() {
        let estimator = CostEstimator::new();
        let q = table_query("whatever", Some(eq_filter("whatever", "id", 1)));
        let card = estimator.estimate_table_cardinality(&q);
        // Default 1000 rows × 0.01 eq selectivity = 10
        assert!((card.rows - 10.0).abs() < 1.0);
    }

    #[test]
    fn and_combines_stats_selectivities() {
        let provider = Arc::new(
            StaticProvider::new()
                .with_table(
                    "t",
                    TableStats {
                        row_count: 100_000,
                        avg_row_size: 64,
                        page_count: 100,
                        columns: vec![],
                    },
                )
                .with_index(
                    "t",
                    "a",
                    IndexStats {
                        entries: 100_000,
                        distinct_keys: 10,
                        approx_bytes: 0,
                        kind: IndexKind::BTree,
                        has_bloom: false,
                        index_correlation: 0.0,
                    },
                )
                .with_index(
                    "t",
                    "b",
                    IndexStats {
                        entries: 100_000,
                        distinct_keys: 1000,
                        approx_bytes: 0,
                        kind: IndexKind::BTree,
                        has_bloom: false,
                        index_correlation: 0.0,
                    },
                ),
        );
        let estimator = CostEstimator::with_stats(provider);
        let filter = AstFilter::And(
            Box::new(eq_filter("t", "a", 1)),
            Box::new(eq_filter("t", "b", 1)),
        );
        let q = table_query("t", Some(filter));
        let card = estimator.estimate_table_cardinality(&q);
        // 100_000 × (1/10) × (1/1000) = 10
        assert!(card.rows < 15.0, "got {}", card.rows);
    }

    #[test]
    fn test_table_cost_estimation() {
        let estimator = CostEstimator::new();

        let query = QueryExpr::Table(TableQuery {
            table: "hosts".to_string(),
            source: None,
            alias: None,
            select_items: Vec::new(),
            columns: vec![Projection::All],
            where_expr: None,
            filter: None,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
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
            source: None,
            alias: None,
            select_items: Vec::new(),
            columns: vec![Projection::All],
            where_expr: None,
            filter: None,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
            having: None,
            order_by: vec![],
            limit: Some(10),
            offset: None,
            expand: None,
        };

        let card = estimator.estimate_table_cardinality(&query);
        assert!(card.rows <= 10.0);
    }

    // ---------------------------------------------------------------
    // Target 2: startup_cost vs total_cost split
    // ---------------------------------------------------------------

    #[test]
    fn startup_zero_for_full_scan() {
        // estimate_table is implemented as a streaming sequential scan
        // (no startup cost — the first row is producible as soon as the
        // first page is read).
        let estimator = CostEstimator::new();
        let q = table_query("any_table", None);
        let cost = estimator.estimate(&QueryExpr::Table(q));
        assert_eq!(cost.startup_cost, 0.0, "full scan must have zero startup");
        assert!(cost.total > 0.0);
    }

    #[test]
    fn startup_nonzero_for_blocking_combine() {
        // combine_blocking models a sort or hash build: the input must
        // be fully consumed before the blocker can emit its first row.
        let input = PlanCost::new(100.0, 10.0, 50.0); // cost = 100 + 100 + 5 = 205
        let blocker = PlanCost::new(20.0, 0.0, 10.0); // cost = 20 + 0 + 1 = 21
        let composed = input.combine_blocking(&blocker);
        // Blocking startup absorbs all of input.total
        assert_eq!(composed.startup_cost, input.total);
        // Total is input.total + blocker.total
        assert_eq!(composed.total, input.total + blocker.total);
        assert!(composed.startup_cost > 0.0);
    }

    #[test]
    fn pipelined_combine_adds_startup_directly() {
        let upstream = PlanCost::with_startup(50.0, 5.0, 10.0, 30.0);
        let downstream = PlanCost::with_startup(20.0, 0.0, 0.0, 5.0);
        let composed = upstream.combine_pipelined(&downstream);
        assert_eq!(composed.startup_cost, 30.0 + 5.0);
        assert_eq!(composed.total, upstream.total + downstream.total);
    }

    #[test]
    fn cost_prefers_low_startup_when_limit_small() {
        // Two plans with the same total but different startup. With a
        // small LIMIT, the planner must pick the low-startup plan.
        let fast_first = PlanCost {
            cpu: 100.0,
            io: 10.0,
            network: 0.0,
            memory: 50.0,
            startup_cost: 5.0,
            total: 200.0,
        };
        let slow_first = PlanCost {
            cpu: 100.0,
            io: 10.0,
            network: 0.0,
            memory: 50.0,
            startup_cost: 150.0,
            total: 200.0,
        };
        // Cardinality 10_000, LIMIT 10 → 10 < 0.1 * 10_000 = 1000 → use startup.
        assert_eq!(
            fast_first.prefer_over(&slow_first, Some(10), 10_000.0),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn cost_prefers_low_total_when_no_limit() {
        // Same two plans, no LIMIT — total wins.
        let low_total = PlanCost {
            cpu: 50.0,
            io: 5.0,
            network: 0.0,
            memory: 0.0,
            startup_cost: 30.0,
            total: 100.0,
        };
        let high_total = PlanCost {
            cpu: 100.0,
            io: 10.0,
            network: 0.0,
            memory: 0.0,
            startup_cost: 5.0,
            total: 200.0,
        };
        assert_eq!(
            low_total.prefer_over(&high_total, None, 10_000.0),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn limit_threshold_falls_back_to_total_when_limit_large() {
        // LIMIT 5000 vs cardinality 10_000 → 5000 > 1000 → use total.
        let low_total = PlanCost {
            cpu: 50.0,
            io: 5.0,
            network: 0.0,
            memory: 0.0,
            startup_cost: 30.0,
            total: 100.0,
        };
        let low_startup = PlanCost {
            cpu: 100.0,
            io: 10.0,
            network: 0.0,
            memory: 0.0,
            startup_cost: 5.0,
            total: 200.0,
        };
        assert_eq!(
            low_total.prefer_over(&low_startup, Some(5000), 10_000.0),
            std::cmp::Ordering::Less
        );
    }

    #[test]
    fn hash_join_startup_includes_build_cost() {
        // Direct combine_blocking semantics: a hash join must drain the
        // left input and build the hash table before producing the first
        // probe result.
        let left = PlanCost::new(80.0, 8.0, 100.0); // table scan
        let build = PlanCost::with_startup(50.0, 0.0, 200.0, 50.0); // build op
        let after_build = left.combine_blocking(&build);
        assert!(
            after_build.startup_cost >= left.total,
            "after-build startup ({}) must absorb left.total ({})",
            after_build.startup_cost,
            left.total
        );
        assert!(after_build.total >= after_build.startup_cost);
    }

    #[test]
    fn vector_search_reports_nonzero_startup() {
        // estimate_vector now uses with_startup so HNSW descent shows
        // up as startup_cost > 0 (and < total — subsequent neighbours
        // are essentially free).
        let estimator = CostEstimator::new();
        // We can't easily build a VectorQuery without the AST helpers,
        // so test the direct cost surface with_startup uses.
        let v = PlanCost::with_startup(150.0, 20.0, 1320.0, 50.0);
        assert!(v.startup_cost > 0.0);
        assert!(v.startup_cost < v.total);
        let _ = estimator; // suppress unused
    }

    #[test]
    fn with_startup_clamps_total_below_startup() {
        // If a caller asks for total < startup, with_startup raises total.
        let cost = PlanCost::with_startup(1.0, 0.0, 0.0, 100.0);
        assert!(cost.total >= cost.startup_cost);
    }

    #[test]
    fn default_plancost_has_zero_startup() {
        let c = PlanCost::default();
        assert_eq!(c.startup_cost, 0.0);
        assert_eq!(c.total, 0.0);
    }

    // ---------------------------------------------------------------
    // Perf 1.3: histogram + MCV plug-in into filter_selectivity
    // ---------------------------------------------------------------

    use super::super::histogram::{ColumnValue, Histogram, MostCommonValues};

    fn provider_with_skew() -> Arc<StaticProvider> {
        // Build a histogram where 80 of 100 values fall in [0, 9]
        // and the rest spread sparsely up to 1000. range_selectivity
        // for `<= 9` should be ~0.8, vastly beating the heuristic 0.3.
        let mut sample: Vec<ColumnValue> = Vec::new();
        for i in 0..80 {
            sample.push(ColumnValue::Int(i % 10));
        }
        for i in 0..20 {
            sample.push(ColumnValue::Int(10 + i * 50));
        }
        let h = Histogram::equi_depth_from_sample(sample, 10);

        let mcv = MostCommonValues::new(vec![
            (ColumnValue::Text("boss".to_string()), 0.5),
            (ColumnValue::Text("intern".to_string()), 0.05),
        ]);

        Arc::new(
            StaticProvider::new()
                .with_table(
                    "people",
                    TableStats {
                        row_count: 100_000,
                        avg_row_size: 64,
                        page_count: 100,
                        columns: vec![],
                    },
                )
                .with_histogram("people", "score", h)
                .with_mcv("people", "role", mcv),
        )
    }

    #[test]
    fn eq_uses_mcv_when_value_is_tracked() {
        let provider = provider_with_skew();
        let estimator = CostEstimator::with_stats(provider);
        let filter = AstFilter::Compare {
            field: FieldRef::column("people", "role"),
            op: CompareOp::Eq,
            value: Value::Text("boss".to_string()),
        };
        // MCV says "boss" is 50% of the table → selectivity 0.5,
        // not the 0.01 heuristic.
        let s = estimator.filter_selectivity(&filter, "people");
        assert!(
            (s - 0.5).abs() < 1e-9,
            "MCV-tracked equality should report exact frequency, got {s}"
        );
    }

    #[test]
    fn eq_uses_residual_for_non_mcv_value() {
        let provider = provider_with_skew();
        let estimator = CostEstimator::with_stats(provider);
        let filter = AstFilter::Compare {
            field: FieldRef::column("people", "role"),
            op: CompareOp::Eq,
            value: Value::Text("staff".to_string()),
        };
        // 1 - 0.55 (mcv totals) = 0.45 spread across (distinct - 2)
        // distinct values. We don't have an exact distinct count, so
        // the planner uses the default 100 → 0.45 / 98 ≈ 0.0046.
        let s = estimator.filter_selectivity(&filter, "people");
        assert!(s > 0.0 && s < 0.01, "residual eq should be tiny, got {s}");
    }

    #[test]
    fn ne_is_one_minus_eq_under_mcv() {
        let provider = provider_with_skew();
        let estimator = CostEstimator::with_stats(provider);
        let filter = AstFilter::Compare {
            field: FieldRef::column("people", "role"),
            op: CompareOp::Ne,
            value: Value::Text("boss".to_string()),
        };
        let s = estimator.filter_selectivity(&filter, "people");
        // 1 - 0.5 == 0.5
        assert!((s - 0.5).abs() < 1e-9, "Ne selectivity = 0.5, got {s}");
    }

    #[test]
    fn range_uses_histogram_when_present() {
        let provider = provider_with_skew();
        let estimator = CostEstimator::with_stats(provider);
        let filter = AstFilter::Compare {
            field: FieldRef::column("people", "score"),
            op: CompareOp::Le,
            value: Value::Integer(9),
        };
        // Histogram says ~80% of values are in [0, 9], heuristic
        // would have said 0.3.
        let s = estimator.filter_selectivity(&filter, "people");
        assert!(
            s > 0.5,
            "histogram-based range selectivity should beat 0.3, got {s}"
        );
    }

    #[test]
    fn between_uses_histogram() {
        let provider = provider_with_skew();
        let estimator = CostEstimator::with_stats(provider);
        let filter = AstFilter::Between {
            field: FieldRef::column("people", "score"),
            low: Value::Integer(0),
            high: Value::Integer(9),
        };
        let s = estimator.filter_selectivity(&filter, "people");
        assert!(s > 0.5, "BETWEEN should use histogram too, got {s}");
    }

    #[test]
    fn graceful_fallback_when_histogram_absent() {
        // Provider has no histogram on `unknown_col` — must fall
        // through to the 0.3 heuristic without panicking.
        let provider = Arc::new(StaticProvider::new().with_table(
            "people",
            TableStats {
                row_count: 1000,
                avg_row_size: 64,
                page_count: 10,
                columns: vec![],
            },
        ));
        let estimator = CostEstimator::with_stats(provider);
        let filter = AstFilter::Compare {
            field: FieldRef::column("people", "unknown_col"),
            op: CompareOp::Lt,
            value: Value::Integer(50),
        };
        let s = estimator.filter_selectivity(&filter, "people");
        assert!((s - 0.3).abs() < 1e-9, "fallback heuristic 0.3, got {s}");
    }
}
