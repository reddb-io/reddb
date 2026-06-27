//! RedDB wire protocol vocabulary.
//!
//! This crate is the shared, transport-agnostic layer that
//! `reddb-server`, `reddb-client`, and the official language
//! drivers depend on. It deliberately has no dependency on the
//! engine, storage, or runtime modules.
//!
//! It owns the shared connection-string parser, audit-safe sanitizers,
//! RedWire frame layout and codec, handshake payloads, topology payloads,
//! query parameter encoding, queue/stream payloads, and replication wire
//! messages. Listener loops, authentication policy, SQL dispatch, and
//! runtime integration stay in `reddb-server`.

#![allow(clippy::unwrap_used)]
// Legacy allow for the cast_possible_truncation ratchet (PRD #1252):
// pre-existing truncating `as` casts on frame lengths/offsets. The lint bites
// on new/changed code; remove once those casts become checked conversions.
#![allow(clippy::cast_possible_truncation)]

pub mod auth;
pub mod conn_string;
pub mod jsonrpc;
pub mod knowledge;
pub mod legacy;
pub mod query_with_params;
pub mod redwire;
pub mod replication;
pub mod sanitizer;
pub mod topology;

pub use conn_string::{
    is_embedded_connection_uri, parse, parse_with_auth, parse_with_limits, ConnStringLimits,
    ConnectionAuth, ConnectionScheme, ConnectionSpec, ConnectionTarget, ParseError, ParseErrorKind,
    DEFAULT_PORT_GRPC, DEFAULT_PORT_GRPCS, DEFAULT_PORT_RED, DEFAULT_PORT_WS, DEFAULT_PORT_WSS,
    SUPPORTED_SCHEMES,
};
pub use knowledge::*;
pub use redwire::{BuildError, FrameBuilder};
pub use sanitizer::{
    audit_safe_log_field, Boundary, ConnStringSanitizer, EscapeError, EscapedFor, ParsedConnString,
    Tainted, TaintedRef, TaintedTarget,
};
pub use topology::{
    decode_topology, encode_topology, Endpoint, ReplicaInfo, Topology, TopologyError,
    MAX_KNOWN_TOPOLOGY_VERSION, TOPOLOGY_HEADER_SIZE, TOPOLOGY_WIRE_VERSION_V1,
};
