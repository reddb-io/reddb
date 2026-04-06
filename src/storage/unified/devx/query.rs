//! Query Builder and Query Types
//!
//! Fluent query API for multi-modal queries across tables, graphs, and vectors.

use std::collections::HashMap;
use std::sync::Arc;

use super::super::{EntityData, EntityId, EntityKind, MetadataValue, UnifiedEntity, UnifiedStore};
use super::error::DevXError;
use super::helpers::cosine_similarity;
use crate::storage::schema::Value;

// ============================================================================
// Query Builder
// ============================================================================

/// Fluent query builder for multi-modal queries
pub struct QueryBuilder {
    store: Arc<UnifiedStore>,
    collections: Option<Vec<String>>,
    vector_query: Option<Vec<f32>>,
    similarity_threshold: f32,
    property_filters: Vec<(String, PropertyFilter)>,
    metadata_filters: Vec<(String, MetadataFilter)>,
    expand_edges: Vec<(String, u32)>, // (edge_label, depth)
    limit: usize,
    offset: usize,
}

impl QueryBuilder {
    pub(crate) fn new(store: Arc<UnifiedStore>) -> Self {
        Self {
            store,
            collections: None,
            vector_query: None,
            similarity_threshold: 0.7,
            property_filters: Vec::new(),
            metadata_filters: Vec::new(),
            expand_edges: Vec::new(),
            limit: 100,
            offset: 0,
        }
    }

    /// Search in specific collections
    pub fn collection(mut self, name: impl Into<String>) -> Self {
        self.collections.get_or_insert(Vec::new()).push(name.into());
        self
    }

    /// Search in multiple collections
    pub fn collections(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let cols = self.collections.get_or_insert(Vec::new());
        for name in names {
            cols.push(name.into());
        }
        self
    }

    /// Vector similarity search
    pub fn similar_to(mut self, vector: Vec<f32>, threshold: f32) -> Self {
        self.vector_query = Some(vector);
        self.similarity_threshold = threshold;
        self
    }

    /// Filter by property value (equality)
    pub fn where_prop(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.property_filters
            .push((key.into(), PropertyFilter::Eq(value.into())));
        self
    }

    /// Filter by property (greater than)
    pub fn where_prop_gt(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.property_filters
            .push((key.into(), PropertyFilter::Gt(value.into())));
        self
    }

    /// Filter by property (less than)
    pub fn where_prop_lt(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.property_filters
            .push((key.into(), PropertyFilter::Lt(value.into())));
        self
    }

    /// Filter by property (contains text)
    pub fn where_prop_contains(
        mut self,
        key: impl Into<String>,
        substr: impl Into<String>,
    ) -> Self {
        self.property_filters
            .push((key.into(), PropertyFilter::Contains(substr.into())));
        self
    }

    /// Filter by metadata
    pub fn where_meta(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata_filters
            .push((key.into(), MetadataFilter::Eq(value.into())));
        self
    }

    /// Expand through edges
    pub fn expand(mut self, edge_label: impl Into<String>, depth: u32) -> Self {
        self.expand_edges.push((edge_label.into(), depth));
        self
    }

    /// Set result limit
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Set offset for pagination
    pub fn offset(mut self, offset: usize) -> Self {
        self.offset = offset;
        self
    }

    /// Execute the query
    pub fn execute(mut self) -> Result<QueryResult, DevXError> {
        let mut results = Vec::new();

        let collections = self
            .collections
            .take()
            .unwrap_or_else(|| self.store.list_collections());

        for col_name in &collections {
            let manager = match self.store.get_collection(col_name) {
                Some(m) => m,
                None => continue,
            };

            let entities = manager.query_all(|_| true);

            for entity in entities {
                let mut score = 0.0f32;
                let mut include = true;

                // Vector similarity
                if let Some(ref query_vec) = self.vector_query {
                    let sim = match &entity.data {
                        EntityData::Vector(v) => cosine_similarity(query_vec, &v.dense),
                        _ => entity
                            .embeddings
                            .iter()
                            .map(|e| cosine_similarity(query_vec, &e.vector))
                            .fold(0.0f32, f32::max),
                    };

                    if sim < self.similarity_threshold {
                        include = false;
                    } else {
                        score = sim;
                    }
                }

                // Property filters
                if include {
                    let props = self.extract_properties(&entity);
                    for (key, filter) in &self.property_filters {
                        if !filter.matches(props.get(key)) {
                            include = false;
                            break;
                        }
                    }
                }

                if include {
                    results.push(QueryResultItem {
                        entity,
                        collection: col_name.clone(),
                        score,
                        expanded: Vec::new(),
                    });
                }
            }
        }

        // Sort by score
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Expand edges
        if !self.expand_edges.is_empty() {
            for item in &mut results {
                for (edge_label, depth) in &self.expand_edges {
                    let expanded = self.expand_entity(item.entity.id, edge_label, *depth);
                    item.expanded.extend(expanded);
                }
            }
        }

        // Apply pagination
        let total = results.len();
        let results: Vec<_> = results
            .into_iter()
            .skip(self.offset)
            .take(self.limit)
            .collect();

        Ok(QueryResult {
            items: results,
            total,
            offset: self.offset,
            limit: self.limit,
        })
    }

    fn extract_properties(&self, entity: &UnifiedEntity) -> HashMap<String, Value> {
        match &entity.data {
            EntityData::Node(n) => n.properties.clone(),
            EntityData::Edge(e) => e.properties.clone(),
            EntityData::Row(r) => r.named.clone().unwrap_or_default(),
            EntityData::Vector(_) => HashMap::new(),
        }
    }

    fn expand_entity(&self, id: EntityId, edge_label: &str, depth: u32) -> Vec<ExpandedEntity> {
        if depth == 0 {
            return Vec::new();
        }

        let mut expanded = Vec::new();
        let refs = self.store.get_refs_from(id);

        for (target_id, _ref_type, collection) in refs {
            if let Some(entity) = self.store.get(&collection, target_id) {
                // Check if edge matches label
                let matches = match &entity.kind {
                    EntityKind::GraphEdge { label, .. } => label == edge_label,
                    _ => true, // Include non-edge entities
                };

                if matches {
                    expanded.push(ExpandedEntity {
                        entity,
                        collection,
                        depth,
                    });

                    // Recursively expand
                    if depth > 1 {
                        let sub_expanded = self.expand_entity(target_id, edge_label, depth - 1);
                        expanded.extend(sub_expanded);
                    }
                }
            }
        }

        expanded
    }
}

// ============================================================================
// Filter Types
// ============================================================================

/// Property filter operations
#[derive(Debug, Clone)]
pub enum PropertyFilter {
    Eq(Value),
    Gt(Value),
    Lt(Value),
    Contains(String),
}

impl PropertyFilter {
    pub fn matches(&self, value: Option<&Value>) -> bool {
        match (self, value) {
            (PropertyFilter::Eq(expected), Some(actual)) => expected == actual,
            (PropertyFilter::Contains(substr), Some(Value::Text(s))) => s.contains(substr),
            (PropertyFilter::Gt(expected), Some(actual)) => match (expected, actual) {
                (Value::Integer(e), Value::Integer(a)) => a > e,
                (Value::Float(e), Value::Float(a)) => a > e,
                _ => false,
            },
            (PropertyFilter::Lt(expected), Some(actual)) => match (expected, actual) {
                (Value::Integer(e), Value::Integer(a)) => a < e,
                (Value::Float(e), Value::Float(a)) => a < e,
                _ => false,
            },
            _ => false,
        }
    }
}

/// Metadata filter operations
#[derive(Debug, Clone)]
pub enum MetadataFilter {
    Eq(MetadataValue),
}

// ============================================================================
// Result Types
// ============================================================================

/// Query result container
#[derive(Debug)]
pub struct QueryResult {
    pub items: Vec<QueryResultItem>,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
}

impl QueryResult {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

/// Single item in query results
#[derive(Debug)]
pub struct QueryResultItem {
    pub entity: UnifiedEntity,
    pub collection: String,
    pub score: f32,
    pub expanded: Vec<ExpandedEntity>,
}

/// Expanded entity from graph traversal
#[derive(Debug)]
pub struct ExpandedEntity {
    pub entity: UnifiedEntity,
    pub collection: String,
    pub depth: u32,
}
