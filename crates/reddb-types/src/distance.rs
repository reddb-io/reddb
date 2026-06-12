//! Vector distance-metric AST leaf (ADR 0053, RQL Phase 2 S4b).
//!
//! [`DistanceMetric`] is referenced by the canonical SQL AST
//! (`CreateVectorQuery.metric`, `SearchCommand` metric slots). Only the metric
//! *enum* lives here — the SIMD-accelerated distance computations stay in the
//! server engine (`storage::engine::distance`), which keeps a re-export shim.

/// Distance metric types supported by vector operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DistanceMetric {
    /// Euclidean (L2) distance - good for dense vectors
    #[default]
    L2,
    /// Cosine distance - good for normalized embeddings
    Cosine,
    /// Inner product (dot product) - for maximum inner product search
    InnerProduct,
}
