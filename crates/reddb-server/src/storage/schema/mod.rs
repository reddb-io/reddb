//! RedDB Schema System
//!
//! This module provides a complete schema system for RedDB including:
//! - Type system with primitive and network-specific types
//! - Table definitions with columns, constraints, and indexes
//! - Schema registry for storing and managing table definitions
//!
//! The schema system is designed to support security-focused data types
//! like IP addresses, MAC addresses, and vectors for similarity search.

pub mod canonical_key;
pub mod cast_catalog;
pub mod coerce;
pub mod function_catalog;
pub mod operator_catalog;
pub mod parametric;
pub mod polymorphic;
pub mod registry;
pub mod row_slot;
pub mod table;
pub mod types;

// Re-export common types
pub use canonical_key::{value_to_canonical_key, CanonicalKey, CanonicalKeyFamily};
pub use coerce::coerce;
pub use registry::{SchemaError, SchemaRegistry};
pub use table::{ColumnDef, Constraint, ConstraintType, IndexDef, IndexType, TableDef};
pub use types::{DataType, Row, SqlTypeName, TypeModifier, Value, ValueError};
