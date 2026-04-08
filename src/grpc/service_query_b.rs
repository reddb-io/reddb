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
