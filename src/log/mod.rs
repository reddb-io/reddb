//! High-performance append-only log collections.
//!
//! LOG tables are optimized for high-throughput sequential writes with
//! time-ordered IDs and automatic retention.

pub mod id;
pub mod store;

pub use id::LogId;
pub use store::{LogCollection, LogCollectionConfig, LogEntry, LogRetention};
