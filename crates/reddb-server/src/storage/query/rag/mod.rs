//! RAG (Retrieval-Augmented Generation) Engine
//!
//! This module provides intelligent context retrieval by combining:
//! - Vector similarity search (semantic matching)
//! - Graph traversal (relationship-based context)
//! - Table queries (structured data filtering)
//!
//! The RAG engine is designed for security intelligence use cases:
//! - "What vulnerabilities affect this host?" → Vector + Graph
//! - "Similar CVEs to CVE-2024-1234" → Pure vector search
//! - "Attack paths to database servers" → Graph + Vector ranking
//!
//! # Architecture
//!
//! ```text
//! Query → Analyzer → Strategy Selection → Parallel Retrieval → Fusion → Context
//!                         │
//!                    ┌────┼────┐
//!                    ▼    ▼    ▼
//!                 Vector Graph Table
//!                    │    │    │
//!                    └────┼────┘
//!                         ▼
//!                   Context Fusion
//!                         │
//!                         ▼
//!                 Ranked Results + Explanations
//! ```

pub mod context;
pub mod fusion;
pub mod retriever;
pub mod unified_adapter;

use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::engine::graph_store::GraphStore;
use crate::storage::engine::graph_table_index::GraphTableIndex;
use crate::storage::engine::unified_index::UnifiedIndex;
use crate::storage::engine::vector_store::VectorStore;
use crate::storage::query::unified::ExecutionError;
use crate::storage::schema::Value;

pub use context::{ChunkSource, ContextChunk, RetrievalContext};
pub use fusion::{ContextFusion, FusionConfig, ResultReranker};
pub use retriever::{MultiSourceRetriever, RetrievalStrategy};
pub use unified_adapter::{
    EdgeDirection, EdgePatternSpec, GraphQueryPattern, MatchSource, MatchedEntity, MetadataQuery,
    MultiModalQuery, NodePattern, QueryCondition, QueryValue, UnifiedQueryResult,
    UnifiedQueryStats, UnifiedStoreAdapter,
};

/// RAG Engine configuration
#[derive(Debug, Clone)]
pub struct RagConfig {
    /// Maximum number of chunks to retrieve per source
    pub max_chunks_per_source: usize,
    /// Maximum total chunks to return
    pub max_total_chunks: usize,
    /// Default vector similarity threshold
    pub similarity_threshold: f32,
    /// Graph traversal depth for context expansion
    pub graph_depth: u32,
    /// Enable cross-reference expansion
    pub expand_cross_refs: bool,
    /// Minimum relevance score to include in results
    pub min_relevance: f32,
}

impl Default for RagConfig {
    fn default() -> Self {
        Self {
            max_chunks_per_source: 10,
            max_total_chunks: 25,
            similarity_threshold: 0.8,
            graph_depth: 2,
            expand_cross_refs: true,
            min_relevance: 0.3,
        }
    }
}

/// The main RAG Engine that orchestrates retrieval
pub struct RagEngine {
    /// Configuration
    config: RagConfig,
    /// Multi-source retriever
    retriever: MultiSourceRetriever,
    /// Query analyzer for strategy selection
    analyzer: QueryAnalyzer,
}

impl RagEngine {
    /// Create a new RAG engine with all storage backends
    pub fn new(
        graph: Arc<GraphStore>,
        index: Arc<GraphTableIndex>,
        vector_store: Arc<VectorStore>,
        unified_index: Arc<UnifiedIndex>,
    ) -> Self {
        Self {
            config: RagConfig::default(),
            retriever: MultiSourceRetriever::new(graph, index, vector_store, unified_index),
            analyzer: QueryAnalyzer::new(),
        }
    }

    /// Configure the RAG engine
    pub fn with_config(mut self, config: RagConfig) -> Self {
        self.config = config;
        self
    }

    /// Retrieve context for a query
    pub fn retrieve(&self, query: &str) -> Result<RetrievalContext, ExecutionError> {
        // 1. Analyze the query to determine best retrieval strategy
        let analysis = self.analyzer.analyze(query);

        // 2. Execute retrieval with determined strategy
        let context = self.retriever.retrieve(query, &analysis, &self.config)?;

        Ok(context)
    }

    /// Retrieve with explicit strategy override
    pub fn retrieve_with_strategy(
        &self,
        query: &str,
        strategy: RetrievalStrategy,
    ) -> Result<RetrievalContext, ExecutionError> {
        let analysis = QueryAnalysis {
            primary_strategy: strategy,
            ..self.analyzer.analyze(query)
        };

        self.retriever.retrieve(query, &analysis, &self.config)
    }

    /// Retrieve with a query vector (for embedding-based queries)
    pub fn retrieve_by_vector(
        &self,
        vector: &[f32],
        collection: &str,
        k: usize,
    ) -> Result<RetrievalContext, ExecutionError> {
        self.retriever
            .retrieve_by_vector(vector, collection, k, &self.config)
    }

    /// Expand context around a known entity
    pub fn expand_context(
        &self,
        entity_id: &str,
        entity_type: EntityType,
        depth: u32,
    ) -> Result<RetrievalContext, ExecutionError> {
        self.retriever
            .expand_context(entity_id, entity_type, depth, &self.config)
    }

    /// Get similar entities by vector
    pub fn find_similar(
        &self,
        collection: &str,
        entity_id: u64,
        k: usize,
    ) -> Result<Vec<SimilarEntity>, ExecutionError> {
        self.retriever.find_similar(collection, entity_id, k)
    }
}

// ============================================================================
// Query Analysis
// ============================================================================

/// Analyzed query with strategy recommendations
#[derive(Debug, Clone)]
pub struct QueryAnalysis {
    /// Primary retrieval strategy
    pub primary_strategy: RetrievalStrategy,
    /// Secondary strategies to combine
    pub secondary_strategies: Vec<RetrievalStrategy>,
    /// Detected entity types of interest
    pub entity_types: Vec<EntityType>,
    /// Detected keywords/concepts
    pub keywords: Vec<String>,
    /// Query intent classification
    pub intent: QueryIntent,
    /// Confidence in the analysis (0.0-1.0)
    pub confidence: f32,
}

/// Query intent classification
#[derive(Debug, Clone, PartialEq)]
pub enum QueryIntent {
    /// Find similar items (e.g., "similar CVEs")
    Similarity,
    /// Find paths/relationships (e.g., "how to reach X")
    PathFinding,
    /// List/filter entities (e.g., "all hosts with port 22")
    Enumeration,
    /// Get details about specific entity
    Lookup,
    /// Analyze connections/impact
    Analysis,
    /// General/unknown intent
    General,
}

/// Types of entities in the security domain
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityType {
    Host,
    Service,
    Port,
    Vulnerability,
    Credential,
    User,
    Certificate,
    Domain,
    Network,
    Technology,
    Endpoint,
    Unknown,
}

impl EntityType {
    /// Convert from string
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "host" | "hosts" | "ip" | "ips" | "server" | "servers" | "machine" => Self::Host,
            "service" | "services" => Self::Service,
            "port" | "ports" => Self::Port,
            "vuln" | "vulnerability" | "vulnerabilities" | "cve" | "cves" => Self::Vulnerability,
            "cred" | "credential" | "credentials" | "password" | "passwords" => Self::Credential,
            "user" | "users" | "account" | "accounts" => Self::User,
            "cert" | "certificate" | "certificates" | "ssl" | "tls" => Self::Certificate,
            "domain" | "domains" | "dns" => Self::Domain,
            "network" | "networks" | "subnet" | "subnets" => Self::Network,
            "tech" | "technology" | "technologies" | "software" => Self::Technology,
            "endpoint" | "endpoints" | "url" | "urls" | "api" => Self::Endpoint,
            _ => Self::Unknown,
        }
    }

    /// Get the vector collection name for this entity type
    pub fn collection_name(&self) -> &'static str {
        match self {
            Self::Host => "hosts",
            Self::Service => "services",
            Self::Port => "ports",
            Self::Vulnerability => "vulnerabilities",
            Self::Credential => "credentials",
            Self::User => "users",
            Self::Certificate => "certificates",
            Self::Domain => "domains",
            Self::Network => "networks",
            Self::Technology => "technologies",
            Self::Endpoint => "endpoints",
            Self::Unknown => "general",
        }
    }
}

/// Query analyzer for strategy selection
pub struct QueryAnalyzer {
    /// Keywords that suggest similarity search
    similarity_keywords: Vec<&'static str>,
    /// Keywords that suggest path finding
    path_keywords: Vec<&'static str>,
    /// Keywords that suggest enumeration
    enum_keywords: Vec<&'static str>,
}

impl QueryAnalyzer {
    pub fn new() -> Self {
        Self {
            similarity_keywords: vec![
                "similar",
                "like",
                "related",
                "comparable",
                "equivalent",
                "matching",
                "resembling",
                "analogous",
                "close to",
            ],
            path_keywords: vec![
                "path",
                "route",
                "reach",
                "connect",
                "between",
                "from",
                "to",
                "via",
                "through",
                "attack path",
                "lateral",
            ],
            enum_keywords: vec![
                "all", "list", "find", "show", "get", "which", "what", "where", "filter", "having",
                "with",
            ],
        }
    }

    /// Analyze a query to determine optimal retrieval strategy
    pub fn analyze(&self, query: &str) -> QueryAnalysis {
        let query_lower = query.to_lowercase();
        let words: Vec<&str> = query_lower.split_whitespace().collect();

        // Detect intent
        let intent = self.detect_intent(&query_lower);

        // Detect entity types
        let entity_types = self.detect_entity_types(&words);

        // Extract keywords
        let keywords = self.extract_keywords(&query_lower);

        // Determine primary strategy based on intent
        let primary_strategy = match intent {
            QueryIntent::Similarity => RetrievalStrategy::VectorFirst,
            QueryIntent::PathFinding => RetrievalStrategy::GraphFirst,
            QueryIntent::Enumeration => RetrievalStrategy::Hybrid,
            QueryIntent::Lookup => RetrievalStrategy::GraphFirst,
            QueryIntent::Analysis => RetrievalStrategy::Hybrid,
            QueryIntent::General => RetrievalStrategy::Hybrid,
        };

        // Determine secondary strategies
        let mut secondary_strategies = Vec::new();
        if primary_strategy != RetrievalStrategy::VectorFirst {
            secondary_strategies.push(RetrievalStrategy::VectorFirst);
        }
        if primary_strategy != RetrievalStrategy::GraphFirst {
            secondary_strategies.push(RetrievalStrategy::GraphFirst);
        }

        // Calculate confidence
        let confidence = if intent != QueryIntent::General {
            0.8
        } else if !entity_types.is_empty() {
            0.6
        } else {
            0.4
        };

        QueryAnalysis {
            primary_strategy,
            secondary_strategies,
            entity_types,
            keywords,
            intent,
            confidence,
        }
    }

    fn detect_intent(&self, query: &str) -> QueryIntent {
        // Check for similarity keywords
        if self.similarity_keywords.iter().any(|k| query.contains(k)) {
            return QueryIntent::Similarity;
        }

        // Check for path keywords
        if self.path_keywords.iter().any(|k| query.contains(k)) {
            return QueryIntent::PathFinding;
        }

        // Check for enumeration keywords
        if self.enum_keywords.iter().any(|k| query.contains(k)) {
            return QueryIntent::Enumeration;
        }

        // Check for lookup patterns (specific IDs, IPs, CVEs)
        if query.contains("cve-") || query.contains("192.") || query.contains("10.") {
            return QueryIntent::Lookup;
        }

        // Check for analysis keywords
        if query.contains("impact") || query.contains("affect") || query.contains("analyze") {
            return QueryIntent::Analysis;
        }

        QueryIntent::General
    }

    fn detect_entity_types(&self, words: &[&str]) -> Vec<EntityType> {
        let mut types = Vec::new();
        for word in words {
            let entity_type = EntityType::from_str(word);
            if entity_type != EntityType::Unknown && !types.contains(&entity_type) {
                types.push(entity_type);
            }
        }
        types
    }

    fn extract_keywords(&self, query: &str) -> Vec<String> {
        // Simple keyword extraction - filter common words
        let stop_words = [
            "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has",
            "had", "do", "does", "did", "will", "would", "could", "should", "may", "might", "must",
            "can", "to", "of", "in", "for", "on", "with", "at", "by", "from", "as", "into",
            "about", "i", "me", "my", "we", "our",
        ];

        query
            .split_whitespace()
            .filter(|w| w.len() > 2)
            .filter(|w| !stop_words.contains(&w.to_lowercase().as_str()))
            .map(|w| w.to_string())
            .collect()
    }
}

impl Default for QueryAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Similar Entity Result
// ============================================================================

/// A similar entity found via vector search
#[derive(Debug, Clone)]
pub struct SimilarEntity {
    /// Entity ID
    pub id: u64,
    /// Collection/type
    pub collection: String,
    /// Similarity score (0-1, higher is more similar)
    pub similarity: f32,
    /// Entity label/name
    pub label: Option<String>,
    /// Additional properties
    pub properties: HashMap<String, Value>,
}

impl SimilarEntity {
    pub fn new(id: u64, collection: &str, similarity: f32) -> Self {
        Self {
            id,
            collection: collection.to_string(),
            similarity,
            label: None,
            properties: HashMap::new(),
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_property(mut self, key: impl Into<String>, value: Value) -> Self {
        self.properties.insert(key.into(), value);
        self
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_analyzer_similarity_intent() {
        let analyzer = QueryAnalyzer::new();

        let analysis = analyzer.analyze("find similar CVEs to CVE-2024-1234");
        assert_eq!(analysis.intent, QueryIntent::Similarity);
        assert_eq!(analysis.primary_strategy, RetrievalStrategy::VectorFirst);
    }

    #[test]
    fn test_query_analyzer_path_intent() {
        let analyzer = QueryAnalyzer::new();

        let analysis = analyzer.analyze("attack path from webserver to database");
        assert_eq!(analysis.intent, QueryIntent::PathFinding);
        assert_eq!(analysis.primary_strategy, RetrievalStrategy::GraphFirst);
    }

    #[test]
    fn test_query_analyzer_enumeration_intent() {
        let analyzer = QueryAnalyzer::new();

        let analysis = analyzer.analyze("list all hosts with port 22 open");
        assert_eq!(analysis.intent, QueryIntent::Enumeration);
        assert_eq!(analysis.primary_strategy, RetrievalStrategy::Hybrid);
    }

    #[test]
    fn test_entity_type_detection() {
        let analyzer = QueryAnalyzer::new();

        let analysis = analyzer.analyze("show vulnerabilities affecting hosts");
        assert!(analysis.entity_types.contains(&EntityType::Vulnerability));
        assert!(analysis.entity_types.contains(&EntityType::Host));
    }

    #[test]
    fn test_keyword_extraction() {
        let analyzer = QueryAnalyzer::new();

        let analysis = analyzer.analyze("find critical vulnerabilities in production servers");
        assert!(analysis.keywords.contains(&"critical".to_string()));
        assert!(analysis.keywords.contains(&"production".to_string()));
        assert!(!analysis.keywords.contains(&"in".to_string())); // stop word
    }
}
