//! Re-export of the canonical RedWire frame types from `reddb-wire`.
//!
//! The frame layout, `Frame` struct, `MessageKind` enum, `Flags`,
//! and the size constants are owned by the `reddb-wire` crate (see
//! `crates/reddb-wire/src/redwire/frame.rs`). This file keeps the
//! `crate::wire::redwire::frame::…` import paths working for
//! server-side dispatch (auth, session, listener) and for the
//! `reddb::wire::redwire::Frame` re-export consumed by tests and
//! drivers.

pub use reddb_wire::redwire::frame::*;
