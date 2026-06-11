//! Re-export shim for the polymorphic pseudo-types.
//!
//! The polymorphic vocabulary — the `PseudoType` family (`anyelement` /
//! `anyarray` / `anynonarray` / `anycompatible`) and the resolver that
//! instantiates it against concrete call-site arguments — is logical type
//! vocabulary, so it was re-homed byte-faithfully into the neutral keystone
//! crate [`reddb_types`] (ADR 0052). This shim keeps the historical
//! `storage::schema::polymorphic` path resolving so existing call-sites stay
//! untouched.

pub use reddb_types::polymorphic::*;
