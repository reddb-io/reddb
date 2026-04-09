//! RedDB gRPC Client
//!
//! Connects to a remote RedDB server and provides an interactive REPL
//! or one-shot command execution.

pub mod repl;

use crate::grpc::proto::red_db_client::RedDbClient;
use crate::grpc::proto::*;
use tonic::transport::Channel;
use tonic::Request;

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

    pub async fn health(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        let req = self.auth_request(Empty {});
        let resp = self.inner.health(req).await?;
        let reply = resp.into_inner();
        Ok(format!(
            "state: {}, healthy: {}",
            reply.state, reply.healthy
        ))
    }

    pub async fn query(&mut self, sql: &str) -> Result<String, Box<dyn std::error::Error>> {
        let req = self.auth_request(QueryRequest {
            query: sql.to_string(),
            entity_types: vec![],
            capabilities: vec![],
        });
        let resp = self.inner.query(req).await?;
        let reply = resp.into_inner();
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
        let req = self.auth_request(JsonCreateRequest {
            collection: collection.to_string(),
            payload_json: json.to_string(),
        });
        let resp = self.inner.create_row(req).await?;
        let reply = resp.into_inner();
        Ok(format!("id: {}, entity: {}", reply.id, reply.entity_json))
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
}
