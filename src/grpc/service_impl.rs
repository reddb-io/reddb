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
                native_artifacts.unwrap_or(JsonValue::Null),
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

async fn search(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input =
        crate::application::query_payload::parse_unified_search_input(&payload).map_err(to_status)?;
    let response = match input {
        crate::application::query_payload::UnifiedSearchInput::Hybrid(input) => {
            let selection = crate::presentation::query_view::search_selection_json(
                &input.entity_types,
                &input.capabilities,
            );
            let result = self
                .query_use_cases()
                .search_hybrid(input)
                .map_err(to_status)?;
            crate::presentation::query_json::dsl_query_result_json(&result, selection, |item| {
                crate::presentation::query_json::scored_match_json(
                    item,
                    crate::presentation::entity_json::entity_json,
                )
            })
        }
        crate::application::query_payload::UnifiedSearchInput::Multimodal(input) => {
            let selection = crate::presentation::query_view::search_selection_json(
                &input.entity_types,
                &input.capabilities,
            );
            let result = self
                .query_use_cases()
                .search_multimodal(input)
                .map_err(to_status)?;
            crate::presentation::query_json::dsl_query_result_json(&result, selection, |item| {
                crate::presentation::query_json::scored_match_json(
                    item,
                crate::presentation::entity_json::entity_json,
            )
        })
    }
        crate::application::query_payload::UnifiedSearchInput::Index(input) => {
            let selection = crate::presentation::query_view::search_selection_json(
                &input.entity_types,
                &input.capabilities,
            );
            let result = self.query_use_cases().search_index(input).map_err(to_status)?;
            crate::presentation::query_json::dsl_query_result_json(&result, selection, |item| {
                crate::presentation::query_json::scored_match_json(
                    item,
                    crate::presentation::entity_json::entity_json,
                )
            })
        }
    };
    Ok(Response::new(json_payload_reply(response)))
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

async fn multimodal_search(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::query_payload::parse_multimodal_search_input(&payload)
        .map_err(to_status)?;
    let selection = crate::presentation::query_view::search_selection_json(
        &input.entity_types,
        &input.capabilities,
    );

    let result = self
        .query_use_cases()
        .search_multimodal(input)
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
    let input =
        crate::application::query_payload::parse_unified_search_input(&payload).map_err(to_status)?;
    let response = match input {
        crate::application::query_payload::UnifiedSearchInput::Hybrid(input) => {
            let selection = crate::presentation::query_view::search_selection_json(
                &input.entity_types,
                &input.capabilities,
            );
            let result = self
                .query_use_cases()
                .search_hybrid(input)
                .map_err(to_status)?;
            crate::presentation::query_json::dsl_query_result_json(&result, selection, |item| {
                crate::presentation::query_json::scored_match_json(
                    item,
                    crate::presentation::entity_json::entity_json,
                )
            })
        }
        crate::application::query_payload::UnifiedSearchInput::Multimodal(input) => {
            let selection = crate::presentation::query_view::search_selection_json(
                &input.entity_types,
                &input.capabilities,
            );
            let result = self
                .query_use_cases()
                .search_multimodal(input)
                .map_err(to_status)?;
            crate::presentation::query_json::dsl_query_result_json(&result, selection, |item| {
                crate::presentation::query_json::scored_match_json(
                    item,
                crate::presentation::entity_json::entity_json,
            )
        })
    }
        crate::application::query_payload::UnifiedSearchInput::Index(input) => {
            let selection = crate::presentation::query_view::search_selection_json(
                &input.entity_types,
                &input.capabilities,
            );
            let result = self.query_use_cases().search_index(input).map_err(to_status)?;
            crate::presentation::query_json::dsl_query_result_json(&result, selection, |item| {
                crate::presentation::query_json::scored_match_json(
                    item,
                    crate::presentation::entity_json::entity_json,
                )
            })
        }
    };
    Ok(Response::new(json_payload_reply(response)))
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

async fn create_document(
    &self,
    request: Request<JsonCreateRequest>,
) -> Result<Response<EntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(create_document_reply(self, request)?))
}

async fn create_kv(
    &self,
    request: Request<JsonCreateRequest>,
) -> Result<Response<EntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(create_kv_reply(self, request)?))
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

async fn bulk_create_documents(
    &self,
    request: Request<JsonBulkCreateRequest>,
) -> Result<Response<BulkEntityReply>, Status> {
    self.authorize_write(request.metadata())?;
    let request = request.into_inner();
    Ok(Response::new(bulk_create_reply(self, request, create_document_reply)?))
}

async fn ask(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let question = payload
        .get("question")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Status::invalid_argument("field 'question' must be a string"))?;

    let ask_query = crate::storage::query::ast::AskQuery {
        question: question.to_string(),
        provider: payload.get("provider").and_then(|v| v.as_str()).map(String::from),
        model: payload.get("model").and_then(|v| v.as_str()).map(String::from),
        depth: payload.get("depth").and_then(|v| v.as_u64()).map(|v| v as usize),
        limit: payload.get("limit").and_then(|v| v.as_u64()).map(|v| v as usize),
        collection: payload.get("collection").and_then(|v| v.as_str()).map(String::from),
    };

    let result = self.runtime.execute_ask("ASK via gRPC", &ask_query).map_err(to_status)?;
    let mut object = crate::json::Map::new();
    // Extract answer from first record
    if let Some(record) = result.result.records.first() {
        if let Some(crate::storage::schema::Value::Text(answer)) = record.values.get("answer") {
            object.insert("ok".to_string(), crate::json::Value::Bool(true));
            object.insert("answer".to_string(), crate::json::Value::String(answer.clone()));
        }
    }
    Ok(Response::new(json_payload_reply(crate::json::Value::Object(object))))
}

async fn context_search(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let input = crate::application::query_payload::parse_context_search_input(&payload)
        .map_err(to_status)?;
    let result = self
        .query_use_cases()
        .search_context(input)
        .map_err(to_status)?;
    Ok(Response::new(json_payload_reply(
        crate::presentation::query_json::context_search_result_json(&result),
    )))
}

async fn embeddings(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let result = crate::ai::grpc_embeddings(&self.runtime, &payload).map_err(to_status)?;
    Ok(Response::new(json_payload_reply(result)))
}

async fn ai_prompt(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let result = crate::ai::grpc_prompt(&self.runtime, &payload).map_err(to_status)?;
    Ok(Response::new(json_payload_reply(result)))
}

async fn ai_credentials(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let result = crate::ai::grpc_credentials(&self.runtime, &payload).map_err(to_status)?;
    Ok(Response::new(json_payload_reply(result)))
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

async fn replication_status(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let db = self.runtime.db();
    let role = &db.options().replication.role;
    let mut map = crate::json::Map::new();

    match role {
        crate::replication::ReplicationRole::Standalone => {
            map.insert("role".into(), JsonValue::String("standalone".into()));
        }
        crate::replication::ReplicationRole::Primary => {
            map.insert("role".into(), JsonValue::String("primary".into()));
            if let Some(ref repl) = db.replication {
                let lsn = repl.wal_buffer.current_lsn();
                map.insert("current_lsn".into(), JsonValue::Number(lsn as f64));
                map.insert(
                    "replica_count".into(),
                    JsonValue::Number(repl.replica_count() as f64),
                );
                if let Some(oldest) = repl.wal_buffer.oldest_lsn() {
                    map.insert("oldest_available_lsn".into(), JsonValue::Number(oldest as f64));
                }
            }
        }
        crate::replication::ReplicationRole::Replica { primary_addr } => {
            map.insert("role".into(), JsonValue::String("replica".into()));
            map.insert("primary_addr".into(), JsonValue::String(primary_addr.clone()));
        }
    }

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn pull_wal_records(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let db = self.runtime.db();
    let repl = db.replication.as_ref().ok_or_else(|| {
        Status::failed_precondition("this instance is not a replication primary")
    })?;

    let payload = parse_json_payload_allow_empty(&request.into_inner().payload_json)?;
    let since_lsn = json_usize_field(&payload, "since_lsn").unwrap_or(0) as u64;
    let max_count = json_usize_field(&payload, "max_count").unwrap_or(1000);

    let records = repl.wal_buffer.read_since(since_lsn, max_count);
    let mut entries = Vec::with_capacity(records.len());
    for (lsn, data) in &records {
        let mut entry = crate::json::Map::new();
        entry.insert("lsn".into(), JsonValue::Number(*lsn as f64));
        entry.insert("data".into(), JsonValue::String(hex::encode(data)));
        entries.push(JsonValue::Object(entry));
    }

    let mut map = crate::json::Map::new();
    map.insert("records".into(), JsonValue::Array(entries));
    map.insert(
        "current_lsn".into(),
        JsonValue::Number(repl.wal_buffer.current_lsn() as f64),
    );

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn replication_snapshot(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let db = self.runtime.db();

    if db.replication.is_none() {
        return Err(Status::failed_precondition(
            "this instance is not a replication primary",
        ));
    }

    // Trigger a checkpoint first to ensure data is flushed
    db.flush().map_err(|e| Status::internal(e.to_string()))?;

    let mut map = crate::json::Map::new();
    map.insert("snapshot_available".into(), JsonValue::Bool(true));
    if let Some(path) = db.path() {
        map.insert(
            "snapshot_path".into(),
            JsonValue::String(path.display().to_string()),
        );
    }
    if let Some(ref repl) = db.replication {
        map.insert(
            "snapshot_lsn".into(),
            JsonValue::Number(repl.wal_buffer.current_lsn() as f64),
        );
    }

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

// =========================================================================
// Auth RPCs
// =========================================================================

async fn auth_bootstrap(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let username = json_string_field(&payload, "username")
        .ok_or_else(|| Status::invalid_argument("missing field: username"))?;
    let password = json_string_field(&payload, "password")
        .ok_or_else(|| Status::invalid_argument("missing field: password"))?;

    let result = self
        .auth_store
        .bootstrap(&username, &password)
        .map_err(|e| Status::failed_precondition(e.to_string()))?;

    let mut map = Map::new();
    map.insert("ok".into(), JsonValue::Bool(true));
    map.insert("username".into(), JsonValue::String(result.user.username));
    map.insert("role".into(), JsonValue::String(result.user.role.as_str().to_string()));
    map.insert("api_key".into(), JsonValue::String(result.api_key.key));
    map.insert("api_key_name".into(), JsonValue::String(result.api_key.name));
    if let Some(cert) = result.certificate {
        map.insert("certificate".into(), JsonValue::String(cert));
        map.insert("message".into(), JsonValue::String(
            "Save this certificate — it is the ONLY way to unseal the vault after restart.".into()
        ));
    }

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn auth_login(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    if !self.auth_store.is_enabled() {
        return Err(Status::failed_precondition("authentication is disabled"));
    }

    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let username = json_string_field(&payload, "username")
        .ok_or_else(|| Status::invalid_argument("missing field: username"))?;
    let password = json_string_field(&payload, "password")
        .ok_or_else(|| Status::invalid_argument("missing field: password"))?;

    let session = self
        .auth_store
        .authenticate(&username, &password)
        .map_err(|e| Status::unauthenticated(e.to_string()))?;

    let mut map = Map::new();
    map.insert("token".into(), JsonValue::String(session.token));
    map.insert("username".into(), JsonValue::String(session.username));
    map.insert("role".into(), JsonValue::String(session.role.as_str().to_string()));
    map.insert("expires_at".into(), JsonValue::Number(session.expires_at as f64));

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn auth_create_user(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    if !self.auth_store.is_enabled() {
        return Err(Status::failed_precondition("authentication is disabled"));
    }

    self.authorize_admin(request.metadata())?;

    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let username = json_string_field(&payload, "username")
        .ok_or_else(|| Status::invalid_argument("missing field: username"))?;
    let password = json_string_field(&payload, "password")
        .ok_or_else(|| Status::invalid_argument("missing field: password"))?;
    let role_str = json_string_field(&payload, "role").unwrap_or_else(|| "read".to_string());
    let role = crate::auth::Role::from_str(&role_str)
        .ok_or_else(|| Status::invalid_argument(format!("invalid role: {role_str}")))?;

    let user = self
        .auth_store
        .create_user(&username, &password, role)
        .map_err(|e| Status::already_exists(e.to_string()))?;

    let mut map = Map::new();
    map.insert("username".into(), JsonValue::String(user.username));
    map.insert("role".into(), JsonValue::String(user.role.as_str().to_string()));
    map.insert("enabled".into(), JsonValue::Bool(user.enabled));
    map.insert("created_at".into(), JsonValue::Number(user.created_at as f64));

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn auth_delete_user(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    if !self.auth_store.is_enabled() {
        return Err(Status::failed_precondition("authentication is disabled"));
    }

    self.authorize_admin(request.metadata())?;

    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let username = json_string_field(&payload, "username")
        .ok_or_else(|| Status::invalid_argument("missing field: username"))?;

    self.auth_store
        .delete_user(&username)
        .map_err(|e| Status::not_found(e.to_string()))?;

    let mut map = Map::new();
    map.insert("deleted".into(), JsonValue::Bool(true));
    map.insert("username".into(), JsonValue::String(username));

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn auth_list_users(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    if !self.auth_store.is_enabled() {
        return Err(Status::failed_precondition("authentication is disabled"));
    }

    self.authorize_admin(request.metadata())?;

    let users = self.auth_store.list_users();
    let user_list: Vec<JsonValue> = users
        .into_iter()
        .map(|u| {
            let mut m = Map::new();
            m.insert("username".into(), JsonValue::String(u.username));
            m.insert("role".into(), JsonValue::String(u.role.as_str().to_string()));
            m.insert("enabled".into(), JsonValue::Bool(u.enabled));
            m.insert("created_at".into(), JsonValue::Number(u.created_at as f64));
            m.insert("updated_at".into(), JsonValue::Number(u.updated_at as f64));

            let keys: Vec<JsonValue> = u
                .api_keys
                .iter()
                .map(|k| {
                    let mut km = Map::new();
                    let redacted = if k.key.len() > 6 {
                        format!("{}...", &k.key[..6])
                    } else {
                        k.key.clone()
                    };
                    km.insert("key".into(), JsonValue::String(redacted));
                    km.insert("name".into(), JsonValue::String(k.name.clone()));
                    km.insert("role".into(), JsonValue::String(k.role.as_str().to_string()));
                    km.insert("created_at".into(), JsonValue::Number(k.created_at as f64));
                    JsonValue::Object(km)
                })
                .collect();
            m.insert("api_keys".into(), JsonValue::Array(keys));

            JsonValue::Object(m)
        })
        .collect();

    let mut map = Map::new();
    map.insert("users".into(), JsonValue::Array(user_list));

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn auth_create_api_key(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    if !self.auth_store.is_enabled() {
        return Err(Status::failed_precondition("authentication is disabled"));
    }

    self.authorize_admin(request.metadata())?;

    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let username = json_string_field(&payload, "username")
        .ok_or_else(|| Status::invalid_argument("missing field: username"))?;
    let name = json_string_field(&payload, "name")
        .ok_or_else(|| Status::invalid_argument("missing field: name"))?;
    let role_str = json_string_field(&payload, "role").unwrap_or_else(|| "read".to_string());
    let role = crate::auth::Role::from_str(&role_str)
        .ok_or_else(|| Status::invalid_argument(format!("invalid role: {role_str}")))?;

    let api_key = self
        .auth_store
        .create_api_key(&username, &name, role)
        .map_err(|e| Status::internal(e.to_string()))?;

    let mut map = Map::new();
    map.insert("key".into(), JsonValue::String(api_key.key));
    map.insert("name".into(), JsonValue::String(api_key.name));
    map.insert("role".into(), JsonValue::String(api_key.role.as_str().to_string()));
    map.insert("created_at".into(), JsonValue::Number(api_key.created_at as f64));

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn auth_revoke_api_key(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    if !self.auth_store.is_enabled() {
        return Err(Status::failed_precondition("authentication is disabled"));
    }

    self.authorize_admin(request.metadata())?;

    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let key = json_string_field(&payload, "key")
        .ok_or_else(|| Status::invalid_argument("missing field: key"))?;

    self.auth_store
        .revoke_api_key(&key)
        .map_err(|e| Status::not_found(e.to_string()))?;

    let mut map = Map::new();
    map.insert("revoked".into(), JsonValue::Bool(true));

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn auth_change_password(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    if !self.auth_store.is_enabled() {
        return Err(Status::failed_precondition("authentication is disabled"));
    }

    let auth = self.resolve_auth(request.metadata());
    let caller_username = match &auth {
        AuthResult::Authenticated { username, .. } => username.clone(),
        _ => return Err(Status::unauthenticated("authentication required")),
    };

    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let target_username = json_string_field(&payload, "username")
        .unwrap_or_else(|| caller_username.clone());
    let old_password = json_string_field(&payload, "old_password")
        .ok_or_else(|| Status::invalid_argument("missing field: old_password"))?;
    let new_password = json_string_field(&payload, "new_password")
        .ok_or_else(|| Status::invalid_argument("missing field: new_password"))?;

    if target_username != caller_username {
        check_permission(&auth, false, true)
            .map_err(Status::permission_denied)?;
    }

    self.auth_store
        .change_password(&target_username, &old_password, &new_password)
        .map_err(|e| Status::unauthenticated(e.to_string()))?;

    let mut map = Map::new();
    map.insert("changed".into(), JsonValue::Bool(true));
    map.insert("username".into(), JsonValue::String(target_username));

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn auth_who_am_i(
    &self,
    request: Request<Empty>,
) -> Result<Response<PayloadReply>, Status> {
    let auth = self.resolve_auth(request.metadata());

    let mut map = Map::new();
    match &auth {
        AuthResult::Authenticated { username, role } => {
            map.insert("authenticated".into(), JsonValue::Bool(true));
            map.insert("username".into(), JsonValue::String(username.clone()));
            map.insert("role".into(), JsonValue::String(role.as_str().to_string()));
            map.insert("auth_method".into(), JsonValue::String("token".to_string()));
        }
        AuthResult::Anonymous => {
            map.insert("authenticated".into(), JsonValue::Bool(false));
            map.insert("auth_method".into(), JsonValue::String("anonymous".to_string()));
        }
        AuthResult::Denied(reason) => {
            map.insert("authenticated".into(), JsonValue::Bool(false));
            map.insert("denied".into(), JsonValue::Bool(true));
            map.insert("reason".into(), JsonValue::String(reason.clone()));
        }
    }

    map.insert("auth_enabled".into(), JsonValue::Bool(self.auth_store.is_enabled()));

    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

// =========================================================================
// DDL: Create / Drop / Describe Collection
// =========================================================================

async fn create_collection(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_write(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let name = json_string_field(&payload, "name")
        .ok_or_else(|| Status::invalid_argument("missing field: name"))?;
    let default_ttl_ms = crate::application::ttl_payload::parse_collection_default_ttl_ms(&payload)
        .map_err(entity_error_to_status)?;

    self.runtime
        .db()
        .store()
        .create_collection(&name)
        .map_err(|e| Status::internal(format!("{e:?}")))?;
    if let Some(default_ttl_ms) = default_ttl_ms {
        self.runtime
            .db()
            .set_collection_default_ttl_ms(&name, default_ttl_ms);
    }
    self.runtime
        .db()
        .persist_metadata()
        .map_err(|err| Status::internal(err.to_string()))?;

    let mut map = Map::new();
    map.insert("ok".into(), JsonValue::Bool(true));
    map.insert("collection".into(), JsonValue::String(name));
    if let Some(default_ttl_ms) = default_ttl_ms {
        map.insert(
            "default_ttl_ms".into(),
            JsonValue::Number(default_ttl_ms as f64),
        );
        map.insert(
            "default_ttl".into(),
            JsonValue::String(crate::application::ttl_payload::format_ttl_ms(
                default_ttl_ms,
            )),
        );
    }
    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}

async fn drop_collection(
    &self,
    request: Request<JsonPayloadRequest>,
) -> Result<Response<OperationReply>, Status> {
    self.authorize_admin(request.metadata())?;
    let payload = parse_json_payload(&request.into_inner().payload_json)?;
    let name = json_string_field(&payload, "name")
        .ok_or_else(|| Status::invalid_argument("missing field: name"))?;

    self.runtime
        .db()
        .store()
        .drop_collection(&name)
        .map_err(|e| Status::internal(format!("{e:?}")))?;
    self.runtime.db().clear_collection_default_ttl_ms(&name);
    self.runtime
        .db()
        .persist_metadata()
        .map_err(|err| Status::internal(err.to_string()))?;

    Ok(Response::new(OperationReply {
        ok: true,
        message: format!("collection '{}' dropped", name),
    }))
}

async fn describe_collection(
    &self,
    request: Request<CollectionRequest>,
) -> Result<Response<PayloadReply>, Status> {
    self.authorize_read(request.metadata())?;
    let collection = &request.into_inner().collection;
    let store = self.runtime.db().store();

    let manager = store
        .get_collection(collection)
        .ok_or_else(|| Status::not_found(format!("collection '{}' not found", collection)))?;

    let count = manager.count();
    let mut map = Map::new();
    map.insert("ok".into(), JsonValue::Bool(true));
    map.insert(
        "collection".into(),
        JsonValue::String(collection.clone()),
    );
    map.insert("entity_count".into(), JsonValue::Number(count as f64));
    if let Some(default_ttl_ms) = self.runtime.db().collection_default_ttl_ms(collection) {
        map.insert(
            "default_ttl_ms".into(),
            JsonValue::Number(default_ttl_ms as f64),
        );
        map.insert(
            "default_ttl".into(),
            JsonValue::String(crate::application::ttl_payload::format_ttl_ms(
                default_ttl_ms,
            )),
        );
    }
    Ok(Response::new(json_payload_reply(JsonValue::Object(map))))
}
}
