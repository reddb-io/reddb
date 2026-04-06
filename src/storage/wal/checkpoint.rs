//! Checkpoint Manager
//!
//! Responsible for transferring committed transactions from the WAL to the main
//! database file. Checkpointing ensures durability and allows WAL truncation.
//!
//! # Algorithm
//!
//! 1. Read all WAL records sequentially
//! 2. Track transaction states (Begin, Commit, Rollback)
//! 3. For committed transactions, collect PageWrite records
//! 4. Apply committed pages to the Pager in LSN order
//! 5. Sync Pager to disk
//! 6. Update checkpoint LSN in database header
//! 7. Truncate WAL
//!
//! # References
//!
//! - Turso `core/storage/wal.rs:checkpoint()` - Checkpoint logic
//! - SQLite WAL documentation

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use super::reader::WalReader;
use super::record::WalRecord;
use super::writer::WalWriter;
use crate::storage::engine::{Page, Pager, PAGE_SIZE};

/// Checkpoint mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointMode {
    /// Passive: Only checkpoint if no active writers
    Passive,
    /// Full: Wait for active writers to finish, then checkpoint all
    Full,
    /// Restart: Like Full, but also truncates the WAL
    Restart,
    /// Truncate: Checkpoint all and truncate WAL
    Truncate,
}

/// Checkpoint result statistics
#[derive(Debug, Clone, Default)]
pub struct CheckpointResult {
    /// Number of transactions processed
    pub transactions_processed: u64,
    /// Number of pages checkpointed
    pub pages_checkpointed: u64,
    /// Number of records processed
    pub records_processed: u64,
    /// Final LSN after checkpoint
    pub checkpoint_lsn: u64,
    /// Whether WAL was truncated
    pub wal_truncated: bool,
}

/// Checkpoint error types
#[derive(Debug)]
pub enum CheckpointError {
    /// I/O error
    Io(io::Error),
    /// Pager error
    Pager(String),
    /// WAL is corrupted
    CorruptedWal(String),
    /// No WAL file found
    NoWal,
}

impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Pager(msg) => write!(f, "Pager error: {}", msg),
            Self::CorruptedWal(msg) => write!(f, "Corrupted WAL: {}", msg),
            Self::NoWal => write!(f, "No WAL file found"),
        }
    }
}

impl std::error::Error for CheckpointError {}

impl From<io::Error> for CheckpointError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Transaction state during checkpoint
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TxState {
    Active,
    Committed,
    Aborted,
}

/// Pending page write from a transaction
#[derive(Debug)]
struct PendingWrite {
    tx_id: u64,
    page_id: u32,
    data: Vec<u8>,
    lsn: u64,
}

/// Checkpoint manager
///
/// Responsible for transferring committed transactions from the WAL to the main database file.
pub struct Checkpointer {
    /// Checkpoint mode
    mode: CheckpointMode,
}

impl Checkpointer {
    /// Create a new checkpointer with the given mode
    pub fn new(mode: CheckpointMode) -> Self {
        Self { mode }
    }

    /// Create a checkpointer with default mode (Full)
    pub fn default_mode() -> Self {
        Self::new(CheckpointMode::Full)
    }

    /// Perform a checkpoint
    ///
    /// Reads all records from the WAL and applies committed changes to the database.
    ///
    /// # Arguments
    ///
    /// * `pager` - The Pager to write committed pages to
    /// * `wal_path` - Path to the WAL file
    ///
    /// # Returns
    ///
    /// Checkpoint statistics or error
    pub fn checkpoint(
        &self,
        pager: &Pager,
        wal_path: &Path,
    ) -> Result<CheckpointResult, CheckpointError> {
        // Open WAL for reading
        let wal_reader = match WalReader::open(wal_path) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // No WAL file - nothing to checkpoint
                return Ok(CheckpointResult::default());
            }
            Err(e) => return Err(CheckpointError::Io(e)),
        };

        // Phase 1: Read and categorize all records
        let mut tx_states: HashMap<u64, TxState> = HashMap::new();
        let mut pending_writes: Vec<PendingWrite> = Vec::new();
        let mut records_processed: u64 = 0;
        let mut last_lsn: u64 = 0;

        for record_result in wal_reader.iter() {
            let (lsn, record) = record_result.map_err(CheckpointError::Io)?;
            records_processed += 1;
            last_lsn = lsn;

            match record {
                WalRecord::Begin { tx_id } => {
                    tx_states.insert(tx_id, TxState::Active);
                }
                WalRecord::Commit { tx_id } => {
                    tx_states.insert(tx_id, TxState::Committed);
                }
                WalRecord::Rollback { tx_id } => {
                    tx_states.insert(tx_id, TxState::Aborted);
                }
                WalRecord::PageWrite {
                    tx_id,
                    page_id,
                    data,
                } => {
                    pending_writes.push(PendingWrite {
                        tx_id,
                        page_id,
                        data,
                        lsn,
                    });
                }
                WalRecord::Checkpoint {
                    lsn: _checkpoint_lsn,
                } => {
                    // Checkpoint marker - we can skip records before this LSN
                    // For now, we process everything
                }
            }
        }

        // Phase 2: Filter for committed transactions only
        let committed_txs: HashSet<u64> = tx_states
            .iter()
            .filter(|(_, state)| **state == TxState::Committed)
            .map(|(tx_id, _)| *tx_id)
            .collect();

        // Phase 3: Collect pages from committed transactions
        // Keep only the latest write for each page (from committed txs)
        let mut latest_writes: HashMap<u32, Vec<u8>> = HashMap::new();

        for write in pending_writes {
            if committed_txs.contains(&write.tx_id) {
                // Always overwrite with later writes (they have higher LSN)
                latest_writes.insert(write.page_id, write.data);
            }
        }

        // Phase 4: Apply committed pages to Pager
        let mut pages_checkpointed: u64 = 0;

        for (page_id, data) in &latest_writes {
            // Reconstruct page from WAL data
            if data.len() != PAGE_SIZE {
                return Err(CheckpointError::CorruptedWal(format!(
                    "Page {} has wrong size: {} (expected {})",
                    page_id,
                    data.len(),
                    PAGE_SIZE
                )));
            }

            let mut page_data = [0u8; PAGE_SIZE];
            page_data.copy_from_slice(data);
            let page = Page::from_bytes(page_data);

            // Write to pager
            pager
                .write_page(*page_id, page)
                .map_err(|e| CheckpointError::Pager(e.to_string()))?;

            pages_checkpointed += 1;
        }

        // Phase 5: Sync Pager to disk
        pager
            .sync()
            .map_err(|e| CheckpointError::Pager(e.to_string()))?;

        // Phase 6: Truncate WAL if requested
        let wal_truncated = matches!(
            self.mode,
            CheckpointMode::Restart | CheckpointMode::Truncate
        );

        if wal_truncated {
            let mut wal_writer = WalWriter::open(wal_path)?;
            wal_writer.truncate()?;

            // Write checkpoint marker with current LSN
            let checkpoint_record = WalRecord::Checkpoint { lsn: last_lsn };
            wal_writer.append(&checkpoint_record)?;
            wal_writer.sync()?;
        }

        Ok(CheckpointResult {
            transactions_processed: committed_txs.len() as u64,
            pages_checkpointed,
            records_processed,
            checkpoint_lsn: last_lsn,
            wal_truncated,
        })
    }

    /// Perform crash recovery
    ///
    /// Called on database open to apply any committed transactions from the WAL
    /// that weren't checkpointed before the crash.
    ///
    /// # Arguments
    ///
    /// * `pager` - The Pager to recover into
    /// * `wal_path` - Path to the WAL file
    ///
    /// # Returns
    ///
    /// Recovery statistics or error
    pub fn recover(pager: &Pager, wal_path: &Path) -> Result<CheckpointResult, CheckpointError> {
        let checkpointer = Self::new(CheckpointMode::Truncate);
        checkpointer.checkpoint(pager, wal_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::PageType;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> std::path::PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("reddb_checkpoint_test_{}", timestamp))
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_checkpoint_empty_wal() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Pager::open_default(&db_path).unwrap();

        // No WAL file - should succeed with empty result
        let checkpointer = Checkpointer::default_mode();
        let result = checkpointer.checkpoint(&pager, &wal_path).unwrap();

        assert_eq!(result.transactions_processed, 0);
        assert_eq!(result.pages_checkpointed, 0);

        cleanup(&dir);
    }

    #[test]
    fn test_checkpoint_committed_transaction() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Pager::open_default(&db_path).unwrap();

        // Allocate a page to get its ID
        let page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page_id = page.page_id();

        // Create WAL with a committed transaction
        {
            let mut wal_writer = WalWriter::open(&wal_path).unwrap();

            // Begin transaction
            wal_writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();

            // Write a page
            let mut page_data = [0u8; PAGE_SIZE];
            page_data[0] = 0x42; // Mark with test byte
            wal_writer
                .append(&WalRecord::PageWrite {
                    tx_id: 1,
                    page_id,
                    data: page_data.to_vec(),
                })
                .unwrap();

            // Commit transaction
            wal_writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();

            wal_writer.sync().unwrap();
        }

        // Checkpoint
        let checkpointer = Checkpointer::new(CheckpointMode::Full);
        let result = checkpointer.checkpoint(&pager, &wal_path).unwrap();

        assert_eq!(result.transactions_processed, 1);
        assert_eq!(result.pages_checkpointed, 1);
        assert_eq!(result.records_processed, 3);

        // Verify page was written
        let read_page = pager.read_page(page_id).unwrap();
        assert_eq!(read_page.as_bytes()[0], 0x42);

        cleanup(&dir);
    }

    #[test]
    fn test_checkpoint_aborted_transaction() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Pager::open_default(&db_path).unwrap();

        // Allocate a page to get its ID
        let page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page_id = page.page_id();

        // Create WAL with an aborted transaction
        {
            let mut wal_writer = WalWriter::open(&wal_path).unwrap();

            // Begin transaction
            wal_writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();

            // Write a page
            let mut page_data = [0u8; PAGE_SIZE];
            page_data[0] = 0x42;
            wal_writer
                .append(&WalRecord::PageWrite {
                    tx_id: 1,
                    page_id,
                    data: page_data.to_vec(),
                })
                .unwrap();

            // Rollback transaction
            wal_writer
                .append(&WalRecord::Rollback { tx_id: 1 })
                .unwrap();

            wal_writer.sync().unwrap();
        }

        // Checkpoint
        let checkpointer = Checkpointer::new(CheckpointMode::Full);
        let result = checkpointer.checkpoint(&pager, &wal_path).unwrap();

        // Aborted transaction should not be checkpointed
        assert_eq!(result.transactions_processed, 0);
        assert_eq!(result.pages_checkpointed, 0);

        // Verify page was NOT written (should still be zeros)
        let read_page = pager.read_page(page_id).unwrap();
        assert_ne!(read_page.as_bytes()[0], 0x42);

        cleanup(&dir);
    }

    #[test]
    fn test_checkpoint_mixed_transactions() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Pager::open_default(&db_path).unwrap();

        // Allocate pages
        let page1 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page2 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page1_id = page1.page_id();
        let page2_id = page2.page_id();

        // Create WAL with mixed transactions
        {
            let mut wal_writer = WalWriter::open(&wal_path).unwrap();

            // Transaction 1: Committed
            wal_writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            let mut page_data1 = [0u8; PAGE_SIZE];
            page_data1[0] = 0x11;
            wal_writer
                .append(&WalRecord::PageWrite {
                    tx_id: 1,
                    page_id: page1_id,
                    data: page_data1.to_vec(),
                })
                .unwrap();
            wal_writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();

            // Transaction 2: Aborted
            wal_writer.append(&WalRecord::Begin { tx_id: 2 }).unwrap();
            let mut page_data2 = [0u8; PAGE_SIZE];
            page_data2[0] = 0x22;
            wal_writer
                .append(&WalRecord::PageWrite {
                    tx_id: 2,
                    page_id: page2_id,
                    data: page_data2.to_vec(),
                })
                .unwrap();
            wal_writer
                .append(&WalRecord::Rollback { tx_id: 2 })
                .unwrap();

            // Transaction 3: Committed
            wal_writer.append(&WalRecord::Begin { tx_id: 3 }).unwrap();
            let mut page_data3 = [0u8; PAGE_SIZE];
            page_data3[0] = 0x33;
            wal_writer
                .append(&WalRecord::PageWrite {
                    tx_id: 3,
                    page_id: page2_id,
                    data: page_data3.to_vec(),
                })
                .unwrap();
            wal_writer.append(&WalRecord::Commit { tx_id: 3 }).unwrap();

            wal_writer.sync().unwrap();
        }

        // Checkpoint
        let checkpointer = Checkpointer::new(CheckpointMode::Full);
        let result = checkpointer.checkpoint(&pager, &wal_path).unwrap();

        // Only committed transactions (1 and 3) should be processed
        assert_eq!(result.transactions_processed, 2);
        assert_eq!(result.pages_checkpointed, 2);

        // Verify pages
        let read_page1 = pager.read_page(page1_id).unwrap();
        assert_eq!(read_page1.as_bytes()[0], 0x11);

        let read_page2 = pager.read_page(page2_id).unwrap();
        assert_eq!(read_page2.as_bytes()[0], 0x33); // From tx 3, not tx 2

        cleanup(&dir);
    }

    #[test]
    fn test_checkpoint_truncate() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Pager::open_default(&db_path).unwrap();

        // Create WAL with a committed transaction
        {
            let mut wal_writer = WalWriter::open(&wal_path).unwrap();
            wal_writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            wal_writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();
            wal_writer.sync().unwrap();
        }

        // Checkpoint with truncate
        let checkpointer = Checkpointer::new(CheckpointMode::Truncate);
        let result = checkpointer.checkpoint(&pager, &wal_path).unwrap();

        assert!(result.wal_truncated);

        // WAL should be truncated (only header + checkpoint marker)
        let wal_size = fs::metadata(&wal_path).unwrap().len();
        // Header (8 bytes) + Checkpoint record (1 + 8 + 4 = 13 bytes)
        assert!(
            wal_size < 50,
            "WAL should be truncated, but size is {}",
            wal_size
        );

        cleanup(&dir);
    }
}
