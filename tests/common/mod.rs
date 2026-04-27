//! Shared helpers for OAuth-JWT integration tests.
//!
//! Lives under `tests/common/` so that integration test crates
//! (`tests/redwire_oauth_e2e.rs`, `tests/oauth_jwks_server.rs`,
//! and any future agent-A / agent-B HTTP/gRPC OAuth smokes)
//! can `mod common;` and pull only what they need.

#![allow(dead_code)]

pub mod jwks_server;
pub mod jwt_mint;
