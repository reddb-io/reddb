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

// ADR 0065 retires these dormant transaction-coordinator scaffolding modules
// from the normal build. The live engine chose optimistic FCW; it does not
// promote the lock/deadlock/savepoint/log coordinator stack.
#[cfg(any())]
pub mod coordinator;
#[cfg(any())]
pub mod lock;
#[cfg(any())]
pub mod log;
#[cfg(any())]
pub mod savepoint;
pub mod snapshot;
pub mod visibility;

pub use reddb_engine::transaction::IsolationLevel;

#[cfg(any())]
pub use coordinator::{Transaction, TransactionManager, TxnConfig, TxnError, TxnHandle, TxnState};
#[cfg(any())]
pub use lock::{LockManager, LockMode, LockResult, LockWaiter};
#[cfg(any())]
pub use log::{LogEntry, LogEntryType, TransactionLog, WalConfig};
#[cfg(any())]
pub use savepoint::{Savepoint, SavepointManager};
pub use snapshot::{Snapshot, SnapshotManager, TxnContext, Xid, XID_NONE};
pub use visibility::is_visible;
