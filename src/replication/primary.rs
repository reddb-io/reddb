//! Primary-side replication: WAL record production and snapshot serving.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

const LOGICAL_WAL_SPOOL_MAGIC: &[u8; 4] = b"RDLW";
const LOGICAL_WAL_SPOOL_VERSION: u8 = 1;

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
    pub fn append(&self, lsn: u64, data: Vec<u8>) {
        let mut records = self.records.write().unwrap_or_else(|e| e.into_inner());
        records.push_back((lsn, data));
        while records.len() > self.max_size {
            records.pop_front();
        }

        let mut current = self.current_lsn.write().unwrap_or_else(|e| e.into_inner());
        *current = (*current).max(lsn);
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

    pub fn set_current_lsn(&self, lsn: u64) {
        let mut current = self.current_lsn.write().unwrap_or_else(|e| e.into_inner());
        *current = (*current).max(lsn);
    }

    /// Oldest available LSN (for gap detection).
    pub fn oldest_lsn(&self) -> Option<u64> {
        let records = self.records.read().unwrap_or_else(|e| e.into_inner());
        records.front().map(|(lsn, _)| *lsn)
    }
}

#[derive(Debug, Clone)]
struct LogicalWalEntry {
    lsn: u64,
    data: Vec<u8>,
}

#[derive(Debug, Default)]
struct LogicalWalSpoolState {
    current_lsn: u64,
}

/// Durable append-only logical WAL spool kept beside the main `.rdb` file.
///
/// This is not the storage-engine WAL; it is a structured replication/PITR log.
pub struct LogicalWalSpool {
    path: PathBuf,
    state: Mutex<LogicalWalSpoolState>,
}

impl LogicalWalSpool {
    pub fn path_for(data_path: &Path) -> PathBuf {
        let file_name = data_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("reddb.rdb");
        let spool_name = format!("{file_name}.logical.wal");
        match data_path.parent() {
            Some(parent) => parent.join(spool_name),
            None => PathBuf::from(spool_name),
        }
    }

    pub fn open(data_path: &Path) -> io::Result<Self> {
        let path = Self::path_for(data_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            File::create(&path)?;
        }
        let current_lsn = read_entries(&path)?
            .last()
            .map(|entry| entry.lsn)
            .unwrap_or(0);
        Ok(Self {
            path,
            state: Mutex::new(LogicalWalSpoolState { current_lsn }),
        })
    }

    pub fn append(&self, lsn: u64, data: &[u8]) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(LOGICAL_WAL_SPOOL_MAGIC)?;
        file.write_all(&[LOGICAL_WAL_SPOOL_VERSION])?;
        file.write_all(&lsn.to_le_bytes())?;
        file.write_all(&(data.len() as u64).to_le_bytes())?;
        file.write_all(data)?;
        file.flush()?;

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_lsn = state.current_lsn.max(lsn);
        Ok(())
    }

    pub fn read_since(&self, since_lsn: u64, max_count: usize) -> io::Result<Vec<(u64, Vec<u8>)>> {
        let entries = read_entries(&self.path)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.lsn > since_lsn)
            .take(max_count)
            .map(|entry| (entry.lsn, entry.data))
            .collect())
    }

    pub fn current_lsn(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current_lsn
    }

    pub fn oldest_lsn(&self) -> io::Result<Option<u64>> {
        Ok(read_entries(&self.path)?
            .into_iter()
            .next()
            .map(|entry| entry.lsn))
    }

    pub fn prune_through(&self, upto_lsn: u64) -> io::Result<()> {
        let previous_lsn = self.current_lsn();
        let retained: Vec<_> = read_entries(&self.path)?
            .into_iter()
            .filter(|entry| entry.lsn > upto_lsn)
            .collect();
        let temp_path = self.path.with_extension("logical.wal.tmp");
        let mut temp = File::create(&temp_path)?;
        let mut current_lsn = 0;
        for entry in retained {
            temp.write_all(LOGICAL_WAL_SPOOL_MAGIC)?;
            temp.write_all(&[LOGICAL_WAL_SPOOL_VERSION])?;
            temp.write_all(&entry.lsn.to_le_bytes())?;
            temp.write_all(&(entry.data.len() as u64).to_le_bytes())?;
            temp.write_all(&entry.data)?;
            current_lsn = current_lsn.max(entry.lsn);
        }
        temp.flush()?;
        fs::rename(&temp_path, &self.path)?;

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_lsn = previous_lsn.max(current_lsn).max(upto_lsn);
        Ok(())
    }
}

fn read_entries(path: &Path) -> io::Result<Vec<LogicalWalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = File::open(path)?;
    let mut entries = Vec::new();
    loop {
        let mut magic = [0u8; 4];
        match file.read_exact(&mut magic) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err),
        }
        if &magic != LOGICAL_WAL_SPOOL_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid logical wal spool magic",
            ));
        }

        let mut version = [0u8; 1];
        file.read_exact(&mut version)?;
        if version[0] != LOGICAL_WAL_SPOOL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported logical wal spool version",
            ));
        }

        let mut lsn = [0u8; 8];
        file.read_exact(&mut lsn)?;
        let mut len = [0u8; 8];
        file.read_exact(&mut len)?;
        let data_len = u64::from_le_bytes(len) as usize;
        let mut data = vec![0u8; data_len];
        file.read_exact(&mut data)?;
        entries.push(LogicalWalEntry {
            lsn: u64::from_le_bytes(lsn),
            data,
        });
    }
    Ok(entries)
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
    pub logical_wal_spool: Option<Arc<LogicalWalSpool>>,
    pub replicas: RwLock<Vec<ReplicaState>>,
}

impl PrimaryReplication {
    pub fn new(data_path: Option<&Path>) -> Self {
        Self {
            wal_buffer: Arc::new(WalBuffer::new(100_000)),
            logical_wal_spool: data_path
                .and_then(|path| LogicalWalSpool::open(path).ok())
                .map(Arc::new),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::cdc::{ChangeOperation, ChangeRecord};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_data_path(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("reddb_{name}_{suffix}.rdb"))
    }

    #[test]
    fn logical_wal_spool_roundtrip_and_prune() {
        let data_path = temp_data_path("logical_spool");
        let spool_path = LogicalWalSpool::path_for(&data_path);
        let spool = LogicalWalSpool::open(&data_path).expect("open spool");

        let record1 = ChangeRecord {
            lsn: 7,
            timestamp: 1,
            operation: ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: 10,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1, 2, 3]),
            metadata: None,
        };
        let record2 = ChangeRecord {
            lsn: 8,
            timestamp: 2,
            operation: ChangeOperation::Update,
            collection: "users".to_string(),
            entity_id: 10,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![4, 5, 6]),
            metadata: None,
        };

        spool
            .append(record1.lsn, &record1.encode())
            .expect("append 1");
        spool
            .append(record2.lsn, &record2.encode())
            .expect("append 2");

        let entries = spool.read_since(0, usize::MAX).expect("read");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, 7);
        assert_eq!(entries[1].0, 8);

        spool.prune_through(7).expect("prune");
        let retained = spool.read_since(0, usize::MAX).expect("read retained");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].0, 8);

        let _ = fs::remove_file(spool_path);
    }
}
