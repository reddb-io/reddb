//! Memory-mapped I/O
//!
//! Re-exports from `storage::primitives::mmap` (Unix only).

#[cfg(unix)]
pub use crate::storage::primitives::mmap::*;
