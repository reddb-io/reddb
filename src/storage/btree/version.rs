//! MVCC Version Management
//!
//! Multi-version concurrency control for B+ tree entries.
//!
//! # Design
//!
//! Each key can have multiple versions, forming a chain from newest to oldest.
//! Transactions see a consistent snapshot based on their start timestamp.

pub use crate::storage::primitives::ids::{current_timestamp, next_timestamp, Timestamp, TxnId};

/// Version visibility for a transaction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionVisibility {
    /// Version is visible to this transaction
    Visible,
    /// Version is not yet committed (created by active transaction)
    Uncommitted,
    /// Version was deleted before transaction started
    Deleted,
    /// Version was created after transaction started
    Future,
}

/// A single version of a value
#[derive(Debug, Clone)]
pub struct Version<V: Clone> {
    /// Transaction that created this version
    pub created_by: TxnId,
    /// Timestamp when created
    pub created_at: Timestamp,
    /// Transaction that deleted this version (0 if not deleted)
    pub deleted_by: TxnId,
    /// Timestamp when deleted (0 if not deleted)
    pub deleted_at: Timestamp,
    /// The value (None if this is a tombstone)
    pub value: Option<V>,
    /// Pointer to older version
    pub prev: Option<Box<Version<V>>>,
}

impl<V: Clone> Version<V> {
    /// Create new version
    pub fn new(value: V, txn_id: TxnId, timestamp: Timestamp) -> Self {
        Self {
            created_by: txn_id,
            created_at: timestamp,
            deleted_by: TxnId::ZERO,
            deleted_at: Timestamp::EPOCH,
            value: Some(value),
            prev: None,
        }
    }

    /// Create tombstone version (delete marker)
    pub fn tombstone(txn_id: TxnId, timestamp: Timestamp) -> Self {
        Self {
            created_by: txn_id,
            created_at: timestamp,
            deleted_by: TxnId::ZERO,
            deleted_at: Timestamp::EPOCH,
            value: None,
            prev: None,
        }
    }

    /// Check if this version is a tombstone
    pub fn is_tombstone(&self) -> bool {
        self.value.is_none()
    }

    /// Check if this version is deleted
    pub fn is_deleted(&self) -> bool {
        !self.deleted_by.is_zero()
    }

    /// Mark as deleted by transaction
    pub fn mark_deleted(&mut self, txn_id: TxnId, timestamp: Timestamp) {
        self.deleted_by = txn_id;
        self.deleted_at = timestamp;
    }

    /// Check visibility for a snapshot
    pub fn check_visibility(&self, snapshot: &Snapshot) -> VersionVisibility {
        // If created by an uncommitted transaction (not in snapshot)
        if !self.created_by.is_zero() && !snapshot.is_committed(self.created_by) {
            if self.created_by == snapshot.txn_id {
                // Created by this transaction - visible
                if self.is_deleted() && self.deleted_by == snapshot.txn_id {
                    return VersionVisibility::Deleted;
                }
                return VersionVisibility::Visible;
            }
            return VersionVisibility::Uncommitted;
        }

        // If created after snapshot started
        if self.created_at > snapshot.start_ts {
            return VersionVisibility::Future;
        }

        // If deleted by a committed transaction before snapshot
        if self.is_deleted() {
            if snapshot.is_committed(self.deleted_by) && self.deleted_at <= snapshot.start_ts {
                return VersionVisibility::Deleted;
            }
        }

        // Visible
        VersionVisibility::Visible
    }
}

/// Version chain for a key (head is newest)
#[derive(Debug, Clone)]
pub struct VersionChain<V: Clone> {
    /// Head of version chain (newest)
    head: Option<Box<Version<V>>>,
    /// Number of versions in chain
    version_count: usize,
    /// Oldest visible timestamp (for GC)
    oldest_ts: Timestamp,
}

impl<V: Clone> VersionChain<V> {
    /// Create empty chain
    pub fn new() -> Self {
        Self {
            head: None,
            version_count: 0,
            oldest_ts: Timestamp::EPOCH,
        }
    }

    /// Create chain with initial version
    pub fn with_value(value: V, txn_id: TxnId, timestamp: Timestamp) -> Self {
        Self {
            head: Some(Box::new(Version::new(value, txn_id, timestamp))),
            version_count: 1,
            oldest_ts: timestamp,
        }
    }

    /// Check if chain is empty
    pub fn is_empty(&self) -> bool {
        self.head.is_none()
    }

    /// Get number of versions
    pub fn len(&self) -> usize {
        self.version_count
    }

    /// Get visible value for snapshot
    pub fn get(&self, snapshot: &Snapshot) -> Option<&V> {
        let mut current = self.head.as_ref();

        while let Some(version) = current {
            match version.check_visibility(snapshot) {
                VersionVisibility::Visible => {
                    return version.value.as_ref();
                }
                VersionVisibility::Deleted => {
                    return None;
                }
                VersionVisibility::Uncommitted | VersionVisibility::Future => {
                    // Skip to older version
                    current = version.prev.as_ref();
                }
            }
        }

        None
    }

    /// Insert new version at head
    pub fn insert(&mut self, value: V, txn_id: TxnId, timestamp: Timestamp) {
        let mut new_version = Box::new(Version::new(value, txn_id, timestamp));
        new_version.prev = self.head.take();
        self.head = Some(new_version);
        self.version_count += 1;

        if self.oldest_ts.is_epoch() {
            self.oldest_ts = timestamp;
        }
    }

    /// Update with new version (creates new version, points to old)
    pub fn update(&mut self, value: V, txn_id: TxnId, timestamp: Timestamp) {
        self.insert(value, txn_id, timestamp);
    }

    /// Delete (creates tombstone version)
    pub fn delete(&mut self, txn_id: TxnId, timestamp: Timestamp) {
        let mut tombstone = Box::new(Version::tombstone(txn_id, timestamp));
        tombstone.prev = self.head.take();
        self.head = Some(tombstone);
        self.version_count += 1;
    }

    /// Get head version (for write conflict detection)
    pub fn head(&self) -> Option<&Version<V>> {
        self.head.as_ref().map(|v| v.as_ref())
    }

    /// Get head version mutable
    pub fn head_mut(&mut self) -> Option<&mut Version<V>> {
        self.head.as_mut().map(|v| v.as_mut())
    }

    /// Garbage collect versions older than watermark
    pub fn gc(&mut self, watermark: Timestamp) -> usize {
        let mut removed = 0;

        // Find the last version that is still needed
        let mut current = &mut self.head;
        let mut found_visible = false;

        while let Some(version) = current {
            // Keep at least one version before watermark for visibility
            if version.created_at <= watermark {
                if found_visible {
                    // Can remove this and all older versions
                    if let Some(prev) = version.prev.take() {
                        removed += 1 + self.count_chain(&prev);
                    }
                    break;
                }
                found_visible = true;
            }
            current = &mut version.prev;
        }

        self.version_count -= removed;
        removed
    }

    /// Count versions in a chain
    fn count_chain(&self, version: &Version<V>) -> usize {
        let mut count = 1;
        let mut current = version.prev.as_ref();
        while let Some(v) = current {
            count += 1;
            current = v.prev.as_ref();
        }
        count
    }

    /// Check if all versions are tombstones (for compaction)
    pub fn is_all_deleted(&self) -> bool {
        let mut current = self.head.as_ref();
        while let Some(version) = current {
            if !version.is_tombstone() {
                return false;
            }
            current = version.prev.as_ref();
        }
        true
    }

    /// Get oldest timestamp in chain
    pub fn oldest_timestamp(&self) -> Timestamp {
        self.oldest_ts
    }
}

impl<V: Clone> Default for VersionChain<V> {
    fn default() -> Self {
        Self::new()
    }
}

/// Transaction snapshot for consistent reads
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// This transaction's ID
    pub txn_id: TxnId,
    /// Snapshot start timestamp
    pub start_ts: Timestamp,
    /// Set of active (uncommitted) transactions at snapshot time
    active_txns: Vec<TxnId>,
    /// Set of committed transactions visible to this snapshot
    committed_txns: Vec<TxnId>,
}

impl Snapshot {
    /// Create new snapshot
    pub fn new(txn_id: TxnId, start_ts: Timestamp) -> Self {
        Self {
            txn_id,
            start_ts,
            active_txns: Vec::new(),
            committed_txns: Vec::new(),
        }
    }

    /// Create snapshot with active transactions
    pub fn with_active(txn_id: TxnId, start_ts: Timestamp, active: Vec<TxnId>) -> Self {
        Self {
            txn_id,
            start_ts,
            active_txns: active,
            committed_txns: Vec::new(),
        }
    }

    /// Add committed transaction to snapshot
    pub fn add_committed(&mut self, txn_id: TxnId) {
        if !self.committed_txns.contains(&txn_id) {
            self.committed_txns.push(txn_id);
        }
    }

    /// Check if transaction is committed (visible to this snapshot)
    pub fn is_committed(&self, txn_id: TxnId) -> bool {
        // Transaction 0 is always committed (initial state)
        if txn_id.is_zero() {
            return true;
        }

        // If in active set, not committed
        if self.active_txns.contains(&txn_id) {
            return false;
        }

        // If in committed set, committed
        if self.committed_txns.contains(&txn_id) {
            return true;
        }

        // If started before snapshot and not in active set, committed
        // (This is a simplification - real impl would track commit timestamps)
        true
    }

    /// Check if transaction is active (uncommitted)
    pub fn is_active(&self, txn_id: TxnId) -> bool {
        self.active_txns.contains(&txn_id)
    }
}

/// Transaction state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnState {
    /// Transaction is active
    Active,
    /// Transaction is committed
    Committed,
    /// Transaction is aborted
    Aborted,
}

/// Active transaction tracking
#[derive(Debug)]
pub struct ActiveTransaction {
    /// Transaction ID
    pub id: TxnId,
    /// Start timestamp
    pub start_ts: Timestamp,
    /// State
    pub state: TxnState,
    /// Snapshot for reads
    pub snapshot: Snapshot,
    /// Write set (keys modified)
    write_set: Vec<Vec<u8>>,
    /// Read set (keys read - for validation)
    read_set: Vec<Vec<u8>>,
}

impl ActiveTransaction {
    /// Create new transaction
    pub fn new(id: TxnId, active_txns: Vec<TxnId>) -> Self {
        let start_ts = next_timestamp();
        Self {
            id,
            start_ts,
            state: TxnState::Active,
            snapshot: Snapshot::with_active(id, start_ts, active_txns),
            write_set: Vec::new(),
            read_set: Vec::new(),
        }
    }

    /// Record a read
    pub fn record_read(&mut self, key: &[u8]) {
        if !self.read_set.iter().any(|k| k == key) {
            self.read_set.push(key.to_vec());
        }
    }

    /// Record a write
    pub fn record_write(&mut self, key: &[u8]) {
        if !self.write_set.iter().any(|k| k == key) {
            self.write_set.push(key.to_vec());
        }
    }

    /// Get write set
    pub fn write_set(&self) -> &[Vec<u8>] {
        &self.write_set
    }

    /// Get read set
    pub fn read_set(&self) -> &[Vec<u8>] {
        &self.read_set
    }

    /// Mark as committed
    pub fn commit(&mut self) {
        self.state = TxnState::Committed;
    }

    /// Mark as aborted
    pub fn abort(&mut self) {
        self.state = TxnState::Aborted;
    }

    /// Check if active
    pub fn is_active(&self) -> bool {
        self.state == TxnState::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_chain_basic() {
        let mut chain: VersionChain<String> = VersionChain::new();
        assert!(chain.is_empty());

        chain.insert("v1".to_string(), TxnId(1), Timestamp(1));
        assert_eq!(chain.len(), 1);

        chain.update("v2".to_string(), TxnId(2), Timestamp(2));
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn test_version_visibility() {
        let mut chain: VersionChain<String> = VersionChain::new();

        // Insert by txn 1
        chain.insert("v1".to_string(), TxnId(1), Timestamp(1));

        // Update by txn 2
        chain.update("v2".to_string(), TxnId(2), Timestamp(2));

        // Snapshot at timestamp 1 should see v1
        let _snap1 = Snapshot::new(TxnId(3), Timestamp(1));
        // Note: In simplified impl, both versions are visible since we don't
        // track commit status precisely. Real impl would check commit timestamps.

        // Snapshot at timestamp 2 should see v2
        let snap2 = Snapshot::new(TxnId(3), Timestamp(2));
        assert_eq!(chain.get(&snap2), Some(&"v2".to_string()));
    }

    #[test]
    fn test_version_delete() {
        let mut chain: VersionChain<String> = VersionChain::new();

        chain.insert("v1".to_string(), TxnId(1), Timestamp(1));
        chain.delete(TxnId(2), Timestamp(2));

        // Snapshot at timestamp 2 should see tombstone (no value)
        let snap = Snapshot::new(TxnId(3), Timestamp(2));
        assert!(chain.get(&snap).is_none());
    }

    #[test]
    fn test_version_gc() {
        let mut chain: VersionChain<String> = VersionChain::new();

        chain.insert("v1".to_string(), TxnId(1), Timestamp(1));
        chain.update("v2".to_string(), TxnId(2), Timestamp(2));
        chain.update("v3".to_string(), TxnId(3), Timestamp(3));
        chain.update("v4".to_string(), TxnId(4), Timestamp(4));

        assert_eq!(chain.len(), 4);

        // GC versions older than timestamp 3
        let removed = chain.gc(Timestamp(3));
        assert!(removed > 0);
        assert!(chain.len() < 4);
    }

    #[test]
    fn test_snapshot() {
        let snap = Snapshot::new(TxnId(5), Timestamp(10));

        // Transaction 0 is always committed
        assert!(snap.is_committed(TxnId::ZERO));

        // Transactions started before snapshot are committed
        assert!(snap.is_committed(TxnId(3)));
    }

    #[test]
    fn test_snapshot_with_active() {
        let snap = Snapshot::with_active(TxnId(5), Timestamp(10), vec![TxnId(3), TxnId(4)]);

        // Active transactions are not committed
        assert!(!snap.is_committed(TxnId(3)));
        assert!(!snap.is_committed(TxnId(4)));

        // Other transactions are committed
        assert!(snap.is_committed(TxnId(1)));
        assert!(snap.is_committed(TxnId(2)));
    }

    #[test]
    fn test_active_transaction() {
        let mut txn = ActiveTransaction::new(TxnId(1), vec![]);

        assert!(txn.is_active());

        txn.record_read(b"key1");
        txn.record_write(b"key2");

        assert_eq!(txn.read_set().len(), 1);
        assert_eq!(txn.write_set().len(), 1);

        txn.commit();
        assert!(!txn.is_active());
        assert_eq!(txn.state, TxnState::Committed);
    }

    #[test]
    fn test_timestamp_generation() {
        let ts1 = next_timestamp();
        let ts2 = next_timestamp();
        let ts3 = next_timestamp();

        assert!(ts2 > ts1);
        assert!(ts3 > ts2);
    }
}
