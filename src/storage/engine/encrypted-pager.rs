//! Encrypted Pager
//!
//! A wrapper around the Pager that transparently encrypts/decrypts pages
//! using AES-256-GCM. Each page has a unique nonce, and the page ID is
//! used as additional authenticated data (AAD) to prevent page swapping attacks.
//!
//! # File Layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ Page 0: Database Header (unencrypted header, encrypted body)│
//! │   - Encryption salt (32 bytes)                              │
//! │   - Key verification blob (60 bytes)                        │
//! │   - Encrypted database metadata                             │
//! ├─────────────────────────────────────────────────────────────┤
//! │ Page 1..N: Encrypted data pages                             │
//! │   - [Nonce (12)] [Ciphertext] [Tag (16)]                    │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # References
//!
//! - SQLCipher: Encrypted SQLite database
//! - Turso encryption model

use std::path::{Path, PathBuf};

use super::pager::{Pager, PagerConfig, PagerError};
use super::{Page, PageType, HEADER_SIZE, PAGE_SIZE};
use crate::storage::encryption::{
    page_encryptor::OVERHEAD, EncryptionHeader, PageEncryptor, SecureKey,
};

/// Usable page content size after encryption overhead
pub const ENCRYPTED_CONTENT_SIZE: usize = PAGE_SIZE - OVERHEAD;

/// Encrypted Pager error types
#[derive(Debug)]
pub enum EncryptedPagerError {
    /// Pager error
    Pager(PagerError),
    /// Encryption error
    Encryption(String),
    /// Key validation failed
    InvalidKey,
    /// Database is not encrypted
    NotEncrypted,
    /// Database is already encrypted
    AlreadyEncrypted,
}

impl std::fmt::Display for EncryptedPagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pager(e) => write!(f, "Pager error: {}", e),
            Self::Encryption(msg) => write!(f, "Encryption error: {}", msg),
            Self::InvalidKey => write!(f, "Invalid encryption key"),
            Self::NotEncrypted => write!(f, "Database is not encrypted"),
            Self::AlreadyEncrypted => write!(f, "Database is already encrypted"),
        }
    }
}

impl std::error::Error for EncryptedPagerError {}

impl From<PagerError> for EncryptedPagerError {
    fn from(e: PagerError) -> Self {
        Self::Pager(e)
    }
}

/// Configuration for encrypted pager
#[derive(Debug, Clone, Default)]
pub struct EncryptedPagerConfig {
    /// Base pager configuration
    pub pager_config: PagerConfig,
    /// Encryption key (must be 32 bytes for AES-256)
    /// If None, the database is opened unencrypted
    pub key: Option<SecureKey>,
}

/// Encrypted Pager
///
/// Transparently encrypts/decrypts pages on read/write.
pub struct EncryptedPager {
    /// Inner pager (handles I/O)
    inner: Pager,
    /// Page encryptor (if encryption is enabled)
    encryptor: Option<PageEncryptor>,
    /// Encryption header (if encrypted)
    encryption_header: Option<EncryptionHeader>,
    /// Whether the database is encrypted
    is_encrypted: bool,
    /// Database path
    path: PathBuf,
}

impl EncryptedPager {
    /// Open or create an encrypted database
    pub fn open<P: AsRef<Path>>(
        path: P,
        config: EncryptedPagerConfig,
    ) -> Result<Self, EncryptedPagerError> {
        let path = path.as_ref().to_path_buf();
        let exists = path.exists();

        // Open inner pager
        let inner = Pager::open(&path, config.pager_config.clone())?;

        if exists && inner.page_count() > 0 {
            // Existing database - check if encrypted and validate key
            Self::open_existing(inner, path, config.key)
        } else {
            // New database - initialize with or without encryption
            Self::create_new(inner, path, config.key)
        }
    }

    /// Open an existing database
    fn open_existing(
        inner: Pager,
        path: PathBuf,
        key: Option<SecureKey>,
    ) -> Result<Self, EncryptedPagerError> {
        // Read header page to check for encryption marker
        let header_page = inner.read_page(0)?;
        let data = header_page.as_bytes();

        // Check for encryption marker at offset HEADER_SIZE + 32 (after DB header)
        // Encryption marker: "RDBE" (RedDB Encrypted)
        const ENCRYPTION_MARKER_OFFSET: usize = HEADER_SIZE + 32;
        const ENCRYPTION_MARKER: &[u8; 4] = b"RDBE";

        let has_marker = data.len() > ENCRYPTION_MARKER_OFFSET + 4
            && &data[ENCRYPTION_MARKER_OFFSET..ENCRYPTION_MARKER_OFFSET + 4] == ENCRYPTION_MARKER;

        if has_marker {
            // Database is encrypted
            let key = key.ok_or(EncryptedPagerError::InvalidKey)?;

            // Load encryption header (after marker)
            let header_start = ENCRYPTION_MARKER_OFFSET + 4;
            let header = EncryptionHeader::from_bytes(&data[header_start..])
                .map_err(EncryptedPagerError::Encryption)?;

            // Validate key
            if !header.validate(&key) {
                return Err(EncryptedPagerError::InvalidKey);
            }

            let encryptor = PageEncryptor::new(key);

            Ok(Self {
                inner,
                encryptor: Some(encryptor),
                encryption_header: Some(header),
                is_encrypted: true,
                path,
            })
        } else {
            // Database is not encrypted
            if key.is_some() {
                // User provided key but database is not encrypted
                return Err(EncryptedPagerError::NotEncrypted);
            }

            Ok(Self {
                inner,
                encryptor: None,
                encryption_header: None,
                is_encrypted: false,
                path,
            })
        }
    }

    /// Create a new database
    fn create_new(
        inner: Pager,
        path: PathBuf,
        key: Option<SecureKey>,
    ) -> Result<Self, EncryptedPagerError> {
        if let Some(key) = key {
            // Create encrypted database
            let header = EncryptionHeader::new(&key);
            let encryptor = PageEncryptor::new(key);

            let mut pager = Self {
                inner,
                encryptor: Some(encryptor),
                encryption_header: Some(header),
                is_encrypted: true,
                path,
            };

            // Write encryption header to page 0
            pager.write_encryption_header()?;

            Ok(pager)
        } else {
            // Create unencrypted database
            Ok(Self {
                inner,
                encryptor: None,
                encryption_header: None,
                is_encrypted: false,
                path,
            })
        }
    }

    /// Write encryption header to page 0
    fn write_encryption_header(&mut self) -> Result<(), EncryptedPagerError> {
        let header = self
            .encryption_header
            .as_ref()
            .ok_or(EncryptedPagerError::NotEncrypted)?;

        // Read current header page
        let mut page = self.inner.read_page(0)?;
        let data = page.as_bytes_mut();

        // Write marker and header after database header
        const ENCRYPTION_MARKER_OFFSET: usize = HEADER_SIZE + 32;
        const ENCRYPTION_MARKER: &[u8; 4] = b"RDBE";

        data[ENCRYPTION_MARKER_OFFSET..ENCRYPTION_MARKER_OFFSET + 4]
            .copy_from_slice(ENCRYPTION_MARKER);

        let header_bytes = header.to_bytes();
        let header_start = ENCRYPTION_MARKER_OFFSET + 4;
        data[header_start..header_start + header_bytes.len()].copy_from_slice(&header_bytes);

        // Write back (header page is not encrypted itself, but contains encryption params)
        self.inner.write_page(0, page)?;

        Ok(())
    }

    /// Check if database is encrypted
    pub fn is_encrypted(&self) -> bool {
        self.is_encrypted
    }

    /// Read a page (decrypts if encrypted)
    pub fn read_page(&self, page_id: u32) -> Result<Page, EncryptedPagerError> {
        if page_id == 0 || !self.is_encrypted {
            // Header page is never encrypted (contains encryption params)
            // Or database is not encrypted - use normal read with checksum
            let raw_page = self.inner.read_page(page_id)?;
            return Ok(raw_page);
        }

        // For encrypted pages, skip checksum verification (GCM provides integrity)
        let raw_page = self.inner.read_page_no_checksum(page_id)?;

        // Decrypt the page content
        let encryptor = self
            .encryptor
            .as_ref()
            .ok_or(EncryptedPagerError::NotEncrypted)?;

        // Page layout: [Nonce (12)] [Ciphertext (PAGE_SIZE - OVERHEAD)] [Tag (16)]
        // Total: PAGE_SIZE bytes
        let raw_data = raw_page.as_bytes();
        let plaintext = encryptor
            .decrypt(page_id, raw_data)
            .map_err(EncryptedPagerError::Encryption)?;

        // Reconstruct page from plaintext (plaintext is PAGE_SIZE - OVERHEAD bytes)
        // We need to pad it back to PAGE_SIZE
        let mut page_data = [0u8; PAGE_SIZE];
        let copy_len = plaintext.len().min(PAGE_SIZE);
        page_data[..copy_len].copy_from_slice(&plaintext[..copy_len]);

        Ok(Page::from_bytes(page_data))
    }

    /// Write a page (encrypts if encrypted)
    pub fn write_page(&self, page_id: u32, page: Page) -> Result<(), EncryptedPagerError> {
        if page_id == 0 || !self.is_encrypted {
            // Header page is never encrypted
            // Or database is not encrypted
            return Ok(self.inner.write_page(page_id, page)?);
        }

        // Encrypt the page content
        let encryptor = self
            .encryptor
            .as_ref()
            .ok_or(EncryptedPagerError::NotEncrypted)?;

        // Only encrypt PAGE_SIZE - OVERHEAD bytes to leave room for nonce and tag
        // Layout after encryption: [Nonce (12)] [Ciphertext (PAGE_SIZE - OVERHEAD)] [Tag (16)]
        // Total: PAGE_SIZE bytes
        let plaintext = &page.as_bytes()[..ENCRYPTED_CONTENT_SIZE];
        let encrypted = encryptor.encrypt(page_id, plaintext);

        // Create a new page with encrypted content
        let mut encrypted_page = Page::new(PageType::BTreeLeaf, page_id);
        let encrypted_data = encrypted_page.as_bytes_mut();

        // Copy encrypted data (should be exactly PAGE_SIZE bytes)
        assert_eq!(encrypted.len(), PAGE_SIZE, "Encrypted size mismatch");
        encrypted_data.copy_from_slice(&encrypted);

        // Use write_page_no_checksum to avoid corrupting the ciphertext
        // (encrypted pages use GCM authentication tag for integrity instead)
        self.inner.write_page_no_checksum(page_id, encrypted_page)?;

        Ok(())
    }

    /// Allocate a new page
    pub fn allocate_page(&self, page_type: PageType) -> Result<Page, EncryptedPagerError> {
        Ok(self.inner.allocate_page(page_type)?)
    }

    /// Free a page
    pub fn free_page(&self, page_id: u32) -> Result<(), EncryptedPagerError> {
        Ok(self.inner.free_page(page_id)?)
    }

    /// Sync to disk
    pub fn sync(&self) -> Result<(), EncryptedPagerError> {
        Ok(self.inner.sync()?)
    }

    /// Get page count
    pub fn page_count(&self) -> u32 {
        self.inner.page_count()
    }

    /// Get database path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get inner pager (for advanced operations)
    pub fn inner(&self) -> &Pager {
        &self.inner
    }

    /// Change the encryption key (re-encrypts all pages)
    ///
    /// This is an expensive operation that reads and re-writes all pages.
    pub fn rekey(&mut self, new_key: SecureKey) -> Result<(), EncryptedPagerError> {
        if !self.is_encrypted {
            return Err(EncryptedPagerError::NotEncrypted);
        }

        // Read all pages
        let page_count = self.inner.page_count();
        let mut pages = Vec::with_capacity(page_count as usize);

        for i in 0..page_count {
            let page = self.read_page(i)?;
            pages.push(page);
        }

        // Create new encryption header and encryptor
        let new_header = EncryptionHeader::new(&new_key);
        let new_encryptor = PageEncryptor::new(new_key);

        self.encryption_header = Some(new_header);
        self.encryptor = Some(new_encryptor);

        // Write encryption header
        self.write_encryption_header()?;

        // Re-write all pages (except header, which we just wrote)
        for i in 1..page_count {
            self.write_page(i, pages[i as usize].clone())?;
        }

        self.sync()?;

        Ok(())
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
        std::env::temp_dir().join(format!("reddb_encrypted_test_{}.rdb", timestamp))
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_encrypted_pager_create() {
        let path = temp_db_path();
        cleanup(&path);

        let key = SecureKey::new(&[0x42u8; 32]);
        let config = EncryptedPagerConfig {
            key: Some(key),
            ..Default::default()
        };

        {
            let pager = EncryptedPager::open(&path, config.clone()).unwrap();
            assert!(pager.is_encrypted());
        }

        cleanup(&path);
    }

    #[test]
    fn test_encrypted_pager_write_read() {
        let path = temp_db_path();
        cleanup(&path);

        let key = SecureKey::new(&[0x42u8; 32]);
        let config = EncryptedPagerConfig {
            key: Some(key.clone()),
            ..Default::default()
        };

        {
            let pager = EncryptedPager::open(&path, config.clone()).unwrap();
            assert!(pager.is_encrypted(), "Should be encrypted after create");

            // Allocate and write a page
            let mut page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            let page_id = page.page_id();
            page.as_bytes_mut()[100] = 0xAB;
            pager.write_page(page_id, page).unwrap();

            // Read back
            let read_page = pager.read_page(page_id).unwrap();
            assert_eq!(read_page.as_bytes()[100], 0xAB);

            pager.sync().unwrap();
        }

        // Reopen and verify
        {
            let pager = EncryptedPager::open(&path, config).unwrap();
            assert!(pager.is_encrypted(), "Should be encrypted after reopen");

            let page = pager.read_page(1).unwrap();
            assert_eq!(page.as_bytes()[100], 0xAB);
        }

        cleanup(&path);
    }

    #[test]
    fn test_encrypted_pager_wrong_key() {
        let path = temp_db_path();
        cleanup(&path);

        let key = SecureKey::new(&[0x42u8; 32]);
        let config = EncryptedPagerConfig {
            key: Some(key),
            ..Default::default()
        };

        // Create encrypted database
        {
            let pager = EncryptedPager::open(&path, config).unwrap();
            pager.sync().unwrap();
        }

        // Try to open with wrong key
        let wrong_key = SecureKey::new(&[0x11u8; 32]);
        let wrong_config = EncryptedPagerConfig {
            key: Some(wrong_key),
            ..Default::default()
        };

        let result = EncryptedPager::open(&path, wrong_config);
        assert!(matches!(result, Err(EncryptedPagerError::InvalidKey)));

        cleanup(&path);
    }

    #[test]
    fn test_unencrypted_pager() {
        let path = temp_db_path();
        cleanup(&path);

        let config = EncryptedPagerConfig::default(); // No key = unencrypted

        {
            let pager = EncryptedPager::open(&path, config.clone()).unwrap();
            assert!(!pager.is_encrypted());

            let mut page = pager.allocate_page(PageType::BTreeLeaf).unwrap();
            let page_id = page.page_id();
            page.as_bytes_mut()[100] = 0xCD;
            pager.write_page(page_id, page).unwrap();
            pager.sync().unwrap();
        }

        // Reopen
        {
            let pager = EncryptedPager::open(&path, config).unwrap();
            let page = pager.read_page(1).unwrap();
            assert_eq!(page.as_bytes()[100], 0xCD);
        }

        cleanup(&path);
    }
}
