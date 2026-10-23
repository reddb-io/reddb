//! Storage primitives shared by native and browser runtimes.
//!
//! The native pager uses these exact page, freelist, cache and byte-I/O
//! implementations, including pager orchestration and allocation fences.
//! WAL implementation and encryption remain host/runtime policy.

pub mod btree;
pub mod crc32;
pub mod entity;
pub mod entity_id;
pub mod freelist;
pub mod overflow;
pub mod page;
pub mod page_cache;
pub mod page_store;
pub mod pager;
pub mod pager_io;
