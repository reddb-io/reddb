//! Generated gRPC protobuf types for RedDB.
//!
//! This crate exists so `reddb-server` and `reddb-client` can both
//! reference the same tonic-generated client + server stubs without
//! one depending on the other (which would create a cycle).
//! The `.proto` source lives in this crate's `proto/` directory.
//!
//! Re-exports the entire `reddb.v1` module produced by
//! `tonic_prost_build` at compile time.
//!
//! Import the matching runtime types through this crate's re-export:
//! ```no_run
//! use reddb_grpc_proto::{red_db_client::RedDbClient, tonic::Request, Empty};
//! async fn health(endpoint: String) -> Result<(), Box<dyn std::error::Error>> {
//!     let mut client = RedDbClient::connect(endpoint).await?;
//!     client.health(Request::new(Empty {})).await?;
//!     Ok(())
//! }
//! ```

#![allow(clippy::all)]
// `clippy::all` does NOT include the pedantic `too_many_lines` lint, and the
// tonic-generated service stubs contain functions far over the 120-line
// threshold (PRD #1252 ratchet). This crate is entirely generated, so the
// allow is permanent rather than a clean-up TODO.
#![allow(clippy::too_many_lines)]

tonic::include_proto!("reddb.v1");

/// The exact Tonic runtime used by the generated stubs and their public types.
pub use tonic;

#[cfg(test)]
mod runtime_type_tests {
    #[test]
    fn public_runtime_rejects_metadata_header_injection() {
        for input in [
            "ok\r\nauthorization: injected",
            "ok\nheader: injected",
            "bad\0value",
        ] {
            assert!(super::tonic::metadata::MetadataValue::try_from(input).is_err());
        }
        let value = super::tonic::metadata::MetadataValue::try_from("Bearer example-test-token")
            .expect("valid authorization metadata");
        let mut request = super::tonic::Request::new(super::Empty {});
        request.metadata_mut().insert("authorization", value);
        assert_eq!(
            request
                .metadata()
                .get("authorization")
                .expect("metadata preserved"),
            "Bearer example-test-token"
        );
    }
}

mod client;
pub use client::RedDBClient;

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
                rebootstrapping: false,
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
