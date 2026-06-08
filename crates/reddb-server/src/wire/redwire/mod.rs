//! RedWire — RedDB's binary TCP / TLS wire protocol with auth
//! handshake, multiplex, compression, and version negotiation.
//!
//! See `.red/adr/0001-redwire-tcp-protocol.md`. The protocol is
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
pub mod input_stream;
pub mod listener;
pub mod output_stream;
pub mod queue_wait;
pub mod session;

#[cfg(unix)]
pub use listener::start_redwire_unix_listener;
pub use listener::{
    start_redwire_listener, start_redwire_listener_on, start_redwire_tls_listener, RedWireConfig,
};

pub use reddb_wire::redwire::{
    decode_frame, encode_frame, BuildError, Flags, Frame, FrameBuilder, FrameError, MessageKind,
    DEFAULT_REDWIRE_PORT, FRAME_HEADER_SIZE, MAX_FRAME_SIZE, MAX_KNOWN_MINOR_VERSION,
    REDWIRE_MAGIC,
};
