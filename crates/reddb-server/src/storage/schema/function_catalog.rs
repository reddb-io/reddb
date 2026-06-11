//! Re-export shim for the function catalog.
//!
//! The function catalog — static table of built-in scalar / aggregate
//! signatures — is mutually recursive with the coercion spine
//! (`function_catalog::resolve` delegates to `coercion_spine::resolve_function`),
//! so it travelled with the spine into the neutral keystone crate
//! [`reddb_types`] (ADR 0052). This shim keeps the historical
//! `storage::schema::function_catalog` path resolving so existing call-sites
//! stay untouched.

pub use reddb_types::function_catalog::*;
