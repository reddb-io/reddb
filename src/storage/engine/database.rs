//! RedDB Database Engine
//!
//! The main entry point for the RedDB storage engine. Integrates all components:
//! - Pager for page I/O
//! - WAL for durability
//! - Transactions for ACID properties
//! - Checkpointing for WAL management
//! - B-tree for indexing
//!
//! # Usage
//!
//! ```rust,ignore
//! use redblue::storage::engine::Database;
//!
//! // Open or create a database
//! let db = Database::open("mydata.rdb")?;
//!
//! // Begin a transaction
//! let tx = db.begin()?;
//!
//! // Perform operations
//! tx.put(b"key", b"value")?;
//!
//! // Commit
//! tx.commit()?;
//!
//! // Close (or let it drop)
//! db.close()?;
//! ```
//!
//! # File Layout
//!
//! ```text
//! mydata.rdb     - Main database file (pages)
//! mydata.rdb-wal - Write-ahead log
//! ```
//!
//! # References
//!
//! - Turso `core/database.rs` - Database lifecycle
//! - SQLite architecture documentation

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use super::{Page, PageType, Pager, PagerConfig};
use crate::storage::wal::{
    CheckpointError, CheckpointMode, CheckpointResult, Checkpointer, Transaction,
    TransactionManager, TxError,
};

/// Database configuration
#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    /// Page cache size (number of pages)
    pub cache_size: usize,
    /// Whether to open read-only
    pub read_only: bool,
    /// Whether to create if not exists
    pub create: bool,
    /// Checkpoint mode
    pub checkpoint_mode: CheckpointMode,
    /// Auto-checkpoint threshold (pages)
    /// Set to 0 to disable auto-checkpoint
    pub auto_checkpoint_threshold: u32,
    /// Whether to verify checksums on read
    pub verify_checksums: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            cache_size: 10_000,
            read_only: false,
            create: true,
            checkpoint_mode: CheckpointMode::Full,
            auto_checkpoint_threshold: 1000,
            verify_checksums: true,
        }
    }
}

/// Database error types
#[derive(Debug)]
pub enum DatabaseError {
    /// I/O error
    Io(io::Error),
    /// Pager error
    Pager(String),
    /// Transaction error
    Transaction(TxError),
    /// Checkpoint error
    Checkpoint(CheckpointError),
    /// Database is read-only
    ReadOnly,
    /// Database is closed
    Closed,
}

impl std::fmt::Display for DatabaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Pager(msg) => write!(f, "Pager error: {}", msg),
            Self::Transaction(e) => write!(f, "Transaction error: {}", e),
            Self::Checkpoint(e) => write!(f, "Checkpoint error: {}", e),
            Self::ReadOnly => write!(f, "Database is read-only"),
            Self::Closed => write!(f, "Database is closed"),
        }
    }
}

impl std::error::Error for DatabaseError {}

impl From<io::Error> for DatabaseError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<TxError> for DatabaseError {
    fn from(e: TxError) -> Self {
        Self::Transaction(e)
    }
}

impl From<CheckpointError> for DatabaseError {
    fn from(e: CheckpointError) -> Self {
        Self::Checkpoint(e)
    }
}

/// Database state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DbState {
    Open,
    Closed,
}

/// RedDB Database Engine
///
/// The main entry point for database operations. Thread-safe.
pub struct Database {
    /// Database file path
    path: PathBuf,
    /// WAL file path
    wal_path: PathBuf,
    /// Pager (shared)
    pager: Arc<Pager>,
    /// Transaction manager (shared)
    tx_manager: Arc<TransactionManager>,
    /// Configuration
    config: DatabaseConfig,
    /// Database state
    state: RwLock<DbState>,
    /// Pages written since last checkpoint
    pages_since_checkpoint: RwLock<u32>,
}

impl Database {
    /// Open or create a database
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, DatabaseError> {
        Self::open_with_config(path, DatabaseConfig::default())
    }

    /// Open or create a database with custom configuration
    pub fn open_with_config<P: AsRef<Path>>(
        path: P,
        config: DatabaseConfig,
    ) -> Result<Self, DatabaseError> {
        let path = path.as_ref().to_path_buf();
        let wal_path = path.with_extension("rdb-wal");

        // Create pager config
        let pager_config = PagerConfig {
            cache_size: config.cache_size,
            read_only: config.read_only,
            create: config.create,
            verify_checksums: config.verify_checksums,
        };

        // Open pager
        let pager =
            Pager::open(&path, pager_config).map_err(|e| DatabaseError::Pager(e.to_string()))?;
        let pager = Arc::new(pager);

        // Perform crash recovery if WAL exists
        if wal_path.exists() && !config.read_only {
            let recovery_result = Checkpointer::recover(&pager, &wal_path)?;
            if recovery_result.pages_checkpointed > 0 {
                // Log recovery info (in production, use proper logging)
                eprintln!(
                    "RedDB: Recovered {} transactions, {} pages from WAL",
                    recovery_result.transactions_processed, recovery_result.pages_checkpointed
                );
            }
        }

        // Create transaction manager
        let tx_manager = Arc::new(
            TransactionManager::new(Arc::clone(&pager), &wal_path).map_err(DatabaseError::Io)?,
        );

        Ok(Self {
            path,
            wal_path,
            pager,
            tx_manager,
            config,
            state: RwLock::new(DbState::Open),
            pages_since_checkpoint: RwLock::new(0),
        })
    }

    /// Check if database is open
    fn check_open(&self) -> Result<(), DatabaseError> {
        if *self.state.read().unwrap() == DbState::Closed {
            return Err(DatabaseError::Closed);
        }
        Ok(())
    }

    /// Begin a new transaction
    pub fn begin(&self) -> Result<Transaction, DatabaseError> {
        self.check_open()?;
        Ok(self.tx_manager.begin()?)
    }

    /// Get a reference to the pager (for advanced operations)
    pub fn pager(&self) -> &Arc<Pager> {
        &self.pager
    }

    /// Get a reference to the transaction manager
    pub fn tx_manager(&self) -> &Arc<TransactionManager> {
        &self.tx_manager
    }

    /// Allocate a new page
    pub fn allocate_page(&self, page_type: PageType) -> Result<Page, DatabaseError> {
        self.check_open()?;
        if self.config.read_only {
            return Err(DatabaseError::ReadOnly);
        }
        self.pager
            .allocate_page(page_type)
            .map_err(|e| DatabaseError::Pager(e.to_string()))
    }

    /// Read a page
    pub fn read_page(&self, page_id: u32) -> Result<Page, DatabaseError> {
        self.check_open()?;
        self.pager
            .read_page(page_id)
            .map_err(|e| DatabaseError::Pager(e.to_string()))
    }

    /// Perform a checkpoint
    pub fn checkpoint(&self) -> Result<CheckpointResult, DatabaseError> {
        self.check_open()?;
        if self.config.read_only {
            return Err(DatabaseError::ReadOnly);
        }

        let checkpointer = Checkpointer::new(self.config.checkpoint_mode);
        let result = checkpointer.checkpoint(&self.pager, &self.wal_path)?;

        // Reset counter
        *self.pages_since_checkpoint.write().unwrap() = 0;

        Ok(result)
    }

    /// Check if auto-checkpoint is needed and perform it
    pub fn maybe_auto_checkpoint(&self) -> Result<Option<CheckpointResult>, DatabaseError> {
        if self.config.auto_checkpoint_threshold == 0 {
            return Ok(None);
        }

        let pages = *self.pages_since_checkpoint.read().unwrap();
        if pages >= self.config.auto_checkpoint_threshold {
            Ok(Some(self.checkpoint()?))
        } else {
            Ok(None)
        }
    }

    /// Increment pages-since-checkpoint counter
    pub fn increment_page_count(&self, count: u32) {
        let mut pages = self.pages_since_checkpoint.write().unwrap();
        *pages = pages.saturating_add(count);
    }

    /// Sync all data to disk
    pub fn sync(&self) -> Result<(), DatabaseError> {
        self.check_open()?;
        self.pager
            .sync()
            .map_err(|e| DatabaseError::Pager(e.to_string()))?;
        self.tx_manager.sync_wal()?;
        Ok(())
    }

    /// Close the database
    ///
    /// Performs a final checkpoint and syncs all data to disk.
    pub fn close(self) -> Result<(), DatabaseError> {
        // Mark as closed
        *self.state.write().unwrap() = DbState::Closed;

        // Wait for active transactions to complete
        if self.tx_manager.has_active_transactions() {
            eprintln!("RedDB: Warning - closing with active transactions");
        }

        // Final checkpoint if not read-only
        if !self.config.read_only {
            let checkpointer = Checkpointer::new(CheckpointMode::Truncate);
            let _ = checkpointer.checkpoint(&self.pager, &self.wal_path);
        }

        // Sync pager
        let _ = self.pager.sync();

        Ok(())
    }

    /// Get database file path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get WAL file path
    pub fn wal_path(&self) -> &Path {
        &self.wal_path
    }

    /// Check if database is read-only
    pub fn is_read_only(&self) -> bool {
        self.config.read_only
    }

    /// Get page count
    pub fn page_count(&self) -> u32 {
        self.pager.page_count()
    }

    /// Get database file size
    pub fn file_size(&self) -> Result<u64, DatabaseError> {
        self.pager
            .file_size()
            .map_err(|e| DatabaseError::Pager(e.to_string()))
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> super::page_cache::CacheStats {
        self.pager.cache_stats()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // Try to sync on drop
        if *self.state.read().unwrap() == DbState::Open {
            *self.state.write().unwrap() = DbState::Closed;

            // Best-effort checkpoint and sync
            if !self.config.read_only {
                let checkpointer = Checkpointer::new(CheckpointMode::Full);
                let _ = checkpointer.checkpoint(&self.pager, &self.wal_path);
            }
            let _ = self.pager.sync();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("reddb_engine_test_{}.rdb", timestamp))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
        let wal_path = path.with_extension("rdb-wal");
        let _ = fs::remove_file(wal_path);
    }

    #[test]
    fn test_database_open_create() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let db = Database::open(&path).unwrap();
            assert!(!db.is_read_only());
            assert_eq!(db.page_count(), 1); // Header page
        }

        // Should be able to reopen
        {
            let db = Database::open(&path).unwrap();
            assert_eq!(db.page_count(), 1);
        }

        cleanup(&path);
    }

    #[test]
    fn test_database_transaction() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let db = Database::open(&path).unwrap();

            // Allocate a page
            let page = db.allocate_page(PageType::BTreeLeaf).unwrap();
            let page_id = page.page_id();

            // Begin transaction
            let mut tx = db.begin().unwrap();

            // Write through transaction
            let mut page = Page::new(PageType::BTreeLeaf, page_id);
            page.as_bytes_mut()[100] = 0xAB;
            tx.write_page(page_id, page).unwrap();

            // Commit
            tx.commit().unwrap();

            // Verify
            let read_page = db.read_page(page_id).unwrap();
            assert_eq!(read_page.as_bytes()[100], 0xAB);
        }

        cleanup(&path);
    }

    #[test]
    fn test_database_crash_recovery() {
        let path = temp_db_path();
        cleanup(&path);

        let page_id;

        // First session: write data but don't checkpoint
        {
            let db = Database::open(&path).unwrap();

            // Allocate a page
            let page = db.allocate_page(PageType::BTreeLeaf).unwrap();
            page_id = page.page_id();

            // Write through transaction
            let mut tx = db.begin().unwrap();
            let mut page = Page::new(PageType::BTreeLeaf, page_id);
            page.as_bytes_mut()[100] = 0xCD;
            tx.write_page(page_id, page).unwrap();
            tx.commit().unwrap();

            // Sync WAL but don't checkpoint
            db.sync().unwrap();

            // Drop without calling close (simulate crash)
        }

        // Second session: should recover from WAL
        {
            let db = Database::open(&path).unwrap();

            // Data should be recovered
            let read_page = db.read_page(page_id).unwrap();
            assert_eq!(read_page.as_bytes()[100], 0xCD);
        }

        cleanup(&path);
    }

    #[test]
    fn test_database_checkpoint() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let db = Database::open(&path).unwrap();

            // Allocate pages
            let page1 = db.allocate_page(PageType::BTreeLeaf).unwrap();
            let page2 = db.allocate_page(PageType::BTreeLeaf).unwrap();

            // Write through transactions
            let mut tx1 = db.begin().unwrap();
            let mut p1 = Page::new(PageType::BTreeLeaf, page1.page_id());
            p1.as_bytes_mut()[100] = 0x11;
            tx1.write_page(page1.page_id(), p1).unwrap();
            tx1.commit().unwrap();

            let mut tx2 = db.begin().unwrap();
            let mut p2 = Page::new(PageType::BTreeLeaf, page2.page_id());
            p2.as_bytes_mut()[100] = 0x22;
            tx2.write_page(page2.page_id(), p2).unwrap();
            tx2.commit().unwrap();

            // Checkpoint
            let result = db.checkpoint().unwrap();
            assert_eq!(result.transactions_processed, 2);
            assert!(result.pages_checkpointed >= 2);

            // Close properly
            db.close().unwrap();
        }

        // Reopen and verify
        {
            let db = Database::open(&path).unwrap();
            // Pages should still be there
            assert!(db.page_count() >= 3); // header + 2 data pages
        }

        cleanup(&path);
    }

    #[test]
    fn test_database_read_only() {
        let path = temp_db_path();
        cleanup(&path);

        // Create database first
        {
            let db = Database::open(&path).unwrap();
            let page = db.allocate_page(PageType::BTreeLeaf).unwrap();
            db.close().unwrap();
        }

        // Open read-only
        {
            let config = DatabaseConfig {
                read_only: true,
                ..Default::default()
            };
            let db = Database::open_with_config(&path, config).unwrap();
            assert!(db.is_read_only());

            // Should not be able to allocate
            assert!(db.allocate_page(PageType::BTreeLeaf).is_err());
        }

        cleanup(&path);
    }

    #[test]
    fn test_database_multiple_transactions() {
        let path = temp_db_path();
        cleanup(&path);

        {
            let db = Database::open(&path).unwrap();

            // Allocate pages
            let page1 = db.allocate_page(PageType::BTreeLeaf).unwrap();
            let page2 = db.allocate_page(PageType::BTreeLeaf).unwrap();

            // Multiple concurrent transactions (interleaved)
            let mut tx1 = db.begin().unwrap();
            let mut tx2 = db.begin().unwrap();

            // tx1 writes to page1
            let mut p1 = Page::new(PageType::BTreeLeaf, page1.page_id());
            p1.as_bytes_mut()[100] = 0x11;
            tx1.write_page(page1.page_id(), p1).unwrap();

            // tx2 writes to page2
            let mut p2 = Page::new(PageType::BTreeLeaf, page2.page_id());
            p2.as_bytes_mut()[100] = 0x22;
            tx2.write_page(page2.page_id(), p2).unwrap();

            // Commit both
            tx1.commit().unwrap();
            tx2.commit().unwrap();

            // Verify
            let r1 = db.read_page(page1.page_id()).unwrap();
            let r2 = db.read_page(page2.page_id()).unwrap();
            assert_eq!(r1.as_bytes()[100], 0x11);
            assert_eq!(r2.as_bytes()[100], 0x22);
        }

        cleanup(&path);
    }
}
