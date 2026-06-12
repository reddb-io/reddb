//! Cross-type `Value` comparison.
//!
//! Re-homed to the neutral keystone crate (ADR 0053, RQL Phase 2 S4b) as the
//! minimal transitive closure that lets the `vector_metadata` AST leaves keep
//! their inherent comparison methods without a `reddb-server` edge. This shim
//! keeps `storage::query::value_compare::{partial_compare_values,
//! total_compare_values}` valid for existing call-sites.
pub(crate) use reddb_types::value_compare::{partial_compare_values, total_compare_values};
