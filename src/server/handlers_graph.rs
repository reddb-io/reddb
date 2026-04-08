use super::*;

impl RedDBServer {
    fn handle_graph_neighborhood(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input =
            match crate::application::graph_payload::parse_graph_neighborhood_input(&payload, projection)
            {
                Ok(input) => input,
                Err(err) => return json_error(400, err.to_string()),
            };

        match self.graph_use_cases().neighborhood(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::graph_json::graph_neighborhood_json(&result),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_traverse(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input =
            match crate::application::graph_payload::parse_graph_traversal_input(&payload, projection)
            {
                Ok(input) => input,
                Err(err) => return json_error(400, err.to_string()),
            };

        match self.graph_use_cases().traverse(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::graph_json::graph_traversal_json(&result),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_shortest_path(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input =
            match crate::application::graph_payload::parse_graph_shortest_path_input(&payload, projection)
            {
                Ok(input) => input,
                Err(err) => return json_error(400, err.to_string()),
            };

        match self.graph_use_cases().shortest_path(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::graph_json::graph_path_result_json(&result),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn handle_graph_components(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input = crate::application::graph_payload::parse_graph_components_input(&payload, projection);
        let metadata = crate::application::graph_payload::graph_components_metadata(&input);
        if let Err(response) = self.start_graph_analytics_job(
            "graph.components",
            projection_name.as_deref(),
            metadata.clone(),
        ) {
            return response;
        }

        match self.graph_use_cases().components(input) {
            Ok(result) => {
                if let Err(response) = self.complete_graph_analytics_job(
                    "graph.components",
                    projection_name.as_deref(),
                    metadata,
                ) {
                    return response;
                }
                json_response(200, crate::presentation::graph_json::graph_components_json(&result))
            }
            Err(err) => {
                let _ = self.fail_graph_analytics_job(
                    "graph.components",
                    projection_name.as_deref(),
                    metadata,
                );
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_graph_centrality(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input = crate::application::graph_payload::parse_graph_centrality_input(&payload, projection);
        let metadata = crate::application::graph_payload::graph_centrality_metadata(&input);
        let kind = crate::application::graph_payload::graph_centrality_kind(input.algorithm);
        if let Err(response) =
            self.start_graph_analytics_job(&kind, projection_name.as_deref(), metadata.clone())
        {
            return response;
        }

        match self
            .graph_use_cases()
            .centrality(input)
        {
            Ok(result) => {
                if let Err(response) =
                    self.complete_graph_analytics_job(&kind, projection_name.as_deref(), metadata)
                {
                    return response;
                }
                json_response(200, crate::presentation::graph_json::graph_centrality_json(&result))
            }
            Err(err) => {
                let _ = self.fail_graph_analytics_job(&kind, projection_name.as_deref(), metadata);
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_graph_community(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input =
            crate::application::graph_payload::parse_graph_communities_input(&payload, projection);
        let metadata = crate::application::graph_payload::graph_communities_metadata(&input);
        let kind = crate::application::graph_payload::graph_communities_kind(input.algorithm);
        if let Err(response) =
            self.start_graph_analytics_job(&kind, projection_name.as_deref(), metadata.clone())
        {
            return response;
        }

        match self.graph_use_cases().communities(input) {
            Ok(result) => {
                if let Err(response) =
                    self.complete_graph_analytics_job(&kind, projection_name.as_deref(), metadata)
                {
                    return response;
                }
                json_response(200, crate::presentation::graph_json::graph_community_json(&result))
            }
            Err(err) => {
                let _ = self.fail_graph_analytics_job(&kind, projection_name.as_deref(), metadata);
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_graph_clustering(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input =
            crate::application::graph_payload::parse_graph_clustering_input(&payload, projection);
        let metadata = crate::application::graph_payload::graph_clustering_metadata(&input);
        if let Err(response) = self.start_graph_analytics_job(
            "graph.clustering",
            projection_name.as_deref(),
            metadata.clone(),
        ) {
            return response;
        }

        match self.graph_use_cases().clustering(input) {
            Ok(result) => {
                if let Err(response) = self.complete_graph_analytics_job(
                    "graph.clustering",
                    projection_name.as_deref(),
                    metadata,
                ) {
                    return response;
                }
                json_response(200, crate::presentation::graph_json::graph_clustering_json(&result))
            }
            Err(err) => {
                let _ = self.fail_graph_analytics_job(
                    "graph.clustering",
                    projection_name.as_deref(),
                    metadata,
                );
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_graph_personalized_pagerank(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input = match crate::application::graph_payload::parse_graph_personalized_pagerank_input(
            &payload,
            projection,
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };
        let metadata =
            crate::application::graph_payload::graph_personalized_pagerank_metadata(&input);
        if let Err(response) = self.start_graph_analytics_job(
            "graph.pagerank.personalized",
            projection_name.as_deref(),
            metadata.clone(),
        ) {
            return response;
        }

        match self
            .graph_use_cases()
            .personalized_pagerank(input)
        {
            Ok(result) => {
                if let Err(response) = self.complete_graph_analytics_job(
                    "graph.pagerank.personalized",
                    projection_name.as_deref(),
                    metadata,
                ) {
                    return response;
                }
                json_response(200, crate::presentation::graph_json::graph_centrality_json(&result))
            }
            Err(err) => {
                let _ = self.fail_graph_analytics_job(
                    "graph.pagerank.personalized",
                    projection_name.as_deref(),
                    metadata,
                );
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_graph_hits(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input = crate::application::graph_payload::parse_graph_hits_input(&payload, projection);
        let metadata = crate::application::graph_payload::graph_hits_metadata(&input);
        if let Err(response) = self.start_graph_analytics_job(
            "graph.hits",
            projection_name.as_deref(),
            metadata.clone(),
        ) {
            return response;
        }

        match self.graph_use_cases().hits(input) {
            Ok(result) => {
                if let Err(response) = self.complete_graph_analytics_job(
                    "graph.hits",
                    projection_name.as_deref(),
                    metadata,
                ) {
                    return response;
                }
                json_response(200, crate::presentation::graph_json::graph_hits_json(&result))
            }
            Err(err) => {
                let _ = self.fail_graph_analytics_job(
                    "graph.hits",
                    projection_name.as_deref(),
                    metadata,
                );
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_graph_cycles(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input = crate::application::graph_payload::parse_graph_cycles_input(&payload, projection);
        let metadata = crate::application::graph_payload::graph_cycles_metadata(&input);
        if let Err(response) = self.start_graph_analytics_job(
            "graph.cycles",
            projection_name.as_deref(),
            metadata.clone(),
        ) {
            return response;
        }

        match self.graph_use_cases().cycles(input) {
            Ok(result) => {
                if let Err(response) = self.complete_graph_analytics_job(
                    "graph.cycles",
                    projection_name.as_deref(),
                    metadata,
                ) {
                    return response;
                }
                json_response(200, crate::presentation::graph_json::graph_cycles_json(&result))
            }
            Err(err) => {
                let _ = self.fail_graph_analytics_job(
                    "graph.cycles",
                    projection_name.as_deref(),
                    metadata,
                );
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_graph_topological_sort(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let projection_name = json_string_field(&payload, "projection_name");
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input = crate::application::graph_payload::parse_graph_topological_sort_input(projection);
        let metadata = BTreeMap::new();
        if let Err(response) = self.start_graph_analytics_job(
            "graph.topological_sort",
            projection_name.as_deref(),
            metadata.clone(),
        ) {
            return response;
        }

        match self.graph_use_cases().topological_sort(input) {
            Ok(result) => {
                if let Err(response) = self.complete_graph_analytics_job(
                    "graph.topological_sort",
                    projection_name.as_deref(),
                    metadata,
                ) {
                    return response;
                }
                json_response(
                    200,
                    crate::presentation::graph_json::graph_topological_sort_json(&result),
                )
            }
            Err(err) => {
                let _ = self.fail_graph_analytics_job(
                    "graph.topological_sort",
                    projection_name.as_deref(),
                    metadata,
                );
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_graph_projection_upsert(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(name) = json_string_field(&payload, "name") else {
            return json_error(400, "field 'name' must be a string");
        };
        let projection = match self.resolve_projection_payload(&payload) {
            Ok(projection) => projection,
            Err(response) => return response,
        };
        let input = match crate::application::admin_payload::finalize_graph_projection_upsert_input(
            name,
            projection,
            json_string_field(&payload, "source"),
            "graph projection requires at least one of node_labels, node_types, edge_labels or projection_name",
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        match self
            .admin_use_cases()
            .save_graph_projection(input.name, input.projection, input.source)
        {
            Ok(projection) => json_response(
                200,
                crate::presentation::admin_json::graph_projection_json(&projection),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn materialize_graph_projection_transition(&self, name: &str) -> HttpResponse {
        if let Err(err) = self.admin_use_cases().mark_graph_projection_materializing(name) {
            return json_error(400, err.to_string());
        }

        match self.admin_use_cases().materialize_graph_projection(name) {
            Ok(projection) => json_response(
                200,
                crate::presentation::admin_json::graph_projection_json(&projection),
            ),
            Err(err) => {
                let _ = self.admin_use_cases().fail_graph_projection(name);
                json_error(400, err.to_string())
            }
        }
    }

    fn handle_analytics_job_upsert(&self, body: Vec<u8>) -> HttpResponse {
        self.handle_analytics_job_transition(body, |kind, projection, metadata| {
            self.admin_use_cases()
                .save_analytics_job(kind, projection, metadata)
        })
    }

    fn handle_analytics_job_start(&self, body: Vec<u8>) -> HttpResponse {
        self.handle_analytics_job_transition(body, |kind, projection, metadata| {
            self.admin_use_cases()
                .start_analytics_job(kind, projection, metadata)
        })
    }

    fn handle_analytics_job_queue(&self, body: Vec<u8>) -> HttpResponse {
        self.handle_analytics_job_transition(body, |kind, projection, metadata| {
            self.admin_use_cases()
                .queue_analytics_job(kind, projection, metadata)
        })
    }

    fn handle_analytics_job_fail(&self, body: Vec<u8>) -> HttpResponse {
        self.handle_analytics_job_transition(body, |kind, projection, metadata| {
            self.admin_use_cases()
                .fail_analytics_job(kind, projection, metadata)
        })
    }

    fn handle_analytics_job_stale(&self, body: Vec<u8>) -> HttpResponse {
        self.handle_analytics_job_transition(body, |kind, projection, metadata| {
            self.admin_use_cases()
                .mark_analytics_job_stale(kind, projection, metadata)
        })
    }

    fn handle_analytics_job_complete(&self, body: Vec<u8>) -> HttpResponse {
        self.handle_analytics_job_transition(body, |kind, projection, metadata| {
            self.admin_use_cases()
                .complete_analytics_job(kind, projection, metadata)
        })
    }

    fn handle_analytics_job_transition<F>(&self, body: Vec<u8>, apply: F) -> HttpResponse
    where
        F: FnOnce(
            String,
            Option<String>,
            BTreeMap<String, String>,
        ) -> RedDBResult<crate::PhysicalAnalyticsJob>,
    {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::admin_payload::parse_analytics_job_mutation_input(
            &payload,
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        match apply(input.kind, input.projection, input.metadata) {
            Ok(job) => json_response(200, analytics_job_json(&job)),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    fn start_graph_analytics_job(
        &self,
        kind: &str,
        projection: Option<&str>,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), HttpResponse> {
        self.admin_use_cases()
            .queue_analytics_job(
                kind.to_string(),
                projection.map(str::to_string),
                metadata.clone(),
            )
            .map(|_| ())
            .map_err(|err| json_error(409, err.to_string()))?;
        self.admin_use_cases()
            .start_analytics_job(kind.to_string(), projection.map(str::to_string), metadata)
            .map(|_| ())
            .map_err(|err| json_error(409, err.to_string()))
    }

    fn complete_graph_analytics_job(
        &self,
        kind: &str,
        projection: Option<&str>,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), HttpResponse> {
        self.admin_use_cases()
            .complete_analytics_job(kind.to_string(), projection.map(str::to_string), metadata)
            .map(|_| ())
            .map_err(|err| json_error(409, err.to_string()))
    }

    fn fail_graph_analytics_job(
        &self,
        kind: &str,
        projection: Option<&str>,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), HttpResponse> {
        self.admin_use_cases()
            .fail_analytics_job(kind.to_string(), projection.map(str::to_string), metadata)
            .map(|_| ())
            .map_err(|err| json_error(409, err.to_string()))
    }
}
