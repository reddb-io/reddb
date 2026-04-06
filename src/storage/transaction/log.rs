//! Write-Ahead Log (WAL) for Transaction Durability
//!
//! Provides crash recovery through sequential logging.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Transaction ID type
pub type TxnId = u64;

/// Log Sequence Number
pub type Lsn = u64;

/// Timestamp type
pub type Timestamp = u64;

/// Log entry types
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEntryType {
    /// Transaction begin
    Begin,
    /// Transaction commit
    Commit,
    /// Transaction abort
    Abort,
    /// Data insert
    Insert { key: Vec<u8>, value: Vec<u8> },
    /// Data update (before and after images)
    Update {
        key: Vec<u8>,
        old_value: Vec<u8>,
        new_value: Vec<u8>,
    },
    /// Data delete
    Delete { key: Vec<u8>, old_value: Vec<u8> },
    /// Checkpoint marker
    Checkpoint { active_txns: Vec<TxnId> },
    /// Savepoint creation
    Savepoint { name: String },
    /// Savepoint rollback
    RollbackToSavepoint { name: String },
    /// Compensation log record (for undo)
    Compensate { original_lsn: Lsn },
    /// End of transaction (after all cleanup)
    End,
}

impl LogEntryType {
    /// Check if this is a commit record
    pub fn is_commit(&self) -> bool {
        matches!(self, LogEntryType::Commit)
    }

    /// Check if this is an abort record
    pub fn is_abort(&self) -> bool {
        matches!(self, LogEntryType::Abort)
    }

    /// Check if this is a data modification
    pub fn is_data_modification(&self) -> bool {
        matches!(
            self,
            LogEntryType::Insert { .. } | LogEntryType::Update { .. } | LogEntryType::Delete { .. }
        )
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        match self {
            LogEntryType::Begin => buf.push(0),
            LogEntryType::Commit => buf.push(1),
            LogEntryType::Abort => buf.push(2),
            LogEntryType::Insert { key, value } => {
                buf.push(3);
                buf.extend(&(key.len() as u32).to_le_bytes());
                buf.extend(key);
                buf.extend(&(value.len() as u32).to_le_bytes());
                buf.extend(value);
            }
            LogEntryType::Update {
                key,
                old_value,
                new_value,
            } => {
                buf.push(4);
                buf.extend(&(key.len() as u32).to_le_bytes());
                buf.extend(key);
                buf.extend(&(old_value.len() as u32).to_le_bytes());
                buf.extend(old_value);
                buf.extend(&(new_value.len() as u32).to_le_bytes());
                buf.extend(new_value);
            }
            LogEntryType::Delete { key, old_value } => {
                buf.push(5);
                buf.extend(&(key.len() as u32).to_le_bytes());
                buf.extend(key);
                buf.extend(&(old_value.len() as u32).to_le_bytes());
                buf.extend(old_value);
            }
            LogEntryType::Checkpoint { active_txns } => {
                buf.push(6);
                buf.extend(&(active_txns.len() as u32).to_le_bytes());
                for txn in active_txns {
                    buf.extend(&txn.to_le_bytes());
                }
            }
            LogEntryType::Savepoint { name } => {
                buf.push(7);
                let name_bytes = name.as_bytes();
                buf.extend(&(name_bytes.len() as u32).to_le_bytes());
                buf.extend(name_bytes);
            }
            LogEntryType::RollbackToSavepoint { name } => {
                buf.push(8);
                let name_bytes = name.as_bytes();
                buf.extend(&(name_bytes.len() as u32).to_le_bytes());
                buf.extend(name_bytes);
            }
            LogEntryType::Compensate { original_lsn } => {
                buf.push(9);
                buf.extend(&original_lsn.to_le_bytes());
            }
            LogEntryType::End => buf.push(10),
        }

        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> io::Result<(Self, usize)> {
        if data.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty data"));
        }

        let mut offset = 0;
        let tag = data[offset];
        offset += 1;

        let entry = match tag {
            0 => LogEntryType::Begin,
            1 => LogEntryType::Commit,
            2 => LogEntryType::Abort,
            3 => {
                // Insert
                let key_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let key = data[offset..offset + key_len].to_vec();
                offset += key_len;
                let value_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let value = data[offset..offset + value_len].to_vec();
                offset += value_len;
                LogEntryType::Insert { key, value }
            }
            4 => {
                // Update
                let key_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let key = data[offset..offset + key_len].to_vec();
                offset += key_len;
                let old_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let old_value = data[offset..offset + old_len].to_vec();
                offset += old_len;
                let new_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let new_value = data[offset..offset + new_len].to_vec();
                offset += new_len;
                LogEntryType::Update {
                    key,
                    old_value,
                    new_value,
                }
            }
            5 => {
                // Delete
                let key_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let key = data[offset..offset + key_len].to_vec();
                offset += key_len;
                let old_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let old_value = data[offset..offset + old_len].to_vec();
                offset += old_len;
                LogEntryType::Delete { key, old_value }
            }
            6 => {
                // Checkpoint
                let count =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let mut active_txns = Vec::with_capacity(count);
                for _ in 0..count {
                    let txn = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                    offset += 8;
                    active_txns.push(txn);
                }
                LogEntryType::Checkpoint { active_txns }
            }
            7 => {
                // Savepoint
                let name_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let name = String::from_utf8_lossy(&data[offset..offset + name_len]).to_string();
                offset += name_len;
                LogEntryType::Savepoint { name }
            }
            8 => {
                // RollbackToSavepoint
                let name_len =
                    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
                offset += 4;
                let name = String::from_utf8_lossy(&data[offset..offset + name_len]).to_string();
                offset += name_len;
                LogEntryType::RollbackToSavepoint { name }
            }
            9 => {
                // Compensate
                let original_lsn = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
                offset += 8;
                LogEntryType::Compensate { original_lsn }
            }
            10 => LogEntryType::End,
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid tag")),
        };

        Ok((entry, offset))
    }
}

/// A single log entry
#[derive(Debug, Clone)]
pub struct LogEntry {
    /// Log sequence number
    pub lsn: Lsn,
    /// Transaction ID
    pub txn_id: TxnId,
    /// Previous LSN for this transaction (for undo chain)
    pub prev_lsn: Option<Lsn>,
    /// Timestamp
    pub timestamp: Timestamp,
    /// Entry type
    pub entry_type: LogEntryType,
}

impl LogEntry {
    /// Create new log entry
    pub fn new(txn_id: TxnId, prev_lsn: Option<Lsn>, entry_type: LogEntryType) -> Self {
        Self {
            lsn: 0, // Will be assigned by log
            txn_id,
            prev_lsn,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as Timestamp,
            entry_type,
        }
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();

        // Header: lsn(8) + txn_id(8) + prev_lsn(8) + timestamp(8) = 32 bytes
        buf.extend(&self.lsn.to_le_bytes());
        buf.extend(&self.txn_id.to_le_bytes());
        buf.extend(&self.prev_lsn.unwrap_or(0).to_le_bytes());
        buf.extend(&self.timestamp.to_le_bytes());

        // Entry type
        let type_bytes = self.entry_type.to_bytes();
        buf.extend(&(type_bytes.len() as u32).to_le_bytes());
        buf.extend(&type_bytes);

        // Checksum (simple XOR for demo)
        let checksum: u8 = buf.iter().fold(0, |acc, &b| acc ^ b);
        buf.push(checksum);

        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> io::Result<Self> {
        if data.len() < 37 {
            // 32 header + 4 type_len + 1 checksum minimum
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Too short"));
        }

        let lsn = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let txn_id = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let prev_lsn_raw = u64::from_le_bytes(data[16..24].try_into().unwrap());
        let prev_lsn = if prev_lsn_raw == 0 {
            None
        } else {
            Some(prev_lsn_raw)
        };
        let timestamp = u64::from_le_bytes(data[24..32].try_into().unwrap());
        let type_len = u32::from_le_bytes(data[32..36].try_into().unwrap()) as usize;

        let (entry_type, _) = LogEntryType::from_bytes(&data[36..36 + type_len])?;

        // Verify checksum
        let stored_checksum = data[36 + type_len];
        let computed: u8 = data[..36 + type_len].iter().fold(0, |acc, &b| acc ^ b);
        if stored_checksum != computed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Checksum mismatch",
            ));
        }

        Ok(Self {
            lsn,
            txn_id,
            prev_lsn,
            timestamp,
            entry_type,
        })
    }

    /// Get the size of this entry when serialized
    pub fn serialized_size(&self) -> usize {
        32 + 4 + self.entry_type.to_bytes().len() + 1
    }
}

/// WAL configuration
#[derive(Debug, Clone)]
pub struct WalConfig {
    /// Log file path
    pub path: PathBuf,
    /// Sync mode (fsync after each write)
    pub sync_on_commit: bool,
    /// Buffer size
    pub buffer_size: usize,
    /// Maximum log file size before rotation
    pub max_file_size: u64,
    /// Checkpoint interval (in entries)
    pub checkpoint_interval: u64,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("wal.log"),
            sync_on_commit: true,
            buffer_size: 64 * 1024,           // 64KB
            max_file_size: 100 * 1024 * 1024, // 100MB
            checkpoint_interval: 1000,
        }
    }
}

impl WalConfig {
    /// Create config with path
    pub fn with_path<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            ..Default::default()
        }
    }
}

/// WAL statistics
#[derive(Debug, Clone, Default)]
pub struct WalStats {
    /// Total entries written
    pub entries_written: u64,
    /// Total bytes written
    pub bytes_written: u64,
    /// Total syncs
    pub syncs: u64,
    /// Checkpoints performed
    pub checkpoints: u64,
    /// Current file size
    pub file_size: u64,
}

/// Transaction Log (WAL)
pub struct TransactionLog {
    /// Configuration
    config: WalConfig,
    /// Next LSN to assign
    next_lsn: AtomicU64,
    /// Log file (optional for in-memory mode)
    file: Option<Mutex<BufWriter<File>>>,
    /// In-memory buffer for entries
    buffer: RwLock<VecDeque<LogEntry>>,
    /// Transaction prev_lsn tracking
    txn_prev_lsn: RwLock<std::collections::HashMap<TxnId, Lsn>>,
    /// Statistics
    stats: RwLock<WalStats>,
    /// Last checkpoint LSN
    last_checkpoint_lsn: AtomicU64,
}

impl TransactionLog {
    /// Create new transaction log
    pub fn new(config: WalConfig) -> io::Result<Self> {
        let file = if config.path.as_os_str().is_empty() {
            None
        } else {
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(&config.path)?;
            Some(Mutex::new(BufWriter::with_capacity(config.buffer_size, f)))
        };

        Ok(Self {
            config,
            next_lsn: AtomicU64::new(1),
            file,
            buffer: RwLock::new(VecDeque::new()),
            txn_prev_lsn: RwLock::new(std::collections::HashMap::new()),
            stats: RwLock::new(WalStats::default()),
            last_checkpoint_lsn: AtomicU64::new(0),
        })
    }

    /// Create in-memory log (no persistence)
    pub fn in_memory() -> Self {
        Self {
            config: WalConfig {
                path: PathBuf::new(),
                ..Default::default()
            },
            next_lsn: AtomicU64::new(1),
            file: None,
            buffer: RwLock::new(VecDeque::new()),
            txn_prev_lsn: RwLock::new(std::collections::HashMap::new()),
            stats: RwLock::new(WalStats::default()),
            last_checkpoint_lsn: AtomicU64::new(0),
        }
    }

    /// Append entry to log
    pub fn append(&self, mut entry: LogEntry) -> io::Result<Lsn> {
        // Assign LSN
        let lsn = self.next_lsn.fetch_add(1, Ordering::SeqCst);
        entry.lsn = lsn;

        // Update prev_lsn tracking
        {
            let mut prev_lsns = self.txn_prev_lsn.write().unwrap();
            entry.prev_lsn = prev_lsns.get(&entry.txn_id).copied();
            prev_lsns.insert(entry.txn_id, lsn);
        }

        let bytes = entry.to_bytes();

        // Write to file if available
        if let Some(ref file) = self.file {
            let mut writer = file.lock().unwrap();
            // Write length prefix
            writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
            writer.write_all(&bytes)?;

            // Sync on commit if configured
            if self.config.sync_on_commit && entry.entry_type.is_commit() {
                writer.flush()?;
                writer.get_mut().sync_all()?;

                let mut stats = self.stats.write().unwrap();
                stats.syncs += 1;
            }
        }

        // Store in buffer
        {
            let mut buffer = self.buffer.write().unwrap();
            buffer.push_back(entry);

            // Limit buffer size
            while buffer.len() > 10000 {
                buffer.pop_front();
            }
        }

        // Update stats
        {
            let mut stats = self.stats.write().unwrap();
            stats.entries_written += 1;
            stats.bytes_written += bytes.len() as u64 + 4;
            stats.file_size += bytes.len() as u64 + 4;
        }

        Ok(lsn)
    }

    /// Log transaction begin
    pub fn log_begin(&self, txn_id: TxnId) -> io::Result<Lsn> {
        self.append(LogEntry::new(txn_id, None, LogEntryType::Begin))
    }

    /// Log transaction commit
    pub fn log_commit(&self, txn_id: TxnId) -> io::Result<Lsn> {
        let lsn = self.append(LogEntry::new(txn_id, None, LogEntryType::Commit))?;

        // Clean up prev_lsn tracking
        {
            let mut prev_lsns = self.txn_prev_lsn.write().unwrap();
            prev_lsns.remove(&txn_id);
        }

        Ok(lsn)
    }

    /// Log transaction abort
    pub fn log_abort(&self, txn_id: TxnId) -> io::Result<Lsn> {
        let lsn = self.append(LogEntry::new(txn_id, None, LogEntryType::Abort))?;

        // Clean up prev_lsn tracking
        {
            let mut prev_lsns = self.txn_prev_lsn.write().unwrap();
            prev_lsns.remove(&txn_id);
        }

        Ok(lsn)
    }

    /// Log insert operation
    pub fn log_insert(&self, txn_id: TxnId, key: Vec<u8>, value: Vec<u8>) -> io::Result<Lsn> {
        self.append(LogEntry::new(
            txn_id,
            None,
            LogEntryType::Insert { key, value },
        ))
    }

    /// Log update operation
    pub fn log_update(
        &self,
        txn_id: TxnId,
        key: Vec<u8>,
        old_value: Vec<u8>,
        new_value: Vec<u8>,
    ) -> io::Result<Lsn> {
        self.append(LogEntry::new(
            txn_id,
            None,
            LogEntryType::Update {
                key,
                old_value,
                new_value,
            },
        ))
    }

    /// Log delete operation
    pub fn log_delete(&self, txn_id: TxnId, key: Vec<u8>, old_value: Vec<u8>) -> io::Result<Lsn> {
        self.append(LogEntry::new(
            txn_id,
            None,
            LogEntryType::Delete { key, old_value },
        ))
    }

    /// Log savepoint
    pub fn log_savepoint(&self, txn_id: TxnId, name: String) -> io::Result<Lsn> {
        self.append(LogEntry::new(
            txn_id,
            None,
            LogEntryType::Savepoint { name },
        ))
    }

    /// Write checkpoint
    pub fn checkpoint(&self, active_txns: Vec<TxnId>) -> io::Result<Lsn> {
        let lsn = self.append(LogEntry::new(
            0, // System transaction
            None,
            LogEntryType::Checkpoint { active_txns },
        ))?;

        // Force sync
        if let Some(ref file) = self.file {
            let mut writer = file.lock().unwrap();
            writer.flush()?;
            writer.get_mut().sync_all()?;
        }

        self.last_checkpoint_lsn.store(lsn, Ordering::SeqCst);

        {
            let mut stats = self.stats.write().unwrap();
            stats.checkpoints += 1;
        }

        Ok(lsn)
    }

    /// Flush buffer to disk
    pub fn flush(&self) -> io::Result<()> {
        if let Some(ref file) = self.file {
            let mut writer = file.lock().unwrap();
            writer.flush()?;
            writer.get_mut().sync_all()?;
        }
        Ok(())
    }

    /// Get entries for a transaction (for undo)
    pub fn get_txn_entries(&self, txn_id: TxnId) -> Vec<LogEntry> {
        let buffer = self.buffer.read().unwrap();
        buffer
            .iter()
            .filter(|e| e.txn_id == txn_id)
            .cloned()
            .collect()
    }

    /// Get entries since LSN
    pub fn get_entries_since(&self, lsn: Lsn) -> Vec<LogEntry> {
        let buffer = self.buffer.read().unwrap();
        buffer.iter().filter(|e| e.lsn >= lsn).cloned().collect()
    }

    /// Get current LSN
    pub fn current_lsn(&self) -> Lsn {
        self.next_lsn.load(Ordering::SeqCst) - 1
    }

    /// Get last checkpoint LSN
    pub fn last_checkpoint(&self) -> Lsn {
        self.last_checkpoint_lsn.load(Ordering::SeqCst)
    }

    /// Get statistics
    pub fn stats(&self) -> WalStats {
        self.stats.read().unwrap().clone()
    }

    /// Get configuration
    pub fn config(&self) -> &WalConfig {
        &self.config
    }
}

/// Log reader for recovery
pub struct LogReader {
    reader: BufReader<File>,
}

impl LogReader {
    /// Open log file for reading
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self {
            reader: BufReader::new(file),
        })
    }

    /// Read all entries
    pub fn read_all(&mut self) -> io::Result<Vec<LogEntry>> {
        let mut entries = Vec::new();

        loop {
            match self.read_entry() {
                Ok(entry) => entries.push(entry),
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }

        Ok(entries)
    }

    /// Read single entry
    pub fn read_entry(&mut self) -> io::Result<LogEntry> {
        let mut len_buf = [0u8; 4];
        self.reader.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        let mut data = vec![0u8; len];
        self.reader.read_exact(&mut data)?;

        LogEntry::from_bytes(&data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_entry_serialize() {
        let entry = LogEntry {
            lsn: 42,
            txn_id: 1,
            prev_lsn: Some(40),
            timestamp: 1234567890,
            entry_type: LogEntryType::Insert {
                key: b"key1".to_vec(),
                value: b"value1".to_vec(),
            },
        };

        let bytes = entry.to_bytes();
        let recovered = LogEntry::from_bytes(&bytes).unwrap();

        assert_eq!(recovered.lsn, entry.lsn);
        assert_eq!(recovered.txn_id, entry.txn_id);
        assert_eq!(recovered.prev_lsn, entry.prev_lsn);
    }

    #[test]
    fn test_in_memory_log() {
        let log = TransactionLog::in_memory();

        let lsn1 = log.log_begin(1).unwrap();
        let lsn2 = log
            .log_insert(1, b"key".to_vec(), b"value".to_vec())
            .unwrap();
        let lsn3 = log.log_commit(1).unwrap();

        assert_eq!(lsn1, 1);
        assert_eq!(lsn2, 2);
        assert_eq!(lsn3, 3);

        let entries = log.get_txn_entries(1);
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_checkpoint() {
        let log = TransactionLog::in_memory();

        log.log_begin(1).unwrap();
        log.log_begin(2).unwrap();

        let cp_lsn = log.checkpoint(vec![1, 2]).unwrap();
        assert_eq!(log.last_checkpoint(), cp_lsn);
    }

    #[test]
    fn test_log_entry_types() {
        let types = vec![
            LogEntryType::Begin,
            LogEntryType::Commit,
            LogEntryType::Abort,
            LogEntryType::Insert {
                key: b"k".to_vec(),
                value: b"v".to_vec(),
            },
            LogEntryType::Update {
                key: b"k".to_vec(),
                old_value: b"old".to_vec(),
                new_value: b"new".to_vec(),
            },
            LogEntryType::Delete {
                key: b"k".to_vec(),
                old_value: b"v".to_vec(),
            },
            LogEntryType::Checkpoint {
                active_txns: vec![1, 2, 3],
            },
            LogEntryType::Savepoint {
                name: "sp1".to_string(),
            },
            LogEntryType::End,
        ];

        for t in types {
            let bytes = t.to_bytes();
            let (recovered, _) = LogEntryType::from_bytes(&bytes).unwrap();
            assert_eq!(recovered, t);
        }
    }

    #[test]
    fn test_prev_lsn_chain() {
        let log = TransactionLog::in_memory();

        log.log_begin(1).unwrap(); // LSN 1, prev_lsn = None
        log.log_insert(1, b"k1".to_vec(), b"v1".to_vec()).unwrap(); // LSN 2, prev_lsn = 1
        log.log_insert(1, b"k2".to_vec(), b"v2".to_vec()).unwrap(); // LSN 3, prev_lsn = 2

        let entries = log.get_txn_entries(1);
        assert_eq!(entries[0].prev_lsn, None);
        assert_eq!(entries[1].prev_lsn, Some(1));
        assert_eq!(entries[2].prev_lsn, Some(2));
    }
}
