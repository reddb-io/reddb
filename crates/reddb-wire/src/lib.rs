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

pub mod auth;
pub mod conn_string;
pub mod jsonrpc;
pub mod legacy;
pub mod query_with_params;
pub mod redwire;
pub mod replication;
pub mod sanitizer;
pub mod topology;

pub use conn_string::{
    is_embedded_connection_uri, parse, parse_with_limits, ConnStringLimits, ConnectionTarget,
    ParseError, ParseErrorKind, DEFAULT_PORT_GRPC, DEFAULT_PORT_RED, DEFAULT_PORT_WS,
    DEFAULT_PORT_WSS,
};
pub use redwire::{BuildError, FrameBuilder};
pub use sanitizer::{
    audit_safe_log_field, Boundary, ConnStringSanitizer, EscapeError, EscapedFor, ParsedConnString,
    Tainted, TaintedRef, TaintedTarget,
};
pub use topology::{
    decode_topology, encode_topology, Endpoint, ReplicaInfo, Topology, TopologyError,
    MAX_KNOWN_TOPOLOGY_VERSION, TOPOLOGY_HEADER_SIZE, TOPOLOGY_WIRE_VERSION_V1,
};
