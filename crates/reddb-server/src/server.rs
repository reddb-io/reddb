//! Minimal HTTP server for RedDB management and remote access.

pub(crate) use crate::application::json_input::{
    json_bool_field, json_f32_field, json_string_field, json_usize_field,
};
pub(crate) use crate::application::{
    AdminUseCases, CatalogUseCases, CreateDocumentInput, CreateEdgeInput, CreateEntityOutput,
    CreateKvInput, CreateNodeEmbeddingInput, CreateNodeGraphLinkInput, CreateNodeInput,
    CreateNodeTableLinkInput, CreateRowInput, CreateVectorInput, DeleteEntityInput, EntityUseCases,
    ExecuteQueryInput, ExplainQueryInput, GraphCentralityInput, GraphClusteringInput,
    GraphCommunitiesInput, GraphComponentsInput, GraphCyclesInput, GraphHitsInput,
    GraphNeighborhoodInput, GraphPersonalizedPageRankInput, GraphShortestPathInput,
    GraphTopologicalSortInput, GraphTraversalInput, GraphUseCases, InspectNativeArtifactInput,
    NativeUseCases, PatchEntityInput, PatchEntityOperation, PatchEntityOperationType,
    QueryUseCases, SearchHybridInput, SearchIvfInput, SearchMultimodalInput, SearchSimilarInput,
    SearchTextInput, TreeUseCases,
};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use std::sync::Arc;

use crate::api::{RedDBError, RedDBOptions, RedDBResult};
use crate::auth::store::AuthStore;
use crate::catalog::{CatalogModelSnapshot, CollectionDescriptor, CollectionModel, SchemaMode};
use crate::health::{HealthProvider, HealthReport, HealthState};
use crate::json::{parse_json, to_vec as json_to_vec, Map, Value as JsonValue};
use crate::runtime::{
    RedDBRuntime, RuntimeFilter, RuntimeFilterValue, RuntimeGraphCentralityAlgorithm,
    RuntimeGraphCentralityResult, RuntimeGraphClusteringResult, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphCommunityResult, RuntimeGraphComponentsMode, RuntimeGraphComponentsResult,
    RuntimeGraphCyclesResult, RuntimeGraphDirection, RuntimeGraphHitsResult,
    RuntimeGraphNeighborhoodResult, RuntimeGraphPathAlgorithm, RuntimeGraphPathResult,
    RuntimeGraphPattern, RuntimeGraphProjection, RuntimeGraphTopologicalSortResult,
    RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy, RuntimeIvfSearchResult,
    RuntimeQueryWeights, RuntimeStats, ScanCursor, ScanPage,
};
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, TableRef, VectorRef};
use crate::storage::unified::dsl::{MatchComponents, QueryResult as DslQueryResult};
use crate::storage::unified::{MetadataValue, RefTarget, SparseVector};
use crate::storage::{CrossRef, EntityData, EntityId, EntityKind, SimilarResult, UnifiedEntity};

fn analytics_job_json(job: &crate::PhysicalAnalyticsJob) -> JsonValue {
    crate::presentation::admin_json::analytics_job_json(job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::health::HealthReport;
    use crate::service_cli::{
        TransportListenerFailure, TransportListenerState, TransportReadiness,
    };

    #[test]
    fn server_options_default_http_body_limit_is_32_mib() {
        assert_eq!(ServerOptions::default().max_body_bytes, 32 * 1024 * 1024);
    }

    #[test]
    fn health_json_reports_transport_listeners() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let mut options = ServerOptions::default();
        options.transport_readiness = TransportReadiness {
            active: vec![TransportListenerState {
                transport: "grpc".to_string(),
                bind_addr: "127.0.0.1:50051".to_string(),
                explicit: true,
            }],
            failed: vec![TransportListenerFailure {
                transport: "http".to_string(),
                bind_addr: "127.0.0.1:5055".to_string(),
                explicit: false,
                reason: "http listener bind 127.0.0.1:5055: address in use".to_string(),
            }],
        };
        let server = RedDBServer::with_options(runtime, options);

        let payload = server.health_json_with_transport(&HealthReport::healthy());
        let JsonValue::Object(root) = payload else {
            panic!("health payload should be an object");
        };
        let Some(JsonValue::Object(listeners)) = root.get("transport_listeners") else {
            panic!("health payload should include transport_listeners");
        };
        let Some(JsonValue::Array(active)) = listeners.get("active") else {
            panic!("transport_listeners.active should be an array");
        };
        let Some(JsonValue::Array(failed)) = listeners.get("failed") else {
            panic!("transport_listeners.failed should be an array");
        };

        assert_eq!(active.len(), 1);
        assert_eq!(failed.len(), 1);
    }
}

fn graph_projection_json(projection: &crate::PhysicalGraphProjection) -> JsonValue {
    crate::presentation::admin_json::graph_projection_json(projection)
}

mod axum_edge;
mod ws_edge;
pub mod handlers_admin;
mod handlers_ai;
mod handlers_ai_model_cache;
mod handlers_auth;
mod handlers_backup;
mod handlers_collection_policy;
mod handlers_ec;
pub(crate) mod handlers_entity;
mod handlers_geo;
mod handlers_graph;
mod handlers_keyed;
mod handlers_log;
mod handlers_metrics;
mod handlers_ops;
mod handlers_ops_policy;
// `pub(crate)` so the RedWire input-stream path (issue #764 / S5)
// can reuse the canonical S4 INSERT builders / identifier checks
// (`build_insert_sql`, `is_safe_sql_identifier`) rather than fork
// the SQL-escaping logic.
pub(crate) mod handlers_query;
mod handlers_replication;
mod handlers_topology;
mod handlers_vcs;
mod handlers_vector;
pub mod header_escape_guard;
pub mod http_connection_limiter;
pub mod http_handler_metrics;
pub mod http_limits;
pub mod ingest_pipeline;
pub mod output_stream;
mod patch_support;
mod request_body;
mod request_context;
mod routing;
mod serverless_support;
pub mod tls;
mod transport;

use self::handlers_ai::*;
use self::handlers_entity::*;
use self::handlers_graph::*;
use self::handlers_keyed::*;
use self::handlers_metrics::*;
use self::handlers_ops::*;
use self::handlers_query::*;
use self::http_connection_limiter::{
    HandlerDeadline, HttpConnectionLimiter, MonotonicClock, SystemMonotonicClock,
};
use self::http_handler_metrics::{HttpHandlerMetrics, HttpRejectReason, HttpTransport};
pub use self::http_limits::{
    HttpLimitsCliInput, HttpLimitsResolved, DEFAULT_HANDLER_TIMEOUT_MS, DEFAULT_RETRY_AFTER_SECS,
};
use self::patch_support::*;
use self::request_body::*;
use self::routing::*;
use self::serverless_support::*;
use self::transport::*;

/// PLAN.md Phase 6.2 — endpoint segregation. A given HTTP listener
/// can serve either every public surface (`Public`, default) or a
/// restricted slice (`AdminOnly`, `MetricsOnly`). The route filter at
/// the top of `route()` consults this so a port bound only to
/// loopback for admin work won't accidentally hand out DML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSurface {
    /// Everything routed normally (default — matches v0 behaviour).
    Public,
    /// Only `/admin/*`, `/metrics`, and `/health/*`. Other paths
    /// return 404. Intended for `RED_ADMIN_BIND` operator listeners
    /// which default to `127.0.0.1`.
    AdminOnly,
    /// Only `/metrics` and `/health/*`. Intended for
    /// `RED_METRICS_BIND` Prometheus scrape ports that may be
    /// exposed to non-admin networks.
    MetricsOnly,
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub bind_addr: String,
    pub max_body_bytes: usize,
    pub read_timeout_ms: u64,
    pub write_timeout_ms: u64,
    pub max_scan_limit: usize,
    /// Which subset of paths this listener serves. Defaults to
    /// `Public`. Set to `AdminOnly` / `MetricsOnly` for dedicated
    /// admin / scrape ports (PLAN.md Phase 6.2).
    pub surface: ServerSurface,
    pub transport_readiness: crate::service_cli::TransportReadiness,
    /// Allowed `Origin` values for the RedWire-over-WSS browser endpoint
    /// (issue #935, ADR 0036). WebSocket is not covered by CORS, so the
    /// upgrade is gated on an explicit allowlist to block Cross-Site
    /// WebSocket Hijacking. **Default-deny:** empty means the `/redwire`
    /// route is not mounted at all — operators opt in by configuring at
    /// least one origin. Matched exactly (scheme+host+port), e.g.
    /// `https://app.example.com`.
    pub websocket_allowed_origins: Vec<String>,
}

pub const DEFAULT_HTTP_MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:5055".to_string(),
            max_body_bytes: DEFAULT_HTTP_MAX_BODY_BYTES,
            read_timeout_ms: 5_000,
            write_timeout_ms: 5_000,
            max_scan_limit: 1_000,
            surface: ServerSurface::Public,
            transport_readiness: crate::service_cli::TransportReadiness::default(),
            websocket_allowed_origins: Vec::new(),
        }
    }
}

/// Replication state exposed to the HTTP server.
pub struct ServerReplicationState {
    pub config: crate::replication::ReplicationConfig,
    pub primary: Option<crate::replication::primary::PrimaryReplication>,
}

#[derive(Clone)]
pub struct RedDBServer {
    runtime: RedDBRuntime,
    options: ServerOptions,
    auth_store: Option<Arc<AuthStore>>,
    replication: Option<Arc<ServerReplicationState>>,
    /// Bounded handler-thread admission for the clear-text HTTP accept
    /// loop (issue #570 slice 1). Cloned with the server; `Clone` of
    /// `HttpConnectionLimiter` shares an `Arc` so every serve loop on
    /// the same `RedDBServer` shares one cap.
    http_limiter: HttpConnectionLimiter,
    /// Per-handler total-time deadline (issue #570 slice 2). Each
    /// clear-text handler thread arms a deadline at spawn and bails
    /// with a best-effort 503 at coarse boundaries between request
    /// parse, route dispatch, and response write. Hard-coded to 30s
    /// here; the config knob lands in slice 5.
    handler_timeout: Duration,
    /// Monotonic clock the per-handler deadline (issue #621) is armed
    /// against — the same [`MonotonicClock`] abstraction the limiter
    /// uses. Production wires [`SystemMonotonicClock`] (real wall time);
    /// tests inject a fake to drive timeout expiry deterministically
    /// without `sleep()`. Shared via `Arc` so cloned server handles
    /// (e.g. `serve_in_background`) read the same clock.
    handler_clock: Arc<dyn MonotonicClock>,
    /// Test-only synchronous sleep injected between route dispatch and
    /// response write so an integration test can simulate a slow
    /// downstream tripping the deadline. Default 0 (no-op). Shared via
    /// `Arc` so a cloned `RedDBServer` (e.g. `serve_in_background`)
    /// observes flips from the originating handle. Production callers
    /// have no way to set this — the setter is `#[doc(hidden)]`.
    slow_inject_ms: Arc<AtomicU64>,
    /// Prometheus metrics for the HTTP handler-thread pool (issue
    /// #573 slice 4). Records rejections (cap_exhausted /
    /// handler_timeout) and per-handler duration histograms. Cloned
    /// with the server via `Arc` so every serve loop on the same
    /// `RedDBServer` writes to one set of counters.
    http_metrics: HttpHandlerMetrics,
    /// `Retry-After` value (seconds) emitted in the async edge's
    /// capacity-reject 503 path (issue #574 slice 5). Read on the reject
    /// path in `axum_edge`.
    retry_after_secs: u64,
    /// Issue #761 / S2 — process-wide output-stream capacity registry.
    /// Shared via `Arc` so cloned server handles (e.g.
    /// `serve_in_background`) all enforce against one set of counters.
    /// The HTTP NDJSON path acquires through this in
    /// `try_route_streaming` before invoking the handler; the guard is
    /// dropped on return so any stream-end path (success / mid-stream
    /// error / snapshot expiry / panic unwind) releases the slot.
    pub(crate) stream_capacity: Arc<output_stream::StreamCapacityRegistry>,
    /// Issue #766 / S7 — resume coordinator ledger. Tracks
    /// `(snapshot_lsn → opened_at_ms, ttl_ms)` for resume-eligibility
    /// checks. Shared via `Arc` so cloned server handles see one
    /// ledger.
    pub(crate) lease_registry: Arc<output_stream::LeaseRegistry>,
    /// Issue #807 / PRD #750 — `/query/stream` cursor registry. Holds the
    /// opaque token → (snapshot pin, TTL, tenant, principal, query) entries
    /// that let a client resume or reference a streamed read. Shared via
    /// `Arc` so cloned server handles see one registry.
    pub(crate) cursor_registry: Arc<output_stream::CursorRegistry>,
}

/// Default per-handler total-time budget (issue #571 slice 2).
const DEFAULT_HANDLER_TIMEOUT: Duration = Duration::from_millis(30_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerlessWarmupScope {
    Indexes,
    GraphProjections,
    AnalyticsJobs,
    NativeArtifacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeploymentProfile {
    Embedded,
    Server,
    Serverless,
}

fn percent_decode_path_segment(input: &str) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return Err("truncated percent escape".to_string());
                }
                let high = hex_value(bytes[index + 1])
                    .ok_or_else(|| "invalid percent escape".to_string())?;
                let low = hex_value(bytes[index + 2])
                    .ok_or_else(|| "invalid percent escape".to_string())?;
                out.push((high << 4) | low);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| "path segment is not valid UTF-8".to_string())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct ParsedQueryRequest {
    query: String,
    entity_types: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
    /// Optional positional `$N` bind parameters (#358). When `Some`, the
    /// query handler runs the user_params binder before executing.
    /// Absence preserves the legacy `query`-only behavior.
    params: Option<Vec<Value>>,
}

#[derive(Debug, Clone, Copy)]
enum PatchOperationType {
    Set,
    Replace,
    Unset,
}

#[derive(Debug, Clone)]
struct PatchOperation {
    op: PatchOperationType,
    path: Vec<String>,
    value: Option<JsonValue>,
}

impl RedDBServer {
    pub fn new(runtime: RedDBRuntime) -> Self {
        Self::with_options(runtime, ServerOptions::default())
    }

    pub fn from_database_options(
        db_options: RedDBOptions,
        server_options: ServerOptions,
    ) -> RedDBResult<Self> {
        let runtime = RedDBRuntime::with_options(db_options)?;
        Ok(Self::with_options(runtime, server_options))
    }

    pub fn with_options(runtime: RedDBRuntime, options: ServerOptions) -> Self {
        Self {
            runtime,
            options,
            auth_store: None,
            replication: None,
            http_limiter: HttpConnectionLimiter::with_default_cap(),
            handler_timeout: DEFAULT_HANDLER_TIMEOUT,
            handler_clock: Arc::new(SystemMonotonicClock::new()),
            slow_inject_ms: Arc::new(AtomicU64::new(0)),
            http_metrics: HttpHandlerMetrics::new(),
            retry_after_secs: DEFAULT_RETRY_AFTER_SECS,
            stream_capacity: output_stream::StreamCapacityRegistry::new(),
            lease_registry: output_stream::LeaseRegistry::new(),
            cursor_registry: output_stream::CursorRegistry::new(),
        }
    }

    #[doc(hidden)]
    pub fn stream_capacity(&self) -> &Arc<output_stream::StreamCapacityRegistry> {
        &self.stream_capacity
    }

    #[doc(hidden)]
    pub fn lease_registry(&self) -> &Arc<output_stream::LeaseRegistry> {
        &self.lease_registry
    }

    #[doc(hidden)]
    pub fn cursor_registry(&self) -> &Arc<output_stream::CursorRegistry> {
        &self.cursor_registry
    }

    #[doc(hidden)]
    pub fn http_metrics(&self) -> &HttpHandlerMetrics {
        &self.http_metrics
    }

    /// Visible for tests. Lets the integration test in
    /// `tests/http_connection_limiter.rs` saturate the cap and observe
    /// `503 Service Unavailable` responses without spinning up
    /// thousands of sockets.
    #[doc(hidden)]
    pub fn with_http_limiter_cap(mut self, cap: usize) -> Self {
        self.http_limiter = HttpConnectionLimiter::new(cap);
        self
    }

    /// Stamp resolved HTTP limits onto the server (issue #574 slice 5).
    /// Replaces the limiter cap, the per-handler deadline, and the
    /// `Retry-After` value used by the limiter's reject path. All
    /// values are assumed validated by [`http_limits::resolve_http_limits`].
    pub fn with_http_limits(mut self, limits: HttpLimitsResolved) -> Self {
        self.http_limiter = HttpConnectionLimiter::new(limits.max_handlers);
        self.handler_timeout = Duration::from_millis(limits.handler_timeout_ms);
        self.retry_after_secs = limits.retry_after_secs;
        self
    }

    #[doc(hidden)]
    pub fn retry_after_secs(&self) -> u64 {
        self.retry_after_secs
    }

    #[doc(hidden)]
    pub fn http_limiter(&self) -> &HttpConnectionLimiter {
        &self.http_limiter
    }

    /// Visible for tests. Override the per-handler total-time deadline
    /// (issue #570 slice 2). Default 30s.
    #[doc(hidden)]
    pub fn with_handler_timeout(mut self, timeout: Duration) -> Self {
        self.handler_timeout = timeout;
        self
    }

    #[doc(hidden)]
    pub fn handler_timeout(&self) -> Duration {
        self.handler_timeout
    }

    /// Visible for tests. Override the clock the per-handler deadline
    /// (issue #621) is armed against, so timeout expiry can be driven
    /// deterministically without real sleeps. Default is the real
    /// monotonic clock.
    #[doc(hidden)]
    pub fn with_handler_clock(mut self, clock: Arc<dyn MonotonicClock>) -> Self {
        self.handler_clock = clock;
        self
    }

    /// Test hook: set a synchronous sleep (in ms) inserted between
    /// route dispatch and response write. The integration test for
    /// slice 2 sets a value greater than `handler_timeout` to trip
    /// the deadline, then resets to 0 to verify recovery. Shared via
    /// `Arc<AtomicU64>` so cloned server handles see the same flip.
    #[doc(hidden)]
    pub fn set_test_slow_inject_ms(&self, ms: u64) {
        self.slow_inject_ms.store(ms, Ordering::Relaxed);
    }

    /// Attach an `AuthStore` for HTTP-layer authentication.
    /// Also injects the store into the runtime so that `Value::Secret`
    /// auto-encrypt/decrypt can reach the vault AES key.
    pub fn with_auth(mut self, auth_store: Arc<AuthStore>) -> Self {
        self.runtime.set_auth_store(Arc::clone(&auth_store));
        self.auth_store = Some(auth_store);
        self
    }

    /// Attach replication state for status and snapshot endpoints.
    pub fn with_replication(mut self, state: Arc<ServerReplicationState>) -> Self {
        self.replication = Some(state);
        self
    }

    /// Set the `Origin` allowlist that enables the RedWire-over-WSS
    /// browser endpoint (issue #935, ADR 0036). A non-empty list mounts
    /// the `/redwire` upgrade route on the TLS edge; an empty list leaves
    /// it unmounted (default-deny).
    pub fn with_websocket_allowed_origins(mut self, origins: Vec<String>) -> Self {
        self.options.websocket_allowed_origins = origins;
        self
    }

    /// The configured RedWire-over-WSS `Origin` allowlist (issue #935).
    pub(crate) fn websocket_allowed_origins(&self) -> &[String] {
        &self.options.websocket_allowed_origins
    }

    pub fn runtime(&self) -> &RedDBRuntime {
        &self.runtime
    }

    pub fn options(&self) -> &ServerOptions {
        &self.options
    }

    fn query_use_cases(&self) -> QueryUseCases<'_, RedDBRuntime> {
        QueryUseCases::new(&self.runtime)
    }

    fn admin_use_cases(&self) -> AdminUseCases<'_, RedDBRuntime> {
        AdminUseCases::new(&self.runtime)
    }

    fn entity_use_cases(&self) -> EntityUseCases<'_, RedDBRuntime> {
        EntityUseCases::new(&self.runtime)
    }

    fn catalog_use_cases(&self) -> CatalogUseCases<'_, RedDBRuntime> {
        CatalogUseCases::new(&self.runtime)
    }

    fn graph_use_cases(&self) -> GraphUseCases<'_, RedDBRuntime> {
        GraphUseCases::new(&self.runtime)
    }

    fn native_use_cases(&self) -> NativeUseCases<'_, RedDBRuntime> {
        NativeUseCases::new(&self.runtime)
    }

    fn tree_use_cases(&self) -> TreeUseCases<'_, RedDBRuntime> {
        TreeUseCases::new(&self.runtime)
    }

    fn transport_readiness_json(&self) -> JsonValue {
        let active = self
            .options
            .transport_readiness
            .active
            .iter()
            .map(|listener| {
                let mut object = Map::new();
                object.insert(
                    "transport".to_string(),
                    JsonValue::String(listener.transport.clone()),
                );
                object.insert(
                    "bind_addr".to_string(),
                    JsonValue::String(listener.bind_addr.clone()),
                );
                object.insert("explicit".to_string(), JsonValue::Bool(listener.explicit));
                JsonValue::Object(object)
            })
            .collect();
        let failed = self
            .options
            .transport_readiness
            .failed
            .iter()
            .map(|listener| {
                let mut object = Map::new();
                object.insert(
                    "transport".to_string(),
                    JsonValue::String(listener.transport.clone()),
                );
                object.insert(
                    "bind_addr".to_string(),
                    JsonValue::String(listener.bind_addr.clone()),
                );
                object.insert("explicit".to_string(), JsonValue::Bool(listener.explicit));
                object.insert(
                    "reason".to_string(),
                    JsonValue::String(listener.reason.clone()),
                );
                JsonValue::Object(object)
            })
            .collect();

        let mut object = Map::new();
        object.insert("active".to_string(), JsonValue::Array(active));
        object.insert("failed".to_string(), JsonValue::Array(failed));
        JsonValue::Object(object)
    }

    fn handle_grpc_discovery(&self) -> HttpResponse {
        let mut methods = Map::new();
        methods.insert(
            "query".to_string(),
            JsonValue::String("reddb.v1.RedDB/Query".to_string()),
        );
        methods.insert(
            "batch_query".to_string(),
            JsonValue::String("reddb.v1.RedDB/BatchQuery".to_string()),
        );
        methods.insert(
            "health".to_string(),
            JsonValue::String("reddb.v1.RedDB/Health".to_string()),
        );
        methods.insert(
            "prepare".to_string(),
            JsonValue::String("reddb.v1.RedDB/Prepare".to_string()),
        );
        methods.insert(
            "execute_prepared".to_string(),
            JsonValue::String("reddb.v1.RedDB/ExecutePrepared".to_string()),
        );

        let mut examples = Map::new();
        examples.insert(
            "query".to_string(),
            JsonValue::String(
                "grpcurl -plaintext -d '{\"query\":\"SELECT 1\"}' 127.0.0.1:50051 reddb.v1.RedDB/Query"
                    .to_string(),
            ),
        );
        examples.insert(
            "query_with_params".to_string(),
            JsonValue::String(
                "grpcurl -plaintext -d '{\"query\":\"SELECT $1 AS value\",\"params\":[{\"intValue\":42}]}' 127.0.0.1:50051 reddb.v1.RedDB/Query"
                    .to_string(),
            ),
        );
        examples.insert(
            "health".to_string(),
            JsonValue::String(
                "grpcurl -plaintext -d '{}' 127.0.0.1:50051 reddb.v1.RedDB/Health".to_string(),
            ),
        );

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "service".to_string(),
            JsonValue::String("reddb.v1.RedDB".to_string()),
        );
        object.insert(
            "package".to_string(),
            JsonValue::String("reddb.v1".to_string()),
        );
        object.insert(
            "proto".to_string(),
            JsonValue::String("crates/reddb-grpc-proto/proto/reddb.proto".to_string()),
        );
        object.insert("methods".to_string(), JsonValue::Object(methods));
        object.insert("examples".to_string(), JsonValue::Object(examples));
        object.insert(
            "transport_listeners".to_string(),
            self.transport_readiness_json(),
        );
        object.insert(
            "hint".to_string(),
            JsonValue::String(
                "If grpcurl cannot list services, pass the proto file with -import-path crates/reddb-grpc-proto/proto -proto reddb.proto."
                    .to_string(),
            ),
        );
        json_response(200, JsonValue::Object(object))
    }

    fn handle_query_contract(&self) -> HttpResponse {
        let mut examples = Map::new();
        examples.insert(
            "raw_sql".to_string(),
            JsonValue::String("curl -sS http://127.0.0.1:8080/query -d 'SELECT 1'".to_string()),
        );
        examples.insert(
            "json_query".to_string(),
            JsonValue::String(
                "curl -sS http://127.0.0.1:8080/query -H 'content-type: application/json' -d '{\"query\":\"SELECT 1\"}'"
                    .to_string(),
            ),
        );
        examples.insert(
            "json_query_with_params".to_string(),
            JsonValue::String(
                "curl -sS http://127.0.0.1:8080/query -H 'content-type: application/json' -d '{\"query\":\"SELECT $1 AS value\",\"params\":[42]}'"
                    .to_string(),
            ),
        );

        let mut request_body = Map::new();
        request_body.insert(
            "query".to_string(),
            JsonValue::String("required string".to_string()),
        );
        request_body.insert(
            "params".to_string(),
            JsonValue::String("optional array".to_string()),
        );

        let mut response_shape = Map::new();
        response_shape.insert(
            "columns".to_string(),
            JsonValue::String("projected column names".to_string()),
        );
        response_shape.insert(
            "records[].values".to_string(),
            JsonValue::String("only projected values".to_string()),
        );
        response_shape.insert(
            "records[].meta".to_string(),
            JsonValue::String("internal metadata when present".to_string()),
        );

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(false));
        object.insert(
            "code".to_string(),
            JsonValue::String("method_not_allowed".to_string()),
        );
        object.insert(
            "message".to_string(),
            JsonValue::String("/query accepts POST requests".to_string()),
        );
        object.insert(
            "hint".to_string(),
            JsonValue::String(
                "Send raw SQL in the body, or JSON with a string 'query' field.".to_string(),
            ),
        );
        object.insert("method".to_string(), JsonValue::String("POST".to_string()));
        object.insert("path".to_string(), JsonValue::String("/query".to_string()));
        object.insert("request_body".to_string(), JsonValue::Object(request_body));
        object.insert(
            "response_shape".to_string(),
            JsonValue::Object(response_shape),
        );
        object.insert("examples".to_string(), JsonValue::Object(examples));
        object.insert(
            "docs".to_string(),
            JsonValue::String("https://reddb.io/docs/query".to_string()),
        );

        json_response(405, JsonValue::Object(object))
            .with_header("Allow", http::HeaderValue::from_static("POST"))
    }

    fn handle_root_discovery(&self) -> HttpResponse {
        let mut endpoints = Map::new();
        endpoints.insert(
            "health".to_string(),
            JsonValue::String("GET /health".to_string()),
        );
        endpoints.insert(
            "ready".to_string(),
            JsonValue::String("GET /ready".to_string()),
        );
        endpoints.insert(
            "query".to_string(),
            JsonValue::String("POST /query".to_string()),
        );
        endpoints.insert(
            "query_readiness".to_string(),
            JsonValue::String("GET /ready/query".to_string()),
        );
        endpoints.insert(
            "catalog".to_string(),
            JsonValue::String("GET /catalog".to_string()),
        );
        endpoints.insert(
            "deployment_profiles".to_string(),
            JsonValue::String("GET /deployment/profiles".to_string()),
        );

        let mut examples = Map::new();
        examples.insert(
            "http_raw_sql".to_string(),
            JsonValue::String("curl -sS http://127.0.0.1:8080/query -d 'SELECT 1'".to_string()),
        );
        examples.insert(
            "http_json_query".to_string(),
            JsonValue::String(
                "curl -sS http://127.0.0.1:8080/query -H 'content-type: application/json' -d '{\"query\":\"SELECT 1\"}'"
                    .to_string(),
            ),
        );
        examples.insert(
            "http_json_query_with_params".to_string(),
            JsonValue::String(
                "curl -sS http://127.0.0.1:8080/query -H 'content-type: application/json' -d '{\"query\":\"SELECT $1 AS value\",\"params\":[42]}'"
                    .to_string(),
            ),
        );

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "service".to_string(),
            JsonValue::String("reddb".to_string()),
        );
        object.insert(
            "version".to_string(),
            JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
        );
        object.insert("endpoints".to_string(), JsonValue::Object(endpoints));
        object.insert("examples".to_string(), JsonValue::Object(examples));
        object.insert(
            "docs".to_string(),
            JsonValue::String("https://reddb.io/docs".to_string()),
        );
        object.insert(
            "transport_listeners".to_string(),
            self.transport_readiness_json(),
        );
        json_response(200, JsonValue::Object(object))
    }

    fn health_json_with_transport(&self, report: &HealthReport) -> JsonValue {
        let mut value = crate::presentation::ops_json::health_json(report);
        if let JsonValue::Object(ref mut object) = value {
            object.insert(
                "transport_listeners".to_string(),
                self.transport_readiness_json(),
            );
        }
        value
    }

    pub fn serve(&self) -> io::Result<()> {
        let listener = TcpListener::bind(&self.options.bind_addr)?;
        self.serve_on(listener)
    }

    /// Serve the async axum/hyper HTTP edge (issue #931) on the given
    /// listener until it errors fatally. A dedicated multi-threaded tokio
    /// runtime drives the I/O; the synchronous disk-backed engine is
    /// reached via `spawn_blocking`. This replaces the retired
    /// thread-per-connection accept loop and its `(2*num_cpus)` thread
    /// cap — idle keep-alive connections are now cheap parked tasks.
    pub fn serve_on(&self, listener: TcpListener) -> io::Result<()> {
        let runtime = axum_edge::build_edge_runtime()?;
        runtime.block_on(
            self.clone()
                .serve_edge_on_std(listener, HttpTransport::Http),
        )
    }

    /// Accept and serve a single connection to completion, then return.
    /// Used by tests that want a one-shot HTTP server alongside another
    /// transport.
    pub fn serve_one_on(&self, listener: TcpListener) -> io::Result<()> {
        let runtime = axum_edge::build_background_edge_runtime()?;
        let server = self.clone();
        runtime.block_on(async move {
            listener.set_nonblocking(true)?;
            let listener = tokio::net::TcpListener::from_std(listener)?;
            let (stream, _peer) = listener.accept().await?;
            server.serve_edge_one(stream).await;
            Ok(())
        })
    }

    pub fn serve_in_background(&self) -> thread::JoinHandle<io::Result<()>> {
        let server = self.clone();
        thread::spawn(move || server.serve())
    }

    pub fn serve_in_background_on(
        &self,
        listener: TcpListener,
    ) -> thread::JoinHandle<io::Result<()>> {
        let server = self.clone();
        thread::spawn(move || {
            let runtime = axum_edge::build_background_edge_runtime()?;
            runtime.block_on(server.serve_edge_on_std(listener, HttpTransport::Http))
        })
    }

    /// Serve TLS-wrapped HTTPS on the configured `bind_addr`. The
    /// `tls_config` is shared across all connections (rustls
    /// `ServerConfig` is `Send + Sync`).
    pub fn serve_tls(&self, tls_config: std::sync::Arc<rustls::ServerConfig>) -> io::Result<()> {
        let listener = TcpListener::bind(&self.options.bind_addr)?;
        self.serve_tls_on(listener, tls_config)
    }

    pub fn serve_tls_on(
        &self,
        listener: TcpListener,
        tls_config: std::sync::Arc<rustls::ServerConfig>,
    ) -> io::Result<()> {
        let runtime = axum_edge::build_edge_runtime()?;
        let acceptor = axum_edge::tls_acceptor(tls_config);
        runtime.block_on(self.clone().serve_edge_tls_on_std(
            listener,
            acceptor,
            HttpTransport::Https,
        ))
    }

    pub fn serve_tls_in_background(
        &self,
        tls_config: std::sync::Arc<rustls::ServerConfig>,
    ) -> thread::JoinHandle<io::Result<()>> {
        let server = self.clone();
        thread::spawn(move || server.serve_tls(tls_config))
    }

    pub fn serve_tls_in_background_on(
        &self,
        listener: TcpListener,
        tls_config: std::sync::Arc<rustls::ServerConfig>,
    ) -> thread::JoinHandle<io::Result<()>> {
        let server = self.clone();
        thread::spawn(move || {
            let runtime = axum_edge::build_background_edge_runtime()?;
            let acceptor = axum_edge::tls_acceptor(tls_config);
            runtime.block_on(server.serve_edge_tls_on_std(listener, acceptor, HttpTransport::Https))
        })
    }

    fn handle_connection(&self, stream: TcpStream) -> io::Result<()> {
        let started = Instant::now();
        let result = self.handle_connection_inner(stream);
        let elapsed = started.elapsed().as_secs_f64();
        self.http_metrics
            .record_duration(HttpTransport::Http, elapsed);
        result
    }

    fn handle_connection_inner(&self, mut stream: TcpStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_millis(self.options.read_timeout_ms)))?;
        stream.set_write_timeout(Some(Duration::from_millis(self.options.write_timeout_ms)))?;

        // Issue #570 slice 2 / #621: arm a deadline at handler spawn and
        // check at coarse boundaries. Armed against the injectable
        // monotonic clock (#621) so timeout behaviour is deterministically
        // testable without real sleeps; production wires the real clock,
        // so this tracks wall time. No hard pre-emption — a thread blocked
        // inside a true syscall is still bounded only by the per-socket
        // read/write timeouts.
        let deadline = HandlerDeadline::arm(Arc::clone(&self.handler_clock), self.handler_timeout);

        let request = HttpRequest::read_from(&mut stream, self.options.max_body_bytes)?;

        // Boundary (a): between request parse and route dispatch.
        if deadline.expired() {
            self.http_metrics
                .record_reject(HttpTransport::Http, HttpRejectReason::HandlerTimeout);
            Self::write_handler_timeout_503(&mut stream);
            return Ok(());
        }

        if self.try_route_streaming(&request, &mut stream)? {
            return Ok(());
        }
        let response = self.route(request);

        // Test-only injected slow downstream (issue #570 slice 2
        // integration test). Production builds set this to 0, so this
        // is a single relaxed atomic load on the hot path.
        let inject_ms = self.slow_inject_ms.load(Ordering::Relaxed);
        if inject_ms > 0 {
            thread::sleep(Duration::from_millis(inject_ms));
        }

        // Boundary (b): between route dispatch and response write.
        if deadline.expired() {
            self.http_metrics
                .record_reject(HttpTransport::Http, HttpRejectReason::HandlerTimeout);
            Self::write_handler_timeout_503(&mut stream);
            return Ok(());
        }

        stream.write_all(&response.to_http_bytes())?;
        stream.flush()?;
        Ok(())
    }

    /// Best-effort 503 emitted when the per-handler deadline expires
    /// at a coarse boundary. Writes are swallowed — the caller has
    /// already exceeded its budget, so we do not propagate write
    /// errors. Permit drop happens on the handler thread's normal
    /// exit path.
    fn write_handler_timeout_503<S: Write>(stream: &mut S) {
        const RESPONSE: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\n\
            Connection: close\r\n\
            Content-Length: 0\r\n\
            \r\n";
        let _ = stream.write_all(RESPONSE);
        let _ = stream.flush();
    }
}
