//! Encrypted Pager (deprecated wrapper).
//!
//! This module used to host a separate `EncryptedPager` struct that
//! wrapped `Pager` and added transparent AES-GCM encryption. The
//! encryption ownership has moved into `Pager` itself
//! (`PagerConfig::encryption` + `Pager::{read_page_decrypted,
//! write_page_encrypted}`), so this file now exists only to keep the
//! historical surface compiling for callers outside the engine crate.
//!
//! Migration path:
//!   * Construct `Pager` with `PagerConfig { encryption: Some(key), .. }`.
//!   * Replace `EncryptedPager::read_page` with `Pager::read_page_decrypted`.
//!   * Replace `EncryptedPager::write_page` with `Pager::write_page_encrypted`.
//!
//! New code should not introduce additional `EncryptedPager` callers.

use std::path::{Path, PathBuf};

use super::pager::{Pager, PagerConfig, PagerError};
use super::{Page, PageType, PAGE_SIZE};
use crate::storage::encryption::{page_encryptor::OVERHEAD, SecureKey};

/// Usable page content size after encryption overhead
pub const ENCRYPTED_CONTENT_SIZE: usize = PAGE_SIZE - OVERHEAD;

/// Errors emitted by the deprecated wrapper. Surface kept stable for
/// existing callers; new code should use `PagerError` directly.
#[derive(Debug)]
pub enum EncryptedPagerError {
    Pager(PagerError),
    Encryption(String),
    InvalidKey,
    NotEncrypted,
    AlreadyEncrypted,
}

impl std::fmt::Display for EncryptedPagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pager(e) => write!(f, "Pager error: {e}"),
            Self::Encryption(msg) => write!(f, "Encryption error: {msg}"),
            Self::InvalidKey => write!(f, "Invalid encryption key"),
            Self::NotEncrypted => write!(f, "Database is not encrypted"),
            Self::AlreadyEncrypted => write!(f, "Database is already encrypted"),
        }
    }
}

impl std::error::Error for EncryptedPagerError {}

impl From<PagerError> for EncryptedPagerError {
    fn from(e: PagerError) -> Self {
        match e {
            PagerError::InvalidKey => Self::InvalidKey,
            PagerError::EncryptionRequired => Self::NotEncrypted,
            PagerError::PlainDatabaseRefusesKey => Self::AlreadyEncrypted,
            other => Self::Pager(other),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct EncryptedPagerConfig {
    pub pager_config: PagerConfig,
    pub key: Option<SecureKey>,
}

/// Thin wrapper around `Pager` that maps the legacy
/// `EncryptedPager::{read_page, write_page}` calls onto the new
/// `Pager::{read_page_decrypted, write_page_encrypted}` surface.
#[deprecated(
    note = "use Pager with PagerConfig::encryption + read_page_decrypted/write_page_encrypted"
)]
pub struct EncryptedPager {
    inner: Pager,
    is_encrypted: bool,
    path: PathBuf,
}

#[allow(deprecated)]
impl EncryptedPager {
    pub fn open<P: AsRef<Path>>(
        path: P,
        config: EncryptedPagerConfig,
    ) -> Result<Self, EncryptedPagerError> {
        let path_buf = path.as_ref().to_path_buf();
        let mut pager_config = config.pager_config;
        let is_encrypted = config.key.is_some();
        pager_config.encryption = config.key;
        let inner = Pager::open(&path_buf, pager_config)?;
        Ok(Self {
            inner,
            is_encrypted,
            path: path_buf,
        })
    }

    pub fn is_encrypted(&self) -> bool {
        self.is_encrypted
    }

    pub fn read_page(&self, page_id: u32) -> Result<Page, EncryptedPagerError> {
        Ok(self.inner.read_page_decrypted(page_id)?)
    }

    pub fn write_page(&self, page_id: u32, page: Page) -> Result<(), EncryptedPagerError> {
        Ok(self.inner.write_page_encrypted(page_id, page)?)
    }

    pub fn allocate_page(&self, page_type: PageType) -> Result<Page, EncryptedPagerError> {
        Ok(self.inner.allocate_page(page_type)?)
    }

    pub fn free_page(&self, page_id: u32) -> Result<(), EncryptedPagerError> {
        Ok(self.inner.free_page(page_id)?)
    }

    pub fn sync(&self) -> Result<(), EncryptedPagerError> {
        Ok(self.inner.sync()?)
    }

    pub fn page_count(&self) -> u32 {
        self.inner.page_count().unwrap_or(0)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
