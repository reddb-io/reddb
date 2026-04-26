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
    /// A compare-and-swap / conditional write precondition failed.
    PreconditionFailed(String),
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
            Self::PreconditionFailed(msg) => write!(f, "backend precondition failed: {msg}"),
            Self::Config(msg) => write!(f, "backend config error: {msg}"),
            Self::Internal(msg) => write!(f, "backend internal error: {msg}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// Backend-specific version token for one remote object.
///
/// S3-compatible backends use ETag, generic HTTP backends use ETag, and
/// LocalBackend derives a content hash token. Callers must treat the
/// token as opaque and feed it back through conditional operations only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendObjectVersion {
    pub token: String,
}

impl BackendObjectVersion {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

/// Conditional upload semantics for backends that support compare-and-swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalPut {
    /// Succeed only when the target object is absent.
    IfAbsent,
    /// Succeed only when the target object still has this version token.
    IfVersion(BackendObjectVersion),
}

/// Conditional delete semantics for backends that support compare-and-swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalDelete {
    /// Succeed only when the target object still has this version token.
    IfVersion(BackendObjectVersion),
}

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

/// Backends that can enforce compare-and-swap atomically.
///
/// Where `RemoteBackend` is "snapshot transport," `AtomicRemoteBackend`
/// is the contract callers need when they cannot tolerate lost updates
/// (writer leases, distributed locks, ledger appenders). Implementing
/// this trait is a *promise* the backend never silently overwrites a
/// versioned object — preconditions translate to backend-native
/// guarantees (S3 ETag + If-Match, FS lock + content-hash CAS, HTTP
/// servers that honor RFC 7232 preconditions).
///
/// Backends that cannot meet that promise (Turso, D1, HTTP servers
/// without ETag) deliberately do **not** implement this trait, so a
/// caller that needs CAS will fail at compile time rather than at the
/// first contended write.
pub trait AtomicRemoteBackend: RemoteBackend {
    /// Return the current opaque version token for an object.
    /// `Ok(None)` means the object does not exist.
    fn object_version(
        &self,
        remote_key: &str,
    ) -> Result<Option<BackendObjectVersion>, BackendError>;

    /// Upload a local file only if the backend-side condition still
    /// holds. Returns the new version token on success;
    /// `BackendError::PreconditionFailed` on contention.
    fn upload_conditional(
        &self,
        local_path: &Path,
        remote_key: &str,
        condition: ConditionalPut,
    ) -> Result<BackendObjectVersion, BackendError>;

    /// Delete a remote object only if the backend-side condition
    /// still holds. `BackendError::PreconditionFailed` on contention.
    fn delete_conditional(
        &self,
        remote_key: &str,
        condition: ConditionalDelete,
    ) -> Result<(), BackendError>;
}

#[cfg(feature = "backend-d1")]
pub use d1::{D1Backend, D1Config};
pub use http::{AtomicHttpBackend, HttpBackend, HttpBackendConfig};
pub use local::LocalBackend;
#[cfg(feature = "backend-s3")]
pub use s3::{S3Backend, S3Config};
#[cfg(feature = "backend-turso")]
pub use turso::{TursoBackend, TursoConfig};
