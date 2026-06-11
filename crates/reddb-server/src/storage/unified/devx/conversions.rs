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

impl From<&str> for MetadataValue {
    fn from(s: &str) -> Self {
        MetadataValue::String(s.to_string())
    }
}

impl From<String> for MetadataValue {
    fn from(s: String) -> Self {
        MetadataValue::String(s)
    }
}

impl From<i64> for MetadataValue {
    fn from(n: i64) -> Self {
        MetadataValue::Int(n)
    }
}

impl From<f64> for MetadataValue {
    fn from(n: f64) -> Self {
        MetadataValue::Float(n)
    }
}

impl From<bool> for MetadataValue {
    fn from(b: bool) -> Self {
        MetadataValue::Bool(b)
    }
}

impl From<RefTarget> for MetadataValue {
    fn from(r: RefTarget) -> Self {
        MetadataValue::Reference(r)
    }
}

impl From<Vec<RefTarget>> for MetadataValue {
    fn from(refs: Vec<RefTarget>) -> Self {
        MetadataValue::References(refs)
    }
}

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
