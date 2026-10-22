//! Storage primitives shared by native and browser runtimes.
//!
//! The native pager uses these exact page, freelist and cache types. File I/O,
//! recovery orchestration and MVCC remain native until their portable boundary
//! is extracted; these modules do not constitute a second storage engine.

pub mod crc32;
pub mod freelist;
pub mod page;
pub mod page_cache;
