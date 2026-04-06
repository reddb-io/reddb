//! Reference query builder
//!
//! Builder for cross-reference traversal queries.

use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;

use super::super::super::entity::{EntityId, RefType};
use super::super::super::store::UnifiedStore;
use super::super::execution::execute_ref_query;
use super::super::filters::{Filter, FilterAcceptor, WhereClause};
use super::super::types::QueryResult;

/// Builder for cross-reference traversal
#[derive(Debug, Clone)]
pub struct RefQueryBuilder {
    pub(crate) source_id: EntityId,
    pub(crate) ref_type: RefType,
    pub(crate) max_depth: u32,
    pub(crate) filters: Vec<Filter>,
    pub(crate) include_source: bool,
}

impl RefQueryBuilder {
    pub fn new(id: EntityId, ref_type: RefType) -> Self {
        Self {
            source_id: id,
            ref_type,
            max_depth: 3,
            filters: Vec::new(),
            include_source: false,
        }
    }

    /// Set maximum depth
    pub fn depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    /// Include the source entity in results
    pub fn include_source(mut self) -> Self {
        self.include_source = true;
        self
    }

    /// Add a filter condition
    pub fn where_(self, field: impl Into<String>) -> WhereClause<Self> {
        WhereClause::new(self, field.into())
    }

    /// Execute the query
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_ref_query(self, store)
    }
}

impl FilterAcceptor for RefQueryBuilder {
    fn add_filter(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
}
