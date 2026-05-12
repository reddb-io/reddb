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
                        Err(err) => return json_error(400, err.to_string()),
                    },
                    Err(err) => return json_error(400, err.to_string()),
                }
            }
            None => self.query_use_cases().execute(ExecuteQueryInput { query }),
        };

        match exec_result {
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
            Err(err) => {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::runtime::RedDBRuntime;
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
    fn http_query_params_arity_mismatch_returns_400() {
        let server = make_server();
        let ddl = br#"{"query": "CREATE TABLE pa (id INTEGER)"}"#;
        assert_eq!(server.handle_query(ddl.to_vec()).status, 200);

        let body = br#"{"query": "SELECT * FROM pa WHERE id = $1", "params": [1, 2]}"#;
        let r = server.handle_query(body.to_vec());
        assert_eq!(r.status, 400, "body: {}", body_str(&r));
        let text = body_str(&r).to_lowercase();
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
        assert!(body_str(&r).contains("params"));
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
}
