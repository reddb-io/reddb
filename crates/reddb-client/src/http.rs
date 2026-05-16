//! HTTP / HTTPS transport for the Rust client.
//!
//! Mirrors `drivers/js/src/http.js` so JS + Rust drivers agree on
//! the same REST endpoints. Uses `reqwest` with `rustls-tls`
//! to share the same crypto stack the redwire-tls feature
//! already pulls in.
//!
//! Endpoint mapping (server-side at `src/server/routing.rs`):
//!   query              → POST /query
//!   insert             → POST /collections/:name/rows
//!   bulk_insert        → POST /collections/:name/bulk/rows
//!   get                → GET  /collections/:name/{id}
//!   delete             → DELETE /collections/:name/{id}
//!   health             → GET  /health
//!   version            → GET  /admin/version
//!   auth.login         → POST /auth/login
//!   auth.whoami        → GET  /auth/whoami

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Client, ClientBuilder, Method, StatusCode};
use serde_json::Value;

use crate::error::{ClientError, ErrorCode, Result};
use crate::types::{BulkInsertResult, InsertResult, JsonValue, KvWatchEvent, QueryResult};

/// HTTP/HTTPS client. Talks the REST surface at the configured
/// `base_url`. `Authorization: Bearer <token>` set when the user
/// supplied a session token (or completed `/auth/login`).
#[derive(Debug, Clone)]
pub struct HttpClient {
    base_url: String,
    inner: Client,
    token: Option<String>,
}

/// Configuration accepted by `HttpClient::connect`.
#[derive(Debug, Clone)]
pub struct HttpOptions {
    pub base_url: String,
    pub token: Option<String>,
    /// Skip TLS server-cert verification (dev only).
    pub dangerous_accept_invalid_certs: bool,
}

impl HttpOptions {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: None,
            dangerous_accept_invalid_certs: false,
        }
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    pub fn with_dangerous_accept_invalid_certs(mut self, accept: bool) -> Self {
        self.dangerous_accept_invalid_certs = accept;
        self
    }
}

impl HttpClient {
    pub async fn connect(opts: HttpOptions) -> Result<Self> {
        let mut builder =
            ClientBuilder::new().user_agent(format!("reddb-rs/{}", env!("CARGO_PKG_VERSION")));
        if opts.dangerous_accept_invalid_certs {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let client = builder
            .build()
            .map_err(|e| ClientError::new(ErrorCode::Network, format!("reqwest: {e}")))?;
        let handle = Self {
            base_url: opts.base_url,
            inner: client,
            token: opts.token,
        };
        // Sanity check before returning.
        handle.health().await?;
        Ok(handle)
    }

    /// Exchange username + password at `POST /auth/login` for a
    /// bearer token, then store it for subsequent calls. Returns
    /// the full login envelope so callers can see role + expiry.
    pub async fn login(&mut self, username: &str, password: &str) -> Result<Value> {
        let body = serde_json::json!({
            "username": username,
            "password": password,
        });
        let url = format!("{}/auth/login", self.base_url);
        let response = self
            .inner
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(net_err)?;
        let value = decode_envelope(response).await?;
        if let Some(token) = value
            .as_object()
            .and_then(|o| o.get("token"))
            .and_then(|v| v.as_str())
        {
            self.token = Some(token.to_string());
        }
        Ok(value)
    }

    pub async fn health(&self) -> Result<Value> {
        let url = format!("{}/health", self.base_url);
        let resp = self.inner.get(&url).send().await.map_err(net_err)?;
        decode_envelope(resp).await
    }

    pub async fn query(&self, sql: &str) -> Result<QueryResult> {
        let body = serde_json::json!({ "query": sql });
        let value = self.send_json(Method::POST, "/query", &body).await?;
        Ok(QueryResult::from_envelope(value))
    }

    /// Parameterized HTTP query — sends `{"query": sql, "params": [...]}`
    /// to `POST /query`. The server-side binder (`#358`) accepts the
    /// same JSON envelope shape `Value::into_json_param` emits.
    pub async fn query_with(
        &self,
        sql: &str,
        params: &[crate::params::Value],
    ) -> Result<QueryResult> {
        let json_params: Vec<serde_json::Value> = params
            .iter()
            .cloned()
            .map(crate::params::Value::into_json_param)
            .collect();
        let body = serde_json::json!({ "query": sql, "params": json_params });
        let value = self.send_json(Method::POST, "/query", &body).await?;
        Ok(QueryResult::from_envelope(value))
    }

    pub async fn insert(&self, collection: &str, payload: &JsonValue) -> Result<InsertResult> {
        let url_path = format!("/collections/{}/rows", urlencoded(collection),);
        let value = self
            .send_json(Method::POST, &url_path, &json_value_to_serde(payload))
            .await?;
        let affected = value
            .as_object()
            .and_then(|o| o.get("affected"))
            .and_then(|v| v.as_u64())
            .unwrap_or_else(|| {
                value
                    .as_object()
                    .and_then(|o| o.get("id"))
                    .map(|_| 1)
                    .unwrap_or(0)
            });
        let rid = value
            .as_object()
            .and_then(|o| o.get("rid").or_else(|| o.get("id")))
            .and_then(json_id_to_string);
        let id = value
            .as_object()
            .and_then(|o| o.get("id"))
            .and_then(json_id_to_string)
            .or_else(|| rid.clone());
        Ok(InsertResult { affected, rid, id })
    }

    pub async fn bulk_insert(
        &self,
        collection: &str,
        payloads: &[JsonValue],
    ) -> Result<BulkInsertResult> {
        let url_path = format!("/collections/{}/bulk/rows", urlencoded(collection),);
        let body = serde_json::json!({
            "rows": payloads.iter().map(json_value_to_serde).collect::<Vec<_>>(),
        });
        let value = self.send_json(Method::POST, &url_path, &body).await?;
        Ok(bulk_insert_result_from_json(value))
    }

    pub async fn delete(&self, collection: &str, id: &str) -> Result<u64> {
        let url_path = format!("/collections/{}/{}", urlencoded(collection), urlencoded(id),);
        let url = format!("{}{}", self.base_url, url_path);
        let resp = self
            .inner
            .delete(&url)
            .headers(self.headers())
            .send()
            .await
            .map_err(net_err)?;
        let value = decode_envelope(resp).await?;
        Ok(value
            .as_object()
            .and_then(|o| o.get("affected"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0))
    }

    pub async fn watch_kv(
        &self,
        collection: &str,
        key: &str,
        since_lsn: Option<u64>,
        limit: Option<u64>,
    ) -> Result<Vec<KvWatchEvent>> {
        let mut path = format!(
            "/collections/{}/kv/{}/watch",
            urlencoded(collection),
            urlencoded(key)
        );
        let mut parts = Vec::new();
        if let Some(lsn) = since_lsn {
            parts.push(format!("since_lsn={lsn}"));
        }
        if let Some(limit) = limit {
            parts.push(format!("limit={limit}"));
        }
        if !parts.is_empty() {
            path.push('?');
            path.push_str(&parts.join("&"));
        }
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .inner
            .request(Method::GET, &url)
            .headers(self.headers())
            .send()
            .await
            .map_err(net_err)?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ClientError::new(ErrorCode::Network, format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(http_err(status, Some(Value::String(text))));
        }
        let mut out = Vec::new();
        for block in text.split("\n\n") {
            let Some(line) = block.lines().find(|line| line.starts_with("data: ")) else {
                continue;
            };
            let value = serde_json::from_str::<Value>(&line[6..]).map_err(|e| {
                ClientError::new(ErrorCode::Protocol, format!("decode kv watch event: {e}"))
            })?;
            out.push(KvWatchEvent {
                key: value
                    .get("key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                op: value
                    .get("op")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                before: value.get("before").cloned().unwrap_or(Value::Null),
                after: value.get("after").cloned().unwrap_or(Value::Null),
                lsn: value.get("lsn").and_then(Value::as_u64).unwrap_or(0),
                committed_at: value
                    .get("committed_at")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                dropped_event_count: value
                    .get("dropped_event_count")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            });
        }
        Ok(out)
    }

    pub async fn close(&self) -> Result<()> {
        // HTTP is stateless — nothing to close.
        Ok(())
    }

    async fn send_json(&self, method: Method, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .inner
            .request(method, &url)
            .headers(self.headers())
            .json(body)
            .send()
            .await
            .map_err(net_err)?;
        decode_envelope(resp).await
    }

    fn headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(t) = &self.token {
            if let Ok(v) = HeaderValue::from_str(&format!("Bearer {t}")) {
                h.insert(AUTHORIZATION, v);
            }
        }
        h
    }
}

async fn decode_envelope(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| ClientError::new(ErrorCode::Network, format!("read body: {e}")))?;
    let body: Option<Value> = if text.is_empty() {
        None
    } else {
        match serde_json::from_str::<Value>(&text) {
            Ok(v) => Some(v),
            Err(_) => Some(Value::String(text.clone())),
        }
    };
    if !status.is_success() {
        return Err(http_err(status, body));
    }
    // RedDB envelope is `{ ok, result, error? }` for most endpoints
    // and bare JSON for some; unwrap when present.
    if let Some(Value::Object(map)) = &body {
        if let Some(Value::Bool(false)) = map.get("ok") {
            let msg = map
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("server returned ok=false")
                .to_string();
            return Err(ClientError::new(ErrorCode::Engine, msg));
        }
        if let Some(result) = map.get("result") {
            return Ok(result.clone());
        }
    }
    Ok(body.unwrap_or(Value::Null))
}

fn http_err(status: StatusCode, body: Option<Value>) -> ClientError {
    let msg = body
        .as_ref()
        .and_then(|v| v.as_object())
        .and_then(|o| o.get("error"))
        .and_then(|x| x.as_str())
        .map(String::from)
        .or_else(|| body.as_ref().and_then(|v| v.as_str()).map(String::from))
        .unwrap_or_else(|| format!("request failed with status {status}"));
    let code = match status.as_u16() {
        401 | 403 => ErrorCode::AuthRefused,
        404 => ErrorCode::NotFound,
        _ if status.is_server_error() => ErrorCode::Engine,
        _ => ErrorCode::Protocol,
    };
    ClientError::new(code, msg)
}

fn net_err(err: reqwest::Error) -> ClientError {
    ClientError::new(ErrorCode::Network, err.to_string())
}

fn urlencoded(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{:02X}", byte);
            }
        }
    }
    out
}

fn json_value_to_serde(v: &JsonValue) -> Value {
    // Bridge our minimal JsonValue to serde_json::Value via the
    // owned-string round-trip. The driver's JsonValue API is
    // intentionally simple; this saves duplicating its match.
    match serde_json::from_str::<Value>(&v.to_json_string()) {
        Ok(v) => v,
        Err(_) => Value::Null,
    }
}

fn bulk_insert_result_from_json(value: Value) -> BulkInsertResult {
    let affected = value
        .as_object()
        .and_then(|o| o.get("affected"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let rids: Vec<String> = value
        .as_object()
        .and_then(|o| o.get("rids").or_else(|| o.get("ids")))
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(json_id_to_string).collect())
        .unwrap_or_default();
    let ids = value
        .as_object()
        .and_then(|o| o.get("ids"))
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(json_id_to_string).collect())
        .unwrap_or_else(|| rids.clone());
    BulkInsertResult {
        affected,
        rids,
        ids,
    }
}

fn json_id_to_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(String::from)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
}
