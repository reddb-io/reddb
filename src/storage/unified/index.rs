//! Integrated Index System
//!
//! Combines multiple index types for optimal multi-modal queries:
//! - HNSW: Vector similarity search
//! - B-tree: Metadata range queries (via MetadataStorage)
//! - Inverted: Full-text search on content
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                   IntegratedIndexManager                     │
//! ├─────────────────────────────────────────────────────────────┤
//! │  ┌───────────┐  ┌───────────────┐  ┌─────────────────────┐  │
//! │  │   HNSW    │  │  InvertedIndex│  │  MetadataIndex      │  │
//! │  │  (vector) │  │  (full-text)  │  │  (B-tree ranges)    │  │
//! │  └─────┬─────┘  └───────┬───────┘  └──────────┬──────────┘  │
//! │        │                │                      │             │
//! │        └────────────────┼──────────────────────┘             │
//! │                         │                                    │
//! │              ┌──────────▼──────────┐                        │
//! │              │   Unified Query     │                        │
//! │              │   Executor          │                        │
//! │              └─────────────────────┘                        │
//! └─────────────────────────────────────────────────────────────┘
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use super::entity::EntityId;
use super::metadata::{Metadata, MetadataStorage, MetadataValue};

// ============================================================================
// Graph Adjacency Index
// ============================================================================

/// Edge direction for adjacency lookups
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeDirection {
    Outgoing,
    Incoming,
    Both,
}

/// An adjacency entry representing a single edge
#[derive(Debug, Clone)]
pub struct AdjacencyEntry {
    /// The edge entity ID
    pub edge_id: EntityId,
    /// The target node (for outgoing) or source node (for incoming)
    pub neighbor_id: EntityId,
    /// Edge label/type
    pub label: String,
    /// Edge weight (optional, default 1.0)
    pub weight: f32,
}

/// Graph adjacency index for fast edge/neighbor lookups
///
/// Supports O(1) lookups for:
/// - All outgoing edges from a node
/// - All incoming edges to a node
/// - Edges filtered by label
pub struct GraphAdjacencyIndex {
    /// Node → outgoing edges: HashMap<source_id, Vec<AdjacencyEntry>>
    outgoing: RwLock<HashMap<EntityId, Vec<AdjacencyEntry>>>,
    /// Node → incoming edges: HashMap<target_id, Vec<AdjacencyEntry>>
    incoming: RwLock<HashMap<EntityId, Vec<AdjacencyEntry>>>,
    /// Label → edge IDs for label-based filtering
    by_label: RwLock<HashMap<String, HashSet<EntityId>>>,
    /// Edge count for stats
    edge_count: RwLock<usize>,
    /// Node count (unique nodes with edges)
    node_count: RwLock<usize>,
}

impl GraphAdjacencyIndex {
    /// Create a new empty adjacency index
    pub fn new() -> Self {
        Self {
            outgoing: RwLock::new(HashMap::new()),
            incoming: RwLock::new(HashMap::new()),
            by_label: RwLock::new(HashMap::new()),
            edge_count: RwLock::new(0),
            node_count: RwLock::new(0),
        }
    }

    /// Index an edge for fast lookups
    pub fn index_edge(
        &self,
        edge_id: EntityId,
        source_id: EntityId,
        target_id: EntityId,
        label: &str,
        weight: f32,
    ) {
        // Add to outgoing adjacency
        if let Ok(mut outgoing) = self.outgoing.write() {
            let entry = AdjacencyEntry {
                edge_id,
                neighbor_id: target_id,
                label: label.to_string(),
                weight,
            };
            outgoing.entry(source_id).or_default().push(entry);
        }

        // Add to incoming adjacency
        if let Ok(mut incoming) = self.incoming.write() {
            let entry = AdjacencyEntry {
                edge_id,
                neighbor_id: source_id,
                label: label.to_string(),
                weight,
            };
            incoming.entry(target_id).or_default().push(entry);
        }

        // Add to label index
        if let Ok(mut by_label) = self.by_label.write() {
            by_label
                .entry(label.to_string())
                .or_default()
                .insert(edge_id);
        }

        // Update counts
        if let Ok(mut count) = self.edge_count.write() {
            *count += 1;
        }

        // Update node count (track unique nodes)
        self.update_node_count();
    }

    /// Remove an edge from the index
    pub fn remove_edge(&self, edge_id: EntityId) {
        // Remove from outgoing
        if let Ok(mut outgoing) = self.outgoing.write() {
            for entries in outgoing.values_mut() {
                entries.retain(|e| e.edge_id != edge_id);
            }
        }

        // Remove from incoming
        if let Ok(mut incoming) = self.incoming.write() {
            for entries in incoming.values_mut() {
                entries.retain(|e| e.edge_id != edge_id);
            }
        }

        // Remove from label index
        if let Ok(mut by_label) = self.by_label.write() {
            for edges in by_label.values_mut() {
                edges.remove(&edge_id);
            }
        }

        // Update counts
        if let Ok(mut count) = self.edge_count.write() {
            *count = count.saturating_sub(1);
        }
    }

    /// Get neighbors in a direction (optionally filtered by label)
    pub fn get_neighbors(
        &self,
        node_id: EntityId,
        direction: EdgeDirection,
        label_filter: Option<&str>,
    ) -> Vec<AdjacencyEntry> {
        let mut results = Vec::new();

        if matches!(direction, EdgeDirection::Outgoing | EdgeDirection::Both) {
            if let Ok(outgoing) = self.outgoing.read() {
                if let Some(entries) = outgoing.get(&node_id) {
                    for entry in entries {
                        if label_filter.map_or(true, |l| entry.label == l) {
                            results.push(entry.clone());
                        }
                    }
                }
            }
        }

        if matches!(direction, EdgeDirection::Incoming | EdgeDirection::Both) {
            if let Ok(incoming) = self.incoming.read() {
                if let Some(entries) = incoming.get(&node_id) {
                    for entry in entries {
                        if label_filter.map_or(true, |l| entry.label == l) {
                            results.push(entry.clone());
                        }
                    }
                }
            }
        }

        results
    }

    /// Get all edges with a specific label
    pub fn get_edges_by_label(&self, label: &str) -> Vec<EntityId> {
        self.by_label
            .read()
            .map(|idx| {
                idx.get(label)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// Get outgoing degree of a node
    pub fn out_degree(&self, node_id: EntityId) -> usize {
        self.outgoing
            .read()
            .map(|o| o.get(&node_id).map(|v| v.len()).unwrap_or(0))
            .unwrap_or(0)
    }

    /// Get incoming degree of a node
    pub fn in_degree(&self, node_id: EntityId) -> usize {
        self.incoming
            .read()
            .map(|i| i.get(&node_id).map(|v| v.len()).unwrap_or(0))
            .unwrap_or(0)
    }

    /// Get total degree of a node
    pub fn degree(&self, node_id: EntityId) -> usize {
        self.out_degree(node_id) + self.in_degree(node_id)
    }

    /// Get edge count
    pub fn edge_count(&self) -> usize {
        self.edge_count.read().map(|c| *c).unwrap_or(0)
    }

    /// Get node count
    pub fn node_count(&self) -> usize {
        self.node_count.read().map(|c| *c).unwrap_or(0)
    }

    /// Clear the entire index
    pub fn clear(&self) {
        if let Ok(mut o) = self.outgoing.write() {
            o.clear();
        }
        if let Ok(mut i) = self.incoming.write() {
            i.clear();
        }
        if let Ok(mut l) = self.by_label.write() {
            l.clear();
        }
        if let Ok(mut c) = self.edge_count.write() {
            *c = 0;
        }
        if let Ok(mut n) = self.node_count.write() {
            *n = 0;
        }
    }

    fn update_node_count(&self) {
        let out_nodes = self
            .outgoing
            .read()
            .map(|o| o.keys().copied().collect::<HashSet<_>>())
            .unwrap_or_default();
        let in_nodes = self
            .incoming
            .read()
            .map(|i| i.keys().copied().collect::<HashSet<_>>())
            .unwrap_or_default();

        let total: HashSet<_> = out_nodes.union(&in_nodes).collect();
        if let Ok(mut count) = self.node_count.write() {
            *count = total.len();
        }
    }
}

impl Default for GraphAdjacencyIndex {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Inverted Index for Full-Text Search
// ============================================================================

/// Token with position information for phrase queries
#[derive(Debug, Clone)]
pub struct TokenPosition {
    pub entity_id: EntityId,
    pub field: String,
    pub position: u32,
}

/// A posting list entry
#[derive(Debug, Clone)]
pub struct PostingEntry {
    pub entity_id: EntityId,
    pub collection: String,
    pub field: String,
    pub positions: Vec<u32>,
    pub term_frequency: f32,
}

/// Inverted index for full-text search
pub struct InvertedIndex {
    /// Term → Posting list
    index: RwLock<BTreeMap<String, Vec<PostingEntry>>>,
    /// Document frequencies for TF-IDF
    doc_count: RwLock<usize>,
    /// Indexed fields per collection
    indexed_fields: RwLock<HashMap<String, HashSet<String>>>,
}

impl InvertedIndex {
    /// Create a new empty inverted index
    pub fn new() -> Self {
        Self {
            index: RwLock::new(BTreeMap::new()),
            doc_count: RwLock::new(0),
            indexed_fields: RwLock::new(HashMap::new()),
        }
    }

    /// Configure which fields to index for a collection
    pub fn add_indexed_field(&self, collection: &str, field: &str) {
        if let Ok(mut fields) = self.indexed_fields.write() {
            fields
                .entry(collection.to_string())
                .or_default()
                .insert(field.to_string());
        }
    }

    /// Index a document's text content
    pub fn index_document(
        &self,
        collection: &str,
        entity_id: EntityId,
        field: &str,
        content: &str,
    ) {
        let tokens = self.tokenize(content);
        let term_count = tokens.len() as f32;

        // Count term frequencies
        let mut term_freqs: HashMap<String, Vec<u32>> = HashMap::new();
        for (position, token) in tokens.iter().enumerate() {
            term_freqs
                .entry(token.clone())
                .or_default()
                .push(position as u32);
        }

        // Add to index
        if let Ok(mut index) = self.index.write() {
            for (term, positions) in term_freqs {
                let tf = positions.len() as f32 / term_count.max(1.0);

                let entry = PostingEntry {
                    entity_id,
                    collection: collection.to_string(),
                    field: field.to_string(),
                    positions,
                    term_frequency: tf,
                };

                index.entry(term).or_default().push(entry);
            }
        }

        // Update doc count
        if let Ok(mut count) = self.doc_count.write() {
            *count += 1;
        }
    }

    /// Remove a document from the index
    pub fn remove_document(&self, entity_id: EntityId) {
        if let Ok(mut index) = self.index.write() {
            for postings in index.values_mut() {
                postings.retain(|p| p.entity_id != entity_id);
            }
        }
    }

    /// Search for documents containing all terms (AND query)
    pub fn search(&self, query: &str, limit: usize) -> Vec<TextSearchResult> {
        let terms = self.tokenize(query);
        if terms.is_empty() {
            return Vec::new();
        }

        let index = match self.index.read() {
            Ok(i) => i,
            Err(_) => return Vec::new(),
        };

        let doc_count = self.doc_count.read().map(|g| *g).unwrap_or(1);

        // Get posting lists for all terms
        let mut term_postings: Vec<&Vec<PostingEntry>> = Vec::new();
        for term in &terms {
            if let Some(postings) = index.get(term) {
                term_postings.push(postings);
            } else {
                // Term not found, AND query fails
                return Vec::new();
            }
        }

        // Find documents containing all terms
        let mut scores: HashMap<EntityId, f32> = HashMap::new();

        // Start with first term's documents
        if let Some(first_postings) = term_postings.first() {
            for posting in *first_postings {
                let idf = ((doc_count as f32) / (first_postings.len() as f32 + 1.0)).ln();
                scores.insert(posting.entity_id, posting.term_frequency * idf);
            }
        }

        // Intersect with remaining terms
        for postings in term_postings.iter().skip(1) {
            let idf = ((doc_count as f32) / (postings.len() as f32 + 1.0)).ln();
            let entities_in_term: HashSet<EntityId> =
                postings.iter().map(|p| p.entity_id).collect();

            // Keep only documents that have this term
            scores.retain(|id, _| entities_in_term.contains(id));

            // Add TF-IDF score
            for posting in *postings {
                if let Some(score) = scores.get_mut(&posting.entity_id) {
                    *score += posting.term_frequency * idf;
                }
            }
        }

        // Convert to results and sort
        let mut results: Vec<TextSearchResult> = scores
            .into_iter()
            .map(|(entity_id, score)| {
                // Get collection from first posting
                let collection = term_postings
                    .first()
                    .and_then(|p| p.iter().find(|e| e.entity_id == entity_id))
                    .map(|p| p.collection.clone())
                    .unwrap_or_default();

                TextSearchResult {
                    entity_id,
                    collection,
                    score,
                    matched_terms: terms.clone(),
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
        results
    }

    /// Search with prefix matching (for autocomplete)
    pub fn search_prefix(&self, prefix: &str, limit: usize) -> Vec<String> {
        let prefix_lower = prefix.to_lowercase();

        let index = match self.index.read() {
            Ok(i) => i,
            Err(_) => return Vec::new(),
        };

        index
            .range(prefix_lower.clone()..)
            .take_while(|(term, _)| term.starts_with(&prefix_lower))
            .take(limit)
            .map(|(term, _)| term.clone())
            .collect()
    }

    /// Simple tokenization - splits on whitespace and punctuation
    fn tokenize(&self, text: &str) -> Vec<String> {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| s.len() >= 2)
            .map(|s| s.to_string())
            .collect()
    }
}

impl Default for InvertedIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Result from a text search
#[derive(Debug, Clone)]
pub struct TextSearchResult {
    pub entity_id: EntityId,
    pub collection: String,
    pub score: f32,
    pub matched_terms: Vec<String>,
}

// ============================================================================
// Integrated Index Manager
// ============================================================================

/// Configuration for the integrated index system
#[derive(Debug, Clone)]
pub struct IntegratedIndexConfig {
    /// Enable HNSW indexing for vectors
    pub enable_hnsw: bool,
    /// Enable full-text indexing
    pub enable_fulltext: bool,
    /// Enable metadata indexing
    pub enable_metadata: bool,
    /// Enable graph adjacency indexing
    pub enable_graph: bool,
    /// HNSW M parameter (connections per node)
    pub hnsw_m: usize,
    /// HNSW ef_construction parameter
    pub hnsw_ef_construction: usize,
    /// HNSW ef_search parameter
    pub hnsw_ef_search: usize,
}

impl Default for IntegratedIndexConfig {
    fn default() -> Self {
        Self {
            enable_hnsw: true,
            enable_fulltext: true,
            enable_metadata: true,
            enable_graph: true,
            hnsw_m: 16,
            hnsw_ef_construction: 100,
            hnsw_ef_search: 50,
        }
    }
}

/// Statistics about the index system
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    /// Number of indexed vectors
    pub vector_count: usize,
    /// Number of indexed documents (for full-text)
    pub document_count: usize,
    /// Number of unique terms
    pub term_count: usize,
    /// Number of metadata entries
    pub metadata_entries: usize,
    /// Number of graph nodes
    pub graph_node_count: usize,
    /// Number of graph edges
    pub graph_edge_count: usize,
    /// Index creation timestamp
    pub created_at: u64,
    /// Last update timestamp
    pub updated_at: u64,
}

/// Types of indices that can be managed
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IndexType {
    /// HNSW vector index
    Hnsw,
    /// Inverted full-text index
    Fulltext,
    /// B-tree metadata index
    Metadata,
    /// Graph adjacency index
    Graph,
}

/// Status of an index operation
#[derive(Debug, Clone)]
pub enum IndexStatus {
    /// Index is ready for use
    Ready,
    /// Index is being built
    Building { progress: f32 },
    /// Index needs rebuild
    Stale,
    /// Index is disabled
    Disabled,
    /// Index encountered an error
    Error(String),
}

/// Index lifecycle event for tracking
#[derive(Debug, Clone)]
pub struct IndexEvent {
    pub index_type: IndexType,
    pub collection: Option<String>,
    pub event: IndexEventKind,
    pub timestamp: u64,
}

#[derive(Debug, Clone)]
pub enum IndexEventKind {
    Created,
    Dropped,
    Rebuilt,
    Updated { entries_affected: usize },
}

/// Integrated Index Manager combining all index types
pub struct IntegratedIndexManager {
    /// Configuration
    config: IntegratedIndexConfig,
    /// Inverted index for full-text search
    text_index: InvertedIndex,
    /// Metadata storage (provides B-tree indexing)
    metadata_index: RwLock<MetadataStorage>,
    /// Collection-specific HNSW indices
    /// Key: collection name, Value: (dimension, index data)
    hnsw_indices: RwLock<HashMap<String, HnswIndexInfo>>,
    /// Graph adjacency index
    graph_index: GraphAdjacencyIndex,
    /// Index status tracking
    index_status: RwLock<HashMap<(IndexType, Option<String>), IndexStatus>>,
    /// Lifecycle event history
    event_history: RwLock<Vec<IndexEvent>>,
    /// Creation timestamp
    created_at: u64,
}

/// Information about an HNSW index for a collection
struct HnswIndexInfo {
    /// Vector dimension
    dimension: usize,
    /// Vectors stored (id → vector)
    vectors: HashMap<EntityId, Vec<f32>>,
    /// HNSW graph layers (simplified representation)
    /// In production, would use the full HNSW implementation
    entry_point: Option<EntityId>,
}

mod manager_impl;
impl Default for IntegratedIndexManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Result from a vector similarity search
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    pub entity_id: EntityId,
    pub collection: String,
    pub similarity: f32,
}

/// Filter for metadata queries
#[derive(Debug, Clone)]
pub enum MetadataQueryFilter {
    /// Exact match
    Equals(MetadataValue),
    /// Range query (inclusive)
    Range {
        min: Option<MetadataValue>,
        max: Option<MetadataValue>,
    },
    /// String contains
    Contains(String),
    /// Value in set
    In(Vec<MetadataValue>),
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Compute cosine similarity between two vectors
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inverted_index_basic() {
        let index = InvertedIndex::new();

        index.index_document(
            "docs",
            EntityId(1),
            "content",
            "hello world rust programming",
        );
        index.index_document("docs", EntityId(2), "content", "rust is fast and safe");
        index.index_document("docs", EntityId(3), "content", "python is easy to learn");

        let results = index.search("rust", 10);
        assert_eq!(results.len(), 2);
        assert!(results.iter().any(|r| r.entity_id == EntityId(1)));
        assert!(results.iter().any(|r| r.entity_id == EntityId(2)));
    }

    #[test]
    fn test_inverted_index_and_query() {
        let index = InvertedIndex::new();

        index.index_document("docs", EntityId(1), "content", "rust programming language");
        index.index_document("docs", EntityId(2), "content", "rust systems programming");
        index.index_document(
            "docs",
            EntityId(3),
            "content",
            "python programming language",
        );

        // AND query: both "rust" and "programming"
        let results = index.search("rust programming", 10);
        assert_eq!(results.len(), 2);

        // "language" appears in docs 1 and 3, "programming" in all three
        // AND of "language programming" gives docs 1 and 3
        let results = index.search("language programming", 10);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_prefix_search() {
        let index = InvertedIndex::new();

        index.index_document("docs", EntityId(1), "content", "programming rust rustacean");

        let suggestions = index.search_prefix("rust", 10);
        assert!(suggestions.contains(&"rust".to_string()));
        assert!(suggestions.contains(&"rustacean".to_string()));
    }

    #[test]
    fn test_vector_search() {
        let manager = IntegratedIndexManager::new();

        manager.index_vector("embeddings", EntityId(1), &[1.0, 0.0, 0.0]);
        manager.index_vector("embeddings", EntityId(2), &[0.9, 0.1, 0.0]);
        manager.index_vector("embeddings", EntityId(3), &[0.0, 1.0, 0.0]);

        let results = manager.search_similar("embeddings", &[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].entity_id, EntityId(1));
        assert!(results[0].similarity > 0.99);
    }

    #[test]
    fn test_cosine_similarity() {
        let a = [1.0, 0.0, 0.0];
        let b = [1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

        let c = [0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &c).abs() < 0.001);
    }

    // =========================================================================
    // Graph Adjacency Index Tests
    // =========================================================================

    #[test]
    fn test_graph_adjacency_basic() {
        let index = GraphAdjacencyIndex::new();

        // Create nodes: 1 -> 2 -> 3
        index.index_edge(EntityId(100), EntityId(1), EntityId(2), "KNOWS", 1.0);
        index.index_edge(EntityId(101), EntityId(2), EntityId(3), "KNOWS", 1.0);

        // Check outgoing from node 1
        let neighbors = index.get_neighbors(EntityId(1), EdgeDirection::Outgoing, None);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].neighbor_id, EntityId(2));

        // Check incoming to node 2
        let neighbors = index.get_neighbors(EntityId(2), EdgeDirection::Incoming, None);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].neighbor_id, EntityId(1));

        // Check both directions for node 2
        let neighbors = index.get_neighbors(EntityId(2), EdgeDirection::Both, None);
        assert_eq!(neighbors.len(), 2);
    }

    #[test]
    fn test_graph_adjacency_label_filter() {
        let index = GraphAdjacencyIndex::new();

        // Create edges with different labels
        index.index_edge(EntityId(100), EntityId(1), EntityId(2), "KNOWS", 1.0);
        index.index_edge(EntityId(101), EntityId(1), EntityId(3), "WORKS_WITH", 1.0);
        index.index_edge(EntityId(102), EntityId(1), EntityId(4), "KNOWS", 1.0);

        // Filter by KNOWS label
        let neighbors = index.get_neighbors(EntityId(1), EdgeDirection::Outgoing, Some("KNOWS"));
        assert_eq!(neighbors.len(), 2);

        // Filter by WORKS_WITH label
        let neighbors =
            index.get_neighbors(EntityId(1), EdgeDirection::Outgoing, Some("WORKS_WITH"));
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].neighbor_id, EntityId(3));
    }

    #[test]
    fn test_graph_adjacency_degree() {
        let index = GraphAdjacencyIndex::new();

        // Create a star graph: 1 -> [2, 3, 4, 5]
        index.index_edge(EntityId(100), EntityId(1), EntityId(2), "LINK", 1.0);
        index.index_edge(EntityId(101), EntityId(1), EntityId(3), "LINK", 1.0);
        index.index_edge(EntityId(102), EntityId(1), EntityId(4), "LINK", 1.0);
        index.index_edge(EntityId(103), EntityId(1), EntityId(5), "LINK", 1.0);

        assert_eq!(index.out_degree(EntityId(1)), 4);
        assert_eq!(index.in_degree(EntityId(1)), 0);
        assert_eq!(index.degree(EntityId(1)), 4);

        // Leaf nodes have in-degree 1
        assert_eq!(index.in_degree(EntityId(2)), 1);
        assert_eq!(index.out_degree(EntityId(2)), 0);
    }

    #[test]
    fn test_graph_adjacency_edge_by_label() {
        let index = GraphAdjacencyIndex::new();

        index.index_edge(EntityId(100), EntityId(1), EntityId(2), "A", 1.0);
        index.index_edge(EntityId(101), EntityId(2), EntityId(3), "B", 1.0);
        index.index_edge(EntityId(102), EntityId(3), EntityId(4), "A", 1.0);

        let edges_a = index.get_edges_by_label("A");
        assert_eq!(edges_a.len(), 2);
        assert!(edges_a.contains(&EntityId(100)));
        assert!(edges_a.contains(&EntityId(102)));

        let edges_b = index.get_edges_by_label("B");
        assert_eq!(edges_b.len(), 1);
        assert!(edges_b.contains(&EntityId(101)));
    }

    #[test]
    fn test_graph_adjacency_remove() {
        let index = GraphAdjacencyIndex::new();

        index.index_edge(EntityId(100), EntityId(1), EntityId(2), "LINK", 1.0);
        index.index_edge(EntityId(101), EntityId(1), EntityId(3), "LINK", 1.0);

        assert_eq!(index.edge_count(), 2);

        // Remove one edge
        index.remove_edge(EntityId(100));

        assert_eq!(index.edge_count(), 1);
        let neighbors = index.get_neighbors(EntityId(1), EdgeDirection::Outgoing, None);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].neighbor_id, EntityId(3));
    }

    // =========================================================================
    // Index Lifecycle Tests
    // =========================================================================

    #[test]
    fn test_index_lifecycle_create_drop() {
        let manager = IntegratedIndexManager::new();

        // Create a new HNSW index for a specific collection
        let result = manager.create_index(IndexType::Hnsw, Some("my_collection"));
        assert!(result.is_ok());

        // Check status
        let status = manager.index_status(IndexType::Hnsw, Some("my_collection"));
        assert!(matches!(status, IndexStatus::Ready));

        // Drop the index
        let result = manager.drop_index(IndexType::Hnsw, Some("my_collection"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_index_lifecycle_rebuild() {
        let manager = IntegratedIndexManager::new();

        // Index some vectors
        manager.index_vector("test", EntityId(1), &[1.0, 0.0, 0.0]);
        manager.index_vector("test", EntityId(2), &[0.0, 1.0, 0.0]);

        // Rebuild the index
        let result = manager.rebuild_index(IndexType::Hnsw, Some("test"));
        assert!(result.is_ok());

        // Check status is ready
        let status = manager.index_status(IndexType::Hnsw, Some("test"));
        assert!(matches!(status, IndexStatus::Ready));
    }

    #[test]
    fn test_index_stats_with_graph() {
        let manager = IntegratedIndexManager::new();

        // Add some edges
        manager.index_edge(EntityId(100), EntityId(1), EntityId(2), "LINK", 1.0);
        manager.index_edge(EntityId(101), EntityId(2), EntityId(3), "LINK", 1.0);

        let stats = manager.stats();
        assert_eq!(stats.graph_edge_count, 2);
        assert!(stats.graph_node_count >= 2); // At least source nodes
    }

    #[test]
    fn test_integrated_manager_graph_operations() {
        let manager = IntegratedIndexManager::new();

        // Index edges through the manager
        manager.index_edge(EntityId(100), EntityId(1), EntityId(2), "KNOWS", 1.0);
        manager.index_edge(EntityId(101), EntityId(2), EntityId(3), "KNOWS", 0.5);

        // Query neighbors through the manager
        let neighbors = manager.get_neighbors(EntityId(1), EdgeDirection::Outgoing, None);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].neighbor_id, EntityId(2));
        assert_eq!(neighbors[0].weight, 1.0);

        // Check degree
        assert_eq!(manager.node_degree(EntityId(2), EdgeDirection::Both), 2);
    }
}
