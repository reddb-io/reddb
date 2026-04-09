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
