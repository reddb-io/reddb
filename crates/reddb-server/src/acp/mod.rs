//! Agent Client Protocol adapter for the RedDB ASK harness.
//!
//! ACP owns session lifecycle and newline-delimited JSON-RPC transport. The
//! reasoning loop and database tools remain owned by [`crate::runtime`], so
//! ASK, MCP, and ACP cannot drift into separate authorization paths.

pub mod protocol;
pub mod server;
