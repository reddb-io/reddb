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
pub use types::{InsertResult, JsonValue, QueryResult, ValueOut};

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
            Target::GrpcCluster {
                primary,
                replicas,
                force_primary,
            } => {
                #[cfg(feature = "grpc")]
                {
                    return grpc::GrpcClient::connect_cluster(primary, replicas, force_primary)
                        .await
                        .map(Reddb::Grpc);
                }
                #[cfg(not(feature = "grpc"))]
                {
                    let _ = (primary, replicas, force_primary);
                    return Err(ClientError::feature_disabled("grpc"));
                }
            }
            Target::Http { base_url } => {
                #[cfg(feature = "http")]
                {
                    return http::HttpClient::connect(http::HttpOptions::new(base_url))
                        .await
                        .map(Reddb::Http);
                }
                #[cfg(not(feature = "http"))]
                {
                    let _ = base_url;
                    return Err(ClientError::feature_disabled("http"));
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

    pub async fn kv_incr(
        &self,
        collection: &str,
        key: &str,
        by: i64,
        ttl_ms: Option<u64>,
    ) -> Result<i64> {
        let result = self
            .query(&kv_counter_sql("INCR", collection, key, by, ttl_ms))
            .await?;
        kv_counter_value(&result)
    }

    pub async fn kv_decr(
        &self,
        collection: &str,
        key: &str,
        by: i64,
        ttl_ms: Option<u64>,
    ) -> Result<i64> {
        let result = self
            .query(&kv_counter_sql("DECR", collection, key, by, ttl_ms))
            .await?;
        kv_counter_value(&result)
    }

    pub async fn kv_get(&self, collection: &str, key: &str) -> Result<Option<ValueOut>> {
        match self {
            #[cfg(feature = "http")]
            Reddb::Http(c) => c
                .kv_get(collection, key)
                .await
                .map(|value| value.and_then(serde_json_to_value_out)),
            _ => {
                let result = self
                    .query(&format!("GET {}", kv_qualified_key(collection, key)))
                    .await?;
                Ok(result
                    .rows
                    .first()
                    .and_then(|row| row.iter().find(|(name, _)| name == "value"))
                    .map(|(_, value)| value.clone()))
            }
        }
    }

    pub async fn kv_set(
        &self,
        collection: &str,
        key: &str,
        value: &JsonValue,
        tags: &[String],
    ) -> Result<()> {
        match self {
            #[cfg(feature = "http")]
            Reddb::Http(c) => {
                c.kv_set(collection, key, value, tags).await?;
                Ok(())
            }
            _ => {
                self.query(&kv_set_sql(collection, key, value, tags))
                    .await?;
                Ok(())
            }
        }
    }

    pub async fn kv_delete(&self, collection: &str, key: &str) -> Result<bool> {
        match self {
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.kv_delete(collection, key).await,
            _ => {
                let result = self
                    .query(&format!("DELETE {}", kv_qualified_key(collection, key)))
                    .await?;
                Ok(result.affected > 0)
            }
        }
    }

    pub async fn kv_invalidate_tags(&self, collection: &str, tags: &[String]) -> Result<u64> {
        match self {
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.kv_invalidate_tags(collection, tags).await,
            _ => {
                let result = self
                    .query(&kv_invalidate_tags_sql(collection, tags))
                    .await?;
                Ok(result.affected)
            }
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
}

/// Crate version (matches the engine version when published in lockstep).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn kv_counter_sql(op: &str, collection: &str, key: &str, by: i64, ttl_ms: Option<u64>) -> String {
    let ttl = ttl_ms
        .map(|ms| format!(" EXPIRE {ms} ms"))
        .unwrap_or_default();
    format!(
        "{op} {}.{} BY {by}{ttl}",
        sql_ident(collection),
        sql_literal(key)
    )
}

fn kv_qualified_key(collection: &str, key: &str) -> String {
    format!("{}.{}", sql_ident(collection), sql_literal(key))
}

fn kv_set_sql(collection: &str, key: &str, value: &JsonValue, tags: &[String]) -> String {
    let tags = if tags.is_empty() {
        String::new()
    } else {
        format!(
            " TAGS [{}]",
            tags.iter()
                .map(|tag| sql_ident(tag))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "PUT {} = {}{}",
        kv_qualified_key(collection, key),
        json_value_sql(value),
        tags
    )
}

fn kv_invalidate_tags_sql(collection: &str, tags: &[String]) -> String {
    format!(
        "INVALIDATE TAGS [{}] FROM {}",
        tags.iter()
            .map(|tag| sql_ident(tag))
            .collect::<Vec<_>>()
            .join(", "),
        sql_ident(collection)
    )
}

fn json_value_sql(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "NULL".to_string(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) if value.fract() == 0.0 && value.is_finite() => {
            format!("{}", *value as i64)
        }
        JsonValue::Number(value) => value.to_string(),
        JsonValue::String(value) => sql_literal(value),
        JsonValue::Array(_) | JsonValue::Object(_) => sql_literal(&value.to_json_string()),
    }
}

fn serde_json_to_value_out(value: serde_json::Value) -> Option<ValueOut> {
    match value {
        serde_json::Value::Null => Some(ValueOut::Null),
        serde_json::Value::Bool(value) => Some(ValueOut::Bool(value)),
        serde_json::Value::Number(value) => value
            .as_i64()
            .map(ValueOut::Integer)
            .or_else(|| value.as_f64().map(ValueOut::Float)),
        serde_json::Value::String(value) => Some(ValueOut::String(value)),
        _ => None,
    }
}

fn kv_counter_value(result: &QueryResult) -> Result<i64> {
    result
        .rows
        .first()
        .and_then(|row| row.iter().find(|(name, _)| name == "value"))
        .and_then(|(_, value)| match value {
            ValueOut::Integer(value) => Some(*value),
            _ => None,
        })
        .ok_or_else(|| {
            ClientError::new(
                ErrorCode::Protocol,
                "KV counter response did not contain an integer 'value' column",
            )
        })
}

fn sql_ident(value: &str) -> String {
    if value.chars().enumerate().all(|(index, ch)| {
        ch == '_' || ch.is_ascii_alphanumeric() && (index > 0 || !ch.is_ascii_digit())
    }) {
        value.to_string()
    } else {
        format!("\"{}\"", value.replace('"', "\"\""))
    }
}

fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
