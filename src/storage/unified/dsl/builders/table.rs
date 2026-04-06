//! Table and scan query builders
//!
//! Builders for table/collection queries and full scans.

use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;

use super::super::super::store::UnifiedStore;
use super::super::execution::{execute_scan_query, execute_table_query};
use super::super::filters::{Filter, FilterAcceptor, WhereClause};
use super::super::types::QueryResult;

/// Builder for table/collection queries
#[derive(Debug, Clone)]
pub struct TableQueryBuilder {
    pub(crate) collection: String,
    pub(crate) filters: Vec<Filter>,
    pub(crate) order_by: Option<(String, SortOrder)>,
    pub(crate) limit: Option<usize>,
    pub(crate) offset: usize,
    pub(crate) with_embeddings: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum SortOrder {
    Asc,
    Desc,
}

impl TableQueryBuilder {
    pub fn new(collection: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            filters: Vec::new(),
            order_by: None,
            limit: None,
            offset: 0,
            with_embeddings: false,
        }
    }

    /// Add a filter condition
    pub fn where_(self, field: impl Into<String>) -> WhereClause<Self> {
        WhereClause::new(self, field.into())
    }

    /// Order results by field
    pub fn order_by(mut self, field: impl Into<String>, order: SortOrder) -> Self {
        self.order_by = Some((field.into(), order));
        self
    }

    /// Shorthand for ascending order
    pub fn order_by_asc(self, field: impl Into<String>) -> Self {
        self.order_by(field, SortOrder::Asc)
    }

    /// Shorthand for descending order
    pub fn order_by_desc(self, field: impl Into<String>) -> Self {
        self.order_by(field, SortOrder::Desc)
    }

    /// Limit results
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Skip first N results
    pub fn offset(mut self, n: usize) -> Self {
        self.offset = n;
        self
    }

    /// Include embeddings in results
    pub fn with_embeddings(mut self) -> Self {
        self.with_embeddings = true;
        self
    }

    /// Execute the query
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_table_query(self, store)
    }
}

impl FilterAcceptor for TableQueryBuilder {
    fn add_filter(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
}

/// Builder for full collection scans
#[derive(Debug, Clone)]
pub struct ScanQueryBuilder {
    pub(crate) collection: String,
    pub(crate) filters: Vec<Filter>,
    pub(crate) limit: Option<usize>,
}

impl ScanQueryBuilder {
    pub fn new(collection: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            filters: Vec::new(),
            limit: None,
        }
    }

    /// Add a filter condition
    pub fn where_(self, field: impl Into<String>) -> WhereClause<Self> {
        WhereClause::new(self, field.into())
    }

    /// Limit results
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Execute the query
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_scan_query(self, store)
    }
}

impl FilterAcceptor for ScanQueryBuilder {
    fn add_filter(&mut self, filter: Filter) {
        self.filters.push(filter);
    }
}
