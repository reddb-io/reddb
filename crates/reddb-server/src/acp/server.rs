//! Stable ACP v1 session adapter over the RedDB ASK harness.

use std::collections::HashMap;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use parking_lot::Mutex;

use super::protocol;
use crate::json::{from_str as json_from_str, Map, Value as JsonValue};
use crate::runtime::ask_pipeline::AskScopeGuard;
use crate::runtime::execution_context::{self, SnapshotBundle};
use crate::runtime::query_budget::RequestCancellation;
use crate::runtime::RedDBRuntime;

const ACP_PROTOCOL_VERSION: i64 = 1;
const OUTPUT_QUEUE_CAPACITY: usize = 256;
const MAX_SESSIONS: usize = 64;
const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 128 * 1024;

pub struct AcpServer {
    runtime: RedDBRuntime,
    state: Arc<Mutex<ServerState>>,
    next_session: AtomicU64,
}

struct ServerState {
    initialized: bool,
    sessions: HashMap<String, Arc<AcpSession>>,
}

struct AcpSession {
    identity: SnapshotBundle,
    active: Mutex<Option<ActiveTurn>>,
    next_tool_call: AtomicU64,
}

#[derive(Clone)]
struct ActiveTurn {
    cancellation: RequestCancellation,
    cancelled: Arc<AtomicBool>,
}

struct PromptJob {
    id: JsonValue,
    session_id: String,
    question: String,
    session: Arc<AcpSession>,
}

enum Dispatch {
    Message(String),
    Prompt(PromptJob),
    None,
}

enum BoundedLine {
    Eof,
    Message,
    TooLarge,
}

impl AcpServer {
    pub fn new(runtime: RedDBRuntime) -> Self {
        Self {
            runtime,
            state: Arc::new(Mutex::new(ServerState {
                initialized: false,
                sessions: HashMap::new(),
            })),
            next_session: AtomicU64::new(1),
        }
    }

    /// Serve stable ACP v1 over newline-delimited JSON-RPC on stdio.
    /// Prompt turns run on dedicated workers so the reader can process
    /// `session/cancel` while ASK is executing.
    pub fn run_stdio(&self) {
        let (output_tx, output_rx) = mpsc::sync_channel::<String>(OUTPUT_QUEUE_CAPACITY);
        thread::spawn(move || {
            let stdout = io::stdout();
            let mut writer = io::BufWriter::new(stdout.lock());
            for message in output_rx {
                if protocol::write_message(&mut writer, &message).is_err() {
                    break;
                }
            }
        });

        let stdin = io::stdin();
        let mut reader = io::BufReader::new(stdin.lock());
        let mut line = Vec::new();
        loop {
            match read_bounded_line(&mut reader, &mut line) {
                Ok(BoundedLine::Eof) => break,
                Ok(BoundedLine::Message) => {}
                Ok(BoundedLine::TooLarge) => {
                    let _ = output_tx.send(protocol::build_error_message(
                        None,
                        -32600,
                        "message exceeds the 1 MiB limit",
                    ));
                    continue;
                }
                Err(error) => {
                    tracing::error!(target: "reddb::acp", err = %error, "ACP stdin read failed");
                    break;
                }
            }
            let Ok(payload) = std::str::from_utf8(&line) else {
                let _ = output_tx.send(protocol::build_error_message(None, -32700, "parse error"));
                continue;
            };
            let payload = payload.trim_end_matches(['\r', '\n']);
            if payload.is_empty() {
                continue;
            }
            let message = match json_from_str(payload) {
                Ok(message) => message,
                Err(_) => {
                    let _ =
                        output_tx.send(protocol::build_error_message(None, -32700, "parse error"));
                    continue;
                }
            };

            match self.dispatch(&message) {
                Dispatch::Message(message) => {
                    let _ = output_tx.send(message);
                }
                Dispatch::Prompt(job) => {
                    let runtime = self.runtime.clone();
                    let worker_tx = output_tx.clone();
                    thread::spawn(move || run_prompt(runtime, job, worker_tx));
                }
                Dispatch::None => {}
            }
        }

        self.cancel_all();
    }

    fn dispatch(&self, message: &JsonValue) -> Dispatch {
        if message.get("jsonrpc").and_then(JsonValue::as_str) != Some("2.0") {
            return Dispatch::Message(protocol::build_error_message(
                message.get("id"),
                -32600,
                "invalid JSON-RPC version",
            ));
        }
        let Some(method) = message.get("method").and_then(JsonValue::as_str) else {
            return Dispatch::Message(protocol::build_error_message(
                message.get("id"),
                -32600,
                "invalid request",
            ));
        };
        let id = message.get("id");
        let params = message.get("params");

        match method {
            "initialize" => Dispatch::Message(self.initialize(id, params)),
            "session/new" => Dispatch::Message(self.new_session(id, params)),
            "session/prompt" => self.prompt(id, params),
            "session/cancel" => {
                self.cancel(params);
                Dispatch::None
            }
            "session/close" => Dispatch::Message(self.close_session(id, params)),
            _ => Dispatch::Message(protocol::build_error_message(
                id,
                -32601,
                &format!("method not found: {method}"),
            )),
        }
    }

    fn initialize(&self, id: Option<&JsonValue>, params: Option<&JsonValue>) -> String {
        if id.is_none() {
            return protocol::build_error_message(None, -32600, "initialize requires an id");
        }
        if params
            .and_then(|value| value.get("protocolVersion"))
            .and_then(JsonValue::as_i64)
            .is_none()
        {
            return protocol::build_error_message(id, -32602, "protocolVersion is required");
        }

        self.state.lock().initialized = true;

        let mut prompt_capabilities = Map::new();
        prompt_capabilities.insert("audio".into(), JsonValue::Bool(false));
        prompt_capabilities.insert("embeddedContext".into(), JsonValue::Bool(false));
        prompt_capabilities.insert("image".into(), JsonValue::Bool(false));

        let mut mcp_capabilities = Map::new();
        mcp_capabilities.insert("http".into(), JsonValue::Bool(false));
        mcp_capabilities.insert("sse".into(), JsonValue::Bool(false));

        let mut session_capabilities = Map::new();
        session_capabilities.insert("close".into(), JsonValue::Object(Map::new()));

        let mut capabilities = Map::new();
        capabilities.insert("auth".into(), JsonValue::Object(Map::new()));
        capabilities.insert("loadSession".into(), JsonValue::Bool(false));
        capabilities.insert(
            "mcpCapabilities".into(),
            JsonValue::Object(mcp_capabilities),
        );
        capabilities.insert(
            "promptCapabilities".into(),
            JsonValue::Object(prompt_capabilities),
        );
        capabilities.insert(
            "sessionCapabilities".into(),
            JsonValue::Object(session_capabilities),
        );

        let mut agent_info = Map::new();
        agent_info.insert("name".into(), JsonValue::String("reddb".into()));
        agent_info.insert("title".into(), JsonValue::String("RedDB ASK".into()));
        agent_info.insert(
            "version".into(),
            JsonValue::String(env!("CARGO_PKG_VERSION").into()),
        );

        let mut result = Map::new();
        result.insert("agentCapabilities".into(), JsonValue::Object(capabilities));
        result.insert("agentInfo".into(), JsonValue::Object(agent_info));
        result.insert("authMethods".into(), JsonValue::Array(Vec::new()));
        result.insert(
            "protocolVersion".into(),
            JsonValue::Integer(ACP_PROTOCOL_VERSION),
        );
        protocol::build_result_message(id, JsonValue::Object(result))
    }

    fn new_session(&self, id: Option<&JsonValue>, params: Option<&JsonValue>) -> String {
        if id.is_none() {
            return protocol::build_error_message(None, -32600, "session/new requires an id");
        }
        if !self.state.lock().initialized {
            return protocol::build_error_message(id, -32600, "initialize must be called first");
        }
        let Some(params) = params else {
            return protocol::build_error_message(id, -32602, "params are required");
        };
        let Some(cwd) = params.get("cwd").and_then(JsonValue::as_str) else {
            return protocol::build_error_message(id, -32602, "cwd is required");
        };
        if !Path::new(cwd).is_absolute() {
            return protocol::build_error_message(id, -32602, "cwd must be absolute");
        }
        let Some(mcp_servers) = params.get("mcpServers").and_then(JsonValue::as_array) else {
            return protocol::build_error_message(id, -32602, "mcpServers is required");
        };
        if !mcp_servers.is_empty() {
            return protocol::build_error_message(
                id,
                -32602,
                "external MCP servers are not supported by this ACP adapter",
            );
        }
        if params
            .get("additionalDirectories")
            .and_then(JsonValue::as_array)
            .is_some_and(|directories| !directories.is_empty())
            || params
                .get("systemPrompt")
                .is_some_and(|prompt| !matches!(prompt, &JsonValue::Null))
        {
            return protocol::build_error_message(
                id,
                -32602,
                "unadvertised session capabilities are not supported",
            );
        }

        let mut state = self.state.lock();
        if state.sessions.len() >= MAX_SESSIONS {
            return protocol::build_error_message(id, -32603, "session limit reached");
        }
        let sequence = self.next_session.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("reddb-{}-{sequence}", std::process::id());
        let mut identity = execution_context::snapshot_bundle();
        // Authorization belongs to the ACP session, but MVCC visibility does
        // not. Every prompt must capture a fresh statement snapshot so writes
        // committed after session/new remain visible.
        identity.snapshot = None;
        state.sessions.insert(
            session_id.clone(),
            Arc::new(AcpSession {
                identity,
                active: Mutex::new(None),
                next_tool_call: AtomicU64::new(1),
            }),
        );

        let mut result = Map::new();
        result.insert("sessionId".into(), JsonValue::String(session_id));
        protocol::build_result_message(id, JsonValue::Object(result))
    }

    fn prompt(&self, id: Option<&JsonValue>, params: Option<&JsonValue>) -> Dispatch {
        let Some(id) = id.cloned() else {
            return Dispatch::Message(protocol::build_error_message(
                None,
                -32600,
                "session/prompt requires an id",
            ));
        };
        let Some(params) = params else {
            return Dispatch::Message(protocol::build_error_message(
                Some(&id),
                -32602,
                "params are required",
            ));
        };
        let Some(session_id) = params.get("sessionId").and_then(JsonValue::as_str) else {
            return Dispatch::Message(protocol::build_error_message(
                Some(&id),
                -32602,
                "sessionId is required",
            ));
        };
        let session = {
            let state = self.state.lock();
            match state.sessions.get(session_id) {
                Some(session) => Arc::clone(session),
                None => {
                    return Dispatch::Message(protocol::build_error_message(
                        Some(&id),
                        -32002,
                        "session not found",
                    ));
                }
            }
        };
        let question = match prompt_text(params) {
            Ok(question) => question,
            Err(message) => {
                return Dispatch::Message(protocol::build_error_message(
                    Some(&id),
                    -32602,
                    &message,
                ));
            }
        };

        let mut active = session.active.lock();
        if active.is_some() {
            return Dispatch::Message(protocol::build_error_message(
                Some(&id),
                -32600,
                "session already has an active prompt",
            ));
        }
        *active = Some(ActiveTurn {
            cancellation: RequestCancellation::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        drop(active);

        Dispatch::Prompt(PromptJob {
            id,
            session_id: session_id.to_string(),
            question,
            session,
        })
    }

    fn cancel(&self, params: Option<&JsonValue>) {
        let Some(session_id) = params
            .and_then(|value| value.get("sessionId"))
            .and_then(JsonValue::as_str)
        else {
            return;
        };
        let session = self.state.lock().sessions.get(session_id).cloned();
        if let Some(session) = session {
            if let Some(active) = session.active.lock().as_ref() {
                active.cancelled.store(true, Ordering::Release);
                active.cancellation.cancel();
            }
        }
    }

    fn close_session(&self, id: Option<&JsonValue>, params: Option<&JsonValue>) -> String {
        let Some(session_id) = params
            .and_then(|value| value.get("sessionId"))
            .and_then(JsonValue::as_str)
        else {
            return protocol::build_error_message(id, -32602, "sessionId is required");
        };
        let Some(session) = self.state.lock().sessions.remove(session_id) else {
            return protocol::build_error_message(id, -32002, "session not found");
        };
        if let Some(active) = session.active.lock().as_ref() {
            active.cancelled.store(true, Ordering::Release);
            active.cancellation.cancel();
        }
        protocol::build_result_message(id, JsonValue::Object(Map::new()))
    }

    fn cancel_all(&self) {
        for session in self.state.lock().sessions.values() {
            if let Some(active) = session.active.lock().as_ref() {
                active.cancelled.store(true, Ordering::Release);
                active.cancellation.cancel();
            }
        }
    }
}

fn read_bounded_line<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> io::Result<BoundedLine> {
    line.clear();
    let mut too_large = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if too_large {
                BoundedLine::TooLarge
            } else if line.is_empty() {
                BoundedLine::Eof
            } else {
                BoundedLine::Message
            });
        }

        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let payload_bytes = newline.map_or(consumed, |index| index);
        if !too_large {
            if line.len().saturating_add(payload_bytes) > MAX_MESSAGE_BYTES {
                too_large = true;
            } else {
                line.extend_from_slice(&available[..consumed]);
            }
        }
        reader.consume(consumed);

        if newline.is_some() {
            return Ok(if too_large {
                BoundedLine::TooLarge
            } else {
                BoundedLine::Message
            });
        }
    }
}

fn prompt_text(params: &JsonValue) -> Result<String, String> {
    let blocks = params
        .get("prompt")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| "prompt is required".to_string())?;
    let mut parts = Vec::with_capacity(blocks.len());
    let mut bytes = 0_usize;
    for block in blocks {
        let block_type = block
            .get("type")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| "prompt block type is required".to_string())?;
        let part = match block_type {
            "text" => block
                .get("text")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| "text prompt block requires text".to_string())?
                .to_string(),
            "resource_link" => {
                let name = block
                    .get("name")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| "resource_link requires name".to_string())?;
                let uri = block
                    .get("uri")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| "resource_link requires uri".to_string())?;
                format!("Referenced resource: {name} ({uri})")
            }
            unsupported => {
                return Err(format!("unsupported prompt block type: {unsupported}"));
            }
        };
        bytes = bytes.saturating_add(part.len());
        if bytes > MAX_PROMPT_BYTES {
            return Err("prompt exceeds the 128 KiB limit".to_string());
        }
        parts.push(part);
    }
    let question = parts.join("\n\n");
    if question.trim().is_empty() {
        return Err("prompt must contain text or a resource link".to_string());
    }
    Ok(question)
}

fn run_prompt(runtime: RedDBRuntime, job: PromptJob, output: mpsc::SyncSender<String>) {
    let active = job.session.active.lock().clone();
    let Some(active) = active else {
        return;
    };
    let result = execution_context::with_snapshot_bundle(&job.session.identity, || {
        let _cancellation = active.cancellation.install();
        let scope = runtime.ai_scope();
        let _scope = AskScopeGuard::install(&runtime, &scope);
        runtime.execute_ask("ASK <acp>", &ask_query(job.question))
    });

    let cancelled = active.cancelled.load(Ordering::Acquire);
    if cancelled {
        let _ = output.send(prompt_response(&job.id, "cancelled"));
    } else {
        match result {
            Ok(result) => {
                let envelope = crate::presentation::query_result::tagged_summary(&result);
                for message in
                    turn_messages(&job.session_id, &envelope, &job.session.next_tool_call)
                {
                    let _ = output.send(message);
                }
                let _ = output.send(prompt_response(&job.id, "end_turn"));
            }
            Err(error) => {
                let _ = output.send(protocol::build_error_message(
                    Some(&job.id),
                    -32603,
                    &error.to_string(),
                ));
            }
        }
    }
    *job.session.active.lock() = None;
}

fn ask_query(question: String) -> reddb_rql::ast::AskQuery {
    reddb_rql::ast::AskQuery {
        explain: false,
        question,
        question_param: None,
        provider: None,
        model: None,
        depth: None,
        limit: None,
        min_score: None,
        collection: None,
        temperature: None,
        seed: None,
        strict: true,
        stream: false,
        cache: reddb_rql::ast::AskCacheClause::Default,
        plan_only: false,
        steps: None,
    }
}

fn turn_messages(
    session_id: &str,
    envelope: &JsonValue,
    next_tool_call: &AtomicU64,
) -> Vec<String> {
    let mut messages = Vec::new();
    if let Some(summary) = envelope
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("summary"))
        .and_then(JsonValue::as_str)
        .filter(|summary| !summary.is_empty())
    {
        messages.push(session_text_update(
            session_id,
            "agent_thought_chunk",
            summary,
        ));
    }

    if let Some(steps) = envelope
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("steps"))
        .and_then(JsonValue::as_array)
    {
        for step in steps {
            let tool_call_id = next_tool_call.fetch_add(1, Ordering::Relaxed);
            messages.push(session_tool_update(session_id, tool_call_id, step));
        }
    }

    let answer = envelope
        .get("answer")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    messages.push(session_text_update(
        session_id,
        "agent_message_chunk",
        answer,
    ));
    messages
}

fn session_text_update(session_id: &str, kind: &str, text: &str) -> String {
    let mut content = Map::new();
    content.insert("text".into(), JsonValue::String(text.to_string()));
    content.insert("type".into(), JsonValue::String("text".into()));

    let mut update = Map::new();
    update.insert("content".into(), JsonValue::Object(content));
    update.insert("sessionUpdate".into(), JsonValue::String(kind.to_string()));

    session_update(session_id, JsonValue::Object(update))
}

fn session_tool_update(session_id: &str, sequence: u64, step: &JsonValue) -> String {
    let name = step
        .get("tool")
        .and_then(JsonValue::as_str)
        .unwrap_or("reddb_query");
    let title = step
        .get("purpose")
        .and_then(JsonValue::as_str)
        .unwrap_or(name);

    let mut update = Map::new();
    update.insert("name".into(), JsonValue::String(name.to_string()));
    update.insert("rawInput".into(), step.clone());
    if let Some(output) = step.get("observation") {
        update.insert("rawOutput".into(), output.clone());
    }
    update.insert(
        "sessionUpdate".into(),
        JsonValue::String("tool_call".into()),
    );
    update.insert("status".into(), JsonValue::String("completed".into()));
    update.insert("title".into(), JsonValue::String(title.to_string()));
    update.insert(
        "toolCallId".into(),
        JsonValue::String(format!("reddb-{sequence}")),
    );
    session_update(session_id, JsonValue::Object(update))
}

fn session_update(session_id: &str, update: JsonValue) -> String {
    let mut params = Map::new();
    params.insert(
        "sessionId".into(),
        JsonValue::String(session_id.to_string()),
    );
    params.insert("update".into(), update);
    protocol::build_notification("session/update", JsonValue::Object(params))
}

fn prompt_response(id: &JsonValue, stop_reason: &str) -> String {
    let mut result = Map::new();
    result.insert(
        "stopReason".into(),
        JsonValue::String(stop_reason.to_string()),
    );
    protocol::build_result_message(Some(id), JsonValue::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_line_is_drained_without_hiding_the_next_message() {
        let mut input = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        input.extend_from_slice(b"\n{}\n");
        let mut reader = io::Cursor::new(input);
        let mut line = Vec::new();

        assert!(matches!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            BoundedLine::TooLarge
        ));
        assert!(line.len() <= MAX_MESSAGE_BYTES);
        assert!(matches!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            BoundedLine::Message
        ));
        assert_eq!(line, b"{}\n");
    }

    fn object(entries: Vec<(&str, JsonValue)>) -> JsonValue {
        JsonValue::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    fn request(id: i64, method: &str, params: JsonValue) -> JsonValue {
        object(vec![
            ("id", JsonValue::Integer(id)),
            ("jsonrpc", JsonValue::String("2.0".into())),
            ("method", JsonValue::String(method.into())),
            ("params", params),
        ])
    }

    fn initialized_server() -> AcpServer {
        let server = AcpServer::new(RedDBRuntime::in_memory().unwrap());
        let response = server.dispatch(&request(
            1,
            "initialize",
            object(vec![("protocolVersion", JsonValue::Integer(1))]),
        ));
        assert!(matches!(response, Dispatch::Message(_)));
        server
    }

    fn new_session(server: &AcpServer) -> (String, Arc<AcpSession>) {
        let params = object(vec![
            (
                "cwd",
                JsonValue::String(std::env::current_dir().unwrap().display().to_string()),
            ),
            ("mcpServers", JsonValue::Array(Vec::new())),
        ]);
        let Dispatch::Message(response) = server.dispatch(&request(2, "session/new", params))
        else {
            panic!("new session response");
        };
        let response: JsonValue = json_from_str(&response).unwrap();
        let session_id = response
            .get("result")
            .and_then(|result| result.get("sessionId"))
            .and_then(JsonValue::as_str)
            .unwrap()
            .to_string();
        let session = server
            .state
            .lock()
            .sessions
            .get(&session_id)
            .cloned()
            .unwrap();
        (session_id, session)
    }

    #[test]
    fn initialize_advertises_only_implemented_capabilities() {
        let server = AcpServer::new(RedDBRuntime::in_memory().unwrap());
        let Dispatch::Message(response) = server.dispatch(&request(
            1,
            "initialize",
            object(vec![("protocolVersion", JsonValue::Integer(1))]),
        )) else {
            panic!("initialize response");
        };
        let response: JsonValue = json_from_str(&response).unwrap();
        let result = response.get("result").unwrap();
        assert_eq!(
            result.get("protocolVersion").and_then(JsonValue::as_i64),
            Some(1)
        );
        assert!(result
            .get("authMethods")
            .and_then(JsonValue::as_array)
            .unwrap()
            .is_empty());
        assert!(result
            .get("agentCapabilities")
            .and_then(|capabilities| capabilities.get("auth"))
            .and_then(JsonValue::as_object)
            .unwrap()
            .is_empty());
        assert!(result
            .get("agentCapabilities")
            .and_then(|capabilities| capabilities.get("sessionCapabilities"))
            .and_then(|sessions| sessions.get("close"))
            .is_some());
    }

    #[test]
    fn session_creation_requires_initialize_and_rejects_external_mcp() {
        let runtime = RedDBRuntime::in_memory().unwrap();
        let server = AcpServer::new(runtime);
        let params = object(vec![
            (
                "cwd",
                JsonValue::String(std::env::current_dir().unwrap().display().to_string()),
            ),
            ("mcpServers", JsonValue::Array(Vec::new())),
        ]);
        let Dispatch::Message(response) = server.dispatch(&request(1, "session/new", params))
        else {
            panic!("new session response");
        };
        assert!(response.contains("initialize must be called first"));

        let server = initialized_server();
        let params = object(vec![
            (
                "cwd",
                JsonValue::String(std::env::current_dir().unwrap().display().to_string()),
            ),
            ("mcpServers", JsonValue::Array(vec![object(Vec::new())])),
        ]);
        let Dispatch::Message(response) = server.dispatch(&request(2, "session/new", params))
        else {
            panic!("new session response");
        };
        assert!(response.contains("external MCP servers are not supported"));
    }

    #[test]
    fn prompt_accepts_text_and_baseline_resource_links() {
        let params = object(vec![(
            "prompt",
            JsonValue::Array(vec![
                object(vec![
                    ("text", JsonValue::String("Find the owner".into())),
                    ("type", JsonValue::String("text".into())),
                ]),
                object(vec![
                    ("name", JsonValue::String("passport".into())),
                    ("type", JsonValue::String("resource_link".into())),
                    ("uri", JsonValue::String("reddb://people/123".into())),
                ]),
            ]),
        )]);
        assert_eq!(
            prompt_text(&params).unwrap(),
            "Find the owner\n\nReferenced resource: passport (reddb://people/123)"
        );
    }

    #[test]
    fn session_captures_identity_but_not_a_stale_snapshot() {
        struct ResetIdentity;
        impl Drop for ResetIdentity {
            fn drop(&mut self) {
                crate::runtime::impl_core::clear_current_auth_identity();
                crate::runtime::impl_core::clear_current_tenant();
            }
        }
        let _reset = ResetIdentity;
        crate::runtime::impl_core::set_current_tenant("tenant-a".into());
        crate::runtime::impl_core::set_current_auth_identity(
            "alice".into(),
            crate::auth::Role::Read,
        );

        let server = initialized_server();
        let (_, session) = new_session(&server);

        crate::runtime::impl_core::set_current_tenant("tenant-b".into());
        crate::runtime::impl_core::set_current_auth_identity(
            "bob".into(),
            crate::auth::Role::Admin,
        );
        assert_eq!(session.identity.tenant.as_deref(), Some("tenant-a"));
        assert_eq!(
            session
                .identity
                .auth
                .as_ref()
                .map(|(user, role)| (user.as_str(), *role)),
            Some(("alice", crate::auth::Role::Read))
        );
        assert_eq!(
            session
                .identity
                .auth_principal
                .as_ref()
                .map(|principal| principal.username.as_str()),
            Some("alice")
        );
        assert!(session.identity.snapshot.is_none());
    }

    #[test]
    fn cancel_notification_reaches_the_active_query_token() {
        let server = initialized_server();
        let (session_id, session) = new_session(&server);
        let prompt = object(vec![
            (
                "prompt",
                JsonValue::Array(vec![object(vec![
                    ("text", JsonValue::String("Who is active?".into())),
                    ("type", JsonValue::String("text".into())),
                ])]),
            ),
            ("sessionId", JsonValue::String(session_id.clone())),
        ]);
        assert!(matches!(
            server.dispatch(&request(3, "session/prompt", prompt)),
            Dispatch::Prompt(_)
        ));

        let cancel = object(vec![
            ("jsonrpc", JsonValue::String("2.0".into())),
            ("method", JsonValue::String("session/cancel".into())),
            (
                "params",
                object(vec![("sessionId", JsonValue::String(session_id))]),
            ),
        ]);
        assert!(matches!(server.dispatch(&cancel), Dispatch::None));
        assert!(session
            .active
            .lock()
            .as_ref()
            .unwrap()
            .cancelled
            .load(Ordering::Acquire));
    }

    #[test]
    fn ask_trace_becomes_thought_tools_and_answer_updates() {
        let step = object(vec![
            ("purpose", JsonValue::String("Validate the answer".into())),
            (
                "sql",
                JsonValue::String("SELECT name FROM people WHERE passport = $1".into()),
            ),
            ("tool", JsonValue::String("reddb_final_query".into())),
        ]);
        let envelope = object(vec![
            ("answer", JsonValue::String("Ana is active.".into())),
            (
                "reasoning",
                object(vec![
                    ("steps", JsonValue::Array(vec![step])),
                    ("summary", JsonValue::String("Located the person.".into())),
                ]),
            ),
        ]);
        let next_tool_call = AtomicU64::new(1);
        let messages = turn_messages("s-1", &envelope, &next_tool_call);
        assert_eq!(messages.len(), 3);
        assert!(messages[0].contains("agent_thought_chunk"));
        assert!(messages[1].contains("reddb_final_query"));
        assert!(messages[1].contains("SELECT name FROM people"));
        assert!(messages[2].contains("Ana is active."));

        let next_messages = turn_messages("s-1", &envelope, &next_tool_call);
        assert!(messages[1].contains("reddb-1"));
        assert!(next_messages[1].contains("reddb-2"));
    }
}
