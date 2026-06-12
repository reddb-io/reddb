//! Re-export shim: the in-house JSON encode/decode stack now lives in
//! `reddb-io-types` (ADR 0053). This module preserves every
//! `crate::serde_json::...` import path — including the `json!` macro
//! re-exported into this namespace — so the 200+ call-sites across the
//! server compile unchanged. The byte layout of `Value` is owned by
//! `reddb_types::serde_json`; nothing about the wire/payload format
//! changed in the move.
pub use reddb_types::serde_json::*;
