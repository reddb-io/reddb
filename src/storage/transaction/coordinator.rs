//! Transaction Coordinator
//!
//! Central coordinator for transaction lifecycle management.

use super::lock::{LockManager, LockMode, LockResult};
use super::log::{TransactionLog, WalConfig};
use super::savepoint::TxnSavepoints;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Transaction ID
pub type TxnId = u64;

/// Timestamp
pub type Timestamp = u64;

/// Transaction error types
#[derive(Debug, Clone)]
pub enum TxnError {
    /// Transaction not found
    NotFound(TxnId),
    /// Transaction already committed
    AlreadyCommitted(TxnId),
    /// Transaction already aborted
    AlreadyAborted(TxnId),
    /// Write-write conflict
    WriteConflict { key: Vec<u8>, holder: TxnId },
    /// Deadlock detected
    Deadlock(Vec<TxnId>),
    /// Lock limit exceeded
    LockLimitExceeded { limit: usize },
    /// Lock timeout
    LockTimeout { key: Vec<u8>, timeout: Duration },
    /// Validation failed (optimistic)
    ValidationFailed {
        key: Vec<u8>,
        expected_ts: Timestamp,
        actual_ts: Timestamp,
    },
    /// WAL error
    LogError(String),
    /// Savepoint not found
    SavepointNotFound(String),
    /// Transaction timeout
    Timeout(TxnId),
    /// Internal error
    Internal(String),
}

impl std::fmt::Display for TxnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TxnError::NotFound(id) => write!(f, "Transaction {} not found", id),
            TxnError::AlreadyCommitted(id) => write!(f, "Transaction {} already committed", id),
            TxnError::AlreadyAborted(id) => write!(f, "Transaction {} already aborted", id),
            TxnError::WriteConflict { key, holder } => {
                write!(f, "Write conflict on {:?}, held by txn {}", key, holder)
            }
            TxnError::Deadlock(cycle) => write!(f, "Deadlock detected: {:?}", cycle),
            TxnError::LockLimitExceeded { limit } => {
                write!(f, "Lock limit exceeded: max {}", limit)
            }
            TxnError::LockTimeout { key, timeout } => {
                write!(f, "Lock timeout on {:?} after {:?}", key, timeout)
            }
            TxnError::ValidationFailed {
                key,
                expected_ts,
                actual_ts,
            } => {
                write!(
                    f,
                    "Validation failed for {:?}: expected ts {}, actual {}",
                    key, expected_ts, actual_ts
                )
            }
            TxnError::LogError(msg) => write!(f, "WAL error: {}", msg),
            TxnError::SavepointNotFound(name) => write!(f, "Savepoint '{}' not found", name),
            TxnError::Timeout(id) => write!(f, "Transaction {} timed out", id),
            TxnError::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for TxnError {}

/// Transaction state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnState {
    /// Active and running
    Active,
    /// Preparing to commit (2PC)
    Preparing,
    /// Committed
    Committed,
    /// Aborted
    Aborted,
}

/// Isolation level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    /// Read uncommitted (no isolation)
    ReadUncommitted,
    /// Read committed (see committed values)
    ReadCommitted,
    /// Repeatable read / Snapshot isolation
    SnapshotIsolation,
    /// Serializable (full isolation)
    Serializable,
}

impl Default for IsolationLevel {
    fn default() -> Self {
        IsolationLevel::SnapshotIsolation
    }
}

/// Transaction configuration
#[derive(Debug, Clone)]
pub struct TxnConfig {
    /// Default isolation level
    pub isolation_level: IsolationLevel,
    /// Lock timeout
    pub lock_timeout: Duration,
    /// Transaction timeout
    pub txn_timeout: Duration,
    /// Enable optimistic concurrency
    pub optimistic: bool,
    /// Enable WAL
    pub wal_enabled: bool,
    /// WAL sync mode
    pub wal_sync: WalSyncMode,
}

/// WAL sync mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalSyncMode {
    /// Sync on every commit (safest, slowest)
    EveryCommit,
    /// Sync periodically (balance)
    Periodic(Duration),
    /// Don't sync (fastest, least safe)
    None,
}

impl TxnConfig {
    /// Create default config
    pub fn new() -> Self {
        Self {
            isolation_level: IsolationLevel::SnapshotIsolation,
            lock_timeout: Duration::from_secs(30),
            txn_timeout: Duration::from_secs(300),
            optimistic: true,
            wal_enabled: true,
            wal_sync: WalSyncMode::EveryCommit,
        }
    }

    /// Set isolation level
    pub fn with_isolation(mut self, level: IsolationLevel) -> Self {
        self.isolation_level = level;
        self
    }

    /// Set lock timeout
    pub fn with_lock_timeout(mut self, timeout: Duration) -> Self {
        self.lock_timeout = timeout;
        self
    }

    /// Enable/disable optimistic concurrency
    pub fn with_optimistic(mut self, enabled: bool) -> Self {
        self.optimistic = enabled;
        self
    }
}

impl Default for TxnConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Transaction handle (returned to callers)
#[derive(Debug, Clone)]
pub struct TxnHandle {
    /// Transaction ID
    pub id: TxnId,
    /// Start timestamp
    pub start_ts: Timestamp,
    /// Isolation level
    pub isolation: IsolationLevel,
}

impl TxnHandle {
    /// Get transaction ID
    pub fn id(&self) -> TxnId {
        self.id
    }

    /// Get start timestamp
    pub fn start_ts(&self) -> Timestamp {
        self.start_ts
    }
}

/// Internal transaction state
struct TransactionState {
    /// Transaction handle info
    handle: TxnHandle,
    /// Current state
    state: TxnState,
    /// Start time
    start_time: Instant,
    /// Read set (keys read)
    read_set: Vec<(Vec<u8>, Timestamp)>,
    /// Write set (keys written with old values for rollback)
    write_set: Vec<WriteEntry>,
    /// Savepoints (per-transaction)
    savepoints: TxnSavepoints,
    /// Locks held
    locks_held: Vec<Vec<u8>>,
}

/// Write set entry
#[derive(Debug, Clone)]
struct WriteEntry {
    /// Key
    key: Vec<u8>,
    /// Old value (for rollback)
    old_value: Option<Vec<u8>>,
    /// New value
    new_value: Option<Vec<u8>>,
    /// Operation timestamp
    timestamp: Timestamp,
}

/// Active transaction representation
pub struct Transaction {
    /// Transaction ID
    id: TxnId,
    /// Coordinator reference
    coordinator: Arc<TransactionManager>,
}

impl Transaction {
    /// Get transaction ID
    pub fn id(&self) -> TxnId {
        self.id
    }

    /// Record a read
    pub fn record_read(&self, key: &[u8], read_ts: Timestamp) {
        self.coordinator.record_read(self.id, key, read_ts);
    }

    /// Record a write
    pub fn record_write(&self, key: &[u8], old_value: Option<&[u8]>, new_value: Option<&[u8]>) {
        self.coordinator
            .record_write(self.id, key, old_value, new_value);
    }

    /// Create savepoint
    pub fn savepoint(&self, name: &str) -> Result<(), TxnError> {
        self.coordinator.create_savepoint(self.id, name)
    }

    /// Rollback to savepoint
    pub fn rollback_to(&self, name: &str) -> Result<(), TxnError> {
        self.coordinator.rollback_to_savepoint(self.id, name)
    }

    /// Commit transaction
    pub fn commit(self) -> Result<(), TxnError> {
        self.coordinator.commit(self.id)
    }

    /// Abort transaction
    pub fn abort(self) -> Result<(), TxnError> {
        self.coordinator.abort(self.id)
    }
}

/// Transaction manager
pub struct TransactionManager {
    /// Configuration
    config: TxnConfig,
    /// Next transaction ID
    next_id: AtomicU64,
    /// Current timestamp
    current_ts: AtomicU64,
    /// Active transactions
    transactions: RwLock<HashMap<TxnId, TransactionState>>,
    /// Lock manager
    lock_manager: LockManager,
    /// Transaction log
    log: Option<TransactionLog>,
    /// Committed timestamps per key (for validation)
    committed_ts: RwLock<HashMap<Vec<u8>, Timestamp>>,
}

impl TransactionManager {
    /// Create new transaction manager
    pub fn new(config: TxnConfig) -> Self {
        let log = if config.wal_enabled {
            Some(TransactionLog::new(WalConfig::default()))
        } else {
            None
        };

        Self {
            config,
            next_id: AtomicU64::new(1),
            current_ts: AtomicU64::new(1),
            transactions: RwLock::new(HashMap::new()),
            lock_manager: LockManager::with_defaults(),
            log: log.and_then(|r| r.ok()),
            committed_ts: RwLock::new(HashMap::new()),
        }
    }

    /// Create with default config
    pub fn with_default_config() -> Self {
        Self::new(TxnConfig::default())
    }

    /// Get configuration
    pub fn config(&self) -> &TxnConfig {
        &self.config
    }

    /// Get next timestamp
    fn next_timestamp(&self) -> Timestamp {
        self.current_ts.fetch_add(1, Ordering::SeqCst)
    }

    /// Begin a new transaction
    pub fn begin(&self) -> TxnHandle {
        self.begin_with_isolation(self.config.isolation_level)
    }

    /// Begin transaction with specific isolation level
    pub fn begin_with_isolation(&self, isolation: IsolationLevel) -> TxnHandle {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let start_ts = self.next_timestamp();

        let handle = TxnHandle {
            id,
            start_ts,
            isolation,
        };

        let state = TransactionState {
            handle: handle.clone(),
            state: TxnState::Active,
            start_time: Instant::now(),
            read_set: Vec::new(),
            write_set: Vec::new(),
            savepoints: TxnSavepoints::new(id),
            locks_held: Vec::new(),
        };

        // Log transaction begin
        if let Some(ref log) = self.log {
            let _ = log.log_begin(id);
        }

        self.transactions.write().unwrap().insert(id, state);

        handle
    }

    /// Begin with Transaction wrapper
    pub fn begin_transaction(self: &Arc<Self>) -> Transaction {
        let handle = self.begin();
        Transaction {
            id: handle.id,
            coordinator: Arc::clone(self),
        }
    }

    /// Record a read operation
    pub fn record_read(&self, txn_id: TxnId, key: &[u8], read_ts: Timestamp) {
        let mut txns = self.transactions.write().unwrap();
        if let Some(state) = txns.get_mut(&txn_id) {
            if state.state == TxnState::Active {
                state.read_set.push((key.to_vec(), read_ts));
            }
        }
    }

    /// Record a write operation
    pub fn record_write(
        &self,
        txn_id: TxnId,
        key: &[u8],
        old_value: Option<&[u8]>,
        new_value: Option<&[u8]>,
    ) {
        let timestamp = self.next_timestamp();

        let mut txns = self.transactions.write().unwrap();
        if let Some(state) = txns.get_mut(&txn_id) {
            if state.state == TxnState::Active {
                let entry = WriteEntry {
                    key: key.to_vec(),
                    old_value: old_value.map(|v| v.to_vec()),
                    new_value: new_value.map(|v| v.to_vec()),
                    timestamp,
                };

                // Log the write
                if let Some(ref log) = self.log {
                    if let Some(old) = old_value {
                        if let Some(new) = new_value {
                            let _ =
                                log.log_update(txn_id, key.to_vec(), old.to_vec(), new.to_vec());
                        } else {
                            let _ = log.log_delete(txn_id, key.to_vec(), old.to_vec());
                        }
                    } else if let Some(new) = new_value {
                        let _ = log.log_insert(txn_id, key.to_vec(), new.to_vec());
                    }
                }

                state.write_set.push(entry);
            }
        }
    }

    /// Acquire lock for key
    pub fn acquire_lock(&self, txn_id: TxnId, key: &[u8], mode: LockMode) -> Result<(), TxnError> {
        // Check transaction is active
        {
            let txns = self.transactions.read().unwrap();
            let state = txns.get(&txn_id).ok_or(TxnError::NotFound(txn_id))?;
            if state.state != TxnState::Active {
                return Err(TxnError::AlreadyAborted(txn_id));
            }
        }

        // Try to acquire lock with timeout
        match self
            .lock_manager
            .acquire_with_timeout(txn_id, key, mode, self.config.lock_timeout)
        {
            LockResult::Granted | LockResult::Upgraded | LockResult::AlreadyHeld => {
                // Record lock (even if already held - idempotent)
                let mut txns = self.transactions.write().unwrap();
                if let Some(state) = txns.get_mut(&txn_id) {
                    if !state.locks_held.contains(&key.to_vec()) {
                        state.locks_held.push(key.to_vec());
                    }
                }
                Ok(())
            }
            LockResult::Waiting => {
                // This shouldn't happen with acquire_with_timeout (it blocks)
                Err(TxnError::Internal(
                    "Lock returned Waiting unexpectedly".to_string(),
                ))
            }
            LockResult::Timeout => Err(TxnError::LockTimeout {
                key: key.to_vec(),
                timeout: self.config.lock_timeout,
            }),
            LockResult::Deadlock(cycle) => Err(TxnError::Deadlock(cycle)),
            LockResult::LockLimitExceeded => Err(TxnError::LockLimitExceeded {
                limit: self.lock_manager.config().max_locks_per_txn,
            }),
            LockResult::TxnNotFound => Err(TxnError::NotFound(txn_id)),
        }
    }

    /// Release all locks for transaction
    fn release_locks(&self, txn_id: TxnId) {
        let locks = {
            let txns = self.transactions.read().unwrap();
            txns.get(&txn_id)
                .map(|s| s.locks_held.clone())
                .unwrap_or_default()
        };

        for key in locks {
            self.lock_manager.release(txn_id, &key);
        }
    }

    /// Validate transaction (optimistic concurrency check)
    fn validate(&self, txn_id: TxnId) -> Result<(), TxnError> {
        let txns = self.transactions.read().unwrap();
        let state = txns.get(&txn_id).ok_or(TxnError::NotFound(txn_id))?;

        if !self.config.optimistic {
            return Ok(());
        }

        let committed = self.committed_ts.read().unwrap();

        // Check read set: no key was modified since we read it
        for (key, read_ts) in &state.read_set {
            if let Some(&commit_ts) = committed.get(key) {
                if commit_ts > *read_ts && commit_ts > state.handle.start_ts {
                    return Err(TxnError::ValidationFailed {
                        key: key.clone(),
                        expected_ts: *read_ts,
                        actual_ts: commit_ts,
                    });
                }
            }
        }

        Ok(())
    }

    /// Commit transaction
    pub fn commit(&self, txn_id: TxnId) -> Result<(), TxnError> {
        // Validate
        self.validate(txn_id)?;

        let commit_ts = self.next_timestamp();

        // Update state to committed
        {
            let mut txns = self.transactions.write().unwrap();
            let state = txns.get_mut(&txn_id).ok_or(TxnError::NotFound(txn_id))?;

            match state.state {
                TxnState::Active | TxnState::Preparing => {
                    state.state = TxnState::Committed;
                }
                TxnState::Committed => return Err(TxnError::AlreadyCommitted(txn_id)),
                TxnState::Aborted => return Err(TxnError::AlreadyAborted(txn_id)),
            }

            // Update committed timestamps for written keys
            let mut committed = self.committed_ts.write().unwrap();
            for entry in &state.write_set {
                committed.insert(entry.key.clone(), commit_ts);
            }
        }

        // Log commit
        if let Some(ref log) = self.log {
            let _ = log.log_commit(txn_id);

            // Flush if configured
            if matches!(self.config.wal_sync, WalSyncMode::EveryCommit) {
                let _ = log.flush();
            }
        }

        // Release locks
        self.release_locks(txn_id);

        Ok(())
    }

    /// Abort transaction
    pub fn abort(&self, txn_id: TxnId) -> Result<(), TxnError> {
        // Update state to aborted
        {
            let mut txns = self.transactions.write().unwrap();
            let state = txns.get_mut(&txn_id).ok_or(TxnError::NotFound(txn_id))?;

            match state.state {
                TxnState::Active | TxnState::Preparing => {
                    state.state = TxnState::Aborted;
                }
                TxnState::Committed => return Err(TxnError::AlreadyCommitted(txn_id)),
                TxnState::Aborted => return Err(TxnError::AlreadyAborted(txn_id)),
            }
        }

        // Log abort
        if let Some(ref log) = self.log {
            let _ = log.log_abort(txn_id);
        }

        // Release locks
        self.release_locks(txn_id);

        Ok(())
    }

    /// Create savepoint
    pub fn create_savepoint(&self, txn_id: TxnId, name: &str) -> Result<(), TxnError> {
        let mut txns = self.transactions.write().unwrap();
        let state = txns.get_mut(&txn_id).ok_or(TxnError::NotFound(txn_id))?;

        if state.state != TxnState::Active {
            return Err(TxnError::AlreadyAborted(txn_id));
        }

        let write_set_index = state.write_set.len();
        let lock_count = state.locks_held.len();
        // Use 0 for LSN - coordinator doesn't track LSNs at this level
        state
            .savepoints
            .create(name.to_string(), 0, lock_count, write_set_index);

        Ok(())
    }

    /// Rollback to savepoint
    pub fn rollback_to_savepoint(&self, txn_id: TxnId, name: &str) -> Result<(), TxnError> {
        let mut txns = self.transactions.write().unwrap();
        let state = txns.get_mut(&txn_id).ok_or(TxnError::NotFound(txn_id))?;

        if state.state != TxnState::Active {
            return Err(TxnError::AlreadyAborted(txn_id));
        }

        let savepoint = state
            .savepoints
            .get(name)
            .ok_or_else(|| TxnError::SavepointNotFound(name.to_string()))?;

        // Truncate write set to savepoint
        state.write_set.truncate(savepoint.write_set_index);

        // Remove savepoint and all after it
        state.savepoints.release(name);

        Ok(())
    }

    /// Get transaction state
    pub fn get_state(&self, txn_id: TxnId) -> Option<TxnState> {
        self.transactions
            .read()
            .unwrap()
            .get(&txn_id)
            .map(|s| s.state)
    }

    /// Check if transaction is active
    pub fn is_active(&self, txn_id: TxnId) -> bool {
        self.get_state(txn_id) == Some(TxnState::Active)
    }

    /// Get active transaction count
    pub fn active_count(&self) -> usize {
        self.transactions
            .read()
            .unwrap()
            .values()
            .filter(|s| s.state == TxnState::Active)
            .count()
    }

    /// Get oldest active transaction timestamp
    pub fn oldest_active_ts(&self) -> Option<Timestamp> {
        self.transactions
            .read()
            .unwrap()
            .values()
            .filter(|s| s.state == TxnState::Active)
            .map(|s| s.handle.start_ts)
            .min()
    }

    /// Cleanup finished transactions
    pub fn cleanup(&self, max_age: Duration) {
        let mut txns = self.transactions.write().unwrap();
        let now = Instant::now();

        txns.retain(|_, state| {
            if state.state == TxnState::Active {
                true
            } else {
                now.duration_since(state.start_time) < max_age
            }
        });
    }
}

impl Default for TransactionManager {
    fn default() -> Self {
        Self::with_default_config()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_begin_commit() {
        let tm = TransactionManager::with_default_config();

        let handle = tm.begin();
        assert!(tm.is_active(handle.id));

        tm.commit(handle.id).unwrap();
        assert!(!tm.is_active(handle.id));
        assert_eq!(tm.get_state(handle.id), Some(TxnState::Committed));
    }

    #[test]
    fn test_begin_abort() {
        let tm = TransactionManager::with_default_config();

        let handle = tm.begin();
        assert!(tm.is_active(handle.id));

        tm.abort(handle.id).unwrap();
        assert!(!tm.is_active(handle.id));
        assert_eq!(tm.get_state(handle.id), Some(TxnState::Aborted));
    }

    #[test]
    fn test_double_commit() {
        let tm = TransactionManager::with_default_config();

        let handle = tm.begin();
        tm.commit(handle.id).unwrap();

        assert!(matches!(
            tm.commit(handle.id),
            Err(TxnError::AlreadyCommitted(_))
        ));
    }

    #[test]
    fn test_transaction_wrapper() {
        let tm = Arc::new(TransactionManager::with_default_config());

        let txn = tm.begin_transaction();
        let id = txn.id();

        txn.record_write(b"key1", None, Some(b"value1"));
        txn.commit().unwrap();

        assert!(!tm.is_active(id));
    }

    #[test]
    fn test_savepoints() {
        let tm = TransactionManager::with_default_config();

        let handle = tm.begin();

        tm.record_write(handle.id, b"key1", None, Some(b"v1"));
        tm.create_savepoint(handle.id, "sp1").unwrap();

        tm.record_write(handle.id, b"key2", None, Some(b"v2"));
        tm.record_write(handle.id, b"key3", None, Some(b"v3"));

        // Rollback to savepoint
        tm.rollback_to_savepoint(handle.id, "sp1").unwrap();

        // Should be able to commit
        tm.commit(handle.id).unwrap();
    }

    #[test]
    fn test_isolation_levels() {
        let tm = TransactionManager::with_default_config();

        let h1 = tm.begin_with_isolation(IsolationLevel::ReadCommitted);
        let h2 = tm.begin_with_isolation(IsolationLevel::SnapshotIsolation);

        assert_eq!(h1.isolation, IsolationLevel::ReadCommitted);
        assert_eq!(h2.isolation, IsolationLevel::SnapshotIsolation);

        tm.abort(h1.id).unwrap();
        tm.abort(h2.id).unwrap();
    }

    #[test]
    fn test_active_count() {
        let tm = TransactionManager::with_default_config();

        assert_eq!(tm.active_count(), 0);

        let h1 = tm.begin();
        let h2 = tm.begin();
        assert_eq!(tm.active_count(), 2);

        tm.commit(h1.id).unwrap();
        assert_eq!(tm.active_count(), 1);

        tm.abort(h2.id).unwrap();
        assert_eq!(tm.active_count(), 0);
    }

    #[test]
    fn test_oldest_active_ts() {
        let tm = TransactionManager::with_default_config();

        let h1 = tm.begin();
        let ts1 = h1.start_ts;

        let _h2 = tm.begin();

        assert_eq!(tm.oldest_active_ts(), Some(ts1));
    }

    #[test]
    fn test_config() {
        let config = TxnConfig::new()
            .with_isolation(IsolationLevel::Serializable)
            .with_lock_timeout(Duration::from_secs(10))
            .with_optimistic(false);

        assert_eq!(config.isolation_level, IsolationLevel::Serializable);
        assert_eq!(config.lock_timeout, Duration::from_secs(10));
        assert!(!config.optimistic);
    }
}
