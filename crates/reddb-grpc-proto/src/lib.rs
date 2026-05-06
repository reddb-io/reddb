//! Generated gRPC protobuf types for RedDB.
//!
//! This crate exists so `reddb-server` and `reddb-client` can both
//! reference the same tonic-generated client + server stubs without
//! one depending on the other (which would create a cycle).
//! The `.proto` source lives in this crate's `proto/` directory.
//!
//! Re-exports the entire `reddb.v1` module produced by
//! `tonic_prost_build` at compile time.

#![allow(clippy::all)]

tonic::include_proto!("reddb.v1");

// Re-export the canonical Topology Rust type from reddb-wire so
// gRPC consumers reach for one place when they need the schema.
// The on-wire bytes carried in `TopologyReply.topology_bytes` are
// produced by `reddb_wire::topology::encode_topology` — the same
// bytes the RedWire HelloAck embeds base64-wrapped. (Issue #166)
pub use reddb_wire::topology as topology_schema;
pub use reddb_wire::{
    decode_topology, encode_topology, Endpoint as TopologyEndpoint, ReplicaInfo as TopologyReplica,
    Topology, TopologyError, MAX_KNOWN_TOPOLOGY_VERSION, TOPOLOGY_HEADER_SIZE,
    TOPOLOGY_WIRE_VERSION_V1,
};

#[cfg(test)]
mod topology_tests {
    use super::*;

    fn fixture() -> Topology {
        Topology {
            epoch: 7,
            primary: TopologyEndpoint {
                addr: "primary:5050".into(),
                region: "eu-west-1".into(),
            },
            replicas: vec![TopologyReplica {
                addr: "r1:5050".into(),
                region: "eu-west-1".into(),
                healthy: true,
                lag_ms: 4,
                last_applied_lsn: 99,
            }],
        }
    }

    #[test]
    fn topology_round_trip_through_grpc_message() {
        // The acceptance criterion (#166 §5): encode a fixture, ship
        // it as `TopologyReply.topology_bytes`, decode at the other
        // end, assert byte-for-byte equivalence on the inner
        // schema.
        let t = fixture();
        let canonical = encode_topology(&t);
        let reply = TopologyReply {
            topology_bytes: canonical.clone(),
        };
        // Same bytes both transports carry.
        assert_eq!(reply.topology_bytes, canonical);
        let decoded = decode_topology(&reply.topology_bytes)
            .expect("decode")
            .expect("v1 known");
        assert_eq!(decoded, t);
    }

    #[test]
    fn topology_unknown_version_drops_cleanly() {
        let t = fixture();
        let mut bytes = encode_topology(&t);
        bytes[0] = 0x80;
        let reply = TopologyReply {
            topology_bytes: bytes,
        };
        let decoded = decode_topology(&reply.topology_bytes).expect("decode");
        assert!(decoded.is_none());
    }
}
