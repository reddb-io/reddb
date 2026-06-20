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
#![allow(clippy::unwrap_used)]
#![allow(unused_imports)]
// Legacy allow for the too_many_lines ratchet (PRD #1252): pre-existing
// codec/type functions exceed the 120-line threshold. The lint bites on
// new/changed code; remove once those functions are split up.
#![allow(clippy::too_many_lines)]

pub mod canonical_key;
pub mod cast_catalog;
pub mod catalog;
pub mod coerce;
pub mod coercion_spine;
mod conversions;
pub mod distance;
pub mod duration;
pub mod function_catalog;
pub mod index_hint;
pub mod json;
pub mod operator;
pub mod operator_catalog;
pub mod parametric;
pub mod polymorphic;
pub mod queue_mode;
pub mod serde_json;
pub mod types;
pub mod utils;
pub mod value_codec;
pub mod value_compare;
pub mod vector_metadata;

pub use canonical_key::{value_to_canonical_key, CanonicalKey, CanonicalKeyFamily};
pub use operator::BinOp;
pub use types::{DataType, Row, SqlTypeName, TypeCategory, TypeModifier, Value, ValueError};
