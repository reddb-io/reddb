use super::*;
use std::sync::{Mutex, OnceLock};

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
        if !matches!(
            (request.method.as_str(), request.path.as_str()),
            ("POST", "/query")
        ) || !is_stream_ask_query_body(&request.body)
        {
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
                self.handle_query_sse_stream(request.body.clone(), writer)?;
                Ok(true)
            }
        }
    }

    pub(crate) fn route(&self, request: HttpRequest) -> HttpResponse {
        let HttpRequest {
            method,
            path,
            query,
            headers,
            body,
        } = request;

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
            ("GET", "/ec/status") => handlers_ec::handle_ec_global_status(&self.runtime),

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
            ("GET", "/backup/status") => self.handle_backup_status(),
            ("POST", "/backup/trigger") => self.handle_backup_trigger(),
            ("GET", "/recovery/restore-points") => self.handle_restore_points(),

            // Replication endpoints
            ("GET", "/replication/status") => self.handle_replication_status(),
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
            ("GET", "/admin/blob_cache/stats") => self.handle_admin_blob_cache_stats(&query),
            // PLAN.md Phase 11.6 — manual replica → primary promotion.
            ("POST", "/admin/failover/promote") => self.handle_admin_failover_promote(body),
            // PLAN.md Phase 5.1 / 5.4 — observability endpoints.
            ("GET", "/metrics") => self.handle_metrics(),
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
            ("GET", "/admin/status") => self.handle_admin_status(),
            // Red UI cluster status snapshot (#738) — single aggregated
            // contract so the UI doesn't need to stitch /admin/status,
            // /replication/status, /backup/status, /ready/* together.
            ("GET", "/cluster/status") => self.handle_cluster_status(),
            // SOC 2 / HIPAA structured audit query — JSONL/JSON over
            // the active `.audit.log` plus rotated archives.
            ("GET", "/admin/audit") => self.handle_admin_audit_query(&query),

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
                        return self.handle_describe_collection(collection);
                    }
                    if let Some(collection) = collection_from_scan_path(&path) {
                        return self.handle_scan(collection, &query);
                    }
                    if let Some((collection, id)) = collection_entity_path(&path) {
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
                        return self.handle_bulk_create(
                            collection,
                            body,
                            Self::handle_create_document,
                        );
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/rows") {
                        return self.handle_bulk_create_rows_fast(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/nodes") {
                        return self.handle_bulk_create(collection, body, Self::handle_create_node);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/edges") {
                        return self.handle_bulk_create(collection, body, Self::handle_create_edge);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "bulk/vectors") {
                        return self.handle_bulk_create(
                            collection,
                            body,
                            Self::handle_create_vector,
                        );
                    }
                    if let Some(collection) = collection_from_action_path(&path, "rows") {
                        return self.handle_create_row(collection, body);
                    }
                    // Issue #582 — Analytics slice 4. BatchInsertEndpoint
                    // accepts an array body, enforces all-or-nothing
                    // commit + AnalyticsSchemaRegistry validation, and
                    // dedups by `Idempotency-Key` header.
                    if let Some(collection) = collection_from_action_path(&path, "batch") {
                        let idempotency_key =
                            headers.get("idempotency-key").map(|value| value.as_str());
                        return self.handle_batch_insert(collection, body, idempotency_key);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "nodes") {
                        return self.handle_create_node(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "edges") {
                        return self.handle_create_edge(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "vectors") {
                        return self.handle_create_vector(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "documents") {
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

/// PLAN.md Phase 4.4 — derive a stable principal label from a
/// request's headers. Bearer tokens are hashed (sha256 prefix) so
/// the metrics label never leaks the raw token. Unauthenticated
/// requests share the `anon` bucket; replica RPCs that pass a
/// `x-reddb-replica-id` header get their own `replica:<id>` bucket.
fn principal_for(headers: &BTreeMap<String, String>) -> String {
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

/// PLAN.md Phase 6.1 — read the operator admin token. Honors
/// `RED_ADMIN_TOKEN` and the `RED_ADMIN_TOKEN_FILE` companion via
/// the shared `crate::utils::env_with_file_fallback` helper.
fn read_admin_token() -> Option<String> {
    crate::utils::env_with_file_fallback("RED_ADMIN_TOKEN")
}
