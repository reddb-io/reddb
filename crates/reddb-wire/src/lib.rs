//! RedDB wire protocol vocabulary.
//!
//! This crate is the shared, transport-agnostic layer that
//! `reddb-server`, `reddb-client`, and the official language
//! drivers depend on. It deliberately has no dependency on the
//! engine, storage, or runtime modules.
//!
//! Today it exposes the [`conn_string`] connection-string parser.
//! Future slices will add the RedWire frame layout, header types,
//! and framing codec (see ADR 0001 in `docs/adr/`).

pub mod conn_string;
pub mod redwire;
pub mod topology;

pub use conn_string::{
    parse, parse_with_limits, ConnStringLimits, ConnectionTarget, ParseError, ParseErrorKind,
};
pub use redwire::{BuildError, FrameBuilder};
pub use topology::{
    decode_topology, encode_topology, Endpoint, ReplicaInfo, Topology, TopologyError,
    MAX_KNOWN_TOPOLOGY_VERSION, TOPOLOGY_HEADER_SIZE, TOPOLOGY_WIRE_VERSION_V1,
};
