async fn health(&self, _request: Request<Empty>) -> Result<Response<HealthReply>, Status> {
    let report = self.native_use_cases().health();
    Ok(Response::new(HealthReply {
        healthy: report.is_healthy(),
        state: match report.state {
            HealthState::Healthy => "healthy",
            HealthState::Degraded => "degraded",
            HealthState::Unhealthy => "unhealthy",
        }
        .to_string(),
        checked_at_unix_ms: report.checked_at_unix_ms as u64,
    }))
}

async fn ready(&self, _request: Request<Empty>) -> Result<Response<HealthReply>, Status> {
    let report = self.native_use_cases().health();
    Ok(Response::new(HealthReply {
        healthy: report.is_healthy(),
        state: match report.state {
            HealthState::Healthy => "healthy",
            HealthState::Degraded => "degraded",
            HealthState::Unhealthy => "unhealthy",
        }
        .to_string(),
        checked_at_unix_ms: report.checked_at_unix_ms as u64,
    }))
}

async fn catalog_readiness(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let native = self.native_use_cases();
    let readiness = native.readiness();
    let health = native.health();
    let authority = native.physical_authority_status();
    Ok(Response::new(json_payload_reply(
        crate::presentation::ops_json::catalog_readiness_json(
            readiness.query,
            readiness.write,
            readiness.repair,
            &health,
            &authority,
        ),
    )))
}

async fn deployment_profiles(
    &self,
    request: Request<DeploymentProfileRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let profile = {
        let profile = request.into_inner().profile;
        let normalized = profile.trim().to_lowercase();
        grpc_deployment_profile_from_token(&normalized)
    };
    let payload = match profile {
        Some(profile) => crate::presentation::deployment_json::deployment_profile_json(
            match profile {
                GrpcDeploymentProfile::Embedded => crate::presentation::deployment_json::DeploymentProfileView::Embedded,
                GrpcDeploymentProfile::Server => crate::presentation::deployment_json::DeploymentProfileView::Server,
                GrpcDeploymentProfile::Serverless => crate::presentation::deployment_json::DeploymentProfileView::Serverless,
            },
        ),
        None => crate::presentation::deployment_json::deployment_profiles_catalog_json(
            &[
                crate::presentation::deployment_json::DeploymentProfileView::Embedded,
                crate::presentation::deployment_json::DeploymentProfileView::Server,
                crate::presentation::deployment_json::DeploymentProfileView::Serverless,
            ],
            "Call DeploymentProfiles with profile='serverless' for the exact serverless contract.",
        ),
    };
    Ok(Response::new(json_payload_reply(payload)))
}

async fn stats(&self, _request: Request<Empty>) -> Result<Response<StatsReply>, Status> {
    self.authorize_read(_request.metadata())?;
    let stats = self.catalog_use_cases().stats();
    Ok(Response::new(StatsReply {
        collection_count: stats.store.collection_count as u64,
        total_entities: stats.store.total_entities as u64,
        total_memory_bytes: stats.store.total_memory_bytes as u64,
        cross_ref_count: stats.store.cross_ref_count as u64,
        active_connections: stats.active_connections as u64,
        idle_connections: stats.idle_connections as u64,
        total_checkouts: stats.total_checkouts,
        paged_mode: stats.paged_mode,
        started_at_unix_ms: stats.started_at_unix_ms as u64,
    }))
}

async fn collections(
    &self,
    _request: Request<Empty>,
) -> Result<Response<CollectionsReply>, Status> {
    self.authorize_read(_request.metadata())?;
    Ok(Response::new(CollectionsReply {
        collections: self.catalog_use_cases().collections(),
    }))
}

async fn collection_readiness(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let catalog = self.catalog_use_cases().snapshot();
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_collection_readiness_json(&catalog.collections),
    )))
}

async fn collection_attention(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_collection_attention_json(
            &self.catalog_use_cases().collection_attention(),
        ),
    )))
}

async fn catalog_attention_summary(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_attention_summary_json(
            &self.catalog_use_cases().attention_summary(),
        ),
    )))
}

async fn serverless_attach(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let required =
        grpc_parse_serverless_readiness_requirements(&payload).map_err(Status::invalid_argument)?;
    let readiness = self.native_use_cases().readiness();
    let (query_ready, write_ready, repair_ready) = (
        readiness.query_serverless,
        readiness.write_serverless,
        readiness.repair_serverless,
    );
    let missing = crate::application::serverless_payload::missing_serverless_readiness(
        &required,
        query_ready,
        write_ready,
        repair_ready,
    );
    Ok(Response::new(json_payload_reply(
        crate::presentation::serverless_json::serverless_attach_json(
            &required,
            &missing,
            query_ready,
            write_ready,
            repair_ready,
            grpc_serverless_readiness_summary_to_json(
                query_ready,
                write_ready,
                repair_ready,
                &self.native_use_cases().health(),
                &self.native_use_cases().physical_authority_status(),
            ),
        ),
    )))
}

async fn serverless_warmup(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;

    let force = json_bool_field(&payload, "force").unwrap_or(false);
    let dry_run = json_bool_field(&payload, "dry_run").unwrap_or(false);
    let scopes = grpc_parse_serverless_warmup_scopes(&payload).map_err(Status::invalid_argument)?;
    let readiness = self.native_use_cases().readiness();
    let (query_ready, write_ready, repair_ready) = (
        readiness.query_serverless,
        readiness.write_serverless,
        readiness.repair_serverless,
    );
    let missing = crate::application::serverless_payload::missing_serverless_warmup_preconditions(
        dry_run,
        query_ready,
        write_ready,
        repair_ready,
    );
    if !missing.is_empty() {
        return Err(Status::failed_precondition(format!(
            "warmup precondition not met: {}",
            missing.join(", ")
        )));
    }
    let plan = self.admin_use_cases().build_serverless_warmup_plan(
        &self.catalog_use_cases().index_statuses(),
        &self.catalog_use_cases().graph_projection_statuses(),
        &self.catalog_use_cases().analytics_job_statuses(),
        force,
        scopes.contains(&GrpcServerlessWarmupScope::Indexes),
        scopes.contains(&GrpcServerlessWarmupScope::GraphProjections),
        scopes.contains(&GrpcServerlessWarmupScope::AnalyticsJobs),
        scopes.contains(&GrpcServerlessWarmupScope::NativeArtifacts),
    );

    let mut ready_indexes = Vec::new();
    let mut failed_indexes = Vec::new();
    let mut ready_graph = Vec::new();
    let mut failed_graph = Vec::new();
    let mut ready_jobs = Vec::new();
    let mut failed_jobs = Vec::new();
    let mut native_artifacts = None;
    let mut failures = Vec::new();

    if !dry_run {
        for name in &plan.indexes {
            match self.admin_use_cases().warmup_index(name) {
                Ok(index) => {
                    ready_indexes.push(crate::presentation::admin_json::index_json(&index))
                }
                Err(err) => {
                    let mut failure = Map::new();
                    failure.insert("kind".to_string(), JsonValue::String("index".to_string()));
                    failure.insert("name".to_string(), JsonValue::String(name.clone()));
                    failure.insert("error".to_string(), JsonValue::String(err.to_string()));
                    let failure = JsonValue::Object(failure);
                    failed_indexes.push(failure.clone());
                    failures.push(failure);
                }
            }
        }

        for name in &plan.graph_projections {
            if let Err(err) = self.admin_use_cases().mark_graph_projection_materializing(name) {
                let mut failure = Map::new();
                failure.insert(
                    "kind".to_string(),
                    JsonValue::String("graph_projection".to_string()),
                );
                failure.insert("name".to_string(), JsonValue::String(name.clone()));
                failure.insert("error".to_string(), JsonValue::String(err.to_string()));
                let failure = JsonValue::Object(failure);
                failed_graph.push(failure.clone());
                failures.push(failure);
                continue;
            }

            match self.admin_use_cases().materialize_graph_projection(name) {
                Ok(projection) => ready_graph.push(
                    crate::presentation::admin_json::graph_projection_json(&projection),
                ),
                Err(err) => {
                    let _ = self.admin_use_cases().fail_graph_projection(name);
                    let mut failure = Map::new();
                    failure.insert(
                        "kind".to_string(),
                        JsonValue::String("graph_projection".to_string()),
                    );
                    failure.insert("name".to_string(), JsonValue::String(name.clone()));
                    failure.insert("error".to_string(), JsonValue::String(err.to_string()));
                    let failure = JsonValue::Object(failure);
                    failed_graph.push(failure.clone());
                    failures.push(failure);
                }
            }
        }

        for job in &plan.analytics_jobs {
            let metadata = crate::application::graph_payload::analytics_metadata(vec![(
                "source",
                "serverless_warmup".to_string(),
            )]);
            match self.admin_use_cases().queue_analytics_job(
                job.kind.clone(),
                job.projection.clone(),
                metadata,
            ) {
                Ok(job) => ready_jobs.push(analytics_job_json(&job)),
                Err(err) => {
                    let mut failure = Map::new();
                    failure.insert(
                        "kind".to_string(),
                        JsonValue::String("analytics_job".to_string()),
                    );
                    failure.insert(
                        "id".to_string(),
                        JsonValue::String(match &job.projection {
                            Some(projection) => format!("{}:{}", job.kind, projection),
                            None => job.kind.clone(),
                        }),
                    );
                    failure.insert("error".to_string(), JsonValue::String(err.to_string()));
                    let failure = JsonValue::Object(failure);
                    failed_jobs.push(failure.clone());
                    failures.push(failure);
                }
            }
        }

        if plan.includes_native_artifacts {
            match self.native_use_cases().warmup_vector_artifacts() {
                Ok(batch) => {
                    native_artifacts = Some(JsonValue::Object({
                        let mut object = Map::new();
                        object.insert(
                            "status".to_string(),
                            JsonValue::String("executed".to_string()),
                        );
                        object.insert(
                            "batch".to_string(),
                            crate::presentation::native_state_json::native_vector_artifact_batch_json(
                                &batch,
                            ),
                        );
                        object
                    }));
                }
                Err(err) => {
                    let mut failure = Map::new();
                    failure.insert(
                        "kind".to_string(),
                        JsonValue::String("native_artifacts".to_string()),
                    );
                    failure.insert("error".to_string(), JsonValue::String(err.to_string()));
                    let failure = JsonValue::Object(failure);
                    failures.push(failure);
                }
            };
        }
    }

    if native_artifacts.is_none() && plan.includes_native_artifacts {
        let status = if dry_run { "not_executed" } else { "failed" };
        native_artifacts = Some(JsonValue::Object({
            let mut object = Map::new();
            object.insert("status".to_string(), JsonValue::String(status.to_string()));
            object.insert(
                "error".to_string(),
                JsonValue::String(if dry_run {
                    "dry_run".to_string()
                } else {
                    "warmup failed".to_string()
                }),
            );
            object
        }));
    }

    let mut object = Map::new();
    object.insert("dry_run".to_string(), JsonValue::Bool(dry_run));
    object.insert("force".to_string(), JsonValue::Bool(force));
    object.insert(
        "plan".to_string(),
        crate::presentation::serverless_json::serverless_warmup_plan_json(&plan),
    );
    object.insert(
        "results".to_string(),
        JsonValue::Object({
            let mut object = Map::new();
            object.insert("indexes_ready".to_string(), JsonValue::Array(ready_indexes));
            object.insert(
                "indexes_failed".to_string(),
                JsonValue::Array(failed_indexes),
            );
            object.insert(
                "graph_projections_ready".to_string(),
                JsonValue::Array(ready_graph),
            );
            object.insert(
                "graph_projections_failed".to_string(),
                JsonValue::Array(failed_graph),
            );
            object.insert(
                "analytics_jobs_queued".to_string(),
                JsonValue::Array(ready_jobs),
            );
            object.insert(
                "analytics_jobs_failed".to_string(),
                JsonValue::Array(failed_jobs),
            );
            object.insert(
                "native_artifacts".to_string(),
                native_artifacts.unwrap_or_else(|| JsonValue::Null),
            );
            object
        }),
    );
    object.insert(
        "readiness".to_string(),
        grpc_serverless_readiness_summary_to_json(
            query_ready,
            write_ready,
            repair_ready,
            &self.native_use_cases().health(),
            &self.native_use_cases().physical_authority_status(),
        ),
    );
    object.insert("ok".to_string(), JsonValue::Bool(failures.is_empty()));
    if !failures.is_empty() {
        object.insert("failures".to_string(), JsonValue::Array(failures));
    }
    Ok(Response::new(json_payload_reply(JsonValue::Object(object))))
}

async fn serverless_reclaim(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let dry_run = json_bool_field(&payload, "dry_run").unwrap_or(false);
    let operations =
        grpc_parse_serverless_reclaim_operations(&payload).map_err(Status::invalid_argument)?;
    let readiness = self.native_use_cases().readiness();
    let repair_ready = readiness.repair_serverless;
    if !dry_run && !operations.is_empty() && !repair_ready {
        return Err(Status::failed_precondition(
            "reclaim precondition not met: repair",
        ));
    }

    let mut executed = Vec::new();
    let mut failures = Vec::new();

    if !dry_run {
        for operation in &operations {
            let mut result = Map::new();
            result.insert(
                "operation".to_string(),
                JsonValue::String(operation.clone()),
            );
            match operation.as_str() {
                "maintenance" => match self.native_use_cases().run_maintenance() {
                    Ok(()) => {
                        result.insert("ok".to_string(), JsonValue::Bool(true));
                    }
                    Err(err) => {
                        result.insert("ok".to_string(), JsonValue::Bool(false));
                        result.insert("error".to_string(), JsonValue::String(err.to_string()));
                        failures.push(format!("{operation}: {}", err));
                    }
                },
                "retention" => match self.native_use_cases().apply_retention_policy() {
                    Ok(()) => {
                        result.insert("ok".to_string(), JsonValue::Bool(true));
                    }
                    Err(err) => {
                        result.insert("ok".to_string(), JsonValue::Bool(false));
                        result.insert("error".to_string(), JsonValue::String(err.to_string()));
                        failures.push(format!("{operation}: {}", err));
                    }
                },
                "checkpoint" => match self.native_use_cases().checkpoint() {
                    Ok(()) => {
                        result.insert("ok".to_string(), JsonValue::Bool(true));
                    }
                    Err(err) => {
                        result.insert("ok".to_string(), JsonValue::Bool(false));
                        result.insert("error".to_string(), JsonValue::String(err.to_string()));
                        failures.push(format!("{operation}: {}", err));
                    }
                },
                _ => {}
            }
            executed.push(JsonValue::Object(result));
        }
    }

    let mut object = Map::new();
    object.insert("dry_run".to_string(), JsonValue::Bool(dry_run));
    object.insert(
        "operations".to_string(),
        JsonValue::Array(
            operations
                .iter()
                .map(|op| JsonValue::String(op.clone()))
                .collect(),
        ),
    );
    object.insert(
        "results".to_string(),
        JsonValue::Object({
            let mut object = Map::new();
            if dry_run {
                object.insert(
                    "status".to_string(),
                    JsonValue::String("not_executed".to_string()),
                );
            } else {
                object.insert("executed".to_string(), JsonValue::Array(executed));
                object.insert(
                    "failure_count".to_string(),
                    JsonValue::Number(failures.len() as f64),
                );
            }
            object
        }),
    );
    object.insert(
        "readiness".to_string(),
        grpc_serverless_readiness_summary_to_json(
            readiness.query_serverless,
            readiness.write_serverless,
            readiness.repair_serverless,
            &self.native_use_cases().health(),
            &self.native_use_cases().physical_authority_status(),
        ),
    );
    object.insert("ok".to_string(), JsonValue::Bool(failures.is_empty()));
    Ok(Response::new(json_payload_reply(JsonValue::Object(object))))
}
