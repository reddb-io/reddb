use async_trait::async_trait;
use reddb_wire::ConnectionTarget;
use tokio::sync::Mutex;

use crate::bookmark_routing::QueryOptions;
use crate::error::{ClientError, ErrorCode, Result};
use crate::params::Value;
use crate::redwire::{Auth, ConnectOptions, RedWireClient};
use crate::types::{BulkInsertResult, InsertResult, JsonValue, KvWatchEvent, QueryResult};

pub(crate) type DynTransport = Box<dyn Transport>;

#[async_trait]
pub(crate) trait Transport: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &'static str;

    async fn query(&self, sql: &str) -> Result<QueryResult>;

    async fn query_with_options(&self, sql: &str, options: QueryOptions) -> Result<QueryResult> {
        let _ = options;
        self.query(sql).await
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<QueryResult>;

    async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult>;

    async fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult>;

    async fn delete(&self, collection: &str, rid: &str) -> Result<u64>;

    async fn watch_kv(
        &self,
        collection: &str,
        key: &str,
        from_lsn: Option<u64>,
    ) -> Result<Vec<KvWatchEvent>> {
        let _ = (collection, key, from_lsn);
        Err(ClientError::feature_disabled(&format!(
            "kv.watch {}",
            self.name()
        )))
    }

    async fn close(&self) -> Result<()>;
}

pub(crate) async fn connect(target: ConnectionTarget) -> Result<DynTransport> {
    match target {
        ConnectionTarget::Memory => {
            #[cfg(feature = "embedded")]
            {
                Ok(embedded(crate::embedded::EmbeddedClient::in_memory()?))
            }
            #[cfg(not(feature = "embedded"))]
            {
                Err(ClientError::feature_disabled("embedded"))
            }
        }
        ConnectionTarget::File { path } => {
            #[cfg(feature = "embedded")]
            {
                Ok(embedded(crate::embedded::EmbeddedClient::open(path)?))
            }
            #[cfg(not(feature = "embedded"))]
            {
                let _ = path;
                Err(ClientError::feature_disabled("embedded"))
            }
        }
        ConnectionTarget::Grpc { endpoint } => {
            #[cfg(feature = "grpc")]
            {
                Ok(grpc(crate::grpc::GrpcClient::connect(endpoint).await?))
            }
            #[cfg(not(feature = "grpc"))]
            {
                let _ = endpoint;
                Err(ClientError::feature_disabled("grpc"))
            }
        }
        ConnectionTarget::GrpcCluster {
            primary,
            replicas,
            force_primary,
        } => {
            #[cfg(feature = "grpc")]
            {
                Ok(grpc(
                    crate::grpc::GrpcClient::connect_cluster(primary, replicas, force_primary)
                        .await?,
                ))
            }
            #[cfg(not(feature = "grpc"))]
            {
                let _ = (primary, replicas, force_primary);
                Err(ClientError::feature_disabled("grpc"))
            }
        }
        ConnectionTarget::Http { base_url } => {
            #[cfg(feature = "http")]
            {
                let options = crate::http::HttpOptions::new(base_url);
                Ok(http(crate::http::HttpClient::connect(options).await?))
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = base_url;
                Err(ClientError::feature_disabled("http"))
            }
        }
        ConnectionTarget::RedWire { host, port, tls } => {
            let options = ConnectOptions::new(host, port).with_auth(Auth::Anonymous);
            #[cfg(feature = "redwire-tls")]
            let options = if tls {
                options.with_tls(crate::redwire::TlsConfig::new())
            } else {
                options
            };
            #[cfg(not(feature = "redwire-tls"))]
            if tls {
                return Err(ClientError::feature_disabled("redwire-tls"));
            }
            Ok(redwire(RedWireClient::connect(options).await?))
        }
        ConnectionTarget::WsNative { .. } => {
            Err(ClientError::feature_disabled("redwire-websocket"))
        }
    }
}

#[cfg(feature = "embedded")]
pub(crate) fn embedded(client: crate::embedded::EmbeddedClient) -> DynTransport {
    Box::new(EmbeddedAdapter(client))
}

#[cfg(feature = "grpc")]
pub(crate) fn grpc(client: crate::grpc::GrpcClient) -> DynTransport {
    Box::new(GrpcAdapter(client))
}

#[cfg(feature = "http")]
pub(crate) fn http(client: crate::http::HttpClient) -> DynTransport {
    Box::new(HttpAdapter(client))
}

pub(crate) fn redwire(client: RedWireClient) -> DynTransport {
    Box::new(RedWireAdapter(Mutex::new(Some(client))))
}

#[cfg(feature = "embedded")]
#[derive(Debug)]
struct EmbeddedAdapter(crate::embedded::EmbeddedClient);

#[cfg(feature = "embedded")]
#[async_trait]
impl Transport for EmbeddedAdapter {
    fn name(&self) -> &'static str {
        "embedded"
    }

    async fn query(&self, sql: &str) -> Result<QueryResult> {
        self.0.query(sql)
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.0.query_with(sql, params)
    }

    async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        self.0.insert(collection, payload)
    }

    async fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult> {
        self.0.bulk_insert(collection, payloads)
    }

    async fn delete(&self, collection: &str, rid: &str) -> Result<u64> {
        self.0.delete(collection, rid)
    }

    async fn close(&self) -> Result<()> {
        self.0.close()
    }
}

#[cfg(feature = "grpc")]
#[derive(Debug)]
struct GrpcAdapter(crate::grpc::GrpcClient);

#[cfg(feature = "grpc")]
#[async_trait]
impl Transport for GrpcAdapter {
    fn name(&self) -> &'static str {
        "grpc"
    }

    async fn query(&self, sql: &str) -> Result<QueryResult> {
        self.0.query(sql).await
    }

    async fn query_with_options(&self, sql: &str, options: QueryOptions) -> Result<QueryResult> {
        self.0.query_with_options(sql, options).await
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.0.query_with(sql, params).await
    }

    async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        self.0.insert(collection, payload).await
    }

    async fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult> {
        self.0.bulk_insert(collection, payloads).await
    }

    async fn delete(&self, collection: &str, rid: &str) -> Result<u64> {
        self.0.delete(collection, rid).await
    }

    async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

#[cfg(feature = "http")]
#[derive(Debug)]
struct HttpAdapter(crate::http::HttpClient);

#[cfg(feature = "http")]
#[async_trait]
impl Transport for HttpAdapter {
    fn name(&self) -> &'static str {
        "http"
    }

    async fn query(&self, sql: &str) -> Result<QueryResult> {
        self.0.query(sql).await
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        self.0.query_with(sql, params).await
    }

    async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        self.0.insert(collection, payload).await
    }

    async fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult> {
        self.0.bulk_insert(collection, payloads).await
    }

    async fn delete(&self, collection: &str, rid: &str) -> Result<u64> {
        self.0.delete(collection, rid).await
    }

    async fn watch_kv(
        &self,
        collection: &str,
        key: &str,
        from_lsn: Option<u64>,
    ) -> Result<Vec<KvWatchEvent>> {
        self.0.watch_kv(collection, key, from_lsn, None).await
    }

    async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

#[derive(Debug)]
struct RedWireAdapter(Mutex<Option<RedWireClient>>);

#[async_trait]
impl Transport for RedWireAdapter {
    fn name(&self) -> &'static str {
        "redwire"
    }

    async fn query(&self, sql: &str) -> Result<QueryResult> {
        redwire_client(&mut *self.0.lock().await)?.query(sql).await
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<QueryResult> {
        redwire_client(&mut *self.0.lock().await)?
            .query_with(sql, params)
            .await
    }

    async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        let result = redwire_client(&mut *self.0.lock().await)?
            .bulk_insert(collection, vec![json_value_to_serde(payload)])
            .await?;
        let rid = result.rids.into_iter().next();
        let id = result.ids.into_iter().next();
        Ok(InsertResult {
            affected: result.affected,
            rid,
            id,
        })
    }

    async fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult> {
        let payloads = payloads.iter().map(json_value_to_serde).collect();
        redwire_client(&mut *self.0.lock().await)?
            .bulk_insert(collection, payloads)
            .await
    }

    async fn delete(&self, collection: &str, rid: &str) -> Result<u64> {
        redwire_client(&mut *self.0.lock().await)?
            .delete(collection, rid)
            .await
    }

    async fn close(&self) -> Result<()> {
        if let Some(client) = self.0.lock().await.take() {
            client.close().await?;
        }
        Ok(())
    }
}

fn redwire_client(client: &mut Option<RedWireClient>) -> Result<&mut RedWireClient> {
    client
        .as_mut()
        .ok_or_else(|| ClientError::new(ErrorCode::Network, "RedWire connection is closed"))
}

fn json_value_to_serde(value: &JsonValue) -> serde_json::Value {
    match value {
        JsonValue::Null => serde_json::Value::Null,
        JsonValue::Bool(value) => serde_json::Value::Bool(*value),
        JsonValue::Number(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        JsonValue::String(value) => serde_json::Value::String(value.clone()),
        JsonValue::Array(values) => {
            serde_json::Value::Array(values.iter().map(json_value_to_serde).collect())
        }
        JsonValue::Object(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), json_value_to_serde(value)))
                .collect(),
        ),
    }
}
