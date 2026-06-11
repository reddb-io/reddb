//! Re-export shim for the cast catalog.
//!
//! The cast catalog — PostgreSQL `pg_cast`-style static table of allowed
//! conversions and the `find_cast` / `can_implicit_cast` / `can_explicit_cast`
//! lookups — was re-homed byte-faithfully into the neutral keystone crate
//! [`reddb_types`] (ADR 0052), where it anchors the coercion spine. This shim
//! keeps the historical `storage::schema::cast_catalog` path resolving so
//! existing call-sites stay untouched.

pub use reddb_types::cast_catalog::*;
