//! Transaction Manager
//!
//! Provides ACID transaction support for the RedDB storage engine.
//!
//! # Transaction Lifecycle
//!
//! 1. Begin: Allocate transaction ID, write Begin record to WAL
//! 2. Read/Write: Track page reads and buffer page writes
//! 3. Commit: Write Commit record to WAL, sync WAL
//! 4. Rollback: Write Rollback record to WAL, discard buffered writes
//!
//! # Isolation Level
//!
//! Currently implements Read Committed isolation:
//! - Reads see committed data at the start of the statement
//! - No dirty reads
//! - Possible non-repeatable reads
//!
//! # References
//!
//! - Turso `core/transaction.rs` - Transaction implementation
//! - SQLite transaction documentation

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::record::WalRecord;
use super::writer::WalWriter;
use crate::storage::engine::{Page, Pager, PAGE_SIZE};

/// Global transaction ID counter
static NEXT_TX_ID: AtomicU64 = AtomicU64::new(1);

/// Generate a new unique transaction ID
fn next_transaction_id() -> u64 {
    NEXT_TX_ID.fetch_add(1, Ordering::SeqCst)
}

/// Transaction state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    /// Transaction is active and can perform operations
    Active,
    /// Transaction has been committed
    Committed,
    /// Transaction has been rolled back
    Aborted,
}

/// Transaction error types
#[derive(Debug)]
pub enum TxError {
    /// I/O error
    Io(io::Error),
    /// Pager error
    Pager(String),
    /// Internal lock was poisoned by a panic
    LockPoisoned(&'static str),
    /// Transaction is not active
    NotActive,
    /// Transaction already committed
    AlreadyCommitted,
    /// Transaction already aborted
    AlreadyAborted,
    /// Write conflict
    WriteConflict(u32),
    /// Invalid page data
    InvalidPage(String),
}

impl std::fmt::Display for TxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Pager(msg) => write!(f, "Pager error: {}", msg),
            Self::LockPoisoned(name) => write!(f, "Lock poisoned: {}", name),
            Self::NotActive => write!(f, "Transaction is not active"),
            Self::AlreadyCommitted => write!(f, "Transaction already committed"),
            Self::AlreadyAborted => write!(f, "Transaction already aborted"),
            Self::WriteConflict(page_id) => write!(f, "Write conflict on page {}", page_id),
            Self::InvalidPage(msg) => write!(f, "Invalid page: {}", msg),
        }
    }
}

impl std::error::Error for TxError {}

impl From<io::Error> for TxError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A buffered page write
#[derive(Clone)]
struct BufferedWrite {
    page_id: u32,
    data: [u8; PAGE_SIZE],
}

/// A single transaction
///
/// Transactions buffer writes and commit them atomically to the WAL.
pub struct Transaction {
    /// Transaction ID
    id: u64,
    /// Transaction state
    state: TxState,
    /// Buffered page writes (page_id -> page data)
    write_set: HashMap<u32, BufferedWrite>,
    /// Pages read in this transaction (for conflict detection)
    read_set: Vec<u32>,
    /// Reference to the transaction manager
    manager: Arc<TransactionManager>,
}

impl Transaction {
    /// Get transaction ID
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Get transaction state
    pub fn state(&self) -> TxState {
        self.state
    }

    /// Check if transaction is active
    pub fn is_active(&self) -> bool {
        self.state == TxState::Active
    }

    /// Read a page through this transaction
    ///
    /// If the page has been written in this transaction, returns the buffered version.
    /// Otherwise, reads from the pager.
    pub fn read_page(&mut self, page_id: u32) -> Result<Page, TxError> {
        if self.state != TxState::Active {
            return Err(TxError::NotActive);
        }

        // Check write set first
        if let Some(buffered) = self.write_set.get(&page_id) {
            return Ok(Page::from_bytes(buffered.data));
        }

        // Track the read
        self.read_set.push(page_id);

        // Read from pager
        self.manager
            .pager
            .read_page(page_id)
            .map_err(|e| TxError::Pager(e.to_string()))
    }

    /// Write a page through this transaction
    ///
    /// The write is buffered and will be committed to the WAL on commit.
    pub fn write_page(&mut self, page_id: u32, page: Page) -> Result<(), TxError> {
        if self.state != TxState::Active {
            return Err(TxError::NotActive);
        }

        // Buffer the write
        let mut data = [0u8; PAGE_SIZE];
        data.copy_from_slice(page.as_bytes());

        self.write_set
            .insert(page_id, BufferedWrite { page_id, data });

        Ok(())
    }

    /// Commit the transaction
    ///
    /// Writes all buffered pages to the WAL, then writes a Commit record.
    ///
    /// **Read-only fast path:** when `write_set` is empty, the
    /// transaction wrote nothing, so there is nothing to make
    /// durable. We skip the WAL append, the `wal.sync()` (which costs
    /// ~100 µs of fsync), and the pager apply loop entirely. The
    /// transaction still transitions to `Committed` and unregisters
    /// from the manager so subsequent state checks work correctly.
    /// This mirrors postgres' optimisation in `RecordTransactionCommit`
    /// (`xact.c`) which skips `XLogFlush` when nothing was written.
    pub fn commit(mut self) -> Result<(), TxError> {
        if self.state != TxState::Active {
            return match self.state {
                TxState::Committed => Err(TxError::AlreadyCommitted),
                TxState::Aborted => Err(TxError::AlreadyAborted),
                _ => Err(TxError::NotActive),
            };
        }

        // ── Read-only fast path ─────────────────────────────────────
        // No writes → no WAL record → no fsync. Saves ~100 µs per
        // read-only commit and removes contention on the WAL writer
        // mutex for read-heavy workloads.
        if self.write_set.is_empty() {
            self.state = TxState::Committed;
            self.manager.unregister_transaction(self.id);
            return Ok(());
        }

        // Write all buffered pages to WAL
        let mut wal = self.manager.wal_writer()?;

        for (page_id, buffered) in &self.write_set {
            let record = WalRecord::PageWrite {
                tx_id: self.id,
                page_id: *page_id,
                data: buffered.data.to_vec(),
            };
            wal.append(&record)?;
        }

        // Write commit record
        let commit_record = WalRecord::Commit { tx_id: self.id };
        wal.append(&commit_record)?;

        // Sync WAL to disk
        wal.sync()?;

        // Apply writes to pager cache (for immediate visibility)
        for (page_id, buffered) in &self.write_set {
            let page = Page::from_bytes(buffered.data);
            self.manager
                .pager
                .write_page(*page_id, page)
                .map_err(|e| TxError::Pager(e.to_string()))?;
        }

        self.state = TxState::Committed;

        // Unregister from manager
        self.manager.unregister_transaction(self.id);

        Ok(())
    }

    /// Rollback the transaction
    ///
    /// Discards all buffered writes and writes a Rollback record to the WAL.
    pub fn rollback(mut self) -> Result<(), TxError> {
        if self.state != TxState::Active {
            return match self.state {
                TxState::Committed => Err(TxError::AlreadyCommitted),
                TxState::Aborted => Err(TxError::AlreadyAborted),
                _ => Err(TxError::NotActive),
            };
        }

        // Write rollback record to WAL
        let mut wal = self.manager.wal_writer()?;
        let rollback_record = WalRecord::Rollback { tx_id: self.id };
        wal.append(&rollback_record)?;
        wal.sync()?;

        // Clear write set
        self.write_set.clear();
        self.state = TxState::Aborted;

        // Unregister from manager
        self.manager.unregister_transaction(self.id);

        Ok(())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // If transaction is still active when dropped, it means it was neither
        // committed nor rolled back. This is a bug, but we'll clean up anyway.
        if self.state == TxState::Active {
            // Try to write rollback record
            if let Ok(mut wal) = self.manager.wal.lock() {
                let _ = wal.append(&WalRecord::Rollback { tx_id: self.id });
                let _ = wal.sync();
            }
            self.manager.unregister_transaction(self.id);
        }
    }
}

/// Transaction Manager
///
/// Coordinates transactions and manages the WAL.
pub struct TransactionManager {
    /// Pager for reading/writing pages
    pager: Arc<Pager>,
    /// WAL writer
    wal: Mutex<WalWriter>,
    /// WAL file path
    wal_path: PathBuf,
    /// Active transaction IDs
    active_transactions: RwLock<Vec<u64>>,
}

impl TransactionManager {
    /// Create a new transaction manager
    ///
    /// # Arguments
    ///
    /// * `pager` - The pager to use for page I/O
    /// * `wal_path` - Path to the WAL file
    pub fn new(pager: Arc<Pager>, wal_path: impl AsRef<Path>) -> io::Result<Self> {
        let wal_path = wal_path.as_ref().to_path_buf();
        let wal = WalWriter::open(&wal_path)?;

        Ok(Self {
            pager,
            wal: Mutex::new(wal),
            wal_path,
            active_transactions: RwLock::new(Vec::new()),
        })
    }

    fn wal_writer(&self) -> Result<MutexGuard<'_, WalWriter>, TxError> {
        self.wal
            .lock()
            .map_err(|_| TxError::LockPoisoned("wal writer"))
    }

    fn active_transactions_write(&self) -> RwLockWriteGuard<'_, Vec<u64>> {
        self.active_transactions
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn active_transactions_read(&self) -> RwLockReadGuard<'_, Vec<u64>> {
        self.active_transactions
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Begin a new transaction
    pub fn begin(self: &Arc<Self>) -> Result<Transaction, TxError> {
        let tx_id = next_transaction_id();

        // Write Begin record to WAL
        {
            let mut wal = self.wal_writer()?;
            let begin_record = WalRecord::Begin { tx_id };
            wal.append(&begin_record)?;
        }

        // Register transaction
        {
            let mut active = self.active_transactions_write();
            active.push(tx_id);
        }

        Ok(Transaction {
            id: tx_id,
            state: TxState::Active,
            write_set: HashMap::new(),
            read_set: Vec::new(),
            manager: Arc::clone(self),
        })
    }

    /// Unregister a transaction (called on commit/rollback)
    fn unregister_transaction(&self, tx_id: u64) {
        let mut active = self.active_transactions_write();
        active.retain(|&id| id != tx_id);
    }

    /// Get list of active transaction IDs
    pub fn active_transactions(&self) -> Vec<u64> {
        self.active_transactions_read().clone()
    }

    /// Get WAL file path
    pub fn wal_path(&self) -> &Path {
        &self.wal_path
    }

    /// Get reference to pager
    pub fn pager(&self) -> &Arc<Pager> {
        &self.pager
    }

    /// Sync WAL to disk
    pub fn sync_wal(&self) -> io::Result<()> {
        let mut wal = self
            .wal
            .lock()
            .map_err(|_| io::Error::other("transaction WAL lock poisoned"))?;
        wal.sync()
    }

    /// Check if there are active transactions
    pub fn has_active_transactions(&self) -> bool {
        !self.active_transactions_read().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::PageType;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("reddb_tx_test_{}", timestamp))
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_transaction_commit() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Arc::new(Pager::open_default(&db_path).unwrap());

        // Allocate a page
        let page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page_id = page.page_id();

        // Create transaction manager
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        // Begin transaction
        let mut tx = tm.begin().unwrap();
        assert!(tx.is_active());

        // Write through transaction
        let mut page = Page::new(PageType::BTreeLeaf, page_id);
        page.as_bytes_mut()[100] = 0xAB;
        tx.write_page(page_id, page).unwrap();

        // Read through transaction (should see buffered write)
        let read_page = tx.read_page(page_id).unwrap();
        assert_eq!(read_page.as_bytes()[100], 0xAB);

        // Commit
        tx.commit().unwrap();

        // Verify write is visible through pager
        let final_page = pager.read_page(page_id).unwrap();
        assert_eq!(final_page.as_bytes()[100], 0xAB);

        cleanup(&dir);
    }

    #[test]
    fn test_transaction_rollback() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Arc::new(Pager::open_default(&db_path).unwrap());

        // Allocate a page and write initial value
        let mut page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page_id = page.page_id();
        page.as_bytes_mut()[100] = 0x11;
        pager.write_page(page_id, page).unwrap();

        // Create transaction manager
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        // Begin transaction
        let mut tx = tm.begin().unwrap();

        // Write through transaction
        let mut page = Page::new(PageType::BTreeLeaf, page_id);
        page.as_bytes_mut()[100] = 0xAB;
        tx.write_page(page_id, page).unwrap();

        // Read through transaction (should see buffered write)
        let read_page = tx.read_page(page_id).unwrap();
        assert_eq!(read_page.as_bytes()[100], 0xAB);

        // Rollback
        tx.rollback().unwrap();

        // Original value should be preserved
        let final_page = pager.read_page(page_id).unwrap();
        assert_eq!(final_page.as_bytes()[100], 0x11);

        cleanup(&dir);
    }

    #[test]
    fn test_multiple_transactions() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Arc::new(Pager::open_default(&db_path).unwrap());

        // Allocate two pages
        let page1 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page2 = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page1_id = page1.page_id();
        let page2_id = page2.page_id();

        // Create transaction manager
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        // Transaction 1: Write to page 1
        let mut tx1 = tm.begin().unwrap();
        let mut page1 = Page::new(PageType::BTreeLeaf, page1_id);
        page1.as_bytes_mut()[100] = 0x11;
        tx1.write_page(page1_id, page1).unwrap();
        tx1.commit().unwrap();

        // Transaction 2: Write to page 2
        let mut tx2 = tm.begin().unwrap();
        let mut page2 = Page::new(PageType::BTreeLeaf, page2_id);
        page2.as_bytes_mut()[100] = 0x22;
        tx2.write_page(page2_id, page2).unwrap();
        tx2.commit().unwrap();

        // Verify both writes
        let final_page1 = pager.read_page(page1_id).unwrap();
        let final_page2 = pager.read_page(page2_id).unwrap();
        assert_eq!(final_page1.as_bytes()[100], 0x11);
        assert_eq!(final_page2.as_bytes()[100], 0x22);

        cleanup(&dir);
    }

    #[test]
    fn test_transaction_isolation() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        // Create pager
        let pager = Arc::new(Pager::open_default(&db_path).unwrap());

        // Allocate a page with initial value
        let mut page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page_id = page.page_id();
        page.as_bytes_mut()[100] = 0x00;
        pager.write_page(page_id, page).unwrap();

        // Create transaction manager
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        // Transaction 1: Begin and write (but don't commit yet)
        let mut tx1 = tm.begin().unwrap();
        let mut page1 = Page::new(PageType::BTreeLeaf, page_id);
        page1.as_bytes_mut()[100] = 0x11;
        tx1.write_page(page_id, page1).unwrap();

        // Transaction 1 should see its own write
        let tx1_read = tx1.read_page(page_id).unwrap();
        assert_eq!(tx1_read.as_bytes()[100], 0x11);

        // Another read from pager should not see uncommitted write
        let pager_read = pager.read_page(page_id).unwrap();
        assert_eq!(pager_read.as_bytes()[100], 0x00);

        // Commit tx1
        tx1.commit().unwrap();

        // Now pager should see the write
        let final_read = pager.read_page(page_id).unwrap();
        assert_eq!(final_read.as_bytes()[100], 0x11);

        cleanup(&dir);
    }

    #[test]
    fn test_active_transaction_tracking() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        let pager = Arc::new(Pager::open_default(&db_path).unwrap());
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        assert!(!tm.has_active_transactions());

        let tx1 = tm.begin().unwrap();
        let tx1_id = tx1.id();
        assert!(tm.has_active_transactions());
        assert!(tm.active_transactions().contains(&tx1_id));

        let tx2 = tm.begin().unwrap();
        let tx2_id = tx2.id();
        assert_eq!(tm.active_transactions().len(), 2);

        tx1.commit().unwrap();
        assert!(!tm.active_transactions().contains(&tx1_id));
        assert!(tm.active_transactions().contains(&tx2_id));

        tx2.rollback().unwrap();
        assert!(!tm.has_active_transactions());

        cleanup(&dir);
    }

    #[test]
    fn test_transaction_double_commit() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        let pager = Arc::new(Pager::open_default(&db_path).unwrap());
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        // The transaction is consumed on commit, so double commit is impossible at compile time
        // This test just verifies commit works
        let tx = tm.begin().unwrap();
        tx.commit().unwrap();

        cleanup(&dir);
    }

    #[test]
    fn test_begin_returns_structured_error_when_wal_lock_is_poisoned() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("test.db");
        let wal_path = dir.join("test.wal");

        let pager = Arc::new(Pager::open_default(&db_path).unwrap());
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        let poison_target = Arc::clone(&tm);
        let _ = std::thread::spawn(move || {
            let _guard = poison_target
                .wal
                .lock()
                .expect("wal lock should be acquired");
            panic!("poison wal mutex");
        })
        .join();

        match tm.begin() {
            Ok(_) => panic!("begin should fail after WAL lock poisoning"),
            Err(err) => assert!(matches!(err, TxError::LockPoisoned("wal writer"))),
        }

        cleanup(&dir);
    }

    // ---------------------------------------------------------------
    // Perf 1.2: read-only commit fast path
    // ---------------------------------------------------------------

    #[test]
    fn read_only_commit_does_not_advance_durable_lsn() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("ro_durable.db");
        let wal_path = dir.join("ro_durable.wal");

        let pager = Arc::new(Pager::open_default(&db_path).unwrap());
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        // Snapshot the WAL durable_lsn BEFORE the txn.
        let before = {
            let wal = tm.wal_writer().unwrap();
            wal.durable_lsn()
        };

        let tx = tm.begin().unwrap();
        // Empty write_set on purpose — read-only.
        tx.commit().unwrap();

        // After RO commit, the WAL durable_lsn must NOT have advanced.
        // No Begin record, no Commit record, no fsync.
        let after = {
            let wal = tm.wal_writer().unwrap();
            wal.durable_lsn()
        };
        assert_eq!(
            before, after,
            "read-only commit must not advance durable_lsn (was {} → {})",
            before, after
        );

        cleanup(&dir);
    }

    #[test]
    fn read_only_commit_does_not_grow_wal_file() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("ro_size.db");
        let wal_path = dir.join("ro_size.wal");

        let pager = Arc::new(Pager::open_default(&db_path).unwrap());
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        // Snapshot file size after WAL header.
        let size_before = std::fs::metadata(&wal_path).unwrap().len();
        assert_eq!(
            size_before, 8,
            "fresh WAL must be exactly the 8-byte header"
        );

        // 100 read-only commits in a loop.
        for _ in 0..100 {
            let tx = tm.begin().unwrap();
            tx.commit().unwrap();
        }

        let size_after = std::fs::metadata(&wal_path).unwrap().len();
        assert_eq!(
            size_after, size_before,
            "100 read-only commits should not have written any WAL bytes"
        );
        cleanup(&dir);
    }

    #[test]
    fn read_only_commit_marks_transaction_committed() {
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("ro_state.db");
        let wal_path = dir.join("ro_state.wal");

        let pager = Arc::new(Pager::open_default(&db_path).unwrap());
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        let tx = tm.begin().unwrap();
        let id = tx.id();
        tx.commit().unwrap();

        // Manager must have unregistered this txn — the active list
        // no longer contains its id.
        assert!(
            !tm.active_transactions().contains(&id),
            "RO-committed txn {id} must no longer be active in the manager"
        );

        cleanup(&dir);
    }

    #[test]
    fn writing_commit_still_syncs_after_ro_fast_path() {
        // Sanity: the fast path must NOT short-circuit a transaction
        // that did write something. Verify the writing commit path
        // still flushes WAL and the value lands in the pager.
        let dir = temp_dir();
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("rw_after_ro.db");
        let wal_path = dir.join("rw_after_ro.wal");

        let pager = Arc::new(Pager::open_default(&db_path).unwrap());
        let allocated = pager.allocate_page(PageType::BTreeLeaf).unwrap();
        let page_id = allocated.page_id();
        let tm = Arc::new(TransactionManager::new(Arc::clone(&pager), &wal_path).unwrap());

        // First a RO commit (must take the fast path).
        let ro = tm.begin().unwrap();
        ro.commit().unwrap();

        // Then a real writing commit.
        let mut rw = tm.begin().unwrap();
        let mut page = Page::new(PageType::BTreeLeaf, page_id);
        page.as_bytes_mut()[42] = 0x77;
        rw.write_page(page_id, page).unwrap();
        rw.commit().unwrap();

        // The WAL file must now contain bytes (PageWrite + Commit
        // records, and the BufWriter has been flushed by sync()).
        let size = std::fs::metadata(&wal_path).unwrap().len();
        assert!(size > 8, "writing commit should grow the WAL");

        // The pager cache must reflect the write.
        let read_back = pager.read_page(page_id).unwrap();
        assert_eq!(read_back.as_bytes()[42], 0x77);

        cleanup(&dir);
    }
}
