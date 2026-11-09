//! Negotiated ACP v1/v2 session adapter over the RedDB ASK harness.

use std::collections::HashMap;
use std::io::{self, BufRead};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;

use parking_lot::{Condvar, Mutex};

use super::protocol;
use crate::json::{from_str as json_from_str, Map, Value as JsonValue};
use crate::runtime::ask_pipeline::AskScopeGuard;
use crate::runtime::execution_context::{self, SnapshotBundle};
use crate::runtime::query_budget::RequestCancellation;
use crate::runtime::RedDBRuntime;

const ACP_PROTOCOL_V1: i64 = 1;
const ACP_PROTOCOL_V2: i64 = 2;
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
    protocol_version: Option<ProtocolVersion>,
    sessions: HashMap<String, Arc<AcpSession>>,
}

struct AcpSession {
    identity: SnapshotBundle,
    cwd: String,
    active: Mutex<Option<ActiveTurn>>,
    active_finished: Condvar,
    next_message: AtomicU64,
    next_tool_call: AtomicU64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProtocolVersion {
    V1,
    V2,
}

#[derive(Clone)]
struct ActiveTurn {
    cancellation: RequestCancellation,
    cancelled: Arc<AtomicBool>,
}

struct ActivePromptGuard<'a> {
    session: &'a AcpSession,
}

impl Drop for ActivePromptGuard<'_> {
    fn drop(&mut self) {
        *self.session.active.lock() = None;
        self.session.active_finished.notify_all();
    }
}

struct PromptJob {
    id: JsonValue,
    session_id: String,
    question: String,
    prompt: JsonValue,
    protocol_version: ProtocolVersion,
    user_message_id: Option<String>,
    session: Arc<AcpSession>,
}

enum Dispatch {
    Message(String),
    Messages(Vec<String>),
    Prompt {
        job: PromptJob,
        accepted: Option<String>,
    },
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
                protocol_version: None,
                sessions: HashMap::new(),
            })),
            next_session: AtomicU64::new(1),
        }
    }

    /// Serve negotiated ACP v1/v2 over newline-delimited JSON-RPC on stdio.
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
                Dispatch::Messages(messages) => {
                    for message in messages {
                        let _ = output_tx.send(message);
                    }
                }
                Dispatch::Prompt { job, accepted } => {
                    if let Some(accepted) = accepted {
                        let _ = output_tx.send(accepted);
                    }
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
        if id.is_some_and(|id| {
            !matches!(
                id,
                JsonValue::Null | JsonValue::Integer(_) | JsonValue::String(_)
            )
        }) {
            return Dispatch::Message(protocol::build_error_message(
                None,
                -32600,
                "request id must be a string or integer",
            ));
        }
        let params = message.get("params");

        if method == "initialize" {
            return Dispatch::Message(self.initialize(id, params));
        }
        let Some(protocol_version) = self.state.lock().protocol_version else {
            return Dispatch::Message(protocol::build_error_message(
                id,
                -32600,
                "initialize must be called first",
            ));
        };
        if protocol_version == ProtocolVersion::V2 && matches!(id, Some(JsonValue::Null)) {
            return Dispatch::Message(protocol::build_error_message(
                None,
                -32600,
                "ACP v2 request id must not be null",
            ));
        }

        match method {
            "session/new" => Dispatch::Message(self.new_session(protocol_version, id, params)),
            "session/list" if protocol_version == ProtocolVersion::V2 => {
                self.list_sessions(id, params)
            }
            "session/resume" if protocol_version == ProtocolVersion::V2 => {
                self.resume_session(id, params)
            }
            "session/prompt" => self.prompt(protocol_version, id, params),
            "session/cancel" => {
                self.cancel(params);
                Dispatch::None
            }
            "session/close" => Dispatch::Message(self.close_session(protocol_version, id, params)),
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
        let Some(requested_version) = params
            .and_then(|value| value.get("protocolVersion"))
            .and_then(JsonValue::as_i64)
        else {
            return protocol::build_error_message(id, -32602, "protocolVersion is required");
        };
        if requested_version <= 0 {
            return protocol::build_error_message(id, -32022, "unsupported protocol version");
        }
        if requested_version > u16::MAX as i64 {
            return protocol::build_error_message(id, -32602, "protocolVersion exceeds uint16");
        }
        if self.state.lock().protocol_version.is_some() {
            return protocol::build_error_message(id, -32600, "connection is already initialized");
        }

        let protocol_version = if requested_version >= ACP_PROTOCOL_V2 {
            ProtocolVersion::V2
        } else {
            ProtocolVersion::V1
        };
        if protocol_version == ProtocolVersion::V2 && matches!(id, Some(JsonValue::Null)) {
            return protocol::build_error_message(
                None,
                -32600,
                "ACP v2 request id must not be null",
            );
        }
        if protocol_version == ProtocolVersion::V2 {
            let valid_info = params
                .and_then(|value| value.get("info"))
                .is_some_and(|info| {
                    info.get("name").and_then(JsonValue::as_str).is_some()
                        && info.get("version").and_then(JsonValue::as_str).is_some()
                });
            if !valid_info {
                return protocol::build_error_message(
                    id,
                    -32602,
                    "ACP v2 initialize requires info.name and info.version",
                );
            }
        }
        self.state.lock().protocol_version = Some(protocol_version);

        match protocol_version {
            ProtocolVersion::V1 => initialize_v1(id),
            ProtocolVersion::V2 => initialize_v2(id),
        }
    }

    fn new_session(
        &self,
        protocol_version: ProtocolVersion,
        id: Option<&JsonValue>,
        params: Option<&JsonValue>,
    ) -> String {
        if id.is_none() {
            return protocol::build_error_message(None, -32600, "session/new requires an id");
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
        match params.get("mcpServers") {
            Some(JsonValue::Array(servers)) if servers.is_empty() => {}
            Some(JsonValue::Array(_)) => {
                return protocol::build_error_message(
                    id,
                    -32602,
                    "external MCP servers are not supported by this ACP adapter",
                );
            }
            None | Some(JsonValue::Null) if protocol_version == ProtocolVersion::V2 => {}
            _ => {
                return protocol::build_error_message(id, -32602, "mcpServers must be an array");
            }
        }
        if has_unadvertised_session_options(params) {
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
                cwd: cwd.to_string(),
                active: Mutex::new(None),
                active_finished: Condvar::new(),
                next_message: AtomicU64::new(1),
                next_tool_call: AtomicU64::new(1),
            }),
        );

        let mut result = Map::new();
        result.insert("sessionId".into(), JsonValue::String(session_id));
        protocol::build_result_message(id, JsonValue::Object(result))
    }

    fn list_sessions(&self, id: Option<&JsonValue>, params: Option<&JsonValue>) -> Dispatch {
        if id.is_none() {
            return Dispatch::Message(protocol::build_error_message(
                None,
                -32600,
                "session/list requires an id",
            ));
        }
        let cwd = match params.and_then(|value| value.get("cwd")) {
            None | Some(JsonValue::Null) => None,
            Some(JsonValue::String(cwd)) => Some(cwd.as_str()),
            Some(_) => {
                return Dispatch::Message(protocol::build_error_message(
                    id,
                    -32602,
                    "cwd must be a string",
                ));
            }
        };
        if cwd.is_some_and(|path| !Path::new(path).is_absolute()) {
            return Dispatch::Message(protocol::build_error_message(
                id,
                -32602,
                "cwd must be absolute",
            ));
        }
        if params
            .and_then(|value| value.get("cursor"))
            .is_some_and(|cursor| !matches!(cursor, JsonValue::Null))
        {
            return Dispatch::Message(protocol::build_error_message(
                id,
                -32602,
                "session/list cursor is not valid for an unpaginated result",
            ));
        }

        let state = self.state.lock();
        let mut sessions = state
            .sessions
            .iter()
            .filter(|(_, session)| cwd.is_none_or(|cwd| session.cwd == cwd))
            .map(|(session_id, session)| session_info(session_id, session))
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| {
            left.get("sessionId")
                .and_then(JsonValue::as_str)
                .cmp(&right.get("sessionId").and_then(JsonValue::as_str))
        });

        let mut result = Map::new();
        result.insert("sessions".into(), JsonValue::Array(sessions));
        Dispatch::Message(protocol::build_result_message(
            id,
            JsonValue::Object(result),
        ))
    }

    fn resume_session(&self, id: Option<&JsonValue>, params: Option<&JsonValue>) -> Dispatch {
        if id.is_none() {
            return Dispatch::Message(protocol::build_error_message(
                None,
                -32600,
                "session/resume requires an id",
            ));
        }
        let Some(params) = params else {
            return Dispatch::Message(protocol::build_error_message(
                id,
                -32602,
                "params are required",
            ));
        };
        let Some(session_id) = params.get("sessionId").and_then(JsonValue::as_str) else {
            return Dispatch::Message(protocol::build_error_message(
                id,
                -32602,
                "sessionId is required",
            ));
        };
        let Some(cwd) = params.get("cwd").and_then(JsonValue::as_str) else {
            return Dispatch::Message(protocol::build_error_message(id, -32602, "cwd is required"));
        };
        let Some(session) = self.state.lock().sessions.get(session_id).cloned() else {
            return Dispatch::Message(protocol::build_error_message(
                id,
                -32002,
                "session not found",
            ));
        };
        if !Path::new(cwd).is_absolute() || cwd != session.cwd {
            return Dispatch::Message(protocol::build_error_message(
                id,
                -32602,
                "cwd does not match the session",
            ));
        }
        if has_unadvertised_session_options(params)
            || params.get("mcpServers").is_some_and(|value| match value {
                JsonValue::Null => false,
                JsonValue::Array(servers) => !servers.is_empty(),
                _ => true,
            })
            || params
                .get("replayFrom")
                .is_some_and(|cursor| !matches!(cursor, JsonValue::Null))
        {
            return Dispatch::Message(protocol::build_error_message(
                id,
                -32602,
                "unsupported session/resume option",
            ));
        }

        let state_update = if session.active.lock().is_some() {
            v2_state_update(session_id, "running", None, None)
        } else {
            v2_state_update(session_id, "idle", None, None)
        };
        let response = protocol::build_result_message(id, JsonValue::Object(Map::new()));
        Dispatch::Messages(vec![state_update, response])
    }

    fn prompt(
        &self,
        protocol_version: ProtocolVersion,
        id: Option<&JsonValue>,
        params: Option<&JsonValue>,
    ) -> Dispatch {
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

        let prompt = params.get("prompt").cloned().unwrap_or(JsonValue::Null);
        let user_message_id = (protocol_version == ProtocolVersion::V2).then(|| {
            let sequence = session.next_message.fetch_add(1, Ordering::Relaxed);
            format!("message-{sequence}")
        });
        let accepted = user_message_id.as_ref().map(|message_id| {
            let mut result = Map::new();
            result.insert("messageId".into(), JsonValue::String(message_id.clone()));
            protocol::build_result_message(Some(&id), JsonValue::Object(result))
        });

        Dispatch::Prompt {
            job: PromptJob {
                id,
                session_id: session_id.to_string(),
                question,
                prompt,
                protocol_version,
                user_message_id,
                session,
            },
            accepted,
        }
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

    fn close_session(
        &self,
        protocol_version: ProtocolVersion,
        id: Option<&JsonValue>,
        params: Option<&JsonValue>,
    ) -> String {
        if id.is_none() {
            return protocol::build_error_message(None, -32600, "session/close requires an id");
        }
        let Some(session_id) = params
            .and_then(|value| value.get("sessionId"))
            .and_then(JsonValue::as_str)
        else {
            return protocol::build_error_message(id, -32602, "sessionId is required");
        };
        let Some(session) = self.state.lock().sessions.remove(session_id) else {
            return if protocol_version == ProtocolVersion::V2 {
                protocol::build_result_message(id, JsonValue::Object(Map::new()))
            } else {
                protocol::build_error_message(id, -32002, "session not found")
            };
        };
        let mut active = session.active.lock();
        if let Some(turn) = active.as_ref() {
            turn.cancelled.store(true, Ordering::Release);
            turn.cancellation.cancel();
        }
        while active.is_some() {
            session.active_finished.wait(&mut active);
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

fn initialize_v1(id: Option<&JsonValue>) -> String {
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

    let mut result = Map::new();
    result.insert("agentCapabilities".into(), JsonValue::Object(capabilities));
    result.insert("agentInfo".into(), agent_info());
    result.insert("authMethods".into(), JsonValue::Array(Vec::new()));
    result.insert(
        "protocolVersion".into(),
        JsonValue::Integer(ACP_PROTOCOL_V1),
    );
    protocol::build_result_message(id, JsonValue::Object(result))
}

fn initialize_v2(id: Option<&JsonValue>) -> String {
    let mut capabilities = Map::new();
    capabilities.insert("session".into(), JsonValue::Object(Map::new()));

    let mut result = Map::new();
    result.insert("authMethods".into(), JsonValue::Array(Vec::new()));
    result.insert("capabilities".into(), JsonValue::Object(capabilities));
    result.insert("info".into(), agent_info());
    result.insert(
        "protocolVersion".into(),
        JsonValue::Integer(ACP_PROTOCOL_V2),
    );
    protocol::build_result_message(id, JsonValue::Object(result))
}

fn agent_info() -> JsonValue {
    let mut info = Map::new();
    info.insert("name".into(), JsonValue::String("reddb".into()));
    info.insert("title".into(), JsonValue::String("RedDB ASK".into()));
    info.insert(
        "version".into(),
        JsonValue::String(env!("CARGO_PKG_VERSION").into()),
    );
    JsonValue::Object(info)
}

fn has_unadvertised_session_options(params: &JsonValue) -> bool {
    params
        .get("additionalDirectories")
        .is_some_and(|value| match value {
            JsonValue::Null => false,
            JsonValue::Array(directories) => !directories.is_empty(),
            _ => true,
        })
        || params
            .get("systemPrompt")
            .is_some_and(|prompt| !matches!(prompt, JsonValue::Null))
}

fn session_info(session_id: &str, session: &AcpSession) -> JsonValue {
    let mut info = Map::new();
    info.insert("cwd".into(), JsonValue::String(session.cwd.to_string()));
    info.insert(
        "sessionId".into(),
        JsonValue::String(session_id.to_string()),
    );
    JsonValue::Object(info)
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
        if !parts.is_empty() {
            bytes = bytes.saturating_add(2);
        }
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
    let _active_guard = ActivePromptGuard {
        session: &job.session,
    };
    if job.protocol_version == ProtocolVersion::V2 {
        let _ = output.send(v2_state_update(&job.session_id, "running", None, None));
        if let Some(message_id) = job.user_message_id.as_deref() {
            let _ = output.send(v2_message_update(
                &job.session_id,
                "user_message",
                message_id,
                job.prompt.clone(),
            ));
        }
    }
    let result = execution_context::with_snapshot_bundle(&job.session.identity, || {
        let _cancellation = active.cancellation.install();
        let scope = runtime.ai_scope();
        let _scope = AskScopeGuard::install(&runtime, &scope);
        runtime.execute_ask("ASK <acp>", &ask_query(job.question))
    });

    let cancelled = active.cancelled.load(Ordering::Acquire);
    if cancelled {
        let message = match job.protocol_version {
            ProtocolVersion::V1 => prompt_response(&job.id, "cancelled"),
            ProtocolVersion::V2 => {
                v2_state_update(&job.session_id, "idle", Some("cancelled"), None)
            }
        };
        let _ = output.send(message);
    } else {
        match result {
            Ok(result) => {
                let envelope = crate::presentation::query_result::tagged_summary(&result);
                let messages = match job.protocol_version {
                    ProtocolVersion::V1 => {
                        turn_messages_v1(&job.session_id, &envelope, &job.session.next_tool_call)
                    }
                    ProtocolVersion::V2 => {
                        turn_messages_v2(&job.session_id, &envelope, &job.session)
                    }
                };
                for message in messages {
                    let _ = output.send(message);
                }
                let completion = match job.protocol_version {
                    ProtocolVersion::V1 => prompt_response(&job.id, "end_turn"),
                    ProtocolVersion::V2 => {
                        v2_state_update(&job.session_id, "idle", Some("end_turn"), None)
                    }
                };
                let _ = output.send(completion);
            }
            Err(error) => {
                let message = match job.protocol_version {
                    ProtocolVersion::V1 => {
                        protocol::build_error_message(Some(&job.id), -32603, &error.to_string())
                    }
                    ProtocolVersion::V2 => v2_state_update(
                        &job.session_id,
                        "idle",
                        Some("error"),
                        Some(&error.to_string()),
                    ),
                };
                let _ = output.send(message);
            }
        }
    }
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

fn turn_messages_v1(
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
        messages.push(v1_text_update(session_id, "agent_thought_chunk", summary));
    }

    if let Some(steps) = envelope
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("steps"))
        .and_then(JsonValue::as_array)
    {
        for step in steps {
            let tool_call_id = next_tool_call.fetch_add(1, Ordering::Relaxed);
            messages.push(v1_tool_update(session_id, tool_call_id, step));
        }
    }

    let answer = envelope
        .get("answer")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    messages.push(v1_text_update(session_id, "agent_message_chunk", answer));
    messages
}

fn v1_text_update(session_id: &str, kind: &str, text: &str) -> String {
    let mut content = Map::new();
    content.insert("text".into(), JsonValue::String(text.to_string()));
    content.insert("type".into(), JsonValue::String("text".into()));

    let mut update = Map::new();
    update.insert("content".into(), JsonValue::Object(content));
    update.insert("sessionUpdate".into(), JsonValue::String(kind.to_string()));

    session_update(session_id, JsonValue::Object(update))
}

fn v1_tool_update(session_id: &str, sequence: u64, step: &JsonValue) -> String {
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

fn turn_messages_v2(session_id: &str, envelope: &JsonValue, session: &AcpSession) -> Vec<String> {
    let mut messages = Vec::new();
    if let Some(summary) = envelope
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("summary"))
        .and_then(JsonValue::as_str)
        .filter(|summary| !summary.is_empty())
    {
        let message_id = next_message_id(session);
        messages.push(v2_message_update(
            session_id,
            "agent_thought",
            &message_id,
            JsonValue::Array(vec![text_content(summary)]),
        ));
    }

    if let Some(steps) = envelope
        .get("reasoning")
        .and_then(|reasoning| reasoning.get("steps"))
        .and_then(JsonValue::as_array)
    {
        for step in steps {
            let sequence = session.next_tool_call.fetch_add(1, Ordering::Relaxed);
            messages.push(v2_tool_update(session_id, sequence, step));
        }
    }

    let answer = envelope
        .get("answer")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let message_id = next_message_id(session);
    messages.push(v2_message_update(
        session_id,
        "agent_message",
        &message_id,
        JsonValue::Array(vec![text_content(answer)]),
    ));
    messages
}

fn next_message_id(session: &AcpSession) -> String {
    let sequence = session.next_message.fetch_add(1, Ordering::Relaxed);
    format!("message-{sequence}")
}

fn text_content(text: &str) -> JsonValue {
    let mut content = Map::new();
    content.insert("text".into(), JsonValue::String(text.to_string()));
    content.insert("type".into(), JsonValue::String("text".into()));
    JsonValue::Object(content)
}

fn v2_message_update(session_id: &str, kind: &str, message_id: &str, content: JsonValue) -> String {
    let mut update = Map::new();
    update.insert("content".into(), content);
    update.insert(
        "messageId".into(),
        JsonValue::String(message_id.to_string()),
    );
    update.insert("sessionUpdate".into(), JsonValue::String(kind.to_string()));
    session_update(session_id, JsonValue::Object(update))
}

fn v2_tool_update(session_id: &str, sequence: u64, step: &JsonValue) -> String {
    let name = step
        .get("tool")
        .and_then(JsonValue::as_str)
        .unwrap_or("reddb_query");
    let title = step
        .get("purpose")
        .and_then(JsonValue::as_str)
        .unwrap_or(name);

    let mut update = Map::new();
    update.insert("kind".into(), JsonValue::String("search".into()));
    update.insert("name".into(), JsonValue::String(name.to_string()));
    update.insert("rawInput".into(), step.clone());
    if let Some(output) = step.get("observation") {
        update.insert("rawOutput".into(), output.clone());
    }
    update.insert(
        "sessionUpdate".into(),
        JsonValue::String("tool_call_update".into()),
    );
    update.insert("status".into(), JsonValue::String("completed".into()));
    update.insert("title".into(), JsonValue::String(title.to_string()));
    update.insert(
        "toolCallId".into(),
        JsonValue::String(format!("tool-{sequence}")),
    );
    session_update(session_id, JsonValue::Object(update))
}

fn v2_state_update(
    session_id: &str,
    state: &str,
    stop_reason: Option<&str>,
    error_message: Option<&str>,
) -> String {
    let mut update = Map::new();
    update.insert(
        "sessionUpdate".into(),
        JsonValue::String("state_update".into()),
    );
    update.insert("state".into(), JsonValue::String(state.to_string()));
    if let Some(stop_reason) = stop_reason {
        update.insert(
            "stopReason".into(),
            JsonValue::String(stop_reason.to_string()),
        );
    }
    if let Some(message) = error_message {
        let mut error = Map::new();
        error.insert("code".into(), JsonValue::Integer(-32603));
        error.insert("message".into(), JsonValue::String(message.to_string()));
        update.insert("error".into(), JsonValue::Object(error));
    }
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

    fn initialized_v2_server() -> AcpServer {
        let server = AcpServer::new(RedDBRuntime::in_memory().unwrap());
        let response = server.dispatch(&request(
            1,
            "initialize",
            object(vec![
                (
                    "info",
                    object(vec![
                        ("name", JsonValue::String("test-client".into())),
                        ("version", JsonValue::String("1.0.0".into())),
                    ]),
                ),
                ("protocolVersion", JsonValue::Integer(2)),
            ]),
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

    fn new_v2_session(server: &AcpServer) -> (String, Arc<AcpSession>) {
        let params = object(vec![(
            "cwd",
            JsonValue::String(std::env::current_dir().unwrap().display().to_string()),
        )]);
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
    fn initialize_negotiates_v2_and_rejects_unsupported_versions() {
        let server = AcpServer::new(RedDBRuntime::in_memory().unwrap());
        let Dispatch::Message(missing_info) = server.dispatch(&request(
            1,
            "initialize",
            object(vec![("protocolVersion", JsonValue::Integer(2))]),
        )) else {
            panic!("initialize response");
        };
        assert!(missing_info.contains("requires info.name and info.version"));

        let Dispatch::Message(response) = server.dispatch(&request(
            2,
            "initialize",
            object(vec![
                (
                    "info",
                    object(vec![
                        ("name", JsonValue::String("test-client".into())),
                        ("version", JsonValue::String("1.0.0".into())),
                    ]),
                ),
                ("protocolVersion", JsonValue::Integer(9)),
            ]),
        )) else {
            panic!("initialize response");
        };
        let response: JsonValue = json_from_str(&response).unwrap();
        let result = response.get("result").unwrap();
        assert_eq!(
            result.get("protocolVersion").and_then(JsonValue::as_i64),
            Some(2)
        );
        assert!(result
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("session"))
            .and_then(JsonValue::as_object)
            .unwrap()
            .is_empty());
        assert!(result.get("agentCapabilities").is_none());

        let unsupported = AcpServer::new(RedDBRuntime::in_memory().unwrap());
        let Dispatch::Message(response) = unsupported.dispatch(&request(
            1,
            "initialize",
            object(vec![("protocolVersion", JsonValue::Integer(0))]),
        )) else {
            panic!("initialize response");
        };
        assert!(response.contains("-32022"));

        let null_id = AcpServer::new(RedDBRuntime::in_memory().unwrap());
        let message = object(vec![
            ("id", JsonValue::Null),
            ("jsonrpc", JsonValue::String("2.0".into())),
            ("method", JsonValue::String("initialize".into())),
            (
                "params",
                object(vec![
                    (
                        "info",
                        object(vec![
                            ("name", JsonValue::String("test-client".into())),
                            ("version", JsonValue::String("1.0.0".into())),
                        ]),
                    ),
                    ("protocolVersion", JsonValue::Integer(2)),
                ]),
            ),
        ]);
        let Dispatch::Message(response) = null_id.dispatch(&message) else {
            panic!("initialize response");
        };
        assert!(response.contains("must not be null"));
    }

    #[test]
    fn v2_baseline_lists_and_resumes_connection_sessions() {
        let server = initialized_v2_server();
        let (session_id, session) = new_v2_session(&server);

        let Dispatch::Message(response) =
            server.dispatch(&request(3, "session/list", object(Vec::new())))
        else {
            panic!("list response");
        };
        let response: JsonValue = json_from_str(&response).unwrap();
        let sessions = response
            .get("result")
            .and_then(|result| result.get("sessions"))
            .and_then(JsonValue::as_array)
            .unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0].get("sessionId").and_then(JsonValue::as_str),
            Some(session_id.as_str())
        );

        let params = object(vec![
            ("cwd", JsonValue::String(session.cwd.clone())),
            ("sessionId", JsonValue::String(session_id)),
        ]);
        let Dispatch::Messages(messages) = server.dispatch(&request(4, "session/resume", params))
        else {
            panic!("resume response");
        };
        assert_eq!(messages.len(), 2);
        assert!(messages[0].contains("state_update"));
        assert!(messages[0].contains("idle"));
        assert!(messages[1].contains("\"result\":{}"));
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
            Dispatch::Prompt { .. }
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
        let messages = turn_messages_v1("s-1", &envelope, &next_tool_call);
        assert_eq!(messages.len(), 3);
        assert!(messages[0].contains("agent_thought_chunk"));
        assert!(messages[1].contains("reddb_final_query"));
        assert!(messages[1].contains("SELECT name FROM people"));
        assert!(messages[2].contains("Ana is active."));

        let next_messages = turn_messages_v1("s-1", &envelope, &next_tool_call);
        assert!(messages[1].contains("reddb-1"));
        assert!(next_messages[1].contains("reddb-2"));
    }

    #[test]
    fn v2_prompt_acknowledges_insertion_and_uses_upsert_updates() {
        let server = initialized_v2_server();
        let (session_id, session) = new_v2_session(&server);
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
        let Dispatch::Prompt { job, accepted } =
            server.dispatch(&request(3, "session/prompt", prompt))
        else {
            panic!("prompt job");
        };
        let accepted: JsonValue = json_from_str(&accepted.unwrap()).unwrap();
        assert_eq!(
            accepted
                .get("result")
                .and_then(|result| result.get("messageId"))
                .and_then(JsonValue::as_str),
            job.user_message_id.as_deref()
        );
        assert_eq!(job.protocol_version, ProtocolVersion::V2);

        let step = object(vec![
            ("purpose", JsonValue::String("Validate the answer".into())),
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
        let messages = turn_messages_v2(&session_id, &envelope, &session);
        assert_eq!(messages.len(), 3);
        assert!(messages[0].contains("agent_thought"));
        assert!(messages[0].contains("messageId"));
        assert!(messages[1].contains("tool_call_update"));
        assert!(messages[1].contains("toolCallId"));
        assert!(messages[2].contains("agent_message"));
        assert!(v2_state_update(&session_id, "idle", Some("end_turn"), None).contains("stopReason"));
    }
}
