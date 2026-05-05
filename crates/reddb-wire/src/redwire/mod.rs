//! RedWire — RedDB's binary TCP/TLS wire protocol.
//!
//! ADR 0001 (`docs/adr/0001-redwire-tcp-protocol.md`) is the
//! normative spec. This module owns the *transport-agnostic* parts:
//! frame layout, message-kind discriminator, flags, and the
//! encode/decode codec. Server-side dispatch (auth handshake,
//! session loop, listener accept) stays in `reddb` and depends on
//! these types.

pub mod builder;
pub mod codec;
pub mod frame;

pub use builder::{BuildError, FrameBuilder};
pub use codec::{decode_frame, encode_frame, FrameError};
pub use frame::{Flags, Frame, MessageKind, FRAME_HEADER_SIZE, MAX_FRAME_SIZE};

/// Discriminator byte every RedWire client sends as the very first
/// byte off the wire. The service-router detector keys off this
/// (and so does the standalone listener path).
pub const REDWIRE_MAGIC: u8 = 0xFE;

/// Highest minor version the server supports. Wire-bumped as we
/// add features that change the handshake; data-plane additions
/// flow through `Hello.features` instead.
pub const MAX_KNOWN_MINOR_VERSION: u8 = 0x01;

/// Default port for the RedWire listener.
pub const DEFAULT_REDWIRE_PORT: u16 = 5050;
