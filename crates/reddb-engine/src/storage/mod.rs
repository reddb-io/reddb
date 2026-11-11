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
#[doc(hidden)]
pub mod paged_file;
pub mod pager;
pub mod pager_io;

// Canonical resident collection storage used by native and browser hosts.
#[cfg(feature = "full")]
#[doc(hidden)]
pub mod memory_size;
pub mod metadata;
#[cfg(feature = "full")]
pub mod segment;
#[cfg(feature = "full")]
mod segment_graph_index;
#[cfg(feature = "full")]
#[doc(hidden)]
pub mod segment_key_lookup;
#[cfg(feature = "full")]
mod segment_unique_key;

pub mod metadata_codec;
#[cfg(feature = "full")]
pub mod snapshot;
#[doc(hidden)]
pub mod system_collection_names;

#[cfg(feature = "full")]
pub mod manager;
#[cfg(feature = "full")]
pub mod visibility;
#[cfg(feature = "full")]
pub mod visibility_map;

#[cfg(feature = "full")]
pub mod context_index;
#[cfg(feature = "full")]
pub mod entity_cache;
#[cfg(feature = "full")]
pub mod tokenization;

mod collection_key_read;
#[doc(hidden)]
pub mod collection_read;
#[cfg(feature = "full")]
pub mod ordered_secondary_key;
#[doc(hidden)]
#[cfg(feature = "full")]
pub mod resident_store;
#[cfg(feature = "full")]
pub mod secondary_index_read;

pub mod error;
#[doc(hidden)]
pub mod store_wal;

#[cfg(feature = "full")]
mod resident_snapshot;

#[doc(hidden)]
#[cfg(feature = "full")]
pub mod resident_file;
