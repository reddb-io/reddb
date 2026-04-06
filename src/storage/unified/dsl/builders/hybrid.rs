//! Hybrid query builder
//!
//! Builder for complex hybrid queries combining vector, graph, and filter operations.

use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;

use super::super::super::entity::RefType;
use super::super::super::store::UnifiedStore;
use super::super::execution::execute_hybrid_query;
use super::super::filters::{Filter, FilterAcceptor, WhereClause};
use super::super::types::QueryResult;

/// Builder for complex hybrid queries
#[derive(Debug, Clone)]
pub struct HybridQueryBuilder {
    pub(crate) vector_query: Option<(Vec<f32>, usize)>,
    pub(crate) graph_pattern: Option<GraphPatternDsl>,
    pub(crate) filters: Vec<Filter>,
    pub(crate) collections: Option<Vec<String>>,
    pub(crate) weights: QueryWeights,
    pub(crate) min_score: f32,
    pub(crate) limit: Option<usize>,
    pub(crate) expand_refs: Option<RefType>,
}

#[derive(Debug, Clone)]
pub struct GraphPatternDsl {
    pub node_label: Option<String>,
    pub node_type: Option<String>,
    pub edge_labels: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct QueryWeights {
    pub vector: f32,
    pub graph: f32,
    pub filter: f32,
}

impl Default for QueryWeights {
    fn default() -> Self {
        Self {
            vector: 0.5,
            graph: 0.3,
            filter: 0.2,
        }
    }
}

impl HybridQueryBuilder {
    pub fn new() -> Self {
        Self {
            vector_query: None,
            graph_pattern: None,
            filters: Vec::new(),
            collections: None,
            weights: QueryWeights::default(),
            min_score: 0.1,
            limit: None,
            expand_refs: None,
        }
    }

    /// Add vector similarity component
    pub fn similar_to(mut self, vector: &[f32], k: usize) -> Self {
        self.vector_query = Some((vector.to_vec(), k));
        self
    }

    /// Add graph pattern component
    pub fn matching_nodes(mut self, label: impl Into<String>) -> Self {
        self.graph_pattern = Some(GraphPatternDsl {
            node_label: Some(label.into()),
            node_type: None,
            edge_labels: Vec::new(),
        });
        self
    }

    /// Limit to collections
    pub fn in_collection(mut self, name: impl Into<String>) -> Self {
        self.collections
            .get_or_insert_with(Vec::new)
            .push(name.into());
        self
    }

    /// Add filter
    pub fn where_(self, field: impl Into<String>) -> WhereClause<Self> {
        WhereClause::new(self, field.into())
    }

    /// Set scoring weights
    pub fn with_weights(mut self, vector: f32, graph: f32, filter: f32) -> Self {
        self.weights = QueryWeights {
            vector,
            graph,
            filter,
        };
        self
    }

    /// Set minimum score threshold
    pub fn min_score(mut self, score: f32) -> Self {
        self.min_score = score;
        self
    }

    /// Limit results
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Expand via cross-references
    pub fn expand_via(mut self, ref_type: RefType) -> Self {
        self.expand_refs = Some(ref_type);
        self
    }

    /// Execute the query
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_hybrid_query(self, store)
    }
}

impl Default for HybridQueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterAcceptor for HybridQueryBuilder {
    fn add_filter(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
}
