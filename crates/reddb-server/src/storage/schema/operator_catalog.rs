//! Re-export shim for the operator catalog.
//!
//! The operator catalog — static table of built-in operator overloads keyed
//! by symbol and operand types — travelled with the coercion spine into the
//! neutral keystone crate [`reddb_types`] (ADR 0052). This shim keeps the
//! historical `storage::schema::operator_catalog` path resolving so existing
//! call-sites stay untouched.

pub use reddb_types::operator_catalog::*;
