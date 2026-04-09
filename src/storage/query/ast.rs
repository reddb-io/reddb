//! Unified Query AST
//!
//! Defines the abstract syntax tree for unified table+graph queries.
//! Supports:
//! - Pure table queries (SELECT ... FROM ...)
//! - Pure graph queries (MATCH (a)-[r]->(b) ...)
//! - Table-graph joins (FROM t JOIN GRAPH ...)
//! - Path queries (PATH FROM ... TO ... VIA ...)
//!
//! # Examples
//!
//! ```text
//! -- Table query
//! SELECT ip, ports FROM hosts WHERE os = 'Linux'
//!
//! -- Graph query
//! MATCH (h:Host)-[:HAS_SERVICE]->(s:Service)
//! WHERE h.ip STARTS WITH '192.168'
//! RETURN h, s
//!
//! -- Join query
//! FROM hosts h
//! JOIN GRAPH (h)-[:HAS_VULN]->(v:Vulnerability) AS g
//! WHERE h.criticality > 7
//! RETURN h.ip, h.hostname, v.cve
//!
//! -- Path query
//! PATH FROM host('192.168.1.1') TO host('10.0.0.1')
//! VIA [:AUTH_ACCESS, :CONNECTS_TO]
//! RETURN path
//! ```

#[path = "builders.rs"]
mod builders;
#[path = "core.rs"]
mod core;

pub use builders::*;
pub use core::*;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
