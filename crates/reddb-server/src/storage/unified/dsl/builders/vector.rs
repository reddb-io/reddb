//! Vector similarity query builder
//!
//! Builder for semantic/vector similarity searches.

use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;

use super::super::super::entity::RefType;
use super::super::super::store::UnifiedStore;
use super::super::execution::execute_vector_query;
use super::super::filters::{Filter, FilterAcceptor, WhereClause};
use super::super::types::QueryResult;

/// Builder for vector similarity queries
#[derive(Debug, Clone)]
pub struct VectorQueryBuilder {
    pub(crate) vector: Vec<f32>,
    pub(crate) k: usize,
    pub(crate) collections: Option<Vec<String>>,
    pub(crate) filters: Vec<Filter>,
    pub(crate) min_similarity: f32,
    pub(crate) include_embeddings: bool,
    pub(crate) embedding_slot: Option<String>,
    pub(crate) expand_refs: Option<RefType>,
    pub(crate) expand_depth: u32,
}

impl VectorQueryBuilder {
    pub fn new(vector: Vec<f32>, k: usize) -> Self {
        Self {
            vector,
            k,
            collections: None,
            filters: Vec::new(),
            min_similarity: 0.0,
            include_embeddings: true,
            embedding_slot: None,
            expand_refs: None,
            expand_depth: 1,
        }
    }

    /// Limit search to specific collection(s)
    pub fn in_collection(mut self, name: impl Into<String>) -> Self {
        self.collections
            .get_or_insert_with(Vec::new)
            .push(name.into());
        self
    }

    /// Limit search to multiple collections
    pub fn in_collections(mut self, names: &[&str]) -> Self {
        let cols = self.collections.get_or_insert_with(Vec::new);
        for name in names {
            cols.push((*name).to_string());
        }
        self
    }

    /// Add a filter condition (returns WhereClause for chaining)
    pub fn where_(self, field: impl Into<String>) -> WhereClause<Self> {
        WhereClause::new(self, field.into())
    }

    /// Set minimum similarity threshold
    pub fn min_similarity(mut self, threshold: f32) -> Self {
        self.min_similarity = threshold;
        self
    }

    /// Search in a specific embedding slot
    pub fn in_slot(mut self, slot: impl Into<String>) -> Self {
        self.embedding_slot = Some(slot.into());
        self
    }

    /// Skip entity embeddings (only search dedicated vector entities)
    pub fn vectors_only(mut self) -> Self {
        self.include_embeddings = false;
        self
    }

    /// Expand results by following cross-references
    pub fn expand_via(mut self, ref_type: RefType) -> Self {
        self.expand_refs = Some(ref_type);
        self
    }

    /// Set expansion depth
    pub fn depth(mut self, depth: u32) -> Self {
        self.expand_depth = depth;
        self
    }

    /// Execute the query
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_vector_query(self, store)
    }
}

impl FilterAcceptor for VectorQueryBuilder {
    fn add_filter(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
}
