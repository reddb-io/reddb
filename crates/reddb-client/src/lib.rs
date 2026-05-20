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
pub use params::{IntoParams, IntoValue, Value};
pub use types::{
    BulkInsertResult, DeleteResult, DocumentItem, ExistsResult, InsertResult, JsonValue, KvItem,
    KvWatchEvent, ListOptions, ListResult, QueryResult, Row, ValueOut,
};

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
        // Spec §3.1: empty SQL is a caller bug; reject locally before the
        // request is sent so every transport (embedded, gRPC, HTTP) maps it to
        // the same `INVALID_ARGUMENT` code.
        if sql.trim().is_empty() {
            return Err(ClientError::new(
                ErrorCode::InvalidArgument,
                "query SQL must not be empty",
            ));
        }
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
    pub async fn query_with<P: IntoParams>(&self, sql: &str, params: P) -> Result<QueryResult> {
        let values = params.into_params();
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.query_with(sql, values),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.query_with(sql, &values).await,
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.query_with(sql, &values).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    /// Parameterized execution for DML statements. This is an alias for
    /// [`Self::query_with`] because RedDB returns one query result envelope for
    /// SELECT and DML.
    pub async fn execute_with<P: IntoParams>(&self, sql: &str, params: P) -> Result<QueryResult> {
        self.query_with(sql, params).await
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

    pub async fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult> {
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

    pub async fn delete(&self, collection: &str, rid: &str) -> Result<u64> {
        match self {
            #[cfg(feature = "embedded")]
            Reddb::Embedded(c) => c.delete(collection, rid),
            #[cfg(feature = "grpc")]
            Reddb::Grpc(c) => c.delete(collection, rid).await,
            #[cfg(feature = "http")]
            Reddb::Http(c) => c.delete(collection, rid).await,
            Reddb::Unavailable(name) => Err(ClientError::feature_disabled(name)),
        }
    }

    pub fn documents(&self) -> DocumentClient<'_> {
        DocumentClient { db: self }
    }

    pub fn queue(&self) -> QueueClient<'_> {
        QueueClient { db: self }
    }

    pub fn kv_collection<'a>(&'a self, collection: &'a str) -> KvClient<'a> {
        KvClient {
            db: self,
            collection,
        }
    }

    pub async fn begin(&self) -> Result<QueryResult> {
        self.query("BEGIN").await
    }

    pub async fn commit(&self) -> Result<QueryResult> {
        self.query("COMMIT").await
    }

    pub async fn rollback(&self) -> Result<QueryResult> {
        self.query("ROLLBACK").await
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
pub struct DocumentClient<'a> {
    db: &'a Reddb,
}

impl<'a> DocumentClient<'a> {
    pub async fn insert(&self, collection: &str, body: &JsonValue) -> Result<DocumentItem> {
        ensure_json_object("document body", body)?;
        let collection = sql_identifier(collection)?;
        self.db
            .query(&format!("CREATE DOCUMENT IF NOT EXISTS {collection}"))
            .await?;
        let result = self
            .db
            .query(&format!(
                "INSERT INTO {collection} DOCUMENT (body) VALUES ({}) RETURNING *",
                json_text_literal(body)
            ))
            .await?;
        document_item_from_first_row(result)
    }

    pub async fn get(&self, collection: &str, rid: &str) -> Result<DocumentItem> {
        let collection = sql_identifier(collection)?;
        let result = self
            .db
            .query(&format!(
                "SELECT * FROM {collection} WHERE rid = {} LIMIT 1",
                sql_string_literal(rid)
            ))
            .await?;
        document_item_from_first_row(result)
    }

    pub async fn list(&self, collection: &str, options: ListOptions<'_>) -> Result<ListResult> {
        let collection = sql_identifier(collection)?;
        let result = self
            .db
            .query(&select_sql(&collection, "*", &options))
            .await?;
        Ok(ListResult {
            affected: result.affected,
            items: result.rows,
        })
    }

    pub async fn filter(&self, collection: &str, filter: &str) -> Result<ListResult> {
        self.list(collection, ListOptions::new().filter(filter))
            .await
    }

    pub async fn patch(
        &self,
        collection: &str,
        rid: &str,
        patch: &JsonValue,
    ) -> Result<DocumentItem> {
        let entries = patch.as_object().ok_or_else(|| {
            ClientError::new(
                ErrorCode::InvalidArgument,
                "document patch must be a JSON object",
            )
        })?;
        if entries.is_empty() {
            return Err(ClientError::new(
                ErrorCode::InvalidArgument,
                "document patch must contain at least one field",
            ));
        }
        let collection = sql_identifier(collection)?;
        let assignments = entries
            .iter()
            .map(|(field, value)| {
                Ok(format!(
                    "{} = {}",
                    sql_identifier(field)?,
                    json_value_literal(value)
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let result = self
            .db
            .query(&format!(
                "UPDATE {collection} DOCUMENTS SET {} WHERE rid = {} RETURNING *",
                assignments.join(", "),
                sql_string_literal(rid)
            ))
            .await?;
        document_item_from_first_row(result)
    }

    pub async fn delete(&self, collection: &str, rid: &str) -> Result<DeleteResult> {
        let collection = sql_identifier(collection)?;
        let result = self
            .db
            .query(&format!(
                "DELETE FROM {collection} WHERE rid = {}",
                sql_string_literal(rid)
            ))
            .await?;
        Ok(DeleteResult {
            affected: result.affected,
            deleted: result.affected > 0,
        })
    }
}

#[derive(Debug)]
pub struct QueueClient<'a> {
    db: &'a Reddb,
}

impl<'a> QueueClient<'a> {
    pub async fn create(&self, queue: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "CREATE QUEUE IF NOT EXISTS {}",
                sql_identifier(queue)?
            ))
            .await
    }

    pub async fn push(&self, queue: &str, value: &JsonValue) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "QUEUE PUSH {} {}",
                sql_identifier(queue)?,
                json_value_literal(value)
            ))
            .await
    }

    pub async fn peek(&self, queue: &str, limit: Option<u64>) -> Result<ListResult> {
        let mut sql = format!("QUEUE PEEK {}", sql_identifier(queue)?);
        if let Some(limit) = limit {
            sql.push_str(&format!(" {limit}"));
        }
        let result = self.db.query(&sql).await?;
        Ok(ListResult {
            affected: result.affected,
            items: result.rows,
        })
    }

    pub async fn pop(&self, queue: &str) -> Result<ListResult> {
        let result = self
            .db
            .query(&format!("QUEUE POP {}", sql_identifier(queue)?))
            .await?;
        Ok(ListResult {
            affected: result.affected,
            items: result.rows,
        })
    }

    pub async fn len(&self, queue: &str) -> Result<u64> {
        let result = self
            .db
            .query(&format!("QUEUE LEN {}", sql_identifier(queue)?))
            .await?;
        row_value(&result.rows, "len")
            .and_then(value_as_u64)
            .ok_or_else(|| ClientError::new(ErrorCode::InvalidResponse, "QUEUE LEN missing len"))
    }

    pub async fn purge(&self, queue: &str) -> Result<DeleteResult> {
        let result = self
            .db
            .query(&format!("QUEUE PURGE {}", sql_identifier(queue)?))
            .await?;
        Ok(DeleteResult {
            affected: result.affected,
            deleted: result.affected > 0,
        })
    }
}

#[derive(Debug)]
pub struct KvClient<'a> {
    db: &'a Reddb,
    collection: &'a str,
}

impl<'a> KvClient<'a> {
    pub async fn set(&self, key: &str, value: JsonValue) -> Result<QueryResult> {
        self.put(key, value, &[]).await
    }

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
                kv_collection_identifier(self.collection)?,
                kv_path_segment(key),
                json_value_literal(&value),
                tag_clause
            ))
            .await
    }

    pub async fn get(&self, key: &str) -> Result<Option<KvItem>> {
        let result = self
            .db
            .query(&format!(
                "KV GET {}.{}",
                kv_collection_identifier(self.collection)?,
                kv_path_segment(key)
            ))
            .await?;
        Ok(kv_item_from_rows(&result.rows))
    }

    pub async fn exists(&self, key: &str) -> Result<ExistsResult> {
        Ok(ExistsResult {
            exists: self.get(key).await?.is_some(),
        })
    }

    pub async fn delete(&self, key: &str) -> Result<DeleteResult> {
        let result = self
            .db
            .query(&format!(
                "KV DELETE {}.{}",
                kv_collection_identifier(self.collection)?,
                kv_path_segment(key)
            ))
            .await?;
        Ok(DeleteResult {
            affected: result.affected,
            deleted: result.affected > 0,
        })
    }

    pub async fn list(&self, options: ListOptions<'_>) -> Result<ListResult> {
        let collection = sql_identifier(self.collection)?;
        let result = self
            .db
            .query(&select_sql(&collection, "key, value", &options))
            .await?;
        Ok(ListResult {
            affected: result.affected,
            items: result.rows,
        })
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
                kv_collection_identifier(self.collection)?
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
            kv_collection_identifier(self.collection)?,
            kv_path_segment(key),
            json_value_literal(&value)
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
            kv_collection_identifier(self.collection)?,
            kv_path_segment(key),
            kv_collection_identifier(vault_collection)?,
            kv_path_segment(vault_key)
        );
        append_tag_clause(&mut sql, tags);
        self.db.query(&sql).await
    }

    pub async fn get(&self, key: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "GET CONFIG {} {}",
                kv_collection_identifier(self.collection)?,
                kv_path_segment(key)
            ))
            .await
    }

    pub async fn resolve(&self, key: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "RESOLVE CONFIG {} {}",
                kv_collection_identifier(self.collection)?,
                kv_path_segment(key)
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
            kv_collection_identifier(self.collection)?,
            kv_path_segment(key),
            json_value_literal(&value)
        );
        append_tag_clause(&mut sql, tags);
        self.db.query(&sql).await
    }

    pub async fn get(&self, key: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "VAULT GET {}.{}",
                kv_collection_identifier(self.collection)?,
                kv_path_segment(key)
            ))
            .await
    }

    pub async fn unseal(&self, key: &str) -> Result<QueryResult> {
        self.db
            .query(&format!(
                "UNSEAL VAULT {}.{}",
                kv_collection_identifier(self.collection)?,
                kv_path_segment(key)
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

fn sql_identifier(value: &str) -> Result<String> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err(ClientError::new(
            ErrorCode::InvalidArgument,
            "identifier must not be empty",
        ));
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(ClientError::new(
            ErrorCode::InvalidArgument,
            format!("invalid identifier `{value}`"),
        ));
    }
    if chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        Ok(value.to_string())
    } else {
        Err(ClientError::new(
            ErrorCode::InvalidArgument,
            format!("invalid identifier `{value}`"),
        ))
    }
}

fn kv_collection_identifier(value: &str) -> Result<String> {
    let mut parts = Vec::new();
    for part in value.split('.') {
        parts.push(sql_identifier(part)?);
    }
    Ok(parts.join("."))
}

fn kv_path_segment(value: &str) -> String {
    if is_plain_path_segment(value) {
        value.to_string()
    } else {
        sql_string_literal(value)
    }
}

fn is_plain_path_segment(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn json_value_literal(value: &JsonValue) -> String {
    match value {
        JsonValue::Null => "NULL".to_string(),
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::Number(value) => value.to_string(),
        JsonValue::String(value) => sql_string_literal(value),
        JsonValue::Array(_) | JsonValue::Object(_) => value.to_json_string(),
    }
}

fn json_text_literal(value: &JsonValue) -> String {
    sql_string_literal(&value.to_json_string())
}

fn kv_tag_literal(value: &str) -> String {
    sql_string_literal(value)
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn ensure_json_object(name: &str, value: &JsonValue) -> Result<()> {
    if value.as_object().is_some() {
        Ok(())
    } else {
        Err(ClientError::new(
            ErrorCode::InvalidArgument,
            format!("{name} must be a JSON object"),
        ))
    }
}

fn select_sql(collection: &str, columns: &str, options: &ListOptions<'_>) -> String {
    let mut sql = format!("SELECT {columns} FROM {collection}");
    if let Some(filter) = options.filter {
        sql.push_str(" WHERE ");
        sql.push_str(filter);
    }
    if let Some(order_by) = options.order_by {
        sql.push_str(" ORDER BY ");
        sql.push_str(order_by);
    }
    if let Some(limit) = options.limit {
        sql.push_str(&format!(" LIMIT {limit}"));
    }
    sql
}

fn document_item_from_first_row(result: QueryResult) -> Result<DocumentItem> {
    let Some(row) = result.rows.into_iter().next() else {
        return Err(ClientError::new(ErrorCode::NotFound, "document not found"));
    };
    let rid = row
        .iter()
        .find(|(column, _)| column == "rid")
        .and_then(|(_, value)| value_as_string(value))
        .ok_or_else(|| ClientError::new(ErrorCode::InvalidResponse, "document row missing rid"))?;
    Ok(DocumentItem { rid, fields: row })
}

fn kv_item_from_rows(rows: &[Row]) -> Option<KvItem> {
    let row = rows.first()?;
    let value = row
        .iter()
        .find(|(column, _)| column == "value")
        .map(|(_, value)| value.clone())?;
    let rid = row
        .iter()
        .find(|(column, _)| column == "rid")
        .map(|(_, value)| value);
    if matches!(rid, Some(ValueOut::Null)) && value == ValueOut::Null {
        return None;
    }
    let collection = row
        .iter()
        .find(|(column, _)| column == "collection")
        .and_then(|(_, value)| value_as_string(value))
        .unwrap_or_default();
    let key = row
        .iter()
        .find(|(column, _)| column == "key")
        .and_then(|(_, value)| value_as_string(value))
        .unwrap_or_default();
    Some(KvItem {
        collection,
        key,
        value,
    })
}

fn row_value<'a>(rows: &'a [Row], column: &str) -> Option<&'a ValueOut> {
    rows.first()?
        .iter()
        .find(|(name, _)| name == column)
        .map(|(_, value)| value)
}

fn value_as_string(value: &ValueOut) -> Option<String> {
    match value {
        ValueOut::String(value) => Some(value.clone()),
        ValueOut::Integer(value) => Some(value.to_string()),
        _ => None,
    }
}

fn value_as_u64(value: &ValueOut) -> Option<u64> {
    match value {
        ValueOut::Integer(value) => (*value).try_into().ok(),
        ValueOut::Float(value) if *value >= 0.0 => Some(*value as u64),
        ValueOut::String(value) => value.parse().ok(),
        _ => None,
    }
}

/// Crate version (matches the engine version when published in lockstep).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// SDK Helper Spec version this driver conforms to.
///
/// Spec §14 asks every official driver to expose a `helper_spec_version`
/// constant so cross-driver CI dashboards can assert all drivers track the
/// same contract. Rust is the reference implementation: `reddb-io-client`
/// ≥ 1.2.0 satisfies Helper Spec v1.0 (`docs/spec/sdk-helpers.md`).
pub const HELPER_SPEC_VERSION: &str = "1.0";

#[cfg(test)]
mod helper_spec_tests {
    use super::HELPER_SPEC_VERSION;

    #[test]
    fn helper_spec_version_is_pinned() {
        assert_eq!(HELPER_SPEC_VERSION, "1.0");
    }
}
