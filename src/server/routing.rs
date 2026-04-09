use super::*;

impl RedDBServer {
    pub(crate) fn route(&self, request: HttpRequest) -> HttpResponse {
        let HttpRequest {
            method,
            path,
            query,
            headers,
            body,
        } = request;

        if !self.is_authorized(&method, &path, &headers) {
            return json_error(401, "unauthorized");
        }

        match (method.as_str(), path.as_str()) {
            // Auth endpoints
            ("POST", "/auth/bootstrap") => return self.handle_auth_bootstrap(body),
            ("POST", "/auth/login") => return self.handle_auth_login(body),
            ("POST", "/auth/users") => return self.handle_auth_create_user(body),
            ("GET", "/auth/users") => return self.handle_auth_list_users(),
            ("POST", "/auth/api-keys") => return self.handle_auth_create_api_key(body),
            ("POST", "/auth/change-password") => return self.handle_auth_change_password(body),
            ("GET", "/auth/whoami") => return self.handle_auth_whoami(&headers),

            // Replication endpoints
            ("GET", "/replication/status") => return self.handle_replication_status(),
            ("POST", "/replication/snapshot") => return self.handle_replication_snapshot(),

            ("GET", "/health") => {
                let report = self.native_use_cases().health();
                let status = if report.is_healthy() { 200 } else { 503 };
                json_response(status, crate::presentation::ops_json::health_json(&report))
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
                let status = if report.is_healthy() { 200 } else { 503 };
                json_response(status, crate::presentation::ops_json::health_json(&report))
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
                json_response(
                    200,
                    crate::presentation::catalog_json::catalog_collection_readiness_json(
                        &catalog.collections,
                    ),
                )
            }
            ("GET", "/catalog/collections/readiness/attention") => json_response(
                200,
                crate::presentation::catalog_json::catalog_collection_attention_json(
                    &self.catalog_use_cases().collection_attention(),
                ),
            ),
            ("GET", "/catalog/consistency") => json_response(
                200,
                crate::presentation::catalog_json::catalog_consistency_json(
                    &self.catalog_use_cases().consistency_report(),
                ),
            ),
            ("GET", "/catalog/indexes/declared") => json_response(
                200,
                crate::presentation::admin_json::indexes_json(
                    &self.catalog_use_cases().declared_indexes(),
                ),
            ),
            ("GET", "/catalog/indexes/operational") => json_response(
                200,
                crate::presentation::admin_json::indexes_json(&self.catalog_use_cases().indexes()),
            ),
            ("GET", "/catalog/indexes/status") => json_response(
                200,
                crate::presentation::catalog_json::catalog_index_statuses_json(
                    &self.catalog_use_cases().index_statuses(),
                ),
            ),
            ("GET", "/catalog/indexes/attention") => json_response(
                200,
                crate::presentation::catalog_json::catalog_index_attention_json(
                    &self.catalog_use_cases().index_attention(),
                ),
            ),
            ("GET", "/catalog/graph/projections/declared") => {
                match self.catalog_use_cases().graph_projections() {
                    Ok(projections) => json_response(
                        200,
                        crate::presentation::admin_json::graph_projections_json(&projections),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/catalog/graph/projections/operational") => json_response(
                200,
                crate::presentation::admin_json::graph_projections_json(
                    &self.catalog_use_cases().operational_graph_projections(),
                ),
            ),
            ("GET", "/catalog/graph/projections/status") => json_response(
                200,
                crate::presentation::catalog_json::catalog_graph_projection_statuses_json(
                    &self.catalog_use_cases().graph_projection_statuses(),
                ),
            ),
            ("GET", "/catalog/graph/projections/attention") => json_response(
                200,
                crate::presentation::catalog_json::catalog_graph_projection_attention_json(
                    &self.catalog_use_cases().graph_projection_attention(),
                ),
            ),
            ("GET", "/catalog/analytics-jobs/declared") => {
                match self.catalog_use_cases().analytics_jobs() {
                    Ok(jobs) => json_response(
                        200,
                        crate::presentation::admin_json::analytics_jobs_json(&jobs),
                    ),
                    Err(err) => json_error(404, err.to_string()),
                }
            }
            ("GET", "/catalog/analytics-jobs/operational") => json_response(
                200,
                crate::presentation::admin_json::analytics_jobs_json(
                    &self.catalog_use_cases().operational_analytics_jobs(),
                ),
            ),
            ("GET", "/catalog/analytics-jobs/status") => json_response(
                200,
                crate::presentation::catalog_json::catalog_analytics_job_statuses_json(
                    &self.catalog_use_cases().analytics_job_statuses(),
                ),
            ),
            ("GET", "/catalog/analytics-jobs/attention") => json_response(
                200,
                crate::presentation::catalog_json::catalog_analytics_job_attention_json(
                    &self.catalog_use_cases().analytics_job_attention(),
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
            ("POST", "/text/search") => self.handle_text_search(body),
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
            ("POST", "/graph/projections") => self.handle_graph_projection_upsert(body),
            ("POST", "/graph/jobs") => self.handle_analytics_job_upsert(body),
            ("POST", "/graph/jobs/queue") => self.handle_analytics_job_queue(body),
            ("POST", "/graph/jobs/start") => self.handle_analytics_job_start(body),
            ("POST", "/graph/jobs/complete") => self.handle_analytics_job_complete(body),
            ("POST", "/graph/jobs/stale") => self.handle_analytics_job_stale(body),
            ("POST", "/graph/jobs/fail") => self.handle_analytics_job_fail(body),
            _ => {
                if method == "GET" {
                    if let Some(collection) = collection_from_schema_path(&path) {
                        return self.handle_describe_collection(collection);
                    }
                    if let Some(collection) = collection_from_scan_path(&path) {
                        return self.handle_scan(collection, &query);
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
                }
                if method == "POST" {
                    if let Some(collection) = collection_from_action_path(&path, "bulk/rows") {
                        return self.handle_bulk_create(collection, body, Self::handle_create_row);
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
                    if let Some(collection) = collection_from_action_path(&path, "nodes") {
                        return self.handle_create_node(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "edges") {
                        return self.handle_create_edge(collection, body);
                    }
                    if let Some(collection) = collection_from_action_path(&path, "vectors") {
                        return self.handle_create_vector(collection, body);
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
                    // DDL: DELETE /collections/:name
                    if let Some(name) = collection_from_bare_path(&path) {
                        return self.handle_drop_collection(name);
                    }
                    // Auth: DELETE /auth/users/:username
                    if let Some(username) = path.strip_prefix("/auth/users/") {
                        let username = username.trim_matches('/');
                        if !username.is_empty() {
                            return self.handle_auth_delete_user(username);
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

    fn is_authorized(
        &self,
        method: &str,
        path: &str,
        headers: &BTreeMap<String, String>,
    ) -> bool {
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

        // Extract bearer token from Authorization header.
        let token = headers
            .get("authorization")
            .and_then(|v| v.strip_prefix("Bearer "));

        match token {
            Some(tok) => {
                if let Some((_username, role)) = auth_store.validate_token(tok) {
                    let is_write = !matches!(method, "GET" | "HEAD" | "OPTIONS");
                    if is_write {
                        role.can_write()
                    } else {
                        role.can_read()
                    }
                } else {
                    false
                }
            }
            None => {
                // No token: allow only if require_auth is false.
                !auth_store.config().require_auth
            }
        }
    }
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
