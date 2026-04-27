use super::*;

impl RedDBServer {
    pub(crate) fn handle_query(&self, body: Vec<u8>) -> HttpResponse {
        let request = match extract_query_request(&body) {
            Ok(request) => request,
            Err(response) => return response,
        };
        let ParsedQueryRequest {
            query,
            entity_types,
            capabilities,
        } = request;

        match self.query_use_cases().execute(ExecuteQueryInput { query }) {
            Ok(result) => {
                // PLAN.md Phase 11.4 — when the operator picked a
                // commit policy that requires replica acks (`ack_n`),
                // block until the configured count of replicas have
                // ack'd the write's LSN. Reads short-circuit
                // (statement_type=="select"), so SELECT latency is
                // unaffected. The helper itself is a no-op when
                // policy is `Local` (the default) or when the
                // statement isn't a mutation.
                let is_mutation = matches!(result.statement_type, "insert" | "update" | "delete");
                if is_mutation {
                    let post_lsn = self.runtime.cdc_current_lsn();
                    if let Err(err) = self.runtime.enforce_commit_policy(post_lsn) {
                        // Only fired when RED_COMMIT_FAIL_ON_TIMEOUT=true
                        // — operator opted into hard-blocking. 504
                        // is the right code: the local write
                        // succeeded, but the configured durability
                        // contract didn't.
                        return json_error(504, err.to_string());
                    }
                }
                json_response(
                    200,
                    crate::presentation::query_result_json::runtime_query_json(
                        &result,
                        &entity_types,
                        &capabilities,
                    ),
                )
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_query_explain(&self, body: Vec<u8>) -> HttpResponse {
        let query = match extract_query(&body) {
            Ok(query) => query,
            Err(response) => return response,
        };

        match self.query_use_cases().explain(ExplainQueryInput { query }) {
            Ok(result) => {
                let universal_mode =
                    crate::presentation::query_plan_json::logical_plan_uses_universal_mode(
                        &result.logical_plan.root,
                    );
                let query_capability = if universal_mode {
                    "multi"
                } else {
                    crate::presentation::query_result_json::query_mode_capability(result.mode)
                };
                json_response(
                    200,
                    crate::presentation::query_plan_json::query_explain_json(
                        &result,
                        crate::presentation::query_result_json::query_mode_name(result.mode),
                        Some(query_capability),
                        true,
                        universal_mode,
                    ),
                )
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_similar(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::query_payload::parse_similar_search_input(
            collection.to_string(),
            &payload,
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };
        let response_collection = input.collection.clone();
        let k = input.k;
        let min_score = input.min_score;

        match self.query_use_cases().search_similar(input) {
            Ok(results) => json_response(
                200,
                crate::presentation::query_json::similar_results_json(
                    &response_collection,
                    k,
                    min_score,
                    &results,
                    crate::presentation::entity_json::entity_json,
                ),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_ivf_search(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::query_payload::parse_ivf_search_input(
            collection.to_string(),
            &payload,
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        match self.query_use_cases().search_ivf(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_json::runtime_ivf_json(
                    &result,
                    crate::presentation::entity_json::entity_json,
                ),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_hybrid_search(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::query_payload::parse_hybrid_search_input(
            &payload,
            "hybrid search",
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };
        let selection = crate::presentation::query_view::search_selection_json(
            &input.entity_types,
            &input.capabilities,
        );

        match self.query_use_cases().search_hybrid(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_json::dsl_query_result_json(
                    &result,
                    selection,
                    |item| {
                        crate::presentation::query_json::scored_match_json(
                            item,
                            crate::presentation::entity_json::entity_json,
                        )
                    },
                ),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_universal_search(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::query_payload::parse_unified_search_input(&payload) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        match input {
            crate::application::query_payload::UnifiedSearchInput::Hybrid(input) => {
                let selection = crate::presentation::query_view::search_selection_json(
                    &input.entity_types,
                    &input.capabilities,
                );
                match self.query_use_cases().search_hybrid(input) {
                    Ok(result) => json_response(
                        200,
                        crate::presentation::query_json::dsl_query_result_json(
                            &result,
                            selection,
                            |item| {
                                crate::presentation::query_json::scored_match_json(
                                    item,
                                    crate::presentation::entity_json::entity_json,
                                )
                            },
                        ),
                    ),
                    Err(err) => json_error(400, err.to_string()),
                }
            }
            crate::application::query_payload::UnifiedSearchInput::Multimodal(input) => {
                let selection = crate::presentation::query_view::search_selection_json(
                    &input.entity_types,
                    &input.capabilities,
                );
                match self.query_use_cases().search_multimodal(input) {
                    Ok(result) => json_response(
                        200,
                        crate::presentation::query_json::dsl_query_result_json(
                            &result,
                            selection,
                            |item| {
                                crate::presentation::query_json::scored_match_json(
                                    item,
                                    crate::presentation::entity_json::entity_json,
                                )
                            },
                        ),
                    ),
                    Err(err) => json_error(400, err.to_string()),
                }
            }
            crate::application::query_payload::UnifiedSearchInput::Index(input) => {
                let selection = crate::presentation::query_view::search_selection_json(
                    &input.entity_types,
                    &input.capabilities,
                );
                match self.query_use_cases().search_index(input) {
                    Ok(result) => json_response(
                        200,
                        crate::presentation::query_json::dsl_query_result_json(
                            &result,
                            selection,
                            |item| {
                                crate::presentation::query_json::scored_match_json(
                                    item,
                                    crate::presentation::entity_json::entity_json,
                                )
                            },
                        ),
                    ),
                    Err(err) => json_error(400, err.to_string()),
                }
            }
        }
    }

    pub(crate) fn handle_text_search(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::query_payload::parse_text_search_input(&payload) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };
        let selection = crate::presentation::query_view::search_selection_json(
            &input.entity_types,
            &input.capabilities,
        );

        match self.query_use_cases().search_text(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_json::dsl_query_result_json(
                    &result,
                    selection,
                    |item| {
                        crate::presentation::query_json::scored_match_json(
                            item,
                            crate::presentation::entity_json::entity_json,
                        )
                    },
                ),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_context_search(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::query_payload::parse_context_search_input(&payload) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        match self.query_use_cases().search_context(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_json::context_search_result_json(&result),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_multimodal_search(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::query_payload::parse_multimodal_search_input(&payload)
        {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };
        let selection = crate::presentation::query_view::search_selection_json(
            &input.entity_types,
            &input.capabilities,
        );

        match self.query_use_cases().search_multimodal(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_json::dsl_query_result_json(
                    &result,
                    selection,
                    |item| {
                        crate::presentation::query_json::scored_match_json(
                            item,
                            crate::presentation::entity_json::entity_json,
                        )
                    },
                ),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }
}
