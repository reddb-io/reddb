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
