use super::*;
use std::sync::{Mutex, OnceLock};

/// Issue #767 / S8 — context handed to streaming handlers so they can
/// emit `stream.opened` / `stream.closed` audit events without
/// re-deriving the principal or peeking back into the request headers.
///
/// `principal` is the stable label produced by [`principal_for`] (the
/// same value used by the QPS quota gate and the capacity registry).
/// `token` is the raw bearer string, retained so handlers can decide
/// whether the bearer credential's expiry is shorter than the lease's
/// snapshot-TTL deadline.
pub(crate) struct StreamAuditCtx<'a> {
    pub principal: &'a str,
    pub token: Option<&'a str>,
}

fn extract_bearer(headers: &BTreeMap<String, String>) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
}

const CATALOG_DEPRECATION_DATE: &str = "2026-08-08";
const CATALOG_DEPRECATION_LOG_INTERVAL: Duration = Duration::from_secs(60);

fn deprecated_catalog_response(endpoint: &'static str, mut response: HttpResponse) -> HttpResponse {
    warn_deprecated_catalog_endpoint(endpoint);

    response.extra_headers.push((
        "Deprecation",
        crate::server::header_escape_guard::HeaderEscapeGuard::header_value(
            CATALOG_DEPRECATION_DATE,
        )
        .expect("catalog deprecation date is a valid HTTP header value"),
    ));
    response.extra_headers.push((
        "Sunset",
        crate::server::header_escape_guard::HeaderEscapeGuard::header_value(
            CATALOG_DEPRECATION_DATE,
        )
        .expect("catalog sunset date is a valid HTTP header value"),
    ));
    response
}

fn warn_deprecated_catalog_endpoint(endpoint: &'static str) {
    static LAST_WARNED: OnceLock<Mutex<BTreeMap<&'static str, SystemTime>>> = OnceLock::new();

    let now = SystemTime::now();
    let mut last_warned = match LAST_WARNED
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
    {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let should_warn = last_warned
        .get(endpoint)
        .and_then(|last| now.duration_since(*last).ok())
        .map(|elapsed| elapsed >= CATALOG_DEPRECATION_LOG_INTERVAL)
        .unwrap_or(true);

    if should_warn {
        last_warned.insert(endpoint, now);
        tracing::warn!(
            target: "reddb::http",
            endpoint,
            deprecation = CATALOG_DEPRECATION_DATE,
            sunset = CATALOG_DEPRECATION_DATE,
            "deprecated catalog endpoint used"
        );
    }
}

impl RedDBServer {
    pub(crate) fn try_route_streaming<W: std::io::Write>(
        &self,
        request: &HttpRequest,
        writer: &mut W,
    ) -> io::Result<bool> {
        // Two streaming dispatch paths share this entry:
        //   * SSE for `ASK ... STREAM` queries (pre-existing).
        //   * NDJSON for clients that opt in with
        //     `Accept: application/x-ndjson` (issue #760).
        // Both ride the same auth + quota gate below so the streaming
        // surface inherits the non-streaming /query authorisation
        // model unchanged.
        let is_query_post = matches!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/query")
        );
        let is_input_stream_post = matches!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/streams/input")
        ) && content_type_is_ndjson(&request.headers);
        // Issue #805 / #750 — the dedicated read-only SELECT streaming
        // route. Unlike `/query` (which only streams when the client
        // opts in via an NDJSON Accept header or an ASK STREAM body),
        // `/query/stream` is inherently a streaming endpoint: any POST
        // to it streams NDJSON, so no content negotiation is needed.
        let is_select_stream_post = matches!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/query/stream")
        );
        if !is_query_post && !is_input_stream_post && !is_select_stream_post {
            return Ok(false);
        }
        let is_sse = is_query_post && is_stream_ask_query_body(&request.body);
        let is_ndjson = is_query_post && wants_ndjson_response(&request.headers);
        if !is_sse && !is_ndjson && !is_input_stream_post && !is_select_stream_post {
            return Ok(false);
        }

        if !self.surface_allows(&request.path) {
            writer.write_all(&json_error(404, "not found on this listener").to_http_bytes())?;
            writer.flush()?;
            return Ok(true);
        }

        if !self.is_authorized(&request.method, &request.path, &request.headers) {
            writer.write_all(&json_error(401, "unauthorized").to_http_bytes())?;
            writer.flush()?;
            return Ok(true);
        }

        let principal = principal_for(&request.headers);
        match self.runtime.quota_bucket().consume(&principal) {
            crate::runtime::quota_bucket::QuotaOutcome::Throttled => {
                let retry = self.runtime.quota_bucket().retry_after_secs();
                let response = HttpResponse {
                    status: 429,
                    content_type: "application/json",
                    body: format!("{{\"error\":\"rate limited\",\"retry_after_secs\":{retry}}}")
                        .into_bytes(),
                    extra_headers: Vec::new(),
                };
                writer.write_all(&response.to_http_bytes())?;
                writer.flush()?;
                Ok(true)
            }
            crate::runtime::quota_bucket::QuotaOutcome::Granted
            | crate::runtime::quota_bucket::QuotaOutcome::NotConfigured => {
                // SSE wins precedence when both an ASK STREAM body and
                // an NDJSON Accept header are present — ASK STREAM is
                // the long-standing surface; NDJSON is opt-in for
                // ordinary SELECTs.
                let bearer = extract_bearer(&request.headers);
                let audit_ctx = StreamAuditCtx {
                    principal: &principal,
                    token: bearer,
                };
                if is_select_stream_post {
                    // Issue #805 / #750 — read-only SELECT streaming.
                    // Rides the same auth + quota gate above; the
                    // handler owns the read-only gate, descriptor-first
                    // framing, and chunked NDJSON body. Lease/audit/
                    // capacity plumbing is deferred to #750 siblings, so
                    // this branch does not acquire a capacity guard.
                    //
                    // Issue #807 / 750c — the cursor registry scopes each
                    // entry to the requesting `(tenant, principal)`, so the
                    // handler needs both here. `principal` is the same label
                    // the quota gate used above; `tenant` is resolved from
                    // the request headers.
                    let tenant = self.stream_tenant_for(&request.headers);
                    self.handle_query_select_stream(
                        request.body.clone(),
                        &principal,
                        &tenant,
                        writer,
                    )?;
                } else if is_sse {
                    self.handle_query_sse_stream(request.body.clone(), writer)?;
                } else if is_input_stream_post {
                    let cfg = crate::server::output_stream::StreamConfig::load(&self.runtime);
                    match self.stream_capacity.try_acquire(
                        &principal,
                        cfg.max_global_streams,
                        cfg.max_per_principal_streams,
                    ) {
                        Ok(_capacity_guard) => {
                            self.handle_query_ndjson_input_stream(
                                request.body.clone(),
                                &audit_ctx,
                                writer,
                            )?;
                        }
                        Err(err) => {
                            emit_capacity_refused_audit(&self.runtime, &principal, &err);
                            let response = stream_capacity_refusal_response(&err);
                            writer.write_all(&response.to_http_bytes())?;
                            writer.flush()?;
                        }
                    }
                } else {
                    // Issue #761 / S2 — capacity guard. Caps are read
                    // from `red.config` at OpenStream so subsequent
                    // KV mutations apply to future acquisitions only.
                    // The RAII guard is held for the duration of the
                    // handler call so success / mid-stream error /
                    // snapshot expiry / panic unwind all release the
                    // slot through Drop.
                    let cfg = crate::server::output_stream::StreamConfig::load(&self.runtime);
                    match self.stream_capacity.try_acquire(
                        &principal,
                        cfg.max_global_streams,
                        cfg.max_per_principal_streams,
                    ) {
                        Ok(_capacity_guard) => {
                            self.handle_query_ndjson_stream(
                                request.body.clone(),
                                &audit_ctx,
                                writer,
                            )?;
                        }
                        Err(err) => {
                            emit_capacity_refused_audit(&self.runtime, &principal, &err);
                            let response = stream_capacity_refusal_response(&err);
                            writer.write_all(&response.to_http_bytes())?;
                            writer.flush()?;
                        }
                    }
                }
                Ok(true)
            }
        }
    }

    /// Issue #807 / 750c — resolve the tenant a `/query/stream` request runs
    /// under so a minted cursor can be scoped to it. Precedence mirrors the
    /// metrics surface: explicit `x-reddb-tenant` header, then the tenant
    /// carried by the bearer credential (validated JWT or auth-store token),
    /// then the ambient connection tenant, falling back to `"default"`.
    pub(crate) fn stream_tenant_for(&self, headers: &BTreeMap<String, String>) -> String {
        headers
            .get("x-reddb-tenant")
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.trim().to_string())
            .or_else(|| self.stream_tenant_from_bearer(headers))
            .or_else(crate::runtime::impl_core::current_tenant)
            .unwrap_or_else(|| "default".to_string())
    }

    fn stream_tenant_from_bearer(&self, headers: &BTreeMap<String, String>) -> Option<String> {
        let token = headers
            .get("authorization")
            .and_then(|value| value.strip_prefix("Bearer "))?;
        if looks_like_jwt(token) {
            if let Some(validator) = self.runtime.oauth_validator() {
                if let Ok((tenant, _username, _role)) =
                    crate::wire::redwire::auth::validate_oauth_jwt_full(&validator, token)
                {
                    return tenant;
                }
            }
        }
        let auth_store = self.auth_store.as_ref()?;
        auth_store
            .validate_token_full(token)
            .and_then(|(id, _role)| id.tenant)
    }

    pub(crate) fn route(&self, request: HttpRequest) -> HttpResponse {
        let HttpRequest {
            method,
            path,
            query,
            headers,
            body,
        } = request;

        // CORS preflight. Browsers send `OPTIONS` with no credentials
        // before a cross-origin request; it must be answered *before*
        // auth/surface checks (a 401/404 on preflight would block the
        // real request). 204 + the permissive `Access-Control-*` headers
        // that ride on every response (see `HttpResponse::to_http_bytes`).
        if method == "OPTIONS" {
            return HttpResponse {
                status: 204,
                content_type: "application/json",
                body: Vec::new(),
                extra_headers: Vec::new(),
            };
        }

        // PLAN.md Phase 6.2 — endpoint segregation. Listeners bound
        // to the dedicated admin / metrics ports refuse paths
        // outside their surface so accidentally exposing them does
        // not leak the rest of the API.
        if !self.surface_allows(&path) {
            return json_error(404, "not found on this listener");
        }

        if !self.is_authorized(&method, &path, &headers) {
            return json_error(401, "unauthorized");
        }

        // PLAN.md Phase 4.4 — per-caller QPS quota. Health probes
        // skip the gate so probes never trip 429 on a hot instance.
        let is_health_probe = matches!(
            (method.as_str(), path.as_str()),
            ("GET", "/health/live") | ("GET", "/health/ready") | ("GET", "/health/startup")
        );
        if !is_health_probe {
            let principal = principal_for(&headers);
            match self.runtime.quota_bucket().consume(&principal) {
                crate::runtime::quota_bucket::QuotaOutcome::Throttled => {
                    let retry = self.runtime.quota_bucket().retry_after_secs();
                    return HttpResponse {
                        status: 429,
                        content_type: "application/json",
                        body: format!(
                            "{{\"error\":\"rate limited\",\"retry_after_secs\":{retry}}}"
                        )
                        .into_bytes(),
                        extra_headers: Vec::new(),
                    };
                }
                crate::runtime::quota_bucket::QuotaOutcome::Granted
                | crate::runtime::quota_bucket::QuotaOutcome::NotConfigured => {}
            }
        }

        match (method.as_str(), path.as_str()) {
            // Auth endpoints
            ("POST", "/auth/bootstrap") => self.handle_auth_bootstrap(body),
            ("POST", "/auth/login") => self.handle_auth_login(body),
            ("POST", "/v1/_admin/system-users") => self.handle_admin_create_system_user(body),
            ("POST", "/auth/users") => self.handle_auth_create_user(&headers, body, None),
            ("GET", "/auth/users") => self.handle_auth_list_users(&headers, &query),
            ("GET", "/auth/tenants") => self.handle_auth_list_tenants(&headers),
            ("GET", "/auth/policies") => self.handle_auth_list_policies(&headers),
            ("POST", "/auth/can") => self.handle_auth_can(&headers, body),
            ("POST", "/auth/api-keys") => self.handle_auth_create_api_key(body),
            ("POST", "/auth/change-password") => self.handle_auth_change_password(body),
            ("GET", "/auth/whoami") => self.handle_auth_whoami(&headers),
            ("GET", "/config") => self.handle_config_export(),
            ("POST", "/config") => self.handle_config_import(body),
            ("POST", "/ai/ask") => self.handle_ai_ask(body),
            ("POST", "/ai/embeddings") => self.handle_ai_embeddings(body),
            ("POST", "/ai/prompt") => self.handle_ai_prompt(body),
            ("POST", "/ai/credentials") => self.handle_ai_credentials(body),
            ("POST", "/ai/models") => self.handle_ai_model_register(body),
            ("GET", "/ai/models") => self.handle_ai_model_list(),

            // Self-describing entrypoints for first-run/devex.
            ("GET", "/") => self.handle_root_discovery(),
            ("GET", "/query") => self.handle_query_contract(),
            ("GET", "/grpc") => self.handle_grpc_discovery(),

            // Eventual Consistency endpoints
            ("GET", "/ec/status") => {
                if let Some(deny) =
                    self.check_ops_http_policy(&headers, "ops:read:cluster", "eventual-consistency")
                {
                    return deny;
                }
                handlers_ec::handle_ec_global_status(&self.runtime)
            }

            // Geo endpoints
            ("POST", "/geo/distance") => handlers_geo::handle_geo_distance(body),
            ("POST", "/geo/bearing") => handlers_geo::handle_geo_bearing(body),
            ("POST", "/geo/midpoint") => handlers_geo::handle_geo_midpoint(body),
            ("POST", "/geo/destination") => handlers_geo::handle_geo_destination(body),
            ("POST", "/geo/bounding-box") => handlers_geo::handle_geo_bounding_box(body),

            // Vector clustering
            ("POST", "/vectors/cluster") => {
                handlers_vector::handle_vector_cluster(&self.runtime, body)
            }

            // Log endpoints (handled dynamically below for /logs/{name}/...)

            // CDC & Backup endpoints
            ("GET", "/changes") => self.handle_cdc_poll(&query),
            ("GET", "/backup/status") => {
                if let Some(deny) =
                    self.check_ops_http_policy(&headers, "ops:read:cluster", "backup-status")
                {
                    return deny;
                }
                self.handle_backup_status()
            }
            ("POST", "/backup/trigger") => self.handle_backup_trigger(),
            ("GET", "/recovery/restore-points") => self.handle_restore_points(),

            // Replication endpoints
            ("GET", "/replication/status") => {
                if let Some(deny) =
                    self.check_ops_http_policy(&headers, "ops:read:cluster", "replication-status")
                {
                    return deny;
                }
                self.handle_replication_status()
            }

            // Topology graph (#803) — built-in `red.topology.cluster` analytics,
            // aggregated into the PRD #794 nodes/edges/groups/metadata document.
            ("GET", "/v1/topology/graph") => {
                if let Some(deny) =
                    self.check_ops_http_policy(&headers, "ops:read:cluster", "topology-graph")
                {
                    return deny;
                }
                self.handle_topology_graph()
            }
            ("POST", "/replication/snapshot") => self.handle_replication_snapshot(),

            // PLAN.md Phase 1 — universal lifecycle/health contract.
            ("GET", "/health/live") => self.handle_health_live(),
            ("GET", "/health/ready") => self.handle_health_ready(),
            ("GET", "/health/startup") => self.handle_health_startup(),
            ("POST", "/admin/shutdown") => self.handle_admin_shutdown(),
            ("POST", "/admin/drain") => self.handle_admin_drain(),
            // PLAN.md Phase 3.2 / 3.3 — admin restore + backup.
            ("POST", "/admin/restore") => self.handle_admin_restore(body),
            ("POST", "/admin/backup") => self.handle_admin_backup(&query),
            // PLAN.md Phase 4.3 — dynamic read-only toggle.
            ("POST", "/admin/readonly") => self.handle_admin_readonly(body),
            // Issue #148 — Blob Cache admin maintenance endpoints
            // (closes sweeper.rs flag #4 + the operator UX gap from #148).
            ("POST", "/admin/blob_cache/sweep") => self.handle_admin_blob_cache_sweep(body),
            ("POST", "/admin/blob_cache/flush_namespace") => {
                self.handle_admin_blob_cache_flush_namespace(body)
            }
            // Issue #195 — optimistic-lock compare-and-set on the result cache.
            ("POST", "/admin/cache/compare-and-set") => {
                self.handle_admin_blob_cache_compare_and_set(body)
            }
            // Issue #198 — blob cache stats endpoint.
            ("GET", "/admin/blob_cache/stats") => {
                if let Some(deny) =
                    self.check_ops_http_policy(&headers, "ops:read:cluster", "blob-cache-stats")
                {
                    return deny;
                }
                self.handle_admin_blob_cache_stats(&query)
            }
            // PLAN.md Phase 11.6 — manual replica → primary promotion.
            ("POST", "/admin/failover/promote") => self.handle_admin_failover_promote(body),
            // PLAN.md Phase 5.1 / 5.4 — observability endpoints.
            ("GET", "/metrics") => {
                if let Some(deny) =
                    self.check_ops_http_policy(&headers, "ops:read:cluster", "metrics")
                {
                    return deny;
                }
                self.handle_metrics()
            }
            ("GET", "/api/v1/query") => self.handle_prometheus_query(&headers, &query, None),
            ("POST", "/api/v1/query") => self.handle_prometheus_query(&headers, &query, Some(body)),
            ("GET", "/api/v1/query_range") => {
                self.handle_prometheus_query_range(&headers, &query, None)
            }
            ("POST", "/api/v1/query_range") => {
                self.handle_prometheus_query_range(&headers, &query, Some(body))
            }
            ("POST", "/api/v1/write") => {
                self.handle_prometheus_remote_write(&query, &headers, body)
            }
            ("GET", "/admin/status") => {
                if let Some(deny) =
                    self.check_ops_http_policy(&headers, "ops:read:cluster", "admin-status")
                {
                    return deny;
                }
                self.handle_admin_status()
            }
            // Red UI cluster status snapshot (#738) — single aggregated
            // contract so the UI doesn't need to stitch /admin/status,
            // /replication/status, /backup/status, /ready/* together.
            ("GET", "/cluster/status") => {
                if let Some(deny) =
                    self.check_ops_http_policy(&headers, "ops:read:cluster", "cluster-status")
                {
                    return deny;
                }
                self.handle_cluster_status()
            }
            // Red UI feature & capability discovery (#752). Two levels:
            // static/system capabilities and the effective principal
            // view. Both are on the public allowlist in `is_authorized`.
            ("GET", "/capabilities") => self.handle_capabilities(),
            ("GET", "/auth/capabilities") => self.handle_auth_capabilities(&headers),
            // SOC 2 / HIPAA structured audit query — JSONL/JSON over
            // the active `.audit.log` plus rotated archives.
            ("GET", "/admin/audit") => {
                if let Some(deny) = self.check_ops_http_policy(&headers, "ops:admin", "audit") {
                    return deny;
                }
                self.handle_admin_audit_query(&query)
            }

            ("GET", "/health") => {
                let report = self.native_use_cases().health();
                let status = if report.allows_serving_traffic() {
                    200
                } else {
                    503
                };
                json_response(status, self.health_json_with_transport(&report))
            }
            ("GET", "/ready/query") => {
                let ready = self.native_use_cases().readiness().query;
                let status = if ready { 200 } else { 503 };
                json_response(
                    status,
                    crate::presentation::catalog_json::readiness_json("query", ready),
                )
            }
            ("GET", "/ready/write") => {
                let ready = self.native_use_cases().readiness().write;
                let status = if ready { 200 } else { 503 };
                json_response(
                    status,
                    crate::presentation::catalog_json::readiness_json("write", ready),
                )
            }
            ("GET", "/ready/repair") => {
                let ready = self.native_use_cases().readiness().repair;
                let status = if ready { 200 } else { 503 };
                json_response(
                    status,
                    crate::presentation::catalog_json::readiness_json("repair", ready),
                )
            }
            ("GET", "/ready/serverless") => {
                let native = self.native_use_cases();
                let readiness = native.readiness();
                let health = native.health();
                let authority = native.physical_authority_status();
                let (query_ready, write_ready, repair_ready) = (
                    readiness.query_serverless,
                    readiness.write_serverless,
                    readiness.repair_serverless,
                );
                let ready = query_ready && write_ready && repair_ready;
                let status = if ready { 200 } else { 503 };
                json_response(
                    status,
                    serverless_readiness_summary_to_json(
                        query_ready,
                        write_ready,
                        repair_ready,
                        &health,
                        &authority,
                    ),
                )
            }
            ("GET", "/ready/serverless/query") => {
                let ready = self.native_use_cases().readiness().query_serverless;
                json_response(
                    if ready { 200 } else { 503 },
                    crate::presentation::catalog_json::readiness_json("query", ready),
                )
            }
            ("GET", "/ready/serverless/write") => {
                let ready = self.native_use_cases().readiness().write_serverless;
                json_response(
                    if ready { 200 } else { 503 },
                    crate::presentation::catalog_json::readiness_json("write", ready),
                )
            }
            ("GET", "/ready/serverless/repair") => {
                let ready = self.native_use_cases().readiness().repair_serverless;
                json_response(
                    if ready { 200 } else { 503 },
                    crate::presentation::catalog_json::readiness_json("repair", ready),
                )
            }
            ("GET", "/ready") => {
                let report = self.native_use_cases().health();
                let status = if report.allows_serving_traffic() {
                    200
                } else {
                    503
                };
                json_response(status, self.health_json_with_transport(&report))
            }
            ("GET", "/deployment/profiles") => {
                let profile = query
                    .get("profile")
                    .and_then(|value| deployment_profile_from_token(value.as_str()));
                json_response(
                    200,
                    match profile {
                        Some(profile) => {
                            crate::presentation::deployment_json::deployment_profile_json(
                                match profile {
                                    DeploymentProfile::Embedded => crate::presentation::deployment_json::DeploymentProfileView::Embedded,
                                    DeploymentProfile::Server => crate::presentation::deployment_json::DeploymentProfileView::Server,
                                    DeploymentProfile::Serverless => crate::presentation::deployment_json::DeploymentProfileView::Serverless,
                                },
                            )
                        }
                        None => crate::presentation::deployment_json::deployment_profiles_catalog_json(
                            &[
                                crate::presentation::deployment_json::DeploymentProfileView::Embedded,
                                crate::presentation::deployment_json::DeploymentProfileView::Server,
                                crate::presentation::deployment_json::DeploymentProfileView::Serverless,
                            ],
                            "Use /deployment/profiles?profile=serverless to get the exact serverless contract.",
                        ),
                    },
                )
            }
            ("GET", "/catalog/readiness") => {
                let native = self.native_use_cases();
                let readiness = native.readiness();
                let health = native.health();
                let authority = native.physical_authority_status();
                json_response(
                    200,
                    crate::presentation::ops_json::catalog_readiness_json(
                        readiness.query,
                        readiness.write,
                        readiness.repair,
                        &health,
                        &authority,
                    ),
                )
            }
            ("GET", "/catalog") => {
                let snapshot = self.catalog_use_cases().snapshot();
                let native = self.native_use_cases();
                let readiness = native.readiness();
                let health = native.health();
                let authority = native.physical_authority_status();
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_model_snapshot_with_readiness_json(
                        &snapshot,
                        crate::presentation::ops_json::catalog_readiness_json(
                            readiness.query,
                            readiness.write,
                            readiness.repair,
                            &health,
                            &authority,
                        ),
                    ),
                )
            }
            ("GET", "/catalog/attention") => json_response(
                200,
                crate::presentation::catalog_json::catalog_attention_summary_json(
                    &self.catalog_use_cases().attention_summary(),
                ),
            ),
            ("GET", "/catalog/collections/readiness") => {
                let catalog = self.catalog_use_cases().snapshot();
                deprecated_catalog_response(
                    "/catalog/collections/readiness",
                    json_response(
                        200,
                        crate::presentation::catalog_json::catalog_collection_readiness_json(
                            &catalog.collections,
                        ),
                    ),
                )
            }
            ("GET", "/catalog/collections/readiness/attention") => deprecated_catalog_response(
                "/catalog/collections/readiness/attention",
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_collection_attention_json(
                        &self.catalog_use_cases().collection_attention(),
                    ),
                ),
            ),
            ("GET", "/catalog/consistency") => json_response(
                200,
                crate::presentation::catalog_json::catalog_consistency_json(
                    &self.catalog_use_cases().consistency_report(),
                ),
            ),
            ("GET", "/catalog/indexes/declared") => deprecated_catalog_response(
                "/catalog/indexes/declared",
                json_response(
                    200,
                    crate::presentation::admin_json::indexes_json(
                        &self.catalog_use_cases().declared_indexes(),
                    ),
                ),
            ),
            ("GET", "/catalog/indexes/operational") => deprecated_catalog_response(
                "/catalog/indexes/operational",
                json_response(
                    200,
                    crate::presentation::admin_json::indexes_json(
                        &self.catalog_use_cases().indexes(),
                    ),
                ),
            ),
            ("GET", "/catalog/indexes/status") => deprecated_catalog_response(
                "/catalog/indexes/status",
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_index_statuses_json(
                        &self.catalog_use_cases().index_statuses(),
                    ),
                ),
            ),
            ("GET", "/catalog/indexes/attention") => deprecated_catalog_response(
                "/catalog/indexes/attention",
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_index_attention_json(
                        &self.catalog_use_cases().index_attention(),
                    ),
                ),
            ),
            ("GET", "/catalog/graph/projections/declared") => deprecated_catalog_response(
                "/catalog/graph/projections/declared",
                match self.catalog_use_cases().graph_projections() {
                    Ok(projections) => json_response(
                        200,
                        crate::presentation::admin_json::graph_projections_json(&projections),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                },
            ),
            ("GET", "/catalog/graph/projections/operational") => deprecated_catalog_response(
                "/catalog/graph/projections/operational",
                json_response(
                    200,
                    crate::presentation::admin_json::graph_projections_json(
                        &self.catalog_use_cases().operational_graph_projections(),
                    ),
                ),
            ),
            ("GET", "/catalog/graph/projections/status") => deprecated_catalog_response(
                "/catalog/graph/projections/status",
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_graph_projection_statuses_json(
                        &self.catalog_use_cases().graph_projection_statuses(),
                    ),
                ),
            ),
            ("GET", "/catalog/graph/projections/attention") => deprecated_catalog_response(
                "/catalog/graph/projections/attention",
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_graph_projection_attention_json(
                        &self.catalog_use_cases().graph_projection_attention(),
                    ),
                ),
            ),
            ("GET", "/catalog/analytics-jobs/declared") => deprecated_catalog_response(
                "/catalog/analytics-jobs/declared",
                match self.catalog_use_cases().analytics_jobs() {
                    Ok(jobs) => json_response(
                        200,
                        crate::presentation::admin_json::analytics_jobs_json(&jobs),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                },
            ),
            ("GET", "/catalog/analytics-jobs/operational") => deprecated_catalog_response(
                "/catalog/analytics-jobs/operational",
                json_response(
                    200,
                    crate::presentation::admin_json::analytics_jobs_json(
                        &self.catalog_use_cases().operational_analytics_jobs(),
                    ),
                ),
            ),
            ("GET", "/catalog/analytics-jobs/status") => deprecated_catalog_response(
                "/catalog/analytics-jobs/status",
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_analytics_job_statuses_json(
                        &self.catalog_use_cases().analytics_job_statuses(),
                    ),
                ),
            ),
            ("GET", "/catalog/analytics-jobs/attention") => deprecated_catalog_response(
                "/catalog/analytics-jobs/attention",
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_analytics_job_attention_json(
                        &self.catalog_use_cases().analytics_job_attention(),
                    ),
                ),
            ),
            ("GET", "/physical/metadata") => match self.native_use_cases().physical_metadata() {
                Ok(metadata) => json_response(200, metadata.to_json_value()),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/physical/native-header") => match self.native_use_cases().native_header() {
                Ok(header) => json_response(
                    200,
                    crate::presentation::native_json::native_header_json(header),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/physical/native-collection-roots") => {
                match self.native_use_cases().native_collection_roots() {
                    Ok(roots) => json_response(
                        200,
                        crate::presentation::native_json::collection_roots_json(&roots),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/physical/native-manifest") => {
                match self.native_use_cases().native_manifest_summary() {
                    Ok(summary) => json_response(
                        200,
                        crate::presentation::native_json::native_manifest_summary_json(&summary),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/physical/native-registry") => {
                match self.native_use_cases().native_registry_summary() {
                    Ok(summary) => json_response(
                        200,
                        crate::presentation::ops_json::native_registry_summary_json(&summary),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/physical/native-recovery") => match self
                .native_use_cases()
                .native_recovery_summary()
            {
                Ok(summary) => json_response(
                    200,
                    crate::presentation::native_state_json::native_recovery_summary_json(&summary),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/physical/native-catalog") => match self
                .native_use_cases()
                .native_catalog_summary()
            {
                Ok(summary) => json_response(
                    200,
                    crate::presentation::native_state_json::native_catalog_summary_json(&summary),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/physical/native-metadata-state") => {
                match self.native_use_cases().native_metadata_state_summary() {
                    Ok(summary) => json_response(
                        200,
                        crate::presentation::native_state_json::native_metadata_state_summary_json(
                            &summary,
                        ),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/physical/authority") => json_response(
                200,
                crate::presentation::ops_json::physical_authority_status_json(
                    &self.native_use_cases().physical_authority_status(),
                ),
            ),
            ("GET", "/physical/native-state") => {
                match self.native_use_cases().native_physical_state() {
                    Ok(state) => json_response(
                        200,
                        crate::presentation::native_state_json::native_physical_state_json(
                            &state,
                            crate::presentation::native_json::native_header_json,
                            crate::presentation::native_json::collection_roots_json,
                            crate::presentation::native_json::native_manifest_summary_json,
                            crate::presentation::ops_json::native_registry_summary_json,
                        ),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/physical/native-vector-artifacts") => {
                match self.native_use_cases().native_vector_artifact_pages() {
                    Ok(summaries) => json_response(
                        200,
                        crate::presentation::native_state_json::native_vector_artifact_pages_json(
                            &summaries,
                        ),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/physical/native-vector-artifacts/inspect") => {
                match self.native_use_cases().inspect_vector_artifacts() {
                    Ok(batch) => json_response(
                        200,
                        crate::presentation::native_state_json::native_vector_artifact_batch_json(
                            &batch,
                        ),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/physical/native-header/repair-policy") => {
                match self.native_use_cases().native_header_repair_policy() {
                    Ok(policy) => json_response(
                        200,
                        crate::presentation::native_json::repair_policy_json(&policy),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/manifest") => match self.native_use_cases().manifest_events_filtered(
                query.get("collection").map(String::as_str),
                query.get("kind").map(String::as_str),
                query
                    .get("since_snapshot")
                    .and_then(|value| value.parse::<u64>().ok()),
            ) {
                Ok(events) => json_response(
                    200,
                    crate::presentation::native_json::manifest_events_json(&events),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/graph/projections") => match self.catalog_use_cases().graph_projections() {
                Ok(projections) => json_response(
                    200,
                    crate::presentation::admin_json::graph_projections_json(&projections),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/graph/jobs") => match self.catalog_use_cases().analytics_jobs() {
                Ok(jobs) => json_response(
                    200,
                    crate::presentation::admin_json::analytics_jobs_json(&jobs),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/roots") => match self.native_use_cases().collection_roots() {
                Ok(roots) => json_response(
                    200,
                    crate::presentation::native_json::collection_roots_json(&roots),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/snapshots") => match self.native_use_cases().snapshots() {
                Ok(snapshots) => json_response(
                    200,
                    crate::presentation::native_json::snapshots_json(&snapshots),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/exports") => match self.native_use_cases().exports() {
                Ok(exports) => json_response(
                    200,
                    crate::presentation::native_json::exports_json(&exports),
                ),
                Err(err) => json_error(404, err.to_string()),
            },
            ("GET", "/indexes") => json_response(
                200,
                crate::presentation::admin_json::indexes_json(&self.catalog_use_cases().indexes()),
            ),
            ("GET", "/stats") => json_response(
                200,
                crate::presentation::query_result_json::runtime_stats_json(
                    &self.catalog_use_cases().stats(),
                ),
            ),
            ("GET", "/collections") => {
                let values = self
                    .catalog_use_cases()
                    .collections()
                    .into_iter()
                    .map(JsonValue::String)
                    .collect();
                let mut object = Map::new();
                object.insert("collections".to_string(), JsonValue::Array(values));
                json_response(200, JsonValue::Object(object))
            }
            ("POST", "/collections") => self.handle_create_collection(body),
            ("POST", "/checkpoint") => match self.native_use_cases().checkpoint() {
                Ok(()) => json_ok("checkpoint completed"),
                Err(err) => json_error(500, err.to_string()),
            },
            ("POST", "/snapshot") => match self.native_use_cases().create_snapshot() {
                Ok(snapshot) => json_response(
                    200,
                    crate::presentation::native_json::snapshot_descriptor_json(&snapshot),
                ),
                Err(err) => json_error(500, err.to_string()),
            },
            ("POST", "/physical/native-header/repair") => {
                match self.native_use_cases().repair_native_header_from_metadata() {
                    Ok(policy) => json_response(
                        200,
                        crate::presentation::native_json::repair_policy_json(&policy),
                    ),
                    Err(err) => json_error(500, err.to_string()),
                }
            }
            ("POST", "/physical/metadata/rebuild") => {
                match self
                    .native_use_cases()
                    .rebuild_physical_metadata_from_native_state()
                {
                    Ok(true) => json_ok("physical metadata rebuilt from native state"),
                    Ok(false) => {
                        json_error(409, "native state is not available for metadata rebuild")
                    }
                    Err(err) => json_error(500, err.to_string()),
                }
            }
            ("POST", "/physical/native-state/repair") => {
                match self
                    .native_use_cases()
                    .repair_native_physical_state_from_metadata()
                {
                    Ok(true) => json_ok("native physical state republished from physical metadata"),
                    Ok(false) => json_error(
                        409,
                        "native physical state repair is not available in this mode",
                    ),
                    Err(err) => json_error(500, err.to_string()),
                }
            }
            ("POST", "/physical/native-vector-artifacts/warmup") => {
                match self.native_use_cases().warmup_vector_artifacts() {
                    Ok(batch) => json_response(
                        200,
                        crate::presentation::native_state_json::native_vector_artifact_batch_json(
                            &batch,
                        ),
                    ),
                    Err(err) => json_error(500, err.to_string()),
                }
            }
            ("POST", "/serverless/attach") => self.handle_serverless_attach(body),
            ("POST", "/serverless/warmup") => self.handle_serverless_warmup(body),
            ("POST", "/tick") => self.handle_serverless_reclaim(body),
            ("POST", "/serverless/reclaim") => self.handle_serverless_reclaim(body),
            ("POST", "/export") => self.handle_export(body),
            ("POST", "/indexes/rebuild") => self.handle_rebuild_indexes(body, None),
            ("POST", "/retention/apply") => {
                match self.native_use_cases().apply_retention_policy() {
                    Ok(()) => json_ok("retention policy applied"),
                    Err(err) => json_error(500, err.to_string()),
                }
            }
            ("POST", "/maintenance") => match self.native_use_cases().run_maintenance() {
                Ok(()) => json_ok("maintenance completed"),
                Err(err) => json_error(500, err.to_string()),
            },
            ("POST", "/query/explain") => self.handle_query_explain(body),
            // Issue #808 / 750d — explicit out-of-band cancel for a live
            // `/query/stream`. Accepts `{"cursor":"<token>"}`, scoped to the
            // caller, and tombstones the cursor + raises its executor cancel
            // token. Not a streaming route, so it rides the standard
            // request/response path here rather than `try_route_streaming`.
            ("POST", "/query/stream/cancel") => {
                let principal = principal_for(&headers);
                let tenant = self.stream_tenant_for(&headers);
                self.handle_query_stream_cancel(&body, &principal, &tenant)
            }
            ("POST", "/query") => self.handle_query(body),
            ("POST", "/search") => self.handle_universal_search(body),
            ("POST", "/context") => self.handle_context_search(body),
            ("POST", "/text/search") => self.handle_text_search(body),
            ("POST", "/multimodal/search") => self.handle_multimodal_search(body),
            ("POST", "/hybrid/search") => self.handle_hybrid_search(body),
            ("POST", "/graph/neighborhood") => self.handle_graph_neighborhood(body),
            ("POST", "/graph/traverse") => self.handle_graph_traverse(body),
            ("POST", "/graph/shortest-path") => self.handle_graph_shortest_path(body),
            ("POST", "/graph/analytics/components") => self.handle_graph_components(body),
            ("POST", "/graph/analytics/centrality") => self.handle_graph_centrality(body),
            ("POST", "/graph/analytics/community") => self.handle_graph_community(body),
            ("POST", "/graph/analytics/clustering") => self.handle_graph_clustering(body),
            ("POST", "/graph/analytics/pagerank/personalized") => {
                self.handle_graph_personalized_pagerank(body)
            }
            ("POST", "/graph/analytics/hits") => self.handle_graph_hits(body),
            ("POST", "/graph/analytics/cycles") => self.handle_graph_cycles(body),
            ("POST", "/graph/analytics/topological-sort") => {
                self.handle_graph_topological_sort(body)
            }
            ("POST", "/graph/analytics/properties") => self.handle_graph_properties(body),
            ("POST", "/graph/projections") => self.handle_graph_projection_upsert(body),
            ("POST", "/graph/jobs") => self.handle_analytics_job_upsert(body),
            ("POST", "/graph/jobs/queue") => self.handle_analytics_job_queue(body),
            ("POST", "/graph/jobs/start") => self.handle_analytics_job_start(body),
            ("POST", "/graph/jobs/complete") => self.handle_analytics_job_complete(body),
            ("POST", "/graph/jobs/stale") => self.handle_analytics_job_stale(body),
            ("POST", "/graph/jobs/fail") => self.handle_analytics_job_fail(body),

            // ─── Git-for-Data / VCS — RESTful, collection-centric ───
            ("GET", "/repo") => handlers_vcs::handle_repo_info(&self.runtime),
            ("GET", "/repo/refs") => handlers_vcs::handle_refs_list(&self.runtime, &query),
            ("GET", "/repo/refs/heads") => handlers_vcs::handle_branches_list(&self.runtime),
            ("POST", "/repo/refs/heads") => handlers_vcs::handle_branch_create(&self.runtime, body),
            ("GET", "/repo/refs/tags") => handlers_vcs::handle_tags_list(&self.runtime),
            ("POST", "/repo/refs/tags") => handlers_vcs::handle_tag_create(&self.runtime, body),
            ("GET", "/repo/commits") => handlers_vcs::handle_commits_list(&self.runtime, &query),
            ("POST", "/repo/commits") => handlers_vcs::handle_commit_create(&self.runtime, body),
            _ => {
                // `GET /catalog/collections/{name}` — Red UI metadata
                // contract (#736). Strip the prefix and dispatch only
                // when the tail looks like a single collection name —
                // the exact-match arms above (`/catalog/collections/readiness`,
                // `/catalog/collections/readiness/attention`) already
                // took priority, so a trailing `/readiness*` segment
                // can never reach here.
                if method == "GET" {
                    if let Some(rest) = path.strip_prefix("/catalog/collections/") {
                        let name = rest.trim_matches('/');
                        if !name.is_empty() && !name.contains('/') {
                            if let Some(deny) =
                                self.check_collection_http_policy(&headers, "select", name)
                            {
                                return deny;
                            }
                            return self.handle_collection_ui_metadata(name, &headers);
                        }
                    }
                }

                // IAM policy admin endpoints (Agent #28 lane).
                if path == "/admin/policies" {
                    return match method.as_str() {
                        "GET" => self.handle_iam_policy_list(),
                        _ => json_error(405, "method not allowed"),
                    };
                }
                if path == "/admin/policies/simulate" && method == "POST" {
                    return self.handle_iam_simulate(body);
                }
                if path == "/admin/policies/lint" {
                    return match method.as_str() {
                        "POST" => self.handle_iam_policy_lint(body),
                        _ => json_error(405, "method not allowed"),
                    };
                }
                if path == "/admin/policies/migrate-mode" {
                    return match method.as_str() {
                        "POST" => self.handle_iam_policy_migrate_mode(body),
                        _ => json_error(405, "method not allowed"),
                    };
                }
                if path == "/admin/policies/actions" {
                    return match method.as_str() {
                        "GET" => self.handle_iam_policy_actions(),
                        _ => json_error(405, "method not allowed"),
                    };
                }
                if let Some(rest) = path.strip_prefix("/admin/policies/") {
                    let id = rest.trim_matches('/');
                    if !id.is_empty() && !id.contains('/') {
                        return match method.as_str() {
                            "PUT" => self.handle_iam_policy_put(id, body),
                            "GET" => self.handle_iam_policy_get(id),
                            "DELETE" => self.handle_iam_policy_delete(id),
                            _ => json_error(405, "method not allowed"),
                        };
                    }
                }
                if let Some(rest) = path.strip_prefix("/admin/users/") {
                    // /admin/users/:user/policies/:policy_id  PUT|DELETE
                    // /admin/users/:user/groups/:group       PUT|DELETE
                    // /admin/users/:user/effective-permissions GET
                    if let Some((user, tail)) = rest.split_once('/') {
                        if tail == "effective-permissions" && method == "GET" {
                            return self.handle_iam_effective_permissions(user, &query);
                        }
                        if let Some(group) = tail.strip_prefix("groups/") {
                            let group = group.trim_matches('/');
                            if !group.is_empty() && !group.contains('/') {
                                return match method.as_str() {
                                    "PUT" => self.handle_iam_add_user_group(user, group),
                                    "DELETE" => self.handle_iam_remove_user_group(user, group),
                                    _ => json_error(405, "method not allowed"),
                                };
                            }
                        }
                        if let Some(pid) = tail.strip_prefix("policies/") {
                            let pid = pid.trim_matches('/');
                            if !pid.is_empty() && !pid.contains('/') {
                                return match method.as_str() {
                                    "PUT" => self.handle_iam_attach_user(user, pid),
                                    "DELETE" => self.handle_iam_detach_user(user, pid),
                                    _ => json_error(405, "method not allowed"),
                                };
                            }
                        }
                    }
                }
                if let Some(rest) = path.strip_prefix("/admin/groups/") {
                    if let Some((group, tail)) = rest.split_once('/') {
                        if let Some(pid) = tail.strip_prefix("policies/") {
                            let pid = pid.trim_matches('/');
                            if !pid.is_empty() && !pid.contains('/') {
                                return match method.as_str() {
                                    "PUT" => self.handle_iam_attach_group(group, pid),
                                    "DELETE" => self.handle_iam_detach_group(group, pid),
                                    _ => json_error(405, "method not allowed"),
                                };
                            }
                        }
                    }
                }

                // Log dynamic routes: /logs/{name}/append, /logs/{name}/query, /logs/{name}/retention
                if let Some(rest) = path.strip_prefix("/logs/") {
                    let parts: Vec<&str> = rest.split('/').collect();
                    if parts.len() >= 2 {
                        let log_name = parts[0];
                        let action = parts[1];
                        return match (method.as_str(), action) {
                            ("POST", "append") => {
                                handlers_log::handle_log_append(&self.runtime, log_name, body)
                            }
                            ("GET", "query") => {
                                handlers_log::handle_log_query(&self.runtime, log_name, &query)
                            }
                            ("POST", "retention") => {
                                handlers_log::handle_log_retention(&self.runtime, log_name)
                            }
                            _ => json_error(405, "method not allowed for log endpoint"),
                        };
                    }
                }

                // ─── VCS dynamic routes ───
                //
                // /repo/refs/heads/{name}        GET | PUT | DELETE
                // /repo/refs/tags/{name}         GET | DELETE
                // /repo/commits/{hash}           GET
                // /repo/commits/{a}/diff/{b}     GET
                // /repo/commits/{a}/lca/{b}      GET
                // /repo/sessions/{conn}          GET
                // /repo/sessions/{conn}/*        POST (checkout/merge/reset/
                //                                     cherry-pick/revert)
                // /repo/merges/{msid}            GET
                // /repo/merges/{msid}/conflicts  GET
                // /repo/merges/{msid}/conflicts/{cid}/resolve  POST
                // /collections/{name}/vcs        GET | PUT
                if let Some(rest) = path.strip_prefix("/repo/refs/heads/") {
                    return match method.as_str() {
                        "GET" => handlers_vcs::handle_branch_show(&self.runtime, rest),
                        "PUT" => handlers_vcs::handle_branch_move(&self.runtime, rest, body),
                        "DELETE" => handlers_vcs::handle_branch_delete(&self.runtime, rest),
                        _ => json_error(405, "method not allowed"),
                    };
                }
                if let Some(rest) = path.strip_prefix("/repo/refs/tags/") {
                    return match method.as_str() {
                        "GET" => handlers_vcs::handle_tag_show(&self.runtime, rest),
                        "DELETE" => handlers_vcs::handle_tag_delete(&self.runtime, rest),
                        _ => json_error(405, "method not allowed"),
                    };
                }
                if let Some(rest) = path.strip_prefix("/repo/commits/") {
                    let parts: Vec<&str> = rest.split('/').collect();
                    match parts.as_slice() {
                        [hash] => {
                            return match method.as_str() {
                                "GET" => handlers_vcs::handle_commit_show(&self.runtime, hash),
                                _ => json_error(405, "method not allowed"),
                            };
                        }
                        [a, "diff", b] if method.as_str() == "GET" => {
                            return handlers_vcs::handle_commit_diff(&self.runtime, a, b, &query);
                        }
                        [a, "lca", b] if method.as_str() == "GET" => {
                            return handlers_vcs::handle_commit_lca(&self.runtime, a, b);
                        }
                        _ => {}
                    }
                }
                if let Some(rest) = path.strip_prefix("/repo/sessions/") {
                    let parts: Vec<&str> = rest.split('/').collect();
                    if let Some(conn_str) = parts.first() {
                        if let Ok(conn) = conn_str.parse::<u64>() {
                            match parts.as_slice() {
                                [_] if method.as_str() == "GET" => {
                                    return handlers_vcs::handle_session_status(
                                        &self.runtime,
                                        conn,
                                    );
                                }
                                [_, "checkout"] if method.as_str() == "POST" => {
                                    return handlers_vcs::handle_session_checkout(
                                        &self.runtime,
                                        conn,
                                        body,
                                    );
                                }
                                [_, "merge"] if method.as_str() == "POST" => {
                                    return handlers_vcs::handle_session_merge(
                                        &self.runtime,
                                        conn,
                                        body,
                                    );
                                }
                                [_, "reset"] if method.as_str() == "POST" => {
                                    return handlers_vcs::handle_session_reset(
                                        &self.runtime,
                                        conn,
                                        body,
                                    );
                                }
                                [_, "cherry-pick"] if method.as_str() == "POST" => {
                                    return handlers_vcs::handle_session_cherry_pick(
                                        &self.runtime,
                                        conn,
                                        body,
                                    );
                                }
                                [_, "revert"] if method.as_str() == "POST" => {
                                    return handlers_vcs::handle_session_revert(
                                        &self.runtime,
                                        conn,
                                        body,
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }
                if let Some(rest) = path.strip_prefix("/repo/merges/") {
                    let parts: Vec<&str> = rest.split('/').collect();
                    match parts.as_slice() {
                        [msid] if method.as_str() == "GET" => {
                            return handlers_vcs::handle_merge_show(&self.runtime, msid);
                        }
                        [msid, "conflicts"] if method.as_str() == "GET" => {
                            return handlers_vcs::handle_merge_conflicts(&self.runtime, msid);
                        }
                        [msid, "conflicts", cid, "resolve"] if method.as_str() == "POST" => {
                            return handlers_vcs::handle_conflict_resolve(
                                &self.runtime,
                                msid,
                                cid,
                                body,
                            );
                        }
                        _ => {}
                    }
                }
                if let Some(rest) = path.strip_prefix("/collections/") {
                    let parts: Vec<&str> = rest.split('/').collect();
                    if parts.len() == 2 && parts[1] == "vcs" {
                        return match method.as_str() {
                            "GET" => {
                                handlers_vcs::handle_collection_vcs_show(&self.runtime, parts[0])
                            }
                            "PUT" => handlers_vcs::handle_collection_vcs_set(
                                &self.runtime,
                                parts[0],
                                body,
                            ),
                            _ => json_error(405, "method not allowed"),
                        };
                    }
                }

                // EC dynamic routes: /ec/{collection}/{field}/{action}
                if let Some(rest) = path.strip_prefix("/ec/") {
                    let parts: Vec<&str> = rest.split('/').collect();
                    if parts.len() >= 3 {
                        let ec_collection = parts[0];
                        let ec_field = parts[1];
                        let ec_action = parts[2];
                        return match (method.as_str(), ec_action) {
                            ("POST", "add") => handlers_ec::handle_ec_mutate(
                                &self.runtime,
                                ec_collection,
                                ec_field,
                                "add",
                                body,
                            ),
                            ("POST", "sub") => handlers_ec::handle_ec_mutate(
                                &self.runtime,
                                ec_collection,
                                ec_field,
                                "sub",
                                body,
                            ),
                            ("POST", "set") => handlers_ec::handle_ec_mutate(
                                &self.runtime,
                                ec_collection,
                                ec_field,
                                "set",
                                body,
                            ),
                            ("POST", "consolidate") => handlers_ec::handle_ec_consolidate(
                                &self.runtime,
                                ec_collection,
                                ec_field,
                            ),
                            ("GET", "status") => handlers_ec::handle_ec_status(
                                &self.runtime,
                                ec_collection,
                                ec_field,
                                &query,
                            ),
                            _ => json_error(405, "method not allowed for EC endpoint"),
                        };
                    }
                }

                if method == "GET" {
                    // Auth: GET /auth/tenants/:tenant/users — list
                    // users scoped to that tenant. Platform admins can
                    // hit any tenant; tenant-scoped admins only their
                    // own (the handler enforces this).
                    if let Some(rest) = path.strip_prefix("/auth/tenants/") {
                        if let Some((tenant, tail)) = rest.split_once('/') {
                            if tail == "users" {
                                let tenant = tenant.trim_matches('/');
                                if !tenant.is_empty() {
                                    let mut q = query.clone();
                                    q.insert("tenant".to_string(), tenant.to_string());
                                    return self.handle_auth_list_users(&headers, &q);
                                }
                            }
                        }
                    }
                    if let Some(collection) = collection_from_schema_path(&path) {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "select", collection)
                        {
                            return deny;
                        }
                        return self.handle_describe_collection(collection);
                    }
                    if let Some(collection) = collection_from_scan_path(&path) {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "select", collection)
                        {
                            return deny;
                        }
                        return self.handle_scan(collection, &query);
                    }
                    if let Some((collection, id)) = collection_entity_path(&path) {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "select", collection)
                        {
                            return deny;
                        }
                        return self.handle_get_entity(collection, id);
                    }
                    if let Some(collection) = collection_from_native_vector_artifact_path(&path) {
                        return match self.native_use_cases().inspect_vector_artifact(
                            InspectNativeArtifactInput {
                                collection: collection.to_string(),
                                artifact_kind: query.get("kind").cloned(),
                            },
                        ) {
                            Ok(artifact) => json_response(
                                200,
                                crate::presentation::native_state_json::native_vector_artifact_inspection_json(
                                    &artifact,
                                ),
                            ),
                            Err(err) => json_error(404, err.to_string()),
                        };
                    }
                    if let Some(collection) = collection_from_action_path(&path, "indexes") {
                        return json_response(
                            200,
                            crate::presentation::admin_json::indexes_json(
                                &self.catalog_use_cases().indexes_for_collection(collection),
                            ),
                        );
                    }
                    // Issue #524 — chain-tip endpoint. Returns the cached tip
                    // for a `KIND blockchain` collection so a client can
                    // construct the next INSERT without scanning the chain.
                    if let Some(collection) = collection_from_action_path(&path, "chain-tip") {
                        return handle_chain_tip(&self.runtime, collection);
                    }
                }
                if let Some(response) =
                    self.handle_v1_keyed_route(method.as_str(), &path, &query, &body)
                {
                    return response;
                }
                // AI model registry: /ai/models/{name} and
                // cache actions /ai/models/{name}/{pull|cache}.
                if let Some(rest) = path.strip_prefix("/ai/models/") {
                    let rest = rest.trim_matches('/');
                    if !rest.is_empty() {
                        if let Some((model_name, action)) = rest.split_once('/') {
                            if !model_name.is_empty() && !model_name.contains('/') {
                                return match (method.as_str(), action) {
                                    ("POST", "pull") => self.handle_ai_model_pull(model_name, body),
                                    ("GET", "cache") => {
                                        self.handle_ai_model_cache_status(model_name)
                                    }
                                    ("DELETE", "cache") => {
                                        self.handle_ai_model_cache_drop(model_name)
                                    }
                                    _ => json_error(
                                        405,
                                        "method not allowed for /ai/models/{name}/{action}",
                                    ),
                                };
                            }
                        } else if !rest.contains('/') {
                            return match method.as_str() {
                                "GET" => self.handle_ai_model_get(rest),
                                "PUT" => self.handle_ai_model_update(rest, body),
                                _ => json_error(405, "method not allowed for /ai/models/{name}"),
                            };
                        }
                    }
                }
                // Config key routes: /config/{key.path}
                if let Some(config_key) = path.strip_prefix("/config/") {
                    let config_key = config_key.trim_matches('/');
                    if !config_key.is_empty() {
                        return match method.as_str() {
                            "GET" => self.handle_config_get_key(config_key),
                            "PUT" => self.handle_config_set_key(config_key, body),
                            "DELETE" => self.handle_config_delete_key(config_key),
                            _ => json_error(405, "method not allowed"),
                        };
                    }
                }

                // KV routes: /collections/{collection}/kvs/{key}
                if let Some(collection) = collection_kv_invalidate_tags_path(&path) {
                    return match method.as_str() {
                        "POST" => self.handle_invalidate_kv_tags(collection, body),
                        _ => json_error(405, "method not allowed for KV tag invalidation endpoint"),
                    };
                }
                if let Some((collection, key)) = collection_kv_watch_path(&path) {
                    return match method.as_str() {
                        "GET" => self.handle_watch_kv(&collection, &key, &query),
                        _ => json_error(405, "method not allowed for KV watch endpoint"),
                    };
                }
                if let Some((collection, key)) = collection_kv_path(&path) {
                    let policy_action = match method.as_str() {
                        "GET" => "select",
                        "PUT" => "insert",
                        "DELETE" => "delete",
                        _ => return json_error(405, "method not allowed for KV endpoint"),
                    };
                    if let Some(deny) =
                        self.check_collection_http_policy(&headers, policy_action, &collection)
                    {
                        return deny;
                    }
                    return match method.as_str() {
                        "GET" => self.handle_get_kv(&collection, &key),
                        "PUT" => self.handle_put_kv(&collection, &key, body),
                        "PATCH" => self.handle_patch_kv(&collection, &key, body),
                        "DELETE" => self.handle_delete_kv(&collection, &key),
                        _ => json_error(405, "method not allowed for KV endpoint"),
                    };
                }
                if method == "POST" {
                    // Issue #525 — admin-gated verify-chain + clear-integrity
                    // endpoints.  Both require `RED_ADMIN_TOKEN` when one is
                    // configured (operator surface, not application auth).
                    if let Some(collection) = collection_from_action_path(&path, "verify-chain") {
                        if !admin_token_ok(&headers) {
                            return json_error(401, "verify-chain requires admin token");
                        }
                        return handle_verify_chain(&self.runtime, collection);
                    }
                    if let Some(collection) =
                        collection_from_action_path(&path, "clear-integrity-flag")
                    {
                        if !admin_token_ok(&headers) {
                            return json_error(401, "clear-integrity-flag requires admin token");
                        }
                        return handle_clear_integrity_flag(&self.runtime, collection);
                    }
                    // Auth: POST /auth/tenants/:tenant/users — explicit
                    // tenant-scoped user creation. Equivalent to the
                    // body-`tenant_id` field on /auth/users.
                    if let Some(rest) = path.strip_prefix("/auth/tenants/") {
                        if let Some((tenant, tail)) = rest.split_once('/') {
                            if tail == "users" {
                                let tenant = tenant.trim_matches('/');
                                if !tenant.is_empty() {
                                    return self.handle_auth_create_user(
                                        &headers,
                                        body,
                                        Some(tenant),
                                    );
                                }
                            }
                        }
                    }
                    if let Some(collection) = collection_from_action_path(&path, "trees") {
                        return self.handle_create_tree(collection, body);
                    }
                    if let Some((collection, tree_name)) =
                        collection_tree_action_path(&path, "nodes")
                    {
                        return self.handle_tree_insert_node(collection, tree_name, body);
                    }
                    if let Some((collection, tree_name)) =
                        collection_tree_action_path(&path, "move")
                    {
                        return self.handle_tree_move(collection, tree_name, body);
                    }
                    if let Some((collection, tree_name)) =
                        collection_tree_action_path(&path, "validate")
                    {
                        return self.handle_tree_validate(collection, tree_name);
                    }
                    if let Some((collection, tree_name)) =
                        collection_tree_action_path(&path, "rebalance")
                    {
                        return self.handle_tree_rebalance(collection, tree_name, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/documents") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_bulk_create(
                            collection,
                            body,
                            Self::handle_create_document,
                        );
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/rows") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_bulk_create_rows_fast(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/nodes") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_bulk_create(collection, body, Self::handle_create_node);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/edges") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_bulk_create(collection, body, Self::handle_create_edge);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/vectors") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_bulk_create(
                            collection,
                            body,
                            Self::handle_create_vector,
                        );
                    }
                    if let Some(collection) = collection_from_action_path(&path, "rows") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_create_row(collection, body);
                    }
                    // Issue #582 — Analytics slice 4. BatchInsertEndpoint
                    // accepts an array body, enforces all-or-nothing
                    // commit + AnalyticsSchemaRegistry validation, and
                    // dedups by `Idempotency-Key` header.
                    if let Some(collection) = collection_from_action_path(&path, "batch") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        let idempotency_key =
                            headers.get("idempotency-key").map(|value| value.as_str());
                        return self.handle_batch_insert(collection, body, idempotency_key);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "nodes") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_create_node(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "edges") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_create_edge(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "vectors") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_create_vector(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "documents") {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "insert", collection)
                        {
                            return deny;
                        }
                        return self.handle_create_document(collection, body);
                    }
                    if let Some(name) = index_named_action_path(&path, "enable") {
                        return match self.admin_use_cases().set_index_enabled(&name, true) {
                            Ok(index) => json_response(
                                200,
                                crate::presentation::admin_json::index_json(&index),
                            ),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = index_named_action_path(&path, "disable") {
                        return match self.admin_use_cases().set_index_enabled(&name, false) {
                            Ok(index) => json_response(
                                200,
                                crate::presentation::admin_json::index_json(&index),
                            ),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = index_named_action_path(&path, "warmup") {
                        return match self.admin_use_cases().warmup_index(&name) {
                            Ok(index) => json_response(
                                200,
                                crate::presentation::admin_json::index_json(&index),
                            ),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = index_named_action_path(&path, "building") {
                        return match self.admin_use_cases().mark_index_building(&name) {
                            Ok(index) => json_response(
                                200,
                                crate::presentation::admin_json::index_json(&index),
                            ),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = index_named_action_path(&path, "fail") {
                        return match self.admin_use_cases().fail_index(&name) {
                            Ok(index) => json_response(
                                200,
                                crate::presentation::admin_json::index_json(&index),
                            ),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = index_named_action_path(&path, "stale") {
                        return match self.admin_use_cases().mark_index_stale(&name) {
                            Ok(index) => json_response(
                                200,
                                crate::presentation::admin_json::index_json(&index),
                            ),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = index_named_action_path(&path, "ready") {
                        return match self.admin_use_cases().mark_index_ready(&name) {
                            Ok(index) => json_response(
                                200,
                                crate::presentation::admin_json::index_json(&index),
                            ),
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = graph_projection_named_action_path(&path, "materialize") {
                        return self.materialize_graph_projection_transition(&name);
                    }
                    if let Some(name) = graph_projection_named_action_path(&path, "materializing") {
                        return match self
                            .admin_use_cases()
                            .mark_graph_projection_materializing(&name)
                        {
                            Ok(projection) => {
                                json_response(200, graph_projection_json(&projection))
                            }
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = graph_projection_named_action_path(&path, "fail") {
                        return match self.admin_use_cases().fail_graph_projection(&name) {
                            Ok(projection) => {
                                json_response(200, graph_projection_json(&projection))
                            }
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(name) = graph_projection_named_action_path(&path, "stale") {
                        return match self.admin_use_cases().mark_graph_projection_stale(&name) {
                            Ok(projection) => {
                                json_response(200, graph_projection_json(&projection))
                            }
                            Err(err) => json_error(400, err.to_string()),
                        };
                    }
                    if let Some(collection) =
                        collection_from_native_vector_artifact_warmup_path(&path)
                    {
                        return match self.native_use_cases().warmup_vector_artifact(
                            InspectNativeArtifactInput {
                                collection: collection.to_string(),
                                artifact_kind: query.get("kind").cloned(),
                            },
                        ) {
                            Ok(artifact) => json_response(
                                200,
                                crate::presentation::native_state_json::native_vector_artifact_inspection_json(
                                    &artifact,
                                ),
                            ),
                            Err(err) => json_error(404, err.to_string()),
                        };
                    }
                    if let Some(collection) = collection_from_action_path(&path, "similar") {
                        return self.handle_similar(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "ivf/search") {
                        return self.handle_ivf_search(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "indexes/rebuild")
                    {
                        return self.handle_rebuild_indexes(body, Some(collection));
                    }
                }
                if method == "PATCH" {
                    if let Some((collection, id)) = collection_entity_path(&path) {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "update", collection)
                        {
                            return deny;
                        }
                        return self.handle_patch_entity(collection, id, body);
                    }
                }
                if method == "DELETE" {
                    if let Some((collection, tree_name, node_id)) = collection_tree_node_path(&path)
                    {
                        return self.handle_tree_delete_node(collection, tree_name, node_id);
                    }
                    if let Some((collection, tree_name)) = collection_tree_bare_path(&path) {
                        return self.handle_drop_tree(collection, tree_name);
                    }
                    // DDL: DELETE /collections/:name
                    if let Some(name) = collection_from_bare_path(&path) {
                        return self.handle_drop_collection(name);
                    }
                    // Auth: DELETE /auth/tenants/:tenant/users/:username
                    if let Some(rest) = path.strip_prefix("/auth/tenants/") {
                        if let Some((tenant, tail)) = rest.split_once('/') {
                            if let Some(username) = tail.strip_prefix("users/") {
                                let username = username.trim_matches('/');
                                let tenant = tenant.trim_matches('/');
                                if !username.is_empty() && !tenant.is_empty() {
                                    return self.handle_auth_delete_user(
                                        &headers,
                                        &query,
                                        Some(tenant),
                                        username,
                                    );
                                }
                            }
                        }
                    }
                    // Auth: DELETE /auth/users/:username
                    if let Some(username) = path.strip_prefix("/auth/users/") {
                        let username = username.trim_matches('/');
                        if !username.is_empty() {
                            return self.handle_auth_delete_user(&headers, &query, None, username);
                        }
                    }
                    // Auth: DELETE /auth/api-keys/:key
                    if let Some(key) = path.strip_prefix("/auth/api-keys/") {
                        let key = key.trim_matches('/');
                        if !key.is_empty() {
                            return self.handle_auth_revoke_api_key(key);
                        }
                    }
                    if let Some((collection, id)) = collection_entity_path(&path) {
                        if let Some(deny) =
                            self.check_collection_http_policy(&headers, "delete", collection)
                        {
                            return deny;
                        }
                        return self.handle_delete_entity(collection, id);
                    }
                }
                json_error(404, format!("route not found: {} {}", method, path))
            }
        }
    }

    pub(crate) fn resolve_projection_payload(
        &self,
        payload: &JsonValue,
    ) -> Result<Option<RuntimeGraphProjection>, HttpResponse> {
        self.graph_use_cases()
            .resolve_projection(
                json_string_field(payload, "projection_name").as_deref(),
                crate::application::graph_payload::parse_inline_projection(payload),
            )
            .map_err(|err| json_error(400, err.to_string()))
    }

    /// PLAN.md Phase 6.2 — return whether `path` is reachable on
    /// this listener given its `ServerSurface` (Public / AdminOnly /
    /// MetricsOnly). `/health/*` always passes so orchestrator
    /// probes work on every bind.
    fn surface_allows(&self, path: &str) -> bool {
        match self.options.surface {
            crate::server::ServerSurface::Public => true,
            crate::server::ServerSurface::AdminOnly => {
                path.starts_with("/admin/")
                    || path == "/metrics"
                    || path == "/admin"
                    || path.starts_with("/health/")
                    || path == "/health"
            }
            crate::server::ServerSurface::MetricsOnly => {
                path == "/metrics"
                    || path == "/api/v1/query"
                    || path == "/api/v1/query_range"
                    || path == "/api/v1/write"
                    || path.starts_with("/health/")
                    || path == "/health"
            }
        }
    }

    fn is_authorized(&self, method: &str, path: &str, headers: &BTreeMap<String, String>) -> bool {
        // PLAN.md Phase 1.2 — health probes are unauthenticated by
        // contract: orchestrator probes can't carry tokens and a 401
        // there would mark the pod unhealthy on the first scrape.
        // Liveness, readiness, and startup all stay public.
        if matches!(
            (method, path),
            ("GET", "/health/live") | ("GET", "/health/ready") | ("GET", "/health/startup")
        ) {
            return true;
        }

        // PLAN.md Phase 6.1 — operator endpoints (/admin/*, /metrics)
        // are gated by a separate `RED_ADMIN_TOKEN` independent of
        // the application auth store. When the env var (or its
        // `_FILE` companion) is unset, the endpoints stay open so
        // dev installs and dashboards keep working; once set, every
        // hit needs `Authorization: Bearer <token>` with a
        // constant-time match.
        let requires_admin_token = path.starts_with("/v1/_admin/");
        let is_admin_surface =
            path.starts_with("/admin/") || requires_admin_token || path == "/metrics";
        if is_admin_surface {
            if let Some(expected) = read_admin_token() {
                let presented = headers
                    .get("authorization")
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .unwrap_or("");
                if !crate::crypto::constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
                    return false;
                }
                // Admin token matches — bypass the rest of the auth
                // chain. Operators want /admin/* to keep working when
                // the user-auth backend is down for maintenance.
                return true;
            }
            if requires_admin_token {
                return false;
            }
        }

        // Public endpoints that never require authentication.
        if matches!(
            (method, path),
            ("GET", "/health")
                | ("GET", "/ready")
                | ("GET", "/ready/query")
                | ("GET", "/ready/write")
                | ("GET", "/ready/repair")
                | ("GET", "/ready/serverless")
                | ("GET", "/ready/serverless/query")
                | ("GET", "/ready/serverless/write")
                | ("GET", "/ready/serverless/repair")
                | ("POST", "/auth/login")
                | ("POST", "/auth/bootstrap")
                // #752 — feature/capability discovery. Both levels are
                // unauthenticated: the UI must be able to discover what
                // the server offers (and what an *anonymous* caller may
                // do) before holding a token. `/auth/capabilities`
                // upgrades its answer when a valid bearer is presented.
                | ("GET", "/capabilities")
                | ("GET", "/auth/capabilities")
        ) {
            return true;
        }

        // If no auth store is configured, all requests are permitted.
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return true,
        };

        // If auth is disabled in the config, allow everything.
        if !auth_store.is_enabled() {
            return true;
        }

        // Extract bearer token from Authorization header. JWT-shaped
        // tokens are routed through the configured OAuthValidator
        // first; fallback to AuthStore lookup for opaque API keys /
        // session tokens.
        let token = headers
            .get("authorization")
            .and_then(|v| v.strip_prefix("Bearer "));

        match token {
            Some(tok) => {
                let role = match resolve_bearer_role(tok, &self.runtime, auth_store.as_ref()) {
                    BearerOutcome::Valid(role) => role,
                    BearerOutcome::Invalid => return false,
                    BearerOutcome::NoMatch => return false,
                };
                // POST /auth/can is a self-introspection probe — any
                // authenticated principal (including read-only) can ask
                // what they themselves are permitted to do.
                let is_self_probe = matches!((method, path), ("POST", "/auth/can"));
                let is_write = !matches!(method, "GET" | "HEAD" | "OPTIONS");
                if is_self_probe {
                    role.can_read()
                } else if is_write {
                    role.can_write()
                } else {
                    role.can_read()
                }
            }
            None => {
                // No token: allow only if require_auth is false.
                !auth_store.config().require_auth
            }
        }
    }
}

/// Outcome of resolving a bearer token. Distinguishes a hard reject
/// (signature/issuer/expiry failure) from "no match in any store"
/// because the audit log treats them differently.
pub(crate) enum BearerOutcome {
    Valid(crate::auth::Role),
    /// JWT-shaped + OAuth validator rejected (issuer mismatch, expired,
    /// bad signature, audience mismatch). Keep distinct so callers can
    /// surface `WWW-Authenticate: Bearer error="invalid_token"`.
    Invalid,
    /// Neither the OAuth validator nor the AuthStore recognized the
    /// token. 401 with a generic error.
    NoMatch,
}

/// Returns true when `token` is shaped like a compact JWT
/// (`header.payload.signature` with three base64url segments).
/// Cheap structural check — does NOT validate the signature.
pub(crate) fn looks_like_jwt(token: &str) -> bool {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    if h.is_empty() || p.is_empty() || s.is_empty() {
        return false;
    }
    let valid_b64url = |chunk: &str| {
        chunk
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    valid_b64url(h) && valid_b64url(p) && valid_b64url(s)
}

/// Bearer-token resolution shared by `is_authorized` and the
/// `/auth/whoami` handler. Tries OAuth JWT validation first when the
/// token has JWT shape AND the runtime has an `OAuthValidator`
/// configured; falls back to `AuthStore::validate_token` for opaque
/// tokens (API keys, session tokens) and JWT-shaped tokens when no
/// validator is wired.
pub(crate) fn resolve_bearer_role(
    token: &str,
    runtime: &crate::runtime::RedDBRuntime,
    auth_store: &crate::auth::store::AuthStore,
) -> BearerOutcome {
    let token_fp = token_fingerprint(token);
    if looks_like_jwt(token) {
        if let Some(validator) = runtime.oauth_validator() {
            match crate::wire::redwire::auth::validate_oauth_jwt(&validator, token) {
                Ok((username, role)) => {
                    // F-04: `username` is a JWT-claim string in the
                    // federated case, attacker-influenceable. Strip
                    // CR/LF/control bytes through the LogField escaper.
                    tracing::info!(
                        target: "reddb::http_auth",
                        method = "oauth_jwt",
                        user = %reddb_wire::audit_safe_log_field(&username),
                        token_sha256 = %token_fp,
                        "JWT accepted"
                    );
                    return BearerOutcome::Valid(role);
                }
                Err(reason) => {
                    // F-04: `reason` quotes parts of the token; sanitize.
                    tracing::warn!(
                        target: "reddb::http_auth",
                        method = "oauth_jwt",
                        token_sha256 = %token_fp,
                        reason = %reddb_wire::audit_safe_log_field(&reason),
                        "JWT rejected"
                    );
                    return BearerOutcome::Invalid;
                }
            }
        }
        // JWT-shaped but no validator: fall through to AuthStore. This
        // matches the behaviour where an operator hands out long-lived
        // base64-segmented API keys that happen to look like JWTs.
    }
    match auth_store.validate_token(token) {
        Some((_username, role)) => BearerOutcome::Valid(role),
        None => {
            tracing::debug!(
                target: "reddb::http_auth",
                method = "bearer",
                token_sha256 = %token_fp,
                "bearer token unknown"
            );
            BearerOutcome::NoMatch
        }
    }
}

fn token_fingerprint(token: &str) -> String {
    let digest = crate::crypto::sha256(token.as_bytes());
    crate::utils::to_hex_prefix(&digest, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &str) -> HttpRequest {
        request_with("GET", path, Vec::new())
    }

    fn request_with(method: &str, path: &str, body: Vec<u8>) -> HttpRequest {
        HttpRequest {
            method: method.to_string(),
            path: path.to_string(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            body,
        }
    }

    fn request_with_bearer(method: &str, path: &str, body: Vec<u8>, token: &str) -> HttpRequest {
        let mut request = request_with(method, path, body);
        request
            .headers
            .insert("authorization".to_string(), format!("Bearer {token}"));
        request
    }

    #[test]
    fn fresh_server_health_is_ready_when_query_endpoint_works() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let server = RedDBServer::new(runtime);

        let query = server.route(request_with(
            "POST",
            "/query",
            br#"{"query":"SELECT 1"}"#.to_vec(),
        ));
        assert_eq!(
            query.status,
            200,
            "fresh server should accept queries: {}",
            String::from_utf8_lossy(&query.body)
        );

        let health = server.route(request("/health"));
        let health_body = String::from_utf8_lossy(&health.body);
        assert_eq!(
            health.status, 200,
            "fresh server should not report degraded health after queries work: {}",
            health_body
        );
        assert!(
            health_body.contains("\"state\":\"healthy\""),
            "fresh server should report healthy state after queries work: {health_body}"
        );
        assert!(
            health_body.contains("\"issues\":[]"),
            "fresh server should not report startup drift issues after queries work: {health_body}"
        );

        let ready = server.route(request("/ready"));
        assert_eq!(
            ready.status,
            200,
            "fresh server should be ready after queries work: {}",
            String::from_utf8_lossy(&ready.body)
        );

        let query_ready = server.route(request("/ready/query"));
        assert_eq!(
            query_ready.status,
            200,
            "fresh server should report query readiness after /query works: {}",
            String::from_utf8_lossy(&query_ready.body)
        );
    }

    #[test]
    fn get_grpc_explains_grpc_contract() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let server = RedDBServer::new(runtime);

        let response = server.route(request("/grpc"));

        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let json: crate::json::Value =
            crate::json::from_slice(&response.body).expect("grpc discovery json");
        assert_eq!(json.get("service"), Some(&crate::json!("reddb.v1.RedDB")));
        assert!(
            json.get("examples")
                .and_then(crate::json::Value::as_object)
                .and_then(|examples| examples.get("query"))
                .and_then(crate::json::Value::as_str)
                .unwrap_or_default()
                .contains("grpcurl"),
            "GET /grpc should advertise a grpcurl query example: {json:?}"
        );
    }

    #[test]
    fn get_query_explains_post_contract() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let server = RedDBServer::new(runtime);

        let response = server.route(request("/query"));

        assert_eq!(
            response.status,
            405,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        assert!(
            response
                .extra_headers
                .iter()
                .any(|(name, value)| *name == "Allow" && value == "POST"),
            "GET /query should advertise Allow: POST"
        );
        let json: crate::json::Value =
            crate::json::from_slice(&response.body).expect("query contract json");
        assert_eq!(json.get("ok"), Some(&crate::json!(false)));
        assert_eq!(json.get("code"), Some(&crate::json!("method_not_allowed")));
        assert!(
            json.get("examples")
                .and_then(crate::json::Value::as_object)
                .and_then(|examples| examples.get("json_query"))
                .and_then(crate::json::Value::as_str)
                .unwrap_or_default()
                .contains("SELECT 1"),
            "GET /query should teach the minimal POST /query shape: {json:?}"
        );
    }

    #[test]
    fn root_endpoint_is_self_describing_for_first_run() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let server = RedDBServer::new(runtime);

        let response = server.route(request("/"));

        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let json: crate::json::Value =
            crate::json::from_slice(&response.body).expect("root discovery json");
        assert_eq!(json.get("service"), Some(&crate::json!("reddb")));
        assert_eq!(json.get("ok"), Some(&crate::json!(true)));
        assert!(
            json.get("endpoints")
                .and_then(crate::json::Value::as_object)
                .and_then(|endpoints| endpoints.get("query"))
                .and_then(crate::json::Value::as_str)
                == Some("POST /query"),
            "root discovery should advertise /query: {json:?}"
        );
        assert!(
            json.get("examples")
                .and_then(crate::json::Value::as_object)
                .and_then(|examples| examples.get("http_json_query"))
                .and_then(crate::json::Value::as_str)
                .unwrap_or_default()
                .contains("SELECT 1"),
            "root discovery should include a minimal query example: {json:?}"
        );
    }

    #[test]
    fn deprecated_catalog_endpoint_returns_deprecation_headers() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let server = RedDBServer::new(runtime);

        let response = server.route(request("/catalog/indexes/status"));

        assert_eq!(response.status, 200);
        assert!(response.extra_headers.iter().any(|(name, value)| {
            *name == "Deprecation" && value.to_str().ok() == Some(CATALOG_DEPRECATION_DATE)
        }));
        assert!(response.extra_headers.iter().any(|(name, value)| {
            *name == "Sunset" && value.to_str().ok() == Some(CATALOG_DEPRECATION_DATE)
        }));
    }

    #[test]
    fn canonical_catalog_endpoint_is_not_deprecated() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let server = RedDBServer::new(runtime);

        let response = server.route(request("/catalog"));

        assert_eq!(response.status, 200);
        assert!(response
            .extra_headers
            .iter()
            .all(|(name, _)| *name != "Deprecation" && *name != "Sunset"));
    }

    #[test]
    fn admin_system_users_requires_shared_secret_and_marks_user_system_owned() {
        let previous = std::env::var_os("RED_ADMIN_TOKEN");
        std::env::set_var("RED_ADMIN_TOKEN", "admin-secret");

        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let auth = std::sync::Arc::new(crate::auth::AuthStore::new(crate::auth::AuthConfig {
            enabled: true,
            require_auth: true,
            ..crate::auth::AuthConfig::default()
        }));
        let server = RedDBServer::new(runtime).with_auth(std::sync::Arc::clone(&auth));
        let body =
            br#"{"username":"system","password":"pw","role":"admin","tenant_id":"acme"}"#.to_vec();

        let unauthorized = server.route(request_with(
            "POST",
            "/v1/_admin/system-users",
            body.clone(),
        ));
        assert_eq!(unauthorized.status, 401);

        let created = server.route(request_with_bearer(
            "POST",
            "/v1/_admin/system-users",
            body,
            "admin-secret",
        ));
        assert_eq!(
            created.status,
            201,
            "{}",
            String::from_utf8_lossy(&created.body)
        );

        let user = auth.get_user(Some("acme"), "system").unwrap();
        assert!(user.system_owned);
        assert_eq!(user.role, crate::auth::Role::Admin);

        match previous {
            Some(value) => std::env::set_var("RED_ADMIN_TOKEN", value),
            None => std::env::remove_var("RED_ADMIN_TOKEN"),
        }
    }

    #[test]
    fn v1_keyed_routes_split_kv_config_and_vault_domains() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let server = RedDBServer::new(runtime);

        let kv_put = server.route(request_with(
            "PUT",
            "/v1/kv/sessions/token",
            br#"{"value":"abc","ttl_ms":1000}"#.to_vec(),
        ));
        assert_eq!(
            kv_put.status,
            200,
            "{}",
            String::from_utf8_lossy(&kv_put.body)
        );

        let kv_get = server.route(request("/v1/kv/sessions/token"));
        assert_eq!(kv_get.status, 200);
        let body = String::from_utf8_lossy(&kv_get.body);
        assert!(body.contains("\"collection\":\"sessions\""), "{body}");
        assert!(body.contains("\"key\":\"token\""), "{body}");

        let config_put = server.route(request_with(
            "PUT",
            "/v1/config/app/feature",
            br#"{"value":"on"}"#.to_vec(),
        ));
        assert_eq!(
            config_put.status,
            200,
            "{}",
            String::from_utf8_lossy(&config_put.body)
        );

        let config_ttl = server.route(request_with(
            "PUT",
            "/v1/config/app/temporary",
            br#"{"value":"on","ttl_ms":1000}"#.to_vec(),
        ));
        assert_eq!(config_ttl.status, 400);
        assert!(
            String::from_utf8_lossy(&config_ttl.body).contains("CONFIG does not support TTL"),
            "{}",
            String::from_utf8_lossy(&config_ttl.body)
        );

        let vault_counter = server.route(request_with(
            "POST",
            "/v1/vault/secrets/api_key/incr",
            br#"{"by":1}"#.to_vec(),
        ));
        assert_eq!(vault_counter.status, 405);
    }

    // ---------- Issue #736 — Red UI collection metadata contract ----------
    //
    // Contract tests for `GET /catalog/collections/{name}`. Each test
    // creates a collection of a given model kind, fetches the
    // metadata, and pins the shape the Red UI relies on:
    //   - `model` + `primary_capability` mirror the model kind
    //   - `capabilities[]` lists the action surfaces for that model
    //   - `schema`, `indexes`, `retention`, `tenant_scope`,
    //     `entity_count`, `model_specific`, `actions` all present
    //   - empty collections still classify by model without probing
    //   - not-found returns 404 with the authorization-safe body
    //
    // The runtime is in-memory and has no auth store wired, so the
    // server's principal resolver returns `(None, false)` and the
    // coarse action gates default to "allowed". That is the right
    // contract for "auth not configured" mode.

    fn ui_metadata(server: &RedDBServer, name: &str) -> crate::json::Value {
        let response = server.route(request(&format!("/catalog/collections/{name}")));
        assert_eq!(
            response.status,
            200,
            "ui_metadata expected 200 for {name}: {}",
            String::from_utf8_lossy(&response.body)
        );
        crate::json::from_slice(&response.body).expect("ui metadata JSON")
    }

    fn ddl(server: &RedDBServer, sql: &str) {
        let response = server.route(request_with(
            "POST",
            "/query",
            format!("{{\"query\":{}}}", crate::json!(sql)).into_bytes(),
        ));
        assert!(
            (200..300).contains(&response.status),
            "DDL `{sql}` failed: {}",
            String::from_utf8_lossy(&response.body)
        );
    }

    fn assert_metadata_envelope(payload: &crate::json::Value, name: &str, model: &str) {
        assert_eq!(payload.get("ok"), Some(&crate::json!(true)));
        let collection = payload.get("collection").expect("collection object");
        assert_eq!(collection.get("name"), Some(&crate::json!(name)));
        assert_eq!(collection.get("model"), Some(&crate::json!(model)));
        assert_eq!(
            collection.get("primary_capability"),
            Some(&crate::json!(model))
        );
        let caps = collection
            .get("capabilities")
            .and_then(crate::json::Value::as_array)
            .expect("capabilities array");
        assert!(
            !caps.is_empty(),
            "capabilities must be non-empty for {model}"
        );
        assert!(caps.iter().any(|c| c.as_str() == Some("describe")));
        assert!(collection.get("schema").is_some(), "schema object missing");
        assert!(
            collection.get("indexes").is_some(),
            "indexes object missing"
        );
        assert!(collection.get("retention").is_some(), "retention missing");
        assert!(
            collection.get("tenant_scope").is_some(),
            "tenant_scope missing"
        );
        assert!(
            collection.get("entity_count").is_some(),
            "entity_count missing"
        );
        let actions = collection
            .get("actions")
            .and_then(crate::json::Value::as_object)
            .expect("actions object");
        for required in [
            "read",
            "write",
            "delete",
            "drop_collection",
            "alter_collection",
        ] {
            assert!(
                actions.contains_key(required),
                "actions.{required} missing for {model}"
            );
        }
        let alter = actions.get("alter_collection").unwrap();
        assert_eq!(
            alter.get("allowed"),
            Some(&crate::json!("unknown")),
            "alter_collection must be unknown until #740/#741: {alter}"
        );
        let tenant = collection.get("tenant_scope").unwrap();
        assert_eq!(tenant.get("kind"), Some(&crate::json!("unknown")));
    }

    fn fresh_server() -> RedDBServer {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        RedDBServer::new(runtime)
    }

    #[test]
    fn ui_metadata_table_contract() {
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, name TEXT)",
        );
        let payload = ui_metadata(&server, "accounts");
        assert_metadata_envelope(&payload, "accounts", "table");

        let collection = payload.get("collection").unwrap();
        // Empty table classifies as `model=table` without probing rows.
        assert_eq!(collection.get("entity_count"), Some(&crate::json!(0.0)));
        // Strict schema produces a typed column list from the contract.
        let schema = collection.get("schema").unwrap();
        let mode = schema
            .get("mode")
            .and_then(crate::json::Value::as_str)
            .expect("schema.mode");
        assert!(
            matches!(mode, "strict" | "semi_structured"),
            "table schema.mode unexpected: {mode}"
        );
        let columns = schema
            .get("columns")
            .and_then(crate::json::Value::as_array)
            .expect("schema.columns must be populated for tables");
        assert!(!columns.is_empty(), "table columns must be exposed");
        // Capability tags include table-specific actions.
        let caps: Vec<&str> = collection
            .get("capabilities")
            .and_then(crate::json::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|c| c.as_str())
            .collect();
        for required in ["select", "insert", "update", "delete", "create_index"] {
            assert!(
                caps.contains(&required),
                "table capabilities missing {required}: {caps:?}"
            );
        }
    }

    #[test]
    fn ui_metadata_document_contract() {
        let server = fresh_server();
        ddl(&server, "CREATE DOCUMENT notes");
        let payload = ui_metadata(&server, "notes");
        assert_metadata_envelope(&payload, "notes", "document");
        let caps: Vec<&str> = payload
            .get("collection")
            .unwrap()
            .get("capabilities")
            .and_then(crate::json::Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|c| c.as_str())
            .collect();
        assert!(caps.contains(&"json_path"));
    }

    #[test]
    fn ui_metadata_queue_contract() {
        let server = fresh_server();
        ddl(&server, "CREATE QUEUE jobs");
        let payload = ui_metadata(&server, "jobs");
        assert_metadata_envelope(&payload, "jobs", "queue");

        let collection = payload.get("collection").unwrap();
        let actions = collection
            .get("actions")
            .and_then(crate::json::Value::as_object)
            .unwrap();
        for queue_action in ["push", "peek", "ack", "nack", "purge", "dlq_move"] {
            assert!(
                actions.contains_key(queue_action),
                "queue action {queue_action} missing"
            );
        }
        let model_specific = collection.get("model_specific").unwrap();
        assert!(
            model_specific.get("queue_mode").is_some(),
            "queue_mode must be exposed"
        );
    }

    #[test]
    fn ui_metadata_vector_contract() {
        let server = fresh_server();
        ddl(
            &server,
            "CREATE COLLECTION embeddings KIND vector.turbo DIM 8 METRIC cosine",
        );
        let payload = ui_metadata(&server, "embeddings");
        assert_metadata_envelope(&payload, "embeddings", "vector");
        let model_specific = payload
            .get("collection")
            .unwrap()
            .get("model_specific")
            .unwrap();
        assert_eq!(model_specific.get("dimension"), Some(&crate::json!(8.0)));
        assert_eq!(model_specific.get("metric"), Some(&crate::json!("cosine")));
        let actions = payload
            .get("collection")
            .unwrap()
            .get("actions")
            .and_then(crate::json::Value::as_object)
            .unwrap();
        assert!(actions.contains_key("search"));
        assert!(actions.contains_key("upsert"));
    }

    #[test]
    fn ui_metadata_graph_contract() {
        let server = fresh_server();
        ddl(&server, "CREATE GRAPH social");
        let payload = ui_metadata(&server, "social");
        assert_metadata_envelope(&payload, "social", "graph");
        let actions = payload
            .get("collection")
            .unwrap()
            .get("actions")
            .and_then(crate::json::Value::as_object)
            .unwrap();
        assert!(actions.contains_key("traverse"));
        assert!(actions.contains_key("subgraph"));
    }

    #[test]
    fn ui_metadata_kv_contract() {
        let server = fresh_server();
        ddl(&server, "CREATE KV sessions");
        let payload = ui_metadata(&server, "sessions");
        assert_metadata_envelope(&payload, "sessions", "kv");
        let actions = payload
            .get("collection")
            .unwrap()
            .get("actions")
            .and_then(crate::json::Value::as_object)
            .unwrap();
        for kv_action in ["increment", "compare_and_set", "list_by_prefix"] {
            assert!(
                actions.contains_key(kv_action),
                "kv action {kv_action} missing"
            );
        }
    }

    #[test]
    fn ui_metadata_timeseries_contract() {
        let server = fresh_server();
        ddl(&server, "CREATE TIMESERIES events");
        let payload = ui_metadata(&server, "events");
        assert_metadata_envelope(&payload, "events", "timeseries");
        let model_specific = payload
            .get("collection")
            .unwrap()
            .get("model_specific")
            .unwrap();
        assert!(model_specific.get("session_key").is_some());
        assert!(model_specific.get("session_gap_ms").is_some());
    }

    #[test]
    fn ui_metadata_empty_collection_classifies_without_rows() {
        // Acceptance criterion: empty collections classify by model
        // without probing data rows. We never insert any rows.
        let server = fresh_server();
        ddl(&server, "CREATE TABLE empty_t (id INTEGER PRIMARY KEY)");
        ddl(&server, "CREATE QUEUE empty_q");
        ddl(&server, "CREATE KV empty_kv");

        for (name, model) in [
            ("empty_t", "table"),
            ("empty_q", "queue"),
            ("empty_kv", "kv"),
        ] {
            let payload = ui_metadata(&server, name);
            let collection = payload.get("collection").unwrap();
            assert_eq!(collection.get("model"), Some(&crate::json!(model)));
            assert_eq!(
                collection.get("entity_count"),
                Some(&crate::json!(0.0)),
                "empty collection {name} should report 0 entities without probing"
            );
        }
    }

    #[test]
    fn ui_metadata_not_found_is_authorization_safe() {
        let server = fresh_server();
        let response = server.route(request("/catalog/collections/does_not_exist"));
        assert_eq!(response.status, 404);
        let body: crate::json::Value =
            crate::json::from_slice(&response.body).expect("not-found JSON");
        // Authorization-safe envelope: identical for "absent" and
        // "not-visible-to-this-principal" so the endpoint cannot be
        // used to enumerate collections by probing names.
        assert_eq!(body.get("ok"), Some(&crate::json!(false)));
        assert_eq!(
            body.get("error"),
            Some(&crate::json!("not_found_or_not_visible"))
        );
    }

    // -----------------------------------------------------------------
    // #740 — Red UI security read contracts: /auth/tenants,
    // /auth/policies, /auth/can.
    // -----------------------------------------------------------------

    fn auth_server(require_auth: bool) -> (RedDBServer, std::sync::Arc<crate::auth::AuthStore>) {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let auth = std::sync::Arc::new(crate::auth::AuthStore::new(crate::auth::AuthConfig {
            enabled: true,
            require_auth,
            ..crate::auth::AuthConfig::default()
        }));
        let server = RedDBServer::new(runtime).with_auth(std::sync::Arc::clone(&auth));
        (server, auth)
    }

    fn issue_token(
        auth: &crate::auth::AuthStore,
        tenant: Option<&str>,
        username: &str,
        role: crate::auth::Role,
    ) -> String {
        auth.create_user_in_tenant(tenant, username, "pw", role)
            .expect("create user");
        auth.authenticate_in_tenant(tenant, username, "pw")
            .expect("authenticate")
            .token
    }

    fn parse_json(body: &[u8]) -> crate::json::Value {
        crate::json::from_slice(body).expect("json parse")
    }

    #[test]
    fn auth_tenants_platform_admin_sees_every_tenant() {
        let (server, auth) = auth_server(true);
        let admin_token = issue_token(&auth, None, "admin", crate::auth::Role::Admin);
        // Seed users in two tenants + one platform user.
        auth.create_user_in_tenant(Some("acme"), "alice", "pw", crate::auth::Role::Read)
            .unwrap();
        auth.create_user_in_tenant(Some("widgets"), "bob", "pw", crate::auth::Role::Read)
            .unwrap();

        let response = server.route(request_with_bearer(
            "GET",
            "/auth/tenants",
            Vec::new(),
            &admin_token,
        ));
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );

        let body = parse_json(&response.body);
        let tenants = body
            .get("tenants")
            .and_then(crate::json::Value::as_array)
            .expect("tenants array");
        let ids: Vec<Option<&str>> = tenants
            .iter()
            .map(|t| t.get("id").and_then(crate::json::Value::as_str))
            .collect();
        // Platform admin sees the implicit platform entry + every tenant.
        assert!(ids.contains(&None), "platform entry missing: {ids:?}");
        assert!(ids.contains(&Some("acme")), "acme missing: {ids:?}");
        assert!(ids.contains(&Some("widgets")), "widgets missing: {ids:?}");
    }

    #[test]
    fn auth_tenants_tenant_admin_sees_only_own_tenant() {
        let (server, auth) = auth_server(true);
        // Platform tenant has 'platform_user'; acme has 'alice_admin' as admin.
        auth.create_user_in_tenant(None, "platform_user", "pw", crate::auth::Role::Read)
            .unwrap();
        auth.create_user_in_tenant(Some("widgets"), "bob", "pw", crate::auth::Role::Read)
            .unwrap();
        let token = issue_token(&auth, Some("acme"), "alice_admin", crate::auth::Role::Admin);

        let response = server.route(request_with_bearer(
            "GET",
            "/auth/tenants",
            Vec::new(),
            &token,
        ));
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let body = parse_json(&response.body);
        let tenants = body
            .get("tenants")
            .and_then(crate::json::Value::as_array)
            .unwrap();
        let ids: Vec<Option<&str>> = tenants
            .iter()
            .map(|t| t.get("id").and_then(crate::json::Value::as_str))
            .collect();
        assert_eq!(
            ids,
            vec![Some("acme")],
            "tenant admin must see only own tenant: {ids:?}"
        );
    }

    #[test]
    fn auth_tenants_non_admin_sees_only_self_scope() {
        let (server, auth) = auth_server(true);
        auth.create_user_in_tenant(Some("widgets"), "bob", "pw", crate::auth::Role::Read)
            .unwrap();
        let token = issue_token(&auth, Some("acme"), "carol", crate::auth::Role::Read);
        let response = server.route(request_with_bearer(
            "GET",
            "/auth/tenants",
            Vec::new(),
            &token,
        ));
        assert_eq!(response.status, 200);
        let body = parse_json(&response.body);
        let tenants = body
            .get("tenants")
            .and_then(crate::json::Value::as_array)
            .unwrap();
        let ids: Vec<Option<&str>> = tenants
            .iter()
            .map(|t| t.get("id").and_then(crate::json::Value::as_str))
            .collect();
        assert_eq!(ids, vec![Some("acme")]);
    }

    #[test]
    fn auth_tenants_unauthenticated_requires_auth() {
        let (server, _auth) = auth_server(true);
        let response = server.route(request("/auth/tenants"));
        // Routing middleware gates this — no bearer with require_auth on.
        assert_eq!(response.status, 401);
    }

    #[test]
    fn auth_tenants_auth_disabled_returns_full_set() {
        // require_auth = false, no bearer → handler treats it as
        // platform-admin visibility.
        let (server, auth) = auth_server(false);
        auth.create_user_in_tenant(Some("acme"), "alice", "pw", crate::auth::Role::Read)
            .unwrap();
        let response = server.route(request("/auth/tenants"));
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let body = parse_json(&response.body);
        let tenants = body
            .get("tenants")
            .and_then(crate::json::Value::as_array)
            .unwrap();
        let ids: Vec<Option<&str>> = tenants
            .iter()
            .map(|t| t.get("id").and_then(crate::json::Value::as_str))
            .collect();
        assert!(ids.contains(&Some("acme")));
    }

    #[test]
    fn auth_policies_platform_admin_sees_every_policy() {
        let (server, auth) = auth_server(true);
        let admin_token = issue_token(&auth, None, "admin", crate::auth::Role::Admin);
        // Two policies in different tenants.
        let policy_json = |id: &str, tenant: Option<&str>| {
            let tenant_field = match tenant {
                Some(t) => format!(",\"tenant\":\"{t}\""),
                None => String::new(),
            };
            format!(
                r#"{{"id":"{id}","version":1,"statements":[{{"effect":"Allow","actions":["*"],"resources":["*"]}}]{tenant_field}}}"#
            )
        };
        auth.put_policy(
            crate::auth::policies::Policy::from_json_str(&policy_json("p_platform", None)).unwrap(),
        )
        .unwrap();
        auth.put_policy(
            crate::auth::policies::Policy::from_json_str(&policy_json("p_acme", Some("acme")))
                .unwrap(),
        )
        .unwrap();
        auth.put_policy(
            crate::auth::policies::Policy::from_json_str(&policy_json(
                "p_widgets",
                Some("widgets"),
            ))
            .unwrap(),
        )
        .unwrap();

        let response = server.route(request_with_bearer(
            "GET",
            "/auth/policies",
            Vec::new(),
            &admin_token,
        ));
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let body = parse_json(&response.body);
        let policies = body
            .get("policies")
            .and_then(crate::json::Value::as_array)
            .unwrap();
        let ids: Vec<&str> = policies
            .iter()
            .filter_map(|p| p.get("id").and_then(crate::json::Value::as_str))
            .collect();
        for required in ["p_platform", "p_acme", "p_widgets"] {
            assert!(
                ids.contains(&required),
                "platform admin missing {required}: {ids:?}"
            );
        }
    }

    #[test]
    fn auth_policies_tenant_admin_sees_own_plus_platform() {
        let (server, auth) = auth_server(true);
        let policy_json = |id: &str, tenant: Option<&str>| {
            let tenant_field = match tenant {
                Some(t) => format!(",\"tenant\":\"{t}\""),
                None => String::new(),
            };
            format!(
                r#"{{"id":"{id}","version":1,"statements":[{{"effect":"Allow","actions":["*"],"resources":["*"]}}]{tenant_field}}}"#
            )
        };
        auth.put_policy(
            crate::auth::policies::Policy::from_json_str(&policy_json("p_platform", None)).unwrap(),
        )
        .unwrap();
        auth.put_policy(
            crate::auth::policies::Policy::from_json_str(&policy_json("p_acme", Some("acme")))
                .unwrap(),
        )
        .unwrap();
        auth.put_policy(
            crate::auth::policies::Policy::from_json_str(&policy_json(
                "p_widgets",
                Some("widgets"),
            ))
            .unwrap(),
        )
        .unwrap();
        let token = issue_token(&auth, Some("acme"), "alice_admin", crate::auth::Role::Admin);

        let response = server.route(request_with_bearer(
            "GET",
            "/auth/policies",
            Vec::new(),
            &token,
        ));
        assert_eq!(response.status, 200);
        let body = parse_json(&response.body);
        let policies = body
            .get("policies")
            .and_then(crate::json::Value::as_array)
            .unwrap();
        let ids: Vec<&str> = policies
            .iter()
            .filter_map(|p| p.get("id").and_then(crate::json::Value::as_str))
            .collect();
        assert!(
            ids.contains(&"p_platform"),
            "platform policy hidden: {ids:?}"
        );
        assert!(ids.contains(&"p_acme"), "own tenant policy hidden: {ids:?}");
        assert!(
            !ids.contains(&"p_widgets"),
            "leaked other-tenant policy: {ids:?}"
        );
    }

    #[test]
    fn auth_can_batch_returns_results_per_check() {
        let (server, auth) = auth_server(true);
        let admin_token = issue_token(&auth, None, "admin", crate::auth::Role::Admin);
        // Attach allow-all so admin user has a matching Allow.
        auth.put_policy(
            crate::auth::policies::Policy::from_json_str(
                r#"{"id":"allow_all","version":1,"statements":[{"effect":"Allow","actions":["*"],"resources":["*"]}]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        auth.attach_policy(
            crate::auth::store::PrincipalRef::User(crate::auth::UserId::from_parts(None, "admin")),
            "allow_all",
        )
        .unwrap();

        let body = br#"{"checks":[
            {"action":"read","resource":{"kind":"collection","name":"accounts"}},
            {"action":"delete","resource":{"kind":"collection","name":"audit"}}
        ]}"#
        .to_vec();
        let response = server.route(request_with_bearer("POST", "/auth/can", body, &admin_token));
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let body = parse_json(&response.body);
        let results = body
            .get("results")
            .and_then(crate::json::Value::as_array)
            .expect("results array");
        assert_eq!(results.len(), 2);
        for r in results {
            assert_eq!(r.get("allowed"), Some(&crate::json!(true)));
            assert!(
                r.get("reason")
                    .and_then(crate::json::Value::as_str)
                    .is_some(),
                "reason must be present and a string: {r:?}"
            );
        }
    }

    #[test]
    fn auth_can_denies_when_no_matching_policy_in_policy_only_mode() {
        let (server, auth) = auth_server(true);
        auth.set_enforcement_mode(crate::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
        let token = issue_token(&auth, Some("acme"), "alice", crate::auth::Role::Read);

        let body = br#"{"checks":[
            {"action":"write","resource":{"kind":"collection","name":"secret"}}
        ]}"#
        .to_vec();
        let response = server.route(request_with_bearer("POST", "/auth/can", body, &token));
        assert_eq!(response.status, 200);
        let body = parse_json(&response.body);
        let result = &body
            .get("results")
            .and_then(crate::json::Value::as_array)
            .unwrap()[0];
        assert_eq!(result.get("allowed"), Some(&crate::json!(false)));
        let reason = result
            .get("reason")
            .and_then(crate::json::Value::as_str)
            .unwrap();
        assert!(
            reason.contains("default deny") || reason.starts_with("deny"),
            "reason should explain the deny: {reason}"
        );
    }

    #[test]
    fn auth_can_accepts_single_check_form() {
        let (server, auth) = auth_server(true);
        let token = issue_token(&auth, None, "admin", crate::auth::Role::Admin);
        let body =
            br#"{"action":"read","resource":{"kind":"collection","name":"accounts"}}"#.to_vec();
        let response = server.route(request_with_bearer("POST", "/auth/can", body, &token));
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let body = parse_json(&response.body);
        let results = body
            .get("results")
            .and_then(crate::json::Value::as_array)
            .expect("results array");
        assert_eq!(results.len(), 1, "single-check form must yield one result");
    }

    #[test]
    fn auth_can_unauthenticated_requires_auth() {
        let (server, _auth) = auth_server(true);
        let body = br#"{"checks":[{"action":"read","resource":{"kind":"collection","name":"x"}}]}"#
            .to_vec();
        let response = server.route(request_with("POST", "/auth/can", body));
        // Gated by the routing middleware before reaching the handler.
        assert_eq!(response.status, 401);
    }

    #[test]
    fn auth_can_auth_disabled_returns_results() {
        let (server, _auth) = auth_server(false);
        let body = br#"{"checks":[{"action":"read","resource":{"kind":"collection","name":"x"}}]}"#
            .to_vec();
        let response = server.route(request_with("POST", "/auth/can", body));
        assert_eq!(
            response.status,
            200,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let body = parse_json(&response.body);
        let results = body
            .get("results")
            .and_then(crate::json::Value::as_array)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn ui_metadata_readiness_exact_arm_still_wins() {
        // The exact-match arm for `/catalog/collections/readiness`
        // must not be shadowed by the new dynamic strip_prefix
        // handler (#736). Hitting it must keep returning the
        // readiness payload, not a 404 from the metadata handler.
        let server = fresh_server();
        let response = server.route(request("/catalog/collections/readiness"));
        assert_eq!(
            response.status,
            200,
            "readiness arm must still match: {}",
            String::from_utf8_lossy(&response.body)
        );
    }

    // ───────────────── Issue #760 — output streaming (NDJSON) ─────────────────

    fn ndjson_query(server: &RedDBServer, query: &str) -> Vec<u8> {
        let mut request = request_with(
            "POST",
            "/query",
            format!(r#"{{"query":"{query}"}}"#).into_bytes(),
        );
        request
            .headers
            .insert("accept".to_string(), "application/x-ndjson".to_string());
        let mut buf: Vec<u8> = Vec::new();
        let handled = server
            .try_route_streaming(&request, &mut buf)
            .expect("streaming dispatch");
        assert!(handled, "Accept: application/x-ndjson should be streamed");
        buf
    }

    fn split_response(raw: &[u8]) -> (String, Vec<u8>) {
        let pos = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("header terminator");
        (
            String::from_utf8_lossy(&raw[..pos]).to_string(),
            raw[pos + 4..].to_vec(),
        )
    }

    fn decode_chunked_body(body: &[u8]) -> Vec<u8> {
        // Tiny ad-hoc chunked decoder for the HTTP/1.1 framing we
        // emit in `output_stream::write_chunk`. Sufficient for tests
        // — does not handle trailers or chunk extensions.
        let mut out = Vec::new();
        let mut i = 0;
        while i < body.len() {
            let crlf = body[i..]
                .windows(2)
                .position(|w| w == b"\r\n")
                .expect("chunk size line terminator");
            let size_hex = std::str::from_utf8(&body[i..i + crlf]).expect("utf8 hex");
            let size = usize::from_str_radix(size_hex.trim(), 16).expect("hex chunk size");
            i += crlf + 2;
            if size == 0 {
                break;
            }
            out.extend_from_slice(&body[i..i + size]);
            i += size + 2; // skip trailing \r\n
        }
        out
    }

    #[test]
    fn wants_ndjson_response_matches_canonical_and_alternates() {
        let mut headers = BTreeMap::new();
        assert!(!wants_ndjson_response(&headers));

        headers.insert("accept".to_string(), "application/json".to_string());
        assert!(!wants_ndjson_response(&headers));

        for accept in &[
            "application/x-ndjson",
            "application/ndjson",
            "text/ndjson",
            "Application/X-NDJSON",
            "application/x-ndjson; q=1.0",
            "text/html, application/x-ndjson;q=0.9",
        ] {
            headers.insert("accept".to_string(), (*accept).to_string());
            assert!(wants_ndjson_response(&headers), "Accept={accept}");
        }
    }

    #[test]
    fn ndjson_round_trip_emits_row_lines_and_end_envelope() {
        // End-to-end acceptance criterion #1 — NDJSON wire shape.
        let server = fresh_server();
        let raw = ndjson_query(&server, "SELECT 1 as n");
        let (head, body) = split_response(&raw);
        assert!(
            head.contains("Content-Type: application/x-ndjson"),
            "headers should advertise NDJSON: {head}"
        );
        assert!(
            head.contains("Transfer-Encoding: chunked"),
            "headers should advertise chunked transfer: {head}"
        );
        let decoded = decode_chunked_body(&body);
        let text = String::from_utf8(decoded).expect("ndjson body is utf8");
        let mut lines: Vec<&str> = text.lines().collect();
        let end_line = lines.pop().expect("at least one line");
        assert!(
            end_line.starts_with("{\"end\":"),
            "last line must be end envelope: {end_line}"
        );
        // Issue #766 — first line is the open_ack envelope.
        let open_ack = lines.remove(0);
        assert!(
            open_ack.starts_with("{\"open_ack\":"),
            "first line must be open_ack: {open_ack}"
        );
        assert!(
            open_ack.contains("\"resumable\":"),
            "open_ack must carry resumable flag: {open_ack}"
        );
        assert!(
            !lines.is_empty(),
            "expected at least one row line before end: {text}"
        );
        for row_line in &lines {
            assert!(
                row_line.starts_with("{\"row\":"),
                "non-terminal line must be a row envelope: {row_line}"
            );
        }
    }

    #[test]
    fn ndjson_path_does_not_regress_non_streaming_query() {
        // Acceptance criterion #3 — without the streaming Accept the
        // legacy materialising path keeps the prior wire shape.
        let server = fresh_server();
        let request = request_with("POST", "/query", br#"{"query":"SELECT 1 as n"}"#.to_vec());
        let mut buf: Vec<u8> = Vec::new();
        let handled = server
            .try_route_streaming(&request, &mut buf)
            .expect("dispatch");
        assert!(!handled, "no Accept header → no streaming dispatch");

        let response = server.route(request);
        assert_eq!(response.status, 200);
        let body = String::from_utf8_lossy(&response.body);
        assert!(
            body.contains("\"ok\":true"),
            "legacy shape preserved: {body}"
        );
        assert!(
            body.contains("\"descriptor\":"),
            "descriptor metadata still present: {body}"
        );
    }

    #[test]
    fn ndjson_refuses_when_session_has_active_begin() {
        // Acceptance criterion #4 — OpenStream against a session
        // with an active `BEGIN` is rejected with
        // `stream_in_transaction_unsupported`. We set a non-zero
        // connection id on this thread, execute `BEGIN` to populate
        // the runtime's `tx_contexts` registry, then invoke the
        // streaming dispatch and expect a structured 409 refusal.
        use crate::runtime::impl_core::{clear_current_connection_id, set_current_connection_id};

        let server = fresh_server();
        set_current_connection_id(760_001);
        // Open a transaction on this synthetic connection. The
        // runtime stamps a `TxnContext` keyed by the current id.
        let _ = server
            .runtime
            .execute_query("BEGIN")
            .expect("BEGIN should succeed on a fresh runtime");
        // Sanity check — the accessor sees the open transaction.
        assert!(server.runtime.connection_in_transaction(760_001));

        let mut request = request_with("POST", "/query", br#"{"query":"SELECT 1"}"#.to_vec());
        request
            .headers
            .insert("accept".to_string(), "application/x-ndjson".to_string());
        let mut buf: Vec<u8> = Vec::new();
        let handled = server
            .try_route_streaming(&request, &mut buf)
            .expect("dispatch");
        assert!(handled, "should still be streaming-dispatched");

        // The refusal is a regular HTTP response, not an NDJSON body
        // — headers have not been sent yet at this gate.
        let text = String::from_utf8_lossy(&buf);
        assert!(
            text.starts_with("HTTP/1.1 409"),
            "expected 409 refusal, got: {text}"
        );
        assert!(
            text.contains("stream_in_transaction_unsupported"),
            "structured code missing: {text}"
        );

        // Clean up — rollback so the test does not leak transaction
        // state into other tests run on this thread.
        let _ = server.runtime.execute_query("ROLLBACK");
        clear_current_connection_id();
    }

    // ───────────────── Issue #761 / S2 — capacity guard ─────────────────

    fn ndjson_request_with_bearer(server_query: &str, token: Option<&str>) -> HttpRequest {
        let mut request = request_with(
            "POST",
            "/query",
            format!(r#"{{"query":"{server_query}"}}"#).into_bytes(),
        );
        request
            .headers
            .insert("accept".to_string(), "application/x-ndjson".to_string());
        if let Some(token) = token {
            request
                .headers
                .insert("authorization".to_string(), format!("Bearer {token}"));
        }
        request
    }

    fn dispatch_streaming(server: &RedDBServer, request: &HttpRequest) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        let handled = server
            .try_route_streaming(request, &mut buf)
            .expect("streaming dispatch");
        assert!(handled);
        buf
    }

    #[test]
    fn ndjson_global_capacity_exhausted_returns_429_with_structured_body() {
        // Acceptance criterion #1 + #6 — global cap fires with
        // `server_stream_capacity_exhausted`, HTTP 429, Retry-After,
        // and a structured body carrying `limit` and `current`.
        let server = fresh_server();
        server
            .runtime
            .execute_query("SET CONFIG stream.max_global = 1")
            .expect("set global cap");
        server
            .runtime
            .execute_query("SET CONFIG stream.max_per_principal = 1")
            .expect("set per-principal cap");

        // Manually hold one slot to simulate an in-flight stream.
        let cap_registry = Arc::clone(server.stream_capacity());
        let _hold = cap_registry.try_acquire("anon", 1, 1).expect("first slot");

        let request = ndjson_request_with_bearer("SELECT 1", None);
        let raw = dispatch_streaming(&server, &request);
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.starts_with("HTTP/1.1 429"),
            "expected 429 refusal, got: {text}"
        );
        assert!(
            text.contains("Retry-After: 1"),
            "Retry-After header missing: {text}"
        );
        assert!(
            text.contains("server_stream_capacity_exhausted"),
            "structured code missing: {text}"
        );
        assert!(text.contains("\"limit\":1"), "limit missing: {text}");
        assert!(text.contains("\"current\":1"), "current missing: {text}");
    }

    #[test]
    fn ndjson_per_principal_capacity_isolated_across_principals() {
        // Acceptance criteria #2 + #3 — Alice's quota does not affect
        // Bob, and Alice's overflow is refused with
        // `principal_stream_quota_exhausted` even though global has
        // room.
        let server = fresh_server();
        server
            .runtime
            .execute_query("SET CONFIG stream.max_global = 100")
            .expect("set global cap");
        server
            .runtime
            .execute_query("SET CONFIG stream.max_per_principal = 1")
            .expect("set per-principal cap");

        let cap_registry = Arc::clone(server.stream_capacity());
        // Pre-load: Alice's single slot already held.
        let alice_principal = super::super::routing::principal_for(&{
            let mut h = BTreeMap::new();
            h.insert(
                "authorization".to_string(),
                "Bearer alice-token".to_string(),
            );
            h
        });
        let _hold = cap_registry
            .try_acquire(&alice_principal, 100, 1)
            .expect("Alice's first slot");

        // Alice's second open is refused with the principal-scoped code.
        let alice_request = ndjson_request_with_bearer("SELECT 1", Some("alice-token"));
        let raw = dispatch_streaming(&server, &alice_request);
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.starts_with("HTTP/1.1 429"),
            "expected 429, got: {text}"
        );
        assert!(
            text.contains("principal_stream_quota_exhausted"),
            "expected per-principal code: {text}"
        );
        assert!(text.contains("\"principal\":"), "principal missing: {text}");

        // Bob is unaffected — his stream completes with 200.
        let bob_request = ndjson_request_with_bearer("SELECT 1 as n", Some("bob-token"));
        let raw = dispatch_streaming(&server, &bob_request);
        let text = String::from_utf8_lossy(&raw);
        assert!(
            text.starts_with("HTTP/1.1 200"),
            "Bob should not be affected by Alice's quota: {text}"
        );
    }

    #[test]
    fn ndjson_capacity_slot_released_on_stream_end() {
        // Acceptance criterion #4 — releasing a slot on stream end
        // (normal completion) frees it for both counters. After a
        // successful round-trip the same principal can open a new
        // stream against a cap-of-1 configuration.
        let server = fresh_server();
        server
            .runtime
            .execute_query("SET CONFIG stream.max_global = 1")
            .expect("set global cap");
        server
            .runtime
            .execute_query("SET CONFIG stream.max_per_principal = 1")
            .expect("set per-principal cap");

        for _ in 0..3 {
            let request = ndjson_request_with_bearer("SELECT 1 as n", Some("solo-token"));
            let raw = dispatch_streaming(&server, &request);
            let text = String::from_utf8_lossy(&raw);
            assert!(
                text.starts_with("HTTP/1.1 200"),
                "slot was not released between streams: {text}"
            );
        }
        let (global, per_principal) = server.stream_capacity().snapshot();
        assert_eq!(global, 0, "global counter leaked across streams");
        assert!(
            per_principal.is_empty(),
            "per-principal map leaked: {per_principal:?}"
        );
    }

    // ───────────────── Issue #763 / S4 — input streaming (NDJSON) ─────────────────

    fn input_stream_request(body: &str) -> HttpRequest {
        let mut request = request_with("POST", "/streams/input", body.as_bytes().to_vec());
        request.headers.insert(
            "content-type".to_string(),
            "application/x-ndjson".to_string(),
        );
        request
    }

    fn dispatch_input_stream(server: &RedDBServer, body: &str) -> Vec<u8> {
        let request = input_stream_request(body);
        let mut buf: Vec<u8> = Vec::new();
        let handled = server
            .try_route_streaming(&request, &mut buf)
            .expect("streaming dispatch");
        assert!(
            handled,
            "POST /streams/input with x-ndjson should be streamed"
        );
        buf
    }

    #[test]
    fn ndjson_input_round_trip_commits_rows_and_returns_end_envelope() {
        // Acceptance criterion #1 — round-trip NDJSON insert. After the
        // stream ends every row is visible to non-streaming readers
        // (criterion #2 — chunked auto-commit is durable).
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows763 (id INTEGER PRIMARY KEY, name TEXT)",
        );

        let body = concat!(
            "{\"open\":{\"target\":\"rows763\",\"columns\":[\"id\",\"name\"]}}\n",
            "{\"row\":{\"id\":1,\"name\":\"alice\"}}\n",
            "{\"row\":{\"id\":2,\"name\":\"bob\"}}\n",
            "{\"row\":{\"id\":3,\"name\":\"carol\"}}\n",
        );
        let raw = dispatch_input_stream(&server, body);
        let (head, body_bytes) = split_response(&raw);
        assert!(
            head.contains("Content-Type: application/x-ndjson"),
            "headers should advertise NDJSON: {head}"
        );
        let decoded = decode_chunked_body(&body_bytes);
        let text = String::from_utf8(decoded).expect("ndjson body is utf8");
        // Acceptance criterion #4 — server is silent on success: the
        // only frame in the body is the terminal `{"end": …}` line.
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "server should emit exactly one frame on success: {text}"
        );
        let end_line = lines[0];
        assert!(
            end_line.starts_with("{\"end\":"),
            "single line must be end envelope: {end_line}"
        );
        assert!(end_line.contains("\"row_count\":3"));

        // Acceptance criterion #1 + #2 — rows are durable / visible.
        let response = server.route(request_with(
            "POST",
            "/query",
            br#"{"query":"SELECT id, name FROM rows763"}"#.to_vec(),
        ));
        assert_eq!(response.status, 200);
        let body_text = String::from_utf8_lossy(&response.body);
        assert!(body_text.contains("alice"), "alice missing: {body_text}");
        assert!(body_text.contains("bob"), "bob missing: {body_text}");
        assert!(body_text.contains("carol"), "carol missing: {body_text}");
    }

    #[test]
    fn ndjson_input_malformed_row_terminates_with_error_envelope() {
        // Acceptance criterion #3 — malformed row terminates the stream
        // with a structured error envelope carrying chunk seq + reason
        // + recoverable_rid. Earlier rows up to the failure remain
        // visible.
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows763_err (id INTEGER PRIMARY KEY, name TEXT)",
        );
        // Force per-chunk-of-1 commits so the row before the malformed
        // one is durably committed before the failure point.
        server
            .runtime
            .execute_query("SET CONFIG stream.chunk.max_rows = 1")
            .expect("set chunk size");

        let body = concat!(
            "{\"open\":{\"target\":\"rows763_err\",\"columns\":[\"id\",\"name\"]}}\n",
            "{\"row\":{\"id\":1,\"name\":\"alice\"}}\n",
            "not valid json at all\n",
            "{\"row\":{\"id\":3,\"name\":\"carol\"}}\n",
        );
        let raw = dispatch_input_stream(&server, body);
        let (_head, body_bytes) = split_response(&raw);
        let decoded = decode_chunked_body(&body_bytes);
        let text = String::from_utf8(decoded).expect("utf8");
        let last = text.lines().last().unwrap_or("");
        assert!(
            last.starts_with("{\"error\":"),
            "stream must end with error envelope: {text}"
        );
        assert!(last.contains("\"code\":\"invalid_row\""));
        assert!(last.contains("\"chunk_seq\":"));
        assert!(last.contains("\"recoverable_rid\":"));

        // Acceptance criterion #3 — alice survives the mid-stream failure.
        let response = server.route(request_with(
            "POST",
            "/query",
            br#"{"query":"SELECT id, name FROM rows763_err"}"#.to_vec(),
        ));
        let body_text = String::from_utf8_lossy(&response.body);
        assert!(
            body_text.contains("alice"),
            "alice should survive the failure: {body_text}"
        );
        assert!(
            !body_text.contains("carol"),
            "carol after the failure must not be committed: {body_text}"
        );
    }

    #[test]
    fn ndjson_input_refuses_when_session_has_active_begin() {
        // Acceptance criterion #5 — opening an input stream inside
        // `BEGIN` refuses with `stream_in_transaction_unsupported`.
        use crate::runtime::impl_core::{clear_current_connection_id, set_current_connection_id};

        let server = fresh_server();
        set_current_connection_id(763_001);
        let _ = server
            .runtime
            .execute_query("BEGIN")
            .expect("BEGIN should succeed");
        assert!(server.runtime.connection_in_transaction(763_001));

        let body = "{\"open\":{\"target\":\"any\",\"columns\":[\"id\"]}}\n";
        let request = input_stream_request(body);
        let mut buf: Vec<u8> = Vec::new();
        let handled = server
            .try_route_streaming(&request, &mut buf)
            .expect("dispatch");
        assert!(handled);
        let text = String::from_utf8_lossy(&buf);
        assert!(
            text.starts_with("HTTP/1.1 409"),
            "expected 409 refusal, got: {text}"
        );
        assert!(
            text.contains("stream_in_transaction_unsupported"),
            "structured code missing: {text}"
        );

        let _ = server.runtime.execute_query("ROLLBACK");
        clear_current_connection_id();
    }

    #[test]
    fn ndjson_input_rejects_unsafe_identifier_before_streaming_headers() {
        // SQL-injection guard: identifiers must match the safe
        // character class. A crafted target/column name returns a
        // non-streaming 400 so the failure is unmistakable.
        let server = fresh_server();
        let body = "{\"open\":{\"target\":\"rows; DROP TABLE x\",\"columns\":[\"id\"]}}\n";
        let request = input_stream_request(body);
        let mut buf: Vec<u8> = Vec::new();
        let handled = server
            .try_route_streaming(&request, &mut buf)
            .expect("dispatch");
        assert!(handled);
        let text = String::from_utf8_lossy(&buf);
        assert!(
            text.starts_with("HTTP/1.1 400"),
            "expected 400 refusal, got: {text}"
        );
        assert!(text.contains("safe identifier"), "message: {text}");
    }

    #[test]
    fn ndjson_invalid_query_surfaces_structured_error_line() {
        // Acceptance criterion #2 — mid-stream failures surface as
        // structured NDJSON error envelopes, not silent disconnects.
        let server = fresh_server();
        let raw = ndjson_query(&server, "SELECT NOT_A_REAL_COLUMN FROM nowhere");
        let (_head, body) = split_response(&raw);
        let decoded = decode_chunked_body(&body);
        let text = String::from_utf8(decoded).expect("utf8");
        assert!(
            text.lines().any(|l| l.starts_with("{\"error\":")),
            "expected an error envelope in the stream: {text}"
        );
        assert!(
            text.lines().last().unwrap_or("").starts_with("{\"end\":"),
            "stream must still close with an end envelope: {text}"
        );
    }

    // ───────────── Issue #766 / S7 — resume coordinator ─────────────

    fn ndjson_query_body(server: &RedDBServer, body: Vec<u8>) -> Vec<u8> {
        let mut request = request_with("POST", "/query", body);
        request
            .headers
            .insert("accept".to_string(), "application/x-ndjson".to_string());
        let mut buf: Vec<u8> = Vec::new();
        let handled = server
            .try_route_streaming(&request, &mut buf)
            .expect("streaming dispatch");
        assert!(handled);
        buf
    }

    fn decode_ndjson(raw: &[u8]) -> Vec<String> {
        let (_head, body) = split_response(raw);
        let decoded = decode_chunked_body(&body);
        String::from_utf8(decoded)
            .expect("ndjson utf8")
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn extract_json_field(line: &str, key: &str) -> Option<String> {
        // Crude extractor sufficient for tests; finds `"key":`<value> up
        // to the next `,` or `}` boundary at the same depth (no nesting).
        let needle = format!("\"{key}\":");
        let start = line.find(&needle)? + needle.len();
        let tail = &line[start..];
        let mut depth_obj = 0;
        let mut depth_arr = 0;
        let mut in_str = false;
        let mut escape = false;
        let mut end = tail.len();
        for (i, ch) in tail.char_indices() {
            if escape {
                escape = false;
                continue;
            }
            match ch {
                '\\' if in_str => escape = true,
                '"' => in_str = !in_str,
                '{' if !in_str => depth_obj += 1,
                '}' if !in_str => {
                    if depth_obj == 0 {
                        end = i;
                        break;
                    }
                    depth_obj -= 1;
                }
                '[' if !in_str => depth_arr += 1,
                ']' if !in_str => depth_arr -= 1,
                ',' if !in_str && depth_obj == 0 && depth_arr == 0 => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        Some(tail[..end].trim().trim_matches('"').to_string())
    }

    #[test]
    fn ndjson_open_ack_marks_resumable_for_plain_select() {
        // Acceptance criterion #1 — OpenAck signals resumable: true
        // for an RID-ordered SELECT.
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows766 (id INTEGER PRIMARY KEY, name TEXT)",
        );
        ddl(
            &server,
            "INSERT INTO rows766 (id, name) VALUES (1, 'a'), (2, 'b')",
        );

        let raw = ndjson_query(&server, "SELECT id, name FROM rows766");
        let lines = decode_ndjson(&raw);
        let open_ack = &lines[0];
        assert!(
            open_ack.starts_with("{\"open_ack\":"),
            "first line must be open_ack: {open_ack}"
        );
        assert!(
            open_ack.contains("\"resumable\":true"),
            "plain SELECT should be resumable: {open_ack}"
        );
    }

    #[test]
    fn ndjson_open_ack_marks_non_resumable_for_aggregation() {
        // Acceptance criterion #1 — aggregations are not resumable.
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows766_agg (id INTEGER PRIMARY KEY, name TEXT)",
        );
        ddl(
            &server,
            "INSERT INTO rows766_agg (id, name) VALUES (1, 'a')",
        );

        let raw = ndjson_query(&server, "SELECT COUNT(*) FROM rows766_agg");
        let lines = decode_ndjson(&raw);
        let open_ack = &lines[0];
        assert!(
            open_ack.contains("\"resumable\":false"),
            "aggregation must not be resumable: {open_ack}"
        );
    }

    #[test]
    fn ndjson_resume_continues_from_resume_after_rid_when_hash_matches() {
        // Acceptance criterion #2 — happy-path resume. Open a stream,
        // record the row hash for the prefix, then re-open with
        // `resume_after_rid` + matching `prefix_hash` and observe only
        // the suffix rows being delivered.
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows766_res (id INTEGER PRIMARY KEY, name TEXT)",
        );
        ddl(
            &server,
            "INSERT INTO rows766_res (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')",
        );

        // Open #1 — full stream, capture snapshot_lsn and per-row rids.
        let raw = ndjson_query(&server, "SELECT id, name FROM rows766_res");
        let lines = decode_ndjson(&raw);
        let open_ack = lines.first().expect("open_ack");
        let snapshot_lsn: u64 = extract_json_field(open_ack, "snapshot_lsn")
            .expect("snapshot_lsn")
            .parse()
            .expect("snapshot_lsn integer");

        // Extract row lines (excluding open_ack and end).
        let row_lines: Vec<&str> = lines
            .iter()
            .filter(|l| l.starts_with("{\"row\":"))
            .map(String::as_str)
            .collect();
        assert!(row_lines.len() >= 2, "need ≥2 rows: {lines:?}");

        // Pick the rid of the first row as the resume boundary —
        // resume should deliver rows with rid > that value.
        let first_row = row_lines[0];
        let rid_str = extract_json_field(first_row, "rid").expect("rid present in row metadata");
        let resume_after_rid: u64 = rid_str.parse().expect("rid integer");

        // Compute prefix_hash by hashing the first row's exact bytes.
        use crate::server::output_stream::PrefixHasher;
        let mut hasher = PrefixHasher::new();
        hasher.update(first_row.as_bytes());
        let prefix_hash = hasher.finalize_hex();

        // Resume.
        let body = format!(
            "{{\"query\":\"SELECT id, name FROM rows766_res\",\"resume\":{{\"snapshot_lsn\":{snapshot_lsn},\"resume_after_rid\":{resume_after_rid},\"prefix_hash\":\"{prefix_hash}\"}}}}"
        );
        let raw = ndjson_query_body(&server, body.into_bytes());
        let lines = decode_ndjson(&raw);
        let row_count_in_resume = lines.iter().filter(|l| l.starts_with("{\"row\":")).count();
        let suffix_expected = row_lines.len() - 1;
        assert_eq!(
            row_count_in_resume, suffix_expected,
            "resume delivered wrong row count: lines={lines:?}"
        );
        let end_line = lines.last().expect("end envelope");
        assert!(
            end_line.contains(&format!("\"row_count\":{suffix_expected}")),
            "end row_count must match suffix length: {end_line}"
        );
        assert!(
            !lines.iter().any(|l| l.starts_with("{\"error\":")),
            "happy-path resume must not emit an error envelope: {lines:?}"
        );
    }

    // ───────────── Issue #767 / S8 — lease decoupling + audit ─────────────

    fn read_audit_log(server: &RedDBServer) -> String {
        let logger = server.runtime.audit_log();
        assert!(
            logger.wait_idle(std::time::Duration::from_secs(2)),
            "audit writer did not drain in time"
        );
        std::fs::read_to_string(logger.path()).unwrap_or_default()
    }

    #[test]
    fn ndjson_end_envelope_carries_opaque_lease_handle_not_internal_id() {
        // Acceptance criterion #3 — wire-visible handle is opaque,
        // hex-encoded, ≥128 bits. The deprecated `lease_id` field is
        // gone from the envelope so clients cannot regress to it.
        let server = fresh_server();
        let raw = ndjson_query(&server, "SELECT 1 as n");
        let (_head, body) = split_response(&raw);
        let decoded = decode_chunked_body(&body);
        let text = String::from_utf8(decoded).expect("utf8");
        let end_line = text.lines().last().expect("at least one line");
        assert!(end_line.starts_with("{\"end\":"));
        assert!(
            end_line.contains("\"lease_handle\":"),
            "end envelope must carry lease_handle: {end_line}"
        );
        assert!(
            !end_line.contains("\"lease_id\":"),
            "end envelope must not leak internal lease_id: {end_line}"
        );
        // Extract handle value and check shape.
        let needle = "\"lease_handle\":\"";
        let start = end_line.find(needle).expect("lease_handle field") + needle.len();
        let rest = &end_line[start..];
        let end = rest.find('"').expect("handle terminator");
        let handle = &rest[..end];
        assert_eq!(
            handle.len(),
            32,
            "lease_handle must be 32-char hex (128 bits): {handle}"
        );
        assert!(handle.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Extract the `lease_handle` value (32-char hex) from an NDJSON
    /// `{"end": …}` envelope. Returns `None` when the envelope does not
    /// carry one (e.g. a non-streaming HTTP error response). Tests use
    /// the handle as a unique key when grepping the shared audit log —
    /// `RedDBOptions::in_memory()` writes `.audit.log` next to the
    /// (unique-per-test) data path's *parent* `/tmp`, so multiple
    /// tests running in the same process share the file.
    fn extract_lease_handle(raw: &[u8]) -> Option<String> {
        let (_head, body) = split_response(raw);
        let decoded = decode_chunked_body(&body);
        let text = String::from_utf8(decoded).ok()?;
        let needle = "\"lease_handle\":\"";
        let start = text.find(needle)? + needle.len();
        let rest = &text[start..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    #[test]
    fn ndjson_stream_emits_open_and_close_audit_events() {
        // Acceptance criterion #5 — every state transition produces
        // an audit event. After a successful stream we expect both a
        // `stream.opened` and a `stream.closed` line carrying our
        // stream's unique lease_handle, with `reason=ok`. We filter
        // by handle (not by action count) because the audit file is
        // shared across in-process tests.
        let server = fresh_server();
        let raw = ndjson_query(&server, "SELECT 1 as n");
        let handle = extract_lease_handle(&raw).expect("end envelope carries handle");
        let body = read_audit_log(&server);
        let opened: Vec<&str> = body
            .lines()
            .filter(|l| {
                l.contains("\"action\":\"stream.opened\"")
                    && l.contains(&format!("\"lease_handle\":\"{handle}\""))
            })
            .collect();
        let closed: Vec<&str> = body
            .lines()
            .filter(|l| {
                l.contains("\"action\":\"stream.closed\"")
                    && l.contains(&format!("\"lease_handle\":\"{handle}\""))
            })
            .collect();
        assert_eq!(
            opened.len(),
            1,
            "expected exactly one stream.opened for handle {handle}"
        );
        assert_eq!(
            closed.len(),
            1,
            "expected exactly one stream.closed for handle {handle}"
        );
        assert!(
            opened[0].contains("\"snapshot_lsn\":") && opened[0].contains("\"query_hash\":"),
            "stream.opened missing required fields: {}",
            opened[0]
        );
        assert!(
            closed[0].contains("\"reason\":\"ok\""),
            "stream.closed must record reason=ok: {}",
            closed[0]
        );
    }

    #[test]
    fn ndjson_resume_refuses_on_prefix_hash_mismatch() {
        // Acceptance criterion #4 — wrong prefix_hash returns
        // `prefix_hash_mismatch` and delivers zero rows.
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows766_mis (id INTEGER PRIMARY KEY, name TEXT)",
        );
        ddl(
            &server,
            "INSERT INTO rows766_mis (id, name) VALUES (1, 'a'), (2, 'b'), (3, 'c')",
        );

        let raw = ndjson_query(&server, "SELECT id, name FROM rows766_mis");
        let lines = decode_ndjson(&raw);
        let open_ack = lines.first().expect("open_ack");
        let snapshot_lsn: u64 = extract_json_field(open_ack, "snapshot_lsn")
            .expect("snapshot_lsn")
            .parse()
            .expect("integer");
        let first_row = lines
            .iter()
            .find(|l| l.starts_with("{\"row\":"))
            .expect("first row");
        let resume_after_rid: u64 = extract_json_field(first_row, "rid")
            .expect("rid")
            .parse()
            .expect("u64");

        let body = format!(
            "{{\"query\":\"SELECT id, name FROM rows766_mis\",\"resume\":{{\"snapshot_lsn\":{snapshot_lsn},\"resume_after_rid\":{resume_after_rid},\"prefix_hash\":\"{}\"}}}}",
            "deadbeef".repeat(8)
        );
        let raw = ndjson_query_body(&server, body.into_bytes());
        let lines = decode_ndjson(&raw);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("{\"error\":") && l.contains("prefix_hash_mismatch")),
            "expected prefix_hash_mismatch envelope: {lines:?}"
        );
        let rows_emitted = lines.iter().filter(|l| l.starts_with("{\"row\":")).count();
        assert_eq!(
            rows_emitted, 0,
            "no rows must be delivered on hash mismatch: {lines:?}"
        );
        let end_line = lines.last().expect("end");
        assert!(
            end_line.contains("\"row_count\":0"),
            "end must show 0 rows: {end_line}"
        );
    }

    #[test]
    fn ndjson_capacity_refusal_emits_capacity_refused_audit_event() {
        // Acceptance criterion #5 — capacity-refused is a state
        // transition; the audit log must carry a `stream.closed` row
        // with `reason: capacity_refused` even though no lease was
        // ever issued.
        let server = fresh_server();
        server
            .runtime
            .execute_query("SET CONFIG stream.max_global = 1")
            .expect("set cap");
        server
            .runtime
            .execute_query("SET CONFIG stream.max_per_principal = 1")
            .expect("set cap");
        // Hold the only slot so the next open is refused.
        let cap_registry = Arc::clone(server.stream_capacity());
        let _hold = cap_registry.try_acquire("anon", 1, 1).unwrap();

        let request = ndjson_request_with_bearer("SELECT 1", None);
        let raw = dispatch_streaming(&server, &request);
        assert!(String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 429"));

        let body = read_audit_log(&server);
        assert!(
            body.lines().any(|l| {
                l.contains("\"action\":\"stream.closed\"")
                    && l.contains("\"reason\":\"capacity_refused\"")
            }),
            "audit log missing capacity_refused close event: {body}"
        );
    }

    #[test]
    fn ndjson_resume_refuses_on_expired_snapshot() {
        // Acceptance criterion #3 — an expired snapshot lease (no
        // entry / past TTL) refuses with `snapshot_expired`. We seed
        // the registry with a stale opened_at_ms so the TTL has
        // already elapsed.
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows766_exp (id INTEGER PRIMARY KEY, name TEXT)",
        );
        ddl(
            &server,
            "INSERT INTO rows766_exp (id, name) VALUES (1, 'a'), (2, 'b')",
        );

        // Seed a snapshot LSN we'll claim to resume from, with
        // opened_at_ms = 0 (way in the past) and ttl = 1ms so any
        // wall-clock check expires it.
        let bogus_snapshot_lsn: u64 = 1;
        server.lease_registry().record(bogus_snapshot_lsn, 0, 1);

        let body = format!(
            "{{\"query\":\"SELECT id, name FROM rows766_exp\",\"resume\":{{\"snapshot_lsn\":{bogus_snapshot_lsn},\"resume_after_rid\":0,\"prefix_hash\":\"{}\"}}}}",
            "0".repeat(64)
        );
        let raw = ndjson_query_body(&server, body.into_bytes());
        let lines = decode_ndjson(&raw);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("{\"error\":") && l.contains("snapshot_expired")),
            "expected snapshot_expired envelope: {lines:?}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.starts_with("{\"row\":")).count(),
            0,
            "no rows must be delivered on snapshot expiry: {lines:?}"
        );
    }

    #[test]
    fn ndjson_token_expired_during_lease_audit_emitted_when_jwt_exp_is_short() {
        // Acceptance criterion #4 — a JWT bearer whose `exp` lands
        // inside the lease's snapshot-TTL window triggers the dedicated
        // audit event exactly once per stream, with `lease_continued`
        // = true. The stream itself still completes successfully —
        // the lease, not the token, governs subsequent chunks (PRD
        // #759 lease-decoupling property).
        let server = fresh_server();
        // Synthetic JWT — header.payload.signature where payload
        // sets `exp: 1` (Unix epoch, well in the past). Auth is not
        // configured on this server, so the bearer is unverified;
        // but the audit emit consults the `exp` claim purely for
        // bookkeeping.
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJleHAiOjF9.sig";
        let request = ndjson_request_with_bearer("SELECT 1 as n", Some(token));
        let raw = dispatch_streaming(&server, &request);
        assert!(
            String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 200"),
            "stream must complete despite bearer expiry: {}",
            String::from_utf8_lossy(&raw)
        );
        let handle = extract_lease_handle(&raw).expect("end envelope carries handle");

        let body = read_audit_log(&server);
        let token_events: Vec<&str> = body
            .lines()
            .filter(|l| {
                l.contains("\"action\":\"stream.token_expired_during_lease\"")
                    && l.contains(&format!("\"lease_handle\":\"{handle}\""))
            })
            .collect();
        assert_eq!(
            token_events.len(),
            1,
            "expected exactly one token-expired event for handle {handle}"
        );
        assert!(
            token_events[0].contains("\"lease_continued\":true"),
            "token-expired event must mark lease_continued=true: {}",
            token_events[0]
        );
        // And we still get the open/close pair for this handle.
        assert!(body
            .lines()
            .any(|l| l.contains("\"action\":\"stream.opened\"")
                && l.contains(&format!("\"lease_handle\":\"{handle}\""))));
        assert!(body
            .lines()
            .any(|l| l.contains("\"action\":\"stream.closed\"")
                && l.contains(&format!("\"lease_handle\":\"{handle}\""))));
    }

    #[test]
    fn ndjson_opaque_token_does_not_emit_token_expired_event() {
        // Counterpoint — an opaque (non-JWT) bearer carries no `exp`
        // claim so the audit emitter has nothing to record. The
        // detector must not false-positive on api-key shapes.
        let server = fresh_server();
        let request = ndjson_request_with_bearer("SELECT 1 as n", Some("opaque-api-key"));
        let raw = dispatch_streaming(&server, &request);
        assert!(String::from_utf8_lossy(&raw).starts_with("HTTP/1.1 200"));
        let handle = extract_lease_handle(&raw).expect("end envelope carries handle");
        let body = read_audit_log(&server);
        // Filter strictly by our stream's handle — the shared audit
        // log may carry token-expired rows from other tests, but
        // none should match this lease handle.
        assert!(
            !body.lines().any(|l| {
                l.contains("\"action\":\"stream.token_expired_during_lease\"")
                    && l.contains(&format!("\"lease_handle\":\"{handle}\""))
            }),
            "opaque tokens must not trigger the token-expired audit for this handle: {body}"
        );
    }

    #[test]
    fn ndjson_resume_refuses_on_non_resumable_query() {
        // Acceptance criterion #5 — attempting resume on a
        // non-resumable plan returns `not_resumable`. The check fires
        // before the lease lookup, so a bogus snapshot_lsn does not
        // shadow the not_resumable code.
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows766_nores (id INTEGER PRIMARY KEY, name TEXT)",
        );
        ddl(
            &server,
            "INSERT INTO rows766_nores (id, name) VALUES (1, 'a')",
        );

        let body = format!(
            "{{\"query\":\"SELECT COUNT(*) FROM rows766_nores\",\"resume\":{{\"snapshot_lsn\":1,\"resume_after_rid\":0,\"prefix_hash\":\"{}\"}}}}",
            "0".repeat(64)
        );
        let raw = ndjson_query_body(&server, body.into_bytes());
        let lines = decode_ndjson(&raw);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("{\"error\":") && l.contains("not_resumable")),
            "expected not_resumable envelope: {lines:?}"
        );
    }

    #[test]
    fn ndjson_lease_outlives_short_jwt_exp_acceptance_criterion_one() {
        // Acceptance criterion #1 — a token expiring mid-flight does
        // not terminate the stream. In the shim slice, materialisation
        // already de facto decouples the bearer from chunk delivery;
        // this test pins that property at the wire layer. The stream
        // must complete with `reason=ok` even though the JWT `exp` is
        // already in the past at the moment the open call returns.
        let server = fresh_server();
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJleHAiOjF9.sig";
        let request = ndjson_request_with_bearer("SELECT 1 as n", Some(token));
        let raw = dispatch_streaming(&server, &request);
        let handle = extract_lease_handle(&raw).expect("end envelope carries handle");
        let (_head, http_body) = split_response(&raw);
        let decoded = decode_chunked_body(&http_body);
        let text = String::from_utf8(decoded).unwrap();
        let end_line = text.lines().last().unwrap_or("");
        assert!(
            end_line.starts_with("{\"end\":"),
            "stream must close with end envelope despite token expiry: {text}"
        );
        assert!(
            text.lines().any(|l| l.starts_with("{\"row\":")),
            "row data must still flow when bearer credential is expired: {text}"
        );
        // Close audit reason is `ok` — filter by handle so the shared
        // audit file's other rows don't break the assertion.
        let audit = read_audit_log(&server);
        let close = audit
            .lines()
            .find(|l| {
                l.contains("\"action\":\"stream.closed\"")
                    && l.contains(&format!("\"lease_handle\":\"{handle}\""))
            })
            .expect("close event recorded");
        assert!(
            close.contains("\"reason\":\"ok\""),
            "lease must close cleanly when token expired mid-flight: {close}"
        );
    }

    #[test]
    fn ndjson_input_stream_emits_open_and_close_audit_events() {
        // Symmetric coverage for the input-stream surface. A
        // successful insert path produces the same opened/closed
        // pair, distinguishable from output streams only by the
        // executed query (recorded via query_hash).
        let server = fresh_server();
        ddl(
            &server,
            "CREATE TABLE rows767 (id INTEGER PRIMARY KEY, name TEXT)",
        );
        let body = concat!(
            "{\"open\":{\"target\":\"rows767\",\"columns\":[\"id\",\"name\"]}}\n",
            "{\"row\":{\"id\":1,\"name\":\"alice\"}}\n",
        );
        let _ = dispatch_input_stream(&server, body);
        let log = read_audit_log(&server);
        assert!(
            log.lines()
                .any(|l| l.contains("\"action\":\"stream.opened\"")),
            "input stream missing stream.opened: {log}"
        );
        assert!(
            log.lines().any(|l| {
                l.contains("\"action\":\"stream.closed\"") && l.contains("\"reason\":\"ok\"")
            }),
            "input stream missing stream.closed ok: {log}"
        );
    }
}

/// Issue #524 — `GET /collections/:name/chain-tip`. Returns the cached chain
/// tip JSON. 404 when the collection is not a `KIND blockchain` or has no
/// rows yet (the engine guarantees a genesis row on creation, so the 404
/// branch effectively means "wrong kind / collection absent").
fn handle_chain_tip(runtime: &crate::runtime::RedDBRuntime, collection: &str) -> HttpResponse {
    let Some(tip) = runtime.chain_tip_for_collection(collection) else {
        return json_error(
            404,
            format!("chain-tip: collection '{collection}' is not a blockchain or has no rows"),
        );
    };
    let server_time = crate::runtime::blockchain_kind::now_ms();
    let mut hex = String::with_capacity(64);
    for b in tip.hash.iter() {
        hex.push_str(&format!("{b:02x}"));
    }
    let mut obj = crate::json::Map::new();
    obj.insert(
        "block_height".to_string(),
        crate::json::Value::Number(tip.height as f64),
    );
    obj.insert("hash".to_string(), crate::json::Value::String(hex));
    obj.insert(
        "timestamp".to_string(),
        crate::json::Value::Number(tip.timestamp_ms as f64),
    );
    obj.insert(
        "server_time".to_string(),
        crate::json::Value::Number(server_time as f64),
    );
    json_response(200, crate::json::Value::Object(obj))
}

/// Issue #525 — admin-token gate for verify-chain + clear-integrity endpoints.
/// When `RED_ADMIN_TOKEN` is unset the endpoints stay open (dev installs).
/// When set, callers must present a matching `Authorization: Bearer <token>`.
fn admin_token_ok(headers: &BTreeMap<String, String>) -> bool {
    let Some(expected) = read_admin_token() else {
        return true;
    };
    let presented = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    crate::crypto::constant_time_eq(presented.as_bytes(), expected.as_bytes())
}

/// Issue #525 — `POST /collections/:name/verify-chain`.
fn handle_verify_chain(runtime: &crate::runtime::RedDBRuntime, collection: &str) -> HttpResponse {
    let Some(outcome) = runtime.verify_chain_for_collection(collection) else {
        return json_error(
            404,
            format!(
                "verify-chain: collection '{collection}' is not a blockchain or does not exist"
            ),
        );
    };
    let mut obj = crate::json::Map::new();
    obj.insert(
        "checked".to_string(),
        crate::json::Value::Number(outcome.checked as f64),
    );
    obj.insert("ok".to_string(), crate::json::Value::Bool(outcome.ok));
    obj.insert(
        "first_bad_height".to_string(),
        match outcome.first_bad_height {
            Some(h) => crate::json::Value::Number(h as f64),
            None => crate::json::Value::Null,
        },
    );
    json_response(200, crate::json::Value::Object(obj))
}

/// Issue #525 — `POST /collections/:name/clear-integrity-flag`.
fn handle_clear_integrity_flag(
    runtime: &crate::runtime::RedDBRuntime,
    collection: &str,
) -> HttpResponse {
    if !runtime.clear_chain_integrity_flag(collection) {
        return json_error(
            404,
            format!(
                "clear-integrity-flag: collection '{collection}' is not a blockchain or does not exist"
            ),
        );
    }
    let mut obj = crate::json::Map::new();
    obj.insert("ok".to_string(), crate::json::Value::Bool(true));
    obj.insert(
        "collection".to_string(),
        crate::json::Value::String(collection.to_string()),
    );
    json_response(200, crate::json::Value::Object(obj))
}

fn collection_from_scan_path(path: &str) -> Option<&str> {
    let prefix = "/collections/";
    let suffix = "/scan";
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let collection = trimmed.trim_matches('/');
    if collection.is_empty() {
        None
    } else {
        Some(collection)
    }
}

fn collection_from_action_path<'a>(path: &'a str, action: &str) -> Option<&'a str> {
    let prefix = "/collections/";
    let suffix = format!("/{action}");
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(&suffix)?;
    let collection = trimmed.trim_matches('/');
    if collection.is_empty() {
        None
    } else {
        Some(collection)
    }
}

fn collection_entity_path(path: &str) -> Option<(&str, u64)> {
    let prefix = "/collections/";
    let suffix = "/entities/";
    let trimmed = path.strip_prefix(prefix)?;
    let (collection, id) = trimmed.split_once(suffix)?;
    let collection = collection.trim_matches('/');
    let id = id.trim_matches('/').parse::<u64>().ok()?;
    if collection.is_empty() {
        None
    } else {
        Some((collection, id))
    }
}

fn collection_tree_bare_path(path: &str) -> Option<(&str, &str)> {
    let prefix = "/collections/";
    let trimmed = path.strip_prefix(prefix)?;
    let (collection, tree_name) = trimmed.split_once("/trees/")?;
    let collection = collection.trim_matches('/');
    let tree_name = tree_name.trim_matches('/');
    if collection.is_empty() || tree_name.is_empty() || tree_name.contains('/') {
        None
    } else {
        Some((collection, tree_name))
    }
}

fn collection_tree_action_path<'a>(path: &'a str, action: &str) -> Option<(&'a str, &'a str)> {
    let prefix = "/collections/";
    let suffix = format!("/{action}");
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(&suffix)?;
    let (collection, tree_name) = trimmed.split_once("/trees/")?;
    let collection = collection.trim_matches('/');
    let tree_name = tree_name.trim_matches('/');
    if collection.is_empty() || tree_name.is_empty() || tree_name.contains('/') {
        None
    } else {
        Some((collection, tree_name))
    }
}

fn collection_tree_node_path(path: &str) -> Option<(&str, &str, u64)> {
    let prefix = "/collections/";
    let trimmed = path.strip_prefix(prefix)?;
    let (head, node_id) = trimmed.split_once("/nodes/")?;
    let (collection, tree_name) = head.split_once("/trees/")?;
    let collection = collection.trim_matches('/');
    let tree_name = tree_name.trim_matches('/');
    let node_id = node_id.trim_matches('/').parse::<u64>().ok()?;
    if collection.is_empty() || tree_name.is_empty() || tree_name.contains('/') {
        None
    } else {
        Some((collection, tree_name, node_id))
    }
}

fn index_named_action_path(path: &str, action: &str) -> Option<String> {
    let prefix = "/indexes/";
    let suffix = format!("/{action}");
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(&suffix)?;
    let name = trimmed.trim_matches('/');
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn collection_from_native_vector_artifact_path(path: &str) -> Option<&str> {
    let prefix = "/physical/native-vector-artifacts/";
    let trimmed = path.strip_prefix(prefix)?;
    let collection = trimmed.trim_matches('/');
    if collection.is_empty() || collection.contains('/') {
        None
    } else {
        Some(collection)
    }
}

fn collection_from_native_vector_artifact_warmup_path(path: &str) -> Option<&str> {
    let prefix = "/physical/native-vector-artifacts/";
    let suffix = "/warmup";
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let collection = trimmed.trim_matches('/');
    if collection.is_empty() || collection.contains('/') {
        None
    } else {
        Some(collection)
    }
}

fn graph_projection_named_action_path(path: &str, action: &str) -> Option<String> {
    let prefix = "/graph/projections/";
    let suffix = format!("/{action}");
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(&suffix)?;
    let name = trimmed.trim_matches('/');
    if name.is_empty() || name.contains('/') {
        None
    } else {
        Some(name.to_string())
    }
}

/// Match `/collections/:name/kvs/invalidate_tags`.
fn collection_kv_invalidate_tags_path(path: &str) -> Option<&str> {
    let prefix = "/collections/";
    let trimmed = path.strip_prefix(prefix)?;
    let collection = trimmed.strip_suffix("/kvs/invalidate_tags")?;
    let collection = collection.trim_matches('/');
    if collection.is_empty() {
        None
    } else {
        Some(collection)
    }
}

/// Match `/collections/:name/kvs/:key` to extract collection and key.
fn collection_kv_path(path: &str) -> Option<(String, String)> {
    let prefix = "/collections/";
    let trimmed = path.strip_prefix(prefix)?;
    let (collection, rest) = trimmed.split_once("/kvs/")?;
    let collection = collection.trim_matches('/');
    let key = rest.trim_matches('/');
    if collection.is_empty() || key.is_empty() {
        None
    } else {
        let collection = percent_decode_path_segment(collection).ok()?;
        let key = percent_decode_path_segment(key).ok()?;
        Some((collection, key))
    }
}

/// Match `/collections/:name/kv/:key/watch` or `/collections/:name/kvs/:key/watch`.
fn collection_kv_watch_path(path: &str) -> Option<(String, String)> {
    let prefix = "/collections/";
    let trimmed = path.strip_prefix(prefix)?;
    let (collection, rest) = trimmed
        .split_once("/kv/")
        .or_else(|| trimmed.split_once("/kvs/"))?;
    let key = rest.strip_suffix("/watch")?.trim_matches('/');
    let collection = collection.trim_matches('/');
    if collection.is_empty() || key.is_empty() {
        None
    } else {
        let collection = percent_decode_path_segment(collection).ok()?;
        let key = percent_decode_path_segment(key).ok()?;
        Some((collection, key))
    }
}

/// Match `/collections/:name/schema` to extract the collection name.
fn collection_from_schema_path(path: &str) -> Option<&str> {
    let prefix = "/collections/";
    let suffix = "/schema";
    let trimmed = path.strip_prefix(prefix)?.strip_suffix(suffix)?;
    let name = trimmed.trim_matches('/');
    if name.is_empty() || name.contains('/') {
        None
    } else {
        Some(name)
    }
}

/// Match bare `/collections/:name` (no further sub-path) to extract the collection name.
fn collection_from_bare_path(path: &str) -> Option<&str> {
    let prefix = "/collections/";
    let trimmed = path.strip_prefix(prefix)?;
    let name = trimmed.trim_matches('/');
    if name.is_empty() || name.contains('/') {
        None
    } else {
        Some(name)
    }
}

/// Issue #760 — does the request's `Accept` header opt in to NDJSON
/// output streaming? Matches the canonical `application/x-ndjson` and
/// the two common alternates (`application/ndjson`, `text/ndjson`),
/// case-insensitively, ignoring `q=` weight suffixes. A plain
/// `application/json` Accept is *not* a streaming opt-in — that
/// continues to ride the materialising one-shot path.
/// Issue #763 — does the request carry an NDJSON body? Used to gate
/// the input-stream entry point. Matches `application/x-ndjson` and
/// the same alternates accepted by [`wants_ndjson_response`].
pub(crate) fn content_type_is_ndjson(headers: &BTreeMap<String, String>) -> bool {
    let Some(raw) = headers.get("content-type") else {
        return false;
    };
    let token = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    matches!(
        token.as_str(),
        "application/x-ndjson" | "application/ndjson" | "text/ndjson"
    )
}

pub(crate) fn wants_ndjson_response(headers: &BTreeMap<String, String>) -> bool {
    let Some(raw) = headers.get("accept") else {
        return false;
    };
    raw.split(',').any(|part| {
        let token = part
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        matches!(
            token.as_str(),
            "application/x-ndjson" | "application/ndjson" | "text/ndjson"
        )
    })
}

/// PLAN.md Phase 4.4 — derive a stable principal label from a
/// request's headers. Bearer tokens are hashed (sha256 prefix) so
/// the metrics label never leaks the raw token. Unauthenticated
/// requests share the `anon` bucket; replica RPCs that pass a
/// `x-reddb-replica-id` header get their own `replica:<id>` bucket.
pub(crate) fn principal_for(headers: &BTreeMap<String, String>) -> String {
    if let Some(replica) = headers.get("x-reddb-replica-id") {
        let trimmed = replica.trim();
        if !trimmed.is_empty() {
            return format!("replica:{trimmed}");
        }
    }
    if let Some(auth) = headers.get("authorization") {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            let trimmed = token.trim();
            if !trimmed.is_empty() {
                let digest = crate::crypto::sha256(trimmed.as_bytes());
                return format!("bearer:{}", crate::utils::to_hex_prefix(&digest, 8));
            }
        }
    }
    "anon".to_string()
}

/// Issue #761 / S2 — build the structured 429 refusal handed back when
/// `try_route_streaming` cannot acquire a stream slot. Both cap-fired
/// codes share the same response shape so clients can branch on the
/// `code` field without re-parsing.
///
/// Wire shape:
///   `Retry-After: <seconds>`  header
///   `{"ok": false, "error": {"code": <code>, "limit": N, "current": M,
///                            "principal": "..."?}}`
/// Issue #767 / S8 — record a `stream.closed { reason: capacity_refused }`
/// audit event whenever the capacity guard refuses an open. The brief
/// requires audit coverage of every state transition; capacity_refused
/// is the only one that never produces a lease handle.
fn emit_capacity_refused_audit(
    runtime: &crate::runtime::RedDBRuntime,
    principal: &str,
    err: &crate::server::output_stream::AcquireError,
) {
    use crate::server::output_stream::AcquireError;
    let (limit, current) = match err {
        AcquireError::GlobalExhausted { limit, current } => (*limit, *current),
        AcquireError::PrincipalExhausted { limit, current, .. } => (*limit, *current),
    };
    crate::server::output_stream::audit_stream_capacity_refused(
        runtime,
        principal,
        err.code(),
        limit,
        current,
    );
}

pub(crate) fn stream_capacity_refusal_response(
    err: &crate::server::output_stream::AcquireError,
) -> HttpResponse {
    use crate::json::{Map, Value as JsonValue};
    use crate::server::output_stream::AcquireError;

    let mut error_obj = Map::new();
    error_obj.insert(
        "code".to_string(),
        JsonValue::String(err.code().to_string()),
    );
    error_obj.insert(
        "message".to_string(),
        crate::json_field::SerializedJsonField::tainted(&err.message()),
    );
    match err {
        AcquireError::GlobalExhausted { limit, current } => {
            error_obj.insert("limit".to_string(), JsonValue::Number(*limit as f64));
            error_obj.insert("current".to_string(), JsonValue::Number(*current as f64));
        }
        AcquireError::PrincipalExhausted {
            principal,
            limit,
            current,
        } => {
            error_obj.insert(
                "principal".to_string(),
                crate::json_field::SerializedJsonField::tainted(principal),
            );
            error_obj.insert("limit".to_string(), JsonValue::Number(*limit as f64));
            error_obj.insert("current".to_string(), JsonValue::Number(*current as f64));
        }
    }
    let mut root = Map::new();
    root.insert("ok".to_string(), JsonValue::Bool(false));
    root.insert("error".to_string(), JsonValue::Object(error_obj));

    let mut response = json_response(429, JsonValue::Object(root));
    // `Retry-After: 1` (seconds) — a stream slot frees the moment any
    // other stream ends, so a one-second hint matches the actual
    // recovery cadence; the structured `current`/`limit` fields are
    // the load-bearing signal for clients with smarter backoff.
    if let Ok(retry_after) =
        crate::server::header_escape_guard::HeaderEscapeGuard::header_value("1")
    {
        response = response.with_header("Retry-After", retry_after);
    }
    response
}

/// PLAN.md Phase 6.1 — read the operator admin token. Honors
/// `RED_ADMIN_TOKEN` and the `RED_ADMIN_TOKEN_FILE` companion via
/// the shared `crate::utils::env_with_file_fallback` helper.
pub(crate) fn read_admin_token() -> Option<String> {
    crate::utils::env_with_file_fallback("RED_ADMIN_TOKEN")
}
