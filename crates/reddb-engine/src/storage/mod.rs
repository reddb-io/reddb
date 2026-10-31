//! Storage primitives shared by native and browser runtimes.
//!
//! The native pager uses these exact page, freelist, cache and byte-I/O
//! implementations, including pager orchestration and allocation fences.
//! WAL implementation and encryption remain host/runtime policy.

pub mod btree;
pub mod crc32;
pub mod entity;
pub mod entity_codec;
pub mod entity_id;
pub mod freelist;
pub mod overflow;
pub mod page;
pub mod page_cache;
pub mod page_store;
pub mod pager;
pub mod pager_io;

// Canonical resident collection storage used by native and browser hosts.
#[doc(hidden)]
pub mod memory_size;
pub mod metadata;
pub mod segment;
mod segment_graph_index;
#[doc(hidden)]
pub mod segment_key_lookup;
mod segment_unique_key;

pub mod metadata_codec;
pub mod snapshot;

pub mod manager;
pub mod visibility;
pub mod visibility_map;

pub mod context_index;
pub mod entity_cache;
pub mod tokenization;

mod collection_key_read;
#[doc(hidden)]
pub mod collection_read;
#[doc(hidden)]
pub mod resident_store;

pub mod error;
#[doc(hidden)]
pub mod store_wal;

mod resident_snapshot;

#[doc(hidden)]
pub mod resident_file;
