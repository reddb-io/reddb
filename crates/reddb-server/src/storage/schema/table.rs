//! Compatibility re-export for the logical table-definition vocabulary.
//!
//! The declarations live in the neutral `reddb-types` keystone crate. Keep
//! this module so existing server paths remain source-compatible.

pub use reddb_types::table::*;
