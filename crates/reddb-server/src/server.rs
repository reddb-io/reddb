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

pub mod handlers_admin;
mod handlers_ai;
pub mod http_connection_limiter;
mod handlers_auth;
mod handlers_backup;
mod handlers_ec;
mod handlers_entity;
mod handlers_geo;
mod handlers_graph;
mod handlers_keyed;
mod handlers_log;
mod handlers_metrics;
mod handlers_ops;
mod handlers_query;
mod handlers_replication;
mod handlers_vcs;
mod handlers_vector;
pub mod header_escape_guard;
pub mod ingest_pipeline;
mod patch_support;
mod request_body;
mod request_context;
mod routing;
mod serverless_support;
pub mod tls;
mod transport;

use self::handlers_ai::*;
use self::http_connection_limiter::HttpConnectionLimiter;
use self::handlers_entity::*;
use self::handlers_graph::*;
use self::handlers_keyed::*;
use self::handlers_metrics::*;
use self::handlers_ops::*;
use self::handlers_query::*;
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
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:5055".to_string(),
            max_body_bytes: 1024 * 1024,
            read_timeout_ms: 5_000,
            write_timeout_ms: 5_000,
            max_scan_limit: 1_000,
            surface: ServerSurface::Public,
            transport_readiness: crate::service_cli::TransportReadiness::default(),
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
    /// Test-only synchronous sleep injected between route dispatch and
    /// response write so an integration test can simulate a slow
    /// downstream tripping the deadline. Default 0 (no-op). Shared via
    /// `Arc` so a cloned `RedDBServer` (e.g. `serve_in_background`)
    /// observes flips from the originating handle. Production callers
    /// have no way to set this — the setter is `#[doc(hidden)]`.
    slow_inject_ms: Arc<AtomicU64>,
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
            slow_inject_ms: Arc::new(AtomicU64::new(0)),
        }
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

    pub fn serve_on(&self, listener: TcpListener) -> io::Result<()> {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => match self.http_limiter.try_acquire() {
                    Some(permit) => {
                        // Spawn a thread per connection for concurrent request handling
                        let server = self.clone();
                        thread::spawn(move || {
                            let _guard = permit; // released on thread exit
                            let _ = server.handle_connection(stream);
                        });
                    }
                    None => {
                        // Cap exhausted: write static 503 inline on the
                        // accept thread, close the socket, and continue.
                        // No thread spawn, no `HttpRequest::read_from`,
                        // no runtime call.
                        Self::reject_with_503(stream, self.options.write_timeout_ms);
                    }
                },
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    /// Static 503 response used when the connection limiter is full.
    /// Inlined into the accept loop so it costs one write and a close.
    fn reject_with_503(mut stream: TcpStream, write_timeout_ms: u64) {
        const RESPONSE: &[u8] = b"HTTP/1.1 503 Service Unavailable\r\n\
            Connection: close\r\n\
            Content-Length: 0\r\n\
            Retry-After: 5\r\n\
            \r\n";
        let _ = stream.set_write_timeout(Some(Duration::from_millis(write_timeout_ms)));
        let _ = stream.write_all(RESPONSE);
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }

    pub fn serve_one_on(&self, listener: TcpListener) -> io::Result<()> {
        let (stream, _) = listener.accept()?;
        self.handle_connection(stream)
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
        thread::spawn(move || server.serve_on(listener))
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
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let server = self.clone();
                    let cfg = tls_config.clone();
                    thread::spawn(move || {
                        let _ = server.handle_tls_connection(stream, cfg);
                    });
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
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
        thread::spawn(move || server.serve_tls_on(listener, tls_config))
    }

    fn handle_connection(&self, mut stream: TcpStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_millis(self.options.read_timeout_ms)))?;
        stream.set_write_timeout(Some(Duration::from_millis(self.options.write_timeout_ms)))?;

        // Issue #570 slice 2: arm a deadline at handler spawn and
        // check at coarse boundaries. No hard pre-emption — a thread
        // blocked inside a true syscall is still bounded only by the
        // per-socket read/write timeouts.
        let deadline = Instant::now() + self.handler_timeout;

        let request = HttpRequest::read_from(&mut stream, self.options.max_body_bytes)?;

        // Boundary (a): between request parse and route dispatch.
        if Instant::now() >= deadline {
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
        if Instant::now() >= deadline {
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

    fn handle_tls_connection(
        &self,
        tcp: TcpStream,
        tls_config: std::sync::Arc<rustls::ServerConfig>,
    ) -> io::Result<()> {
        tcp.set_read_timeout(Some(Duration::from_millis(self.options.read_timeout_ms)))?;
        tcp.set_write_timeout(Some(Duration::from_millis(self.options.write_timeout_ms)))?;
        let mut tls_stream = match self::tls::accept_tls(tls_config, tcp) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    target: "reddb::http_tls",
                    err = %err,
                    "TLS handshake failed"
                );
                return Err(err);
            }
        };
        let request = match HttpRequest::read_from(&mut tls_stream, self.options.max_body_bytes) {
            Ok(req) => req,
            Err(err) => {
                tracing::warn!(
                    target: "reddb::http_tls",
                    err = %err,
                    "TLS request parse failed"
                );
                return Err(err);
            }
        };
        if self.try_route_streaming(&request, &mut tls_stream)? {
            return Ok(());
        }
        let response = self.route(request);
        tls_stream.write_all(&response.to_http_bytes())?;
        tls_stream.flush()?;
        Ok(())
    }
}
