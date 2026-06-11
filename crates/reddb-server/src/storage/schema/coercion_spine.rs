//! Re-export shim for the coercion spine.
//!
//! The coercion spine — the single owner of operator / function overload
//! resolution and the implicit casts the engine must insert — was re-homed
//! byte-faithfully into the neutral keystone crate [`reddb_types`] (ADR 0052).
//! Resolving the spine's one external dependency (the query AST's `BinOp`)
//! moved the operator vocabulary there too; see [`reddb_types::operator`].
//! This shim keeps the historical `storage::schema::coercion_spine` path
//! resolving so existing call-sites stay untouched.

pub use reddb_types::coercion_spine::*;
