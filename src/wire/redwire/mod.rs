//! RedWire — RedDB's binary TCP / TLS wire protocol with auth
//! handshake, multiplex, compression, and version negotiation.
//!
//! See `docs/adr/0001-redwire-tcp-protocol.md`. The protocol is
//! gated on a `0xFE` startup magic byte so the listener can share
//! a port with HTTP and gRPC behind the service router.
//!
//! Layered API:
//!   - `frame`  — frame struct + MessageKind + flags
//!   - `codec`  — encode/decode (16-byte header + payload)
//!   - `auth`   — handshake state machine
//!   - `session` — per-connection dispatch loop
//!   - `listener` — TCP / TLS / Unix accept

pub mod auth;
pub mod codec;
pub mod frame;
pub mod listener;
pub mod session;

pub use codec::{decode_frame, encode_frame, FrameError};
pub use frame::{Flags, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_FRAME_SIZE};
#[cfg(unix)]
pub use listener::start_redwire_unix_listener;
pub use listener::{
    start_redwire_listener, start_redwire_listener_on, start_redwire_tls_listener, RedWireConfig,
};

// Constants live in the shared `reddb-wire` crate; re-exported here
// so existing `crate::wire::redwire::REDWIRE_MAGIC` paths continue
// to resolve.
pub use reddb_wire::redwire::{DEFAULT_REDWIRE_PORT, MAX_KNOWN_MINOR_VERSION, REDWIRE_MAGIC};
