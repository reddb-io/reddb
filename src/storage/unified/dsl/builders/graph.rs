//! Graph traversal query builder
//!
//! Builder for graph pattern matching and traversal queries.

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;
use crate::storage::schema::Value;

use super::super::super::entity::EntityId;
use super::super::super::store::UnifiedStore;
use super::super::execution::execute_graph_query;
use super::super::filters::{Filter, FilterAcceptor, WhereClause};
use super::super::types::QueryResult;

/// Builder for graph traversal queries
#[derive(Debug, Clone)]
pub struct GraphQueryBuilder {
    pub(crate) start: GraphStartPoint,
    pub(crate) traversals: Vec<TraversalStep>,
    pub(crate) filters: Vec<Filter>,
    pub(crate) max_depth: u32,
    pub(crate) ranking_vector: Option<Vec<f32>>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub enum GraphStartPoint {
    NodeLabel(String),
    EntityId(EntityId),
    Pattern(NodePatternDsl),
}

#[derive(Debug, Clone)]
pub struct TraversalStep {
    pub edge_label: Option<String>,
    pub direction: TraversalDirection,
    pub node_filter: Option<NodePatternDsl>,
}

#[derive(Debug, Clone, Copy)]
pub enum TraversalDirection {
    Out,
    In,
    Both,
}

#[derive(Debug, Clone, Default)]
pub struct NodePatternDsl {
    pub labels: Vec<String>,
    pub properties: HashMap<String, Value>,
}

impl GraphQueryBuilder {
    pub fn from_node(label: impl Into<String>) -> Self {
        Self {
            start: GraphStartPoint::NodeLabel(label.into()),
            traversals: Vec::new(),
            filters: Vec::new(),
            max_depth: 3,
            ranking_vector: None,
            limit: None,
        }
    }

    pub fn from_id(id: EntityId) -> Self {
        Self {
            start: GraphStartPoint::EntityId(id),
            traversals: Vec::new(),
            filters: Vec::new(),
            max_depth: 3,
            ranking_vector: None,
            limit: None,
        }
    }

    /// Traverse outgoing edges with given label
    pub fn traverse(mut self, edge_label: impl Into<String>) -> Self {
        self.traversals.push(TraversalStep {
            edge_label: Some(edge_label.into()),
            direction: TraversalDirection::Out,
            node_filter: None,
        });
        self
    }

    /// Traverse outgoing edges (any label)
    pub fn out(mut self) -> Self {
        self.traversals.push(TraversalStep {
            edge_label: None,
            direction: TraversalDirection::Out,
            node_filter: None,
        });
        self
    }

    /// Traverse incoming edges (any label)
    pub fn in_(mut self) -> Self {
        self.traversals.push(TraversalStep {
            edge_label: None,
            direction: TraversalDirection::In,
            node_filter: None,
        });
        self
    }

    /// Traverse in both directions
    pub fn both(mut self) -> Self {
        self.traversals.push(TraversalStep {
            edge_label: None,
            direction: TraversalDirection::Both,
            node_filter: None,
        });
        self
    }

    /// Set maximum traversal depth
    pub fn depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    /// Add a filter condition
    pub fn where_(self, field: impl Into<String>) -> WhereClause<Self> {
        WhereClause::new(self, field.into())
    }

    /// Rank results by vector similarity
    pub fn ranked_by(mut self, vector: &[f32]) -> Self {
        self.ranking_vector = Some(vector.to_vec());
        self
    }

    /// Limit results
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Execute the query
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_graph_query(self, store)
    }
}

impl FilterAcceptor for GraphQueryBuilder {
    fn add_filter(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
}
