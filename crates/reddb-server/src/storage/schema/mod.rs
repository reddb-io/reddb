//! RedDB Schema System
//!
//! This module provides a complete schema system for RedDB including:
//! - Type system with primitive and network-specific types
//! - Table definitions with columns, constraints, and indexes
//! - Schema registry for storing and managing table definitions
//!
//! The schema system is designed to support security-focused data types
//! like IP addresses, MAC addresses, and vectors for similarity search.

pub mod row_slot;

// Re-export common types
pub use reddb_types::canonical_key::{value_to_canonical_key, CanonicalKey, CanonicalKeyFamily};
pub use reddb_types::coerce::coerce;
pub use reddb_types::table::{ColumnDef, Constraint, ConstraintType, IndexDef, IndexType, TableDef};
pub use reddb_types::types::{
    decimal_to_f64, DataType, Row, SqlTypeName, TypeModifier, Value, ValueError, DECIMAL_SCALE,
};
