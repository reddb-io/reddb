//! Re-export shim for the parametric type validators.
//!
//! The parametric validators — `validate_varchar` / `validate_decimal` and the
//! `VARCHAR(n)` / `DECIMAL(p,s)` modifier parsers — are logical type
//! vocabulary, so they were re-homed byte-faithfully into the neutral keystone
//! crate [`reddb_types`] (ADR 0052). This shim keeps the historical
//! `storage::schema::parametric` path resolving so existing call-sites stay
//! untouched.

pub use reddb_types::parametric::*;
