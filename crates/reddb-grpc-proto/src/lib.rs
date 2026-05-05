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
