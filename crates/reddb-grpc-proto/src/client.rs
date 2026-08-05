use std::error::Error;

use reddb_wire::auth::{bearer_authorization_value, login_payload_json, AUTHORIZATION_HEADER};
use tonic::transport::Channel;
use tonic::Request;

use crate::red_db_client::RedDbClient;
use crate::{
    BulkEntityReply, DeleteEntityRequest, Empty, EntityReply, HealthReply, JsonBulkCreateRequest,
    JsonCreateRequest, JsonPayloadRequest, OperationReply, PayloadReply, QueryReply, QueryRequest,
    QueryValue, ScanReply, ScanRequest, StatsReply, TopologyRequest,
};

/// Thin authenticated client over the generated tonic stub.
#[derive(Clone)]
pub struct RedDBClient {
    inner: RedDbClient<Channel>,
    token: Option<String>,
    pub addr: String,
}

impl RedDBClient {
    pub async fn connect(addr: &str, token: Option<String>) -> Result<Self, Box<dyn Error>> {
        let endpoint = if addr.starts_with("http") {
            addr.to_string()
        } else {
            format!("http://{addr}")
        };
        let inner = RedDbClient::connect(endpoint.clone()).await?;
        Ok(Self {
            inner,
            token,
            addr: endpoint,
        })
    }

    fn auth_request<T>(&self, inner: T) -> Request<T> {
        let mut request = Request::new(inner);
        if let Some(token) = &self.token {
            if let Ok(value) = bearer_authorization_value(token).parse() {
                request
                    .metadata_mut()
                    .insert(AUTHORIZATION_HEADER, value);
            }
        }
        request
    }

    /// Update the bearer token, for example after a successful login.
    pub fn set_token(&mut self, token: String) {
        self.token = Some(token);
    }

    pub async fn health_status(&mut self) -> Result<HealthReply, Box<dyn Error>> {
        let request = self.auth_request(Empty {});
        Ok(self.inner.health(request).await?.into_inner())
    }

    pub async fn query_reply(&mut self, sql: &str) -> Result<QueryReply, Box<dyn Error>> {
        self.query_reply_with_params(sql, Vec::new()).await
    }

    pub async fn query_reply_with_params(
        &mut self,
        sql: &str,
        params: Vec<QueryValue>,
    ) -> Result<QueryReply, Box<dyn Error>> {
        let request = self.auth_request(QueryRequest {
            query: sql.to_string(),
            entity_types: vec![],
            capabilities: vec![],
            params,
        });
        Ok(self.inner.query(request).await?.into_inner())
    }

    pub async fn query(&mut self, sql: &str) -> Result<String, Box<dyn Error>> {
        Ok(self.query_reply(sql).await?.result_json)
    }

    pub async fn collections(&mut self) -> Result<Vec<String>, Box<dyn Error>> {
        let request = self.auth_request(Empty {});
        Ok(self.inner.collections(request).await?.into_inner().collections)
    }

    pub async fn scan(
        &mut self,
        collection: &str,
        limit: u64,
    ) -> Result<ScanReply, Box<dyn Error>> {
        let request = self.auth_request(ScanRequest {
            collection: collection.to_string(),
            offset: 0,
            limit,
        });
        Ok(self.inner.scan(request).await?.into_inner())
    }

    pub async fn stats(&mut self) -> Result<StatsReply, Box<dyn Error>> {
        let request = self.auth_request(Empty {});
        Ok(self.inner.stats(request).await?.into_inner())
    }

    pub async fn create_row_entity(
        &mut self,
        collection: &str,
        json: &str,
    ) -> Result<EntityReply, Box<dyn Error>> {
        let request = self.auth_request(JsonCreateRequest {
            collection: collection.to_string(),
            payload_json: json.to_string(),
        });
        Ok(self.inner.create_row(request).await?.into_inner())
    }

    pub async fn bulk_create_rows(
        &mut self,
        collection: &str,
        payload_json: Vec<String>,
    ) -> Result<BulkEntityReply, Box<dyn Error>> {
        let request = self.auth_request(JsonBulkCreateRequest {
            collection: collection.to_string(),
            payload_json,
        });
        Ok(self.inner.bulk_create_rows(request).await?.into_inner())
    }

    pub async fn explain(&mut self, sql: &str) -> Result<String, Box<dyn Error>> {
        let request = self.auth_request(QueryRequest {
            query: sql.to_string(),
            entity_types: vec![],
            capabilities: vec![],
            params: vec![],
        });
        Ok(self
            .inner
            .explain_query(request)
            .await?
            .into_inner()
            .payload)
    }

    pub async fn login(
        &mut self,
        username: &str,
        password: &str,
    ) -> Result<String, Box<dyn Error>> {
        let request = self.auth_request(JsonPayloadRequest {
            payload_json: login_payload_json(username, password),
        });
        let reply: PayloadReply = self.inner.auth_login(request).await?.into_inner();
        Ok(reply.payload)
    }

    pub async fn replication_status(&mut self) -> Result<String, Box<dyn Error>> {
        let request = self.auth_request(Empty {});
        Ok(self
            .inner
            .replication_status(request)
            .await?
            .into_inner()
            .payload)
    }

    /// Fetch the canonical topology envelope bytes.
    pub async fn topology(&mut self) -> Result<Vec<u8>, Box<dyn Error>> {
        let request = self.auth_request(TopologyRequest {});
        Ok(self.inner.topology(request).await?.into_inner().topology_bytes)
    }

    pub async fn delete_entity(
        &mut self,
        collection: &str,
        id: u64,
    ) -> Result<OperationReply, Box<dyn Error>> {
        let request = self.auth_request(DeleteEntityRequest {
            collection: collection.to_string(),
            id,
        });
        Ok(self.inner.delete_entity(request).await?.into_inner())
    }
}
