//! MCP (Model Context Protocol) server for RedDB.
//!
//! Exposes RedDB's multi-model storage capabilities to AI agents over
//! the standard MCP JSON-RPC protocol via stdio.

pub mod protocol;
pub mod server;
pub mod tools;
