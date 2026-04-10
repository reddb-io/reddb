//! Entity Preprocessors
//!
//! Preprocessor trait and built-in implementations for automatic entity processing.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use super::super::{EntityData, EntityKind, UnifiedEntity};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for index behavior
#[derive(Debug, Clone)]
pub struct IndexConfig {
    /// Enable HNSW for vectors
    pub hnsw_enabled: bool,
    /// HNSW M parameter
    pub hnsw_m: usize,
    /// HNSW ef_construction
    pub hnsw_ef_construction: usize,
    /// Enable B-tree for properties
    pub btree_enabled: bool,
    /// Enable inverted index for text
    pub inverted_index_enabled: bool,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            hnsw_enabled: true,
            hnsw_m: 16,
            hnsw_ef_construction: 200,
            btree_enabled: true,
            inverted_index_enabled: true,
        }
    }
}

// ============================================================================
// Preprocessor Trait
// ============================================================================

/// Preprocessor trait for automatic processing
pub trait Preprocessor: Send + Sync {
    /// Process an entity before storage
    fn process(&self, entity: &mut UnifiedEntity);

    /// Name for debugging
    fn name(&self) -> &str {
        "Preprocessor"
    }
}

// ============================================================================
// Built-in Preprocessors
// ============================================================================

/// Adds creation/update timestamps to metadata
pub struct TimestampPreprocessor;

impl Preprocessor for TimestampPreprocessor {
    fn process(&self, entity: &mut UnifiedEntity) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // Add created_at if not present (check by looking at timestamp value)
        if entity.created_at == 0 {
            entity.created_at = now;
        }
        entity.updated_at = now;
    }

    fn name(&self) -> &str {
        "TimestampPreprocessor"
    }
}

/// Normalizes vector embeddings to unit length (L2 norm)
pub struct VectorNormalizer;

impl Preprocessor for VectorNormalizer {
    fn process(&self, entity: &mut UnifiedEntity) {
        // Normalize dense vector if present
        if let EntityData::Vector(ref mut vec_data) = entity.data {
            let norm: f32 = vec_data.dense.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for v in &mut vec_data.dense {
                    *v /= norm;
                }
            }
        }

        // Normalize embeddings
        for embedding in &mut entity.embeddings {
            let norm: f32 = embedding.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for v in &mut embedding.vector {
                    *v /= norm;
                }
            }
        }
    }

    fn name(&self) -> &str {
        "VectorNormalizer"
    }
}

/// Adds content hash for deduplication
pub struct ContentHasher;

impl ContentHasher {
    /// Simple hash function for strings
    fn hash_string(s: &str) -> u64 {
        // FNV-1a hash
        const FNV_OFFSET: u64 = 14695981039346656037;
        const FNV_PRIME: u64 = 1099511628211;

        let mut hash = FNV_OFFSET;
        for byte in s.bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }
}

impl Preprocessor for ContentHasher {
    fn process(&self, entity: &mut UnifiedEntity) {
        // Extract label from EntityKind for nodes
        let kind_label = match &entity.kind {
            EntityKind::GraphNode { label, .. } => Some(label.as_str()),
            EntityKind::GraphEdge { label, .. } => Some(label.as_str()),
            _ => None,
        };

        let content: String = match &entity.data {
            EntityData::Vector(v) => v.content.clone().unwrap_or_default(),
            EntityData::Node(n) => {
                // Use kind label or a property value
                kind_label
                    .map(|s| s.to_string())
                    .or_else(|| {
                        n.get("name")
                            .and_then(|v| v.as_text().map(|s| s.to_string()))
                    })
                    .unwrap_or_default()
            }
            EntityData::Row(r) => {
                // Hash based on column values
                r.columns
                    .iter()
                    .filter_map(|c| c.as_text())
                    .collect::<Vec<_>>()
                    .join("|")
            }
            EntityData::Edge(_) => kind_label.unwrap_or("").to_string(),
            EntityData::TimeSeries(ts) => ts.metric.clone(),
            EntityData::QueueMessage(_) => String::new(),
        };

        if !content.is_empty() {
            let hash = Self::hash_string(&content);
            // Store hash in sequence_id as a simple marker
            // In practice, you'd want dedicated hash storage in metadata
            entity.sequence_id = hash;
        }
    }

    fn name(&self) -> &str {
        "ContentHasher"
    }
}

/// Extracts keywords from text content
pub struct KeywordExtractor {
    /// Stop words to skip
    stop_words: HashSet<String>,
}

impl Default for KeywordExtractor {
    fn default() -> Self {
        let mut stop_words = HashSet::new();
        for word in [
            "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has",
            "had", "do", "does", "did", "will", "would", "could", "should", "may", "might", "must",
            "can", "to", "of", "in", "for", "on", "with", "at", "by", "from", "as", "into",
            "about", "i", "me", "my", "we", "our", "and", "or",
        ] {
            stop_words.insert(word.to_string());
        }
        Self { stop_words }
    }
}

impl Preprocessor for KeywordExtractor {
    fn process(&self, entity: &mut UnifiedEntity) {
        // Extract label from EntityKind for nodes
        let kind_label = match &entity.kind {
            EntityKind::GraphNode { label, .. } => Some(label.clone()),
            EntityKind::GraphEdge { label, .. } => Some(label.clone()),
            _ => None,
        };

        // Extract content text
        let content = match &entity.data {
            EntityData::Vector(v) => v.content.clone(),
            EntityData::Node(n) => {
                // Use kind label or a name property
                kind_label.or_else(|| {
                    n.get("name")
                        .and_then(|v| v.as_text().map(|s| s.to_string()))
                })
            }
            _ => None,
        };

        if let Some(text) = content {
            let _keywords: Vec<String> = text
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() >= 3)
                .filter(|w| !self.stop_words.contains(*w))
                .take(10)
                .map(|s| s.to_string())
                .collect();

            // Store keywords in a cross-ref with special type
            // Note: In practice, you'd want to store this differently
            // Keywords stored - we'd need metadata access here
            // This is a limitation of the current preprocessor API
        }
    }

    fn name(&self) -> &str {
        "KeywordExtractor"
    }
}

/// Pipeline that chains multiple preprocessors
pub struct PreprocessorPipeline {
    preprocessors: Vec<Box<dyn Preprocessor>>,
}

impl PreprocessorPipeline {
    pub fn new() -> Self {
        Self {
            preprocessors: Vec::new(),
        }
    }

    pub fn add(mut self, preprocessor: Box<dyn Preprocessor>) -> Self {
        self.preprocessors.push(preprocessor);
        self
    }

    /// Standard pipeline with common preprocessors
    pub fn standard() -> Self {
        Self::new()
            .add(Box::new(TimestampPreprocessor))
            .add(Box::new(VectorNormalizer))
    }
}

impl Preprocessor for PreprocessorPipeline {
    fn process(&self, entity: &mut UnifiedEntity) {
        for preprocessor in &self.preprocessors {
            preprocessor.process(entity);
        }
    }

    fn name(&self) -> &str {
        "PreprocessorPipeline"
    }
}

impl Default for PreprocessorPipeline {
    fn default() -> Self {
        Self::new()
    }
}
