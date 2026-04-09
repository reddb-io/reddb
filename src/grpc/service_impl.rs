#[tonic::async_trait]
impl RedDb for GrpcRuntime {
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
async fn catalog_consistency(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_consistency_json(
            &self.catalog_use_cases().consistency_report(),
        ),
    )))
}

async fn physical_metadata(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let metadata = self
        .native_use_cases()
        .physical_metadata()
        .map_err(to_status)?;
    let payload = json_to_string(&metadata.to_json_value()).unwrap_or_else(|_| "{}".to_string());
    Ok(Response::new(PayloadReply { ok: true, payload }))
}

async fn native_header(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let header = self.native_use_cases().native_header().map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_json::native_header_json(header),
    )))
}

async fn native_collection_roots(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let roots = self
        .native_use_cases()
        .native_collection_roots()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_json::collection_roots_json(&roots),
    )))
}

async fn native_manifest_summary(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let summary = self
        .native_use_cases()
        .native_manifest_summary()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_json::native_manifest_summary_json(&summary),
    )))
}

async fn native_registry_summary(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let summary = self
        .native_use_cases()
        .native_registry_summary()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::ops_json::native_registry_summary_json(&summary),
    )))
}

async fn native_recovery_summary(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let summary = self
        .native_use_cases()
        .native_recovery_summary()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_recovery_summary_json(&summary),
    )))
}

async fn native_catalog_summary(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let summary = self
        .native_use_cases()
        .native_catalog_summary()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_catalog_summary_json(&summary),
    )))
}

async fn native_metadata_state_summary(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let summary = self
        .native_use_cases()
        .native_metadata_state_summary()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_metadata_state_summary_json(&summary),
    )))
}

async fn physical_authority(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::ops_json::physical_authority_status_json(
            &self.native_use_cases().physical_authority_status(),
        ),
    )))
}

async fn native_physical_state(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let state = self
        .native_use_cases()
        .native_physical_state()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_physical_state_json(
            &state,
            crate::presentation::native_json::native_header_json,
            crate::presentation::native_json::collection_roots_json,
            crate::presentation::native_json::native_manifest_summary_json,
            crate::presentation::ops_json::native_registry_summary_json,
        ),
    )))
}

async fn native_vector_artifacts(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let summaries = self
        .native_use_cases()
        .native_vector_artifact_pages()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_vector_artifact_pages_json(&summaries),
    )))
}

async fn inspect_native_vector_artifacts(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let batch = self
        .native_use_cases()
        .inspect_vector_artifacts()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_vector_artifact_batch_json(&batch),
    )))
}

async fn inspect_native_vector_artifact(
    &self,
    request: Request<CollectionRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let artifact = self
        .native_use_cases()
        .inspect_vector_artifact(InspectNativeArtifactInput {
            collection: request.collection,
            artifact_kind: none_if_empty(&request.artifact_kind).map(str::to_string),
        })
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_vector_artifact_inspection_json(&artifact),
    )))
}

async fn native_header_repair_policy(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let policy = self
        .native_use_cases()
        .native_header_repair_policy()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_json::repair_policy_json(&policy),
    )))
}

async fn repair_native_header(
    &self,
    request: Request<Empty>,
) -> Result<Response<OperationReply>, Status> {
    self.authorize_write(request.metadata())?;
    let policy = self
        .native_use_cases()
        .repair_native_header_from_metadata()
        .map_err(to_status)?;
    Ok(Response::new(OperationReply {
        ok: true,
        message: format!("native header repair policy applied: {policy}"),
    }))
}

async fn warmup_native_vector_artifact(
    &self,
    request: Request<CollectionRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    let artifact = self
        .native_use_cases()
        .warmup_vector_artifact(InspectNativeArtifactInput {
            collection: request.collection,
            artifact_kind: none_if_empty(&request.artifact_kind).map(str::to_string),
        })
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_vector_artifact_inspection_json(&artifact),
    )))
}

async fn warmup_native_vector_artifacts(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let batch = self
        .native_use_cases()
        .warmup_vector_artifacts()
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_state_json::native_vector_artifact_batch_json(&batch),
    )))
}

async fn repair_native_physical_state(
    &self,
    request: Request<Empty>,
) -> Result<Response<OperationReply>, Status> {
    self.authorize_write(request.metadata())?;
    let repaired = self
        .native_use_cases()
        .repair_native_physical_state_from_metadata()
        .map_err(to_status)?;
    Ok(Response::new(OperationReply {
        ok: repaired,
        message: if repaired {
            "native physical state republished from physical metadata".to_string()
        } else {
            "native physical state repair is not available in this mode".to_string()
        },
    }))
}

async fn rebuild_physical_metadata(
    &self,
    request: Request<Empty>,
) -> Result<Response<OperationReply>, Status> {
    self.authorize_write(request.metadata())?;
    let rebuilt = self
        .native_use_cases()
        .rebuild_physical_metadata_from_native_state()
        .map_err(to_status)?;
    Ok(Response::new(OperationReply {
        ok: rebuilt,
        message: if rebuilt {
            "physical metadata rebuilt from native state".to_string()
        } else {
            "native state is not available for metadata rebuild".to_string()
        },
    }))
}

async fn manifest(
    &self,
    request: Request<ManifestRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let events = self
        .native_use_cases()
        .manifest_events_filtered(
            none_if_empty(&request.collection),
            none_if_empty(&request.kind),
            request.since_snapshot,
        )
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_json::manifest_events_json(&events),
    )))
}

async fn roots(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let roots = self.native_use_cases().collection_roots().map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::native_json::collection_roots_json(&roots),
    )))
}

async fn snapshots(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let snapshots = self.native_use_cases().snapshots().map_err(to_status)?;
    Ok(Response::new(PayloadReply {
        ok: true,
        payload: format!("{snapshots:?}"),
    }))
}

async fn exports(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let exports = self.native_use_cases().exports().map_err(to_status)?;
    Ok(Response::new(PayloadReply {
        ok: true,
        payload: format!("{exports:?}"),
    }))
}

async fn indexes(
    &self,
    request: Request<CollectionRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let indexes = match none_if_empty(&request.collection) {
        Some(collection) => self.catalog_use_cases().indexes_for_collection(collection),
        None => self.catalog_use_cases().indexes(),
    };
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::indexes_json(&indexes),
    )))
}

async fn declared_indexes(
    &self,
    request: Request<CollectionRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let indexes = match none_if_empty(&request.collection) {
        Some(collection) => self.catalog_use_cases().declared_indexes_for_collection(collection),
        None => self.catalog_use_cases().declared_indexes(),
    };
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::indexes_json(&indexes),
    )))
}

async fn operational_indexes(
    &self,
    request: Request<CollectionRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let indexes = match none_if_empty(&request.collection) {
        Some(collection) => self.catalog_use_cases().indexes_for_collection(collection),
        None => self.catalog_use_cases().indexes(),
    };
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::indexes_json(&indexes),
    )))
}

async fn index_statuses(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_index_statuses_json(
            &self.catalog_use_cases().index_statuses(),
        ),
    )))
}

async fn index_attention(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_index_attention_json(
            &self.catalog_use_cases().index_attention(),
        ),
    )))
}

async fn set_index_enabled(
    &self,
    request: Request<IndexToggleRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument("index name cannot be empty"));
    }
    let index = self
        .admin_use_cases()
        .set_index_enabled(request.name.trim(), request.enabled)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::index_json(&index),
    )))
}

async fn mark_index_building(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument("index name cannot be empty"));
    }
    let index = self
        .admin_use_cases()
        .mark_index_building(request.name.trim())
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::index_json(&index),
    )))
}

async fn mark_index_ready(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument("index name cannot be empty"));
    }
    let index = self
        .admin_use_cases()
        .mark_index_ready(request.name.trim())
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::index_json(&index),
    )))
}

async fn fail_index(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument("index name cannot be empty"));
    }
    let index = self
        .admin_use_cases()
        .fail_index(request.name.trim())
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::index_json(&index),
    )))
}

async fn mark_index_stale(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument("index name cannot be empty"));
    }
    let index = self
        .admin_use_cases()
        .mark_index_stale(request.name.trim())
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::index_json(&index),
    )))
}

async fn warmup_index(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument("index name cannot be empty"));
    }
    let index = self
        .admin_use_cases()
        .warmup_index(request.name.trim())
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::index_json(&index),
    )))
}

async fn rebuild_indexes(
    &self,
    request: Request<CollectionRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    let indexes = self
        .admin_use_cases()
        .rebuild_indexes(none_if_empty(&request.collection))
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::indexes_json(&indexes),
    )))
}
async fn graph_projections(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let projections = self.catalog_use_cases().graph_projections().map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::graph_projections_json(&projections),
    )))
}

async fn declared_graph_projections(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.graph_projections(request).await
}

async fn operational_graph_projections(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::graph_projections_json(
            &self.catalog_use_cases().operational_graph_projections(),
        ),
    )))
}

async fn graph_projection_statuses(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_graph_projection_statuses_json(
            &self.catalog_use_cases().graph_projection_statuses(),
        ),
    )))
}

async fn graph_projection_attention(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_graph_projection_attention_json(
            &self.catalog_use_cases().graph_projection_attention(),
        ),
    )))
}

async fn save_graph_projection(
    &self,
    request: Request<GraphProjectionUpsertRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    let input = crate::application::admin_payload::finalize_graph_projection_upsert_input(
        request.name,
        crate::application::admin_payload::graph_projection_from_parts(
            request.node_labels,
            request.node_types,
            request.edge_labels,
        ),
        Some(request.source),
        "graph projection requires at least one of node_labels, node_types or edge_labels",
    )
    .map_err(to_status)?;

    let saved = self
        .admin_use_cases()
        .save_graph_projection(input.name, input.projection, input.source)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::graph_projection_json(&saved),
    )))
}

async fn materialize_graph_projection(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument(
            "graph projection name cannot be empty",
        ));
    }
    self.admin_use_cases()
        .mark_graph_projection_materializing(request.name.trim())
        .map_err(to_status)?;
    let projection = match self
        .admin_use_cases()
        .materialize_graph_projection(request.name.trim())
    {
        Ok(projection) => projection,
        Err(err) => {
            let _ = self.admin_use_cases().fail_graph_projection(request.name.trim());
            return Err(to_status(err));
        }
    };
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::graph_projection_json(&projection),
    )))
}

async fn mark_graph_projection_materializing(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument(
            "graph projection name cannot be empty",
        ));
    }
    let projection = self
        .admin_use_cases()
        .mark_graph_projection_materializing(request.name.trim())
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::graph_projection_json(&projection),
    )))
}

async fn fail_graph_projection(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument(
            "graph projection name cannot be empty",
        ));
    }
    let projection = self
        .admin_use_cases()
        .fail_graph_projection(request.name.trim())
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::graph_projection_json(&projection),
    )))
}

async fn mark_graph_projection_stale(
    &self,
    request: Request<IndexNameRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument(
            "graph projection name cannot be empty",
        ));
    }
    let projection = self
        .admin_use_cases()
        .mark_graph_projection_stale(request.name.trim())
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::graph_projection_json(&projection),
    )))
}

async fn save_analytics_job(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::admin_payload::parse_analytics_job_mutation_input(&payload)
        .map_err(to_status)?;
    let job = self
        .admin_use_cases()
        .save_analytics_job(input.kind, input.projection, input.metadata)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(analytics_job_json(&job))))
}

async fn queue_analytics_job(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::admin_payload::parse_analytics_job_mutation_input(&payload)
        .map_err(to_status)?;
    let job = self
        .admin_use_cases()
        .queue_analytics_job(input.kind, input.projection, input.metadata)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(analytics_job_json(&job))))
}

async fn start_analytics_job(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::admin_payload::parse_analytics_job_mutation_input(&payload)
        .map_err(to_status)?;
    let job = self
        .admin_use_cases()
        .start_analytics_job(input.kind, input.projection, input.metadata)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(analytics_job_json(&job))))
}

async fn complete_analytics_job(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::admin_payload::parse_analytics_job_mutation_input(&payload)
        .map_err(to_status)?;
    let job = self
        .admin_use_cases()
        .complete_analytics_job(input.kind, input.projection, input.metadata)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(analytics_job_json(&job))))
}

async fn fail_analytics_job(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::admin_payload::parse_analytics_job_mutation_input(&payload)
        .map_err(to_status)?;
    let job = self
        .admin_use_cases()
        .fail_analytics_job(input.kind, input.projection, input.metadata)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(analytics_job_json(&job))))
}

async fn mark_analytics_job_stale(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::admin_payload::parse_analytics_job_mutation_input(&payload)
        .map_err(to_status)?;
    let job = self
        .admin_use_cases()
        .mark_analytics_job_stale(input.kind, input.projection, input.metadata)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(analytics_job_json(&job))))
}
async fn analytics_jobs(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let jobs = self.catalog_use_cases().analytics_jobs().map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::analytics_jobs_json(&jobs),
    )))
}

async fn declared_analytics_jobs(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.analytics_jobs(request).await
}

async fn operational_analytics_jobs(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::admin_json::analytics_jobs_json(
            &self.catalog_use_cases().operational_analytics_jobs(),
        ),
    )))
}

async fn analytics_job_statuses(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_analytics_job_statuses_json(
            &self.catalog_use_cases().analytics_job_statuses(),
        ),
    )))
}

async fn analytics_job_attention(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::catalog_json::catalog_analytics_job_attention_json(
            &self.catalog_use_cases().analytics_job_attention(),
        ),
    )))
}

async fn scan(&self, request: Request<ScanRequest>) -> Result<Response<ScanReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let page = self
        .query_use_cases()
        .scan(crate::application::ScanCollectionInput {
            collection: request.collection,
            offset: request.offset as usize,
            limit: request.limit.max(1) as usize,
        })
        .map_err(to_status)?;
    Ok(Response::new(scan_reply(page)))
}

async fn query(&self, request: Request<QueryRequest>) -> Result<Response<QueryReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let (entity_types, capabilities) = grpc_parse_query_filters(&request)?;
    let result = self
        .query_use_cases()
        .execute(ExecuteQueryInput {
            query: request.query,
        })
        .map_err(to_status)?;
    Ok(Response::new(query_reply(
        result,
        &entity_types,
        &capabilities,
    )))
}

async fn explain_query(
    &self,
    request: Request<QueryRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let result = self
        .query_use_cases()
        .explain(ExplainQueryInput {
            query: request.into_inner().query,
        })
        .map_err(to_status)?;
    let universal_mode =
        crate::presentation::query_plan_json::logical_plan_uses_universal_mode(
            &result.logical_plan.root,
        );
    Ok(Response::new(json_payload_reply(
        crate::presentation::query_plan_json::query_explain_json(
            &result,
            &format!("{:?}", result.mode).to_lowercase(),
            None,
            false,
            universal_mode,
        ),
    )))
}

async fn text_search(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::query_payload::parse_text_search_input(&payload)
        .map_err(to_status)?;
    let selection = crate::presentation::query_view::search_selection_json(
        &input.entity_types,
        &input.capabilities,
    );

    let result = self
        .query_use_cases()
        .search_text(input)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::query_json::dsl_query_result_json(&result, selection, |item| {
            crate::presentation::query_json::scored_match_json(
                item,
                crate::presentation::entity_json::entity_json,
            )
        }),
    )))
}

async fn hybrid_search(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::query_payload::parse_hybrid_search_input(
        &payload,
        "hybrid search",
    )
    .map_err(to_status)?;
    let selection = crate::presentation::query_view::search_selection_json(
        &input.entity_types,
        &input.capabilities,
    );
    let result = self
        .query_use_cases()
        .search_hybrid(input)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::query_json::dsl_query_result_json(&result, selection, |item| {
            crate::presentation::query_json::scored_match_json(
                item,
                crate::presentation::entity_json::entity_json,
            )
        }),
    )))
}

async fn similar(
    &self,
    request: Request<JsonCreateRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let payload = parse_json_payload(&request.payload_json)?;
    let input =
        crate::application::query_payload::parse_similar_search_input(request.collection, &payload)
            .map_err(to_status)?;
    let response_collection = input.collection.clone();
    let k = input.k;
    let min_score = input.min_score;
    let result = self
        .query_use_cases()
        .search_similar(input)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::query_json::similar_results_json(
            &response_collection,
            k,
            min_score,
            &result,
            crate::presentation::entity_json::entity_json,
        ),
    )))
}

async fn ivf_search(
    &self,
    request: Request<JsonCreateRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let request = request.into_inner();
    let payload = parse_json_payload(&request.payload_json)?;
    let input =
        crate::application::query_payload::parse_ivf_search_input(request.collection, &payload)
            .map_err(to_status)?;
    let result = self
        .query_use_cases()
        .search_ivf(input)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::query_json::runtime_ivf_json(
            &result,
            crate::presentation::entity_json::entity_json,
        ),
    )))
}

async fn graph_neighborhood(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let projection = resolve_projection_payload(self, &payload)?;
    let input = crate::application::graph_payload::parse_graph_neighborhood_input(
        &payload,
        projection,
    )
    .map_err(to_status)?;
    let result = self
        .graph_use_cases()
        .neighborhood(input)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_neighborhood_json(&result),
    )))
}

async fn graph_traverse(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let projection = resolve_projection_payload(self, &payload)?;
    let input =
        crate::application::graph_payload::parse_graph_traversal_input(&payload, projection)
            .map_err(to_status)?;
    let result = self
        .graph_use_cases()
        .traverse(input)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_traversal_json(&result),
    )))
}

async fn graph_shortest_path(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let projection = resolve_projection_payload(self, &payload)?;
    let input =
        crate::application::graph_payload::parse_graph_shortest_path_input(&payload, projection)
            .map_err(to_status)?;
    let result = self
        .graph_use_cases()
        .shortest_path(input)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_path_result_json(&result),
    )))
}

async fn graph_components(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let projection_name = json_string_field(&payload, "projection_name");
    let projection = resolve_projection_payload(self, &payload)?;
    let input = crate::application::graph_payload::parse_graph_components_input(&payload, projection);
    let metadata = crate::application::graph_payload::graph_components_metadata(&input);
    self.start_graph_analytics_job(
        "graph.components",
        projection_name.clone(),
        metadata.clone(),
    )?;
    let result = match self.graph_use_cases().components(input) {
        Ok(result) => result,
        Err(err) => {
            let _ = self.fail_graph_analytics_job(
                "graph.components",
                projection_name.clone(),
                metadata.clone(),
            );
            return Err(to_status(err));
        }
    };
    self.complete_graph_analytics_job("graph.components", projection_name, metadata)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_components_json(&result),
    )))
}

async fn graph_centrality(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let projection_name = json_string_field(&payload, "projection_name");
    let projection = resolve_projection_payload(self, &payload)?;
    let input = crate::application::graph_payload::parse_graph_centrality_input(&payload, projection);
    let kind = crate::application::graph_payload::graph_centrality_kind(input.algorithm);
    let metadata = crate::application::graph_payload::graph_centrality_metadata(&input);
    self.start_graph_analytics_job(&kind, projection_name.clone(), metadata.clone())?;
    let result = match self.graph_use_cases().centrality(input) {
        Ok(result) => result,
        Err(err) => {
            let _ = self.fail_graph_analytics_job(&kind, projection_name.clone(), metadata.clone());
            return Err(to_status(err));
        }
    };
    self.complete_graph_analytics_job(&kind, projection_name, metadata)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_centrality_json(&result),
    )))
}

async fn graph_community(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let projection_name = json_string_field(&payload, "projection_name");
    let projection = resolve_projection_payload(self, &payload)?;
    let input =
        crate::application::graph_payload::parse_graph_communities_input(&payload, projection);
    let kind = crate::application::graph_payload::graph_communities_kind(input.algorithm);
    let metadata = crate::application::graph_payload::graph_communities_metadata(&input);
    self.start_graph_analytics_job(&kind, projection_name.clone(), metadata.clone())?;
    let result = match self.graph_use_cases().communities(input) {
        Ok(result) => result,
        Err(err) => {
            let _ = self.fail_graph_analytics_job(&kind, projection_name.clone(), metadata.clone());
            return Err(to_status(err));
        }
    };
    self.complete_graph_analytics_job(&kind, projection_name, metadata)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_community_json(&result),
    )))
}
async fn graph_clustering(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let projection_name = json_string_field(&payload, "projection_name");
    let projection = resolve_projection_payload(self, &payload)?;
    let input =
        crate::application::graph_payload::parse_graph_clustering_input(&payload, projection);
    let metadata = crate::application::graph_payload::graph_clustering_metadata(&input);
    self.start_graph_analytics_job(
        "graph.clustering",
        projection_name.clone(),
        metadata.clone(),
    )?;
    let result = match self.graph_use_cases().clustering(input) {
        Ok(result) => result,
        Err(err) => {
            let _ = self.fail_graph_analytics_job(
                "graph.clustering",
                projection_name.clone(),
                metadata.clone(),
            );
            return Err(to_status(err));
        }
    };
    self.complete_graph_analytics_job("graph.clustering", projection_name, metadata)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_clustering_json(&result),
    )))
}

async fn graph_personalized_pagerank(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let projection_name = json_string_field(&payload, "projection_name");
    let projection = resolve_projection_payload(self, &payload)?;
    let input = crate::application::graph_payload::parse_graph_personalized_pagerank_input(
        &payload,
        projection,
    )
    .map_err(to_status)?;
    let metadata =
        crate::application::graph_payload::graph_personalized_pagerank_metadata(&input);
    self.start_graph_analytics_job(
        "graph.pagerank.personalized",
        projection_name.clone(),
        metadata.clone(),
    )?;
    let result = match self
        .graph_use_cases()
        .personalized_pagerank(input)
    {
        Ok(result) => result,
        Err(err) => {
            let _ = self.fail_graph_analytics_job(
                "graph.pagerank.personalized",
                projection_name.clone(),
                metadata.clone(),
            );
            return Err(to_status(err));
        }
    };
    self.complete_graph_analytics_job("graph.pagerank.personalized", projection_name, metadata)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_centrality_json(&result),
    )))
}

async fn graph_hits(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let projection_name = json_string_field(&payload, "projection_name");
    let projection = resolve_projection_payload(self, &payload)?;
    let input = crate::application::graph_payload::parse_graph_hits_input(&payload, projection);
    let metadata = crate::application::graph_payload::graph_hits_metadata(&input);
    self.start_graph_analytics_job("graph.hits", projection_name.clone(), metadata.clone())?;
    let result = match self.graph_use_cases().hits(input) {
        Ok(result) => result,
        Err(err) => {
            let _ = self.fail_graph_analytics_job(
                "graph.hits",
                projection_name.clone(),
                metadata.clone(),
            );
            return Err(to_status(err));
        }
    };
    self.complete_graph_analytics_job("graph.hits", projection_name, metadata)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_hits_json(&result),
    )))
}

async fn graph_cycles(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let projection_name = json_string_field(&payload, "projection_name");
    let projection = resolve_projection_payload(self, &payload)?;
    let input = crate::application::graph_payload::parse_graph_cycles_input(&payload, projection);
    let metadata = crate::application::graph_payload::graph_cycles_metadata(&input);
    self.start_graph_analytics_job("graph.cycles", projection_name.clone(), metadata.clone())?;
    let result = match self.graph_use_cases().cycles(input) {
        Ok(result) => result,
        Err(err) => {
            let _ = self.fail_graph_analytics_job(
                "graph.cycles",
                projection_name.clone(),
                metadata.clone(),
            );
            return Err(to_status(err));
        }
    };
    self.complete_graph_analytics_job("graph.cycles", projection_name, metadata)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_cycles_json(&result),
    )))
}

async fn graph_topological_sort(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let projection_name = json_string_field(&payload, "projection_name");
    let projection = resolve_projection_payload(self, &payload)?;
    let input = crate::application::graph_payload::parse_graph_topological_sort_input(projection);
    let metadata = BTreeMap::new();
    self.start_graph_analytics_job(
        "graph.topological_sort",
        projection_name.clone(),
        metadata.clone(),
    )?;
    let result = match self
        .graph_use_cases()
        .topological_sort(input)
    {
        Ok(result) => result,
        Err(err) => {
            let _ = self.fail_graph_analytics_job(
                "graph.topological_sort",
                projection_name.clone(),
                metadata.clone(),
            );
            return Err(to_status(err));
        }
    };
    self.complete_graph_analytics_job("graph.topological_sort", projection_name, metadata)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::graph_json::graph_topological_sort_json(&result),
    )))
}

async fn create_row(
    &self,
    request: Request<JsonCreateRequest>,
) -> Result<Response<EntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(create_row_reply(self, request)?))
}

async fn create_node(
    &self,
    request: Request<JsonCreateRequest>,
) -> Result<Response<EntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(create_node_reply(self, request)?))
}

async fn create_edge(
    &self,
    request: Request<JsonCreateRequest>,
) -> Result<Response<EntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(create_edge_reply(self, request)?))
}

async fn create_vector(
    &self,
    request: Request<JsonCreateRequest>,
) -> Result<Response<EntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(create_vector_reply(self, request)?))
}

async fn bulk_create_rows(
    &self,
    request: Request<JsonBulkCreateRequest>,
) -> Result<Response<BulkEntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(bulk_create_reply(self, request, create_row_reply)?))
}

async fn bulk_create_nodes(
    &self,
    request: Request<JsonBulkCreateRequest>,
) -> Result<Response<BulkEntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(bulk_create_reply(self, request, create_node_reply)?))
}

async fn bulk_create_edges(
    &self,
    request: Request<JsonBulkCreateRequest>,
) -> Result<Response<BulkEntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(bulk_create_reply(self, request, create_edge_reply)?))
}

async fn bulk_create_vectors(
    &self,
    request: Request<JsonBulkCreateRequest>,
) -> Result<Response<BulkEntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(bulk_create_reply(self, request, create_vector_reply)?))
}

async fn patch_entity(
    &self,
    request: Request<UpdateEntityRequest>,
) -> Result<Response<EntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(patch_entity_reply(self, request)?))
}

async fn create_snapshot(&self, request: Request<Empty>) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let snapshot = self.native_use_cases().create_snapshot().map_err(to_status)?;
    Ok(Response::new(PayloadReply {
        ok: true,
        payload: format!("{snapshot:?}"),
    }))
}

async fn create_export(
    &self,
    request: Request<ExportRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    if request.name.trim().is_empty() {
        return Err(Status::invalid_argument("export name cannot be empty"));
    }
    let export = self
        .native_use_cases()
        .create_export(request.name)
        .map_err(to_status)?;
    Ok(Response::new(PayloadReply {
        ok: true,
        payload: format!("{export:?}"),
    }))
}

async fn apply_retention(
    &self,
    request: Request<Empty>,
) -> Result<Response<OperationReply>, Status> {
    self.authorize_write(request.metadata())?;
    self.native_use_cases()
        .apply_retention_policy()
        .map_err(to_status)?;
    Ok(Response::new(OperationReply {
        ok: true,
        message: "retention policy applied".to_string(),
    }))
}

async fn delete_entity(
    &self,
    request: Request<DeleteEntityRequest>,
) -> Result<Response<OperationReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    let output = self
        .entity_use_cases()
        .delete(DeleteEntityInput {
            collection: request.collection,
            id: EntityId::new(request.id),
        })
        .map_err(entity_error_to_status)?;
    if !output.deleted {
        return Err(Status::not_found(format!(
            "entity not found: {}",
            request.id
        )));
    }
    Ok(Response::new(OperationReply {
        ok: true,
        message: format!("entity {} deleted", request.id),
    }))
}

async fn checkpoint(&self, _request: Request<Empty>) -> Result<Response<OperationReply>, Status> {
    self.authorize_write(_request.metadata())?;
    self.native_use_cases().checkpoint().map_err(to_status)?;
    Ok(Response::new(OperationReply {
        ok: true,
        message: "checkpoint completed".to_string(),
    }))
}
}
