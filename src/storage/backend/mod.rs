//! Storage Backend Abstraction
//!
//! Enables RedDB to persist database snapshots to remote storage backends
//! (S3, R2, DigitalOcean Spaces, GCS, Turso/libSQL, Cloudflare D1).
//!
//! The pattern is "snapshot transport":
//! - On open: download from remote -> local temp file -> open as normal
//! - On flush: save to local file -> upload to remote
//!
//! # Example
//! ```ignore
//! use reddb::storage::backend::{S3Backend, S3Config};
//!
//! let backend = S3Backend::new(S3Config {
//!     endpoint: "https://s3.amazonaws.com".into(),
//!     bucket: "my-reddb-backups".into(),
//!     key_prefix: "databases/".into(),
//!     access_key: "AKIA...".into(),
//!     secret_key: "...".into(),
//!     region: "us-east-1".into(),
//! });
//!
//! let options = RedDBOptions::persistent("./local-cache")
//!     .with_remote_backend(std::sync::Arc::new(backend), "databases/mydb.rdb");
//! ```

#[cfg(feature = "backend-d1")]
pub mod d1;
pub mod http;
pub mod local;
#[cfg(feature = "backend-s3")]
pub mod s3;
#[cfg(feature = "backend-turso")]
pub mod turso;

use std::fmt;
use std::path::Path;

/// Error type for backend operations.
#[derive(Debug)]
pub enum BackendError {
    /// Network or I/O error during transfer.
    Transport(String),
    /// Authentication or authorization failure.
    Auth(String),
    /// The requested resource was not found.
    NotFound(String),
    /// Configuration error.
    Config(String),
    /// Backend-specific error.
    Internal(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "backend transport error: {msg}"),
            Self::Auth(msg) => write!(f, "backend auth error: {msg}"),
            Self::NotFound(msg) => write!(f, "backend not found: {msg}"),
            Self::Config(msg) => write!(f, "backend config error: {msg}"),
            Self::Internal(msg) => write!(f, "backend internal error: {msg}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// Trait for remote storage backends.
///
/// Implementations handle downloading and uploading database snapshots
/// to/from remote storage. Operations are blocking (called during
/// attach/reclaim lifecycle phases, not in hot query paths).
pub trait RemoteBackend: Send + Sync {
    /// Human-readable name of this backend (e.g., "s3", "r2", "turso", "d1").
    fn name(&self) -> &str;

    /// Download a remote object to a local file path.
    /// Returns `Ok(true)` if downloaded, `Ok(false)` if remote object doesn't exist.
    fn download(&self, remote_key: &str, local_path: &Path) -> Result<bool, BackendError>;

    /// Upload a local file to remote storage.
    fn upload(&self, local_path: &Path, remote_key: &str) -> Result<(), BackendError>;

    /// Check if a remote object exists.
    fn exists(&self, remote_key: &str) -> Result<bool, BackendError>;

    /// Delete a remote object. Returns Ok(()) even if it didn't exist.
    fn delete(&self, remote_key: &str) -> Result<(), BackendError>;

    /// List remote objects matching a prefix.
    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError>;
}

#[cfg(feature = "backend-d1")]
pub use d1::{D1Backend, D1Config};
pub use http::{HttpBackend, HttpBackendConfig};
pub use local::LocalBackend;
#[cfg(feature = "backend-s3")]
pub use s3::{S3Backend, S3Config};
#[cfg(feature = "backend-turso")]
pub use turso::{TursoBackend, TursoConfig};
