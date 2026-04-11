//! Unified DevX Layer
//!
//! The best possible developer experience for working with Tables, Graphs, and Vectors
//! in a single, unified API. Everything is interconnected with metadata that can
//! reference any other entity.
//!
//! # Design Philosophy
//!
//! 1. **Fluent API**: Chain operations naturally
//! 2. **Type Safety**: Compile-time guarantees where possible
//! 3. **Zero Ceremony**: Minimal boilerplate for common operations
//! 4. **Performance by Default**: Automatic indexing and batching
//!
//! # Quick Start
//!
//! ```ignore
//! use reddb::storage::RedDB;
//!
//! let db = RedDB::new();
//!
//! // Create a host (graph node) with metadata pointing to a table row
//! let host = db.node("hosts", "Host")
//!     .property("ip", "192.168.1.1")
//!     .property("os", "Linux")
//!     .metadata("scan_result", db.table_ref("scans", scan_id))
//!     .embedding("description", vec![0.1, 0.2, ...])
//!     .save()?;
//!
//! // Create a service connected to the host
//! let service = db.node("services", "Service")
//!     .property("port", 443)
//!     .property("name", "https")
//!     .link_from(host, "RUNS")
//!     .save()?;
//!
//! // Query: Find hosts similar to a description, filter by OS, expand services
//! let results = db.query()
//!     .similar_to(embedding, 0.8)
//!     .where_prop("os", "Linux")
//!     .expand("RUNS", 1)
//!     .limit(10)
//!     .execute()?;
//! ```

mod batch;
mod builders;
mod conversions;
mod error;
mod helpers;
mod query;
mod reddb;
pub(crate) mod refs;
mod types;

use crate::storage::unified::entity::UnifiedEntity;

/// Preprocessing hook applied to entities before storage.
pub trait Preprocessor: Send + Sync {
    fn process(&self, entity: &mut UnifiedEntity);
    fn name(&self) -> &str {
        "unnamed"
    }
}

/// Index configuration for the storage engine.
#[derive(Debug, Clone)]
pub struct IndexConfig {
    pub hnsw_enabled: bool,
    pub hnsw_m: usize,
    pub hnsw_ef_construction: usize,
    pub btree_enabled: bool,
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

// Re-export all public types
pub use batch::{BatchBuilder, BatchResult};
pub use builders::{
    DocumentBuilder, EdgeBuilder, KvBuilder, NodeBuilder, RowBuilder, VectorBuilder,
};
pub use error::DevXError;
pub use helpers::cosine_similarity;
pub use query::{
    ExpandedEntity, MetadataFilter, PropertyFilter, QueryBuilder, QueryResult, QueryResultItem,
};
pub use reddb::{
    NativeHeaderRepairPolicy, NativeVectorArtifactBatchInspection, NativeVectorArtifactInspection,
    PhysicalAuthorityStatus, RedDB,
};
pub use refs::{AnyRef, NodeRef, TableRef, VectorRef};
pub use types::{LinkedEntity, SimilarResult};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_node() {
        let db = RedDB::new();

        let host = db
            .node("hosts", "Host")
            .property("ip", "192.168.1.1")
            .property("os", "Linux")
            .metadata("scan_time", 1234567890i64)
            .save();

        assert!(host.is_ok());
    }

    #[test]
    fn test_create_edge() {
        let db = RedDB::new();

        let host_a = db
            .node("hosts", "Host")
            .property("ip", "10.0.0.1")
            .save()
            .unwrap();
        let host_b = db
            .node("hosts", "Host")
            .property("ip", "10.0.0.2")
            .save()
            .unwrap();

        let edge = db
            .edge("connections", "CONNECTS_TO")
            .from(host_a)
            .to(host_b)
            .weight(0.95)
            .property("protocol", "TCP")
            .save();

        assert!(edge.is_ok());
    }

    #[test]
    fn test_create_vector() {
        let db = RedDB::new();

        let vec = db
            .vector("embeddings")
            .dense(vec![0.1, 0.2, 0.3])
            .content("Test content")
            .metadata("source", "test")
            .save();

        assert!(vec.is_ok());
    }

    #[test]
    fn test_query_builder() {
        let db = RedDB::new();

        // Create some test data
        let _ = db
            .node("hosts", "Host")
            .property("ip", "192.168.1.1")
            .property("os", "Linux")
            .embedding("desc", vec![0.1, 0.2, 0.3])
            .save();

        let results = db
            .query()
            .collection("hosts")
            .where_prop("os", "Linux")
            .limit(10)
            .execute();

        assert!(results.is_ok());
    }

    #[test]
    fn test_table_ref_metadata() {
        let db = RedDB::new();

        let host = db
            .node("hosts", "Host")
            .property("ip", "192.168.1.1")
            .link_to_table("scan_result", db.table_ref("scans", 42))
            .save();

        assert!(host.is_ok());
    }
}
