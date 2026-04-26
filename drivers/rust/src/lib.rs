//! Official Rust client for [RedDB](https://github.com/forattini-dev/reddb).
//!
//! One connection-string API. Pick your backend at runtime:
//!
//! ```no_run
//! use reddb_client::{Reddb, JsonValue};
//!
//! # async fn run() -> reddb_client::Result<()> {
//! // Embedded: opens the engine in-process, no network.
//! let db = Reddb::connect("memory://").await?;
//! db.insert("users", &JsonValue::object([("name", JsonValue::string("Alice"))])).await?;
//! let result = db.query("SELECT * FROM users").await?;
//! println!("{} rows", result.rows.len());
//! db.close().await?;
//! # Ok(())
//! # }
//! ```
//!
//! Accepted URIs:
//!
//! | URI                       | Backend                              | Status |
//! |---------------------------|--------------------------------------|--------|
//! | `memory://`               | Ephemeral in-memory                  | ✅    |
//! | `file:///abs/path`        | Embedded engine on disk              | ✅    |
//! | `grpc://host:port`        | Remote tonic client                  | ⚠ planned |
//!
//! ## Cargo features
//!
//! - `embedded` (default) — pulls the entire RedDB engine in-process.
//! - `grpc` — reserved for the upcoming remote client.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod connect;
pub mod error;
pub mod types;

#[cfg(feature = "embedded")]
pub mod embedded;

#[cfg(feature = "grpc")]
pub mod grpc;

#[cfg(feature = "redwire")]
pub mod redwire;

pub use error::{ClientError, ErrorCode, Result};
pub use types::{InsertResult, JsonValue, QueryResult, ValueOut};

use connect::Target;

/// Top-level client handle. Use [`Reddb::connect`] to get one.
#[derive(Debug)]
pub enum Reddb {
    #[cfg(feature = "embedded")]
    Embedded(embedded::EmbeddedClient),
    #[cfg(feature = "grpc")]
    Grpc(grpc::GrpcClient),
    /// Constructed when a feature gate would have produced a real
    /// variant but the feature is disabled. Every method on this
    /// variant returns a `FEATURE_DISABLED` error so build-time
    /// configuration bugs surface as runtime errors with a clear
    /// remediation, not as missing trait impls.
    Unavailable(&'static str),
}

impl Reddb {
    /// Open a connection. The backend is selected from the URI scheme.
    pub async fn connect(uri: &str) -> Result<Self> {
        let target = connect::parse(uri)?;
        match target {
            Target::Memory => {
                #[cfg(feature = "embedded")]
                {
                    return embedded::EmbeddedClient::in_memory().map(Reddb::Embedded);
                }
                #[cfg(not(feature = "embedded"))]
                {
                    return Err(ClientError::feature_disabled("embedded"));
                }
            }
            Target::File { path } => {
                #[cfg(feature = "embedded")]
                {
                    return embedded::EmbeddedClient::open(path).map(Reddb::Embedded);
                }
                #[cfg(not(feature = "embedded"))]
                {
                    let _ = path;
                    return Err(ClientError::feature_disabled("embedded"));
                }
            }
            Target::Grpc { endpoint } => {
                #[cfg(feature = "grpc")]
                {
                    return grpc::GrpcClient::connect(endpoint).await.map(Reddb::Grpc);
                }
                #[cfg(not(feature = "grpc"))]
                {
                    let _ = endpoint;
                    return Err(ClientError::feature_disabled("grpc"));
                }
            }
        }
    }

    pub async fn query(&self, sql: &str) -> Result<QueryResult> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.query(sql),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.query(sql).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.insert(collection, payload),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.insert(collection, payload).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn bulk_insert(&self, collection: &str, payloads: &[JsonValue]) -> Result<u64> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.bulk_insert(collection, payloads),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.bulk_insert(collection, payloads).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn delete(&self, collection: &str, id: &str) -> Result<u64> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.delete(collection, id),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.delete(collection, id).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn close(&self) -> Result<()> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.close(),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.close().await,
            Reddb::Unavailable(_) => Ok(()),
        }
    }
}

/// Crate version (matches the engine version when published in lockstep).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
