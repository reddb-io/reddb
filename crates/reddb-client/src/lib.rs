//! Official Rust client for [RedDB](https://github.com/reddb-io/reddb).
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
//! | `grpc://host:port`        | Remote tonic client                  | ✅    |
//! | `red://host:port`         | Remote tonic client (default port 5050) | ✅    |
//! | `http://host:port`        | REST client                          | ✅    |
//!
//! ## Cargo features
//!
//! - `embedded` (default) — pulls the entire RedDB engine in-process.
//! - `grpc` — opt-in remote client over tonic. Pulls the engine for
//!   its `RedDBClient` type today; a thin proto-only client is tracked
//!   in `PLAN_DRIVERS.md`.
//! - `http` — REST client.
//! - `redwire` — RedWire native TCP client (no engine dep).
//!
//! ## Internal connector
//!
//! The crate also hosts the gRPC connector + REPL used by the
//! `red` and `red_client` binaries via the [`connector`] module.
//! That layer is intentionally lighter than the published [`Reddb`]
//! API: it speaks tonic + ureq + serde_json only and never pulls
//! the engine in. It is exposed at the crate root as
//! [`RedDBClient`] and [`repl`] for back-compat with the previous
//! `reddb-client-internal` crate.

#![deny(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod connect;
pub mod connector;
pub mod error;
pub mod params;
pub mod topology;
pub mod types;

#[cfg(feature = "embedded")]
pub mod embedded;

#[cfg(feature = "grpc")]
pub mod grpc;

#[cfg(feature = "grpc")]
pub mod router;

#[cfg(feature = "redwire")]
pub mod redwire;

#[cfg(feature = "http")]
pub mod http;

pub use error::{ClientError, ErrorCode, Result};
pub use params::{IntoValue, Value};
pub use types::{InsertResult, JsonValue, KvWatchEvent, QueryResult, ValueOut};

// Back-compat re-exports for the previous `reddb-client-internal`
// crate. Workspace consumers (`reddb-server::rpc_stdio`, the `red`
// bin's REPL launcher, the `red_client` bin) import these paths
// directly.
pub use connector::{
    repl, BulkCreateStatus, CreatedEntity, HealthStatus, OperationStatus, QueryResponse,
    RedDBClient,
};

use connect::Target;

/// Top-level client handle. Use [`Reddb::connect`] to get one.
#[derive(Debug)]
pub enum Reddb {
    #[cfg(feature = "embedded")]
    Embedded(embedded::EmbeddedClient),
    #[cfg(feature = "grpc")]
    Grpc(grpc::GrpcClient),
    #[cfg(feature = "http")]
    Http(http::HttpClient),
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
                    embedded::EmbeddedClient::in_memory().map(Reddb::Embedded)
                }
                #[cfg(not(feature = "embedded"))]
                {
                    Err(ClientError::feature_disabled("embedded"))
                }
            }
            Target::File { path } => {
                #[cfg(feature = "embedded")]
                {
                    embedded::EmbeddedClient::open(path).map(Reddb::Embedded)
                }
                #[cfg(not(feature = "embedded"))]
                {
                    let _ = path;
                    Err(ClientError::feature_disabled("embedded"))
                }
            }
            Target::Grpc { endpoint } => {
                #[cfg(feature = "grpc")]
                {
                    grpc::GrpcClient::connect(endpoint).await.map(Reddb::Grpc)
                }
                #[cfg(not(feature = "grpc"))]
                {
                    let _ = endpoint;
                    Err(ClientError::feature_disabled("grpc"))
                }
            }
            Target::GrpcCluster {
                primary,
                replicas,
                force_primary,
            } => {
                #[cfg(feature = "grpc")]
                {
                    grpc::GrpcClient::connect_cluster(primary, replicas, force_primary)
                        .await
                        .map(Reddb::Grpc)
                }
                #[cfg(not(feature = "grpc"))]
                {
                    let _ = (primary, replicas, force_primary);
                    Err(ClientError::feature_disabled("grpc"))
                }
            }
            Target::Http { base_url } => {
                #[cfg(feature = "http")]
                {
                    http::HttpClient::connect(http::HttpOptions::new(base_url))
                        .await
                        .map(Reddb::Http)
                }
                #[cfg(not(feature = "http"))]
                {
                    let _ = base_url;
                    Err(ClientError::feature_disabled("http"))
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
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.query(sql).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    /// Parameterized query — `$N` placeholders in `sql` are bound to
    /// `params[N-1]`. Empty params is equivalent to [`Self::query`].
    ///
    /// Native type mapping (driver-side, [`IntoValue`]):
    ///
    /// | Rust                    | Engine `Value` variant |
    /// |-------------------------|------------------------|
    /// | `i8..i64` / `u8..u32`   | `Integer` (i64)        |
    /// | `bool`                  | `Boolean`              |
    /// | `f32` / `f64`           | `Float` (f64)          |
    /// | `&str` / `String`       | `Text`                 |
    /// | `Vec<u8>` / `&[u8]`     | `Blob`                 |
    /// | `Vec<f32>` / `&[f32]`   | `Vector`               |
    /// | `Option<T>`             | `Null` when `None`     |
    /// | `serde_json::Value`     | `Json`                 |
    /// | [`Value::Timestamp`]    | `Timestamp` (seconds)  |
    /// | [`Value::Uuid`]         | `Uuid` (16 raw bytes)  |
    ///
    /// Today the [`Reddb::Embedded`], [`Reddb::Grpc`], and [`Reddb::Http`]
    /// transports carry parameters end-to-end.
    pub async fn query_with<V: IntoValue + Clone>(
        &self,
        sql: &str,
        params: &[V],
    ) -> Result<QueryResult> {
        let values: Vec<Value> = params.iter().cloned().map(IntoValue::into_value).collect();
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.query_with(sql, &values),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.query_with(sql, &values).await,
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.query_with(sql, &values).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.insert(collection, payload),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.insert(collection, payload).await,
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.insert(collection, payload).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn bulk_insert(&self, collection: &str, payloads: &[JsonValue]) -> Result<u64> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.bulk_insert(collection, payloads),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.bulk_insert(collection, payloads).await,
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.bulk_insert(collection, payloads).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn delete(&self, collection: &str, id: &str) -> Result<u64> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.delete(collection, id),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.delete(collection, id).await,
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.delete(collection, id).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn close(&self) -> Result<()> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.close(),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.close().await,
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.close().await,
            Reddb::Unavailable(_) => Ok(()),
        }
    }

    pub fn kv(&self) -> KvClient<'_> {
        KvClient {
            db: self,
            collection: "kv_default",
        }
    }

    pub fn config(&self) -> ConfigClient<'_> {
        self.config_collection("red.config")
    }

    pub fn vault(&self) -> VaultClient<'_> {
        self.vault_collection("red.vault")
    }

    pub fn config_collection<'a>(&'a self, collection: &'a str) -> ConfigClient<'a> {
        ConfigClient {
            db: self,
            collection,
        }
    }

    pub fn vault_collection<'a>(&'a self, collection: &'a str) -> VaultClient<'a> {
        VaultClient {
            db: self,
            collection,
        }
    }
}

#[derive(Debug)]
pub struct KvClient<'a> {
    db: &'a Reddb,
    collection: &'static str,
}

impl<'a> KvClient<'a> {
    pub async fn put(&self, key: &str, value: JsonValue, tags: &[&str]) -> Result<QueryResult> {
        let tag_clause = if tags.is_empty() {
            String::new()
        } else {
            format!(
                " TAGS [{}]",
                tags.iter()
                    .map(|tag| kv_tag_literal(tag))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        self.db
            .query(&format!(
                "KV PUT {}.{} = {}{}",
                kv_identifier(self.collection),
                kv_identifier(key),
                kv_value_literal(&value),
                tag_clause
            ))
            .await
    }

    pub async fn invalidate_tags(&self, tags: &[&str]) -> Result<u64> {
        let result = self
            .db
            .query(&format!(
                "INVALIDATE TAGS [{}] FROM {}",
                tags.iter()
                    .map(|tag| kv_tag_literal(tag))
                    .collect::<Vec<_>>()
                    .join(", "),
                kv_identifier(self.collection)
            ))
            .await?;
        Ok(result.affected)
    }

    pub async fn watch(&self, key: &str) -> Result<Vec<KvWatchEvent>> {
        self.watch_from_lsn(key, None).await
    }

    pub async fn watch_from_lsn(
        &self,
        key: &str,
        from_lsn: Option<u64>,
    ) -> Result<Vec<KvWatchEvent>> {
        #[cfg(not(feature = "http"))]
        {
            let _ = key;
            let _ = from_lsn;
            let _ = self.collection;
        }
        match self.db {
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.watch_kv(self.collection, key, from_lsn, None).await,
            #[cfg(feature = "embedded")]
            Reddb::Embedded(_) => Err(ClientError::feature_disabled("kv.watch embedded")),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(_) => Err(ClientError::feature_disabled("kv.watch grpc")),
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub async fn watch_prefix(&self, prefix: &str) -> Result<Vec<KvWatchEvent>> {
        self.watch_prefix_from_lsn(prefix, None).await
    }

    pub async fn watch_prefix_from_lsn(
        &self,
        prefix: &str,
        from_lsn: Option<u64>,
    ) -> Result<Vec<KvWatchEvent>> {
        let key = format!("{prefix}.*");
        self.watch_from_lsn(&key, from_lsn).await
    }
}

#[derive(Debug)]
pub struct ConfigClient<'a> {
    db: &'a Reddb,
    collection: &'a str,
}

impl<'a> ConfigClient<'a> {
    pub async fn put(&self, key: &str, value: JsonValue, tags: &[&str]) -> Result<QueryResult> {
        let mut sql = format!(
            "PUT CONFIG {} {} = {}",
            kv_identifier(self.collection),
            kv_identifier(key),
            kv_value_literal(&value)
        );
        append_tag_clause(&mut sql, tags);
        self.db.query(&sql).await
    }

    pub async fn put_secret_ref(
        &self,
        key: &str,
        vault_collection: &str,
        vault_key: &str,
        tags: &[&str],
    ) -> Result<QueryResult> {
        let mut sql = format!(
            "PUT CONFIG {} {} = SECRET_REF(vault, {}.{})",
            kv_identifier(self.collection),
            kv_identifier(key),
            kv_identifier(vault_collection),
            kv_identifier(vault_key)
        );
        append_tag_clause(&mut sql, tags);
        self.db.query(&sql).await
    }

    pub async fn get(&self, key: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "GET CONFIG {} {}",
                kv_identifier(self.collection),
                kv_identifier(key)
            ))
            .await
    }

    pub async fn resolve(&self, key: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "RESOLVE CONFIG {} {}",
                kv_identifier(self.collection),
                kv_identifier(key)
            ))
            .await
    }
}

#[derive(Debug)]
pub struct VaultClient<'a> {
    db: &'a Reddb,
    collection: &'a str,
}

impl<'a> VaultClient<'a> {
    pub async fn put(&self, key: &str, value: JsonValue, tags: &[&str]) -> Result<QueryResult> {
        let mut sql = format!(
            "VAULT PUT {}.{} = {}",
            kv_identifier(self.collection),
            kv_identifier(key),
            kv_value_literal(&value)
        );
        append_tag_clause(&mut sql, tags);
        self.db.query(&sql).await
    }

    pub async fn get(&self, key: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "VAULT GET {}.{}",
                kv_identifier(self.collection),
                kv_identifier(key)
            ))
            .await
    }

    pub async fn unseal(&self, key: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "UNSEAL VAULT {}.{}",
                kv_identifier(self.collection),
                kv_identifier(key)
            ))
            .await
    }
}

fn append_tag_clause(sql: &mut String, tags: &[&str]) {
    if tags.is_empty() {
        return;
    }
    sql.push_str(" TAGS [");
    sql.push_str(
        &tags
            .iter()
            .map(|tag| kv_tag_literal(tag))
            .collect::<Vec<_>>()
            .join(", "),
    );
    sql.push(']');
}

fn kv_identifier(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn kv_value_literal(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "NULL".to_string(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) => value.to_string(),
        JsonValue::String(value) => format!("'{}'", value.replace('\'', "''")),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            format!("'{}'", value.to_json_string().replace('\'', "''"))
        }
    }
}

fn kv_tag_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Crate version (matches the engine version when published in lockstep).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
