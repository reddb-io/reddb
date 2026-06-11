//! Re-export shim for the coerce module.
//!
//! The text→value coercion module — `coerce`, `coerce_via_catalog`, and the
//! strict implicit-coercion policy it enforces — was re-homed byte-faithfully
//! into the neutral keystone crate [`reddb_types`] (ADR 0052), where it sits
//! atop the cast catalog and coercion spine. This shim keeps the historical
//! `storage::schema::coerce` path resolving so existing call-sites stay
//! untouched.

pub use reddb_types::coerce::*;
