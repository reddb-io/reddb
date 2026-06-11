//! Re-export shim for the `Value` byte codec.
//!
//! `Value::to_bytes`/`from_bytes` delegate to this codec, so it travelled with
//! the logical type vocabulary into the neutral keystone crate [`reddb_types`]
//! (ADR 0052). This shim keeps the historical
//! `storage::schema::value_codec` path resolving so existing call-sites stay
//! untouched.

pub use reddb_types::value_codec::*;
