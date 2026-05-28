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
            params,
        } = request;
        let stream_ask = is_stream_ask_query(&query);

        // #358: when the client supplied `params`, bind them through the
        // shared user_params binder before dispatch. Falls back to the
        // legacy `execute_query` path when `params` is absent.
        let exec_result = match params {
            Some(binds) => {
                use crate::storage::query::modes::parse_multi;
                use crate::storage::query::user_params;
                match parse_multi(&query) {
                    Ok(parsed) => match user_params::bind(&parsed, &binds) {
                        Ok(bound) => self.runtime.execute_query_expr(bound),
                        Err(err) => {
                            return json_error_code(400, "INVALID_PARAMS", err.to_string());
                        }
                    },
                    Err(err) => return json_error_code(400, "QUERY_ERROR", err.to_string()),
                }
            }
            None => self.query_use_cases().execute(ExecuteQueryInput { query }),
        };

        match exec_result {
            Ok(result) => {
                if stream_ask {
                    return ask_sse_response(&result);
                }
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
            Err(err) => {
                if stream_ask {
                    return ask_sse_error_response(&err);
                }
                if let crate::api::RedDBError::Validation {
                    message,
                    validation,
                } = &err
                {
                    let mut object = crate::json::Map::new();
                    object.insert("ok".to_string(), crate::json::Value::Bool(false));
                    object.insert(
                        "error".to_string(),
                        crate::json_field::SerializedJsonField::tainted(message),
                    );
                    object.insert("validation".to_string(), validation.clone());
                    return json_response(422, crate::json::Value::Object(object));
                }
                let (status, msg) = map_runtime_error(&err);
                json_error(status, msg)
            }
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

    /// Issue #760 — output-stream MVP. NDJSON over HTTP/1.1
    /// chunked encoding. Wraps the existing materialising
    /// [`UnifiedResult`] (the shim slice per ADR 0029): server memory
    /// is unchanged, but the client sees rows arrive incrementally as
    /// the chunk producer flushes its N × 16 KiB production buffer.
    ///
    /// Wire shape (per row):  `{"row": {…projected values…}}\n`
    /// Terminal envelope:     `{"end":  {row_count, lease_handle, snapshot_lsn}}\n`
    /// Mid-stream errors:     `{"error":{code, message}}\n` followed by `{"end": …}`.
    ///
    /// Issue #767 / S8 — the terminal envelope carries `lease_handle`
    /// (opaque 128-bit hex) rather than the internal monotonic id.
    /// Audit events `stream.opened` and `stream.closed` are emitted
    /// around the body so operators can answer "did this stream
    /// outlive its bearer token" without parsing the body.
    ///
    /// Failure modes are differentiated:
    ///   * `OpenStream` time refusals (`stream_in_transaction_unsupported`)
    ///     return a non-streaming JSON response so the client can
    ///     distinguish "the server never accepted my stream" from
    ///     "the server accepted it then failed midway".
    ///   * Errors raised by the executor after headers have been sent
    ///     surface inside the NDJSON body — the HTTP status is already
    ///     200 OK on the wire, and the structured `{"error": …}` line
    ///     carries the failure detail.
    pub(crate) fn handle_query_ndjson_stream<W: std::io::Write>(
        &self,
        body: Vec<u8>,
        ctx: &crate::server::routing::StreamAuditCtx,
        writer: &mut W,
    ) -> io::Result<()> {
        use crate::server::output_stream::{
            self, ChunkProducer, Clock, LeaseLookup, OpenStreamError, PrefixHasher, StreamConfig,
            SystemClock,
        };

        let request = match extract_query_request(&body) {
            Ok(req) => req,
            Err(response) => {
                writer.write_all(&response.to_http_bytes())?;
                return writer.flush();
            }
        };
        let ParsedQueryRequest {
            query,
            entity_types,
            capabilities,
            ..
        } = request;

        // Issue #766 / S7 — resume coordinator. Parse the optional
        // `resume` block from the body. When present, the request is
        // a resumption of a prior stream: the server must validate
        // resumability, lease freshness, and prefix-hash agreement
        // before delivering any rows.
        let resume_params = parse_resume_params(&body);

        // Acceptance criterion #4 — refuse OpenStream when a `BEGIN`
        // is active on the session. HTTP currently runs with the
        // default connection id (0); the stdio transport sets a real
        // id. The gate calls into the runtime accessor either way so
        // future HTTP-level connection-id plumbing wires through with
        // no further routing change.
        let conn_id = crate::runtime::impl_core::current_connection_id();
        if self.runtime.connection_in_transaction(conn_id) {
            let err = OpenStreamError::TransactionActive;
            let response = json_error_code(409, err.code(), err.message());
            writer.write_all(&response.to_http_bytes())?;
            return writer.flush();
        }

        let clock = SystemClock;
        let config = StreamConfig::load(&self.runtime);
        let resumable = output_stream::assess_resumability(&query);

        // For a resume request, the snapshot_lsn comes from the client
        // (the original OpenAck); for a fresh open we capture
        // `cdc_current_lsn()` exactly as before.
        let snapshot_lsn = resume_params
            .as_ref()
            .map(|r| r.snapshot_lsn)
            .unwrap_or_else(|| self.runtime.cdc_current_lsn());
        let lease = output_stream::open_stream(config, snapshot_lsn, false, &clock)
            .expect("OpenStream succeeds once the in-transaction gate has passed");

        // Issue #767 / S8 — emit `stream.opened` audit event with the
        // opaque handle, principal, snapshot lsn, and query hash. The
        // emit is non-blocking; audit failures never terminate a
        // stream that would otherwise succeed.
        let query_hash = query_hash_prefix(&query);
        crate::server::output_stream::audit_stream_opened(
            &self.runtime,
            &lease.lease_handle,
            ctx.principal,
            snapshot_lsn,
            &query_hash,
        );
        // Issue #767 / S8 — if the bearer token is a JWT whose `exp`
        // falls before the lease's snapshot-TTL deadline, the stream
        // *will* outlive the credential. Emit the dedicated audit
        // event so compliance reviewers can answer "did we serve
        // reads after the token expired" without log archaeology.
        let token_expiry_emitted =
            maybe_audit_token_expired_during_lease(&self.runtime, ctx, &lease, clock.now_ms());

        output_stream::write_chunked_response_header(writer, 200, "application/x-ndjson")?;

        let mut producer = ChunkProducer::new(&config, &clock);
        let mut row_count: u64 = 0;
        let mut stream_error: Option<(String, String)> = None;
        let mut close_reason = crate::server::output_stream::CloseReason::Ok;
        let _ = token_expiry_emitted;
        // Resume requests defer registry record until after validation;
        // fresh opens record immediately so a subsequent resume can find
        // them.
        if resume_params.is_none() {
            self.lease_registry
                .record(snapshot_lsn, lease.opened_at_ms, config.snapshot_ttl_ms);
        }

        {
            let mut flush = |bytes: &[u8]| -> io::Result<()> {
                crate::server::output_stream::write_chunk(writer, bytes)
            };

            // Issue #766 — every stream opens with an `open_ack`
            // envelope so the client learns `lease_id`, `snapshot_lsn`,
            // and `resumable` before any rows arrive.
            {
                let mut envelope = crate::json::Map::new();
                let mut payload = crate::json::Map::new();
                payload.insert(
                    "lease_id".to_string(),
                    crate::json::Value::Number(lease.id as f64),
                );
                payload.insert(
                    "snapshot_lsn".to_string(),
                    crate::json::Value::Number(lease.snapshot_lsn as f64),
                );
                payload.insert("resumable".to_string(), crate::json::Value::Bool(resumable));
                envelope.insert("open_ack".to_string(), crate::json::Value::Object(payload));
                let mut line = crate::json::Value::Object(envelope).to_string_compact();
                line.push('\n');
                producer.push_line(line.as_bytes(), &mut flush)?;
            }

            // Validate resume preconditions before re-executing.
            if let Some(rp) = resume_params.as_ref() {
                if !resumable {
                    stream_error = Some((
                        "not_resumable".to_string(),
                        "query plan is not resumable".to_string(),
                    ));
                    close_reason = crate::server::output_stream::CloseReason::Error;
                } else {
                    match self.lease_registry.lookup(rp.snapshot_lsn, clock.now_ms()) {
                        LeaseLookup::Expired | LeaseLookup::Unknown => {
                            stream_error = Some((
                                "snapshot_expired".to_string(),
                                "stream snapshot pin TTL elapsed".to_string(),
                            ));
                            close_reason =
                                crate::server::output_stream::CloseReason::SnapshotExpired;
                        }
                        LeaseLookup::Live => {}
                    }
                }
            }

            let mut prefix_hash_emit = PrefixHasher::new();

            if stream_error.is_none() {
                let exec = self.query_use_cases().execute(ExecuteQueryInput {
                    query: query.clone(),
                });
                match exec {
                    Ok(result) => {
                        let mut records = crate::presentation::query_view::filter_query_records(
                            &result.result.records,
                            &entity_types,
                            &capabilities,
                        );
                        let columns = &result.result.columns;

                        // For resumable plans, force RID ASC order so
                        // both the original stream and any resume re-
                        // execution produce rows in the same sequence.
                        if resumable {
                            records.sort_by_key(|r| record_rid(r).unwrap_or(u64::MAX));
                        }

                        if let Some(rp) = resume_params.as_ref() {
                            // Resume path — hash the prefix up to and
                            // including `resume_after_rid`, compare with
                            // the client's hash, then emit rows whose
                            // rid > resume_after_rid.
                            let mut prefix_hash_check = PrefixHasher::new();
                            let mut crossed = false;
                            let mut hash_mismatch = false;
                            for record in &records {
                                let rid = record_rid(record).unwrap_or(0);
                                let values =
                                    crate::presentation::query_result_json::unified_record_json(
                                        record, columns,
                                    );
                                let mut wrapper = crate::json::Map::new();
                                wrapper.insert("row".to_string(), values);
                                let line = crate::json::Value::Object(wrapper).to_string_compact();
                                if !crossed && rid <= rp.resume_after_rid {
                                    prefix_hash_check.update(line.as_bytes());
                                    continue;
                                }
                                if !crossed {
                                    // First row past the boundary —
                                    // finalize and compare hashes.
                                    let computed = std::mem::replace(
                                        &mut prefix_hash_check,
                                        PrefixHasher::new(),
                                    )
                                    .finalize_hex();
                                    if !constant_time_str_eq(&computed, &rp.prefix_hash) {
                                        hash_mismatch = true;
                                        break;
                                    }
                                    crossed = true;
                                }
                                let mut line_nl = line;
                                line_nl.push('\n');
                                producer.push_line(line_nl.as_bytes(), &mut flush)?;
                                prefix_hash_emit.update(line_nl.trim_end().as_bytes());
                                row_count += 1;
                            }
                            if hash_mismatch {
                                row_count = 0;
                                stream_error = Some((
                                    "prefix_hash_mismatch".to_string(),
                                    "client prefix_hash does not match server re-execution"
                                        .to_string(),
                                ));
                                close_reason =
                                    crate::server::output_stream::CloseReason::IntegrityFailed;
                            } else if !crossed {
                                // Whole result was inside the prefix —
                                // validate hash anyway so a bogus
                                // resume_after_rid past the end still
                                // catches divergence.
                                let computed = prefix_hash_check.finalize_hex();
                                if !constant_time_str_eq(&computed, &rp.prefix_hash) {
                                    row_count = 0;
                                    stream_error = Some((
                                        "prefix_hash_mismatch".to_string(),
                                        "client prefix_hash does not match server re-execution"
                                            .to_string(),
                                    ));
                                    close_reason =
                                        crate::server::output_stream::CloseReason::IntegrityFailed;
                                }
                            }
                        } else {
                            // Fresh open — emit every row, hashing as
                            // we go so the `end` envelope can carry
                            // `prefix_hash` for future resume.
                            for record in &records {
                                if lease.snapshot_expired(clock.now_ms()) {
                                    stream_error = Some((
                                        "snapshot_expired".to_string(),
                                        "stream snapshot pin TTL elapsed".to_string(),
                                    ));
                                    close_reason =
                                        crate::server::output_stream::CloseReason::SnapshotExpired;
                                    break;
                                }
                                let values =
                                    crate::presentation::query_result_json::unified_record_json(
                                        record, columns,
                                    );
                                let mut wrapper = crate::json::Map::new();
                                wrapper.insert("row".to_string(), values);
                                let line = crate::json::Value::Object(wrapper).to_string_compact();
                                prefix_hash_emit.update(line.as_bytes());
                                let mut line_nl = line;
                                line_nl.push('\n');
                                producer.push_line(line_nl.as_bytes(), &mut flush)?;
                                row_count += 1;
                            }
                        }
                    }
                    Err(err) => {
                        let (_status, message) = map_runtime_error(&err);
                        stream_error = Some((ndjson_error_code(&err).to_string(), message));
                        close_reason = crate::server::output_stream::CloseReason::Error;
                    }
                }
            }

            if let Some((code, message)) = stream_error.as_ref() {
                let mut envelope = crate::json::Map::new();
                let mut payload = crate::json::Map::new();
                payload.insert("code".to_string(), crate::json::Value::String(code.clone()));
                payload.insert(
                    "message".to_string(),
                    crate::json_field::SerializedJsonField::tainted(message),
                );
                envelope.insert("error".to_string(), crate::json::Value::Object(payload));
                let mut line = crate::json::Value::Object(envelope).to_string_compact();
                line.push('\n');
                producer.push_line(line.as_bytes(), &mut flush)?;
            }

            // Terminal envelope. `prefix_hash` covers the rows actually
            // emitted in this stream (so a fresh open ships the hash of
            // its full result; a resume ships the hash of its suffix).
            let mut envelope = crate::json::Map::new();
            let mut payload = crate::json::Map::new();
            payload.insert(
                "row_count".to_string(),
                crate::json::Value::Number(row_count as f64),
            );
            payload.insert(
                "lease_handle".to_string(),
                crate::json::Value::String(lease.lease_handle.clone()),
            );
            payload.insert(
                "snapshot_lsn".to_string(),
                crate::json::Value::Number(lease.snapshot_lsn as f64),
            );
            payload.insert(
                "prefix_hash".to_string(),
                crate::json::Value::String(prefix_hash_emit.finalize_hex()),
            );
            envelope.insert("end".to_string(), crate::json::Value::Object(payload));
            let mut line = crate::json::Value::Object(envelope).to_string_compact();
            line.push('\n');
            producer.push_line(line.as_bytes(), &mut flush)?;
            producer.finish(&mut flush)?;
        }

        let bytes_written = producer.total_bytes();
        crate::server::output_stream::audit_stream_closed(
            &self.runtime,
            &lease.lease_handle,
            ctx.principal,
            close_reason,
            row_count,
            bytes_written,
        );

        crate::server::output_stream::write_chunked_terminator(writer)
    }

    /// Issue #763 / S4 — input-stream MVP over HTTP NDJSON.
    ///
    /// Wire shape (request body):
    ///   line 0 — open frame: `{"open":{"target":"<table>","columns":["c1","c2"]}}`
    ///   line N — row frame:  `{"row":{"c1": v1, "c2": v2}}`
    ///
    /// Wire shape (response, `application/x-ndjson` chunked):
    ///   success → silent during ingest, single terminal
    ///     `{"end":{"row_count":N,"committed_rid":L,"lease_handle":H,"snapshot_lsn":S}}`
    ///   error   → single `{"error":{"code":...,"message":...,
    ///                                 "chunk_seq":K,"recoverable_rid":L}}` then close.
    ///
    /// Issue #767 / S8 — the terminal envelope carries `lease_handle`
    /// (opaque 128-bit hex) rather than the internal monotonic id.
    /// `stream.opened` / `stream.closed` audit events bracket the
    /// body for compliance reporting.
    ///
    /// Auto-commit per chunk: rows from a successful chunk are durable
    /// and visible before the next chunk arrives. A mid-stream failure
    /// leaves the recoverable prefix identified by `recoverable_rid`
    /// (the current CDC LSN at last successful commit).
    pub(crate) fn handle_query_ndjson_input_stream<W: std::io::Write>(
        &self,
        body: Vec<u8>,
        ctx: &crate::server::routing::StreamAuditCtx,
        writer: &mut W,
    ) -> io::Result<()> {
        use crate::server::output_stream::{
            self, ChunkProducer, Clock, OpenStreamError, StreamConfig, SystemClock,
        };

        // Acceptance criterion #4 — refuse OpenStream when a `BEGIN`
        // is active on the session. Mirrors the output-stream gate so
        // the structured code is identical across input/output.
        let conn_id = crate::runtime::impl_core::current_connection_id();
        if self.runtime.connection_in_transaction(conn_id) {
            let err = OpenStreamError::TransactionActive;
            let response = json_error_code(409, err.code(), err.message());
            writer.write_all(&response.to_http_bytes())?;
            return writer.flush();
        }

        // Parse line 0 as the open frame so the target table is bound
        // before we send any wire framing — a missing/invalid open
        // returns a non-streaming 400 so callers can distinguish
        // "never accepted" from "accepted then failed mid-stream".
        let mut lines = ndjson_split(&body);
        let open_line = match lines.next() {
            Some(b) if !b.is_empty() => b,
            _ => {
                let response = json_error(400, "input stream requires open frame as line 0");
                writer.write_all(&response.to_http_bytes())?;
                return writer.flush();
            }
        };
        let (target, columns) = match parse_open_frame(open_line) {
            Ok(parsed) => parsed,
            Err(message) => {
                let response = json_error(400, message);
                writer.write_all(&response.to_http_bytes())?;
                return writer.flush();
            }
        };

        let clock = SystemClock;
        let config = StreamConfig::load(&self.runtime);
        // Issue #765 / S6 — opt-in end-to-end SHA-256. The open frame may
        // request `verify` ("sha256" / "none"); absent that, the
        // `stream.integrity.default_verify` config applies. When disabled
        // (the default) no rolling hash is computed and the row path is
        // byte-identical to the S4 baseline.
        let verify_mode = parse_open_verify(open_line, config.default_verify);
        // Capture the table's highest RID before any chunk commits so a
        // later digest mismatch can tombstone exactly the rows this stream
        // appended. Only paid when verification is enabled.
        let pre_max_rid = if verify_mode.is_enabled() {
            table_max_rid(&self.runtime, &target)
        } else {
            0
        };
        let snapshot_lsn = self.runtime.cdc_current_lsn();
        let lease = output_stream::open_stream(config, snapshot_lsn, false, &clock)
            .expect("OpenStream succeeds once the in-transaction gate has passed");

        // Issue #767 / S8 — open audit + token-expiry hint.
        let open_query_repr = format!("INSERT INTO {target}");
        let query_hash = query_hash_prefix(&open_query_repr);
        crate::server::output_stream::audit_stream_opened(
            &self.runtime,
            &lease.lease_handle,
            ctx.principal,
            snapshot_lsn,
            &query_hash,
        );
        let _ = maybe_audit_token_expired_during_lease(&self.runtime, ctx, &lease, clock.now_ms());

        output_stream::write_chunked_response_header(writer, 200, "application/x-ndjson")?;

        let mut row_count: u64 = 0;
        let mut chunk_seq: u64 = 0;
        let mut committed_rid: u64 = snapshot_lsn;
        let mut pending: Vec<Vec<crate::json::Value>> = Vec::new();
        let chunk_size = config.chunk_max_rows.max(1);
        let mut stream_error: Option<(String, String)> = None;
        let mut close_reason = crate::server::output_stream::CloseReason::Ok;
        // Issue #765 / S6 — rolling SHA-256 over the row payloads (raw row
        // frame bytes, in arrival order) and the client's expected digest
        // parsed off the terminal `{"end":{"sha256":...}}` frame.
        let mut hasher = verify_mode
            .is_enabled()
            .then(crate::server::output_stream::PrefixHasher::new);
        let mut expected_digest: Option<String> = None;

        let mut producer = ChunkProducer::new(&config, &clock);
        {
            let mut flush = |bytes: &[u8]| -> io::Result<()> {
                crate::server::output_stream::write_chunk(writer, bytes)
            };

            let commit_chunk = |runtime: &crate::runtime::RedDBRuntime,
                                rows: &mut Vec<Vec<crate::json::Value>>,
                                committed_rid: &mut u64,
                                row_count: &mut u64,
                                chunk_seq: &mut u64|
             -> Result<(), (String, String)> {
                if rows.is_empty() {
                    return Ok(());
                }
                let sql = match build_insert_sql(&target, &columns, rows) {
                    Ok(sql) => sql,
                    Err(message) => return Err(("invalid_row".to_string(), message)),
                };
                match runtime.execute_query(&sql) {
                    Ok(_) => {
                        *row_count += rows.len() as u64;
                        *committed_rid = runtime.cdc_current_lsn();
                        *chunk_seq += 1;
                        rows.clear();
                        Ok(())
                    }
                    Err(err) => {
                        let (_status, message) = map_runtime_error(&err);
                        Err((ndjson_error_code(&err).to_string(), message))
                    }
                }
            };

            for (idx, raw) in lines.enumerate() {
                if raw.is_empty() {
                    continue;
                }
                if lease.snapshot_expired(clock.now_ms()) {
                    stream_error = Some((
                        "snapshot_expired".to_string(),
                        "stream snapshot pin TTL elapsed".to_string(),
                    ));
                    close_reason = crate::server::output_stream::CloseReason::SnapshotExpired;
                    break;
                }
                // Issue #765 / S6 — when verifying, the client closes the
                // row sequence with a terminal `{"end":{"sha256":...}}`
                // frame carrying the expected digest. Detect it before the
                // row parser so it is not mistaken for a malformed row.
                if verify_mode.is_enabled() {
                    if let Some(digest) = parse_client_end_digest(raw) {
                        expected_digest = Some(digest);
                        break;
                    }
                }
                match parse_row_frame(raw, &columns) {
                    Ok(row_values) => {
                        if let Some(h) = hasher.as_mut() {
                            h.update(raw);
                        }
                        pending.push(row_values);
                    }
                    Err(message) => {
                        stream_error = Some((
                            "invalid_row".to_string(),
                            format!("line {}: {}", idx + 1, message),
                        ));
                        close_reason = crate::server::output_stream::CloseReason::Error;
                        break;
                    }
                }
                if pending.len() >= chunk_size {
                    if let Err(err) = commit_chunk(
                        &self.runtime,
                        &mut pending,
                        &mut committed_rid,
                        &mut row_count,
                        &mut chunk_seq,
                    ) {
                        stream_error = Some(err);
                        close_reason = crate::server::output_stream::CloseReason::Error;
                        break;
                    }
                }
            }
            // Drain the tail.
            if stream_error.is_none() {
                if let Err(err) = commit_chunk(
                    &self.runtime,
                    &mut pending,
                    &mut committed_rid,
                    &mut row_count,
                    &mut chunk_seq,
                ) {
                    stream_error = Some(err);
                    close_reason = crate::server::output_stream::CloseReason::Error;
                }
            }

            // Issue #765 / S6 — integrity verification outcome. Only when
            // verification was requested and the ingest itself did not fail.
            // `integrity_ok` flips the terminal `end` envelope's `integrity`
            // field to "ok"; `integrity_failure` carries (expected, actual,
            // tombstoned RID range) for the dedicated error envelope.
            let mut integrity_ok = false;
            let mut integrity_failure: Option<(String, String, Option<(u64, u64)>)> = None;
            if stream_error.is_none() && verify_mode.is_enabled() {
                match (expected_digest.take(), hasher.take()) {
                    (Some(expected), Some(h)) => {
                        let actual = h.finalize_hex();
                        if constant_time_str_eq(&expected, &actual) {
                            integrity_ok = true;
                        } else {
                            // PRD #759: rows already committed per chunk, so
                            // rollback is impossible — tombstone the RID
                            // range this stream appended instead. RIDs are
                            // monotonic, so (pre_max, post_max] is exactly the
                            // committed set absent a concurrent writer.
                            let post_max = table_max_rid(&self.runtime, &target);
                            let range = if post_max > pre_max_rid {
                                let lo = pre_max_rid + 1;
                                self.runtime
                                    .record_integrity_tombstone(&target, lo, post_max);
                                Some((lo, post_max))
                            } else {
                                None
                            };
                            close_reason =
                                crate::server::output_stream::CloseReason::IntegrityFailed;
                            integrity_failure = Some((expected, actual, range));
                        }
                    }
                    _ => {
                        // verify=sha256 requested but no terminal digest
                        // frame arrived — we cannot assert corruption, so the
                        // committed rows are NOT tombstoned. Fail closed with
                        // a distinct code so the client knows verification did
                        // not actually run.
                        stream_error = Some((
                            "integrity_missing_digest".to_string(),
                            "verify=sha256 requested but no terminal digest frame was sent"
                                .to_string(),
                        ));
                        close_reason = crate::server::output_stream::CloseReason::Error;
                    }
                }
            }

            if let Some((code, message)) = stream_error.as_ref() {
                let mut envelope = crate::json::Map::new();
                let mut payload = crate::json::Map::new();
                payload.insert("code".to_string(), crate::json::Value::String(code.clone()));
                payload.insert(
                    "message".to_string(),
                    crate::json_field::SerializedJsonField::tainted(message),
                );
                payload.insert(
                    "chunk_seq".to_string(),
                    crate::json::Value::Number(chunk_seq as f64),
                );
                payload.insert(
                    "recoverable_rid".to_string(),
                    crate::json::Value::Number(committed_rid as f64),
                );
                envelope.insert("error".to_string(), crate::json::Value::Object(payload));
                let mut line = crate::json::Value::Object(envelope).to_string_compact();
                line.push('\n');
                producer.push_line(line.as_bytes(), &mut flush)?;
            } else if let Some((expected, actual, range)) = integrity_failure.as_ref() {
                // Issue #765 / S6 — integrity mismatch envelope. Rows are
                // already durable (auto-commit); the RID range was tombstoned
                // above and is reported so the client can correlate.
                let mut envelope = crate::json::Map::new();
                let mut payload = crate::json::Map::new();
                payload.insert(
                    "code".to_string(),
                    crate::json::Value::String("integrity_failed".to_string()),
                );
                payload.insert(
                    "expected".to_string(),
                    crate::json::Value::String(expected.clone()),
                );
                payload.insert(
                    "actual".to_string(),
                    crate::json::Value::String(actual.clone()),
                );
                let range_value = match range {
                    Some((lo, hi)) => crate::json::Value::Array(vec![
                        crate::json::Value::Number(*lo as f64),
                        crate::json::Value::Number(*hi as f64),
                    ]),
                    None => crate::json::Value::Null,
                };
                payload.insert("tombstoned_rid_range".to_string(), range_value);
                envelope.insert("error".to_string(), crate::json::Value::Object(payload));
                let mut line = crate::json::Value::Object(envelope).to_string_compact();
                line.push('\n');
                producer.push_line(line.as_bytes(), &mut flush)?;
            } else {
                let mut envelope = crate::json::Map::new();
                let mut payload = crate::json::Map::new();
                payload.insert(
                    "row_count".to_string(),
                    crate::json::Value::Number(row_count as f64),
                );
                payload.insert(
                    "committed_rid".to_string(),
                    crate::json::Value::Number(committed_rid as f64),
                );
                payload.insert(
                    "chunk_count".to_string(),
                    crate::json::Value::Number(chunk_seq as f64),
                );
                payload.insert(
                    "lease_handle".to_string(),
                    crate::json::Value::String(lease.lease_handle.clone()),
                );
                payload.insert(
                    "snapshot_lsn".to_string(),
                    crate::json::Value::Number(lease.snapshot_lsn as f64),
                );
                // Issue #765 / S6 — surface the verification result only when
                // it ran. Without `verify`, no integrity field appears.
                if integrity_ok {
                    payload.insert(
                        "integrity".to_string(),
                        crate::json::Value::String("ok".to_string()),
                    );
                }
                envelope.insert("end".to_string(), crate::json::Value::Object(payload));
                let mut line = crate::json::Value::Object(envelope).to_string_compact();
                line.push('\n');
                producer.push_line(line.as_bytes(), &mut flush)?;
            }
            producer.finish(&mut flush)?;
        }

        crate::server::output_stream::audit_stream_closed(
            &self.runtime,
            &lease.lease_handle,
            ctx.principal,
            close_reason,
            row_count,
            0,
        );

        crate::server::output_stream::write_chunked_terminator(writer)
    }

    pub(crate) fn handle_query_sse_stream<W: std::io::Write>(
        &self,
        body: Vec<u8>,
        writer: &mut W,
    ) -> io::Result<()> {
        let request = match extract_query_request(&body) {
            Ok(request) => request,
            Err(response) => return write_sse_http_response(&response, writer),
        };
        let ParsedQueryRequest { query, params, .. } = request;
        if params.is_some() {
            let response = self.handle_query(body);
            return write_sse_http_response(&response, writer);
        }

        let ask = match crate::storage::query::modes::parse_multi(&query) {
            Ok(crate::storage::query::ast::QueryExpr::Ask(ask)) if ask.stream => ask,
            Ok(_) => {
                let response = json_error(400, "query is not ASK ... STREAM");
                return write_sse_http_response(&response, writer);
            }
            Err(err) => {
                let response = json_error(400, err.to_string());
                return write_sse_http_response(&response, writer);
            }
        };

        write_sse_response_header(200, writer)?;
        let emitted_answer_tokens = std::cell::Cell::new(0_usize);
        let mut emit =
            |frame: crate::runtime::ai::sse_frame_encoder::Frame| -> crate::RedDBResult<()> {
                if matches!(
                    frame,
                    crate::runtime::ai::sse_frame_encoder::Frame::AnswerToken { .. }
                ) {
                    emitted_answer_tokens.set(emitted_answer_tokens.get().saturating_add(1));
                }
                let encoded = crate::runtime::ai::sse_frame_encoder::encode(&frame);
                writer
                    .write_all(encoded.as_bytes())
                    .and_then(|_| writer.flush())
                    .map_err(|err| crate::api::RedDBError::Query(err.to_string()))
            };

        match self
            .runtime
            .execute_ask_streaming_frames(&query, &ask, &mut emit)
        {
            Ok(result) => {
                if emitted_answer_tokens.get() == 0 {
                    if let Some(row) = result.result.records.first() {
                        for token in ask_answer_tokens(row).unwrap_or_else(|| {
                            vec![schema_text_field(row, "answer").unwrap_or_default()]
                        }) {
                            emit(crate::runtime::ai::sse_frame_encoder::Frame::AnswerToken {
                                text: token,
                            })
                            .map_err(reddb_error_to_io)?;
                        }
                    }
                }
                if let Some(frame) = ask_sse_validation_frame(&result) {
                    emit(frame).map_err(reddb_error_to_io)?;
                } else {
                    emit(crate::runtime::ai::sse_frame_encoder::Frame::Error {
                        code: 500,
                        message: "ASK STREAM result missing ASK row".to_string(),
                    })
                    .map_err(reddb_error_to_io)?;
                }
            }
            Err(err) => {
                let (code, message) = match &err {
                    crate::api::RedDBError::Validation { message, .. } => (422, message.clone()),
                    _ => map_runtime_error(&err),
                };
                emit(crate::runtime::ai::sse_frame_encoder::Frame::Error { code, message })
                    .map_err(reddb_error_to_io)?;
            }
        }
        Ok(())
    }
}

/// Issue #766 / S7 — resume parameters parsed off the optional
/// `resume` block on the request body.
#[derive(Debug, Clone)]
pub(crate) struct ResumeParams {
    pub snapshot_lsn: u64,
    pub resume_after_rid: u64,
    pub prefix_hash: String,
}

/// Re-parse the request body to surface the optional `resume` block.
/// Returns `None` for any shape that is not a JSON object with a
/// well-formed `resume` field — the resume contract is opt-in and a
/// missing/invalid block must not break the legacy non-resuming open.
pub(crate) fn parse_resume_params(body: &[u8]) -> Option<ResumeParams> {
    let text = std::str::from_utf8(body).ok()?;
    let parsed = crate::json::parse_json(text.trim()).ok()?;
    let json = crate::json::Value::from(parsed);
    let resume = json.get("resume")?;
    let snapshot_lsn = resume.get("snapshot_lsn").and_then(|v| v.as_u64())?;
    let resume_after_rid = resume.get("resume_after_rid").and_then(|v| v.as_u64())?;
    let prefix_hash = resume
        .get("prefix_hash")
        .and_then(|v| v.as_str())
        .map(str::to_string)?;
    Some(ResumeParams {
        snapshot_lsn,
        resume_after_rid,
        prefix_hash,
    })
}

/// Extract a record's internal `rid` value as `u64`, if present.
/// Returns `None` for records without a `rid` field (the resume path
/// treats these as ineligible — the resumable assessor refuses queries
/// where this would happen).
pub(crate) fn record_rid(record: &crate::storage::query::unified::UnifiedRecord) -> Option<u64> {
    use crate::storage::schema::types::Value;
    match record.get("rid")? {
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        Value::UnsignedInteger(v) => Some(*v),
        _ => None,
    }
}

/// Constant-time string equality for the prefix-hash compare. Both
/// inputs are user-controlled hex digests so a timing side-channel is
/// of limited cryptographic interest, but consistency with the rest
/// of the auth-adjacent code path is the cheaper default.
pub(crate) fn constant_time_str_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Issue #760 — map a runtime error to a stable NDJSON error code so
/// clients can branch on the failure class without parsing the
/// (translated, operator-controlled) message string. Mirrors the HTTP
/// status table in [`map_runtime_error`] but emits the shorter
/// machine-readable token.
pub(crate) fn ndjson_error_code(err: &crate::api::RedDBError) -> &'static str {
    use crate::api::RedDBError::*;
    match err {
        NotFound(_) => "not_found",
        ReadOnly(_) => "read_only",
        InvalidConfig(_) => "invalid_config",
        InvalidOperation(_) => "invalid_operation",
        Query(_) => "query_error",
        Validation { .. } => "validation_failed",
        FeatureNotEnabled(_) => "feature_not_enabled",
        SchemaVersionMismatch { .. } => "schema_version_mismatch",
        QuotaExceeded(_) => "quota_exceeded",
        Engine(_) | Catalog(_) | Io(_) | VersionUnavailable | Internal(_) => "internal_error",
    }
}

/// Issue #763 — iterate over the NDJSON request body line by line,
/// trimming optional trailing `\r`. Empty lines are kept as empty
/// slices so callers can position-track for error reporting.
pub(crate) fn ndjson_split(body: &[u8]) -> impl Iterator<Item = &[u8]> {
    body.split(|b| *b == b'\n').map(|line| {
        if line.last() == Some(&b'\r') {
            &line[..line.len() - 1]
        } else {
            line
        }
    })
}

/// Issue #763 — parse the open frame.
/// Shape: `{"open":{"target":"<table>","columns":["c1","c2",...]}}`
pub(crate) fn parse_open_frame(line: &[u8]) -> Result<(String, Vec<String>), String> {
    let value: crate::json::Value =
        crate::json::from_slice(line).map_err(|err| format!("open frame is not JSON: {err}"))?;
    let open = value
        .get("open")
        .ok_or_else(|| "open frame missing 'open' field".to_string())?;
    let target = open
        .get("target")
        .and_then(crate::json::Value::as_str)
        .ok_or_else(|| "open.target missing or not a string".to_string())?
        .to_string();
    if !is_safe_sql_identifier(&target) {
        return Err(format!("open.target is not a safe identifier: {target}"));
    }
    let columns_v = open
        .get("columns")
        .and_then(crate::json::Value::as_array)
        .ok_or_else(|| "open.columns missing or not an array".to_string())?;
    if columns_v.is_empty() {
        return Err("open.columns must be a non-empty array".to_string());
    }
    let mut columns = Vec::with_capacity(columns_v.len());
    for c in columns_v {
        let name = c
            .as_str()
            .ok_or_else(|| "open.columns entry is not a string".to_string())?;
        if !is_safe_sql_identifier(name) {
            return Err(format!("column name is not a safe identifier: {name}"));
        }
        columns.push(name.to_string());
    }
    Ok((target, columns))
}

/// Issue #765 / S6 — read the optional `verify` field off the open frame
/// (`{"open":{...,"verify":"sha256"}}`). Falls back to `default` (the
/// `stream.integrity.default_verify` config) when absent or unparseable —
/// a malformed opt-in must never terminate a stream that would otherwise run.
pub(crate) fn parse_open_verify(
    line: &[u8],
    default: crate::runtime::integrity_tombstone::VerifyMode,
) -> crate::runtime::integrity_tombstone::VerifyMode {
    let Ok(value) = crate::json::from_slice::<crate::json::Value>(line) else {
        return default;
    };
    match value
        .get("open")
        .and_then(|open| open.get("verify"))
        .and_then(crate::json::Value::as_str)
    {
        Some(token) => crate::runtime::integrity_tombstone::VerifyMode::parse(token),
        None => default,
    }
}

/// Issue #765 / S6 — parse the client's terminal digest frame
/// (`{"end":{"sha256":"<hex>"}}`). Returns the expected digest string, or
/// `None` for any line that is not a well-formed end frame (those fall
/// through to the row parser).
pub(crate) fn parse_client_end_digest(line: &[u8]) -> Option<String> {
    let value: crate::json::Value = crate::json::from_slice(line).ok()?;
    value
        .get("end")?
        .get("sha256")
        .and_then(crate::json::Value::as_str)
        .map(str::to_string)
}

/// Issue #765 / S6 — highest RID currently present in `table`, or `0` when
/// the table is empty / unreadable. Used to bracket the RID range an input
/// stream appends so a digest mismatch can tombstone exactly those rows.
fn table_max_rid(runtime: &crate::runtime::RedDBRuntime, table: &str) -> u64 {
    let sql = format!("SELECT rid FROM {table}");
    match runtime.execute_query(&sql) {
        Ok(result) => result
            .result
            .records
            .iter()
            .filter_map(record_rid)
            .max()
            .unwrap_or(0),
        Err(_) => 0,
    }
}

/// Issue #763 — parse a row frame `{"row":{"c1":v1,...}}` into the
/// values aligned with `columns`. Missing keys map to JSON `null`.
pub(crate) fn parse_row_frame(
    line: &[u8],
    columns: &[String],
) -> Result<Vec<crate::json::Value>, String> {
    let value: crate::json::Value =
        crate::json::from_slice(line).map_err(|err| format!("row is not JSON: {err}"))?;
    let row = value
        .get("row")
        .ok_or_else(|| "row frame missing 'row' field".to_string())?;
    let obj = row
        .as_object()
        .ok_or_else(|| "row.row must be an object".to_string())?;
    let mut out = Vec::with_capacity(columns.len());
    for col in columns {
        out.push(obj.get(col).cloned().unwrap_or(crate::json::Value::Null));
    }
    Ok(out)
}

/// Issue #763 — only allow `[A-Za-z_][A-Za-z0-9_]*` identifiers so the
/// generated INSERT statement cannot be broken out of by a crafted
/// table or column name. Values are escaped separately.
pub(crate) fn is_safe_sql_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Issue #763 — render one JSON value as a SQL literal. Strings are
/// single-quote escaped; arrays/objects collapse to a JSON-encoded
/// string literal so the row still parses (the engine accepts text
/// for these in shim form).
pub(crate) fn render_sql_literal(value: &crate::json::Value) -> String {
    use crate::json::Value;
    match value {
        Value::Null => "NULL".to_string(),
        Value::Bool(true) => "TRUE".to_string(),
        Value::Bool(false) => "FALSE".to_string(),
        Value::Number(n) => {
            if n.is_finite() {
                if n.fract() == 0.0 && n.abs() < 1e18 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            } else {
                "NULL".to_string()
            }
        }
        Value::String(s) => sql_quote(s),
        other => sql_quote(&other.to_string_compact()),
    }
}

fn sql_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Issue #763 — build a single `INSERT INTO ... VALUES (...),(...),...`
/// SQL statement for one chunk. Identifiers were validated at open;
/// values are escaped per [`render_sql_literal`].
pub(crate) fn build_insert_sql(
    table: &str,
    columns: &[String],
    rows: &[Vec<crate::json::Value>],
) -> Result<String, String> {
    if rows.is_empty() {
        return Err("cannot build INSERT for an empty chunk".to_string());
    }
    let mut sql = String::with_capacity(64 + rows.len() * 32);
    sql.push_str("INSERT INTO ");
    sql.push_str(table);
    sql.push_str(" (");
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            sql.push_str(", ");
        }
        sql.push_str(c);
    }
    sql.push_str(") VALUES ");
    for (ri, row) in rows.iter().enumerate() {
        if row.len() != columns.len() {
            return Err(format!(
                "row {ri} has {} values, expected {}",
                row.len(),
                columns.len()
            ));
        }
        if ri > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for (i, v) in row.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&render_sql_literal(v));
        }
        sql.push(')');
    }
    Ok(sql)
}

/// Issue #767 / S8 — sha-256 prefix of the query text. Used as the
/// `query_hash` field on `stream.opened` audit events so operators can
/// correlate events to query templates without leaking parameter
/// values into the audit log.
pub(crate) fn query_hash_prefix(query: &str) -> String {
    let digest = crate::crypto::sha256(query.as_bytes());
    crate::utils::to_hex_prefix(&digest, 8)
}

/// Issue #767 / S8 — when the lease will outlive the bearer credential
/// (JWT `exp` lands inside the snapshot-TTL window), emit the
/// dedicated `stream.token_expired_during_lease` audit event exactly
/// once per open. Returns `true` if the event was emitted.
pub(crate) fn maybe_audit_token_expired_during_lease(
    runtime: &crate::runtime::RedDBRuntime,
    ctx: &crate::server::routing::StreamAuditCtx,
    lease: &crate::server::output_stream::StreamLease,
    now_ms: u64,
) -> bool {
    let Some(token) = ctx.token else {
        return false;
    };
    let Some(exp_ms) = crate::server::output_stream::parse_jwt_exp_ms(token) else {
        return false;
    };
    let deadline = lease
        .opened_at_ms
        .saturating_add(lease.config.snapshot_ttl_ms);
    // Token already expired by the time we cleared the auth gate, or
    // will expire before the lease's snapshot TTL would. Either way,
    // the lease — not the bearer — is what governs subsequent chunks,
    // so flag the event for compliance review.
    if exp_ms <= now_ms || exp_ms < deadline {
        crate::server::output_stream::audit_token_expired_during_lease(
            runtime,
            &lease.lease_handle,
            ctx.principal,
            exp_ms,
        );
        true
    } else {
        false
    }
}

pub(crate) fn is_stream_ask_query_body(body: &[u8]) -> bool {
    extract_query_request(body)
        .map(|request| is_stream_ask_query(&request.query))
        .unwrap_or(false)
}

fn is_stream_ask_query(query: &str) -> bool {
    matches!(
        crate::storage::query::modes::parse_multi(query),
        Ok(crate::storage::query::ast::QueryExpr::Ask(ask)) if ask.stream
    )
}

fn write_sse_http_response<W: std::io::Write>(
    response: &HttpResponse,
    writer: &mut W,
) -> io::Result<()> {
    if response.content_type != "text/event-stream" {
        writer.write_all(&response.to_http_bytes())?;
        writer.flush()?;
        return Ok(());
    }

    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n",
        response.status,
        status_text(response.status),
        response.content_type
    );
    writer.write_all(header.as_bytes())?;
    for (name, value) in &response.extra_headers {
        writer.write_all(name.as_bytes())?;
        writer.write_all(b": ")?;
        writer.write_all(value.as_bytes())?;
        writer.write_all(b"\r\n")?;
    }
    writer.write_all(b"\r\n")?;
    flush_sse_frames(&response.body, writer)
}

fn write_sse_response_header<W: std::io::Write>(status: u16, writer: &mut W) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        status,
        status_text(status),
    );
    writer.write_all(header.as_bytes())?;
    writer.flush()
}

fn flush_sse_frames<W: std::io::Write>(body: &[u8], writer: &mut W) -> io::Result<()> {
    let mut start = 0;
    while let Some(offset) = body[start..]
        .windows(2)
        .position(|window| window == b"\n\n")
    {
        let end = start + offset + 2;
        writer.write_all(&body[start..end])?;
        writer.flush()?;
        start = end;
    }
    if start < body.len() {
        writer.write_all(&body[start..])?;
        writer.flush()?;
    }
    Ok(())
}

fn ask_sse_response(result: &crate::runtime::RuntimeQueryResult) -> HttpResponse {
    match ask_sse_body(result) {
        Some(body) => HttpResponse {
            status: 200,
            body: body.into_bytes(),
            content_type: "text/event-stream",
            extra_headers: Vec::new(),
        },
        None => HttpResponse {
            status: 500,
            body: crate::runtime::ai::sse_frame_encoder::encode(
                &crate::runtime::ai::sse_frame_encoder::Frame::Error {
                    code: 500,
                    message: "ASK STREAM result missing ASK row".to_string(),
                },
            )
            .into_bytes(),
            content_type: "text/event-stream",
            extra_headers: Vec::new(),
        },
    }
}

fn ask_sse_error_response(err: &crate::api::RedDBError) -> HttpResponse {
    let (code, message) = match err {
        crate::api::RedDBError::Validation { message, .. } => (422, message.clone()),
        _ => map_runtime_error(err),
    };
    HttpResponse {
        status: 200,
        body: crate::runtime::ai::sse_frame_encoder::encode(
            &crate::runtime::ai::sse_frame_encoder::Frame::Error { code, message },
        )
        .into_bytes(),
        content_type: "text/event-stream",
        extra_headers: Vec::new(),
    }
}

fn ask_sse_body(result: &crate::runtime::RuntimeQueryResult) -> Option<String> {
    use crate::runtime::ai::sse_frame_encoder::{
        encode, AuditSummary, Frame, SourceRow, ValidationWarning,
    };

    if result.statement != "ask" {
        return None;
    }
    let row = result.result.records.first()?;
    let sources_json = schema_json_field(row, "sources_flat").unwrap_or(JsonValue::Array(vec![]));
    let validation_json =
        schema_json_field(row, "validation").unwrap_or(JsonValue::Object(Map::new()));

    let sources_flat = sources_json
        .as_array()
        .unwrap_or(&[])
        .iter()
        .filter_map(|source| {
            let urn = source.get("urn").and_then(JsonValue::as_str)?.to_string();
            let payload = source
                .get("payload")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| source.to_string_compact());
            Some(SourceRow { urn, payload })
        })
        .collect();

    let warnings = validation_json
        .get("warnings")
        .and_then(JsonValue::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|warning| {
            Some(ValidationWarning {
                kind: warning.get("kind").and_then(JsonValue::as_str)?.to_string(),
                detail: warning
                    .get("detail")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect();

    let audit = AuditSummary {
        provider: schema_text_field(row, "provider").unwrap_or_default(),
        model: schema_text_field(row, "model").unwrap_or_default(),
        prompt_tokens: schema_u32_field(row, "prompt_tokens").unwrap_or(0),
        completion_tokens: schema_u32_field(row, "completion_tokens").unwrap_or(0),
        cache_hit: schema_bool_field(row, "cache_hit").unwrap_or(false),
    };

    let mut body = String::new();
    body.push_str(&encode(&Frame::Sources { sources_flat }));
    for token in ask_answer_tokens(row)
        .unwrap_or_else(|| vec![schema_text_field(row, "answer").unwrap_or_default()])
    {
        body.push_str(&encode(&Frame::AnswerToken { text: token }));
    }
    body.push_str(&encode(&Frame::Validation {
        ok: validation_json
            .get("ok")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true),
        warnings,
        audit,
    }));
    Some(body)
}

fn ask_sse_validation_frame(
    result: &crate::runtime::RuntimeQueryResult,
) -> Option<crate::runtime::ai::sse_frame_encoder::Frame> {
    use crate::runtime::ai::sse_frame_encoder::{AuditSummary, Frame, ValidationWarning};

    if result.statement != "ask" {
        return None;
    }
    let row = result.result.records.first()?;
    let validation_json =
        schema_json_field(row, "validation").unwrap_or(JsonValue::Object(Map::new()));
    let warnings = validation_json
        .get("warnings")
        .and_then(JsonValue::as_array)
        .unwrap_or(&[])
        .iter()
        .filter_map(|warning| {
            Some(ValidationWarning {
                kind: warning.get("kind").and_then(JsonValue::as_str)?.to_string(),
                detail: warning
                    .get("detail")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect();
    let audit = AuditSummary {
        provider: schema_text_field(row, "provider").unwrap_or_default(),
        model: schema_text_field(row, "model").unwrap_or_default(),
        prompt_tokens: schema_u32_field(row, "prompt_tokens").unwrap_or(0),
        completion_tokens: schema_u32_field(row, "completion_tokens").unwrap_or(0),
        cache_hit: schema_bool_field(row, "cache_hit").unwrap_or(false),
    };
    Some(Frame::Validation {
        ok: validation_json
            .get("ok")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true),
        warnings,
        audit,
    })
}

fn reddb_error_to_io(err: crate::api::RedDBError) -> io::Error {
    io::Error::other(err.to_string())
}

fn ask_answer_tokens(
    record: &crate::storage::query::unified::UnifiedRecord,
) -> Option<Vec<String>> {
    let value = schema_json_field(record, "answer_tokens")?;
    let tokens = value
        .as_array()?
        .iter()
        .filter_map(|token| token.as_str().map(ToString::to_string))
        .collect::<Vec<_>>();
    (!tokens.is_empty()).then_some(tokens)
}

fn schema_field<'a>(
    record: &'a crate::storage::query::unified::UnifiedRecord,
    name: &str,
) -> Option<&'a Value> {
    record
        .iter_fields()
        .find_map(|(key, value)| (key.as_ref() == name).then_some(value))
}

fn schema_text_field(
    record: &crate::storage::query::unified::UnifiedRecord,
    name: &str,
) -> Option<String> {
    match schema_field(record, name)? {
        Value::Text(s) => Some(s.to_string()),
        Value::Email(s) | Value::Url(s) | Value::NodeRef(s) | Value::EdgeRef(s) => Some(s.clone()),
        other => Some(format!("{other}")),
    }
}

fn schema_u32_field(
    record: &crate::storage::query::unified::UnifiedRecord,
    name: &str,
) -> Option<u32> {
    match schema_field(record, name)? {
        Value::Integer(n) => (*n >= 0).then_some((*n).min(u32::MAX as i64) as u32),
        Value::UnsignedInteger(n) => Some((*n).min(u32::MAX as u64) as u32),
        Value::BigInt(n)
        | Value::TimestampMs(n)
        | Value::Timestamp(n)
        | Value::Duration(n)
        | Value::Decimal(n) => (*n >= 0).then_some((*n).min(u32::MAX as i64) as u32),
        Value::Float(n) => (*n >= 0.0).then_some((*n).min(u32::MAX as f64) as u32),
        _ => None,
    }
}

fn schema_bool_field(
    record: &crate::storage::query::unified::UnifiedRecord,
    name: &str,
) -> Option<bool> {
    match schema_field(record, name)? {
        Value::Boolean(value) => Some(*value),
        _ => None,
    }
}

fn schema_json_field(
    record: &crate::storage::query::unified::UnifiedRecord,
    name: &str,
) -> Option<JsonValue> {
    match schema_field(record, name)? {
        Value::Json(bytes) => crate::json::from_slice(bytes).ok(),
        Value::Text(text) => crate::json::from_str(text).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::runtime::RedDBRuntime;
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    static ASK_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_server() -> RedDBServer {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        RedDBServer::new(rt)
    }

    fn body_str(resp: &HttpResponse) -> String {
        String::from_utf8_lossy(&resp.body).to_string()
    }

    #[test]
    fn http_query_params_select_round_trip() {
        // Mirrors `rpc_stdio::query_with_int_text_params_round_trips` over
        // the HTTP transport. Inserts via unparameterized SQL, then issues
        // a SELECT carrying both an int and a text `$N`.
        let server = make_server();
        let ddl = br#"{"query": "CREATE TABLE p (id INTEGER, name TEXT)"}"#;
        let r = server.handle_query(ddl.to_vec());
        assert_eq!(r.status, 200, "ddl: {}", body_str(&r));

        for ins in [
            br#"{"query": "INSERT INTO p (id, name) VALUES (1, 'Alice')"}"# as &[u8],
            br#"{"query": "INSERT INTO p (id, name) VALUES (2, 'Bob')"}"#,
        ] {
            let r = server.handle_query(ins.to_vec());
            assert_eq!(r.status, 200, "insert: {}", body_str(&r));
        }

        let sel = br#"{"query": "SELECT id, name FROM p WHERE id = $1 AND name = $2", "params": [1, "Alice"]}"#;
        let r = server.handle_query(sel.to_vec());
        assert_eq!(r.status, 200, "select: {}", body_str(&r));
        let text = body_str(&r);
        assert!(
            text.contains("\"Alice\""),
            "expected Alice in response: {text}"
        );
        assert!(
            !text.contains("\"Bob\""),
            "Bob should be filtered out: {text}"
        );
    }

    #[test]
    fn http_query_params_typed_json_values_round_trip() {
        let server = make_server();
        let ddl = br#"{"query": "CREATE TABLE value_params (ok BOOLEAN, score FLOAT, payload BLOB, body JSON, seen_at TIMESTAMP, ident UUID)"}"#;
        let r = server.handle_query(ddl.to_vec());
        assert_eq!(r.status, 200, "ddl: {}", body_str(&r));

        let insert = br#"{"query": "INSERT INTO value_params (ok, score, payload, body, seen_at, ident) VALUES ($1, $2, $3, $4, $5, $6)", "params": [true, 1.5, {"$bytes":"3q2+7w=="}, {"z":[1,{"a":true}],"a":null}, {"$ts":"1700000000"}, {"$uuid":"00112233-4455-6677-8899-aabbccddeeff"}]}"#;
        let r = server.handle_query(insert.to_vec());
        assert_eq!(r.status, 200, "insert: {}", body_str(&r));

        let r = server.handle_query(br#"{"query": "SELECT * FROM value_params"}"#.to_vec());
        assert_eq!(r.status, 200, "select: {}", body_str(&r));
        let text = body_str(&r);
        assert!(text.contains("true"), "bool missing: {text}");
        assert!(text.contains("1.5"), "float missing: {text}");
        assert!(text.contains("deadbeef"), "blob missing: {text}");
        assert!(text.contains("\"a\":null"), "json missing: {text}");
        assert!(text.contains("1700000000"), "timestamp missing: {text}");
        assert!(
            text.contains("00112233445566778899aabbccddeeff"),
            "uuid missing: {text}"
        );
    }

    #[test]
    fn http_query_params_arity_mismatch_returns_400() {
        let server = make_server();
        let ddl = br#"{"query": "CREATE TABLE pa (id INTEGER)"}"#;
        assert_eq!(server.handle_query(ddl.to_vec()).status, 200);

        let body = br#"{"query": "SELECT * FROM pa WHERE id = $1", "params": [1, 2]}"#;
        let r = server.handle_query(body.to_vec());
        assert_eq!(r.status, 400, "body: {}", body_str(&r));
        let text = body_str(&r).to_lowercase();
        assert!(text.contains(r#""code":"invalid_params""#), "got: {text}");
        assert!(
            text.contains("param") || text.contains("arity"),
            "got: {text}"
        );
    }

    #[test]
    fn http_query_params_must_be_array() {
        let server = make_server();
        let body = br#"{"query": "SELECT 1", "params": "not-an-array"}"#;
        let r = server.handle_query(body.to_vec());
        assert_eq!(r.status, 400);
        let text = body_str(&r);
        assert!(text.contains(r#""code":"INVALID_PARAMS""#), "got: {text}");
        assert!(text.contains("params"), "got: {text}");
    }

    #[test]
    fn http_query_malformed_json_returns_json_error_instead_of_sql_parse_error() {
        let server = make_server();
        let r = server.handle_query(br#"{"query": "#.to_vec());

        assert_eq!(r.status, 400, "{}", body_str(&r));
        let text = body_str(&r);
        assert!(text.contains("invalid JSON body"), "got: {text}");
        assert!(
            !text.to_lowercase().contains("unexpected token"),
            "malformed JSON should not fall through to SQL parsing: {text}"
        );
    }

    #[test]
    fn http_query_projected_values_do_not_include_internal_metadata() {
        let server = make_server();
        assert_eq!(
            server
                .handle_query(
                    br#"{"query": "CREATE TABLE products (id INTEGER, name TEXT, price INTEGER)"}"#
                        .to_vec()
                )
                .status,
            200
        );
        assert_eq!(
            server
                .handle_query(br#"{"query": "INSERT INTO products (id, name, price) VALUES (1, 'Red Cloud', 99)"}"#.to_vec())
                .status,
            200
        );

        let r = server.handle_query(
            br#"{"query": "SELECT name, price FROM products WHERE id = 1"}"#.to_vec(),
        );

        assert_eq!(r.status, 200, "{}", body_str(&r));
        let json: crate::json::Value = crate::json::from_str(&body_str(&r)).expect("query json");
        let columns = json
            .get("result")
            .and_then(|value| value.get("columns"))
            .and_then(crate::json::Value::as_array)
            .expect("columns");
        assert_eq!(columns, &vec![crate::json!("name"), crate::json!("price")]);
        let values = json
            .get("result")
            .and_then(|value| value.get("records"))
            .and_then(crate::json::Value::as_array)
            .and_then(|records| records.first())
            .and_then(|record| record.get("values"))
            .and_then(crate::json::Value::as_object)
            .expect("record values");
        let keys: Vec<&str> = values.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["name", "price"], "values: {values:?}");
        assert!(
            !values.contains_key("rid")
                && !values.contains_key("collection")
                && !values.contains_key("kind"),
            "internal metadata leaked into values: {values:?}"
        );
    }

    #[test]
    fn http_query_no_params_keeps_legacy_path() {
        // Sanity: absence of `params` keeps the existing single-arg
        // `execute_query(sql)` path so legacy clients are unaffected.
        let server = make_server();
        let ddl = br#"{"query": "CREATE TABLE legacy (id INTEGER)"}"#;
        assert_eq!(server.handle_query(ddl.to_vec()).status, 200);
        let r = server.handle_query(br#"{"query": "SELECT * FROM legacy"}"#.to_vec());
        assert_eq!(r.status, 200, "{}", body_str(&r));
    }

    #[test]
    fn http_query_ask_prompt_token_guard_returns_413_with_limit_name() {
        let server = make_server();
        server
            .runtime
            .execute_query("SET CONFIG ask.max_prompt_tokens = 1")
            .expect("set prompt guard");

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());

        assert_eq!(r.status, 413, "{}", body_str(&r));
        let text = body_str(&r);
        assert!(
            text.contains("max_prompt_tokens"),
            "missing limit name: {text}"
        );
    }

    #[test]
    fn http_query_ask_provider_timeout_guard_returns_504_with_limit_name() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SlowOpenAiStub::start(Duration::from_millis(25));
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        server
            .runtime
            .execute_query("SET CONFIG ask.timeout_ms = 1")
            .expect("set timeout guard");
        server
            .runtime
            .execute_query("SET CONFIG runtime.ai.transport_retry_max_attempts = 1")
            .expect("disable retries");
        server
            .runtime
            .execute_query("SET CONFIG red.config.ai.openai.default.key = 'sk-test'")
            .expect("set api key");

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());

        assert_eq!(r.status, 504, "{}", body_str(&r));
        let text = body_str(&r);
        assert!(text.contains("timeout_ms"), "missing limit name: {text}");
    }

    #[test]
    fn http_query_ask_completion_token_guard_returns_413_with_limit_name() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SlowOpenAiStub::start_with_completion(Duration::ZERO, 3);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        server
            .runtime
            .execute_query("SET CONFIG ask.max_completion_tokens = 1")
            .expect("set completion guard");
        server
            .runtime
            .execute_query("SET CONFIG runtime.ai.transport_retry_max_attempts = 1")
            .expect("disable retries");
        server
            .runtime
            .execute_query("SET CONFIG red.config.ai.openai.default.key = 'sk-test'")
            .expect("set api key");

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());

        assert_eq!(r.status, 413, "{}", body_str(&r));
        let text = body_str(&r);
        assert!(
            text.contains("max_completion_tokens"),
            "missing limit name: {text}"
        );
    }

    #[test]
    fn http_query_ask_daily_cost_guard_returns_413_with_limit_name() {
        let server = make_server();
        server
            .runtime
            .execute_query("SET CONFIG ask.daily_cost_cap_usd = 0")
            .expect("set daily cost guard");

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());

        assert_eq!(r.status, 413, "{}", body_str(&r));
        let text = body_str(&r);
        assert!(
            text.contains("daily_cost_cap_usd"),
            "missing limit name: {text}"
        );
    }

    #[test]
    fn http_query_ask_writes_five_audit_rows_with_fields() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec![
            "answer one",
            "answer two",
            "answer three",
            "answer four",
            "answer five",
        ]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        assert!(server
            .runtime
            .db()
            .store()
            .get_collection("red_ask_audit")
            .is_none());

        for _ in 0..5 {
            let response =
                server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());
            assert_eq!(response.status, 200, "{}", body_str(&response));
        }

        assert_eq!(stub.request_count(), 5);
        let rows = ask_audit_rows(&server);
        assert_eq!(rows.len(), 5);
        let row = &rows[0];
        for key in [
            "ts",
            "tenant",
            "user",
            "role",
            "question",
            "sources_urns",
            "provider",
            "model",
            "prompt_tokens",
            "completion_tokens",
            "cost_usd",
            "answer_hash",
            "citations",
            "cache_hit",
            "mode",
            "temperature",
            "seed",
            "validation_ok",
            "retry_count",
            "errors",
        ] {
            assert!(row.contains_key(key), "audit row missing `{key}`: {row:?}");
        }
        assert!(
            !row.contains_key("answer"),
            "answer should be redacted by default"
        );
        assert_eq!(row["question"], crate::json!("why did login fail?"));
        assert_eq!(row["provider"], crate::json!("openai"));
        assert_eq!(row["model"], crate::json!("gpt-4.1-mini"));
        assert_eq!(row["prompt_tokens"], crate::json!(12));
        assert_eq!(row["completion_tokens"], crate::json!(3));
        assert_eq!(row["cache_hit"], crate::json!(false));
        assert_eq!(row["mode"], crate::json!("strict"));
        assert_eq!(row["validation_ok"], crate::json!(true));
        assert_eq!(row["retry_count"], crate::json!(0));
    }

    #[test]
    fn http_query_ask_audit_include_answer_toggle_changes_shape() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["redacted answer", "visible answer"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);

        let response = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());
        assert_eq!(response.status, 200, "{}", body_str(&response));
        let rows = ask_audit_rows(&server);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].contains_key("answer"), "{:?}", rows[0]);
        assert!(rows[0].contains_key("answer_hash"), "{:?}", rows[0]);

        server
            .runtime
            .execute_query("SET CONFIG ask.audit.include_answer = true")
            .expect("enable answer audit");
        let response = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());
        assert_eq!(response.status, 200, "{}", body_str(&response));
        let rows = ask_audit_rows(&server);
        assert_eq!(rows.len(), 2);
        let row = rows
            .iter()
            .find(|row| row.get("answer") == Some(&crate::json!("visible answer")))
            .expect("visible answer audit row");
        assert_eq!(
            row["answer_hash"],
            crate::json!(crate::runtime::ai::audit_record_builder::answer_hash(
                "visible answer"
            ))
        );
    }

    #[test]
    fn http_query_ask_cache_ttl_hits_and_writes_cache_hit_audit() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["cached answer"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);

        let first = server
            .handle_query(br#"{"query": "ASK 'why did login fail?' CACHE TTL '5m'"}"#.to_vec());
        assert_eq!(first.status, 200, "{}", body_str(&first));
        let second = server
            .handle_query(br#"{"query": "ASK 'why did login fail?' CACHE TTL '5m'"}"#.to_vec());
        assert_eq!(second.status, 200, "{}", body_str(&second));

        assert_eq!(stub.request_count(), 1);
        let text = body_str(&second);
        assert!(text.contains(r#""cache_hit":true"#), "{text}");
        assert!(text.contains(r#""prompt_tokens":0"#), "{text}");
        assert!(text.contains(r#""completion_tokens":0"#), "{text}");
        assert!(text.contains(r#""cost_usd":0"#), "{text}");

        let rows = ask_audit_rows(&server);
        assert_eq!(rows.len(), 2);
        let hits = rows
            .iter()
            .filter(|row| row.get("cache_hit") == Some(&crate::json!(true)))
            .count();
        assert_eq!(hits, 1);
    }

    #[test]
    fn http_query_ask_cache_default_enabled_and_nocache_bypasses() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["default cached", "nocache fresh"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        server
            .runtime
            .execute_query("SET CONFIG ask.cache.enabled = true")
            .expect("enable ask cache");
        server
            .runtime
            .execute_query("SET CONFIG ask.cache.default_ttl = '5m'")
            .expect("set default ttl");

        let first = server.handle_query(br#"{"query": "ASK 'cache by default?'"}"#.to_vec());
        assert_eq!(first.status, 200, "{}", body_str(&first));
        let second = server.handle_query(br#"{"query": "ASK 'cache by default?'"}"#.to_vec());
        assert_eq!(second.status, 200, "{}", body_str(&second));
        assert_eq!(stub.request_count(), 1);
        assert!(body_str(&second).contains(r#""cache_hit":true"#));

        let bypass =
            server.handle_query(br#"{"query": "ASK 'cache by default?' NOCACHE"}"#.to_vec());
        assert_eq!(bypass.status, 200, "{}", body_str(&bypass));
        assert_eq!(stub.request_count(), 2);
        let text = body_str(&bypass);
        assert!(text.contains("nocache fresh"), "{text}");
        assert!(text.contains(r#""cache_hit":false"#), "{text}");
    }

    #[test]
    fn http_query_ask_cache_invalidates_on_source_mutation() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["before mutation", "after mutation"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        server
            .runtime
            .execute_query("CREATE TABLE incidents (id INTEGER, notes TEXT)")
            .expect("create incidents");
        server
            .runtime
            .execute_query("INSERT INTO incidents (id, notes) VALUES (1, 'login failed FDD-1')")
            .expect("insert incident");

        let query =
            br#"{"query": "ASK 'login failed FDD-1' STRICT OFF CACHE TTL '5m' LIMIT 1 MIN_SCORE 0"}"#;
        let first = server.handle_query(query.to_vec());
        assert_eq!(first.status, 200, "{}", body_str(&first));
        let second = server.handle_query(query.to_vec());
        assert_eq!(second.status, 200, "{}", body_str(&second));
        assert_eq!(stub.request_count(), 1);
        assert!(body_str(&second).contains(r#""cache_hit":true"#));

        server
            .runtime
            .execute_query("INSERT INTO incidents (id, notes) VALUES (2, 'login failed FDD-2')")
            .expect("mutate incidents");
        let third = server.handle_query(query.to_vec());
        assert_eq!(third.status, 200, "{}", body_str(&third));
        assert_eq!(stub.request_count(), 2);
        let text = body_str(&third);
        assert!(text.contains("after mutation"), "{text}");
        assert!(text.contains(r#""cache_hit":false"#), "{text}");
    }

    #[test]
    fn http_query_ask_retries_once_when_strict_citation_is_invalid() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["first invalid [^1]", "retry ok"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(stub.request_count(), 2);
        let text = body_str(&r);
        assert!(text.contains("retry ok"), "{text}");
    }

    #[test]
    fn http_query_ask_retry_exhaustion_returns_422_validation_errors() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["first invalid [^1]", "still invalid [^1]"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());

        assert_eq!(r.status, 422, "{}", body_str(&r));
        assert_eq!(stub.request_count(), 2);
        let text = body_str(&r);
        assert!(text.contains(r#""validation""#), "{text}");
        assert!(text.contains(r#""ok":false"#), "{text}");
        assert!(text.contains(r#""errors""#), "{text}");
        assert!(text.contains("out_of_range"), "{text}");
    }

    #[test]
    fn http_query_ask_strict_off_surfaces_warning_without_retry() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["lenient invalid [^1]"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);

        let r =
            server.handle_query(br#"{"query": "ASK 'why did login fail?' STRICT OFF"}"#.to_vec());

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(stub.request_count(), 1);
        let text = body_str(&r);
        assert!(text.contains("lenient invalid"), "{text}");
        assert!(text.contains(r#""warnings""#), "{text}");
        assert!(text.contains("out_of_range"), "{text}");
    }

    #[test]
    fn http_query_ask_strict_falls_back_for_non_citing_provider() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["ollama invalid [^1]"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OLLAMA_API_BASE", &format!("http://{}", stub.addr()));

        let server = make_server();
        server
            .runtime
            .execute_query("SET CONFIG runtime.ai.transport_retry_max_attempts = 1")
            .expect("disable transport retries");

        let r =
            server.handle_query(br#"{"query": "ASK 'why did login fail?' USING ollama"}"#.to_vec());

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(stub.request_count(), 1);
        let text = body_str(&r);
        assert!(text.contains("ollama invalid"), "{text}");
        assert!(text.contains(r#""mode":"lenient""#), "{text}");
        assert!(text.contains("mode_fallback"), "{text}");
        assert!(text.contains("out_of_range"), "{text}");
    }

    #[test]
    fn http_query_ask_capability_setting_can_downgrade_provider() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["override invalid [^1]"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        server
            .runtime
            .execute_query(
                "SET CONFIG ask.providers.capabilities.openai.supports_citations = false",
            )
            .expect("set provider capability override");

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(stub.request_count(), 1);
        let text = body_str(&r);
        assert!(text.contains("override invalid"), "{text}");
        assert!(text.contains(r#""mode":"lenient""#), "{text}");
        assert!(text.contains("mode_fallback"), "{text}");
    }

    #[test]
    fn http_query_ask_using_provider_list_fails_over_to_second_provider() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let groq = StatusOpenAiStub::start(502, "groq unavailable", None);
        let openai = StatusOpenAiStub::start(200, "", Some("openai answered"));
        let _groq_api_base =
            EnvVarGuard::set("REDDB_GROQ_API_BASE", &format!("http://{}", groq.addr()));
        let _openai_api_base = EnvVarGuard::set(
            "REDDB_OPENAI_API_BASE",
            &format!("http://{}", openai.addr()),
        );
        let _groq_api_key = EnvVarGuard::unset("REDDB_GROQ_API_KEY");
        let _openai_api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        server
            .runtime
            .execute_query("SET CONFIG red.config.ai.groq.default.key = 'sk-groq'")
            .expect("set groq api key");

        let r = server.handle_query(
            br#"{"query": "ASK 'why did login fail?' USING 'groq,openai' TEMPERATURE 0.7 SEED 42"}"#.to_vec(),
        );

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(groq.request_count(), 1);
        assert_eq!(openai.request_count(), 1);
        for body in [groq.request_body(0), openai.request_body(0)] {
            let payload = body.split("\r\n\r\n").nth(1).expect("request payload");
            let parsed: crate::json::Value =
                crate::json::from_str(payload).expect("request payload json");
            let temperature = parsed
                .get("temperature")
                .and_then(crate::json::Value::as_f64)
                .expect("temperature");
            assert!((temperature - 0.7).abs() < 0.000_001, "{body}");
            assert_eq!(
                parsed.get("seed").and_then(crate::json::Value::as_u64),
                Some(42)
            );
        }
        let text = body_str(&r);
        assert!(text.contains("openai answered"), "{text}");
        assert!(text.contains(r#""provider":"openai""#), "{text}");
        let rows = ask_audit_rows(&server);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["provider"], crate::json!("openai"));
        let audit_temperature = rows[0]["temperature"].as_f64().expect("audit temperature");
        assert!((audit_temperature - 0.7).abs() < 0.000_001);
        assert_eq!(rows[0]["seed"], crate::json!(42));
    }

    #[test]
    fn http_query_ask_global_provider_fallback_is_used_when_query_has_no_using() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let groq = StatusOpenAiStub::start(502, "groq unavailable", None);
        let openai = StatusOpenAiStub::start(200, "", Some("global fallback answered"));
        let _groq_api_base =
            EnvVarGuard::set("REDDB_GROQ_API_BASE", &format!("http://{}", groq.addr()));
        let _openai_api_base = EnvVarGuard::set(
            "REDDB_OPENAI_API_BASE",
            &format!("http://{}", openai.addr()),
        );
        let _groq_api_key = EnvVarGuard::unset("REDDB_GROQ_API_KEY");
        let _openai_api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        server
            .runtime
            .execute_query("SET CONFIG red.config.ai.groq.default.key = 'sk-groq'")
            .expect("set groq api key");
        server
            .runtime
            .execute_query("SET CONFIG ask.providers.fallback = 'groq,openai'")
            .expect("set fallback list");

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?'"}"#.to_vec());

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(groq.request_count(), 1);
        assert_eq!(openai.request_count(), 1);
        let text = body_str(&r);
        assert!(text.contains("global fallback answered"), "{text}");
        assert!(text.contains(r#""provider":"openai""#), "{text}");
    }

    #[test]
    fn http_query_ask_all_retryable_providers_failed_returns_503_with_attempts() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let groq = StatusOpenAiStub::start(502, "groq unavailable", None);
        let openai = StatusOpenAiStub::start(503, "openai unavailable", None);
        let _groq_api_base =
            EnvVarGuard::set("REDDB_GROQ_API_BASE", &format!("http://{}", groq.addr()));
        let _openai_api_base = EnvVarGuard::set(
            "REDDB_OPENAI_API_BASE",
            &format!("http://{}", openai.addr()),
        );
        let _groq_api_key = EnvVarGuard::unset("REDDB_GROQ_API_KEY");
        let _openai_api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        server
            .runtime
            .execute_query("SET CONFIG red.config.ai.groq.default.key = 'sk-groq'")
            .expect("set groq api key");

        let r = server.handle_query(
            br#"{"query": "ASK 'why did login fail?' USING 'groq,openai'"}"#.to_vec(),
        );

        assert_eq!(r.status, 503, "{}", body_str(&r));
        assert_eq!(groq.request_count(), 1);
        assert_eq!(openai.request_count(), 1);
        let text = body_str(&r);
        assert!(text.contains("ask_provider_failover_exhausted"), "{text}");
        assert!(text.contains("groq"), "{text}");
        assert!(text.contains("openai"), "{text}");
        assert!(text.contains("502"), "{text}");
        assert!(text.contains("503"), "{text}");
    }

    #[test]
    fn http_query_ask_stream_returns_ordered_sse_frames() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = StreamingOpenAiStub::start(vec!["streamed ", "answer"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?' STREAM"}"#.to_vec());

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(r.content_type, "text/event-stream");
        assert_eq!(stub.request_count(), 1);
        let text = body_str(&r);
        let sources_pos = text.find("event: sources\n").expect("sources frame");
        let token_pos = text
            .find("event: answer_token\n")
            .expect("answer_token frame");
        let validation_pos = text.find("event: validation\n").expect("validation frame");
        assert!(sources_pos < token_pos, "{text}");
        assert!(token_pos < validation_pos, "{text}");
        assert!(text.contains(r#"data: {"sources_flat":[]}"#), "{text}");
        assert!(text.contains(r#"data: {"text":"streamed "}"#), "{text}");
        assert!(text.contains(r#"data: {"text":"answer"}"#), "{text}");
        assert_eq!(text.matches("event: answer_token\n").count(), 2, "{text}");
        assert_eq!(text.matches("event: validation\n").count(), 1, "{text}");
        assert!(text.contains(r#""audit":{"cache_hit":false"#), "{text}");
        assert!(text.contains(r#""ok":true"#), "{text}");
        assert!(!text.contains("event: error\n"), "{text}");
        assert_eq!(ask_audit_rows(&server).len(), 1);
    }

    #[test]
    fn http_query_ask_stream_socket_uses_sse_without_content_length() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = StreamingOpenAiStub::start(vec!["streamed ", "answer"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        let listener = TcpListener::bind("127.0.0.1:0").expect("server bind");
        let addr = listener.local_addr().expect("server addr");
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept request");
            server.handle_connection(stream).expect("handle request");
        });

        let body = br#"{"query": "ASK 'why did login fail?' STREAM"}"#;
        let request = format!(
            "POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).expect("utf8 body")
        );
        let mut client = TcpStream::connect(addr).expect("connect server");
        client.write_all(request.as_bytes()).expect("write request");
        client.shutdown(Shutdown::Write).expect("shutdown request");

        let mut response = String::new();
        client.read_to_string(&mut response).expect("read response");
        handle.join().expect("server thread");

        assert_eq!(stub.request_count(), 1);
        let (headers, body) = response.split_once("\r\n\r\n").expect("http response");
        let lower_headers = headers.to_ascii_lowercase();
        assert!(headers.starts_with("HTTP/1.1 200 OK"), "{headers}");
        assert!(
            lower_headers.contains("content-type: text/event-stream"),
            "{headers}"
        );
        assert!(
            lower_headers.contains("cache-control: no-cache"),
            "{headers}"
        );
        assert!(!lower_headers.contains("content-length:"), "{headers}");
        assert!(body.contains(r#"data: {"text":"streamed "}"#), "{body}");
        assert!(body.contains(r#"data: {"text":"answer"}"#), "{body}");
        assert_eq!(body.matches("event: answer_token\n").count(), 2, "{body}");
        assert_eq!(body.matches("event: validation\n").count(), 1, "{body}");
    }

    #[test]
    fn http_query_ask_stream_socket_flushes_provider_tokens_before_completion() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = StreamingOpenAiStub::start_delayed(
            vec!["streamed ", "answer"],
            Duration::from_millis(250),
        );
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        let audit_server = server.clone();
        let listener = TcpListener::bind("127.0.0.1:0").expect("server bind");
        let addr = listener.local_addr().expect("server addr");
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept request");
            server.handle_connection(stream).expect("handle request");
        });

        let body = br#"{"query": "ASK 'why did login fail?' STREAM STRICT OFF"}"#;
        let request = format!(
            "POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).expect("utf8 body")
        );
        let mut client = TcpStream::connect(addr).expect("connect server");
        client.write_all(request.as_bytes()).expect("write request");
        client.shutdown(Shutdown::Write).expect("shutdown request");
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");

        let mut response = String::new();
        read_until_contains(&mut client, &mut response, r#"data: {"text":"streamed "}"#);
        assert!(response.contains("event: sources\n"), "{response}");
        assert!(
            response.contains(r#"data: {"text":"streamed "}"#),
            "{response}"
        );
        assert!(
            !response.contains(r#"data: {"text":"answer"}"#),
            "{response}"
        );
        assert!(!response.contains("event: validation\n"), "{response}");

        client.read_to_string(&mut response).expect("read response");
        handle.join().expect("server thread");

        assert_eq!(stub.request_count(), 1);
        assert!(
            response.contains(r#"data: {"text":"answer"}"#),
            "{response}"
        );
        assert_eq!(
            response.matches("event: answer_token\n").count(),
            2,
            "{response}"
        );
        assert_eq!(
            response.matches("event: validation\n").count(),
            1,
            "{response}"
        );
        assert_eq!(ask_audit_rows(&audit_server).len(), 1);
    }

    #[test]
    fn http_query_ask_stream_midstream_cost_guard_emits_error_after_partial_token() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = StreamingOpenAiStub::start(vec!["abcd", "efgh"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        configure_ask_stub_runtime(&server);
        server
            .runtime
            .execute_query("SET CONFIG ask.max_completion_tokens = 1")
            .expect("set completion guard");
        let listener = TcpListener::bind("127.0.0.1:0").expect("server bind");
        let addr = listener.local_addr().expect("server addr");
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept request");
            server.handle_connection(stream).expect("handle request");
        });

        let body = br#"{"query": "ASK 'why did login fail?' STREAM STRICT OFF"}"#;
        let request = format!(
            "POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).expect("utf8 body")
        );
        let mut client = TcpStream::connect(addr).expect("connect server");
        client.write_all(request.as_bytes()).expect("write request");
        client.shutdown(Shutdown::Write).expect("shutdown request");

        let mut response = String::new();
        client.read_to_string(&mut response).expect("read response");
        handle.join().expect("server thread");

        assert_eq!(stub.request_count(), 1);
        assert!(response.contains("event: sources\n"), "{response}");
        assert!(response.contains(r#"data: {"text":"abcd"}"#), "{response}");
        assert!(!response.contains(r#"data: {"text":"efgh"}"#), "{response}");
        assert!(response.contains("event: error\n"), "{response}");
        assert!(response.contains(r#""code":413"#), "{response}");
        assert!(response.contains("max_completion_tokens"), "{response}");
        assert!(!response.contains("event: validation\n"), "{response}");
    }

    #[test]
    fn http_query_ask_stream_cost_guard_returns_sse_error_frame() {
        let server = make_server();
        server
            .runtime
            .execute_query("SET CONFIG ask.max_prompt_tokens = 1")
            .expect("set prompt guard");

        let r = server.handle_query(br#"{"query": "ASK 'why did login fail?' STREAM"}"#.to_vec());

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(r.content_type, "text/event-stream");
        let text = body_str(&r);
        assert!(text.starts_with("event: error\n"), "{text}");
        assert!(text.contains(r#""code":413"#), "{text}");
        assert!(text.contains("max_prompt_tokens"), "{text}");
        assert!(!text.contains("event: validation\n"), "{text}");
    }

    #[test]
    fn http_query_explain_ask_returns_plan_without_llm_call() {
        let _guard = ASK_ENV_LOCK.lock().expect("env lock");
        let stub = SequenceOpenAiStub::start(vec!["should not be called"]);
        let _api_base =
            EnvVarGuard::set("REDDB_OPENAI_API_BASE", &format!("http://{}", stub.addr()));
        let _api_key = EnvVarGuard::unset("REDDB_OPENAI_API_KEY");

        let server = make_server();
        assert_eq!(
            server
                .handle_query(br#"{"query": "CREATE TABLE incidents (body TEXT)"}"#.to_vec())
                .status,
            200
        );
        assert_eq!(
            server
                .handle_query(
                    br#"{"query": "INSERT INTO incidents (body) VALUES ('login failed FDD-12313')"}"#.to_vec(),
                )
                .status,
            200
        );

        let r = server.handle_query(
            br#"{"query": "EXPLAIN ASK 'incidents FDD-12313' USING openai LIMIT 3 MIN_SCORE 0.7 DEPTH 2"}"#.to_vec(),
        );

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(stub.request_count(), 0);
        let text = body_str(&r);
        assert!(text.contains(r#""statement":"explain_ask""#), "{text}");
        assert!(text.contains(r#""retrieval""#), "{text}");
        assert!(text.contains(r#""bucket":"bm25""#), "{text}");
        assert!(text.contains(r#""bucket":"vector""#), "{text}");
        assert!(text.contains(r#""bucket":"graph""#), "{text}");
        assert!(text.contains(r#""limit":3"#), "{text}");
        assert!(text.contains(r#""depth":2"#), "{text}");
        assert!(text.contains(r#""provider":{"model":"#), "{text}");
        assert!(text.contains(r#""name":"openai""#), "{text}");
        assert!(text.contains(r#""prompt_tokens""#), "{text}");
        assert!(text.contains("reddb:incidents/"), "{text}");
        assert!(!text.contains("should not be called"), "{text}");
    }

    fn configure_ask_stub_runtime(server: &RedDBServer) {
        server
            .runtime
            .execute_query("SET CONFIG runtime.ai.transport_retry_max_attempts = 1")
            .expect("disable transport retries");
        server
            .runtime
            .execute_query("SET CONFIG red.config.ai.openai.default.key = 'sk-test'")
            .expect("set api key");
    }

    fn ask_audit_rows(server: &RedDBServer) -> Vec<BTreeMap<String, crate::json::Value>> {
        let manager = server
            .runtime
            .db()
            .store()
            .get_collection("red_ask_audit")
            .expect("red_ask_audit collection");
        manager
            .query_all(|entity| entity.data.as_row().is_some())
            .into_iter()
            .map(|entity| {
                let row = entity.data.as_row().expect("audit row");
                row.iter_fields()
                    .map(|(key, value)| {
                        (
                            key.to_string(),
                            crate::presentation::entity_json::storage_value_to_json(value),
                        )
                    })
                    .collect()
            })
            .collect()
    }

    struct EnvVarGuard {
        name: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, previous }
        }

        fn unset(name: &'static str) -> Self {
            let previous = std::env::var(name).ok();
            std::env::remove_var(name);
            Self { name, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    struct SlowOpenAiStub {
        addr: SocketAddr,
        shutdown: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl SlowOpenAiStub {
        fn start(delay: Duration) -> Self {
            Self::start_with_completion(delay, 3)
        }

        fn start_with_completion(delay: Duration, completion_tokens: u64) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("stub bind");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let addr = listener.local_addr().expect("local addr");
            let shutdown = Arc::new(AtomicBool::new(false));
            let server_shutdown = Arc::clone(&shutdown);
            let handle = thread::spawn(move || {
                while !server_shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            read_stub_request(&mut stream);
                            thread::sleep(delay);
                            write_openai_response(&mut stream, completion_tokens);
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                addr,
                shutdown,
                handle: Some(handle),
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }
    }

    impl Drop for SlowOpenAiStub {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.addr);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn read_stub_request(stream: &mut TcpStream) {
        let _ = read_stub_request_text(stream);
    }

    fn read_stub_request_text(stream: &mut TcpStream) -> String {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
        let mut out = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    out.extend_from_slice(&buffer[..n]);
                    if out.len() > 256 * 1024 {
                        break;
                    }
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&out).to_string()
    }

    fn write_openai_response(stream: &mut TcpStream, completion_tokens: u64) {
        let total_tokens = 12 + completion_tokens;
        let body = format!(
            r#"{{"model":"test-model","choices":[{{"message":{{"content":"login failed [^1]"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":12,"completion_tokens":{completion_tokens},"total_tokens":{total_tokens}}}}}"#
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write stub response");
    }

    fn read_until_contains(stream: &mut TcpStream, out: &mut String, needle: &str) {
        let mut buffer = [0_u8; 256];
        while !out.contains(needle) {
            let read = stream.read(&mut buffer).expect("read response chunk");
            assert!(read > 0, "connection closed before {needle}: {out}");
            out.push_str(std::str::from_utf8(&buffer[..read]).expect("utf8 response"));
        }
    }

    struct SequenceOpenAiStub {
        addr: SocketAddr,
        shutdown: Arc<AtomicBool>,
        requests: Arc<std::sync::atomic::AtomicUsize>,
        handle: Option<JoinHandle<()>>,
    }

    impl SequenceOpenAiStub {
        fn start(outputs: Vec<&'static str>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("stub bind");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let addr = listener.local_addr().expect("local addr");
            let shutdown = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let server_shutdown = Arc::clone(&shutdown);
            let server_requests = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                while !server_shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            read_stub_request(&mut stream);
                            let index = server_requests.fetch_add(1, Ordering::Relaxed);
                            let output = outputs
                                .get(index)
                                .or_else(|| outputs.last())
                                .copied()
                                .unwrap_or("");
                            write_openai_text_response(&mut stream, output);
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                addr,
                shutdown,
                requests,
                handle: Some(handle),
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }

        fn request_count(&self) -> usize {
            self.requests.load(Ordering::Relaxed)
        }
    }

    impl Drop for SequenceOpenAiStub {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.addr);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    struct StreamingOpenAiStub {
        addr: SocketAddr,
        shutdown: Arc<AtomicBool>,
        requests: Arc<std::sync::atomic::AtomicUsize>,
        handle: Option<JoinHandle<()>>,
    }

    impl StreamingOpenAiStub {
        fn start(chunks: Vec<&'static str>) -> Self {
            Self::start_with_delay(chunks, None)
        }

        fn start_delayed(chunks: Vec<&'static str>, delay_between_chunks: Duration) -> Self {
            Self::start_with_delay(chunks, Some(delay_between_chunks))
        }

        fn start_with_delay(
            chunks: Vec<&'static str>,
            delay_between_chunks: Option<Duration>,
        ) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("stub bind");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let addr = listener.local_addr().expect("local addr");
            let shutdown = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let server_shutdown = Arc::clone(&shutdown);
            let server_requests = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                while !server_shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            read_stub_request(&mut stream);
                            server_requests.fetch_add(1, Ordering::Relaxed);
                            write_openai_streaming_response(
                                &mut stream,
                                &chunks,
                                delay_between_chunks,
                            );
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                addr,
                shutdown,
                requests,
                handle: Some(handle),
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }

        fn request_count(&self) -> usize {
            self.requests.load(Ordering::Relaxed)
        }
    }

    impl Drop for StreamingOpenAiStub {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.addr);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn write_openai_text_response(stream: &mut TcpStream, output: &str) {
        let escaped = output.replace('\\', "\\\\").replace('"', "\\\"");
        let body = format!(
            r#"{{"model":"test-model","choices":[{{"message":{{"content":"{escaped}"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}}}"#
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write stub response");
    }

    fn write_openai_streaming_response(
        stream: &mut TcpStream,
        chunks: &[&str],
        delay_between_chunks: Option<Duration>,
    ) {
        if let Some(delay) = delay_between_chunks {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                )
                .expect("write stub streaming headers");
            for chunk in chunks {
                let escaped = chunk.replace('\\', "\\\\").replace('"', "\\\"");
                let frame = format!(
                    r#"data: {{"model":"test-model","choices":[{{"delta":{{"content":"{escaped}"}},"finish_reason":null}}]}}"#
                );
                stream
                    .write_all(frame.as_bytes())
                    .and_then(|_| stream.write_all(b"\n\n"))
                    .and_then(|_| stream.flush())
                    .expect("write delayed provider chunk");
                thread::sleep(delay);
            }
            let usage = format!(
                r#"data: {{"model":"test-model","choices":[{{"delta":{{}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":12,"completion_tokens":{},"total_tokens":{}}}}}"#,
                chunks.len(),
                12 + chunks.len()
            );
            stream
                .write_all(usage.as_bytes())
                .and_then(|_| stream.write_all(b"\n\ndata: [DONE]\n\n"))
                .and_then(|_| stream.flush())
                .expect("write delayed provider terminal chunk");
            return;
        }

        let mut body = String::new();
        for chunk in chunks {
            let escaped = chunk.replace('\\', "\\\\").replace('"', "\\\"");
            body.push_str(&format!(
                r#"data: {{"model":"test-model","choices":[{{"delta":{{"content":"{escaped}"}},"finish_reason":null}}]}}"#
            ));
            body.push_str("\n\n");
        }
        body.push_str(&format!(
            r#"data: {{"model":"test-model","choices":[{{"delta":{{}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":12,"completion_tokens":{},"total_tokens":{}}}}}"#,
            chunks.len(),
            12 + chunks.len()
        ));
        body.push_str("\n\ndata: [DONE]\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write stub streaming response");
    }

    struct StatusOpenAiStub {
        addr: SocketAddr,
        shutdown: Arc<AtomicBool>,
        requests: Arc<std::sync::atomic::AtomicUsize>,
        request_bodies: Arc<Mutex<Vec<String>>>,
        handle: Option<JoinHandle<()>>,
    }

    impl StatusOpenAiStub {
        fn start(status: u16, body: &'static str, output: Option<&'static str>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("stub bind");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let addr = listener.local_addr().expect("local addr");
            let shutdown = Arc::new(AtomicBool::new(false));
            let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let request_bodies = Arc::new(Mutex::new(Vec::new()));
            let server_shutdown = Arc::clone(&shutdown);
            let server_requests = Arc::clone(&requests);
            let server_request_bodies = Arc::clone(&request_bodies);
            let handle = thread::spawn(move || {
                while !server_shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request_body = read_stub_request_text(&mut stream);
                            server_requests.fetch_add(1, Ordering::Relaxed);
                            server_request_bodies
                                .lock()
                                .expect("request bodies lock")
                                .push(request_body);
                            if let Some(output) = output {
                                write_openai_text_response(&mut stream, output);
                            } else {
                                write_status_response(&mut stream, status, body);
                            }
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                addr,
                shutdown,
                requests,
                request_bodies,
                handle: Some(handle),
            }
        }

        fn addr(&self) -> SocketAddr {
            self.addr
        }

        fn request_count(&self) -> usize {
            self.requests.load(Ordering::Relaxed)
        }

        fn request_body(&self, index: usize) -> String {
            self.request_bodies
                .lock()
                .expect("request bodies lock")
                .get(index)
                .cloned()
                .unwrap_or_default()
        }
    }

    impl Drop for StatusOpenAiStub {
        fn drop(&mut self) {
            self.shutdown.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.addr);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    fn write_status_response(stream: &mut TcpStream, status: u16, body: &str) {
        let response = format!(
            "HTTP/1.1 {status} Test\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write stub response");
    }
}
