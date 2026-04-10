//! Low-level storage primitives
//!
//! This module contains foundational utilities used by the storage engine:
//! - **IDs**: Type-safe ID wrappers for all storage concepts (nodes, transactions, vectors)
//! - Bloom filters for probabilistic membership testing
//! - Encoding utilities for binary data serialization (varint, zigzag, IP addresses)
//! - Memory-mapped file handling for efficient I/O
//! - Serialization support for structured records
//!
//! These primitives are internal to the storage layer and should not be
//! used directly by external code.

pub mod bloom;
pub mod count_min_sketch;
pub mod cuckoo_filter;
pub mod encoding;
pub mod hyperloglog;
pub mod ids;
#[cfg(unix)]
pub mod mmap;
pub mod serializer;

// Re-export commonly used types
pub use bloom::{BloomFilter, BloomFilterBuilder};
pub use encoding::{
    read_bytes, read_ip, read_string, read_vari32, read_vari64, read_varu32, read_varu64,
    write_bytes, write_ip, write_string, write_vari32, write_vari64, write_varu32, write_varu64,
    DecodeError, IpKey,
};
pub use hyperloglog::HyperLogLog;
pub use ids::{
    current_timestamp, next_btree_node_id, next_timestamp, next_txn_id, BTreeNodeId, EntityId,
    HnswNodeId, PageId, SegmentId, Timestamp, TxnId, VectorId,
};
#[cfg(unix)]
pub use mmap::{MadviseAdvice, MmapFile};
pub use serializer::{Record, Serializer};
