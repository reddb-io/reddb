//! Query result types
//!
//! Types returned from DSL query execution.

use super::super::entity::{EntityId, UnifiedEntity};

/// Result from a DSL query execution
#[derive(Debug, Clone)]
pub struct QueryResult {
    /// Matched entities with scores
    pub matches: Vec<ScoredMatch>,
    /// Total number of entities scanned
    pub scanned: usize,
    /// Execution time in microseconds
    pub execution_time_us: u64,
    /// Query explanation (for debugging)
    pub explanation: String,
}

impl QueryResult {
    /// Create an empty result
    pub fn empty() -> Self {
        Self {
            matches: Vec::new(),
            scanned: 0,
            execution_time_us: 0,
            explanation: String::new(),
        }
    }

    /// Number of matches
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// Get top N matches
    pub fn top(&self, n: usize) -> Vec<&ScoredMatch> {
        self.matches.iter().take(n).collect()
    }

    /// Iterate over matches
    pub fn iter(&self) -> impl Iterator<Item = &ScoredMatch> {
        self.matches.iter()
    }

    /// Get entities only
    pub fn entities(&self) -> Vec<&UnifiedEntity> {
        self.matches.iter().map(|m| &m.entity).collect()
    }
}

/// A matched entity with score and source information
#[derive(Debug, Clone)]
pub struct ScoredMatch {
    /// The matched entity
    pub entity: UnifiedEntity,
    /// Combined relevance score (0.0 - 1.0)
    pub score: f32,
    /// Component scores (what contributed to the match)
    pub components: MatchComponents,
    /// Traversal path (if graph query)
    pub path: Option<Vec<EntityId>>,
}

/// Score components from different query modes
#[derive(Debug, Clone, Default)]
pub struct MatchComponents {
    /// Vector similarity score
    pub vector_similarity: Option<f32>,
    /// Graph pattern match score
    pub graph_match: Option<f32>,
    /// Metadata filter match
    pub filter_match: bool,
    /// Cross-reference hop count
    pub hop_distance: Option<u32>,
}
