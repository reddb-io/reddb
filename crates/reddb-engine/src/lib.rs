//! Shared RedDB query execution and storage primitives. Pager I/O, transactions
//! and synchronization remain native until their portable boundaries are extracted.

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

mod clock;
pub mod query;
pub mod storage;
pub use reddb_types::{json, serde_json};
