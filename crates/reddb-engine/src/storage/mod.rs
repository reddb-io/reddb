//! Storage primitives shared by native and browser runtimes.
//!
//! The native pager uses these exact page, freelist, cache and byte-I/O
//! implementations. WAL-first policy, allocation fences, encryption and MVCC
//! remain native; these modules do not constitute a second storage engine.

pub mod crc32;
pub mod freelist;
pub mod page;
pub mod page_cache;
pub mod pager_io;
