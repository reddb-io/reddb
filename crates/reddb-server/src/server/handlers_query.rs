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
}

fn is_stream_ask_query(query: &str) -> bool {
    matches!(
        crate::storage::query::modes::parse_multi(query),
        Ok(crate::storage::query::ast::QueryExpr::Ask(ask)) if ask.stream
    )
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
    body.push_str(&encode(&Frame::AnswerToken {
        text: schema_text_field(row, "answer")?,
    }));
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
    use std::net::{SocketAddr, TcpListener, TcpStream};
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
            br#"{"query": "ASK 'why did login fail?' USING 'groq,openai'"}"#.to_vec(),
        );

        assert_eq!(r.status, 200, "{}", body_str(&r));
        assert_eq!(groq.request_count(), 1);
        assert_eq!(openai.request_count(), 1);
        let text = body_str(&r);
        assert!(text.contains("openai answered"), "{text}");
        assert!(text.contains(r#""provider":"openai""#), "{text}");
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
        let stub = SequenceOpenAiStub::start(vec!["streamed answer"]);
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
        assert!(
            text.contains(r#"data: {"text":"streamed answer"}"#),
            "{text}"
        );
        assert!(text.contains(r#""audit":{"cache_hit":false"#), "{text}");
        assert!(text.contains(r#""ok":true"#), "{text}");
        assert!(!text.contains("event: error\n"), "{text}");
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
        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
        let mut buffer = [0_u8; 1024];
        let _ = stream.read(&mut buffer);
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

    struct StatusOpenAiStub {
        addr: SocketAddr,
        shutdown: Arc<AtomicBool>,
        requests: Arc<std::sync::atomic::AtomicUsize>,
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
            let server_shutdown = Arc::clone(&shutdown);
            let server_requests = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                while !server_shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            read_stub_request(&mut stream);
                            server_requests.fetch_add(1, Ordering::Relaxed);
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
