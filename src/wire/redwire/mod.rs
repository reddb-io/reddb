//! RedWire v2 — extends the existing wire protocol with auth
//! handshake, multiplex, compression, and version negotiation.
//!
//! See `docs/adr/0001-redwire-tcp-protocol.md`. v1 listener at
//! `src/wire/listener.rs` keeps working unchanged on the same
//! port; v2 is gated on a `0xFE` startup byte so v1 clients are
//! never affected.
//!
//! Layered API:
//!   - `frame`  — frame struct + MessageKind + flags
//!   - `codec`  — encode/decode (16-byte header + payload)
//!   - `auth`   — handshake state machine
//!   - `session` — per-connection dispatch loop
//!   - `listener` — TCP accept

pub mod auth;
pub mod codec;
pub mod frame;
pub mod listener;
pub mod session;

pub use codec::{decode_frame, encode_frame, FrameError};
pub use frame::{Flags, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_FRAME_SIZE};
pub use listener::{start_redwire_listener, RedWireConfig};

/// Discriminator byte the v2 client sends as the very first byte
/// off the wire. v1 clients never set this byte (their first byte
/// is the low byte of a u32 length field, which is well below
/// 0xFE for any reasonable frame size). Service-router detector
/// keys off this.
pub const REDWIRE_V2_MAGIC: u8 = 0xFE;

/// Highest minor version the server supports. Wire-bumped as we
/// add features that change the handshake; data-plane additions
/// flow through `Hello.features` instead.
pub const MAX_KNOWN_MINOR_VERSION: u8 = 0x01;

/// Default port for the RedWire listener (matches v1; both share it
/// via the service-router detector).
pub const DEFAULT_REDWIRE_PORT: u16 = 5050;
