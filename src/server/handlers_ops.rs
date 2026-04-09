use super::*;

impl RedDBServer {
    pub(crate) fn handle_export(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let Some(name) = payload.get("name").and_then(JsonValue::as_str) else {
            return json_error(400, "field 'name' must be a string");
        };
        if name.trim().is_empty() {
            return json_error(400, "field 'name' cannot be empty");
        }

        match self.native_use_cases().create_export(name.to_string()) {
            Ok(export) => json_response(
                200,
                crate::presentation::native_json::export_descriptor_json(&export),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_serverless_attach(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let required = match parse_serverless_readiness_requirements(&payload) {
            Ok(required) => required,
            Err(error) => return json_error(400, error),
        };

        let readiness = self.native_use_cases().readiness();
        let (query_ready, write_ready, repair_ready) = (
            readiness.query_serverless,
            readiness.write_serverless,
            readiness.repair_serverless,
        );
        let health = self.native_use_cases().health();
        let authority = self.native_use_cases().physical_authority_status();
        let missing = crate::application::serverless_payload::missing_serverless_readiness(
            &required,
            query_ready,
            write_ready,
            repair_ready,
        );
        let payload = crate::presentation::serverless_json::serverless_attach_json(
            &required,
            &missing,
            query_ready,
            write_ready,
            repair_ready,
            serverless_readiness_summary_to_json(
                query_ready,
                write_ready,
                repair_ready,
                &health,
                &authority,
            ),
        );
        if required.is_empty() || missing.is_empty() {
            json_response(200, payload)
        } else {
            json_response(503, payload)
        }
    }

    pub(crate) fn handle_serverless_warmup(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let force = json_bool_field(&payload, "force").unwrap_or(false);
        let dry_run = json_bool_field(&payload, "dry_run").unwrap_or(false);
        let scopes = match parse_serverless_warmup_scopes(&payload) {
            Ok(scopes) => scopes,
            Err(error) => return json_error(400, error),
        };
        let readiness = self.native_use_cases().readiness();
        let health = self.native_use_cases().health();
        let authority = self.native_use_cases().physical_authority_status();
        let (query_ready, write_ready, repair_ready) = (
            readiness.query_serverless,
            readiness.write_serverless,
            readiness.repair_serverless,
        );
        let missing =
            crate::application::serverless_payload::missing_serverless_warmup_preconditions(
                dry_run,
                query_ready,
                write_ready,
                repair_ready,
            );
        if !missing.is_empty() {
            let mut object = Map::new();
            object.insert("ready".to_string(), JsonValue::Bool(false));
            object.insert("query_ready".to_string(), JsonValue::Bool(query_ready));
            object.insert("write_ready".to_string(), JsonValue::Bool(write_ready));
            object.insert("repair_ready".to_string(), JsonValue::Bool(repair_ready));
            object.insert(
                "required".to_string(),
                JsonValue::Array(if dry_run {
                    vec![JsonValue::String("query".to_string())]
                } else {
                    vec![
                        JsonValue::String("query".to_string()),
                        JsonValue::String("write".to_string()),
                        JsonValue::String("repair".to_string()),
                    ]
                }),
            );
            object.insert(
                "missing".to_string(),
                JsonValue::Array(missing.iter().cloned().map(JsonValue::String).collect()),
            );
            object.insert(
                "error".to_string(),
                JsonValue::String(format!(
                    "warmup precondition not met: {}",
                    missing.join(", ")
                )),
            );
            object.insert(
                "readiness".to_string(),
                serverless_readiness_summary_to_json(
                    query_ready,
                    write_ready,
                    repair_ready,
                    &health,
                    &authority,
                ),
            );
            return json_response(503, JsonValue::Object(object));
        }

        let plan = self.admin_use_cases().build_serverless_warmup_plan(
            &self.catalog_use_cases().index_statuses(),
            &self.catalog_use_cases().graph_projection_statuses(),
            &self.catalog_use_cases().analytics_job_statuses(),
            force,
            scopes.contains(&ServerlessWarmupScope::Indexes),
            scopes.contains(&ServerlessWarmupScope::GraphProjections),
            scopes.contains(&ServerlessWarmupScope::AnalyticsJobs),
            scopes.contains(&ServerlessWarmupScope::NativeArtifacts),
        );
        let mut ready_indexes: Vec<JsonValue> = Vec::new();
        let mut failed_indexes: Vec<JsonValue> = Vec::new();
        let mut ready_graph: Vec<JsonValue> = Vec::new();
        let mut failed_graph: Vec<JsonValue> = Vec::new();
        let mut ready_jobs: Vec<JsonValue> = Vec::new();
        let mut failed_jobs: Vec<JsonValue> = Vec::new();
        let mut native_artifacts = None;
        let mut failed_reasons = Vec::new();

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
                        failed_indexes.push(JsonValue::Object(failure.clone()));
                        failed_reasons.push(failure);
                    }
                }
            }

            for name in &plan.graph_projections {
                if let Err(err) = self
                    .admin_use_cases()
                    .mark_graph_projection_materializing(name)
                {
                    let mut failure = Map::new();
                    failure.insert(
                        "kind".to_string(),
                        JsonValue::String("graph_projection".to_string()),
                    );
                    failure.insert("name".to_string(), JsonValue::String(name.clone()));
                    failure.insert("error".to_string(), JsonValue::String(err.to_string()));
                    failed_graph.push(JsonValue::Object(failure.clone()));
                    failed_reasons.push(failure);
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
                        failed_graph.push(JsonValue::Object(failure.clone()));
                        failed_reasons.push(failure);
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
                    Ok(queued_job) => ready_jobs.push(
                        crate::presentation::admin_json::analytics_job_json(&queued_job),
                    ),
                    Err(err) => {
                        let mut failure = Map::new();
                        failure.insert(
                            "kind".to_string(),
                            JsonValue::String("analytics_job".to_string()),
                        );
                        failure.insert("id".to_string(), {
                            let mut id = job.kind.clone();
                            if let Some(projection) = &job.projection {
                                id.push(':');
                                id.push_str(projection);
                            }
                            JsonValue::String(id)
                        });
                        failure.insert("error".to_string(), JsonValue::String(err.to_string()));
                        failed_jobs.push(JsonValue::Object(failure.clone()));
                        failed_reasons.push(failure);
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
                        failed_reasons.push(failure);
                    }
                }
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
            serverless_readiness_summary_to_json(
                query_ready,
                write_ready,
                repair_ready,
                &health,
                &authority,
            ),
        );
        object.insert("ok".to_string(), JsonValue::Bool(failed_reasons.is_empty()));
        if !failed_reasons.is_empty() {
            object.insert(
                "failures".to_string(),
                JsonValue::Array(failed_reasons.into_iter().map(JsonValue::Object).collect()),
            );
            json_response(200, JsonValue::Object(object))
        } else {
            json_response(200, JsonValue::Object(object))
        }
    }

    pub(crate) fn handle_serverless_reclaim(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let dry_run = json_bool_field(&payload, "dry_run").unwrap_or(false);
        let operations = match parse_serverless_reclaim_operations(&payload) {
            Ok(operations) => operations,
            Err(error) => return json_error(400, error),
        };
        let readiness = self.native_use_cases().readiness();
        let health = self.native_use_cases().health();
        let authority = self.native_use_cases().physical_authority_status();
        let (query_ready, write_ready, repair_ready) = (
            readiness.query_serverless,
            readiness.write_serverless,
            readiness.repair_serverless,
        );
        if !dry_run && !operations.is_empty() && !repair_ready {
            let mut object = Map::new();
            object.insert("ready".to_string(), JsonValue::Bool(false));
            object.insert("query_ready".to_string(), JsonValue::Bool(query_ready));
            object.insert("write_ready".to_string(), JsonValue::Bool(write_ready));
            object.insert("repair_ready".to_string(), JsonValue::Bool(repair_ready));
            object.insert(
                "required".to_string(),
                JsonValue::Array(vec![JsonValue::String("repair".to_string())]),
            );
            object.insert(
                "missing".to_string(),
                JsonValue::Array(vec![JsonValue::String("repair".to_string())]),
            );
            object.insert(
                "error".to_string(),
                JsonValue::String("reclaim precondition not met: repair".to_string()),
            );
            object.insert(
                "readiness".to_string(),
                serverless_readiness_summary_to_json(
                    query_ready,
                    write_ready,
                    repair_ready,
                    &health,
                    &authority,
                ),
            );
            return json_response(503, JsonValue::Object(object));
        }

        let mut operations_executed = Vec::new();
        let mut failures = Vec::new();
        if !dry_run {
            for operation in &operations {
                match operation.as_str() {
                    "maintenance" => {
                        let mut result = Map::new();
                        result.insert(
                            "operation".to_string(),
                            JsonValue::String(operation.clone()),
                        );
                        match self.native_use_cases().run_maintenance() {
                            Ok(()) => {
                                result.insert("ok".to_string(), JsonValue::Bool(true));
                            }
                            Err(err) => {
                                result.insert("ok".to_string(), JsonValue::Bool(false));
                                result.insert(
                                    "error".to_string(),
                                    JsonValue::String(err.to_string()),
                                );
                                failures.push(format!("{operation}: {}", err));
                            }
                        }
                        operations_executed.push(JsonValue::Object(result));
                    }
                    "retention" => {
                        let mut result = Map::new();
                        result.insert(
                            "operation".to_string(),
                            JsonValue::String(operation.clone()),
                        );
                        match self.native_use_cases().apply_retention_policy() {
                            Ok(()) => {
                                result.insert("ok".to_string(), JsonValue::Bool(true));
                            }
                            Err(err) => {
                                result.insert("ok".to_string(), JsonValue::Bool(false));
                                result.insert(
                                    "error".to_string(),
                                    JsonValue::String(err.to_string()),
                                );
                                failures.push(format!("{operation}: {}", err));
                            }
                        }
                        operations_executed.push(JsonValue::Object(result));
                    }
                    "checkpoint" => {
                        let mut result = Map::new();
                        result.insert(
                            "operation".to_string(),
                            JsonValue::String(operation.clone()),
                        );
                        match self.native_use_cases().checkpoint() {
                            Ok(()) => {
                                result.insert("ok".to_string(), JsonValue::Bool(true));
                            }
                            Err(err) => {
                                result.insert("ok".to_string(), JsonValue::Bool(false));
                                result.insert(
                                    "error".to_string(),
                                    JsonValue::String(err.to_string()),
                                );
                                failures.push(format!("{operation}: {}", err));
                            }
                        }
                        operations_executed.push(JsonValue::Object(result));
                    }
                    _ => {}
                }
            }
        }

        let mut object = Map::new();
        object.insert("dry_run".to_string(), JsonValue::Bool(dry_run));
        object.insert(
            "operations".to_string(),
            JsonValue::Array(
                operations
                    .iter()
                    .map(|operation| JsonValue::String(operation.clone()))
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
                    object.insert(
                        "executed".to_string(),
                        JsonValue::Array(operations_executed),
                    );
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
            serverless_readiness_summary_to_json(
                query_ready,
                write_ready,
                repair_ready,
                &health,
                &authority,
            ),
        );
        object.insert("ok".to_string(), JsonValue::Bool(failures.is_empty()));
        json_response(200, JsonValue::Object(object))
    }

    pub(crate) fn handle_rebuild_indexes(
        &self,
        body: Vec<u8>,
        path_collection: Option<&str>,
    ) -> HttpResponse {
        let body_collection = if body.iter().any(|byte| !byte.is_ascii_whitespace()) {
            match parse_json_body(&body) {
                Ok(payload) => payload
                    .get("collection")
                    .and_then(JsonValue::as_str)
                    .map(|value| value.to_string()),
                Err(response) => return response,
            }
        } else {
            None
        };

        let collection = path_collection
            .map(|value| value.to_string())
            .or(body_collection);

        match self
            .admin_use_cases()
            .rebuild_indexes(collection.as_deref())
        {
            Ok(indexes) => {
                json_response(200, crate::presentation::admin_json::indexes_json(&indexes))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    // ------------------------------------------------------------------
    // DDL: Create / Drop / Describe Collection
    // ------------------------------------------------------------------

    pub(crate) fn handle_create_collection(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let Some(name) = payload.get("name").and_then(JsonValue::as_str) else {
            return json_error(400, "field 'name' must be a string");
        };
        if name.trim().is_empty() {
            return json_error(400, "field 'name' cannot be empty");
        }

        match self.runtime.db().store().create_collection(name) {
            Ok(()) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "collection".to_string(),
                    JsonValue::String(name.to_string()),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(400, format!("{err:?}")),
        }
    }

    pub(crate) fn handle_drop_collection(&self, name: &str) -> HttpResponse {
        if name.trim().is_empty() {
            return json_error(400, "collection name cannot be empty");
        }

        match self.runtime.db().store().drop_collection(name) {
            Ok(()) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "dropped".to_string(),
                    JsonValue::String(name.to_string()),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(400, format!("{err:?}")),
        }
    }

    pub(crate) fn handle_describe_collection(&self, name: &str) -> HttpResponse {
        let store = self.runtime.db().store();
        match store.get_collection(name) {
            Some(manager) => {
                let count = manager.count();
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "collection".to_string(),
                    JsonValue::String(name.to_string()),
                );
                object.insert("entity_count".to_string(), JsonValue::Number(count as f64));
                json_response(200, JsonValue::Object(object))
            }
            None => json_error(404, format!("collection '{}' not found", name)),
        }
    }
}
