//! Re-export of the canonical RedWire codec from `reddb-wire`.
//!
//! See `crates/reddb-wire/src/redwire/codec.rs` for the encode /
//! decode implementation and unit tests. This file keeps existing
//! `crate::wire::redwire::codec::…` import paths working.

pub use reddb_wire::redwire::codec::*;
