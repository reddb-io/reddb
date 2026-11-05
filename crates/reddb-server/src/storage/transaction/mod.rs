//! Live transaction snapshot and visibility support.
//!
//! The live engine is optimistic MVCC: `SnapshotManager` allocates xids,
//! `visibility` decides snapshot reads, and commit-time first-committer-wins
//! checks live in the runtime.
//!
//! # Isolation Levels
//!
//! - **Read Committed**: See committed changes from other transactions
//! - **Snapshot Isolation**: See consistent snapshot from transaction start
//! - **Serializable**: Serializable read/write conflict tracking with runtime commit validation

pub mod snapshot;
pub mod visibility;

pub use reddb_engine::transaction::IsolationLevel;

pub use snapshot::{Snapshot, SnapshotManager, TxnContext, Xid, XID_NONE};
pub use visibility::is_visible;
