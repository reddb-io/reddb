pub(crate) use crate::application::json_input::{
    json_bool_field, json_f32_field, json_string_field, json_usize_field,
};
pub(crate) use crate::application::{
    AdminUseCases, CatalogUseCases, CreateEdgeInput, CreateEntityOutput, CreateNodeGraphLinkInput,
    CreateNodeInput, CreateNodeTableLinkInput, CreateRowInput, CreateVectorInput,
    DeleteEntityInput, EntityUseCases, ExplainQueryInput, GraphCentralityInput,
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
use crate::auth::middleware::{check_permission, AuthResult, AuthSource};
use crate::auth::store::AuthStore;
use crate::auth::Role;
use crate::health::{HealthProvider, HealthState};
use crate::json::{
    from_str as json_from_str, to_string as json_to_string, Map, Value as JsonValue,
};
use crate::runtime::query_request::{
    ParamValue, PreparedId, PreparedRegistry, QueryRequest as RuntimeQueryRequest,
    QueryRequestExecutor,
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
    BatchInsertChunk, BatchInsertReply, BatchQueryReply, BatchQueryRequest, BulkEntityReply,
    Citation, CollectionRequest, CollectionsReply, DeleteEntityRequest, DeploymentProfileRequest,
    Empty, EntityReply, ExecutePreparedRequest, ExportRequest, GraphProjectionUpsertRequest,
    HealthReply, IndexNameRequest, IndexToggleRequest, JsonBulkCreateRequest, JsonCreateRequest,
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
            bind_addr: "127.0.0.1:55055".to_string(),
            tls: None,
        }
    }
}

#[derive(Clone)]
pub struct RedDBGrpcServer {
    runtime: RedDBRuntime,
    options: GrpcServerOptions,
    auth_store: Arc<AuthStore>,
    prepared_registry: Arc<PreparedRegistry>,
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
            let store = AuthStore::with_vault(db_options.auth.clone(), pager)
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
            prepared_registry: Arc::new(PreparedRegistry::new()),
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

    pub(crate) fn with_listener_options(mut self, options: GrpcServerOptions) -> Self {
        self.options = options;
        self
    }

    fn grpc_runtime(&self) -> GrpcRuntime {
        GrpcRuntime {
            runtime: self.runtime.clone(),
            auth_store: self.auth_store.clone(),
            prepared_registry: Arc::clone(&self.prepared_registry),
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

    /// Serve gRPC over a stream of already-accepted connections fed by the
    /// in-process protocol demux (issue #933). The demux classifies each
    /// inbound connection on the shared port and hands the HTTP/2 ones
    /// straight in through `rx` — there is no loopback socket and no
    /// `copy_bidirectional` hop. The server runs until `rx` closes (the
    /// demux acceptor dropped its sender) and all in-flight RPCs drain.
    pub(crate) async fn serve_router_demux(
        &self,
        rx: tokio::sync::mpsc::Receiver<tokio::net::TcpStream>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use tokio_stream::StreamExt;
        let incoming = tokio_stream::wrappers::ReceiverStream::new(rx).map(Ok::<_, std::io::Error>);
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

#[derive(Clone)]
struct GrpcRuntime {
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    prepared_registry: Arc<PreparedRegistry>,
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

fn grpc_query_value_to_param_value(value: QueryValue) -> Result<ParamValue, Status> {
    use proto::query_value::Kind;

    match value
        .kind
        .ok_or_else(|| Status::invalid_argument("missing query param value"))?
    {
        Kind::NullValue(_) => Ok(ParamValue::Null),
        Kind::BoolValue(value) => Ok(ParamValue::Bool(value)),
        Kind::IntValue(value) => Ok(ParamValue::Int64(value)),
        Kind::FloatValue(value) => Ok(ParamValue::Float64(value)),
        Kind::TextValue(value) => Ok(ParamValue::Text(value)),
        Kind::BytesValue(value) => Ok(ParamValue::Bytes(value)),
        Kind::VectorValue(value) => Ok(ParamValue::Vector(value.values)),
        Kind::JsonValue(value) => {
            let parsed = json_from_str::<JsonValue>(&value)
                .map_err(|e| Status::invalid_argument(format!("json param parse error: {e}")))?;
            let encoded = json_to_string(&parsed)
                .map_err(|e| Status::invalid_argument(format!("json param encode error: {e}")))?;
            Ok(ParamValue::Json(encoded.into_bytes()))
        }
        Kind::TimestampValue(value) => Ok(ParamValue::Timestamp(value)),
        Kind::UuidValue(value) => {
            let bytes: [u8; 16] = value.try_into().map_err(|value: Vec<u8>| {
                Status::invalid_argument(format!(
                    "uuid param must be 16 bytes, got {}",
                    value.len()
                ))
            })?;
            Ok(ParamValue::Uuid(bytes))
        }
    }
}

fn execute_grpc_query_request(
    runtime: &RedDBRuntime,
    prepared: &PreparedRegistry,
    query: String,
    params: Vec<QueryValue>,
    commit_policy: Option<crate::replication::CommitPolicy>,
) -> Result<RuntimeQueryResult, Status> {
    if query.trim().is_empty() {
        return Err(Status::invalid_argument("query field cannot be empty"));
    }

    let params = params
        .into_iter()
        .map(grpc_query_value_to_param_value)
        .collect::<Result<Vec<_>, _>>()?;
    let mut request = RuntimeQueryRequest::sql(query, params);
    if let Some(commit_policy) = commit_policy {
        request = request.with_commit_policy(commit_policy);
    }
    QueryRequestExecutor::new(runtime, prepared)
        .execute(request)
        .map_err(grpc_query_request_error)
}

fn grpc_commit_policy_from_metadata(
    metadata: &MetadataMap,
) -> Result<Option<crate::replication::CommitPolicy>, Status> {
    let Some(value) = metadata.get("x-reddb-commit-policy") else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| Status::invalid_argument("x-reddb-commit-policy must be ascii"))?;
    crate::replication::CommitPolicy::parse_strict(value)
        .map(Some)
        .ok_or_else(|| Status::invalid_argument(format!("invalid commit policy '{value}'")))
}

fn grpc_json_bind_to_param_value(value: &str) -> Result<ParamValue, Status> {
    let value = json_from_str::<JsonValue>(value)
        .map_err(|error| Status::invalid_argument(format!("bind parse error: {error}")))?;
    Ok(match value {
        JsonValue::Null => ParamValue::Null,
        JsonValue::Bool(value) => ParamValue::Bool(value),
        JsonValue::Integer(value) => ParamValue::Int64(value),
        JsonValue::Number(value) if value.fract() == 0.0 && value.abs() < i64::MAX as f64 => {
            ParamValue::Int64(value as i64)
        }
        JsonValue::Number(value) => ParamValue::Float64(value),
        JsonValue::String(value) => ParamValue::Text(value),
        other => ParamValue::Text(json_to_string(&other).unwrap_or_default()),
    })
}

fn grpc_query_request_error(error: crate::api::RedDBError) -> Status {
    let message = error.to_string();
    if message.contains("commit policy timed out") {
        return Status::deadline_exceeded(message);
    }
    match error {
        crate::api::RedDBError::Query(message) if message.contains("not found or expired") => {
            Status::not_found(message)
        }
        crate::api::RedDBError::Query(message)
            if message == "prepared_needs_replan" || message == "prepared statements disabled" =>
        {
            Status::failed_precondition(message)
        }
        crate::api::RedDBError::Query(message) => Status::invalid_argument(message),
        crate::api::RedDBError::Validation { message, .. } => Status::invalid_argument(message),
        other => to_status(other),
    }
}

#[cfg(test)]
mod grpc_query_value_tests {
    use super::*;
    use proto::query_value::Kind;

    #[test]
    fn grpc_query_value_maps_to_request_param_variants() {
        let cases = vec![
            (
                QueryValue {
                    kind: Some(Kind::NullValue(proto::QueryNull {})),
                },
                ParamValue::Null,
            ),
            (
                QueryValue {
                    kind: Some(Kind::BoolValue(true)),
                },
                ParamValue::Bool(true),
            ),
            (
                QueryValue {
                    kind: Some(Kind::IntValue(42)),
                },
                ParamValue::Int64(42),
            ),
            (
                QueryValue {
                    kind: Some(Kind::FloatValue(1.5)),
                },
                ParamValue::Float64(1.5),
            ),
            (
                QueryValue {
                    kind: Some(Kind::BytesValue(vec![0, 1, 2])),
                },
                ParamValue::Bytes(vec![0, 1, 2]),
            ),
            (
                QueryValue {
                    kind: Some(Kind::VectorValue(proto::QueryVector {
                        values: vec![0.25, 0.5],
                    })),
                },
                ParamValue::Vector(vec![0.25, 0.5]),
            ),
            (
                QueryValue {
                    kind: Some(Kind::TimestampValue(1_779_999_000)),
                },
                ParamValue::Timestamp(1_779_999_000),
            ),
            (
                QueryValue {
                    kind: Some(Kind::UuidValue(vec![0x11; 16])),
                },
                ParamValue::Uuid([0x11; 16]),
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(grpc_query_value_to_param_value(input).unwrap(), expected);
        }

        assert_eq!(
            grpc_query_value_to_param_value(QueryValue {
                kind: Some(Kind::TextValue("alice".into())),
            })
            .unwrap(),
            ParamValue::Text("alice".to_string())
        );
        assert_eq!(
            grpc_query_value_to_param_value(QueryValue {
                kind: Some(Kind::JsonValue("{\"role\":\"admin\"}".into())),
            })
            .unwrap(),
            ParamValue::Json(b"{\"role\":\"admin\"}".to_vec())
        );
    }

    #[test]
    fn grpc_query_value_rejects_missing_kind_and_bad_uuid() {
        assert!(grpc_query_value_to_param_value(QueryValue { kind: None }).is_err());
        assert!(grpc_query_value_to_param_value(QueryValue {
            kind: Some(Kind::UuidValue(vec![0; 15])),
        })
        .is_err());
    }

    #[test]
    fn grpc_query_rejects_empty_query_before_runtime_parse() {
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        let prepared = PreparedRegistry::new();
        let err =
            execute_grpc_query_request(&runtime, &prepared, "  ".to_string(), Vec::new(), None)
                .expect_err("empty query should fail");

        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert_eq!(err.message(), "query field cannot be empty");
    }

    #[test]
    fn grpc_query_params_are_bound_before_execution() {
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        seed_grpc_param_table(&runtime);
        let prepared = PreparedRegistry::new();

        let result = execute_grpc_query_request(
            &runtime,
            &prepared,
            "SELECT id, name FROM p WHERE id = $1 AND name = $2".to_string(),
            grpc_param_values(),
            None,
        )
        .expect("parameterized query");

        assert_eq!(result.result.records.len(), 1);
    }

    #[test]
    fn grpc_query_enforces_ack_n_commit_policy_fail_closed() {
        let _env_lock = env_lock().lock().expect("env lock");
        let _env = EnvGuard::set(&[
            ("RED_PRIMARY_COMMIT_POLICY", "ack_n=1"),
            ("RED_REPLICATION_ACK_TIMEOUT_MS", "20"),
            ("RED_COMMIT_FAIL_ON_TIMEOUT", "true"),
        ]);
        let data_path = temp_data_path("grpc_ack_n_timeout");
        cleanup(&data_path);

        let runtime = RedDBRuntime::with_options(
            crate::api::RedDBOptions::persistent(&data_path)
                .with_replication(crate::replication::ReplicationConfig::primary()),
        )
        .expect("runtime");
        let prepared = PreparedRegistry::new();

        let err = execute_grpc_query_request(
            &runtime,
            &prepared,
            "INSERT INTO grpc_ack_items (id, name) VALUES (1, 'alpha')".to_string(),
            Vec::new(),
            None,
        )
        .expect_err("ack_n without replica ack must fail closed");

        assert_eq!(err.code(), tonic::Code::DeadlineExceeded);
        assert!(
            err.message().contains("commit policy timed out")
                && err.message().contains("RED_COMMIT_FAIL_ON_TIMEOUT"),
            "error should identify commit policy timeout, got {err:?}"
        );
        assert!(
            runtime.cdc_current_lsn() > 0,
            "local mutation should advance CDC before gRPC response fails"
        );

        cleanup(&data_path);
    }

    #[tokio::test]
    async fn grpc_query_rpc_binds_query_request_params() {
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        seed_grpc_param_table(&runtime);
        let service = GrpcRuntime {
            runtime,
            auth_store: Arc::new(AuthStore::new(crate::auth::AuthConfig::default())),
            prepared_registry: Arc::new(PreparedRegistry::new()),
            oauth_validator: None,
        };

        let reply = RedDb::query(
            &service,
            Request::new(QueryRequest {
                query: "SELECT id, name FROM p WHERE id = $1 AND name = $2".to_string(),
                entity_types: Vec::new(),
                capabilities: Vec::new(),
                params: grpc_param_values(),
            }),
        )
        .await
        .expect("query rpc")
        .into_inner();

        assert_eq!(reply.record_count, 1);
        assert!(reply.result_json.contains("Alice"), "{}", reply.result_json);
        assert!(!reply.result_json.contains("Bob"), "{}", reply.result_json);
    }

    #[tokio::test]
    async fn grpc_prepared_id_is_shared_by_connection_listeners_only() {
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        let server = RedDBGrpcServer::new(runtime.clone());
        let plain_listener = server.grpc_runtime();
        let tls_listener = server.grpc_runtime();
        let second_connection = RedDBGrpcServer::new(runtime).grpc_runtime();

        let prepared = RedDb::prepare_query(
            &plain_listener,
            Request::new(PrepareQueryRequest {
                query: "SELECT 7 AS value".to_string(),
            }),
        )
        .await
        .expect("prepare on plaintext listener")
        .into_inner();

        let reply = RedDb::execute_prepared(
            &tls_listener,
            Request::new(ExecutePreparedRequest {
                prepared_id: prepared.prepared_id,
                bind_json: vec!["42".to_string()],
            }),
        )
        .await
        .expect("execute on TLS listener of the same connection")
        .into_inner();
        assert!(reply.result_json.contains("42"), "{}", reply.result_json);

        let error = RedDb::execute_prepared(
            &second_connection,
            Request::new(ExecutePreparedRequest {
                prepared_id: prepared.prepared_id,
                bind_json: vec!["42".to_string()],
            }),
        )
        .await
        .expect_err("another connection must not resolve the prepared ID");
        assert_eq!(error.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn grpc_execute_prepared_rejects_a_stale_ddl_epoch() {
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        let service = RedDBGrpcServer::new(runtime.clone()).grpc_runtime();
        let prepared = RedDb::prepare_query(
            &service,
            Request::new(PrepareQueryRequest {
                query: "SELECT 7 AS value".to_string(),
            }),
        )
        .await
        .expect("prepare query")
        .into_inner();

        runtime
            .execute_query("CREATE TABLE grpc_prepared_epoch (id INTEGER)")
            .expect("execute DDL");

        let error = RedDb::execute_prepared(
            &service,
            Request::new(ExecutePreparedRequest {
                prepared_id: prepared.prepared_id,
                bind_json: vec!["42".to_string()],
            }),
        )
        .await
        .expect_err("DDL must invalidate the gRPC prepared shape");
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(error.message(), "prepared_needs_replan");
    }

    #[tokio::test]
    async fn grpc_prepared_kill_switch_rejects_prepare_and_execute() {
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        let service = RedDBGrpcServer::new(runtime).grpc_runtime();
        let prepared = RedDb::prepare_query(
            &service,
            Request::new(PrepareQueryRequest {
                query: "SELECT 7 AS value".to_string(),
            }),
        )
        .await
        .expect("prepare before kill switch")
        .into_inner();

        service.prepared_registry.disable();

        let prepare_error = RedDb::prepare_query(
            &service,
            Request::new(PrepareQueryRequest {
                query: "SELECT 1".to_string(),
            }),
        )
        .await
        .expect_err("kill switch must reject prepare");
        assert_eq!(prepare_error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(prepare_error.message(), "prepared statements disabled");

        let execute_error = RedDb::execute_prepared(
            &service,
            Request::new(ExecutePreparedRequest {
                prepared_id: prepared.prepared_id,
                bind_json: vec!["42".to_string()],
            }),
        )
        .await
        .expect_err("kill switch must reject execute");
        assert_eq!(execute_error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(execute_error.message(), "prepared statements disabled");
    }

    #[tokio::test]
    async fn grpc_query_honors_per_request_commit_policy() {
        let _env_lock = env_lock().lock().expect("env lock");
        let _env = EnvGuard::set(&[("RED_PRIMARY_COMMIT_POLICY", "ack_n=1")]);
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        runtime
            .execute_query("CREATE TABLE grpc_request_policy (id INTEGER)")
            .expect("create table");
        let service = RedDBGrpcServer::new(runtime.clone()).grpc_runtime();
        let mut request = Request::new(QueryRequest {
            query: "INSERT INTO grpc_request_policy (id) VALUES (1)".to_string(),
            entity_types: Vec::new(),
            capabilities: Vec::new(),
            params: Vec::new(),
        });
        request
            .metadata_mut()
            .insert("x-reddb-commit-policy", "local".parse().expect("metadata"));

        let error = RedDb::query(&service, request)
            .await
            .expect_err("weaker per-request policy must fail before mutation");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("weaker than resolved floor"));

        let rows = runtime
            .execute_query("SELECT id FROM grpc_request_policy")
            .expect("read rows");
        assert!(rows.result.records.is_empty());
    }

    #[tokio::test]
    async fn pull_wal_records_rejects_stale_term_on_grpc_path() {
        let runtime = RedDBRuntime::with_options(
            crate::api::RedDBOptions::in_memory()
                .with_replication(crate::replication::ReplicationConfig::primary().with_term(6)),
        )
        .expect("runtime");
        let auth_store = Arc::new(AuthStore::new(crate::auth::AuthConfig {
            enabled: true,
            require_auth: true,
            ..crate::auth::AuthConfig::default()
        }));
        let bootstrap = auth_store
            .bootstrap("replica", "secret")
            .expect("bootstrap");
        let policy = crate::auth::policies::Policy::from_json_str(
            r#"{
                "id": "replication-stream",
                "version": 1,
                "statements": [{
                    "effect": "allow",
                    "actions": ["cluster:replication:stream"],
                    "resources": ["cluster:replication"]
                }]
            }"#,
        )
        .expect("policy");
        auth_store.put_policy(policy).expect("install policy");
        auth_store
            .attach_policy(
                crate::auth::store::PrincipalRef::User(crate::auth::UserId::platform("replica")),
                "replication-stream",
            )
            .expect("attach policy");
        let service = GrpcRuntime {
            runtime,
            auth_store,
            prepared_registry: Arc::new(PreparedRegistry::new()),
            oauth_validator: None,
        };

        let open = reddb_wire::replication::WalStreamOpen {
            since_lsn: 0,
            max_count: 1,
            replica_id: Some("replica-a".to_string()),
            term: 5,
            await_data: false,
            await_timeout_ms: 1,
        };
        let mut request = Request::new(JsonPayloadRequest {
            payload_json: String::from_utf8(open.encode_json()).expect("json"),
        });
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", bootstrap.api_key.key)
                .parse()
                .expect("metadata"),
        );

        let err = RedDb::pull_wal_records(&service, request)
            .await
            .expect_err("stale term should be fenced");

        assert_eq!(err.code(), tonic::Code::FailedPrecondition);
        assert!(
            err.message().contains("stale")
                || err.message().contains("fenced")
                || err.message().contains("current term"),
            "unexpected stale-term error: {err:?}"
        );
    }

    fn seed_grpc_param_table(runtime: &RedDBRuntime) {
        runtime
            .execute_query("CREATE TABLE p (id INTEGER, name TEXT)")
            .expect("create table");
        runtime
            .execute_query("INSERT INTO p (id, name) VALUES (1, 'Alice')")
            .expect("insert alice");
        runtime
            .execute_query("INSERT INTO p (id, name) VALUES (2, 'Bob')")
            .expect("insert bob");
    }

    fn grpc_param_values() -> Vec<QueryValue> {
        vec![
            QueryValue {
                kind: Some(Kind::IntValue(1)),
            },
            QueryValue {
                kind: Some(Kind::TextValue("Alice".to_string())),
            },
        ]
    }

    fn env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, &'static str)]) -> Self {
            let previous = vars
                .iter()
                .map(|(key, _)| (*key, std::env::var(key).ok()))
                .collect();
            for (key, value) in vars {
                std::env::set_var(key, value);
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.iter().rev() {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn temp_data_path(name: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("reddb_{name}_{suffix}.rdb"))
    }

    fn cleanup(data_path: &std::path::Path) {
        let _ = std::fs::remove_file(data_path);
        let _ = std::fs::remove_file(
            crate::replication::primary::PrimaryReplication::slot_path_for(data_path),
        );
        let _ = std::fs::remove_file(crate::replication::primary::LogicalWalSpool::path_for(
            data_path,
        ));
        let _ = std::fs::remove_dir_all(
            crate::replication::primary::PrimaryReplication::primary_replica_root_for(data_path),
        );
        reddb_file::cleanup_rebootstrap_artifacts(data_path);
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
            bookmark: None,
            notice: None,
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
                bookmark: None,
                notice: None,
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

    #[test]
    fn query_reply_non_ask_json_column_preserves_object() {
        let mut result = UnifiedResult::with_columns(vec!["value".into()]);
        let mut record = UnifiedRecord::new();
        record.set(
            "value",
            SchemaValue::Json(br#"{"alpha":"A","nested":{"leaf":12}}"#.to_vec()),
        );
        result.push(record);

        let reply = query_reply(
            RuntimeQueryResult {
                query: "LIST KV proj AS JSON".to_string(),
                mode: QueryMode::Sql,
                statement: "kv_list_json",
                engine: "kv",
                result,
                affected_rows: 0,
                statement_type: "select",
                bookmark: None,
                notice: None,
            },
            &None,
            &None,
        );
        let json: crate::json::Value =
            crate::json::from_str(&reply.result_json).expect("valid query json");
        let value = json
            .get("records")
            .and_then(crate::json::Value::as_array)
            .and_then(|records| records.first())
            .and_then(|record| record.get("value"))
            .expect("value column");

        assert_eq!(
            value.get("alpha").and_then(crate::json::Value::as_str),
            Some("A")
        );
        assert_eq!(
            value
                .get("nested")
                .and_then(|nested| nested.get("leaf"))
                .and_then(crate::json::Value::as_f64),
            Some(12.0)
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
