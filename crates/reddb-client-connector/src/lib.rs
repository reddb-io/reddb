//! Deprecated compatibility shim for the authenticated gRPC client.
//!
//! The connector folded into [`reddb_grpc_proto`]: the authenticated client
//! now lives beside the generated tonic stubs as
//! [`reddb_grpc_proto::RedDBClient`], and the mirror response structs are
//! gone in favour of the generated reply messages.
//!
//! Everything below is kept working — same signatures, same return types,
//! same display strings — so existing consumers keep compiling while they
//! migrate. Every public item carries its own `#[deprecated]` attribute
//! (crate-level `#![deprecated]` and plain `pub use` do not warn dependents),
//! and the whole crate is removed at the next major release.

use std::error::Error;

use reddb_grpc_proto::{BulkEntityReply, QueryValue};

/// Deprecated alias — shape-identical to the generated reply.
#[deprecated(
    since = "1.23.2",
    note = "use reddb_grpc_proto::HealthReply — this crate is removed at the next major"
)]
pub type HealthStatus = reddb_grpc_proto::HealthReply;

/// Deprecated alias — shape-identical to the generated reply.
#[deprecated(
    since = "1.23.2",
    note = "use reddb_grpc_proto::QueryReply — this crate is removed at the next major"
)]
pub type QueryResponse = reddb_grpc_proto::QueryReply;

/// Deprecated alias — shape-identical to the generated reply.
#[deprecated(
    since = "1.23.2",
    note = "use reddb_grpc_proto::EntityReply — this crate is removed at the next major"
)]
pub type CreatedEntity = reddb_grpc_proto::EntityReply;

/// Deprecated alias — shape-identical to the generated reply.
#[deprecated(
    since = "1.23.2",
    note = "use reddb_grpc_proto::OperationReply — this crate is removed at the next major"
)]
pub type OperationStatus = reddb_grpc_proto::OperationReply;

/// Deprecated bulk-create summary.
///
/// Not an alias: the canonical [`reddb_grpc_proto::BulkEntityReply`] carries
/// full `items` (id + entity JSON) where this shim only ever exposed `ids`.
#[deprecated(
    since = "1.23.2",
    note = "use reddb_grpc_proto::BulkEntityReply and read `items` — this crate is removed at the next major"
)]
#[derive(Debug, Clone)]
pub struct BulkCreateStatus {
    pub ok: bool,
    pub count: u64,
    pub ids: Vec<u64>,
}

#[allow(deprecated)]
impl From<BulkEntityReply> for BulkCreateStatus {
    fn from(reply: BulkEntityReply) -> Self {
        Self {
            ok: reply.ok,
            count: reply.count,
            ids: reply.items.into_iter().map(|item| item.id).collect(),
        }
    }
}

/// Deprecated wrapper over [`reddb_grpc_proto::RedDBClient`].
///
/// Every method delegates to the canonical client and then reshapes the
/// reply into the type this crate used to return.
#[deprecated(
    since = "1.23.2",
    note = "use reddb_grpc_proto::RedDBClient — this crate is removed at the next major"
)]
#[derive(Clone)]
pub struct RedDBClient {
    inner: reddb_grpc_proto::RedDBClient,
    pub addr: String,
}

#[allow(deprecated)]
impl RedDBClient {
    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::connect — this crate is removed at the next major"
    )]
    pub async fn connect(addr: &str, token: Option<String>) -> Result<Self, Box<dyn Error>> {
        let inner = reddb_grpc_proto::RedDBClient::connect(addr, token).await?;
        let addr = inner.addr.clone();
        Ok(Self { inner, addr })
    }

    /// Update the auth token (e.g. after a successful login).
    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::set_token — this crate is removed at the next major"
    )]
    pub fn set_token(&mut self, token: String) {
        self.inner.set_token(token);
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::health_status — this crate is removed at the next major"
    )]
    pub async fn health_status(&mut self) -> Result<HealthStatus, Box<dyn Error>> {
        self.inner.health_status().await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::health_status and format the reply — this crate is removed at the next major"
    )]
    pub async fn health(&mut self) -> Result<String, Box<dyn Error>> {
        let reply = self.inner.health_status().await?;
        Ok(format!(
            "state: {}, healthy: {}",
            reply.state, reply.healthy
        ))
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::query_reply — this crate is removed at the next major"
    )]
    pub async fn query_reply(&mut self, sql: &str) -> Result<QueryResponse, Box<dyn Error>> {
        self.inner.query_reply(sql).await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::query_reply_with_params — this crate is removed at the next major"
    )]
    pub async fn query_reply_with_params(
        &mut self,
        sql: &str,
        params: Vec<QueryValue>,
    ) -> Result<QueryResponse, Box<dyn Error>> {
        self.inner.query_reply_with_params(sql, params).await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::query — this crate is removed at the next major"
    )]
    pub async fn query(&mut self, sql: &str) -> Result<String, Box<dyn Error>> {
        self.inner.query(sql).await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::collections — this crate is removed at the next major"
    )]
    pub async fn collections(&mut self) -> Result<Vec<String>, Box<dyn Error>> {
        self.inner.collections().await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::scan and format the reply — this crate is removed at the next major"
    )]
    pub async fn scan(&mut self, collection: &str, limit: u64) -> Result<String, Box<dyn Error>> {
        let reply = self.inner.scan(collection, limit).await?;
        let items: Vec<String> = reply.items.iter().map(|e| e.json.clone()).collect();
        Ok(format!(
            "total: {}, items: [{}]",
            reply.total,
            items.join(", ")
        ))
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::stats and format the reply — this crate is removed at the next major"
    )]
    pub async fn stats(&mut self) -> Result<String, Box<dyn Error>> {
        let reply = self.inner.stats().await?;
        Ok(format!(
            "collections: {}, entities: {}, memory: {} bytes, started_at: {}",
            reply.collection_count,
            reply.total_entities,
            reply.total_memory_bytes,
            reply.started_at_unix_ms
        ))
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::create_row_entity and format the reply — this crate is removed at the next major"
    )]
    pub async fn create_row(
        &mut self,
        collection: &str,
        json: &str,
    ) -> Result<String, Box<dyn Error>> {
        let reply = self.inner.create_row_entity(collection, json).await?;
        Ok(format!("id: {}, entity: {}", reply.id, reply.entity_json))
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::create_row_entity — this crate is removed at the next major"
    )]
    pub async fn create_row_entity(
        &mut self,
        collection: &str,
        json: &str,
    ) -> Result<CreatedEntity, Box<dyn Error>> {
        self.inner.create_row_entity(collection, json).await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::bulk_create_rows — this crate is removed at the next major"
    )]
    pub async fn bulk_create_rows(
        &mut self,
        collection: &str,
        payload_json: Vec<String>,
    ) -> Result<BulkCreateStatus, Box<dyn Error>> {
        let reply = self
            .inner
            .bulk_create_rows(collection, payload_json)
            .await?;
        Ok(BulkCreateStatus::from(reply))
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::explain — this crate is removed at the next major"
    )]
    pub async fn explain(&mut self, sql: &str) -> Result<String, Box<dyn Error>> {
        self.inner.explain(sql).await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::login — this crate is removed at the next major"
    )]
    pub async fn login(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<String, Box<dyn Error>> {
        self.inner.login(username, password).await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::replication_status — this crate is removed at the next major"
    )]
    pub async fn replication_status(&mut self) -> Result<String, Box<dyn Error>> {
        self.inner.replication_status().await
    }

    /// Fetch the canonical `Topology` payload (issue #167 / ADR 0008).
    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::topology — this crate is removed at the next major"
    )]
    pub async fn topology(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        self.inner.topology().await
    }

    #[deprecated(
        since = "1.23.2",
        note = "use reddb_grpc_proto::RedDBClient::delete_entity — this crate is removed at the next major"
    )]
    pub async fn delete_entity(
        &mut self,
        collection: &str,
        id: u64,
    ) -> Result<OperationStatus, Box<dyn Error>> {
        self.inner.delete_entity(collection, id).await
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use reddb_grpc_proto::EntityReply;

    /// The one piece of non-delegating logic left in the shim: the canonical
    /// reply carries full `items`, the deprecated shape carries only `ids`.
    #[test]
    fn bulk_create_status_keeps_ids_from_canonical_items() {
        let reply = BulkEntityReply {
            ok: true,
            count: 2,
            items: vec![
                EntityReply {
                    ok: true,
                    id: 7,
                    entity_json: "{\"a\":1}".to_string(),
                },
                EntityReply {
                    ok: true,
                    id: 9,
                    entity_json: "{\"a\":2}".to_string(),
                },
            ],
        };

        let status = BulkCreateStatus::from(reply);

        assert!(status.ok);
        assert_eq!(status.count, 2);
        assert_eq!(status.ids, vec![7, 9]);
    }

    #[test]
    fn bulk_create_status_maps_empty_items_to_empty_ids() {
        let reply = BulkEntityReply {
            ok: false,
            count: 0,
            items: Vec::new(),
        };

        let status = BulkCreateStatus::from(reply);

        assert!(!status.ok);
        assert_eq!(status.count, 0);
        assert!(status.ids.is_empty());
    }
}
