//! Per-connection transaction state and lifecycle transitions.
//!
//! This module owns the connection-to-transaction and transaction-local
//! tenant maps. Runtime dispatch supplies commit-time validation and pending
//! write finalization, while transaction and savepoint state changes stay
//! behind this interface.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::api::{RedDBError, RedDBResult};
use crate::storage::transaction::snapshot::{Snapshot, SnapshotManager, TxnContext, Xid};
use crate::storage::transaction::IsolationLevel;

pub(crate) struct RollbackToSavepoint {
    pub(crate) savepoint_xid: Xid,
    pub(crate) aborted_xids: Vec<Xid>,
}

pub(crate) struct TransactionState {
    snapshot_manager: Arc<SnapshotManager>,
    contexts: RwLock<HashMap<u64, TxnContext>>,
    local_tenants: RwLock<HashMap<u64, Option<String>>>,
}

impl TransactionState {
    pub(crate) fn new() -> Self {
        Self {
            snapshot_manager: Arc::new(SnapshotManager::new()),
            contexts: RwLock::new(HashMap::new()),
            local_tenants: RwLock::new(HashMap::new()),
        }
    }

    pub(crate) fn begin(&self, connection_id: u64, isolation: IsolationLevel) -> Xid {
        let xid = self.snapshot_manager.begin();
        if isolation == IsolationLevel::Serializable {
            self.snapshot_manager.begin_serializable(xid);
        }
        let context = TxnContext {
            xid,
            isolation,
            snapshot: self.snapshot_manager.snapshot(xid),
            savepoints: Vec::new(),
            released_sub_xids: Vec::new(),
        };
        self.contexts.write().insert(connection_id, context);
        xid
    }

    pub(crate) fn commit<E>(
        &self,
        connection_id: u64,
        prepare: impl FnOnce(&TxnContext) -> Result<(), E>,
    ) -> Result<Option<TxnContext>, E> {
        self.local_tenants.write().remove(&connection_id);
        let Some(context) = self.contexts.write().remove(&connection_id) else {
            return Ok(None);
        };

        if let Err(error) = prepare(&context) {
            self.rollback_context(&context);
            return Err(error);
        }

        for (_, xid) in &context.savepoints {
            self.snapshot_manager.commit(*xid);
        }
        for xid in &context.released_sub_xids {
            self.snapshot_manager.commit(*xid);
        }
        self.snapshot_manager.commit(context.xid);
        Ok(Some(context))
    }

    pub(crate) fn rollback(&self, connection_id: u64) -> Option<TxnContext> {
        self.local_tenants.write().remove(&connection_id);
        let context = self.contexts.write().remove(&connection_id)?;
        self.rollback_context(&context);
        Some(context)
    }

    fn rollback_context(&self, context: &TxnContext) {
        for (_, xid) in &context.savepoints {
            self.snapshot_manager.rollback(*xid);
        }
        for xid in &context.released_sub_xids {
            self.snapshot_manager.rollback(*xid);
        }
        self.snapshot_manager.rollback(context.xid);
    }

    pub(crate) fn savepoint(&self, connection_id: u64, name: &str) -> Option<Xid> {
        let mut contexts = self.contexts.write();
        let context = contexts.get_mut(&connection_id)?;
        let xid = self.snapshot_manager.begin();
        context.savepoints.push((name.to_string(), xid));
        Some(xid)
    }

    pub(crate) fn release_savepoint(
        &self,
        connection_id: u64,
        name: &str,
    ) -> RedDBResult<Option<usize>> {
        let mut contexts = self.contexts.write();
        let Some(context) = contexts.get_mut(&connection_id) else {
            return Ok(None);
        };
        let position = context
            .savepoints
            .iter()
            .position(|(savepoint_name, _)| savepoint_name == name)
            .ok_or_else(|| RedDBError::Internal(format!("savepoint {name} does not exist")))?;
        let released = context.savepoints.len() - position;
        context.released_sub_xids.extend(
            context
                .savepoints
                .split_off(position)
                .into_iter()
                .map(|(_, xid)| xid),
        );
        Ok(Some(released))
    }

    pub(crate) fn rollback_to_savepoint(
        &self,
        connection_id: u64,
        name: &str,
    ) -> RedDBResult<Option<RollbackToSavepoint>> {
        let mut contexts = self.contexts.write();
        let Some(context) = contexts.get_mut(&connection_id) else {
            return Ok(None);
        };
        let position = context
            .savepoints
            .iter()
            .position(|(savepoint_name, _)| savepoint_name == name)
            .ok_or_else(|| RedDBError::Internal(format!("savepoint {name} does not exist")))?;
        let savepoint_xid = context.savepoints[position].1;
        let aborted_xids = context
            .savepoints
            .split_off(position)
            .into_iter()
            .map(|(_, xid)| xid)
            .collect::<Vec<_>>();
        drop(contexts);

        for xid in &aborted_xids {
            self.snapshot_manager.rollback(*xid);
        }
        Ok(Some(RollbackToSavepoint {
            savepoint_xid,
            aborted_xids,
        }))
    }

    pub(crate) fn context(&self, connection_id: u64) -> Option<TxnContext> {
        self.contexts.read().get(&connection_id).cloned()
    }

    pub(crate) fn in_transaction(&self, connection_id: u64) -> bool {
        self.contexts.read().contains_key(&connection_id)
    }

    pub(crate) fn current_snapshot(&self, connection_id: u64) -> Snapshot {
        if let Some(context) = self.context(connection_id) {
            if context.isolation == IsolationLevel::ReadCommitted {
                let high_water = self.snapshot_manager.peek_next_xid();
                return self.snapshot_manager.snapshot(high_water);
            }
            return context.snapshot;
        }
        let high_water = self.snapshot_manager.peek_next_xid();
        self.snapshot_manager.snapshot(high_water)
    }

    pub(crate) fn writer_xid(&self, connection_id: u64) -> Option<Xid> {
        self.context(connection_id)
            .map(|context| context.writer_xid())
    }

    pub(crate) fn own_xids(&self, connection_id: u64) -> HashSet<Xid> {
        let mut xids = HashSet::new();
        if let Some(context) = self.context(connection_id) {
            xids.insert(context.xid);
            xids.extend(context.savepoints.into_iter().map(|(_, xid)| xid));
            xids.extend(context.released_sub_xids);
        }
        xids
    }

    pub(crate) fn set_local_tenant(&self, connection_id: u64, tenant: Option<String>) {
        self.local_tenants.write().insert(connection_id, tenant);
    }

    pub(crate) fn local_tenant(&self, connection_id: u64) -> Option<Option<String>> {
        self.local_tenants.read().get(&connection_id).cloned()
    }

    pub(crate) fn snapshot_manager(&self) -> Arc<SnapshotManager> {
        Arc::clone(&self.snapshot_manager)
    }
}

#[cfg(test)]
mod tests {
    use crate::storage::transaction::IsolationLevel;

    use super::TransactionState;

    #[test]
    fn begin_commit_and_rollback_update_snapshot_state() {
        let state = TransactionState::new();

        let committed_xid = state.begin(11, IsolationLevel::SnapshotIsolation);
        assert!(state.in_transaction(11));
        assert_eq!(state.writer_xid(11), Some(committed_xid));
        assert!(state.snapshot_manager().is_active(committed_xid));

        let committed = state
            .commit(11, |_| Ok::<_, ()>(()))
            .expect("commit preparation should succeed")
            .expect("connection should have an active transaction");
        assert_eq!(committed.xid, committed_xid);
        assert!(!state.in_transaction(11));
        assert!(!state.snapshot_manager().is_active(committed_xid));
        assert!(!state.snapshot_manager().is_aborted(committed_xid));

        let aborted_xid = state.begin(11, IsolationLevel::ReadCommitted);
        let aborted = state
            .rollback(11)
            .expect("connection should have an active transaction");
        assert_eq!(aborted.xid, aborted_xid);
        assert!(!state.in_transaction(11));
        assert!(state.snapshot_manager().is_aborted(aborted_xid));
    }

    #[test]
    fn savepoint_release_and_rollback_follow_nested_sequence() {
        let state = TransactionState::new();
        let parent_xid = state.begin(21, IsolationLevel::SnapshotIsolation);
        let outer_xid = state
            .savepoint(21, "outer")
            .expect("transaction should accept a savepoint");
        let released_xid = state
            .savepoint(21, "released")
            .expect("transaction should accept a nested savepoint");

        assert_eq!(
            state
                .release_savepoint(21, "released")
                .expect("savepoint should exist"),
            Some(1)
        );
        assert_eq!(state.writer_xid(21), Some(outer_xid));

        let nested_xid = state
            .savepoint(21, "nested")
            .expect("transaction should accept another nested savepoint");
        let rolled_back = state
            .rollback_to_savepoint(21, "outer")
            .expect("savepoint should exist")
            .expect("connection should have an active transaction");
        assert_eq!(rolled_back.savepoint_xid, outer_xid);
        assert_eq!(rolled_back.aborted_xids, vec![outer_xid, nested_xid]);
        assert_eq!(state.writer_xid(21), Some(parent_xid));
        assert!(state.snapshot_manager().is_aborted(outer_xid));
        assert!(state.snapshot_manager().is_aborted(nested_xid));

        state
            .commit(21, |_| Ok::<_, ()>(()))
            .expect("commit preparation should succeed")
            .expect("connection should have an active transaction");
        assert!(!state.snapshot_manager().is_active(parent_xid));
        assert!(!state.snapshot_manager().is_active(released_xid));
        assert!(!state.snapshot_manager().is_aborted(released_xid));
    }

    #[test]
    fn connections_keep_transaction_and_local_tenant_state_isolated() {
        let state = TransactionState::new();
        let first_xid = state.begin(31, IsolationLevel::SnapshotIsolation);
        let second_xid = state.begin(32, IsolationLevel::ReadCommitted);
        let first_savepoint = state
            .savepoint(31, "first_only")
            .expect("first connection should have an active transaction");

        assert_eq!(state.writer_xid(31), Some(first_savepoint));
        assert_eq!(state.writer_xid(32), Some(second_xid));
        assert_eq!(state.context(31).expect("first context").xid, first_xid);
        assert_eq!(state.context(32).expect("second context").xid, second_xid);

        state.set_local_tenant(31, Some("tenant-a".to_string()));
        state.set_local_tenant(32, Some("tenant-b".to_string()));
        assert_eq!(
            state.local_tenant(31).flatten().as_deref(),
            Some("tenant-a")
        );
        assert_eq!(
            state.local_tenant(32).flatten().as_deref(),
            Some("tenant-b")
        );

        state
            .rollback(31)
            .expect("first transaction should roll back");
        assert!(!state.in_transaction(31));
        assert!(state.in_transaction(32));
        assert_eq!(state.local_tenant(31), None);
        assert_eq!(
            state.local_tenant(32).flatten().as_deref(),
            Some("tenant-b")
        );

        state
            .commit(32, |_| Ok::<_, ()>(()))
            .expect("commit preparation should succeed")
            .expect("connection should have an active transaction");
        assert_eq!(state.local_tenant(32), None);
    }
}
