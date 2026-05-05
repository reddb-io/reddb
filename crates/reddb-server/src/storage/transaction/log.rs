//! Write-Ahead Log (WAL) for Transaction Durability
//!
//! Provides crash recovery through sequential logging.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Transaction ID type
pub type TxnId = u64;

/// Log Sequence Number
pub type Lsn = u64;

/// Timestamp type
pub type Timestamp = u64;

fn read_bytes<'a>(
    data: &'a [u8],
    offset: &mut usize,
    len: usize,
    context: &'static str,
) -> io::Result<&'a [u8]> {
    let end = offset.saturating_add(len);
    if end > data.len() {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, context));
    }
    let bytes = &data[*offset..end];
    *offset = end;
    Ok(bytes)
}

fn read_array<const N: usize>(
    data: &[u8],
    offset: &mut usize,
    context: &'static str,
) -> io::Result<[u8; N]> {
    let bytes = read_bytes(data, offset, N, context)?;
    let mut array = [0u8; N];
    array.copy_from_slice(bytes);
    Ok(array)
}

fn read_u32(data: &[u8], offset: &mut usize, context: &'static str) -> io::Result<u32> {
    Ok(u32::from_le_bytes(read_array::<4>(data, offset, context)?))
}

fn read_u64(data: &[u8], offset: &mut usize, context: &'static str) -> io::Result<u64> {
    Ok(u64::from_le_bytes(read_array::<8>(data, offset, context)?))
}

fn io_lock_error(context: &'static str) -> io::Error {
    io::Error::other(format!("{context} lock poisoned"))
}

fn io_read_guard<'a, T>(
    lock: &'a RwLock<T>,
    context: &'static str,
) -> io::Result<RwLockReadGuard<'a, T>> {
    lock.read().map_err(|_| io_lock_error(context))
}

fn io_write_guard<'a, T>(
    lock: &'a RwLock<T>,
    context: &'static str,
) -> io::Result<RwLockWriteGuard<'a, T>> {
    lock.write().map_err(|_| io_lock_error(context))
}

fn io_mutex_guard<'a, T>(
    lock: &'a Mutex<T>,
    context: &'static str,
) -> io::Result<MutexGuard<'a, T>> {
    lock.lock().map_err(|_| io_lock_error(context))
}

fn recover_read_guard<'a, T>(lock: &'a RwLock<T>) -> RwLockReadGuard<'a, T> {
    match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

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
        let tag = read_bytes(data, &mut offset, 1, "Missing log entry tag")?[0];

        let entry = match tag {
            0 => LogEntryType::Begin,
            1 => LogEntryType::Commit,
            2 => LogEntryType::Abort,
            3 => {
                // Insert
                let key_len =
                    read_u32(data, &mut offset, "Missing WAL insert key length")? as usize;
                let key =
                    read_bytes(data, &mut offset, key_len, "Truncated WAL insert key")?.to_vec();
                let value_len =
                    read_u32(data, &mut offset, "Missing WAL insert value length")? as usize;
                let value = read_bytes(data, &mut offset, value_len, "Truncated WAL insert value")?
                    .to_vec();
                LogEntryType::Insert { key, value }
            }
            4 => {
                // Update
                let key_len =
                    read_u32(data, &mut offset, "Missing WAL update key length")? as usize;
                let key =
                    read_bytes(data, &mut offset, key_len, "Truncated WAL update key")?.to_vec();
                let old_len =
                    read_u32(data, &mut offset, "Missing WAL update old value length")? as usize;
                let old_value =
                    read_bytes(data, &mut offset, old_len, "Truncated WAL update old value")?
                        .to_vec();
                let new_len =
                    read_u32(data, &mut offset, "Missing WAL update new value length")? as usize;
                let new_value =
                    read_bytes(data, &mut offset, new_len, "Truncated WAL update new value")?
                        .to_vec();
                LogEntryType::Update {
                    key,
                    old_value,
                    new_value,
                }
            }
            5 => {
                // Delete
                let key_len =
                    read_u32(data, &mut offset, "Missing WAL delete key length")? as usize;
                let key =
                    read_bytes(data, &mut offset, key_len, "Truncated WAL delete key")?.to_vec();
                let old_len =
                    read_u32(data, &mut offset, "Missing WAL delete old value length")? as usize;
                let old_value =
                    read_bytes(data, &mut offset, old_len, "Truncated WAL delete old value")?
                        .to_vec();
                LogEntryType::Delete { key, old_value }
            }
            6 => {
                // Checkpoint
                let count =
                    read_u32(data, &mut offset, "Missing WAL checkpoint txn count")? as usize;
                let mut active_txns = Vec::with_capacity(count);
                for _ in 0..count {
                    let txn =
                        read_u64(data, &mut offset, "Truncated WAL checkpoint transaction id")?;
                    active_txns.push(txn);
                }
                LogEntryType::Checkpoint { active_txns }
            }
            7 => {
                // Savepoint
                let name_len =
                    read_u32(data, &mut offset, "Missing WAL savepoint name length")? as usize;
                let name = String::from_utf8_lossy(read_bytes(
                    data,
                    &mut offset,
                    name_len,
                    "Truncated WAL savepoint name",
                )?)
                .to_string();
                LogEntryType::Savepoint { name }
            }
            8 => {
                // RollbackToSavepoint
                let name_len = read_u32(
                    data,
                    &mut offset,
                    "Missing WAL rollback-to-savepoint name length",
                )? as usize;
                let name = String::from_utf8_lossy(read_bytes(
                    data,
                    &mut offset,
                    name_len,
                    "Truncated WAL rollback-to-savepoint name",
                )?)
                .to_string();
                LogEntryType::RollbackToSavepoint { name }
            }
            9 => {
                // Compensate
                let original_lsn =
                    read_u64(data, &mut offset, "Truncated WAL compensate original LSN")?;
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

        let mut offset = 0;
        let lsn = read_u64(data, &mut offset, "Missing WAL entry LSN")?;
        let txn_id = read_u64(data, &mut offset, "Missing WAL entry txn id")?;
        let prev_lsn_raw = read_u64(data, &mut offset, "Missing WAL entry prev_lsn")?;
        let prev_lsn = if prev_lsn_raw == 0 {
            None
        } else {
            Some(prev_lsn_raw)
        };
        let timestamp = read_u64(data, &mut offset, "Missing WAL entry timestamp")?;
        let type_len = read_u32(data, &mut offset, "Missing WAL entry type length")? as usize;
        let entry_type_bytes = read_bytes(
            data,
            &mut offset,
            type_len,
            "Truncated WAL entry type bytes",
        )?;
        let (entry_type, consumed) = LogEntryType::from_bytes(entry_type_bytes)?;
        if consumed != entry_type_bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WAL entry type length mismatch",
            ));
        }

        // Verify checksum
        let stored_checksum = *data.get(offset).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "Missing WAL entry checksum")
        })?;
        let computed: u8 = data[..offset].iter().fold(0, |acc, &b| acc ^ b);
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
            let mut prev_lsns = io_write_guard(&self.txn_prev_lsn, "wal prev_lsn map")?;
            entry.prev_lsn = prev_lsns.get(&entry.txn_id).copied();
            prev_lsns.insert(entry.txn_id, lsn);
        }

        let bytes = entry.to_bytes();

        // Write to file if available
        if let Some(ref file) = self.file {
            let mut writer = io_mutex_guard(file, "wal file")?;
            // Write length prefix
            writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
            writer.write_all(&bytes)?;

            // Sync on commit if configured
            if self.config.sync_on_commit && entry.entry_type.is_commit() {
                writer.flush()?;
                writer.get_mut().sync_all()?;

                let mut stats = io_write_guard(&self.stats, "wal stats")?;
                stats.syncs += 1;
            }
        }

        // Store in buffer
        {
            let mut buffer = io_write_guard(&self.buffer, "wal buffer")?;
            buffer.push_back(entry);

            // Limit buffer size
            while buffer.len() > 10000 {
                buffer.pop_front();
            }
        }

        // Update stats
        {
            let mut stats = io_write_guard(&self.stats, "wal stats")?;
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
            let mut prev_lsns = io_write_guard(&self.txn_prev_lsn, "wal prev_lsn map")?;
            prev_lsns.remove(&txn_id);
        }

        Ok(lsn)
    }

    /// Log transaction abort
    pub fn log_abort(&self, txn_id: TxnId) -> io::Result<Lsn> {
        let lsn = self.append(LogEntry::new(txn_id, None, LogEntryType::Abort))?;

        // Clean up prev_lsn tracking
        {
            let mut prev_lsns = io_write_guard(&self.txn_prev_lsn, "wal prev_lsn map")?;
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
            let mut writer = io_mutex_guard(file, "wal file")?;
            writer.flush()?;
            writer.get_mut().sync_all()?;
        }

        self.last_checkpoint_lsn.store(lsn, Ordering::SeqCst);

        {
            let mut stats = io_write_guard(&self.stats, "wal stats")?;
            stats.checkpoints += 1;
        }

        Ok(lsn)
    }

    /// Flush buffer to disk
    pub fn flush(&self) -> io::Result<()> {
        if let Some(ref file) = self.file {
            let mut writer = io_mutex_guard(file, "wal file")?;
            writer.flush()?;
            writer.get_mut().sync_all()?;
        }
        Ok(())
    }

    /// Get entries for a transaction (for undo)
    pub fn get_txn_entries(&self, txn_id: TxnId) -> Vec<LogEntry> {
        let buffer = recover_read_guard(&self.buffer);
        buffer
            .iter()
            .filter(|e| e.txn_id == txn_id)
            .cloned()
            .collect()
    }

    /// Get entries since LSN
    pub fn get_entries_since(&self, lsn: Lsn) -> Vec<LogEntry> {
        let buffer = recover_read_guard(&self.buffer);
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
        recover_read_guard(&self.stats).clone()
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

    #[test]
    fn test_log_entry_type_rejects_truncated_insert() {
        let err = LogEntryType::from_bytes(&[3, 4, 0, 0, 0, b'k'])
            .expect_err("truncated insert should fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn test_log_entry_rejects_truncated_type_payload() {
        let entry = LogEntry {
            lsn: 7,
            txn_id: 9,
            prev_lsn: Some(3),
            timestamp: 42,
            entry_type: LogEntryType::Insert {
                key: b"hello".to_vec(),
                value: b"world".to_vec(),
            },
        };

        let mut bytes = entry.to_bytes();
        bytes.truncate(bytes.len() - 2);

        let err = LogEntry::from_bytes(&bytes).expect_err("truncated entry should fail");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
