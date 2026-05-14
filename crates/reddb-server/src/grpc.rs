pub(crate) use crate::application::json_input::{
    json_bool_field, json_f32_field, json_string_field, json_usize_field,
};
pub(crate) use crate::application::{
    AdminUseCases, CatalogUseCases, CreateEdgeInput, CreateEntityOutput, CreateNodeGraphLinkInput,
    CreateNodeInput, CreateNodeTableLinkInput, CreateRowInput, CreateVectorInput,
    DeleteEntityInput, EntityUseCases, ExecuteQueryInput, ExplainQueryInput, GraphCentralityInput,
    GraphClusteringInput, GraphCommunitiesInput, GraphComponentsInput, GraphCyclesInput,
    GraphHitsInput, GraphNeighborhoodInput, GraphPersonalizedPageRankInput, GraphShortestPathInput,
    GraphTopologicalSortInput, GraphTraversalInput, GraphUseCases, InspectNativeArtifactInput,
    NativeUseCases, PatchEntityInput, QueryUseCases, SearchHybridInput, SearchIvfInput,
    SearchSimilarInput, SearchTextInput,
};
use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{RedDBOptions, RedDBResult};
use crate::auth::middleware::{check_permission, AuthResult};
use crate::auth::store::AuthStore;
use crate::auth::Role;
use crate::health::{HealthProvider, HealthState};
use crate::json::{
    from_str as json_from_str, to_string as json_to_string, Map, Value as JsonValue,
};
use crate::runtime::{
    RedDBRuntime, RuntimeFilter, RuntimeFilterValue, RuntimeGraphCentralityAlgorithm,
    RuntimeGraphCentralityResult, RuntimeGraphClusteringResult, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphCommunityResult, RuntimeGraphComponentsMode, RuntimeGraphComponentsResult,
    RuntimeGraphCyclesResult, RuntimeGraphDirection, RuntimeGraphHitsResult,
    RuntimeGraphNeighborhoodResult, RuntimeGraphPathAlgorithm, RuntimeGraphPathResult,
    RuntimeGraphPattern, RuntimeGraphProjection, RuntimeGraphTopologicalSortResult,
    RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy, RuntimeIvfSearchResult,
    RuntimeQueryResult, RuntimeQueryWeights, RuntimeStats, ScanPage,
};
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, TableRef};
use crate::storage::unified::{Metadata, MetadataValue};
use crate::storage::{EntityData, EntityId, UnifiedEntity};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};

// gRPC protobuf types and tonic stubs live in the standalone
// `reddb-grpc-proto` crate so `reddb-server` and `reddb-client`
// can both consume them without a dependency cycle. We expose
// them under the legacy `proto` module path so existing
// `crate::grpc::proto::…` imports keep resolving.
pub use reddb_grpc_proto as proto;

use proto::red_db_server::{RedDb, RedDbServer};
use proto::{
    ask_stream_event, AskAnswerToken, AskReply, AskRequest, AskSources, AskStreamEvent,
    BatchQueryReply, BatchQueryRequest, BulkEntityReply, Citation, CollectionRequest,
    CollectionsReply, DeleteEntityRequest, DeploymentProfileRequest, Empty, EntityReply,
    ExecutePreparedRequest, ExportRequest, GraphProjectionUpsertRequest, HealthReply,
    IndexNameRequest, IndexToggleRequest, JsonBulkCreateRequest, JsonCreateRequest,
    JsonPayloadRequest, KvWatchEvent, KvWatchRequest, ManifestRequest, OperationReply,
    PayloadReply, PrepareQueryReply, PrepareQueryRequest, QueryReply, QueryRequest, QueryValue,
    ScanEntity, ScanReply, ScanRequest, StatsReply, TopologyReply, TopologyRequest,
    UpdateEntityRequest, Validation, ValidationItem,
};

mod control_support;
mod entity_ops;
mod input_support;
pub(crate) mod scan_json;

use self::control_support::*;
use self::entity_ops::*;
use self::input_support::*;
use self::scan_json::*;

#[derive(Debug, Clone)]
pub struct GrpcServerOptions {
    pub bind_addr: String,
    /// Optional TLS configuration. When set the server terminates
    /// TLS for inbound gRPC traffic via `tonic::transport::ServerTlsConfig`.
    /// When `None`, the listener stays plaintext (back-compat for
    /// loopback / sidecar deployments where a sidecar terminates TLS).
    pub tls: Option<GrpcTlsOptions>,
}

/// PEM-encoded TLS material for gRPC's tonic-rustls server.
///
/// The server identity is required (cert + key); the optional
/// client-CA enables mTLS — when present, tonic verifies and
/// requires a client cert chain that anchors at this CA bundle.
#[derive(Debug, Clone)]
pub struct GrpcTlsOptions {
    /// PEM bytes for the server certificate chain (leaf first).
    pub cert_pem: Vec<u8>,
    /// PEM bytes for the server private key (PKCS#8 / SEC1 / RSA).
    pub key_pem: Vec<u8>,
    /// Optional PEM bytes for the trust anchor used to verify
    /// client certificates. When `Some(_)`, the server requires
    /// every client to present a cert that chains to this CA;
    /// when `None`, the server runs one-way TLS only.
    pub client_ca_pem: Option<Vec<u8>>,
}

impl GrpcTlsOptions {
    /// Build a `tonic` `ServerTlsConfig` from PEM bytes, applying
    /// rustls defaults (TLS 1.2 + 1.3 — older versions are not
    /// negotiable on tokio-rustls 0.26).
    pub fn to_tonic_config(
        &self,
    ) -> Result<tonic::transport::ServerTlsConfig, Box<dyn std::error::Error>> {
        let identity = tonic::transport::Identity::from_pem(&self.cert_pem, &self.key_pem);
        let mut cfg = tonic::transport::ServerTlsConfig::new().identity(identity);
        if let Some(ca_pem) = &self.client_ca_pem {
            cfg = cfg.client_ca_root(tonic::transport::Certificate::from_pem(ca_pem));
        }
        Ok(cfg)
    }
}

impl Default for GrpcServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:5555".to_string(),
            tls: None,
        }
    }
}

#[derive(Clone)]
pub struct RedDBGrpcServer {
    runtime: RedDBRuntime,
    options: GrpcServerOptions,
    auth_store: Arc<AuthStore>,
    /// Optional OAuth/OIDC JWT validator. When set, the gRPC
    /// interceptor validates JWT-shaped bearers against the issuer's
    /// JWKS *before* attempting `AuthStore` session/api-key lookups.
    /// Build externally via `crate::auth::OAuthValidator::with_verifier`
    /// and attach with [`Self::with_oauth_validator`].
    oauth_validator: Option<Arc<crate::auth::OAuthValidator>>,
}

impl RedDBGrpcServer {
    pub fn new(runtime: RedDBRuntime) -> Self {
        let auth_config = crate::auth::AuthConfig::default();
        let auth_store = Arc::new(AuthStore::new(auth_config));
        Self::with_options(runtime, GrpcServerOptions::default(), auth_store)
    }

    pub fn from_database_options(
        db_options: RedDBOptions,
        options: GrpcServerOptions,
    ) -> RedDBResult<Self> {
        // Create runtime first so we can access the pager for vault pages.
        let runtime = RedDBRuntime::with_options(db_options.clone())?;

        let auth_store = if db_options.auth.vault_enabled {
            // The vault stores its encrypted state in reserved pages inside
            // the main .rdb file.  Extract the pager reference from the
            // runtime's underlying store.
            let pager = runtime.db().store().pager().cloned().ok_or_else(|| {
                crate::api::RedDBError::Internal(
                    "vault requires a paged database (persistent mode)".into(),
                )
            })?;
            let store = AuthStore::with_vault(db_options.auth.clone(), pager, None)
                .map_err(|e| crate::api::RedDBError::Internal(e.to_string()))?;
            Arc::new(store)
        } else {
            Arc::new(AuthStore::new(db_options.auth.clone()))
        };
        auth_store.bootstrap_from_env();
        Ok(Self::with_options(runtime, options, auth_store))
    }

    pub fn with_options(
        runtime: RedDBRuntime,
        options: GrpcServerOptions,
        auth_store: Arc<AuthStore>,
    ) -> Self {
        // Inject the auth store into the runtime so that Value::Secret
        // auto-encrypt/decrypt can read the vault AES key.
        runtime.set_auth_store(Arc::clone(&auth_store));
        Self {
            runtime,
            options,
            auth_store,
            oauth_validator: None,
        }
    }

    /// Attach an externally-constructed OAuth/OIDC JWT validator. Once
    /// set, JWT-shaped bearer tokens (3-segment) on the
    /// `authorization` metadata are validated against the issuer's
    /// JWKS, expiry, audience, etc. Non-JWT bearers fall back to the
    /// `AuthStore` session/API-key path.
    pub fn with_oauth_validator(mut self, validator: Arc<crate::auth::OAuthValidator>) -> Self {
        self.oauth_validator = Some(validator);
        self
    }

    /// Inspect the active OAuth validator, when one is configured.
    pub fn oauth_validator(&self) -> Option<&Arc<crate::auth::OAuthValidator>> {
        self.oauth_validator.as_ref()
    }

    pub fn runtime(&self) -> &RedDBRuntime {
        &self.runtime
    }

    pub fn options(&self) -> &GrpcServerOptions {
        &self.options
    }

    pub fn auth_store(&self) -> &Arc<AuthStore> {
        &self.auth_store
    }

    fn grpc_runtime(&self) -> GrpcRuntime {
        GrpcRuntime {
            runtime: self.runtime.clone(),
            auth_store: self.auth_store.clone(),
            prepared_registry: PreparedStatementRegistry::new(),
            oauth_validator: self.oauth_validator.clone(),
        }
    }

    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = self.options.bind_addr.parse()?;
        let mut builder = tonic::transport::Server::builder();
        if let Some(tls) = &self.options.tls {
            // Constant-time SHA256 fingerprint logged for ops triage —
            // never the bytes of cert/key themselves.
            log_grpc_tls_identity(tls);
            builder = builder.tls_config(tls.to_tonic_config()?)?;
        }
        builder
            .add_service(Self::configured_service(self.grpc_runtime()))
            .serve(addr)
            .await?;
        Ok(())
    }

    pub async fn serve_on(
        &self,
        listener: std::net::TcpListener,
    ) -> Result<(), Box<dyn std::error::Error>> {
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let incoming = TcpListenerStream::new(listener);
        let mut builder = tonic::transport::Server::builder();
        if let Some(tls) = &self.options.tls {
            log_grpc_tls_identity(tls);
            builder = builder.tls_config(tls.to_tonic_config()?)?;
        }
        builder
            .add_service(Self::configured_service(self.grpc_runtime()))
            .serve_with_incoming(incoming)
            .await?;
        Ok(())
    }

    fn configured_service(runtime: GrpcRuntime) -> RedDbServer<GrpcRuntime> {
        // Advertise zstd + gzip so clients can opt in. Server compresses
        // outbound replies with zstd; sticking to a single send codec keeps
        // CPU predictable while still accepting either on inbound.
        use tonic::codec::CompressionEncoding;
        RedDbServer::new(runtime)
            .max_decoding_message_size(256 * 1024 * 1024)
            .max_encoding_message_size(256 * 1024 * 1024)
            .accept_compressed(CompressionEncoding::Zstd)
            .accept_compressed(CompressionEncoding::Gzip)
            .send_compressed(CompressionEncoding::Zstd)
    }
}

/// Server-side prepared statement — parsed + parameterized once, executed N times.
struct GrpcPreparedStatement {
    shape: std::sync::Arc<crate::storage::query::ast::QueryExpr>,
    parameter_count: usize,
    created_at: std::time::Instant,
}

/// Registry of prepared statements for one server instance.
/// Session-independent: any connection can execute any prepared statement by ID.
struct PreparedStatementRegistry {
    // parking_lot::RwLock never poisons on panic — safe to use without unwrap().
    map: parking_lot::RwLock<std::collections::HashMap<u64, GrpcPreparedStatement>>,
    next_id: std::sync::atomic::AtomicU64,
    get_count: std::sync::atomic::AtomicU64,
}

impl PreparedStatementRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            map: parking_lot::RwLock::new(std::collections::HashMap::new()),
            next_id: std::sync::atomic::AtomicU64::new(1),
            get_count: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn prepare(&self, shape: crate::storage::query::ast::QueryExpr, parameter_count: usize) -> u64 {
        use std::sync::atomic::Ordering;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut map = self.map.write();
        self.evict_old_locked(&mut map);
        map.insert(
            id,
            GrpcPreparedStatement {
                // Store as Arc to avoid cloning the full AST on every execute.
                shape: std::sync::Arc::new(shape),
                parameter_count,
                created_at: std::time::Instant::now(),
            },
        );
        id
    }

    fn get_shape_and_count(
        &self,
        id: u64,
    ) -> Option<(std::sync::Arc<crate::storage::query::ast::QueryExpr>, usize)> {
        // Periodic eviction on execute/get traffic so long-lived servers that
        // prepare once and execute many times still age out stale statements.
        let get_count = self
            .get_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if get_count.is_multiple_of(256) {
            let mut map = self.map.write();
            self.evict_old_locked(&mut map);
        }
        let map = self.map.read();
        map.get(&id)
            .map(|s| (std::sync::Arc::clone(&s.shape), s.parameter_count))
    }

    fn evict_old_locked(&self, map: &mut std::collections::HashMap<u64, GrpcPreparedStatement>) {
        let threshold = std::time::Duration::from_secs(3600);
        map.retain(|_, v| v.created_at.elapsed() < threshold);
    }
}

#[derive(Clone)]
struct GrpcRuntime {
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    prepared_registry: Arc<PreparedStatementRegistry>,
    /// OAuth/OIDC JWT validator built once from `auth_store.config().oauth`
    /// when the operator enables OAuth. `None` means JWT bearers fall
    /// back to the AuthStore lookup path.
    oauth_validator: Option<Arc<crate::auth::OAuthValidator>>,
}

impl GrpcRuntime {
    fn admin_use_cases(&self) -> AdminUseCases<'_, RedDBRuntime> {
        AdminUseCases::new(&self.runtime)
    }

    fn catalog_use_cases(&self) -> CatalogUseCases<'_, RedDBRuntime> {
        CatalogUseCases::new(&self.runtime)
    }

    fn query_use_cases(&self) -> QueryUseCases<'_, RedDBRuntime> {
        QueryUseCases::new(&self.runtime)
    }

    fn entity_use_cases(&self) -> EntityUseCases<'_, RedDBRuntime> {
        EntityUseCases::new(&self.runtime)
    }

    fn graph_use_cases(&self) -> GraphUseCases<'_, RedDBRuntime> {
        GraphUseCases::new(&self.runtime)
    }

    fn native_use_cases(&self) -> NativeUseCases<'_, RedDBRuntime> {
        NativeUseCases::new(&self.runtime)
    }
}

fn grpc_query_value_to_schema_value(value: QueryValue) -> Result<Value, Status> {
    use proto::query_value::Kind;

    match value
        .kind
        .ok_or_else(|| Status::invalid_argument("missing query param value"))?
    {
        Kind::NullValue(_) => Ok(Value::Null),
        Kind::BoolValue(value) => Ok(Value::Boolean(value)),
        Kind::IntValue(value) => Ok(Value::Integer(value)),
        Kind::FloatValue(value) => Ok(Value::Float(value)),
        Kind::TextValue(value) => Ok(Value::Text(std::sync::Arc::from(value))),
        Kind::BytesValue(value) => Ok(Value::Blob(value)),
        Kind::VectorValue(value) => Ok(Value::Vector(value.values)),
        Kind::JsonValue(value) => {
            let parsed = json_from_str::<JsonValue>(&value)
                .map_err(|e| Status::invalid_argument(format!("json param parse error: {e}")))?;
            let encoded = json_to_string(&parsed)
                .map_err(|e| Status::invalid_argument(format!("json param encode error: {e}")))?;
            Ok(Value::Json(encoded.into_bytes()))
        }
        Kind::TimestampValue(value) => Ok(Value::Timestamp(value)),
        Kind::UuidValue(value) => {
            let bytes: [u8; 16] = value.try_into().map_err(|value: Vec<u8>| {
                Status::invalid_argument(format!(
                    "uuid param must be 16 bytes, got {}",
                    value.len()
                ))
            })?;
            Ok(Value::Uuid(bytes))
        }
    }
}

fn execute_grpc_query_with_optional_params(
    runtime: &RedDBRuntime,
    query: String,
    params: Vec<QueryValue>,
) -> Result<RuntimeQueryResult, Status> {
    if params.is_empty() {
        return runtime.execute_query(&query).map_err(to_status);
    }

    let binds = params
        .into_iter()
        .map(grpc_query_value_to_schema_value)
        .collect::<Result<Vec<_>, _>>()?;
    let parsed = crate::storage::query::modes::parse_multi(&query)
        .map_err(|e| Status::invalid_argument(format!("parse error: {e}")))?;
    let bound = crate::storage::query::user_params::bind(&parsed, &binds)
        .map_err(|e| Status::invalid_argument(format!("bind error: {e}")))?;
    runtime.execute_query_expr(bound).map_err(to_status)
}

#[cfg(test)]
mod grpc_query_value_tests {
    use super::*;
    use proto::query_value::Kind;

    #[test]
    fn grpc_query_value_maps_to_schema_value_variants() {
        let cases = vec![
            (
                QueryValue {
                    kind: Some(Kind::NullValue(proto::QueryNull {})),
                },
                Value::Null,
            ),
            (
                QueryValue {
                    kind: Some(Kind::BoolValue(true)),
                },
                Value::Boolean(true),
            ),
            (
                QueryValue {
                    kind: Some(Kind::IntValue(42)),
                },
                Value::Integer(42),
            ),
            (
                QueryValue {
                    kind: Some(Kind::FloatValue(1.5)),
                },
                Value::Float(1.5),
            ),
            (
                QueryValue {
                    kind: Some(Kind::BytesValue(vec![0, 1, 2])),
                },
                Value::Blob(vec![0, 1, 2]),
            ),
            (
                QueryValue {
                    kind: Some(Kind::VectorValue(proto::QueryVector {
                        values: vec![0.25, 0.5],
                    })),
                },
                Value::Vector(vec![0.25, 0.5]),
            ),
            (
                QueryValue {
                    kind: Some(Kind::TimestampValue(1_779_999_000)),
                },
                Value::Timestamp(1_779_999_000),
            ),
            (
                QueryValue {
                    kind: Some(Kind::UuidValue(vec![0x11; 16])),
                },
                Value::Uuid([0x11; 16]),
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(grpc_query_value_to_schema_value(input).unwrap(), expected);
        }

        assert_eq!(
            grpc_query_value_to_schema_value(QueryValue {
                kind: Some(Kind::TextValue("alice".into())),
            })
            .unwrap(),
            Value::Text(std::sync::Arc::from("alice"))
        );
        assert_eq!(
            grpc_query_value_to_schema_value(QueryValue {
                kind: Some(Kind::JsonValue("{\"role\":\"admin\"}".into())),
            })
            .unwrap(),
            Value::Json(b"{\"role\":\"admin\"}".to_vec())
        );
    }

    #[test]
    fn grpc_query_value_rejects_missing_kind_and_bad_uuid() {
        assert!(grpc_query_value_to_schema_value(QueryValue { kind: None }).is_err());
        assert!(grpc_query_value_to_schema_value(QueryValue {
            kind: Some(Kind::UuidValue(vec![0; 15])),
        })
        .is_err());
    }
}

#[cfg(test)]
mod grpc_ask_query_reply_tests {
    use super::*;
    use crate::storage::query::modes::QueryMode;
    use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
    use crate::storage::schema::Value as SchemaValue;

    fn ask_runtime_result() -> RuntimeQueryResult {
        let mut result = UnifiedResult::with_columns(vec![
            "answer".into(),
            "provider".into(),
            "model".into(),
            "mode".into(),
            "retry_count".into(),
            "prompt_tokens".into(),
            "completion_tokens".into(),
            "sources_flat".into(),
            "citations".into(),
            "validation".into(),
        ]);
        let mut record = UnifiedRecord::new();
        record.set("answer", SchemaValue::text("Deploy failed [^1]."));
        record.set("provider", SchemaValue::text("openai"));
        record.set("model", SchemaValue::text("gpt-4o-mini"));
        record.set("mode", SchemaValue::text("strict"));
        record.set("retry_count", SchemaValue::Integer(0));
        record.set("prompt_tokens", SchemaValue::Integer(11));
        record.set("completion_tokens", SchemaValue::Integer(7));
        record.set(
            "sources_flat",
            SchemaValue::Json(
                br#"[{"urn":"urn:reddb:row:deployments:1","kind":"row","collection":"deployments","id":"1"}]"#.to_vec(),
            ),
        );
        record.set(
            "citations",
            SchemaValue::Json(br#"[{"marker":1,"urn":"urn:reddb:row:deployments:1"}]"#.to_vec()),
        );
        record.set(
            "validation",
            SchemaValue::Json(br#"{"ok":true,"warnings":[],"errors":[]}"#.to_vec()),
        );
        result.push(record);

        RuntimeQueryResult {
            query: "ASK 'why did deploy fail?'".to_string(),
            mode: QueryMode::Sql,
            statement: "ask",
            engine: "runtime-ai",
            result,
            affected_rows: 0,
            statement_type: "select",
        }
    }

    #[test]
    fn query_reply_ask_result_json_uses_full_canonical_schema() {
        let reply = query_reply(ask_runtime_result(), &None, &None);
        let json: crate::json::Value =
            crate::json::from_str(&reply.result_json).expect("valid ask json");

        assert_eq!(
            json.get("answer").and_then(crate::json::Value::as_str),
            Some("Deploy failed [^1].")
        );
        assert_eq!(
            json.get("cache_hit").and_then(crate::json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            json.get("cost_usd").and_then(crate::json::Value::as_f64),
            Some(0.0)
        );
        assert_eq!(
            json.get("mode").and_then(crate::json::Value::as_str),
            Some("strict")
        );
        assert_eq!(
            json.get("retry_count").and_then(crate::json::Value::as_u64),
            Some(0)
        );
        assert!(
            json.get("records").is_none(),
            "ASK must not be row-wrapped: {}",
            reply.result_json
        );
        assert!(
            json.get("sources_flat")
                .and_then(crate::json::Value::as_array)
                .is_some_and(|sources| sources.len() == 1
                    && sources[0]
                        .get("payload")
                        .and_then(crate::json::Value::as_str)
                        .is_some()),
            "sources_flat must be parsed with payload fallback: {}",
            reply.result_json
        );
        assert!(
            json.get("citations")
                .and_then(crate::json::Value::as_array)
                .is_some_and(|citations| citations.len() == 1),
            "citations must be parsed: {}",
            reply.result_json
        );
        assert_eq!(
            json.get("validation")
                .and_then(|v| v.get("ok"))
                .and_then(crate::json::Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn query_reply_non_ask_answer_column_keeps_row_shape() {
        let mut result = UnifiedResult::with_columns(vec!["answer".into()]);
        let mut record = UnifiedRecord::new();
        record.set("answer", SchemaValue::text("plain select"));
        result.push(record);

        let reply = query_reply(
            RuntimeQueryResult {
                query: "SELECT 'plain select' AS answer".to_string(),
                mode: QueryMode::Sql,
                statement: "select",
                engine: "runtime-sql",
                result,
                affected_rows: 0,
                statement_type: "select",
            },
            &None,
            &None,
        );
        let json: crate::json::Value =
            crate::json::from_str(&reply.result_json).expect("valid query json");

        assert!(
            json.get("records").is_some(),
            "non-ASK must stay row-wrapped"
        );
        assert!(
            json.get("answer").is_none(),
            "non-ASK must not use ASK envelope"
        );
    }
}

/// Emit a single info-level event with the SHA-256 fingerprint of the
/// active gRPC server cert + an mTLS flag. Never logs PEM bytes.
fn log_grpc_tls_identity(tls: &GrpcTlsOptions) {
    use sha2::{Digest, Sha256};
    let cert_fp = {
        let mut h = Sha256::new();
        h.update(&tls.cert_pem);
        let digest = h.finalize();
        // First 16 hex chars are enough for human cross-check; the full
        // SHA-256 lives in audit logs only.
        let mut buf = String::with_capacity(64);
        for b in digest.iter() {
            buf.push_str(&format!("{b:02x}"));
        }
        buf
    };
    tracing::info!(
        target: "reddb::security",
        transport = "grpc",
        cert_sha256 = %cert_fp,
        mtls = tls.client_ca_pem.is_some(),
        "gRPC TLS identity loaded"
    );
}

include!("grpc/service_impl.rs");
