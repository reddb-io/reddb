//! Transaction Management
//!
//! Provides ACID transaction guarantees with WAL-based durability.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    Transaction Manager                           │
//! ├─────────────────────────────────────────────────────────────────┤
//! │  begin() → TxnHandle                                            │
//! │  commit(TxnHandle) → Result<(), Error>                         │
//! │  abort(TxnHandle)                                               │
//! └─────────────────────────────────────────────────────────────────┘
//!                              │
//!          ┌──────────────────┼──────────────────┐
//!          ▼                  ▼                  ▼
//!     ┌─────────┐       ┌─────────┐       ┌─────────┐
//!     │   WAL   │       │  MVCC   │       │  Lock   │
//!     │  Logger │       │  Store  │       │ Manager │
//!     └─────────┘       └─────────┘       └─────────┘
//! ```
//!
//! # Isolation Levels
//!
//! - **Read Committed**: See committed changes from other transactions
//! - **Snapshot Isolation**: See consistent snapshot from transaction start
//! - **Serializable**: Full serializability (not yet implemented)

pub mod coordinator;
pub mod lock;
pub mod log;
pub mod savepoint;
pub mod snapshot;

pub use coordinator::{
    IsolationLevel, Transaction, TransactionManager, TxnConfig, TxnError, TxnHandle, TxnState,
};
pub use lock::{LockManager, LockMode, LockResult, LockWaiter};
pub use log::{LogEntry, LogEntryType, TransactionLog, WalConfig};
pub use savepoint::{Savepoint, SavepointManager};
pub use snapshot::{Snapshot, SnapshotManager, TxnContext, Xid, XID_NONE};
