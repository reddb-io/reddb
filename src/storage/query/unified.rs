//! Unified Query Executor
//!
//! Executes parsed RQL queries against both table and graph storage,
//! returning unified results that can contain rows, nodes, edges, and paths.
//!
//! # Architecture
//!
//! The executor is a tree of specialized executors:
//! - `UnifiedExecutor`: Entry point, dispatches to sub-executors
//! - `TableExecutor`: Scans tables, applies filters, sorts
//! - `GraphExecutor`: Matches graph patterns, traverses edges
//! - `JoinExecutor`: Merges table and graph results via GraphTableIndex
//! - `PathExecutor`: Runs graph traversals (BFS/DFS)
//!
//! # Example
//!
//! ```ignore
//! use redblue::storage::query::{parse, UnifiedExecutor, UnifiedResult};
//!
//! let query = parse("FROM hosts h JOIN GRAPH (h)-[:HAS_SERVICE]->(s) RETURN h.ip, s.port")?;
//! let executor = UnifiedExecutor::new(table_store, graph_store, index);
//! let result = executor.execute(&query)?;
//!
//! for record in result.records {
//!     println!("{:?}", record);
//! }
//! ```

mod executor;
mod types;

pub use executor::*;
pub use types::*;

#[cfg(test)]
mod tests;
