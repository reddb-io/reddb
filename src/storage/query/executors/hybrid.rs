//! Hybrid Query Executor
//!
//! Executes HYBRID queries that combine structured (SQL/Graph) queries with
//! vector similarity search, using various fusion strategies to merge results.
//!
//! # Fusion Strategies
//!
//! - **Rerank**: Re-ranks structured results by vector similarity
//! - **FilterThenSearch**: Filters first, then searches vectors
//! - **SearchThenFilter**: Searches vectors first, then applies structured filter
//! - **RRF (Reciprocal Rank Fusion)**: Combines rankings fairly
//! - **Intersection**: Only returns results matching both queries
//! - **Union**: Returns results from either query with combined scores

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::thread;

use crate::storage::engine::graph_store::GraphStore;
use crate::storage::engine::graph_table_index::GraphTableIndex;
use crate::storage::engine::unified_index::UnifiedIndex;
use crate::storage::engine::vector_store::VectorStore;
use crate::storage::query::ast::{FusionStrategy, HybridQuery, VectorQuery};
use crate::storage::query::unified::{
    ExecutionError, QueryStats, UnifiedExecutor, UnifiedRecord, UnifiedResult,
};
use crate::storage::schema::Value;

use super::vector::VectorExecutor;

/// Hybrid query executor that combines structured and vector results
pub struct HybridExecutor {
    /// Structured query executor
    unified: UnifiedExecutor,
    /// Vector search executor
    vector: VectorExecutor,
    /// Cross-reference index for linking results
    unified_index: Option<Arc<UnifiedIndex>>,
}

impl HybridExecutor {
    /// Create a new hybrid executor
    pub fn new(
        graph: Arc<GraphStore>,
        index: Arc<GraphTableIndex>,
        vector_store: Arc<VectorStore>,
    ) -> Self {
        let unified = UnifiedExecutor::new(Arc::clone(&graph), Arc::clone(&index));
        let vector = VectorExecutor::new(vector_store);

        Self {
            unified,
            vector,
            unified_index: None,
        }
    }

    /// Add cross-reference support
    pub fn with_unified_index(mut self, index: Arc<UnifiedIndex>) -> Self {
        self.unified_index = Some(Arc::clone(&index));
        self.vector = self.vector.with_unified_index(index);
        self
    }

    /// Execute a hybrid query
    pub fn execute(&self, query: &HybridQuery) -> Result<UnifiedResult, ExecutionError> {
        let start = std::time::Instant::now();

        // Execute based on fusion strategy
        let mut result = match &query.fusion {
            FusionStrategy::Rerank { weight } => self.execute_rerank(query, *weight)?,
            FusionStrategy::FilterThenSearch => self.execute_filter_then_search(query)?,
            FusionStrategy::SearchThenFilter => self.execute_search_then_filter(query)?,
            FusionStrategy::RRF { k } => self.execute_rrf(query, *k)?,
            FusionStrategy::Intersection => self.execute_intersection(query)?,
            FusionStrategy::Union {
                structured_weight,
                vector_weight,
            } => self.execute_union(query, *structured_weight, *vector_weight)?,
        };

        // Apply limit if specified
        if let Some(limit) = query.limit {
            result.records.truncate(limit);
        }

        // Update stats
        result.stats.exec_time_us = start.elapsed().as_micros() as u64;

        Ok(result)
    }

    // =========================================================================
    // Fusion Strategies
    // =========================================================================

    /// Rerank: Execute structured query, then re-rank by vector similarity
    fn execute_rerank(
        &self,
        query: &HybridQuery,
        weight: f32,
    ) -> Result<UnifiedResult, ExecutionError> {
        // 1. Execute structured query
        let structured_result = self.unified.execute(&query.structured)?;

        if structured_result.is_empty() {
            return Ok(structured_result);
        }

        // 2. Execute vector query
        let vector_result = self.vector.execute(&query.vector)?;

        // 3. Build vector distance lookup
        let mut vector_distances: HashMap<String, f32> = HashMap::new();
        for record in &vector_result.records {
            for vsr in &record.vector_results {
                // Use vector ID as key
                let key = format!("{}:{}", vsr.collection, vsr.id);
                vector_distances.insert(key, vsr.distance);
            }
        }

        // 4. Score and rerank structured results
        let mut scored: Vec<(String, UnifiedRecord, f32)> = structured_result
            .records
            .into_iter()
            .enumerate()
            .map(|(rank, record)| {
                // Structured score: inverse rank (higher = better)
                let struct_score = 1.0 / (rank as f32 + 1.0);

                // Vector score: try to find matching vector via cross-reference
                let vector_score = self.get_vector_score_for_record(&record, &vector_distances);

                // Combined score
                let combined = (1.0 - weight) * struct_score + weight * vector_score;
                (self.record_to_key(&record), record, combined)
            })
            .collect();

        // Sort by combined score (descending), then deterministic key
        scored.sort_by(
            |a, b| match b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal) {
                std::cmp::Ordering::Equal => a.0.cmp(&b.0),
                ordering => ordering,
            },
        );

        // Build result
        let mut result = UnifiedResult::with_columns(structured_result.columns);
        result.stats = structured_result.stats;

        for (_key, mut record, score) in scored {
            record
                .values
                .insert("_hybrid_score".to_string(), Value::Float(score as f64));
            result.push(record);
        }

        Ok(result)
    }

    /// FilterThenSearch: Use structured results to filter vector search space
    fn execute_filter_then_search(
        &self,
        query: &HybridQuery,
    ) -> Result<UnifiedResult, ExecutionError> {
        // 1. Execute structured query to get filter candidates
        let structured_result = self.unified.execute(&query.structured)?;

        if structured_result.is_empty() {
            return Ok(structured_result);
        }

        // 2. Extract IDs from structured results for filtering
        let candidate_ids: HashSet<u64> = structured_result
            .records
            .iter()
            .filter_map(|r| {
                // Try to get ID from values
                r.values.get("id").and_then(|v| match v {
                    Value::Integer(i) => Some(*i as u64),
                    _ => None,
                })
            })
            .collect();

        // 3. Execute vector query
        let vector_result = self.vector.execute(&query.vector)?;

        // 4. Filter vector results to only include structured candidates
        let mut result = UnifiedResult::with_columns(vector_result.columns.clone());

        for record in vector_result.records {
            // Check if this vector result matches any structured candidate
            let matches = record.vector_results.iter().any(|vsr| {
                candidate_ids.contains(&vsr.id) ||
                // Also check linked row if available
                vsr.linked_row.as_ref().map(|(_, row_id)| candidate_ids.contains(row_id)).unwrap_or(false)
            });

            if matches {
                result.push(record);
            }
        }

        result.stats = QueryStats::merge(&structured_result.stats, &vector_result.stats);
        Ok(result)
    }

    /// SearchThenFilter: Vector search first, then apply structured filter
    fn execute_search_then_filter(
        &self,
        query: &HybridQuery,
    ) -> Result<UnifiedResult, ExecutionError> {
        // 1. Execute vector query first
        let vector_result = self.vector.execute(&query.vector)?;

        if vector_result.is_empty() {
            return Ok(vector_result);
        }

        // 2. Execute structured query
        let structured_result = self.unified.execute(&query.structured)?;

        // 3. Extract IDs from structured results
        let structured_ids: HashSet<u64> = structured_result
            .records
            .iter()
            .filter_map(|r| {
                r.values.get("id").and_then(|v| match v {
                    Value::Integer(i) => Some(*i as u64),
                    _ => None,
                })
            })
            .collect();

        // 4. Filter vector results to match structured query
        let mut result = UnifiedResult::with_columns(vector_result.columns.clone());

        for record in vector_result.records {
            let matches = record.vector_results.iter().any(|vsr| {
                structured_ids.contains(&vsr.id)
                    || vsr
                        .linked_row
                        .as_ref()
                        .map(|(_, row_id)| structured_ids.contains(row_id))
                        .unwrap_or(false)
            });

            if matches {
                result.push(record);
            }
        }

        result.stats = QueryStats::merge(&vector_result.stats, &structured_result.stats);
        Ok(result)
    }

    /// RRF: Reciprocal Rank Fusion
    /// Combines rankings using: RRF(d) = Σ(1 / (k + rank(d)))
    /// Execute structured and vector arms concurrently via
    /// [`std::thread::scope`].
    ///
    /// Used by fusion strategies that always run both arms to completion
    /// (RRF, Intersection, Union). Short-circuiting strategies (Rerank,
    /// FilterThenSearch, SearchThenFilter) keep serial execution because
    /// they check for early-exit conditions on the first arm before
    /// deciding whether to run the second.
    ///
    /// Worst-case total latency collapses from `structured + vector` to
    /// `max(structured, vector)` — the planner's pessimistic estimate for
    /// hybrid queries is now tight when both arms dominate.
    fn execute_structured_and_vector_parallel(
        &self,
        query: &HybridQuery,
    ) -> Result<(UnifiedResult, UnifiedResult), ExecutionError> {
        thread::scope(|s| {
            let structured_handle = s.spawn(|| self.unified.execute(&query.structured));
            let vector_handle = s.spawn(|| self.vector.execute(&query.vector));

            // `join` returns `Result<T, Box<dyn Any + Send>>`; a panic in
            // either arm is surfaced as an `ExecutionError` so callers
            // don't see a raw thread panic.
            let structured = structured_handle
                .join()
                .map_err(|_| ExecutionError::new("hybrid: structured arm panicked"))??;
            let vector = vector_handle
                .join()
                .map_err(|_| ExecutionError::new("hybrid: vector arm panicked"))??;
            Ok((structured, vector))
        })
    }

    fn execute_rrf(&self, query: &HybridQuery, k: u32) -> Result<UnifiedResult, ExecutionError> {
        // 1. Execute both queries in parallel — RRF always consumes both.
        let (structured_result, vector_result) =
            self.execute_structured_and_vector_parallel(query)?;

        // 2. Build rank maps (lower rank = better, starting from 1)
        let mut structured_ranks: HashMap<String, u32> = HashMap::new();
        for (rank, record) in structured_result.records.iter().enumerate() {
            let key = self.record_to_key(record);
            structured_ranks.insert(key, (rank + 1) as u32);
        }

        let mut vector_ranks: HashMap<String, u32> = HashMap::new();
        for (rank, record) in vector_result.records.iter().enumerate() {
            let key = self.record_to_key(record);
            vector_ranks.insert(key, (rank + 1) as u32);
        }

        // 3. Calculate RRF scores for all unique records
        let all_keys: HashSet<_> = structured_ranks
            .keys()
            .chain(vector_ranks.keys())
            .cloned()
            .collect();

        let k_f64 = k as f64;
        let mut rrf_scores: Vec<(String, f64)> = all_keys
            .into_iter()
            .map(|key| {
                let struct_contrib = structured_ranks
                    .get(&key)
                    .map(|r| 1.0 / (k_f64 + *r as f64))
                    .unwrap_or(0.0);
                let vector_contrib = vector_ranks
                    .get(&key)
                    .map(|r| 1.0 / (k_f64 + *r as f64))
                    .unwrap_or(0.0);
                (key, struct_contrib + vector_contrib)
            })
            .collect();

        // Sort by RRF score (descending)
        rrf_scores.sort_by(|a, b| {
            match b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal) {
                std::cmp::Ordering::Equal => a.0.cmp(&b.0),
                ordering => ordering,
            }
        });

        // 4. Build result from scored records
        let mut record_map: HashMap<String, UnifiedRecord> = HashMap::new();
        for record in structured_result.records {
            let key = self.record_to_key(&record);
            record_map.insert(key, record);
        }
        for record in vector_result.records {
            let key = self.record_to_key(&record);
            if let Some(existing) = record_map.get_mut(&key) {
                // Merge vector results
                existing.vector_results.extend(record.vector_results);
            } else {
                record_map.insert(key, record);
            }
        }

        // Build final result in RRF order
        let mut columns = structured_result.columns.clone();
        for col in &vector_result.columns {
            if !columns.contains(col) {
                columns.push(col.clone());
            }
        }

        let mut result = UnifiedResult::with_columns(columns);
        result.stats = QueryStats::merge(&structured_result.stats, &vector_result.stats);

        for (key, score) in rrf_scores {
            if let Some(mut record) = record_map.remove(&key) {
                record
                    .values
                    .insert("_rrf_score".to_string(), Value::Float(score));
                result.push(record);
            }
        }

        Ok(result)
    }

    /// Intersection: Only return results present in both
    fn execute_intersection(&self, query: &HybridQuery) -> Result<UnifiedResult, ExecutionError> {
        // 1. Execute both queries in parallel — intersection needs both
        //    result sets before it can filter.
        let (structured_result, vector_result) =
            self.execute_structured_and_vector_parallel(query)?;

        // 2. Build key sets
        let structured_keys: HashSet<String> = structured_result
            .records
            .iter()
            .map(|r| self.record_to_key(r))
            .collect();

        // 3. Filter vector results to only those in structured
        let mut result = UnifiedResult::with_columns(vector_result.columns.clone());

        for record in vector_result.records {
            let key = self.record_to_key(&record);
            if structured_keys.contains(&key) {
                result.push(record);
            }
        }

        result.stats = QueryStats::merge(&structured_result.stats, &vector_result.stats);
        Ok(result)
    }

    /// Union: Combine results with weighted scores
    fn execute_union(
        &self,
        query: &HybridQuery,
        struct_weight: f32,
        vector_weight: f32,
    ) -> Result<UnifiedResult, ExecutionError> {
        // 1. Execute both queries in parallel — union merges both result
        //    sets with weighted scores, so neither arm can be skipped.
        let (structured_result, vector_result) =
            self.execute_structured_and_vector_parallel(query)?;

        // 2. Score and collect all records
        let mut scored_records: HashMap<String, (UnifiedRecord, f32)> = HashMap::new();

        // Add structured results with score based on rank
        for (rank, record) in structured_result.records.into_iter().enumerate() {
            let key = self.record_to_key(&record);
            let score = struct_weight * (1.0 / (rank as f32 + 1.0));
            scored_records.insert(key, (record, score));
        }

        // Add/merge vector results
        for (rank, record) in vector_result.records.into_iter().enumerate() {
            let key = self.record_to_key(&record);
            let vector_score = vector_weight * (1.0 / (rank as f32 + 1.0));

            if let Some((existing, score)) = scored_records.get_mut(&key) {
                // Merge: add vector score and vector results
                *score += vector_score;
                existing.vector_results.extend(record.vector_results);
            } else {
                scored_records.insert(key, (record, vector_score));
            }
        }

        // 3. Sort by combined score
        let mut sorted: Vec<(String, UnifiedRecord, f32)> = scored_records
            .into_iter()
            .map(|(key, (record, score))| (key, record, score))
            .collect();
        sorted.sort_by(
            |a, b| match b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal) {
                std::cmp::Ordering::Equal => a.0.cmp(&b.0),
                ordering => ordering,
            },
        );

        // 4. Build result
        let mut columns = structured_result.columns.clone();
        for col in &vector_result.columns {
            if !columns.contains(col) {
                columns.push(col.clone());
            }
        }

        let mut result = UnifiedResult::with_columns(columns);
        result.stats = QueryStats::merge(&structured_result.stats, &vector_result.stats);

        for (_key, mut record, score) in sorted {
            record
                .values
                .insert("_union_score".to_string(), Value::Float(score as f64));
            result.push(record);
        }

        Ok(result)
    }

    // =========================================================================
    // Helper Methods
    // =========================================================================

    /// Get a unique key for a record (for deduplication)
    fn record_to_key(&self, record: &UnifiedRecord) -> String {
        // Try various ways to identify the record
        if let Some(Value::Integer(id)) = record.values.get("id") {
            return format!("row:{}", id);
        }
        if let Some(first_node) = record.nodes.values().next() {
            return format!("node:{}", first_node.id);
        }
        if let Some(first_vsr) = record.vector_results.first() {
            return format!("vec:{}:{}", first_vsr.collection, first_vsr.id);
        }
        // Fallback: hash of all values
        format!("hash:{:?}", record.values)
    }

    /// Get vector similarity score for a structured record
    fn get_vector_score_for_record(
        &self,
        record: &UnifiedRecord,
        vector_distances: &HashMap<String, f32>,
    ) -> f32 {
        // Try to find matching vector via ID
        if let Some(Value::Integer(id)) = record.values.get("id") {
            // Check all collections in vector_distances
            for (key, distance) in vector_distances {
                if key.ends_with(&format!(":{}", id)) {
                    // Convert distance to similarity (lower distance = higher similarity)
                    return 1.0 / (1.0 + distance);
                }
            }
        }

        // Try via cross-reference if available
        if let Some(ref unified_index) = self.unified_index {
            if let Some(Value::Integer(id)) = record.values.get("id") {
                // Look up if this row has a linked vector
                // This requires the unified_index to track row->vector mappings
                // For now, return 0 if no match found
            }
        }

        0.0 // No vector match found
    }
}

// ============================================================================
// QueryStats Helper
// ============================================================================

impl QueryStats {
    /// Merge two QueryStats
    fn merge(a: &QueryStats, b: &QueryStats) -> QueryStats {
        QueryStats {
            nodes_scanned: a.nodes_scanned + b.nodes_scanned,
            edges_scanned: a.edges_scanned + b.edges_scanned,
            rows_scanned: a.rows_scanned + b.rows_scanned,
            exec_time_us: a.exec_time_us + b.exec_time_us,
        }
    }
}

// ============================================================================
// In-Memory Hybrid Executor for Testing
// ============================================================================

use super::vector::InMemoryVectorExecutor;

/// In-memory hybrid executor for testing
pub struct InMemoryHybridExecutor {
    /// Records keyed by ID
    records: HashMap<u64, UnifiedRecord>,
    /// Vector executor
    vector: InMemoryVectorExecutor,
}

impl InMemoryHybridExecutor {
    /// Create a new in-memory hybrid executor
    pub fn new() -> Self {
        Self {
            records: HashMap::new(),
            vector: InMemoryVectorExecutor::new(),
        }
    }

    /// Add a structured record
    pub fn add_record(&mut self, id: u64, values: HashMap<String, Value>) {
        let mut record = UnifiedRecord::new();
        record.values = values;
        record
            .values
            .insert("id".to_string(), Value::Integer(id as i64));
        self.records.insert(id, record);
    }

    /// Add a vector with optional link to record
    pub fn add_vector(
        &mut self,
        collection: &str,
        id: u64,
        vector: Vec<f32>,
        linked_record_id: Option<u64>,
    ) {
        use crate::storage::engine::vector_metadata::MetadataValue;
        let mut meta = HashMap::new();
        if let Some(record_id) = linked_record_id {
            meta.insert(
                "_linked_record".to_string(),
                MetadataValue::Integer(record_id as i64),
            );
        }
        let meta = if meta.is_empty() { None } else { Some(meta) };
        self.vector.add_vector(collection, id, vector, meta);
    }

    /// Execute a hybrid query with manual fusion
    pub fn execute_with_fusion(
        &self,
        structured_ids: &[u64],
        vector_query: &VectorQuery,
        fusion: &FusionStrategy,
    ) -> Result<UnifiedResult, ExecutionError> {
        // Execute vector query
        let vector_result = self.vector.execute(vector_query)?;

        // Get structured records
        let structured: Vec<_> = structured_ids
            .iter()
            .filter_map(|id| self.records.get(id).cloned())
            .collect();

        // Apply fusion strategy
        match fusion {
            FusionStrategy::Rerank { weight } => {
                self.fuse_rerank(structured, vector_result, *weight)
            }
            FusionStrategy::Intersection => self.fuse_intersection(structured, vector_result),
            FusionStrategy::RRF { k } => self.fuse_rrf(structured, vector_result, *k),
            _ => {
                // Default: just return vector results
                Ok(vector_result)
            }
        }
    }

    fn fuse_rerank(
        &self,
        structured: Vec<UnifiedRecord>,
        vector_result: UnifiedResult,
        weight: f32,
    ) -> Result<UnifiedResult, ExecutionError> {
        let mut scored: Vec<(String, UnifiedRecord, f32)> = Vec::new();

        for (rank, record) in structured.into_iter().enumerate() {
            let struct_score = 1.0 / (rank as f32 + 1.0);
            let vector_score = self.get_vector_score(&record, &vector_result);
            let combined = (1.0 - weight) * struct_score + weight * vector_score;
            let key = self.record_to_key_in_memory(&record);
            scored.push((key, record, combined));
        }

        scored.sort_by(
            |a, b| match b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal) {
                std::cmp::Ordering::Equal => a.0.cmp(&b.0),
                ordering => ordering,
            },
        );

        let mut result = UnifiedResult::with_columns(vec!["id".to_string()]);
        for (_key, mut record, score) in scored {
            record
                .values
                .insert("_hybrid_score".to_string(), Value::Float(score as f64));
            result.push(record);
        }

        Ok(result)
    }

    fn fuse_intersection(
        &self,
        structured: Vec<UnifiedRecord>,
        vector_result: UnifiedResult,
    ) -> Result<UnifiedResult, ExecutionError> {
        let struct_ids: HashSet<i64> = structured
            .iter()
            .filter_map(|r| match r.values.get("id") {
                Some(Value::Integer(i)) => Some(*i),
                _ => None,
            })
            .collect();

        let mut result = UnifiedResult::with_columns(vector_result.columns.clone());

        for record in vector_result.records {
            if let Some(vsr) = record.vector_results.first() {
                if struct_ids.contains(&(vsr.id as i64)) {
                    result.push(record);
                }
            }
        }

        Ok(result)
    }

    fn fuse_rrf(
        &self,
        structured: Vec<UnifiedRecord>,
        vector_result: UnifiedResult,
        k: u32,
    ) -> Result<UnifiedResult, ExecutionError> {
        let k_f64 = k as f64;

        // Build ID -> structured rank map
        let struct_ranks: HashMap<i64, u32> = structured
            .iter()
            .enumerate()
            .filter_map(|(rank, r)| match r.values.get("id") {
                Some(Value::Integer(i)) => Some((*i, (rank + 1) as u32)),
                _ => None,
            })
            .collect();

        // Calculate RRF scores for vector results
        let mut scored: Vec<(String, UnifiedRecord, f64)> = Vec::new();

        for (rank, record) in vector_result.records.into_iter().enumerate() {
            let vector_contrib = 1.0 / (k_f64 + (rank + 1) as f64);

            let struct_contrib = record
                .vector_results
                .first()
                .and_then(|vsr| struct_ranks.get(&(vsr.id as i64)))
                .map(|r| 1.0 / (k_f64 + *r as f64))
                .unwrap_or(0.0);

            let rrf_score = struct_contrib + vector_contrib;
            let key = self.record_to_key_in_memory(&record);
            scored.push((key, record, rrf_score));
        }

        scored.sort_by(
            |a, b| match b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal) {
                std::cmp::Ordering::Equal => a.0.cmp(&b.0),
                ordering => ordering,
            },
        );

        let mut result =
            UnifiedResult::with_columns(vec!["id".to_string(), "distance".to_string()]);
        for (_key, mut record, score) in scored {
            record
                .values
                .insert("_rrf_score".to_string(), Value::Float(score));
            result.push(record);
        }

        Ok(result)
    }

    fn get_vector_score(&self, record: &UnifiedRecord, vector_result: &UnifiedResult) -> f32 {
        if let Some(Value::Integer(id)) = record.values.get("id") {
            for vr in &vector_result.records {
                for vsr in &vr.vector_results {
                    if vsr.id == *id as u64 {
                        return 1.0 / (1.0 + vsr.distance);
                    }
                }
            }
        }
        0.0
    }

    fn record_to_key_in_memory(&self, record: &UnifiedRecord) -> String {
        if let Some(Value::Integer(id)) = record.values.get("id") {
            return format!("row:{}", id);
        }
        if let Some(first_vsr) = record.vector_results.first() {
            return format!("vec:{}:{}", first_vsr.collection, first_vsr.id);
        }
        format!("hash:{:?}", record.values)
    }
}

impl Default for InMemoryHybridExecutor {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::distance::DistanceMetric;
    use crate::storage::query::ast::VectorSource;

    #[test]
    fn test_in_memory_hybrid_rerank() {
        let mut executor = InMemoryHybridExecutor::new();

        // Add structured records
        let mut vals1 = HashMap::new();
        vals1.insert("name".to_string(), Value::Text("host1".to_string()));
        executor.add_record(1, vals1);

        let mut vals2 = HashMap::new();
        vals2.insert("name".to_string(), Value::Text("host2".to_string()));
        executor.add_record(2, vals2);

        let mut vals3 = HashMap::new();
        vals3.insert("name".to_string(), Value::Text("host3".to_string()));
        executor.add_record(3, vals3);

        // Add vectors (host3 is most similar to query)
        executor.add_vector("hosts", 1, vec![0.1, 0.0], Some(1));
        executor.add_vector("hosts", 2, vec![0.5, 0.5], Some(2));
        executor.add_vector("hosts", 3, vec![0.99, 0.0], Some(3)); // Closest to query

        let query = VectorQuery {
            alias: None,
            collection: "hosts".to_string(),
            query_vector: VectorSource::Literal(vec![1.0, 0.0]),
            k: 3,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: false,
            threshold: None,
        };

        // With pure structural ranking (weight=0), order should be 1, 2, 3
        let result = executor
            .execute_with_fusion(&[1, 2, 3], &query, &FusionStrategy::Rerank { weight: 0.0 })
            .unwrap();

        assert_eq!(result.len(), 3);
        assert_eq!(result.records[0].values.get("id"), Some(&Value::Integer(1)));

        // With pure vector ranking (weight=1), order should be 3, 1, 2
        let result = executor
            .execute_with_fusion(&[1, 2, 3], &query, &FusionStrategy::Rerank { weight: 1.0 })
            .unwrap();

        assert_eq!(result.len(), 3);
        assert_eq!(result.records[0].values.get("id"), Some(&Value::Integer(3)));
    }

    #[test]
    fn test_in_memory_hybrid_intersection() {
        let mut executor = InMemoryHybridExecutor::new();

        // Add records 1-5
        for i in 1..=5 {
            let mut vals = HashMap::new();
            vals.insert("name".to_string(), Value::Text(format!("host{}", i)));
            executor.add_record(i, vals);
        }

        // Add vectors for only 2, 3, 4
        executor.add_vector("hosts", 2, vec![0.1, 0.0], Some(2));
        executor.add_vector("hosts", 3, vec![0.5, 0.5], Some(3));
        executor.add_vector("hosts", 4, vec![0.9, 0.0], Some(4));

        let query = VectorQuery {
            alias: None,
            collection: "hosts".to_string(),
            query_vector: VectorSource::Literal(vec![1.0, 0.0]),
            k: 10,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: false,
            threshold: None,
        };

        // Intersection of structured [1,2,3] and vectors [2,3,4] should be [2,3]
        let result = executor
            .execute_with_fusion(&[1, 2, 3], &query, &FusionStrategy::Intersection)
            .unwrap();

        assert_eq!(result.len(), 2);

        let ids: HashSet<i64> = result
            .records
            .iter()
            .filter_map(|r| match r.values.get("id") {
                Some(Value::Integer(i)) => Some(*i),
                _ => None,
            })
            .collect();

        assert!(ids.contains(&2));
        assert!(ids.contains(&3));
    }

    #[test]
    fn test_in_memory_hybrid_rrf() {
        let mut executor = InMemoryHybridExecutor::new();

        for i in 1..=4 {
            let mut vals = HashMap::new();
            vals.insert("name".to_string(), Value::Text(format!("host{}", i)));
            executor.add_record(i, vals);
            executor.add_vector("hosts", i, vec![i as f32 * 0.25, 0.0], Some(i));
        }

        let query = VectorQuery {
            alias: None,
            collection: "hosts".to_string(),
            query_vector: VectorSource::Literal(vec![1.0, 0.0]),
            k: 4,
            filter: None,
            metric: Some(DistanceMetric::L2),
            include_vectors: false,
            include_metadata: false,
            threshold: None,
        };

        // RRF with k=60
        let result = executor
            .execute_with_fusion(
                &[1, 2, 3, 4], // Structured order: 1, 2, 3, 4
                &query,        // Vector order: 4, 3, 2, 1 (by distance to [1.0, 0.0])
                &FusionStrategy::RRF { k: 60 },
            )
            .unwrap();

        assert_eq!(result.len(), 4);

        // All records should have RRF scores
        for record in &result.records {
            assert!(record.values.contains_key("_rrf_score"));
        }
    }
}
