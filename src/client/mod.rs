//! RedDB gRPC Client
//!
//! Connects to a remote RedDB server and provides an interactive REPL
//! or one-shot command execution.

pub mod repl;

use crate::grpc::proto::red_db_client::RedDbClient;
use crate::grpc::proto::*;
use tonic::transport::Channel;
use tonic::Request;

#[derive(Debug, Clone)]
pub struct HealthStatus {
    pub healthy: bool,
    pub state: String,
    pub checked_at_unix_ms: u64,
}

#[derive(Debug, Clone)]
pub struct QueryResponse {
    pub ok: bool,
    pub mode: String,
    pub statement: String,
    pub engine: String,
    pub columns: Vec<String>,
    pub record_count: u64,
    pub result_json: String,
}

#[derive(Debug, Clone)]
pub struct CreatedEntity {
    pub ok: bool,
    pub id: u64,
    pub entity_json: String,
}

#[derive(Debug, Clone)]
pub struct BulkCreateStatus {
    pub ok: bool,
    pub count: u64,
}

#[derive(Debug, Clone)]
pub struct OperationStatus {
    pub ok: bool,
    pub message: String,
}

pub struct RedDBClient {
    inner: RedDbClient<Channel>,
    token: Option<String>,
    pub(crate) addr: String,
}

impl RedDBClient {
    pub async fn connect(
        addr: &str,
        token: Option<String>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let endpoint = if addr.starts_with("http") {
            addr.to_string()
        } else {
            format!("http://{}", addr)
        };
        let inner = RedDbClient::connect(endpoint.clone()).await?;
        Ok(Self {
            inner,
            token,
            addr: endpoint,
        })
    }

    fn auth_request<T>(&self, inner: T) -> Request<T> {
        let mut req = Request::new(inner);
        if let Some(ref token) = self.token {
            if let Ok(value) = format!("Bearer {}", token).parse() {
                req.metadata_mut().insert("authorization", value);
            }
        }
        req
    }

    /// Update the auth token (e.g. after a successful login).
    pub fn set_token(&mut self, token: String) {
        self.token = Some(token);
    }

    // ========================================================================
    // Wrappers for common operations
    // ========================================================================

    pub async fn health_status(&mut self) -> Result<HealthStatus, Box<dyn std::error::Error>> {
        let req = self.auth_request(Empty {});
        let resp = self.inner.health(req).await?;
        let reply = resp.into_inner();
        Ok(HealthStatus {
            healthy: reply.healthy,
            state: reply.state,
            checked_at_unix_ms: reply.checked_at_unix_ms,
        })
    }

    pub async fn health(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let reply = self.health_status().await?;
        Ok(format!(
            "state: {}, healthy: {}",
            reply.state, reply.healthy
        ))
    }

    pub async fn query_reply(
        &mut self,
        sql: &str,
    ) -> Result<QueryResponse, Box<dyn std::error::Error>> {
        let req = self.auth_request(QueryRequest {
            query: sql.to_string(),
            entity_types: vec![],
            capabilities: vec![],
        });
        let resp = self.inner.query(req).await?;
        let reply = resp.into_inner();
        Ok(QueryResponse {
            ok: reply.ok,
            mode: reply.mode,
            statement: reply.statement,
            engine: reply.engine,
            columns: reply.columns,
            record_count: reply.record_count,
            result_json: reply.result_json,
        })
    }

    pub async fn query(&mut self, sql: &str) -> Result<String, Box<dyn std::error::Error>> {
        let reply = self.query_reply(sql).await?;
        Ok(reply.result_json)
    }

    pub async fn collections(&mut self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let req = self.auth_request(Empty {});
        let resp = self.inner.collections(req).await?;
        Ok(resp.into_inner().collections)
    }

    pub async fn scan(
        &mut self,
        collection: &str,
        limit: u64,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let req = self.auth_request(ScanRequest {
            collection: collection.to_string(),
            offset: 0,
            limit,
        });
        let resp = self.inner.scan(req).await?;
        let reply = resp.into_inner();
        let items: Vec<String> = reply.items.iter().map(|e| e.json.clone()).collect();
        Ok(format!(
            "total: {}, items: [{}]",
            reply.total,
            items.join(", ")
        ))
    }

    pub async fn stats(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let req = self.auth_request(Empty {});
        let resp = self.inner.stats(req).await?;
        let reply = resp.into_inner();
        Ok(format!(
            "collections: {}, entities: {}, memory: {} bytes, started_at: {}",
            reply.collection_count,
            reply.total_entities,
            reply.total_memory_bytes,
            reply.started_at_unix_ms
        ))
    }

    pub async fn create_row(
        &mut self,
        collection: &str,
        json: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let reply = self.create_row_entity(collection, json).await?;
        Ok(format!("id: {}, entity: {}", reply.id, reply.entity_json))
    }

    pub async fn create_row_entity(
        &mut self,
        collection: &str,
        json: &str,
    ) -> Result<CreatedEntity, Box<dyn std::error::Error>> {
        let req = self.auth_request(JsonCreateRequest {
            collection: collection.to_string(),
            payload_json: json.to_string(),
        });
        let resp = self.inner.create_row(req).await?;
        let reply = resp.into_inner();
        Ok(CreatedEntity {
            ok: reply.ok,
            id: reply.id,
            entity_json: reply.entity_json,
        })
    }

    pub async fn bulk_create_rows(
        &mut self,
        collection: &str,
        payload_json: Vec<String>,
    ) -> Result<BulkCreateStatus, Box<dyn std::error::Error>> {
        let req = self.auth_request(JsonBulkCreateRequest {
            collection: collection.to_string(),
            payload_json,
        });
        let resp = self.inner.bulk_create_rows(req).await?;
        let reply = resp.into_inner();
        Ok(BulkCreateStatus {
            ok: reply.ok,
            count: reply.count,
        })
    }

    pub async fn explain(&mut self, sql: &str) -> Result<String, Box<dyn std::error::Error>> {
        let req = self.auth_request(QueryRequest {
            query: sql.to_string(),
            entity_types: vec![],
            capabilities: vec![],
        });
        let resp = self.inner.explain_query(req).await?;
        Ok(resp.into_inner().payload)
    }

    pub async fn login(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let payload = format!(
            "{{\"username\":\"{}\",\"password\":\"{}\"}}",
            username, password
        );
        let req = self.auth_request(JsonPayloadRequest {
            payload_json: payload,
        });
        let resp = self.inner.auth_login(req).await?;
        let reply = resp.into_inner();
        Ok(reply.payload)
    }

    pub async fn replication_status(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let req = self.auth_request(Empty {});
        let resp = self.inner.replication_status(req).await?;
        Ok(resp.into_inner().payload)
    }

    pub async fn delete_entity(
        &mut self,
        collection: &str,
        id: u64,
    ) -> Result<OperationStatus, Box<dyn std::error::Error>> {
        let req = self.auth_request(DeleteEntityRequest {
            collection: collection.to_string(),
            id,
        });
        let resp = self.inner.delete_entity(req).await?;
        let reply = resp.into_inner();
        Ok(OperationStatus {
            ok: reply.ok,
            message: reply.message,
        })
    }
}
