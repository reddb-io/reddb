//! `reddb-io-types` — the neutral keystone crate for RedDB's logical type
//! system (ADR 0052).
//!
//! This crate sits at the **bottom of the workspace crate graph**: every
//! authority crate (`reddb-io-file`, `reddb-io-wire`, the planned
//! `reddb-io-rql` and `reddb-io-crypto`) may depend on it, but it depends on
//! **no other workspace crate**. It owns the core type vocabulary —
//! [`Value`], [`DataType`], [`SqlTypeName`], [`TypeModifier`],
//! [`TypeCategory`], [`ValueError`], and [`Row`] — together with the
//! [`value_codec`] serialization that `Value::to_bytes`/`from_bytes` delegate
//! to.
//!
//! The `reddb-server` `storage::schema` module keeps a re-export shim so the
//! ~180 existing call-sites across the workspace stay untouched.

// The byte-faithful re-home (ADR 0052) preserves `types`/`value_codec`
// exactly as they were authored under the server's crate-level
// `#![allow(unused_imports)]`. `types.rs` keeps `std::net::{Ipv4Addr,
// Ipv6Addr}` in its module-level `use` even though the non-test paths
// reference them fully-qualified and the test module re-imports them
// locally; carrying the allow here keeps the move a pure relocation.
#![allow(unused_imports)]

mod conversions;
pub mod types;
pub mod value_codec;

pub use types::{DataType, Row, SqlTypeName, TypeCategory, TypeModifier, Value, ValueError};
