//! Shared RedDB query execution, durable page/B-tree storage and live MVCC state.
//! Host runtimes own storage-backed dispatch, durable transaction publication
//! and synchronization.

// Relocated legacy code retains the native crate's lint policy. New modules
// override these allowances so the workspace ratchets apply to new behavior.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]
#![allow(
    clippy::type_complexity,
    clippy::should_implement_trait,
    clippy::new_without_default
)]

#[doc(hidden)]
pub mod catalog;
mod clock;
#[doc(hidden)]
pub mod execution;
#[doc(hidden)]
pub mod geo;
pub mod query;
pub mod storage;
pub mod transaction;
pub use reddb_types::{json, serde_json};
