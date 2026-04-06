//! Savepoint Management
//!
//! Enables partial rollback within transactions.

use std::collections::HashMap;
use std::sync::RwLock;

/// Transaction ID type
pub type TxnId = u64;

/// Log Sequence Number
pub type Lsn = u64;

/// Timestamp type
pub type Timestamp = u64;

/// A savepoint within a transaction
#[derive(Debug, Clone)]
pub struct Savepoint {
    /// Savepoint name
    pub name: String,
    /// Transaction ID
    pub txn_id: TxnId,
    /// LSN when savepoint was created
    pub lsn: Lsn,
    /// Timestamp when created
    pub created_at: Timestamp,
    /// Lock count at savepoint (for lock release)
    pub lock_count: usize,
    /// Write set index at savepoint (for partial rollback)
    pub write_set_index: usize,
    /// Nested savepoint depth
    pub depth: usize,
}

impl Savepoint {
    /// Create new savepoint
    pub fn new(
        name: String,
        txn_id: TxnId,
        lsn: Lsn,
        lock_count: usize,
        write_set_index: usize,
        depth: usize,
    ) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        Self {
            name,
            txn_id,
            lsn,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as Timestamp,
            lock_count,
            write_set_index,
            depth,
        }
    }
}

/// Savepoint manager for a single transaction
#[derive(Debug)]
pub struct TxnSavepoints {
    /// Transaction ID
    txn_id: TxnId,
    /// Savepoints by name
    savepoints: HashMap<String, Savepoint>,
    /// Savepoint stack (for nested savepoints)
    stack: Vec<String>,
}

impl TxnSavepoints {
    /// Create new savepoint manager for transaction
    pub fn new(txn_id: TxnId) -> Self {
        Self {
            txn_id,
            savepoints: HashMap::new(),
            stack: Vec::new(),
        }
    }

    /// Create a savepoint
    pub fn create(
        &mut self,
        name: String,
        lsn: Lsn,
        lock_count: usize,
        write_set_index: usize,
    ) -> &Savepoint {
        let depth = self.stack.len();
        let savepoint = Savepoint::new(
            name.clone(),
            self.txn_id,
            lsn,
            lock_count,
            write_set_index,
            depth,
        );

        self.savepoints.insert(name.clone(), savepoint);
        self.stack.push(name.clone());

        self.savepoints.get(&name).unwrap()
    }

    /// Get a savepoint by name
    pub fn get(&self, name: &str) -> Option<&Savepoint> {
        self.savepoints.get(name)
    }

    /// Release a savepoint (and all nested ones)
    pub fn release(&mut self, name: &str) -> Option<Savepoint> {
        // Find position in stack
        if let Some(pos) = self.stack.iter().position(|n| n == name) {
            // Remove this and all nested savepoints
            let to_remove: Vec<String> = self.stack.drain(pos..).collect();
            let removed = self.savepoints.remove(name);

            for nested_name in to_remove.iter().skip(1) {
                self.savepoints.remove(nested_name);
            }

            removed
        } else {
            None
        }
    }

    /// Rollback to savepoint (returns savepoints to release)
    pub fn rollback_to(&mut self, name: &str) -> Option<(Savepoint, Vec<String>)> {
        // Find position in stack
        if let Some(pos) = self.stack.iter().position(|n| n == name) {
            // Get savepoint info
            let savepoint = self.savepoints.get(name)?.clone();

            // Collect nested savepoints to release
            let to_release: Vec<String> = self.stack.drain(pos + 1..).collect();

            // Remove nested savepoints
            for nested_name in &to_release {
                self.savepoints.remove(nested_name);
            }

            Some((savepoint, to_release))
        } else {
            None
        }
    }

    /// Check if savepoint exists
    pub fn exists(&self, name: &str) -> bool {
        self.savepoints.contains_key(name)
    }

    /// Get current depth
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Get all savepoint names in order
    pub fn names(&self) -> &[String] {
        &self.stack
    }

    /// Clear all savepoints
    pub fn clear(&mut self) {
        self.savepoints.clear();
        self.stack.clear();
    }
}

/// Savepoint manager for all transactions
pub struct SavepointManager {
    /// Per-transaction savepoints
    txn_savepoints: RwLock<HashMap<TxnId, TxnSavepoints>>,
}

impl SavepointManager {
    /// Create new savepoint manager
    pub fn new() -> Self {
        Self {
            txn_savepoints: RwLock::new(HashMap::new()),
        }
    }

    /// Create a savepoint for a transaction
    pub fn create_savepoint(
        &self,
        txn_id: TxnId,
        name: String,
        lsn: Lsn,
        lock_count: usize,
        write_set_index: usize,
    ) -> Result<Savepoint, SavepointError> {
        let mut txn_map = self.txn_savepoints.write().unwrap();
        let txn_sp = txn_map
            .entry(txn_id)
            .or_insert_with(|| TxnSavepoints::new(txn_id));

        // Check for duplicate name
        if txn_sp.exists(&name) {
            return Err(SavepointError::DuplicateName(name));
        }

        Ok(txn_sp
            .create(name, lsn, lock_count, write_set_index)
            .clone())
    }

    /// Get a savepoint
    pub fn get_savepoint(&self, txn_id: TxnId, name: &str) -> Option<Savepoint> {
        let txn_map = self.txn_savepoints.read().unwrap();
        txn_map.get(&txn_id).and_then(|sp| sp.get(name).cloned())
    }

    /// Release a savepoint
    pub fn release_savepoint(
        &self,
        txn_id: TxnId,
        name: &str,
    ) -> Result<Savepoint, SavepointError> {
        let mut txn_map = self.txn_savepoints.write().unwrap();

        let txn_sp = txn_map
            .get_mut(&txn_id)
            .ok_or(SavepointError::TxnNotFound(txn_id))?;

        txn_sp
            .release(name)
            .ok_or_else(|| SavepointError::NotFound(name.to_string()))
    }

    /// Rollback to a savepoint
    pub fn rollback_to_savepoint(
        &self,
        txn_id: TxnId,
        name: &str,
    ) -> Result<(Savepoint, Vec<String>), SavepointError> {
        let mut txn_map = self.txn_savepoints.write().unwrap();

        let txn_sp = txn_map
            .get_mut(&txn_id)
            .ok_or(SavepointError::TxnNotFound(txn_id))?;

        txn_sp
            .rollback_to(name)
            .ok_or_else(|| SavepointError::NotFound(name.to_string()))
    }

    /// Check if savepoint exists
    pub fn savepoint_exists(&self, txn_id: TxnId, name: &str) -> bool {
        let txn_map = self.txn_savepoints.read().unwrap();
        txn_map
            .get(&txn_id)
            .map(|sp| sp.exists(name))
            .unwrap_or(false)
    }

    /// Get savepoint depth for transaction
    pub fn savepoint_depth(&self, txn_id: TxnId) -> usize {
        let txn_map = self.txn_savepoints.read().unwrap();
        txn_map.get(&txn_id).map(|sp| sp.depth()).unwrap_or(0)
    }

    /// Get all savepoint names for transaction
    pub fn get_savepoint_names(&self, txn_id: TxnId) -> Vec<String> {
        let txn_map = self.txn_savepoints.read().unwrap();
        txn_map
            .get(&txn_id)
            .map(|sp| sp.names().to_vec())
            .unwrap_or_default()
    }

    /// Clean up savepoints for a transaction
    pub fn cleanup_transaction(&self, txn_id: TxnId) {
        let mut txn_map = self.txn_savepoints.write().unwrap();
        txn_map.remove(&txn_id);
    }

    /// Get statistics
    pub fn stats(&self) -> SavepointStats {
        let txn_map = self.txn_savepoints.read().unwrap();
        SavepointStats {
            active_transactions: txn_map.len(),
            total_savepoints: txn_map.values().map(|sp| sp.depth()).sum(),
        }
    }
}

impl Default for SavepointManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Savepoint error types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavepointError {
    /// Savepoint not found
    NotFound(String),
    /// Duplicate savepoint name
    DuplicateName(String),
    /// Transaction not found
    TxnNotFound(TxnId),
    /// Savepoint stack corrupted
    StackCorrupted,
}

impl std::fmt::Display for SavepointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SavepointError::NotFound(name) => write!(f, "Savepoint '{}' not found", name),
            SavepointError::DuplicateName(name) => {
                write!(f, "Savepoint '{}' already exists", name)
            }
            SavepointError::TxnNotFound(id) => write!(f, "Transaction {} not found", id),
            SavepointError::StackCorrupted => write!(f, "Savepoint stack corrupted"),
        }
    }
}

impl std::error::Error for SavepointError {}

/// Savepoint statistics
#[derive(Debug, Clone, Default)]
pub struct SavepointStats {
    /// Number of transactions with savepoints
    pub active_transactions: usize,
    /// Total savepoints across all transactions
    pub total_savepoints: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_savepoint_create() {
        let sp = Savepoint::new("sp1".to_string(), 1, 100, 5, 10, 0);
        assert_eq!(sp.name, "sp1");
        assert_eq!(sp.txn_id, 1);
        assert_eq!(sp.lsn, 100);
        assert_eq!(sp.lock_count, 5);
        assert_eq!(sp.write_set_index, 10);
        assert_eq!(sp.depth, 0);
    }

    #[test]
    fn test_txn_savepoints() {
        let mut sp = TxnSavepoints::new(1);

        // Create savepoints (name, lsn, lock_count, write_set_index)
        sp.create("sp1".to_string(), 100, 1, 0);
        sp.create("sp2".to_string(), 200, 2, 5);
        sp.create("sp3".to_string(), 300, 3, 10);

        assert_eq!(sp.depth(), 3);
        assert!(sp.exists("sp1"));
        assert!(sp.exists("sp2"));
        assert!(sp.exists("sp3"));

        // Get savepoint
        let sp1 = sp.get("sp1").unwrap();
        assert_eq!(sp1.lsn, 100);
        assert_eq!(sp1.depth, 0);

        let sp3 = sp.get("sp3").unwrap();
        assert_eq!(sp3.depth, 2);
    }

    #[test]
    fn test_savepoint_release() {
        let mut sp = TxnSavepoints::new(1);

        sp.create("sp1".to_string(), 100, 1, 0);
        sp.create("sp2".to_string(), 200, 2, 5);
        sp.create("sp3".to_string(), 300, 3, 10);

        // Release sp2 (should also release sp3)
        let released = sp.release("sp2").unwrap();
        assert_eq!(released.name, "sp2");

        assert_eq!(sp.depth(), 1);
        assert!(sp.exists("sp1"));
        assert!(!sp.exists("sp2"));
        assert!(!sp.exists("sp3"));
    }

    #[test]
    fn test_savepoint_rollback() {
        let mut sp = TxnSavepoints::new(1);

        sp.create("sp1".to_string(), 100, 1, 0);
        sp.create("sp2".to_string(), 200, 2, 5);
        sp.create("sp3".to_string(), 300, 3, 10);

        // Rollback to sp2
        let (savepoint, released) = sp.rollback_to("sp2").unwrap();
        assert_eq!(savepoint.name, "sp2");
        assert_eq!(savepoint.lsn, 200);
        assert_eq!(released, vec!["sp3".to_string()]);

        // sp2 should still exist
        assert!(sp.exists("sp1"));
        assert!(sp.exists("sp2"));
        assert!(!sp.exists("sp3"));
        assert_eq!(sp.depth(), 2);
    }

    #[test]
    fn test_savepoint_manager() {
        let manager = SavepointManager::new();

        // Create savepoints for transaction 1
        let sp1 = manager
            .create_savepoint(1, "sp1".to_string(), 100, 1, 0)
            .unwrap();
        assert_eq!(sp1.name, "sp1");

        let sp2 = manager
            .create_savepoint(1, "sp2".to_string(), 200, 2, 0)
            .unwrap();
        assert_eq!(sp2.name, "sp2");

        // Duplicate should fail
        let result = manager.create_savepoint(1, "sp1".to_string(), 300, 3, 0);
        assert!(matches!(result, Err(SavepointError::DuplicateName(_))));

        // Different transaction can have same name
        let sp1_tx2 = manager
            .create_savepoint(2, "sp1".to_string(), 400, 4, 0)
            .unwrap();
        assert_eq!(sp1_tx2.txn_id, 2);

        // Check existence
        assert!(manager.savepoint_exists(1, "sp1"));
        assert!(manager.savepoint_exists(1, "sp2"));
        assert!(manager.savepoint_exists(2, "sp1"));
        assert!(!manager.savepoint_exists(1, "sp3"));
    }

    #[test]
    fn test_manager_rollback() {
        let manager = SavepointManager::new();

        manager
            .create_savepoint(1, "sp1".to_string(), 100, 1, 0)
            .unwrap();
        manager
            .create_savepoint(1, "sp2".to_string(), 200, 2, 0)
            .unwrap();
        manager
            .create_savepoint(1, "sp3".to_string(), 300, 3, 0)
            .unwrap();

        // Rollback to sp2
        let (sp, released) = manager.rollback_to_savepoint(1, "sp2").unwrap();
        assert_eq!(sp.lsn, 200);
        assert_eq!(released, vec!["sp3".to_string()]);

        // sp2 should still exist, sp3 should not
        assert!(manager.savepoint_exists(1, "sp2"));
        assert!(!manager.savepoint_exists(1, "sp3"));
    }

    #[test]
    fn test_manager_cleanup() {
        let manager = SavepointManager::new();

        manager
            .create_savepoint(1, "sp1".to_string(), 100, 1, 0)
            .unwrap();
        manager
            .create_savepoint(1, "sp2".to_string(), 200, 2, 0)
            .unwrap();

        // Cleanup transaction
        manager.cleanup_transaction(1);

        // All savepoints should be gone
        assert!(!manager.savepoint_exists(1, "sp1"));
        assert!(!manager.savepoint_exists(1, "sp2"));
        assert_eq!(manager.savepoint_depth(1), 0);
    }

    #[test]
    fn test_get_savepoint_names() {
        let manager = SavepointManager::new();

        manager
            .create_savepoint(1, "first".to_string(), 100, 1, 0)
            .unwrap();
        manager
            .create_savepoint(1, "second".to_string(), 200, 2, 0)
            .unwrap();
        manager
            .create_savepoint(1, "third".to_string(), 300, 3, 0)
            .unwrap();

        let names = manager.get_savepoint_names(1);
        assert_eq!(names, vec!["first", "second", "third"]);
    }
}
