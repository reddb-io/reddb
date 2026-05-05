//! Cross-modal join query builder
//!
//! Builder for three-way cross-modal joins across Vector, Graph, and Table modalities.

use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;

use super::super::super::entity::{EntityId, UnifiedEntity};
use super::super::super::store::UnifiedStore;
use super::super::execution::execute_three_way_join;
use super::super::filters::{Filter, FilterAcceptor, WhereClause};
use super::super::types::QueryResult;
use super::graph::TraversalDirection;

/// Phase of the three-way join pipeline
#[derive(Debug, Clone)]
pub enum JoinPhase {
    /// Start with vector similarity search
    VectorStart { vector: Vec<f32>, k: usize },
    /// Start with graph traversal from a node
    GraphStart { node_id: String },
    /// Start with table scan
    TableStart { table: String },
}

/// Step in the join pipeline
#[derive(Debug, Clone)]
pub enum JoinStep {
    /// Graph traversal step
    Traverse {
        edge_label: Option<String>,
        depth: u32,
        direction: TraversalDirection,
    },
    /// Table join step
    JoinTable {
        table: String,
        on_field: Option<String>,
    },
    /// Vector expansion (find similar to matched entities)
    VectorExpand { k: usize },
}

/// Builder for three-way cross-modal joins
///
/// Supports chaining operations across Vector, Graph, and Table modalities
/// in any order, efficiently executing the combined query.
#[derive(Debug, Clone)]
pub struct ThreeWayJoinBuilder {
    /// Starting phase
    pub(crate) start: Option<JoinPhase>,
    /// Pipeline of join steps
    pub(crate) pipeline: Vec<JoinStep>,
    /// Filters applied at each stage
    pub(crate) filters: Vec<Filter>,
    /// Result limit
    pub(crate) limit: Option<usize>,
    /// Minimum score threshold
    pub(crate) min_score: f32,
    /// Score weights for each modality
    pub(crate) weights: CrossModalWeights,
}

/// Weights for cross-modal scoring
#[derive(Debug, Clone)]
pub struct CrossModalWeights {
    pub vector: f32,
    pub graph: f32,
    pub table: f32,
}

impl Default for CrossModalWeights {
    fn default() -> Self {
        Self {
            vector: 0.4,
            graph: 0.4,
            table: 0.2,
        }
    }
}

impl ThreeWayJoinBuilder {
    /// Create a new three-way join builder
    pub fn new() -> Self {
        Self {
            start: None,
            pipeline: Vec::new(),
            filters: Vec::new(),
            limit: None,
            min_score: 0.0,
            weights: CrossModalWeights::default(),
        }
    }

    /// Start with vector similarity search
    pub fn start_vector(mut self, vector: &[f32], k: usize) -> Self {
        self.start = Some(JoinPhase::VectorStart {
            vector: vector.to_vec(),
            k,
        });
        self
    }

    /// Start from a graph node
    pub fn start_node(mut self, node_id: impl Into<String>) -> Self {
        self.start = Some(JoinPhase::GraphStart {
            node_id: node_id.into(),
        });
        self
    }

    /// Start from a table
    pub fn start_table(mut self, table: impl Into<String>) -> Self {
        self.start = Some(JoinPhase::TableStart {
            table: table.into(),
        });
        self
    }

    /// Add graph traversal step
    pub fn traverse(mut self, edge_label: impl Into<String>, depth: u32) -> Self {
        self.pipeline.push(JoinStep::Traverse {
            edge_label: Some(edge_label.into()),
            depth,
            direction: TraversalDirection::Out,
        });
        self
    }

    /// Add graph traversal in any direction
    pub fn traverse_any(mut self, depth: u32) -> Self {
        self.pipeline.push(JoinStep::Traverse {
            edge_label: None,
            depth,
            direction: TraversalDirection::Both,
        });
        self
    }

    /// Add incoming edge traversal
    pub fn traverse_in(mut self, edge_label: impl Into<String>, depth: u32) -> Self {
        self.pipeline.push(JoinStep::Traverse {
            edge_label: Some(edge_label.into()),
            depth,
            direction: TraversalDirection::In,
        });
        self
    }

    /// Join with a table
    pub fn join_table(mut self, table: impl Into<String>) -> Self {
        self.pipeline.push(JoinStep::JoinTable {
            table: table.into(),
            on_field: None,
        });
        self
    }

    /// Join with a table on a specific field
    pub fn join_table_on(mut self, table: impl Into<String>, field: impl Into<String>) -> Self {
        self.pipeline.push(JoinStep::JoinTable {
            table: table.into(),
            on_field: Some(field.into()),
        });
        self
    }

    /// Expand to similar vectors
    pub fn expand_similar(mut self, k: usize) -> Self {
        self.pipeline.push(JoinStep::VectorExpand { k });
        self
    }

    /// Add filter
    pub fn where_(self, field: impl Into<String>) -> WhereClause<Self> {
        WhereClause::new(self, field.into())
    }

    /// Set result limit
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Set minimum score threshold
    pub fn min_score(mut self, score: f32) -> Self {
        self.min_score = score;
        self
    }

    /// Set scoring weights
    pub fn with_weights(mut self, vector: f32, graph: f32, table: f32) -> Self {
        self.weights = CrossModalWeights {
            vector,
            graph,
            table,
        };
        self
    }

    /// Execute the three-way cross-modal join
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_three_way_join(self, store)
    }
}

impl Default for ThreeWayJoinBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterAcceptor for ThreeWayJoinBuilder {
    fn add_filter(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
}

/// Internal match tracking during three-way join
pub struct CrossModalMatch {
    pub entity: UnifiedEntity,
    pub vector_score: f32,
    pub graph_score: f32,
    pub table_score: f32,
    pub path: Vec<EntityId>,
}
