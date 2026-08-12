//! Optimizer index-hint AST leaf (ADR 0053, RQL Phase 2 S4b).
//!
//! [`IndexHint`] is referenced by the canonical SQL AST
//! (`ExpandOptions.index_hint`). Only the hint *data* lives here; the optimizer
//! passes that build it live in `reddb-rql`'s storage-agnostic optimizer.

/// Hint about which index method to prefer for a query
#[derive(Debug, Clone)]
pub struct IndexHint {
    /// Preferred index method
    pub method: IndexHintMethod,
    /// Column the index applies to
    pub column: String,
}

/// Which index method the optimizer recommends
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexHintMethod {
    Hash,
    BTree,
    Bitmap,
    Spatial,
}
