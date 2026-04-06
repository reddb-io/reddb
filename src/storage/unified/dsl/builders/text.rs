//! Text search query builder
//!
//! Builder for full-text search queries.

use std::sync::Arc;

use crate::storage::query::unified::ExecutionError;

use super::super::super::store::UnifiedStore;
use super::super::execution::execute_text_query;
use super::super::types::QueryResult;

/// Builder for full-text search queries
#[derive(Debug, Clone)]
pub struct TextSearchBuilder {
    pub(crate) query: String,
    pub(crate) collections: Option<Vec<String>>,
    pub(crate) fields: Option<Vec<String>>,
    pub(crate) limit: Option<usize>,
    pub(crate) fuzzy: bool,
}

impl TextSearchBuilder {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            collections: None,
            fields: None,
            limit: None,
            fuzzy: false,
        }
    }

    /// Search in specific collection(s)
    pub fn in_collection(mut self, name: impl Into<String>) -> Self {
        self.collections
            .get_or_insert_with(Vec::new)
            .push(name.into());
        self
    }

    /// Search specific fields
    pub fn in_field(mut self, field: impl Into<String>) -> Self {
        self.fields.get_or_insert_with(Vec::new).push(field.into());
        self
    }

    /// Enable fuzzy matching
    pub fn fuzzy(mut self) -> Self {
        self.fuzzy = true;
        self
    }

    /// Limit results
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Execute the query
    pub fn execute(self, store: &Arc<UnifiedStore>) -> Result<QueryResult, ExecutionError> {
        execute_text_query(self, store)
    }
}
