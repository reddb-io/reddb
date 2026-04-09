//! Primary-side replication: WAL record production and snapshot serving.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

/// In-memory WAL buffer for replication.
/// Primary appends records here; replicas consume from it.
pub struct WalBuffer {
    /// Circular buffer of (lsn, serialized_record) pairs.
    records: RwLock<VecDeque<(u64, Vec<u8>)>>,
    /// Maximum records to keep in buffer.
    max_size: usize,
    /// Current write LSN.
    current_lsn: RwLock<u64>,
}

impl WalBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            records: RwLock::new(VecDeque::with_capacity(max_size)),
            max_size,
            current_lsn: RwLock::new(0),
        }
    }

    /// Append a WAL record. Called by the storage engine after each write.
    pub fn append(&self, data: Vec<u8>) -> u64 {
        let mut lsn = self.current_lsn.write().unwrap_or_else(|e| e.into_inner());
        *lsn += 1;
        let current = *lsn;
        drop(lsn);

        let mut records = self.records.write().unwrap_or_else(|e| e.into_inner());
        records.push_back((current, data));
        while records.len() > self.max_size {
            records.pop_front();
        }
        current
    }

    /// Read records since the given LSN (exclusive).
    pub fn read_since(&self, since_lsn: u64, max_count: usize) -> Vec<(u64, Vec<u8>)> {
        let records = self.records.read().unwrap_or_else(|e| e.into_inner());
        records
            .iter()
            .filter(|(lsn, _)| *lsn > since_lsn)
            .take(max_count)
            .cloned()
            .collect()
    }

    /// Current LSN.
    pub fn current_lsn(&self) -> u64 {
        *self.current_lsn.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Oldest available LSN (for gap detection).
    pub fn oldest_lsn(&self) -> Option<u64> {
        let records = self.records.read().unwrap_or_else(|e| e.into_inner());
        records.front().map(|(lsn, _)| *lsn)
    }
}

/// State of a connected replica.
#[derive(Debug, Clone)]
pub struct ReplicaState {
    pub id: String,
    pub last_acked_lsn: u64,
    pub connected_at_unix_ms: u128,
}

/// Primary replication manager.
pub struct PrimaryReplication {
    pub wal_buffer: Arc<WalBuffer>,
    pub replicas: RwLock<Vec<ReplicaState>>,
}

impl PrimaryReplication {
    pub fn new() -> Self {
        Self {
            wal_buffer: Arc::new(WalBuffer::new(100_000)),
            replicas: RwLock::new(Vec::new()),
        }
    }

    pub fn register_replica(&self, id: String) -> u64 {
        let lsn = self.wal_buffer.current_lsn();
        let state = ReplicaState {
            id,
            last_acked_lsn: lsn,
            connected_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
        };
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        replicas.push(state);
        lsn
    }

    pub fn ack_replica(&self, id: &str, lsn: u64) {
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = replicas.iter_mut().find(|r| r.id == id) {
            r.last_acked_lsn = lsn;
        }
    }

    pub fn replica_count(&self) -> usize {
        self.replicas
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}
