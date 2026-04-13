//! Multi-Mode Query Executors
//!
//! Specialized executors for each query language mode:
//! - Gremlin: Traverser-based execution with path state tracking
//! - SPARQL: Pattern matching with variable bindings
//! - Natural: Translation + execution with explanation
//! - Vector: HNSW-based similarity search with metadata filtering
//! - Hybrid: Combined structured + vector search with fusion strategies
//!
//! Each executor converts its native AST to execution operations,
//! leveraging the unified executor for common operations.

pub mod aggregation;
pub mod cte;
pub mod gremlin;
pub mod hybrid;
pub mod join;
pub mod natural;
pub mod set_ops;
pub mod sparql;
pub mod subquery;
mod value_compare;
pub mod vector;
pub mod window;

pub use aggregation::{
    create_aggregator, execute_group_by, execute_having, AggregationDef, Aggregator, AvgAggregator,
    CountAggregator, CountDistinctAggregator, GroupConcatAggregator, MaxAggregator, MinAggregator,
    PercentileAggregator, SampleAggregator, StdDevAggregator, SumAggregator, VarianceAggregator,
};
pub use cte::{split_union_parts, CteContext, CteExecutor, CteStats};
pub use gremlin::GremlinExecutor;
pub use hybrid::{HybridExecutor, InMemoryHybridExecutor};
pub use join::{
    choose_strategy, execute_join, hash_join, merge_join, nested_loop_join, JoinCondition,
    JoinStats, JoinStrategy, JoinType,
};
pub use natural::NaturalExecutor;
pub use set_ops::{execute_set_op, set_except, set_intersect, set_union, SetOpStats, SetOpType};
pub use sparql::SparqlExecutor;
pub use subquery::{
    bind_outer_refs, detect_correlation, CompareOp, SubqueryCache, SubqueryDef, SubqueryExecutor,
    SubqueryType, ValueHash,
};
pub use vector::{InMemoryVectorExecutor, VectorExecutor};
pub use window::{
    FrameBound, FrameExclude, FrameSpec, FrameType, NullsOrder, SortDirection, WindowDef,
    WindowExecutor, WindowFunc, WindowFuncType, WindowOrderBy,
};

use std::sync::Arc;

use super::modes::{detect_mode, parse_multi, QueryMode};
use super::unified::{ExecutionError, UnifiedExecutor, UnifiedResult};
use crate::storage::engine::graph_store::GraphStore;
use crate::storage::engine::graph_table_index::GraphTableIndex;

/// Multi-mode executor that routes to specialized executors
pub struct MultiModeExecutor {
    /// Underlying unified executor for common operations
    unified: UnifiedExecutor,
    /// Gremlin executor for traversal queries
    gremlin: GremlinExecutor,
    /// SPARQL executor for triple pattern queries
    sparql: SparqlExecutor,
    /// Natural language executor
    natural: NaturalExecutor,
}

impl MultiModeExecutor {
    /// Create a new multi-mode executor
    pub fn new(graph: Arc<GraphStore>, index: Arc<GraphTableIndex>) -> Self {
        let unified = UnifiedExecutor::new(Arc::clone(&graph), Arc::clone(&index));
        let gremlin = GremlinExecutor::new(Arc::clone(&graph));
        let sparql = SparqlExecutor::new(Arc::clone(&graph));
        let natural = NaturalExecutor::new(Arc::clone(&graph));

        Self {
            unified,
            gremlin,
            sparql,
            natural,
        }
    }

    /// Execute a query string, auto-detecting the mode
    pub fn execute(&self, query: &str) -> Result<ExecuteResult, ExecutionError> {
        let mode = detect_mode(query);

        match mode {
            QueryMode::Sql | QueryMode::Cypher | QueryMode::Path => {
                // Use unified executor for these modes
                let ast = parse_multi(query).map_err(|e| ExecutionError::new(e.to_string()))?;
                let result = self.unified.execute(&ast)?;
                Ok(ExecuteResult {
                    result,
                    mode,
                    explanation: None,
                })
            }
            QueryMode::Gremlin => {
                let result = self.gremlin.execute(query)?;
                Ok(ExecuteResult {
                    result,
                    mode,
                    explanation: None,
                })
            }
            QueryMode::Sparql => {
                let result = self.sparql.execute(query)?;
                Ok(ExecuteResult {
                    result,
                    mode,
                    explanation: None,
                })
            }
            QueryMode::Natural => {
                let (result, explanation) = self.natural.execute_with_explanation(query)?;
                Ok(ExecuteResult {
                    result,
                    mode,
                    explanation: Some(explanation),
                })
            }
            QueryMode::Unknown => Err(ExecutionError::new(format!(
                "Cannot determine query mode for: {}",
                if query.len() > 50 {
                    &query[..50]
                } else {
                    query
                }
            ))),
        }
    }

    /// Execute with explicit mode
    pub fn execute_as(
        &self,
        query: &str,
        mode: QueryMode,
    ) -> Result<ExecuteResult, ExecutionError> {
        match mode {
            QueryMode::Sql | QueryMode::Cypher | QueryMode::Path => {
                let ast = parse_multi(query).map_err(|e| ExecutionError::new(e.to_string()))?;
                let result = self.unified.execute(&ast)?;
                Ok(ExecuteResult {
                    result,
                    mode,
                    explanation: None,
                })
            }
            QueryMode::Gremlin => {
                let result = self.gremlin.execute(query)?;
                Ok(ExecuteResult {
                    result,
                    mode,
                    explanation: None,
                })
            }
            QueryMode::Sparql => {
                let result = self.sparql.execute(query)?;
                Ok(ExecuteResult {
                    result,
                    mode,
                    explanation: None,
                })
            }
            QueryMode::Natural => {
                let (result, explanation) = self.natural.execute_with_explanation(query)?;
                Ok(ExecuteResult {
                    result,
                    mode,
                    explanation: Some(explanation),
                })
            }
            QueryMode::Unknown => Err(ExecutionError::new("Cannot execute unknown query mode")),
        }
    }
}

/// Result of executing a query with mode information
#[derive(Debug)]
pub struct ExecuteResult {
    /// The query result
    pub result: UnifiedResult,
    /// The detected/used query mode
    pub mode: QueryMode,
    /// Explanation (for natural language queries)
    pub explanation: Option<String>,
}

impl ExecuteResult {
    /// Check if an explanation is available
    pub fn has_explanation(&self) -> bool {
        self.explanation.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_executor() -> MultiModeExecutor {
        let graph = Arc::new(GraphStore::new());
        let index = Arc::new(GraphTableIndex::new());
        MultiModeExecutor::new(graph, index)
    }

    #[test]
    fn test_gremlin_mode_detection() {
        let executor = create_test_executor();
        let result = executor.execute("g.V()");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().mode, QueryMode::Gremlin);
    }

    #[test]
    fn test_sparql_mode_detection() {
        let executor = create_test_executor();
        let result = executor.execute("SELECT ?x WHERE { ?x :type :Host }");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().mode, QueryMode::Sparql);
    }

    #[test]
    fn test_natural_mode_detection() {
        let executor = create_test_executor();
        let result = executor.execute("find all hosts with port 22");
        assert!(result.is_ok());
        let exec_result = result.unwrap();
        assert_eq!(exec_result.mode, QueryMode::Natural);
        assert!(exec_result.explanation.is_some());
    }
}
