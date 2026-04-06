//! Query DSL for Multi-Modal Queries
//!
//! Provides a fluent, chainable API for querying across Tables, Graphs, and Vectors
//! in the Unified Storage Layer. Designed for maximum developer experience (DevX).
//!
//! # Design Principles
//!
//! 1. **Chainable**: Every method returns `self` for fluent chaining
//! 2. **Type-Safe**: Compile-time checks for query structure
//! 3. **Expressive**: Read like natural language
//! 4. **Multi-Modal**: Seamlessly combine vector, graph, and table operations
//!
//! # Examples
//!
//! ```ignore
//! use redblue::storage::dsl::*;
//!
//! // Vector similarity + metadata filter
//! let results = Q::similar_to(&embedding, 10)
//!     .in_collection("vulnerabilities")
//!     .where_("severity").equals("critical")
//!     .where_("cvss").greater_than(8.0)
//!     .execute(&store)?;
//!
//! // Graph traversal + vector ranking
//! let paths = Q::from_node("web-server-1")
//!     .traverse("CONNECTS_TO")
//!     .depth(3)
//!     .ranked_by(&attack_vector_embedding)
//!     .execute(&store)?;
//!
//! // Combined: find similar CVEs, expand to affected hosts
//! let context = Q::similar_to(&cve_embedding, 5)
//!     .expand_via(RefType::VectorToNode)
//!     .expand_via(RefType::NodeToRow)
//!     .with_weights(vector: 0.6, graph: 0.3, table: 0.1)
//!     .execute(&store)?;
//! ```

mod builders;
mod execution;
mod filters;
mod helpers;
mod types;

#[cfg(test)]
mod tests;

// Re-exports
pub use builders::TextSearchBuilder;
pub use builders::{
    CrossModalWeights, GraphPatternDsl, GraphQueryBuilder, GraphStartPoint, HybridQueryBuilder,
    JoinPhase, JoinStep, NodePatternDsl, QueryWeights, RefQueryBuilder, ScanQueryBuilder,
    SortOrder, TableQueryBuilder, ThreeWayJoinBuilder, TraversalDirection, TraversalStep,
    VectorQueryBuilder,
};
pub use filters::{Filter, FilterAcceptor, FilterOp, FilterValue, WhereClause};
pub use helpers::cosine_similarity;
pub use types::{MatchComponents, QueryResult, ScoredMatch};

use super::entity::{EntityId, RefType};

// ============================================================================
// Query Builder Entry Point
// ============================================================================

/// Query builder entry point. Start all queries with `Q::`.
///
/// # Examples
///
/// ```ignore
/// // Vector similarity search
/// Q::similar_to(&embedding, 10)
///
/// // Start from a graph node
/// Q::from_node("node-label")
///
/// // Filter table rows
/// Q::table("hosts").where_("status").equals("active")
///
/// // Full collection scan
/// Q::all_in("vulnerabilities")
/// ```
pub struct Q;

impl Q {
    /// Start a vector similarity query
    pub fn similar_to(vector: &[f32], k: usize) -> VectorQueryBuilder {
        VectorQueryBuilder::new(vector.to_vec(), k)
    }

    /// Start a query from a specific graph node
    pub fn from_node(label: impl Into<String>) -> GraphQueryBuilder {
        GraphQueryBuilder::from_node(label)
    }

    /// Start a query from a specific entity by ID
    pub fn from_id(id: EntityId) -> GraphQueryBuilder {
        GraphQueryBuilder::from_id(id)
    }

    /// Query a specific table/collection
    pub fn table(name: impl Into<String>) -> TableQueryBuilder {
        TableQueryBuilder::new(name)
    }

    /// Shorthand for collection query
    pub fn collection(name: impl Into<String>) -> TableQueryBuilder {
        TableQueryBuilder::new(name)
    }

    /// Query all entities in a collection
    pub fn all_in(collection: impl Into<String>) -> ScanQueryBuilder {
        ScanQueryBuilder::new(collection)
    }

    /// Find entities by cross-reference
    pub fn refs_from(id: EntityId, ref_type: RefType) -> RefQueryBuilder {
        RefQueryBuilder::new(id, ref_type)
    }

    /// Text search across all indexed content
    pub fn text_search(query: impl Into<String>) -> TextSearchBuilder {
        TextSearchBuilder::new(query)
    }

    /// Hybrid query combining multiple modes
    pub fn hybrid() -> HybridQueryBuilder {
        HybridQueryBuilder::new()
    }

    /// Three-way cross-modal JOIN
    ///
    /// Efficiently chains queries across Vector → Graph → Table (or any order).
    ///
    /// # Example
    /// ```ignore
    /// // Find CVEs similar to vector, traverse to affected hosts, get host records
    /// let results = Q::cross_modal()
    ///     .start_vector(&cve_embedding, 10)
    ///     .traverse("AFFECTS", 2)
    ///     .join_table("hosts")
    ///     .execute(&store)?;
    /// ```
    pub fn cross_modal() -> ThreeWayJoinBuilder {
        ThreeWayJoinBuilder::new()
    }
}
