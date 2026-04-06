//! RedDB Schema System
//!
//! This module provides a complete schema system for RedDB including:
//! - Type system with primitive and network-specific types
//! - Table definitions with columns, constraints, and indexes
//! - Schema registry for storing and managing table definitions
//!
//! The schema system is designed to support security-focused data types
//! like IP addresses, MAC addresses, and vectors for similarity search.

pub mod registry;
pub mod table;
pub mod types;

// Re-export common types
pub use registry::{SchemaError, SchemaRegistry};
pub use table::{ColumnDef, Constraint, ConstraintType, IndexDef, IndexType, TableDef};
pub use types::{DataType, Row, Value, ValueError};
