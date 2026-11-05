//! Value Conversions for Ergonomics
//!
//! From implementations for Value and MetadataValue for fluent API.

use super::super::{MetadataValue, RefTarget};
use super::refs::{NodeRef, TableRef, VectorRef};

// ============================================================================
// Value Conversions
// ============================================================================
//
// The `From<primitive> for Value` impls were re-homed to the keystone crate
// `reddb-io-types` alongside `Value` itself (ADR 0052, #1061): with `Value`
// no longer local here, the orphan rule requires those impls to live in
// `Value`'s home crate. See `reddb_types::conversions`.

// ============================================================================
// MetadataValue Conversions
// ============================================================================

impl From<TableRef> for MetadataValue {
    fn from(r: TableRef) -> Self {
        MetadataValue::Reference(RefTarget::table(r.table, r.row_id))
    }
}

impl From<NodeRef> for MetadataValue {
    fn from(r: NodeRef) -> Self {
        MetadataValue::Reference(RefTarget::node(r.collection, r.node_id))
    }
}

impl From<VectorRef> for MetadataValue {
    fn from(r: VectorRef) -> Self {
        MetadataValue::Reference(RefTarget::vector(r.collection, r.vector_id))
    }
}
