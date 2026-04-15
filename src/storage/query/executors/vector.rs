//! Vector Query Executor
//!
//! Executes VECTOR SEARCH queries using HNSW approximate nearest neighbor search.
//! Supports metadata filtering, multiple distance metrics, and cross-references.

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::engine::distance::{distance, DistanceMetric};
use crate::storage::engine::hnsw::{HnswConfig, HnswIndex};
use crate::storage::engine::unified_index::UnifiedIndex;
use crate::storage::engine::vector_metadata::{MetadataFilter, MetadataValue};
use crate::storage::engine::vector_store::VectorStore;
use crate::storage::query::ast::{QueryExpr, VectorQuery, VectorSource};
use crate::storage::query::sql_lowering::effective_vector_filter;
use crate::storage::query::unified::{
    ExecutionError, QueryStats, UnifiedRecord, UnifiedResult, VectorSearchResult,
};
use crate::storage::schema::Value;

/// Vector query executor using HNSW index
pub struct VectorExecutor {
    /// Vector store for segment management
    vector_store: Arc<VectorStore>,
    /// Cross-reference index for linking vectors to nodes/rows
    unified_index: Option<Arc<UnifiedIndex>>,
}

impl VectorExecutor {
    /// Create a new vector executor
    pub fn new(vector_store: Arc<VectorStore>) -> Self {
        Self {
            vector_store,
            unified_index: None,
        }
    }

    /// Create with cross-reference support
    pub fn with_unified_index(mut self, index: Arc<UnifiedIndex>) -> Self {
        self.unified_index = Some(index);
        self
    }

    /// Execute a vector search query
    pub fn execute(&self, query: &VectorQuery) -> Result<UnifiedResult, ExecutionError> {
        let start = std::time::Instant::now();

        // Resolve the query vector
        let query_vector = self.resolve_vector_source(&query.query_vector)?;

        // Get the collection
        let collection = self.vector_store.get(&query.collection).ok_or_else(|| {
            ExecutionError::new(format!("Vector collection not found: {}", query.collection))
        })?;

        // Search the vector store with filter
        let search_results = collection.search_with_filter(
            &query_vector,
            query.k,
            effective_vector_filter(query).as_ref(),
        );

        // Build result
        let mut result = UnifiedResult::with_columns(vec![
            "id".to_string(),
            "distance".to_string(),
            "collection".to_string(),
        ]);

        if query.include_vectors {
            result.columns.push("vector".to_string());
        }
        if query.include_metadata {
            result.columns.push("metadata".to_string());
        }

        // Convert search results to unified records
        for sr in search_results {
            // Apply threshold filter if specified
            if let Some(threshold) = query.threshold {
                if sr.distance > threshold {
                    continue;
                }
            }

            let mut record = UnifiedRecord::new();

            // Build vector search result
            let mut vsr = VectorSearchResult::new(sr.id, &query.collection, sr.distance);

            // Include vector data if requested and available
            if query.include_vectors {
                if let Some(vec_data) = sr.vector {
                    vsr = vsr.with_vector(vec_data);
                }
            }

            // Include metadata if requested and available
            if query.include_metadata {
                if let Some(ref meta_entry) = sr.metadata {
                    // Convert MetadataEntry to HashMap<String, Value>
                    let mut meta_map: HashMap<String, Value> = HashMap::new();
                    for (k, v) in &meta_entry.strings {
                        meta_map.insert(k.clone(), Value::Text(v.clone()));
                    }
                    for (k, v) in &meta_entry.integers {
                        meta_map.insert(k.clone(), Value::Integer(*v));
                    }
                    for (k, v) in &meta_entry.floats {
                        meta_map.insert(k.clone(), Value::Float(*v));
                    }
                    for (k, v) in &meta_entry.bools {
                        meta_map.insert(k.clone(), Value::Boolean(*v));
                    }
                    vsr = vsr.with_metadata(meta_map);
                }
            }

            // Add cross-references if available
            if let Some(ref unified) = self.unified_index {
                // Check for linked node
                if let Some(node_id) = unified.get_vector_node(&query.collection, sr.id) {
                    vsr = vsr.with_linked_node(node_id);
                }

                // Check for linked row
                if let Some(row_key) = unified.get_vector_row(&query.collection, sr.id) {
                    vsr = vsr.with_linked_row(&row_key.table, row_key.row_id);
                }
            }

            // Add basic values to record
            record
                .values
                .insert("id".to_string(), Value::Integer(sr.id as i64));
            record
                .values
                .insert("distance".to_string(), Value::Float(sr.distance as f64));
            record.values.insert(
                "collection".to_string(),
                Value::Text(query.collection.clone()),
            );

            record.vector_results.push(vsr);
            result.push(record);
        }

        // Update stats
        result.stats = QueryStats {
            nodes_scanned: 0,
            edges_scanned: 0,
            rows_scanned: result.len() as u64,
            exec_time_us: start.elapsed().as_micros() as u64,
        };

        Ok(result)
    }

    /// Resolve vector source to actual vector data
    fn resolve_vector_source(&self, source: &VectorSource) -> Result<Vec<f32>, ExecutionError> {
        match source {
            VectorSource::Literal(vec) => Ok(vec.clone()),

            VectorSource::Text(text) => {
                // Text embedding would require an embedding model
                // For now, return an error indicating this needs external embedding
                Err(ExecutionError::new(format!(
                    "Text embedding not yet implemented. Provide a literal vector or use an embedding service for: '{}'",
                    text
                )))
            }

            VectorSource::Reference {
                collection,
                vector_id,
            } => {
                if let Some(coll) = self.vector_store.get(collection) {
                    coll.get(*vector_id).cloned().ok_or_else(|| {
                        ExecutionError::new(format!(
                            "Reference vector not found: {}:{}",
                            collection, vector_id
                        ))
                    })
                } else {
                    Err(ExecutionError::new(format!(
                        "Vector collection not found: {}",
                        collection
                    )))
                }
            }

            VectorSource::Subquery(expr) => self.resolve_subquery_vector(expr.as_ref()),
        }
    }

    fn resolve_subquery_vector(&self, expr: &QueryExpr) -> Result<Vec<f32>, ExecutionError> {
        match expr {
            QueryExpr::Vector(query) => {
                let result = self.execute(query)?;
                let (collection, vector_id) =
                    vector_subquery_reference(&result.records, &query.collection)?;
                self.resolve_vector_source(&VectorSource::Reference {
                    collection,
                    vector_id,
                })
            }
            other => Err(ExecutionError::new(format!(
                "Vector subqueries currently support only nested VECTOR SEARCH expressions, got {}",
                query_expr_name(other)
            ))),
        }
    }
}

/// Convert MetadataValue to Value for unified results
fn metadata_value_to_value(mv: MetadataValue) -> Value {
    match mv {
        MetadataValue::String(s) => Value::Text(s),
        MetadataValue::Integer(i) => Value::Integer(i),
        MetadataValue::Float(f) => Value::Float(f),
        MetadataValue::Bool(b) => Value::Boolean(b),
        MetadataValue::Null => Value::Null,
    }
}

// ============================================================================
// In-Memory Executor for Testing
// ============================================================================

/// Simple in-memory vector executor for testing without full VectorStore
pub struct InMemoryVectorExecutor {
    /// Vectors indexed by (collection, id)
    vectors: HashMap<(String, u64), Vec<f32>>,
    /// Metadata indexed by (collection, id)
    metadata: HashMap<(String, u64), HashMap<String, MetadataValue>>,
    /// HNSW indexes by collection
    indexes: HashMap<String, HnswIndex>,
    /// Cross-reference index
    unified_index: Option<Arc<UnifiedIndex>>,
}

impl InMemoryVectorExecutor {
    /// Create a new in-memory executor
    pub fn new() -> Self {
        Self {
            vectors: HashMap::new(),
            metadata: HashMap::new(),
            indexes: HashMap::new(),
            unified_index: None,
        }
    }

    /// Add cross-reference support
    pub fn with_unified_index(mut self, index: Arc<UnifiedIndex>) -> Self {
        self.unified_index = Some(index);
        self
    }

    /// Add a vector to a collection
    pub fn add_vector(
        &mut self,
        collection: &str,
        id: u64,
        vector: Vec<f32>,
        meta: Option<HashMap<String, MetadataValue>>,
    ) {
        let dim = vector.len();

        // Store vector
        self.vectors
            .insert((collection.to_string(), id), vector.clone());

        // Store metadata
        if let Some(m) = meta {
            self.metadata.insert((collection.to_string(), id), m);
        }

        // Add to HNSW index
        let index = self
            .indexes
            .entry(collection.to_string())
            .or_insert_with(|| {
                let config = HnswConfig {
                    m: 16,
                    m_max0: 32,
                    ef_construction: 200,
                    ef_search: 50,
                    ml: 1.0 / (16.0_f64).ln(),
                    metric: DistanceMetric::L2,
                };
                HnswIndex::new(dim, config)
            });

        index.insert_with_id(id, vector.clone());
    }

    /// Execute a vector query
    pub fn execute(&self, query: &VectorQuery) -> Result<UnifiedResult, ExecutionError> {
        let start = std::time::Instant::now();

        // Resolve query vector
        let query_vector = match &query.query_vector {
            VectorSource::Literal(v) => v.clone(),
            VectorSource::Reference {
                collection,
                vector_id,
            } => self
                .vectors
                .get(&(collection.clone(), *vector_id))
                .cloned()
                .ok_or_else(|| ExecutionError::new("Reference vector not found"))?,
            VectorSource::Text(t) => {
                return Err(ExecutionError::new(format!(
                    "Text embedding not implemented: '{}'",
                    t
                )));
            }
            VectorSource::Subquery(expr) => self.resolve_subquery_vector(expr.as_ref())?,
        };

        let metric = query.metric.unwrap_or(DistanceMetric::L2);

        // Get or create result
        let mut result = UnifiedResult::with_columns(vec![
            "id".to_string(),
            "distance".to_string(),
            "collection".to_string(),
        ]);

        // Search using HNSW if available, otherwise brute force
        let search_results: Vec<(u64, f32)> =
            if let Some(index) = self.indexes.get(&query.collection) {
                // HNSW search returns DistanceResult with id and distance
                let mut results: Vec<_> = index
                    .search(&query_vector, query.k)
                    .into_iter()
                    .map(|r| (r.id, r.distance))
                    .collect();
                results.sort_by(|a, b| {
                    match a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal) {
                        std::cmp::Ordering::Equal => a.0.cmp(&b.0),
                        ordering => ordering,
                    }
                });
                results
            } else {
                // Brute force search
                self.brute_force_search(&query.collection, &query_vector, query.k, metric)
            };

        for (vector_id, dist) in search_results {
            // Apply threshold
            if let Some(threshold) = query.threshold {
                if dist > threshold {
                    continue;
                }
            }

            // Apply metadata filter
            if let Some(ref filter) = query.filter {
                let key = (query.collection.clone(), vector_id);
                if let Some(meta) = self.metadata.get(&key) {
                    if !evaluate_filter(filter, meta) {
                        continue;
                    }
                } else {
                    continue; // No metadata, filter fails
                }
            }

            let mut record = UnifiedRecord::new();
            let mut vsr = VectorSearchResult::new(vector_id, &query.collection, dist);

            if query.include_vectors {
                if let Some(vec) = self.vectors.get(&(query.collection.clone(), vector_id)) {
                    vsr = vsr.with_vector(vec.clone());
                }
            }

            if query.include_metadata {
                if let Some(meta) = self.metadata.get(&(query.collection.clone(), vector_id)) {
                    let meta_map: HashMap<String, Value> = meta
                        .iter()
                        .map(|(k, v)| (k.clone(), metadata_value_to_value(v.clone())))
                        .collect();
                    vsr = vsr.with_metadata(meta_map);
                }
            }

            // Add cross-references
            if let Some(ref unified) = self.unified_index {
                if let Some(node_id) = unified.get_vector_node(&query.collection, vector_id) {
                    vsr = vsr.with_linked_node(node_id);
                }

                if let Some(row_key) = unified.get_vector_row(&query.collection, vector_id) {
                    vsr = vsr.with_linked_row(&row_key.table, row_key.row_id);
                }
            }

            record
                .values
                .insert("id".to_string(), Value::Integer(vector_id as i64));
            record
                .values
                .insert("distance".to_string(), Value::Float(dist as f64));
            record.values.insert(
                "collection".to_string(),
                Value::Text(query.collection.clone()),
            );
            record.vector_results.push(vsr);
            result.push(record);
        }

        result.stats = QueryStats {
            nodes_scanned: 0,
            edges_scanned: 0,
            rows_scanned: self.vectors.len() as u64,
            exec_time_us: start.elapsed().as_micros() as u64,
        };

        Ok(result)
    }

    fn resolve_subquery_vector(&self, expr: &QueryExpr) -> Result<Vec<f32>, ExecutionError> {
        match expr {
            QueryExpr::Vector(query) => {
                let result = self.execute(query)?;
                let (collection, vector_id) =
                    vector_subquery_reference(&result.records, &query.collection)?;
                self.vectors
                    .get(&(collection, vector_id))
                    .cloned()
                    .ok_or_else(|| ExecutionError::new("Subquery reference vector not found"))
            }
            other => Err(ExecutionError::new(format!(
                "Vector subqueries currently support only nested VECTOR SEARCH expressions, got {}",
                query_expr_name(other)
            ))),
        }
    }

    /// Brute force search when no index is available
    fn brute_force_search(
        &self,
        collection: &str,
        query: &[f32],
        k: usize,
        metric: DistanceMetric,
    ) -> Vec<(u64, f32)> {
        let mut results: Vec<(u64, f32)> = self
            .vectors
            .iter()
            .filter(|((c, _), _)| c == collection)
            .map(|((_, id), vec)| {
                let dist = distance(query, vec, metric);
                (*id, dist)
            })
            .collect();

        results.sort_by(
            |a, b| match a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal) {
                std::cmp::Ordering::Equal => a.0.cmp(&b.0),
                ordering => ordering,
            },
        );
        results.truncate(k);
        results
    }
}

impl Default for InMemoryVectorExecutor {
    fn default() -> Self {
        Self::new()
    }
}

/// Evaluate a metadata filter against metadata values
fn evaluate_filter(filter: &MetadataFilter, metadata: &HashMap<String, MetadataValue>) -> bool {
    match filter {
        MetadataFilter::Eq(field, value) => {
            metadata.get(field).map(|v| v == value).unwrap_or(false)
        }
        MetadataFilter::Ne(field, value) => metadata.get(field).map(|v| v != value).unwrap_or(true),
        MetadataFilter::Lt(field, value) => {
            compare_values(metadata.get(field), value, |a, b| a < b)
        }
        MetadataFilter::Lte(field, value) => {
            compare_values(metadata.get(field), value, |a, b| a <= b)
        }
        MetadataFilter::Gt(field, value) => {
            compare_values(metadata.get(field), value, |a, b| a > b)
        }
        MetadataFilter::Gte(field, value) => {
            compare_values(metadata.get(field), value, |a, b| a >= b)
        }
        MetadataFilter::In(field, values) => metadata
            .get(field)
            .map(|v| values.contains(v))
            .unwrap_or(false),
        MetadataFilter::NotIn(field, values) => metadata
            .get(field)
            .map(|v| !values.contains(v))
            .unwrap_or(true),
        MetadataFilter::Contains(field, substring) => {
            if let Some(MetadataValue::String(s)) = metadata.get(field) {
                s.contains(substring)
            } else {
                false
            }
        }
        MetadataFilter::And(filters) => filters.iter().all(|f| evaluate_filter(f, metadata)),
        MetadataFilter::Or(filters) => filters.iter().any(|f| evaluate_filter(f, metadata)),
        MetadataFilter::Not(inner) => !evaluate_filter(inner, metadata),
        MetadataFilter::StartsWith(field, prefix) => {
            if let Some(MetadataValue::String(s)) = metadata.get(field) {
                s.starts_with(prefix)
            } else {
                false
            }
        }
        MetadataFilter::EndsWith(field, suffix) => {
            if let Some(MetadataValue::String(s)) = metadata.get(field) {
                s.ends_with(suffix)
            } else {
                false
            }
        }
        MetadataFilter::Exists(field) => metadata.contains_key(field),
        MetadataFilter::NotExists(field) => !metadata.contains_key(field),
    }
}

/// Compare metadata values with a comparison function
fn compare_values<F>(actual: Option<&MetadataValue>, expected: &MetadataValue, cmp: F) -> bool
where
    F: Fn(f64, f64) -> bool,
{
    match (actual, expected) {
        (Some(MetadataValue::Integer(a)), MetadataValue::Integer(b)) => cmp(*a as f64, *b as f64),
        (Some(MetadataValue::Float(a)), MetadataValue::Float(b)) => cmp(*a, *b),
        (Some(MetadataValue::Integer(a)), MetadataValue::Float(b)) => cmp(*a as f64, *b),
        (Some(MetadataValue::Float(a)), MetadataValue::Integer(b)) => cmp(*a, *b as f64),
        _ => false,
    }
}

fn vector_subquery_reference(
    records: &[UnifiedRecord],
    default_collection: &str,
) -> Result<(String, u64), ExecutionError> {
    let record = records
        .first()
        .ok_or_else(|| ExecutionError::new("Vector subquery returned no rows"))?;

    let collection = match record.values.get("collection") {
        Some(Value::Text(collection)) => collection.clone(),
        _ => default_collection.to_string(),
    };

    let vector_id = match record.values.get("id") {
        Some(Value::Integer(id)) if *id >= 0 => *id as u64,
        Some(Value::UnsignedInteger(id)) => *id,
        other => {
            return Err(ExecutionError::new(format!(
                "Vector subquery must expose an integer id column, got {other:?}"
            )));
        }
    };

    Ok((collection, vector_id))
}

fn query_expr_name(expr: &QueryExpr) -> &'static str {
    match expr {
        QueryExpr::Table(_) => "table",
        QueryExpr::Graph(_) => "graph",
        QueryExpr::Join(_) => "join",
        QueryExpr::Path(_) => "path",
        QueryExpr::Vector(_) => "vector",
        QueryExpr::Hybrid(_) => "hybrid",
        QueryExpr::Insert(_) => "insert",
        QueryExpr::Update(_) => "update",
        QueryExpr::Delete(_) => "delete",
        QueryExpr::CreateTable(_) => "create_table",
        QueryExpr::DropTable(_) => "drop_table",
        QueryExpr::AlterTable(_) => "alter_table",
        QueryExpr::GraphCommand(_) => "graph_command",
        QueryExpr::SearchCommand(_) => "search_command",
        QueryExpr::Ask(_) => "ask",
        QueryExpr::CreateIndex(_) => "create_index",
        QueryExpr::DropIndex(_) => "drop_index",
        QueryExpr::ProbabilisticCommand(_) => "probabilistic_command",
        QueryExpr::CreateTimeSeries(_) => "create_timeseries",
        QueryExpr::DropTimeSeries(_) => "drop_timeseries",
        QueryExpr::CreateQueue(_) => "create_queue",
        QueryExpr::DropQueue(_) => "drop_queue",
        QueryExpr::QueueCommand(_) => "queue_command",
        QueryExpr::SetConfig { .. } => "set_config",
        QueryExpr::ShowConfig { .. } => "show_config",
        QueryExpr::ExplainAlter(_) => "explain_alter",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_vector_search() {
        let mut executor = InMemoryVectorExecutor::new();

        // Add some vectors
        executor.add_vector("test", 1, vec![1.0, 0.0, 0.0], None);
        executor.add_vector("test", 2, vec![0.0, 1.0, 0.0], None);
        executor.add_vector("test", 3, vec![0.0, 0.0, 1.0], None);
        executor.add_vector("test", 4, vec![0.9, 0.1, 0.0], None);

        let query = VectorQuery {
            alias: None,
            collection: "test".to_string(),
            query_vector: VectorSource::Literal(vec![1.0, 0.0, 0.0]),
            k: 2,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: false,
            threshold: None,
        };

        let result = executor.execute(&query).unwrap();
        assert_eq!(result.len(), 2);

        // First result should be vector 1 (exact match)
        let first = &result.records[0];
        assert_eq!(first.values.get("id"), Some(&Value::Integer(1)));
    }

    #[test]
    fn test_vector_search_with_metadata_filter() {
        let mut executor = InMemoryVectorExecutor::new();

        let mut meta1 = HashMap::new();
        meta1.insert("type".to_string(), MetadataValue::String("cve".to_string()));
        meta1.insert("severity".to_string(), MetadataValue::Integer(9));

        let mut meta2 = HashMap::new();
        meta2.insert("type".to_string(), MetadataValue::String("cve".to_string()));
        meta2.insert("severity".to_string(), MetadataValue::Integer(5));

        let mut meta3 = HashMap::new();
        meta3.insert(
            "type".to_string(),
            MetadataValue::String("advisory".to_string()),
        );
        meta3.insert("severity".to_string(), MetadataValue::Integer(8));

        executor.add_vector("vulns", 1, vec![1.0, 0.0], Some(meta1));
        executor.add_vector("vulns", 2, vec![0.9, 0.1], Some(meta2));
        executor.add_vector("vulns", 3, vec![0.8, 0.2], Some(meta3));

        // Search with filter: type = 'cve' AND severity >= 7
        let query = VectorQuery {
            alias: None,
            collection: "vulns".to_string(),
            query_vector: VectorSource::Literal(vec![1.0, 0.0]),
            k: 10,
            filter: Some(MetadataFilter::And(vec![
                MetadataFilter::Eq("type".to_string(), MetadataValue::String("cve".to_string())),
                MetadataFilter::Gte("severity".to_string(), MetadataValue::Integer(7)),
            ])),
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: true,
            threshold: None,
        };

        let result = executor.execute(&query).unwrap();

        // Only vector 1 matches (type=cve, severity=9)
        assert_eq!(result.len(), 1);
        assert_eq!(result.records[0].values.get("id"), Some(&Value::Integer(1)));
    }

    #[test]
    fn test_vector_search_with_threshold() {
        let mut executor = InMemoryVectorExecutor::new();

        executor.add_vector("test", 1, vec![1.0, 0.0], None);
        executor.add_vector("test", 2, vec![0.0, 1.0], None); // Far from query

        let query = VectorQuery {
            alias: None,
            collection: "test".to_string(),
            query_vector: VectorSource::Literal(vec![1.0, 0.0]),
            k: 10,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: false,
            threshold: Some(0.5), // Only include close matches
        };

        let result = executor.execute(&query).unwrap();

        // Only vector 1 is within threshold
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_vector_search_include_vectors() {
        let mut executor = InMemoryVectorExecutor::new();

        executor.add_vector("test", 1, vec![1.0, 2.0, 3.0], None);

        let query = VectorQuery {
            alias: None,
            collection: "test".to_string(),
            query_vector: VectorSource::Literal(vec![1.0, 2.0, 3.0]),
            k: 1,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: true,
            include_metadata: false,
            threshold: None,
        };

        let result = executor.execute(&query).unwrap();
        assert_eq!(result.len(), 1);

        let vsr = &result.records[0].vector_results[0];
        assert!(vsr.vector.is_some());
        assert_eq!(vsr.vector.as_ref().unwrap(), &vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn test_vector_executor_reference_source() {
        let mut store = VectorStore::new();
        let collection = store.create_collection("refs", 2);
        let ref_id = collection.insert(vec![1.0, 0.0], None).unwrap();
        collection.insert(vec![0.0, 1.0], None).unwrap();

        let executor = VectorExecutor::new(Arc::new(store));
        let query = VectorQuery {
            alias: None,
            collection: "refs".to_string(),
            query_vector: VectorSource::Reference {
                collection: "refs".to_string(),
                vector_id: ref_id,
            },
            k: 1,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: false,
            threshold: None,
        };

        let result = executor.execute(&query).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.records[0].values.get("id"), Some(&Value::Integer(0)));
    }

    #[test]
    fn test_vector_executor_subquery_source() {
        let mut store = VectorStore::new();
        let collection = store.create_collection("refs", 2);
        collection.insert(vec![1.0, 0.0], None).unwrap();
        collection.insert(vec![0.0, 1.0], None).unwrap();

        let executor = VectorExecutor::new(Arc::new(store));
        let inner = VectorQuery {
            alias: None,
            collection: "refs".to_string(),
            query_vector: VectorSource::Literal(vec![1.0, 0.0]),
            k: 1,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: false,
            threshold: None,
        };
        let query = VectorQuery {
            alias: None,
            collection: "refs".to_string(),
            query_vector: VectorSource::Subquery(Box::new(QueryExpr::Vector(inner))),
            k: 1,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: false,
            threshold: None,
        };

        let result = executor.execute(&query).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result.records[0].values.get("id"), Some(&Value::Integer(0)));
    }
}
