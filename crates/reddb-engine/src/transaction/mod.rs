//! Shared live optimistic transaction state; host runtimes own durable publication.

pub mod read;
pub mod snapshot;
pub mod state;
pub mod visibility;

/// Isolation level requested by `BEGIN ISOLATION LEVEL ...`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IsolationLevel {
    /// `READ UNCOMMITTED`
    ReadUncommitted,
    /// `READ COMMITTED`
    ReadCommitted,
    /// `REPEATABLE READ` / `SNAPSHOT`
    #[default]
    SnapshotIsolation,
    /// `SERIALIZABLE`
    Serializable,
}

impl From<reddb_rql::ast::IsolationLevel> for IsolationLevel {
    fn from(level: reddb_rql::ast::IsolationLevel) -> Self {
        match level {
            reddb_rql::ast::IsolationLevel::ReadUncommitted => Self::ReadUncommitted,
            reddb_rql::ast::IsolationLevel::ReadCommitted => Self::ReadCommitted,
            reddb_rql::ast::IsolationLevel::SnapshotIsolation => Self::SnapshotIsolation,
            reddb_rql::ast::IsolationLevel::Serializable => Self::Serializable,
        }
    }
}

pub use snapshot::{Snapshot, SnapshotManager, TxnContext, Xid, XID_NONE};
pub use visibility::is_visible;
