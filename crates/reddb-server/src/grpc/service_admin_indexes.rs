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
