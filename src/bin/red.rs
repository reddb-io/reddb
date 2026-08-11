#![allow(clippy::unwrap_used)]
// Legacy allow for the too_many_lines ratchet (PRD #1252): the CLI command
// dispatchers in this binary exceed the 120-line threshold. The lint bites on
// new/changed code; remove once those functions are split up.
#![allow(clippy::too_many_lines)]
// Legacy allow for the cast_possible_truncation ratchet (PRD #1252): the CLI
// has pre-existing truncating `as` casts (e.g. f64→u64 narrowing). The lint
// bites on new/changed code; remove once those casts become checked
// conversions.
#![allow(clippy::cast_possible_truncation)]
/// `red` -- RedDB unified CLI binary.
///
/// Parses argv using the schema-driven CLI parser, routes to the
/// appropriate command, and dispatches execution.
use std::cell::OnceCell;
use std::collections::HashMap;
use std::io::Write;
use std::net::ToSocketAddrs;
use std::path::PathBuf;
use std::time::Duration;

use reddb::cli;
use reddb::cli::types::FlagValue;
use reddb::service_cli::{
    install_systemd_service, probe_listener, render_systemd_unit, run_server, BootstrapConfig,
    ServerCommandConfig, ServerTransport, SystemdServiceConfig,
};
use reddb_client::http::{HttpClient, HttpOptions};
use reddb_client::{format_query_result, JsonValue, QueryResult, RowFormat};
use reddb_types::encoding::json_escape;

// ---------------------------------------------------------------------------
// JSON output helpers
// ---------------------------------------------------------------------------

/// Returns `true` when the caller requested structured JSON output.
fn wants_json(flags: &HashMap<String, FlagValue>) -> bool {
    flag_bool(flags, "json") || flag_string(flags, "output").as_deref() == Some("json")
}

/// Print a successful JSON envelope to **stdout** and return.
fn json_ok(command: &str, data: &str) {
    println!(
        "{{\"ok\":true,\"command\":\"{}\",\"data\":{}}}",
        json_escape(command),
        data
    );
}

/// Open a `RedDBRuntime` for the local DML/DDL commands.
///
/// When `--path <file>` is supplied the runtime opens the on-disk
/// database in embedded mode. Without `--path`, falls back to an
/// in-memory runtime so one-shot commands like `red query "SELECT 1"`
/// still work for smoke tests.
fn open_local_runtime(flags: &HashMap<String, FlagValue>) -> Result<reddb::RedDBRuntime, String> {
    match flag_string(flags, "path") {
        Some(path) if !path.is_empty() => {
            let storage_profile = resolve_storage_profile(flags, "local")?;
            let options = reddb::api::RedDBOptions::persistent(&path)
                .with_storage_profile(storage_profile)
                .map_err(|e| format!("storage profile: {e}"))?;
            reddb::RedDBRuntime::with_options(options).map_err(|e| format!("open {path}: {e}"))
        }
        _ => reddb::RedDBRuntime::in_memory().map_err(|e| e.to_string()),
    }
}

/// Flush any pending writes to disk. One-shot CLI commands exit
/// immediately after a single operation, so we have to call this
/// explicitly — the runtime does not flush on drop.
fn checkpoint_local_runtime(rt: &reddb::RedDBRuntime) {
    let _ = rt.checkpoint();
}

/// Returns `true` when a `query` positional names a supported data file,
/// triggering the ephemeral-store tracer (PRD #1785, issues #1786/#1792):
/// `red query <file.csv|file.json> [more.csv ...] <sql>`.
fn is_ephemeral_data_file(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    lower.ends_with(".csv")
        || lower.ends_with(".tsv")
        || lower.ends_with(".tab")
        || lower.ends_with(".json")
        || lower.ends_with(".jsonl")
        || lower.ends_with(".ndjson")
}

/// Run the ephemeral-store tracer: materialize local data files into
/// a throwaway in-memory embedded store, execute the query against them,
/// print the result, and discard the store. No server, no pre-existing
/// store, nothing durable created — each collection is addressable by its
/// sanitized file-stem name and by its positional alias.
fn run_ephemeral_query(
    args: &[String],
    files: &[String],
    sql: &str,
    json_mode: bool,
    row_format: RowFormat,
    save_path: Option<String>,
    dry_run: bool,
) -> ! {
    use std::path::Path;

    if sql.is_empty() {
        if json_mode {
            json_error(
                "query",
                "Usage: red query <file.csv|file.tsv|file.json|file.ndjson> [more files ...] <sql>",
            );
        }
        eprintln!(
            "Usage: red query <file.csv|file.tsv|file.json|file.ndjson> [more files ...] <sql>"
        );
        eprintln!("Example: red query users.csv orders.csv \"SELECT * FROM t1 JOIN t2 ON ...\"");
        std::process::exit(1);
    }

    let params = collect_query_params(args).unwrap_or_else(|err| {
        if json_mode {
            json_error("query", &err);
        }
        eprintln!("query: {err}");
        std::process::exit(1);
    });

    let rt = match save_path.as_deref() {
        Some(path) if path.trim().is_empty() => {
            if json_mode {
                json_error("query", "--save requires a non-empty path");
            }
            eprintln!("query: --save requires a non-empty path");
            std::process::exit(1);
        }
        Some(_) if dry_run => {
            // Preview the statement only; do not create the requested save target.
            reddb::RedDBRuntime::in_memory().unwrap_or_else(|err| {
                if json_mode {
                    json_error("query", &err.to_string());
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            })
        }
        Some(path) => {
            if Path::new(path).exists() {
                if json_mode {
                    json_error("query", &format!("save target '{path}' already exists"));
                }
                eprintln!("query: save target '{path}' already exists");
                std::process::exit(1);
            }
            reddb::RedDBRuntime::with_options(reddb::api::RedDBOptions::persistent(path))
                .unwrap_or_else(|err| {
                    if json_mode {
                        json_error("query", &err.to_string());
                    }
                    eprintln!("error: {err}");
                    std::process::exit(1);
                })
        }
        None => {
            // Throwaway in-memory store — nothing durable is written.
            reddb::RedDBRuntime::in_memory().unwrap_or_else(|err| {
                if json_mode {
                    json_error("query", &err.to_string());
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            })
        }
    };

    let paths: Vec<&Path> = files.iter().map(|file| Path::new(file.as_str())).collect();
    if let Err(err) = rt.materialize_data_files(&paths) {
        // Missing, unreadable, or malformed files land here as a
        // didactic message rather than a panic.
        if json_mode {
            json_error("query", &err.to_string());
        }
        eprintln!("query error: {err}");
        std::process::exit(1);
    }

    let preview_sql = dry_run.then(|| format!("EXPLAIN {sql}"));
    let sql_to_run = preview_sql.as_deref().unwrap_or(sql);

    let exec_result = if params.is_empty() {
        rt.execute_query(sql_to_run)
    } else {
        let params: Vec<_> = params
            .into_iter()
            .map(reddb_client::Value::into_schema_value)
            .collect();
        // Bind inside the runtime's statement frame (#2183) so `--param`
        // queries keep the same snapshot isolation as textual SQL.
        rt.execute_query_with_params(sql_to_run, &params)
    };

    match exec_result {
        Ok(qr) => {
            if let Some(path) = save_path.as_deref().filter(|_| !dry_run) {
                if let Err(err) = rt.checkpoint() {
                    if json_mode {
                        json_error("query", &err.to_string());
                    }
                    eprintln!("query: failed to save '{path}': {err}");
                    std::process::exit(1);
                }
            }
            let result = reddb_client::embedded::query_result_from_runtime(&qr);
            emit_data_result(
                "query",
                &result,
                row_format,
                json_mode,
                preview_sql.map(|preview| (sql, preview)),
            );
            std::process::exit(0);
        }
        Err(err) => {
            if json_mode {
                json_error("query", &err.to_string());
            }
            eprintln!("query error: {err}");
            std::process::exit(1);
        }
    }
}

fn parse_row_format(flags: &HashMap<String, FlagValue>) -> Result<RowFormat, String> {
    match flag_string(flags, "format") {
        Some(value) => RowFormat::parse(value.as_str()).ok_or_else(|| {
            format!(
                "unknown row format '{value}'; expected {}",
                RowFormat::vocabulary()
            )
        }),
        None => Ok(RowFormat::Table),
    }
}

#[derive(Clone)]
struct McpClientOptions {
    redacted_uri: String,
    target: reddb_wire::ConnectionTarget,
    auth: reddb_wire::ConnectionAuth,
    timeout: Duration,
}

impl std::fmt::Debug for McpClientOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClientOptions")
            .field("uri", &self.redacted_uri)
            .field("target", &self.target)
            .field("auth", &self.auth)
            .field("timeout", &self.timeout)
            .finish()
    }
}

fn resolve_mcp_client_options(
    flags: &HashMap<String, FlagValue>,
) -> Result<Option<McpClientOptions>, String> {
    let raw_uri = flag_string(flags, "uri")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| flag_string(flags, "url").filter(|value| !value.trim().is_empty()))
        .filter(|value| !value.trim().is_empty())
        .or_else(|| env_string("REDDB_MCP_URI"));
    let Some(raw_uri) = raw_uri else {
        return Ok(None);
    };

    let mut spec = reddb_wire::parse_with_auth(&raw_uri)
        .map_err(|err| format!("mcp URI parse error: {err}"))?;
    if matches!(spec.auth, reddb_wire::ConnectionAuth::Anonymous) {
        if let Some(token) = flag_string(flags, "token").filter(|value| !value.trim().is_empty()) {
            spec.auth = reddb_wire::ConnectionAuth::bearer(token);
        }
    }

    if spec.auth.is_bearer() && !mcp_uri_allows_bearer(&raw_uri) {
        return Err(format!(
            "bearer token requires TLS transport (reds://, https://, or red+wss://): {}",
            spec.redacted_uri
        ));
    }
    if !matches!(
        spec.target,
        reddb_wire::ConnectionTarget::Memory | reddb_wire::ConnectionTarget::File { .. }
    ) && matches!(spec.auth, reddb_wire::ConnectionAuth::Anonymous)
    {
        return Err(format!(
            "remote MCP requires credentials: {}",
            spec.redacted_uri
        ));
    }

    let timeout = resolve_mcp_timeout(&raw_uri)?;
    Ok(Some(McpClientOptions {
        redacted_uri: spec.redacted_uri,
        target: spec.target,
        auth: spec.auth,
        timeout,
    }))
}

fn resolve_mcp_timeout(uri: &str) -> Result<Duration, String> {
    if let Some(value) = query_param_value(uri, "timeout") {
        return parse_mcp_timeout_s(&value, "timeout");
    }
    if let Some(value) = env_string("REDDB_MCP_TIMEOUT_S") {
        return parse_mcp_timeout_s(&value, "REDDB_MCP_TIMEOUT_S");
    }
    Ok(Duration::from_secs(20))
}

fn parse_mcp_timeout_s(value: &str, source: &str) -> Result<Duration, String> {
    let secs = value
        .parse::<u64>()
        .map_err(|_| format!("{source} must be a positive integer number of seconds"))?;
    if secs == 0 {
        return Err(format!(
            "{source} must be a positive integer number of seconds"
        ));
    }
    Ok(Duration::from_secs(secs))
}

fn query_param_value(uri: &str, key: &str) -> Option<String> {
    let query_start = uri.find('?')? + 1;
    let query_end = uri[query_start..]
        .find('#')
        .map(|offset| query_start + offset)
        .unwrap_or(uri.len());
    uri[query_start..query_end].split('&').find_map(|part| {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        if name == key {
            Some(value.to_string())
        } else {
            None
        }
    })
}

fn mcp_uri_allows_bearer(uri: &str) -> bool {
    let Some(colon) = uri.find(':') else {
        return false;
    };
    matches!(
        uri[..colon].to_ascii_lowercase().as_str(),
        "reds" | "https" | "red+wss"
    )
}

fn run_mcp_remote(mut options: McpClientOptions) -> i32 {
    if let Err(err) = validate_mcp_remote_connect(&mut options) {
        eprintln!("red mcp: {err}");
        return 1;
    }
    let mut server = RemoteMcpServer::new(options);
    server.run_stdio();
    0
}

fn validate_mcp_remote_connect(options: &mut McpClientOptions) -> Result<(), String> {
    match &options.target {
        reddb_wire::ConnectionTarget::Grpc { endpoint } => {
            let mut client = connect_mcp_grpc(endpoint, &options.auth, options.timeout)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| format!("build runtime: {err}"))?;
            rt.block_on(async {
                tokio::time::timeout(options.timeout, client.health_status())
                    .await
                    .map_err(|_| "connection timeout".to_string())?
                    .map_err(|err| format!("connect {}: {err}", options.redacted_uri))
            })?;
            Ok(())
        }
        reddb_wire::ConnectionTarget::RedWire { host, port, .. } => {
            let host = host.clone();
            let port = *port;
            let auth = redwire_auth_from_connection_auth(&options.auth)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|err| format!("build runtime: {err}"))?;
            rt.block_on(async {
                let opts = reddb_client::redwire::ConnectOptions::new(host, port).with_auth(auth);
                tokio::time::timeout(
                    options.timeout,
                    reddb_client::redwire::RedWireClient::connect(opts),
                )
                .await
                .map_err(|_| "connection timeout".to_string())?
                .map(|_| ())
                .map_err(|err| format!("connect {}: {err}", options.redacted_uri))
            })
        }
        reddb_wire::ConnectionTarget::Http { .. } => Ok(()),
        reddb_wire::ConnectionTarget::WsNative { .. } => {
            Err("red mcp: red+ws/red+wss client connector is not implemented yet".to_string())
        }
        reddb_wire::ConnectionTarget::GrpcCluster { .. } => {
            Err("red mcp: clustered MCP client URLs are not supported yet".to_string())
        }
        reddb_wire::ConnectionTarget::Memory | reddb_wire::ConnectionTarget::File { .. } => Ok(()),
    }
}

fn connect_mcp_grpc(
    endpoint: &str,
    auth: &reddb_wire::ConnectionAuth,
    timeout: Duration,
) -> Result<reddb_client::RedDBClient, String> {
    let token = match auth {
        reddb_wire::ConnectionAuth::Anonymous => None,
        reddb_wire::ConnectionAuth::Bearer(token) => Some(token.clone()),
        reddb_wire::ConnectionAuth::Basic { .. } | reddb_wire::ConnectionAuth::ApiKey(_) => {
            return Err("red mcp: gRPC basic/apikey auth codec is not implemented yet".to_string());
        }
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("build runtime: {err}"))?;
    rt.block_on(async {
        tokio::time::timeout(timeout, reddb_client::RedDBClient::connect(endpoint, token))
            .await
            .map_err(|_| "connection timeout".to_string())?
            .map_err(|err| format!("connect: {err}"))
    })
}

fn redwire_auth_from_connection_auth(
    auth: &reddb_wire::ConnectionAuth,
) -> Result<reddb_client::redwire::Auth, String> {
    match auth {
        reddb_wire::ConnectionAuth::Anonymous => Ok(reddb_client::redwire::Auth::Anonymous),
        reddb_wire::ConnectionAuth::Bearer(token) => {
            Ok(reddb_client::redwire::Auth::Bearer(token.clone()))
        }
        reddb_wire::ConnectionAuth::Basic { user, pass } => {
            Ok(reddb_client::redwire::Auth::Basic {
                user: user.clone(),
                pass: pass.clone(),
            })
        }
        reddb_wire::ConnectionAuth::ApiKey(key) => {
            Ok(reddb_client::redwire::Auth::ApiKey(key.clone()))
        }
    }
}

fn http_auth_from_connection_auth(
    auth: &reddb_wire::ConnectionAuth,
) -> reddb_client::connector::http::Auth {
    match auth {
        reddb_wire::ConnectionAuth::Anonymous => reddb_client::connector::http::Auth::Anonymous,
        reddb_wire::ConnectionAuth::Bearer(token) => {
            reddb_client::connector::http::Auth::Bearer(token.clone())
        }
        reddb_wire::ConnectionAuth::Basic { user, pass } => {
            reddb_client::connector::http::Auth::Basic {
                user: user.clone(),
                pass: pass.clone(),
            }
        }
        reddb_wire::ConnectionAuth::ApiKey(key) => {
            reddb_client::connector::http::Auth::ApiKey(key.clone())
        }
    }
}

struct RemoteMcpServer {
    options: McpClientOptions,
    initialized: bool,
}

impl RemoteMcpServer {
    fn new(options: McpClientOptions) -> Self {
        Self {
            options,
            initialized: false,
        }
    }

    fn run_stdio(&mut self) {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        let mut reader = std::io::BufReader::new(stdin.lock());
        let mut writer = std::io::BufWriter::new(stdout.lock());

        loop {
            let payload = match reddb::mcp::protocol::read_payload(&mut reader) {
                Ok(Some(payload)) => payload,
                Ok(None) => break,
                Err(err) => {
                    eprintln!("red mcp: read error: {err}");
                    continue;
                }
            };
            let request: reddb::json::Value = match reddb::json::from_str(&payload) {
                Ok(value) => value,
                Err(_) => {
                    let msg =
                        reddb::mcp::protocol::build_error_message(None, -32700, "parse error");
                    let _ = reddb::mcp::protocol::write_message(&mut writer, &msg);
                    continue;
                }
            };
            if let Some(response) = self.handle_message(&request) {
                if reddb::mcp::protocol::write_message(&mut writer, &response).is_err() {
                    break;
                }
            }
        }
    }

    fn handle_message(&mut self, msg: &reddb::json::Value) -> Option<String> {
        let method = msg.get("method").and_then(|value| value.as_str())?;
        let id = msg.get("id");
        match method {
            "initialize" => Some(self.handle_initialize(id)),
            "initialized" | "notifications/initialized" => None,
            "tools/list" => Some(self.handle_tools_list(id)),
            "tools/call" => Some(self.handle_tools_call(id, msg.get("params"))),
            "resources/list" => Some(self.handle_resources_list(id)),
            "resources/read" => Some(self.handle_resources_read(id, msg.get("params"))),
            "ping" => {
                let mut result = reddb::json::Map::new();
                result.insert(
                    "status".to_string(),
                    reddb::json::Value::String("ok".to_string()),
                );
                Some(reddb::mcp::protocol::build_result_message(
                    id,
                    reddb::json::Value::Object(result),
                ))
            }
            _ => Some(reddb::mcp::protocol::build_error_message(
                id,
                -32601,
                &format!("unknown method: {method}"),
            )),
        }
    }

    fn handle_initialize(&mut self, id: Option<&reddb::json::Value>) -> String {
        self.initialized = true;
        let mut capabilities = reddb::json::Map::new();
        let mut tools_cap = reddb::json::Map::new();
        tools_cap.insert("listChanged".to_string(), reddb::json::Value::Bool(false));
        capabilities.insert("tools".to_string(), reddb::json::Value::Object(tools_cap));
        let mut resources_cap = reddb::json::Map::new();
        resources_cap.insert("subscribe".to_string(), reddb::json::Value::Bool(false));
        resources_cap.insert("listChanged".to_string(), reddb::json::Value::Bool(false));
        capabilities.insert(
            "resources".to_string(),
            reddb::json::Value::Object(resources_cap),
        );

        let mut server_info = reddb::json::Map::new();
        server_info.insert(
            "name".to_string(),
            reddb::json::Value::String("reddb-mcp".to_string()),
        );
        server_info.insert(
            "version".to_string(),
            reddb::json::Value::String(env!("CARGO_PKG_VERSION").to_string()),
        );

        let mut result = reddb::json::Map::new();
        result.insert(
            "protocolVersion".to_string(),
            reddb::json::Value::String("2024-11-05".to_string()),
        );
        result.insert(
            "capabilities".to_string(),
            reddb::json::Value::Object(capabilities),
        );
        result.insert(
            "serverInfo".to_string(),
            reddb::json::Value::Object(server_info),
        );
        reddb::mcp::protocol::build_result_message(id, reddb::json::Value::Object(result))
    }

    fn handle_tools_list(&self, id: Option<&reddb::json::Value>) -> String {
        let mut tools_json: Vec<reddb::json::Value> = reddb::mcp::tools::all_tools()
            .into_iter()
            .map(|def| {
                let mut obj = reddb::json::Map::new();
                obj.insert(
                    "name".to_string(),
                    reddb::json::Value::String(def.name.to_string()),
                );
                obj.insert(
                    "description".to_string(),
                    reddb::json::Value::String(def.descriptor_description()),
                );
                obj.insert("inputSchema".to_string(), def.input_schema);
                reddb::json::Value::Object(obj)
            })
            .collect();
        tools_json.push(reddb::mcp::tools::ask_descriptor());

        let mut result = reddb::json::Map::new();
        result.insert("tools".to_string(), reddb::json::Value::Array(tools_json));
        reddb::mcp::protocol::build_result_message(id, reddb::json::Value::Object(result))
    }

    fn handle_resources_list(&self, id: Option<&reddb::json::Value>) -> String {
        let resources: Vec<reddb::json::Value> = reddb::mcp::tools::knowledge_resources()
            .iter()
            .map(|res| {
                let mut obj = reddb::json::Map::new();
                obj.insert(
                    "uri".to_string(),
                    reddb::json::Value::String(res.uri.to_string()),
                );
                obj.insert(
                    "name".to_string(),
                    reddb::json::Value::String(res.title.to_string()),
                );
                obj.insert(
                    "description".to_string(),
                    reddb::json::Value::String(res.description.to_string()),
                );
                obj.insert(
                    "mimeType".to_string(),
                    reddb::json::Value::String(res.mime_type.to_string()),
                );
                reddb::json::Value::Object(obj)
            })
            .collect();
        let mut result = reddb::json::Map::new();
        result.insert(
            "resources".to_string(),
            reddb::json::Value::Array(resources),
        );
        reddb::mcp::protocol::build_result_message(id, reddb::json::Value::Object(result))
    }

    fn handle_resources_read(
        &self,
        id: Option<&reddb::json::Value>,
        params: Option<&reddb::json::Value>,
    ) -> String {
        let Some(uri) = params
            .and_then(|p| p.get("uri"))
            .and_then(|value| value.as_str())
        else {
            return reddb::mcp::protocol::build_error_message(id, -32602, "missing resource uri");
        };
        let resources = reddb::mcp::tools::knowledge_resources();
        let Some(resource) = resources.iter().find(|res| res.uri == uri) else {
            return reddb::mcp::protocol::build_error_message(
                id,
                -32602,
                &format!("unknown resource: {uri}"),
            );
        };
        let mut contents = reddb::json::Map::new();
        contents.insert(
            "uri".to_string(),
            reddb::json::Value::String(resource.uri.to_string()),
        );
        contents.insert(
            "mimeType".to_string(),
            reddb::json::Value::String(resource.mime_type.to_string()),
        );
        contents.insert(
            "text".to_string(),
            reddb::json::Value::String((resource.body)()),
        );
        let mut result = reddb::json::Map::new();
        result.insert(
            "contents".to_string(),
            reddb::json::Value::Array(vec![reddb::json::Value::Object(contents)]),
        );
        reddb::mcp::protocol::build_result_message(id, reddb::json::Value::Object(result))
    }

    fn handle_tools_call(
        &self,
        id: Option<&reddb::json::Value>,
        params: Option<&reddb::json::Value>,
    ) -> String {
        let Some(name) = params
            .and_then(|p| p.get("name"))
            .and_then(|value| value.as_str())
        else {
            return reddb::mcp::protocol::build_error_message(id, -32602, "missing tool name");
        };
        let empty = reddb::json::Value::Object(reddb::json::Map::new());
        let args = params.and_then(|p| p.get("arguments")).unwrap_or(&empty);
        let posture = if matches!(self.options.auth, reddb_wire::ConnectionAuth::Anonymous) {
            reddb::mcp::tools::McpIdentityPosture::RemoteCredentialsMissing
        } else {
            reddb::mcp::tools::McpIdentityPosture::RemoteCredentialsPresented
        };
        let result = reddb::mcp::tools::authorize_tool(name, posture).and_then(|_| match name {
            "reddb_query" => self.remote_query_tool(args),
            _ => Err(format!(
                "remote MCP client mode currently forwards reddb_query only; tool requires embedded runtime: {name}"
            )),
        });
        mcp_tool_text_result(id, result)
    }

    fn remote_query_tool(&self, args: &reddb::json::Value) -> Result<String, String> {
        let sql = args
            .get("sql")
            .and_then(|value| value.as_str())
            .ok_or("missing required field 'sql'")?;
        if args.get("params").is_some() {
            return Err("remote MCP client mode does not support query params yet".to_string());
        }
        match &self.options.target {
            reddb_wire::ConnectionTarget::Grpc { endpoint } => {
                let mut client =
                    connect_mcp_grpc(endpoint, &self.options.auth, self.options.timeout)?;
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| format!("build runtime: {err}"))?;
                rt.block_on(async {
                    tokio::time::timeout(self.options.timeout, client.query(sql))
                        .await
                        .map_err(|_| "connection timeout".to_string())?
                        .map_err(|err| format!("{err}"))
                })
            }
            reddb_wire::ConnectionTarget::Http { base_url } => {
                let auth = http_auth_from_connection_auth(&self.options.auth);
                reddb_client::connector::http::query_one_shot(base_url, sql, &auth)
                    .map_err(|err| format!("{err}"))
            }
            reddb_wire::ConnectionTarget::RedWire { host, port, .. } => {
                let host = host.clone();
                let port = *port;
                let auth = redwire_auth_from_connection_auth(&self.options.auth)?;
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|err| format!("build runtime: {err}"))?;
                rt.block_on(async {
                    let opts =
                        reddb_client::redwire::ConnectOptions::new(host, port).with_auth(auth);
                    let mut client = tokio::time::timeout(
                        self.options.timeout,
                        reddb_client::redwire::RedWireClient::connect(opts),
                    )
                    .await
                    .map_err(|_| "connection timeout".to_string())?
                    .map_err(|err| format!("{err}"))?;
                    tokio::time::timeout(self.options.timeout, client.query_raw(sql))
                        .await
                        .map_err(|_| "query timeout".to_string())?
                        .map_err(|err| format!("{err}"))
                })
            }
            other => Err(format!(
                "remote MCP connector unsupported for target: {other:?}"
            )),
        }
    }
}

fn mcp_tool_text_result(id: Option<&reddb::json::Value>, result: Result<String, String>) -> String {
    let (text, is_error) = match result {
        Ok(text) => (text, false),
        Err(err) => (err, true),
    };
    let mut content = reddb::json::Map::new();
    content.insert(
        "type".to_string(),
        reddb::json::Value::String("text".to_string()),
    );
    content.insert("text".to_string(), reddb::json::Value::String(text));

    let mut result_obj = reddb::json::Map::new();
    result_obj.insert(
        "content".to_string(),
        reddb::json::Value::Array(vec![reddb::json::Value::Object(content)]),
    );
    if is_error {
        result_obj.insert("isError".to_string(), reddb::json::Value::Bool(true));
    }
    reddb::mcp::protocol::build_result_message(id, reddb::json::Value::Object(result_obj))
}

fn has_cli_vault_key() -> bool {
    reddb::utils::env_with_file_fallback("REDDB_CERTIFICATE").is_some()
}

fn attach_cli_vault(
    rt: &reddb::RedDBRuntime,
    required: bool,
) -> Result<Option<std::sync::Arc<reddb::auth::AuthStore>>, String> {
    let db = rt.db();
    let store = db.store();
    let Some(pager) = store.pager() else {
        if required {
            return Err("vault requires a persistent database".to_string());
        }
        return Ok(None);
    };

    let has_saved_vault = reddb::auth::vault::Vault::has_saved_state(pager);
    if !has_cli_vault_key() {
        if required || has_saved_vault {
            return Err(
                "vault export/import requires REDDB_CERTIFICATE or REDDB_CERTIFICATE_FILE"
                    .to_string(),
            );
        }
        return Ok(None);
    }

    let auth = std::sync::Arc::new(
        reddb::auth::AuthStore::with_vault(
            reddb::auth::AuthConfig::default(),
            std::sync::Arc::clone(pager),
        )
        .map_err(|err| format!("open vault: {err}"))?,
    );
    rt.set_auth_store(std::sync::Arc::clone(&auth));
    Ok(Some(auth))
}

/// Collect every `--param <value>` / `-p <value>` (and the optional following
/// `--param-type <ty>`) from raw argv. The schema-driven flag parser
/// only retains the LAST value of each flag in its `HashMap`, so the
/// query handler walks the original argv directly to support the
/// repeatable form that issue #375 asks for.
///
/// `@<path>` loads the JSON content of `<path>` as the parameter.
/// Without an explicit type, plain values are auto-typed by trying
/// to parse them as JSON first (so `42` → integer, `[1,2,3]` →
/// vector, `true` → boolean, `null` → Null) and falling back to text.
fn collect_query_params(args: &[String]) -> Result<Vec<reddb_client::Value>, String> {
    let mut pairs: Vec<(String, Option<String>)> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--param" || arg == "-p" {
            i += 1;
            let v = args
                .get(i)
                .ok_or_else(|| "--param/-p requires a value".to_string())?
                .clone();
            pairs.push((v, None));
        } else if let Some(rest) = arg.strip_prefix("--param=") {
            pairs.push((rest.to_string(), None));
        } else if let Some(rest) = arg.strip_prefix("-p=") {
            pairs.push((rest.to_string(), None));
        } else if arg == "--param-type" {
            i += 1;
            let ty = args
                .get(i)
                .ok_or_else(|| "--param-type requires a value".to_string())?
                .clone();
            let last = pairs
                .last_mut()
                .ok_or_else(|| "--param-type must follow a --param".to_string())?;
            last.1 = Some(ty);
        } else if let Some(rest) = arg.strip_prefix("--param-type=") {
            let last = pairs
                .last_mut()
                .ok_or_else(|| "--param-type must follow a --param".to_string())?;
            last.1 = Some(rest.to_string());
        }
        i += 1;
    }
    let mut out = Vec::with_capacity(pairs.len());
    for (raw, ty) in pairs {
        out.push(parse_cli_param(&raw, ty.as_deref())?);
    }
    Ok(out)
}

/// Map a CLI `--param` token (and optional `--param-type`) into a
/// driver `Value`. `@path` is unwrapped before type coercion so a
/// file holding a JSON vector or large text works with every type.
fn parse_cli_param(raw: &str, ty: Option<&str>) -> Result<reddb_client::Value, String> {
    use reddb_client::Value;
    let body: String = if let Some(path) = raw.strip_prefix('@') {
        std::fs::read_to_string(path).map_err(|e| format!("--param @{path}: {e}"))?
    } else {
        raw.to_string()
    };
    let trimmed = body.trim();
    match ty {
        Some("text") | Some("string") => Ok(Value::Text(body.clone())),
        Some("int") | Some("integer") => trimmed
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|e| format!("--param-type int: {e}")),
        Some("float") => trimmed
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|e| format!("--param-type float: {e}")),
        Some("bool") | Some("boolean") => trimmed
            .parse::<bool>()
            .map(Value::Bool)
            .map_err(|e| format!("--param-type bool: {e}")),
        Some("null") => Ok(Value::Null),
        Some("vec") | Some("vector") => json_str_to_vector(trimmed),
        Some("json") => {
            let parsed: reddb::json::Value =
                reddb::json::from_str(trimmed).map_err(|e| format!("--param-type json: {e}"))?;
            Ok(Value::Json(json_value_to_client(parsed)))
        }
        Some(other) => Err(format!("unknown --param-type: {other}")),
        None => Ok(auto_type_param(&body)),
    }
}

fn json_str_to_vector(s: &str) -> Result<reddb_client::Value, String> {
    use reddb::json::Value as J;
    let parsed: J = reddb::json::from_str(s).map_err(|e| format!("--param-type vec: {e}"))?;
    let J::Array(items) = parsed else {
        return Err("--param-type vec: expected a JSON array of numbers".into());
    };
    let mut out = Vec::with_capacity(items.len());
    for v in &items {
        match v {
            J::Number(n) => out.push(*n as f32),
            _ => return Err("--param-type vec: array must contain only numbers".into()),
        }
    }
    Ok(reddb_client::Value::Vector(out))
}

/// Auto-type a CLI string: try JSON first (covers ints, floats, bools,
/// null, arrays, objects) and fall back to text. Mirrors the
/// `json_value_to_schema_value` mapping used by HTTP `params`.
fn auto_type_param(s: &str) -> reddb_client::Value {
    use reddb::json::Value as J;
    use reddb_client::Value;
    let trimmed = s.trim();
    if let Ok(parsed) = reddb::json::from_str::<J>(trimmed) {
        return match parsed {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(b),
            J::Integer(n) => Value::Int(n),
            J::Number(n) => {
                if n.fract() == 0.0 && n.abs() < i64::MAX as f64 {
                    Value::Int(n as i64)
                } else {
                    Value::Float(n)
                }
            }
            J::Decimal(n) => Value::Text(n),
            J::String(t) => Value::Text(t),
            J::Array(items) => {
                if items
                    .iter()
                    .all(|v| matches!(v, J::Integer(_) | J::Number(_)))
                {
                    let floats: Vec<f32> = items
                        .iter()
                        .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                        .collect();
                    Value::Vector(floats)
                } else {
                    Value::Text(reddb::json::to_string(&J::Array(items)).unwrap_or_default())
                }
            }
            J::Object(_) => Value::Text(reddb::json::to_string(&parsed).unwrap_or_default()),
        };
    }
    Value::Text(s.to_string())
}

fn json_value_to_client(value: reddb::json::Value) -> reddb_client::JsonValue {
    use reddb::json::Value;
    match value {
        Value::Null => reddb_client::JsonValue::Null,
        Value::Bool(value) => reddb_client::JsonValue::Bool(value),
        Value::Integer(value) => reddb_client::JsonValue::Number(value as f64),
        Value::Number(value) => reddb_client::JsonValue::Number(value),
        Value::Decimal(value) => reddb_client::JsonValue::String(value),
        Value::String(value) => reddb_client::JsonValue::String(value),
        Value::Array(values) => {
            reddb_client::JsonValue::Array(values.into_iter().map(json_value_to_client).collect())
        }
        Value::Object(values) => reddb_client::JsonValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, json_value_to_client(value)))
                .collect(),
        ),
    }
}

// DATA COMMANDS BEGIN (issue #2124)

fn run_data_command(
    command: &str,
    flags: &HashMap<String, FlagValue>,
    args: &[String],
    remaining: &[String],
) {
    let json_mode = wants_json(flags);
    let row_format = parse_row_format(flags).unwrap_or_else(|err| {
        if json_mode {
            json_error(command, &err);
        }
        eprintln!("{command}: {err}");
        std::process::exit(1);
    });

    if command == "query" && remaining.len() >= 2 && is_ephemeral_data_file(&remaining[0]) {
        let file_count = remaining
            .iter()
            .take_while(|arg| is_ephemeral_data_file(arg))
            .count();
        run_ephemeral_query(
            args,
            &remaining[..file_count],
            remaining.get(file_count).map(String::as_str).unwrap_or(""),
            json_mode,
            row_format,
            flag_string(flags, "save"),
            flag_bool(flags, "dry-run"),
        );
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|err| {
            eprintln!("{command}: failed to build async runtime: {err}");
            std::process::exit(1);
        });
    let uri = data_connection_uri(flags, args);
    // `--path` resolves the storage profile/packaging flags + env exactly like
    // the pre-driver `open_local_runtime` did; other targets go through the
    // driver's URI connect.
    let client_result = match flag_string(flags, "path").filter(|path| !path.is_empty()) {
        Some(path) => resolve_storage_profile(flags, "local")
            .and_then(|profile| {
                reddb::api::RedDBOptions::persistent(&path)
                    .with_storage_profile(profile)
                    .map_err(|e| format!("storage profile: {e}"))
            })
            .and_then(|options| {
                reddb_client::embedded::EmbeddedClient::open_with_options(options)
                    .map(reddb_client::Reddb::Embedded)
                    .map_err(|err| err.to_string())
            }),
        None => runtime
            .block_on(reddb_client::Reddb::connect(&uri))
            .map_err(|err| err.to_string()),
    };
    let client = client_result.unwrap_or_else(|err| {
        if json_mode {
            json_error(command, &err);
        }
        eprintln!("{command}: {err}");
        std::process::exit(1);
    });

    let (result, dry_run): (Result<QueryResult, String>, Option<(&str, String)>) = match command {
        "query" => {
            let sql = remaining.first().map(String::as_str).unwrap_or("");
            if sql.is_empty() {
                if json_mode {
                    json_error("query", "Usage: red query [--path file] <sql>");
                }
                eprintln!("Usage: red query [--path file] <sql>");
                eprintln!("Example: red query \"SELECT * FROM users\"");
                std::process::exit(1);
            }
            let params = collect_query_params(args).unwrap_or_else(|err| {
                if json_mode {
                    json_error("query", &err);
                }
                eprintln!("query: {err}");
                std::process::exit(1);
            });
            let dry_run = flag_bool(flags, "dry-run");
            let preview = dry_run.then(|| format!("EXPLAIN {sql}"));
            let sql_to_run = preview.as_deref().unwrap_or(sql);
            let result = if params.is_empty() {
                runtime.block_on(client.query(sql_to_run))
            } else {
                runtime.block_on(client.query_with(sql_to_run, params))
            };
            (
                result.map_err(|err| err.to_string()),
                preview.map(|preview| (sql, preview)),
            )
        }
        "insert" => {
            if remaining.len() < 2 {
                data_usage_error_with_example(
                    "insert",
                    "Usage: red insert [--path file] <collection> <json>",
                    "Example: red insert users '{\"name\": \"Alice\"}'",
                    json_mode,
                );
            }
            let collection = &remaining[0];
            let parsed: reddb::json::Value =
                reddb::json::from_str(&remaining[1]).unwrap_or_else(|err| {
                    if json_mode {
                        json_error("insert", &format!("invalid JSON: {err}"));
                    }
                    eprintln!("invalid JSON: {err}");
                    std::process::exit(1);
                });
            let object = match parsed {
                reddb::json::Value::Object(map) => map,
                _ => data_usage_error("insert", "expected a JSON object", json_mode),
            };
            let mut columns = Vec::with_capacity(object.len());
            let mut values = Vec::with_capacity(object.len());
            for (key, value) in object {
                columns.push(key);
                values.push(match value {
                    reddb::json::Value::String(value) => {
                        format!("'{}'", value.replace('\'', "''"))
                    }
                    reddb::json::Value::Integer(value) => value.to_string(),
                    reddb::json::Value::Number(value) => value.to_string(),
                    reddb::json::Value::Decimal(value) => {
                        format!("'{}'", value.replace('\'', "''"))
                    }
                    reddb::json::Value::Bool(value) => value.to_string(),
                    reddb::json::Value::Null => "NULL".to_string(),
                    other => format!(
                        "'{}'",
                        reddb::json::to_string(&other)
                            .unwrap_or_default()
                            .replace('\'', "''")
                    ),
                });
            }
            let sql = format!(
                "INSERT INTO {collection} ({}) VALUES ({})",
                columns.join(", "),
                values.join(", ")
            );
            (
                runtime
                    .block_on(client.query(&sql))
                    .map_err(|err| err.to_string()),
                None,
            )
        }
        "get" => {
            if remaining.len() < 2 {
                data_usage_error_with_example(
                    "get",
                    "Usage: red get [--path file] <collection> <id>",
                    "Example: red get users 42",
                    json_mode,
                );
            }
            let sql = format!(
                "SELECT * FROM {} WHERE _entity_id = {}",
                remaining[0], remaining[1]
            );
            (
                runtime
                    .block_on(client.query(&sql))
                    .map_err(|err| err.to_string()),
                None,
            )
        }
        "delete" => {
            if remaining.len() < 2 {
                data_usage_error_with_example(
                    "delete",
                    "Usage: red delete [--path file] <collection> <id>",
                    "Example: red delete users 42",
                    json_mode,
                );
            }
            let sql = format!(
                "DELETE FROM {} WHERE _entity_id = {}",
                remaining[0], remaining[1]
            );
            (
                runtime
                    .block_on(client.query(&sql))
                    .map_err(|err| err.to_string()),
                None,
            )
        }
        other => (Err(format!("unsupported data command: {other}")), None),
    };

    let close_result = runtime
        .block_on(client.close())
        .map_err(|err| err.to_string());
    let result = result.and_then(|result| close_result.map(|()| result));
    match result {
        Ok(result) => emit_data_result(command, &result, row_format, json_mode, dry_run),
        Err(err) => {
            if json_mode {
                json_error(command, &err);
            }
            eprintln!("{command} error: {err}");
            std::process::exit(1);
        }
    }
}

fn data_connection_uri(flags: &HashMap<String, FlagValue>, args: &[String]) -> String {
    if let Some(path) = flag_string(flags, "path").filter(|path| !path.is_empty()) {
        return format!("file://{path}");
    }
    if cli_flag_supplied(args, "bind", 'b') {
        let bind = flag_string(flags, "bind").unwrap_or_else(|| "127.0.0.1:5000".to_string());
        if bind.contains("://") {
            return bind;
        }
        return format!("http://{bind}");
    }
    "memory://".to_string()
}

fn cli_flag_supplied(args: &[String], long: &str, short: char) -> bool {
    let long = format!("--{long}");
    let long_prefix = format!("{long}=");
    let short = format!("-{short}");
    let short_prefix = format!("{short}=");
    args.iter().any(|arg| {
        arg == &long
            || arg.starts_with(&long_prefix)
            || arg == &short
            || arg.starts_with(&short_prefix)
    })
}

fn data_usage_error_with_example(command: &str, usage: &str, example: &str, json_mode: bool) -> ! {
    if json_mode {
        json_error(command, usage);
    }
    eprintln!("{usage}");
    eprintln!("{example}");
    std::process::exit(1);
}

fn data_usage_error(command: &str, usage: &str, json_mode: bool) -> ! {
    if json_mode {
        json_error(command, usage);
    }
    eprintln!("{usage}");
    std::process::exit(1);
}

fn emit_data_result(
    command: &str,
    result: &QueryResult,
    format: RowFormat,
    json_mode: bool,
    dry_run: Option<(&str, String)>,
) {
    if json_mode {
        let rows = String::from_utf8(format_query_result(result, RowFormat::Json))
            .expect("driver JSON RowFormat is UTF-8");
        let mut body = format!(
            "{{\"statement\":\"{}\",\"affected\":{},\"rows\":{}",
            json_escape(&result.statement),
            result.affected,
            rows.trim_end()
        );
        if let Some(notice) = result.notice.as_deref() {
            body.push_str(&format!(",\"notice\":\"{}\"", json_escape(notice)));
        }
        body.push('}');
        // Dry-run keeps the documented envelope: the result nests under
        // `preview`, never flattened to the top level.
        let data = if let Some((statement, preview)) = dry_run {
            format!(
                "{{\"statement\":\"explain\",\"dry_run\":true,\"would_run\":\"{}\",\"preview_statement\":\"{}\",\"preview\":{}}}",
                json_escape(statement),
                json_escape(&preview),
                body
            )
        } else {
            body
        };
        json_ok(command, &data);
        return;
    }
    if let Some((statement, preview)) = dry_run {
        println!("dry-run: {statement}");
        println!("preview: {preview}");
    }
    // Writes keep their affected-row feedback: a successful DML must never
    // render as "(no rows)".
    if result.statement != "select" && result.rows.is_empty() {
        println!(
            "{} ok ({} row{} affected)",
            result.statement,
            result.affected,
            if result.affected == 1 { "" } else { "s" },
        );
        return;
    }
    std::io::stdout()
        .write_all(&format_query_result(result, format))
        .expect("write stdout");
}

// DATA COMMANDS END (issue #2124)

/// Print an error JSON envelope to **stderr** and exit with code 1.
fn json_error(command: &str, error: &str) -> ! {
    eprintln!(
        "{{\"ok\":false,\"command\":\"{}\",\"error\":\"{}\"}}",
        json_escape(command),
        json_escape(error)
    );
    std::process::exit(1);
}

fn main() {
    // PLAN.md Phase 6.4 — expand `*_FILE` env companions before any
    // other env reads. Containerised deployments mount tmpfs secrets
    // at /run/secrets/x and point e.g. `REDDB_PASSWORD_FILE` at the
    // mount; we read the file, place the contents in `REDDB_PASSWORD`,
    // and strip the `_FILE` var so it can't leak into `env` dumps.
    // No threads are alive yet, so the unsafe `set_var` is sound.
    if let Some((name, err)) = reddb::utils::expand_all_reddb_secrets().into_iter().next() {
        eprintln!("error: failed to expand {name}_FILE: {err}");
        std::process::exit(2);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();

    // Handle empty args early.
    if args.is_empty() {
        print!("{}", cli::commands::main_help_text());
        return;
    }

    // Handle --complete before normal parsing (shell completion mode).
    if args.first().map(|s| s.as_str()) == Some("--complete") {
        let rest: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();
        let domain_tree = build_completion_tree();
        let completions = cli::complete::complete_partial(&rest, &domain_tree);
        for c in completions {
            println!("{}", c);
        }
        return;
    }

    // Identify the command: first positional token (not starting with -).
    let command = identify_command(&args);

    // Build the appropriate flag schema based on the identified command.
    let flags = cli::commands::flags_for_command(command.as_deref());

    // Tokenize and parse with the full schema.
    let tokens = cli::token::tokenize(&args);
    let parser = cli::schema::SchemaParser::new(flags);
    let result = parser.parse(&tokens);

    // Handle --help with no command or before command detection.
    if result.flags.get("help").is_some_and(|v| v.is_truthy()) {
        match command.as_deref() {
            Some(cmd) => match cli::commands::command_help_text(cmd) {
                Some(text) => {
                    print!("{}", text);
                    return;
                }
                None => {
                    print!("{}", cli::commands::main_help_text());
                    return;
                }
            },
            None => {
                print!("{}", cli::commands::main_help_text());
                return;
            }
        }
    }

    // Handle --version.
    if result.flags.get("version").is_some_and(|v| v.is_truthy()) {
        if wants_json(&result.flags) {
            json_ok(
                "version",
                &format!("{{\"version\":\"{}\"}}", env!("CARGO_PKG_VERSION")),
            );
        } else {
            println!("reddb {}", env!("CARGO_PKG_VERSION"));
        }
        return;
    }

    // Check for parse errors.
    if !result.errors.is_empty() {
        if wants_json(&result.flags) {
            let msgs: Vec<String> = result
                .errors
                .iter()
                .map(|e| json_escape(&e.format_human()))
                .collect();
            json_error("parse", &msgs.join("; "));
        }
        for err in &result.errors {
            eprint!("{}", err.format_human());
        }
        std::process::exit(1);
    }

    // Extract positionals (command was identified separately).
    let positionals = &result.positionals;

    // No command in positionals: show help.
    if positionals.is_empty() {
        print!("{}", cli::commands::main_help_text());
        return;
    }

    let cmd = positionals[0].as_str();
    let remaining = &positionals[1..];

    // Dispatch to commands.
    match cmd {
        "help" => {
            if let Some(cmd_name) = remaining.first() {
                match cli::commands::command_help_text(cmd_name) {
                    Some(text) => print!("{}", text),
                    None => {
                        eprintln!("Unknown command: {}", cmd_name);
                        eprintln!("Run 'red help' for a list of commands.");
                        std::process::exit(1);
                    }
                }
            } else {
                print!("{}", cli::commands::main_help_text());
            }
        }

        "version" => {
            if wants_json(&result.flags) {
                json_ok(
                    "version",
                    &format!("{{\"version\":\"{}\"}}", env!("CARGO_PKG_VERSION")),
                );
            } else {
                println!("reddb {}", env!("CARGO_PKG_VERSION"));
            }
        }

        "doctor" => {
            std::process::exit(run_doctor(&result));
        }

        "bootstrap" => {
            std::process::exit(run_bootstrap_command(&result.flags));
        }

        "server" => {
            let json_mode = wants_json(&result.flags);
            let config = build_server_config(&result.flags, None).unwrap_or_else(|err| {
                if json_mode {
                    json_error("server", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            if json_mode {
                eprintln!("{}", server_command_json("server", &config));
            }
            if let Err(err) = run_server(config) {
                if json_mode {
                    json_error("server", &err.to_string());
                }
                eprintln!("red server: {err}");
                std::process::exit(1);
            }
        }

        "service" => {
            let json_mode = wants_json(&result.flags);
            let subcommand = remaining.first().map(|s| s.as_str()).unwrap_or("help");

            match subcommand {
                "install" => {
                    let config =
                        build_systemd_service_config(&result.flags).unwrap_or_else(|err| {
                            if json_mode {
                                json_error("service.install", &err);
                            }
                            eprintln!("error: {err}");
                            std::process::exit(1);
                        });

                    install_systemd_service(&config).unwrap_or_else(|err| {
                        if json_mode {
                            json_error("service.install", &err);
                        }
                        eprintln!("error: {err}");
                        std::process::exit(1);
                    });

                    let unit_name = format!("{}.service", config.service_name);
                    if json_mode {
                        json_ok(
                            "service.install",
                            &format!(
                                "{{\"unit\":\"{}\",\"path\":\"{}\",\"router_bind\":{},\"grpc_bind\":{},\"http_bind\":{}}}",
                                json_escape(&unit_name),
                                json_escape(&config.unit_path().display().to_string()),
                                json_optional_string(config.router_bind_addr.as_deref()),
                                json_optional_string(config.grpc_bind_addr.as_deref()),
                                json_optional_string(config.http_bind_addr.as_deref())
                            ),
                        );
                    } else {
                        println!("Installed and started {}", unit_name);
                        println!("Status: systemctl status {}", unit_name);
                    }
                }
                "print-unit" => {
                    let config =
                        build_systemd_service_config(&result.flags).unwrap_or_else(|err| {
                            if json_mode {
                                json_error("service.print-unit", &err);
                            }
                            eprintln!("error: {err}");
                            std::process::exit(1);
                        });
                    let unit = render_systemd_unit(&config);
                    if json_mode {
                        json_ok("service.print-unit", &format!("{{\"unit\":{:?}}}", unit));
                    } else {
                        print!("{unit}");
                    }
                }
                _ => {
                    let help = "Usage: red service <install|print-unit> [flags]\n\nExamples:\n  sudo red service install --binary /usr/local/bin/red --bind 0.0.0.0:5050 --path /var/lib/reddb/data.rdb\n  red service print-unit --http --path /var/lib/reddb/data.rdb --bind 127.0.0.1:5000\n";
                    if json_mode {
                        json_ok("service", "{\"subcommands\":[\"install\",\"print-unit\"]}");
                    } else {
                        print!("{help}");
                    }
                }
            }
        }

        "replica" => {
            let json_mode = wants_json(&result.flags);
            let config =
                build_server_config(&result.flags, Some("replica")).unwrap_or_else(|err| {
                    if json_mode {
                        json_error("replica", &err);
                    }
                    eprintln!("error: {err}");
                    std::process::exit(1);
                });
            if json_mode {
                eprintln!("{}", server_command_json("replica", &config));
            }
            if let Err(err) = run_server(config) {
                if json_mode {
                    json_error("replica", &err.to_string());
                }
                eprintln!("red replica: {err}");
                std::process::exit(1);
            }
        }

        "rpc" => {
            let stdio = result.flags.get("stdio").is_some_and(|v| v.is_truthy());
            if !stdio {
                eprintln!("Usage: red rpc --stdio [--path file | --connect grpc://host:port]");
                eprintln!("Only --stdio mode is currently implemented.");
                std::process::exit(1);
            }
            // Remote mode: --connect grpc://host:port forwards every
            // JSON-RPC call via tonic. No local engine is opened.
            if let Some(connect) = flag_string(&result.flags, "connect") {
                if !connect.is_empty() {
                    let token = flag_string(&result.flags, "token").filter(|s| !s.is_empty());
                    let endpoint = connect
                        .strip_prefix("grpc://")
                        .map(|rest| format!("http://{rest}"))
                        .unwrap_or_else(|| connect.clone());
                    let code = reddb::rpc_stdio::run_remote(&endpoint, token);
                    std::process::exit(code);
                }
            }
            // Local mode: open the engine in-process (path or memory).
            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                eprintln!("rpc: {err}");
                std::process::exit(1);
            });
            let code = reddb::rpc_stdio::run(&rt);
            let _ = rt.checkpoint();
            std::process::exit(code);
        }

        "query" | "insert" | "get" | "delete" => {
            run_data_command(
                command.as_deref().unwrap_or_default(),
                &result.flags,
                &args,
                remaining,
            );
        }

        "health" => {
            let json_mode = wants_json(&result.flags);
            let explicit_transport =
                result.flags.contains_key("grpc") || result.flags.contains_key("http");
            let transport = select_transport(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("health", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let bind_addr = flag_string(&result.flags, "bind").unwrap_or_else(|| {
                if explicit_transport {
                    transport.default_bind_addr().to_string()
                } else {
                    reddb::service_cli::DEFAULT_ROUTER_BIND_ADDR.to_string()
                }
            });
            let transport_label = if explicit_transport {
                transport.as_str()
            } else {
                "router"
            };
            let ok = probe_listener(&bind_addr, Duration::from_secs(1));
            if json_mode {
                json_ok(
                    "health",
                    &format!(
                        "{{\"healthy\":{},\"transport\":\"{}\",\"address\":\"{}\"}}",
                        ok,
                        json_escape(transport_label),
                        json_escape(&bind_addr)
                    ),
                );
                if !ok {
                    std::process::exit(1);
                }
            } else if ok {
                println!("ok {} {}", transport_label, bind_addr);
            } else {
                eprintln!("unreachable {} {}", transport_label, bind_addr);
                std::process::exit(1);
            }
        }

        "tick" => {
            let json_mode = wants_json(&result.flags);
            let bind_addr =
                flag_string(&result.flags, "bind").unwrap_or_else(|| "127.0.0.1:5000".to_string());
            let operations = flag_string(&result.flags, "operations");
            let dry_run = flag_bool(&result.flags, "dry-run");

            let (runtime, client) = operator_http_client(&bind_addr, None).unwrap_or_else(|err| {
                if json_mode {
                    json_error("tick", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let payload = build_tick_payload(operations.as_deref(), dry_run);
            let body = runtime
                .block_on(client.post_json("/tick", &payload))
                .unwrap_or_else(|err| {
                    if json_mode {
                        json_error("tick", &err.to_string());
                    }
                    eprintln!("error: {err}");
                    std::process::exit(1);
                });

            if json_mode {
                // The body from /tick is already JSON; wrap it in the envelope.
                json_ok("tick", &body);
            } else {
                println!("{body}");
            }
        }

        "migrate-from-redis" => {
            std::process::exit(run_migrate_from_redis_command(&result.flags));
        }

        "migrate-pager-zone" => {
            std::process::exit(run_migrate_pager_zone_command(&result.flags));
        }

        "salvage" => {
            std::process::exit(run_salvage_command(&result.flags));
        }

        "status" => {
            let json_mode = wants_json(&result.flags);
            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("status", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let stats = rt.stats();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let uptime_ms = now_ms.saturating_sub(stats.started_at_unix_ms);
            let journal_enabled = reddb::seqn_journal_enabled();
            let journal_retention = reddb::seqn_journal_retention();
            let (audit_dest, slow_dest) = reddb::tier_wiring::current_log_destinations();
            let audit_desc = audit_dest.describe();
            let slow_desc = slow_dest.describe();
            if json_mode {
                json_ok(
                    "status",
                    &format!(
                        "{{\"uptime_ms\":{},\"collections\":{},\"entities\":{},\"pid\":{},\"seqn_journal\":{{\"enabled\":{},\"retention\":{}}},\"logs\":{{\"audit\":\"{}\",\"slow\":\"{}\"}}}}",
                        uptime_ms,
                        stats.store.collection_count,
                        stats.store.total_entities,
                        stats.system.pid,
                        journal_enabled,
                        journal_retention,
                        audit_desc.replace('\\', "\\\\").replace('"', "\\\""),
                        slow_desc.replace('\\', "\\\\").replace('"', "\\\""),
                    ),
                );
            } else {
                println!("uptime_ms:   {}", uptime_ms);
                println!("collections: {}", stats.store.collection_count);
                println!("entities:    {}", stats.store.total_entities);
                println!("pid:         {}", stats.system.pid);
                println!("hostname:    {}", stats.system.hostname);
                println!("os/arch:     {}/{}", stats.system.os, stats.system.arch);
                println!(
                    "seqn_journal: {} (retention={})",
                    if journal_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    },
                    journal_retention,
                );
                println!("audit_log:   {}", audit_desc);
                println!("slow_log:    {}", slow_desc);
            }
        }

        "inspect" => {
            run_inspect_command(&result.flags, remaining);
        }

        "mcp" => {
            let mcp_client_options =
                resolve_mcp_client_options(&result.flags).unwrap_or_else(|err| {
                    eprintln!("red mcp: {err}");
                    std::process::exit(1);
                });
            let url_embedded_target = if let Some(options) = mcp_client_options {
                match &options.target {
                    reddb_wire::ConnectionTarget::Memory => {
                        Some(reddb_wire::ConnectionTarget::Memory)
                    }
                    reddb_wire::ConnectionTarget::File { path } => {
                        Some(reddb_wire::ConnectionTarget::File { path: path.clone() })
                    }
                    _ => {
                        let code = run_mcp_remote(options);
                        std::process::exit(code);
                    }
                }
            } else {
                None
            };
            let path = result
                .flags
                .get("path")
                .map(|v| v.as_str_value())
                .unwrap_or_default();
            let runtime = match url_embedded_target {
                Some(reddb_wire::ConnectionTarget::Memory) => {
                    reddb::runtime::RedDBRuntime::in_memory().unwrap()
                }
                Some(reddb_wire::ConnectionTarget::File { path }) => {
                    reddb::runtime::RedDBRuntime::with_options(
                        reddb::api::RedDBOptions::persistent(&path),
                    )
                    .unwrap()
                }
                _ if path.is_empty() => reddb::runtime::RedDBRuntime::in_memory().unwrap(),
                _ => reddb::runtime::RedDBRuntime::with_options(
                    reddb::api::RedDBOptions::persistent(&path),
                )
                .unwrap(),
            };
            let mut server = reddb::mcp::server::McpServer::new(runtime);
            server.run_stdio();
        }

        "auth" => {
            let json_mode = wants_json(&result.flags);
            let subcommand = result
                .positionals
                .first()
                .map(|s| s.as_str())
                .unwrap_or("help");
            let _rt = reddb::RedDBRuntime::in_memory().expect("failed to create runtime");
            let auth_store = std::sync::Arc::new(reddb::auth::store::AuthStore::new(
                reddb::auth::AuthConfig {
                    enabled: true,
                    ..Default::default()
                },
            ));

            match subcommand {
                "bootstrap" => {
                    let user = result
                        .flags
                        .get("user")
                        .map(|v| v.as_str_value())
                        .unwrap_or_else(|| "admin".to_string());
                    let password = result
                        .flags
                        .get("password")
                        .map(|v| v.as_str_value())
                        .unwrap_or_else(|| {
                            if json_mode {
                                json_error(
                                    "auth.bootstrap",
                                    "--password is required for bootstrap",
                                );
                            }
                            eprintln!("error: --password is required for bootstrap");
                            std::process::exit(1);
                        });

                    match auth_store.bootstrap(&user, &password) {
                        Ok(br) => {
                            if json_mode {
                                let cert_json = br
                                    .certificate
                                    .as_ref()
                                    .map(|c| format!("\"{}\"", json_escape(c)))
                                    .unwrap_or_else(|| "null".to_string());
                                json_ok(
                                    "auth.bootstrap",
                                    &format!(
                                        "{{\"username\":\"{}\",\"role\":\"{}\",\"api_key\":\"{}\",\"certificate\":{}}}",
                                        json_escape(&br.user.username),
                                        json_escape(br.user.role.as_str()),
                                        json_escape(&br.api_key.key),
                                        cert_json
                                    ),
                                );
                            } else {
                                println!(
                                    "Admin user '{}' created (role: {})",
                                    br.user.username,
                                    br.user.role.as_str()
                                );
                                println!("API Key: {}", br.api_key.key);

                                if let Some(cert) = br.certificate {
                                    println!();
                                    println!(
                                        "CERTIFICATE (save this — required to unseal the vault):"
                                    );
                                    println!("  {}", cert);
                                    println!();
                                    println!("Without this certificate, the vault cannot be decrypted after restart.");
                                } else {
                                    println!();
                                    println!("Save this API key — it will not be shown again.");
                                }
                            }
                        }
                        Err(e) => {
                            if json_mode {
                                json_error("auth.bootstrap", &format!("{e}"));
                            }
                            eprintln!("error: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                _ => {
                    if json_mode {
                        json_ok(
                            "auth",
                            "{\"subcommands\":[\"bootstrap\",\"create-user\",\"list-users\",\"login\"],\"message\":\"use a subcommand, e.g. red auth bootstrap --password s3cret!\"}",
                        );
                    } else {
                        println!("Usage: red auth <subcommand>");
                        println!();
                        println!("Subcommands:");
                        println!(
                            "  bootstrap    Create the first admin user (only when no users exist)"
                        );
                        println!("  create-user  Create a new user (requires admin)");
                        println!("  list-users   List all users");
                        println!("  login        Login and get a session token");
                        println!();
                        println!("Examples:");
                        println!("  red auth bootstrap --password s3cret!");
                        println!(
                            "  red auth create-user --user alice --password pass --role write"
                        );
                    }
                }
            }
        }

        "connect" => {
            let json_mode = wants_json(&result.flags);
            let addr = remaining
                .first()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "localhost:6380".to_string());
            let token = result.flags.get("token").map(|v| v.as_str_value());
            let one_shot_query = result.flags.get("query").map(|v| v.as_str_value());

            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");

            rt.block_on(async {
                let mut client = match reddb::client::RedDBClient::connect(&addr, token).await {
                    Ok(c) => c,
                    Err(e) => {
                        if json_mode {
                            json_error("connect", &format!("Failed to connect to {}: {}", addr, e));
                        }
                        eprintln!("Failed to connect to {}: {}", addr, e);
                        std::process::exit(1);
                    }
                };

                if let Some(query) = one_shot_query {
                    // One-shot mode: execute a single query and exit
                    match client.query(&query).await {
                        Ok(json) => {
                            if json_mode {
                                json_ok("connect", &json);
                            } else {
                                println!("{}", json);
                            }
                        }
                        Err(e) => {
                            if json_mode {
                                json_error("connect", &format!("{}", e));
                            }
                            eprintln!("error: {}", e);
                            std::process::exit(1);
                        }
                    }
                } else {
                    // Interactive REPL -- JSON mode not applicable
                    reddb::client::repl::run_repl(&mut client).await;
                }
            });
        }

        "dump" => {
            // red dump [--path file] [--collection NAME] [-o FILE]
            //
            // JSONL format: one `{"collection": "...", "fields": {...}}` per
            // line. A full dump also appends one encrypted vault-KV record
            // for secrets. `restore` reads the same format back. When
            // --collection is not provided, every collection in the database
            // is dumped.
            let json_mode = wants_json(&result.flags);
            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("dump", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let auth_store = attach_cli_vault(&rt, false).unwrap_or_else(|err| {
                if json_mode {
                    json_error("dump", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let store = rt.db().store();
            let mut targets: Vec<String> = match flag_string(&result.flags, "collection") {
                Some(name) if !name.is_empty() => {
                    let mut names = vec![name];
                    if names.iter().all(|name| name != "red_config")
                        && store.get_collection("red_config").is_some()
                    {
                        names.push("red_config".to_string());
                    }
                    names
                }
                _ => store.list_collections(),
            };
            targets.sort();
            targets.dedup();
            let output_path = flag_string(&result.flags, "output");

            let mut buf = String::new();
            let mut total_rows = 0usize;
            let mut secret_keys = 0usize;
            for collection in &targets {
                let manager = match store.get_collection(collection) {
                    Some(m) => m,
                    None => continue,
                };
                for entity in manager.query_all(|_| true) {
                    let mut row_obj = reddb::json::Map::new();
                    if let reddb::storage::EntityData::Row(ref row) = entity.data {
                        if let Some(named) = &row.named {
                            for (k, v) in named {
                                row_obj
                                    .insert(k.clone(), reddb::json::Value::String(v.to_string()));
                            }
                        }
                    }
                    let mut wrapper = reddb::json::Map::new();
                    wrapper.insert(
                        "collection".to_string(),
                        reddb::json::Value::String(collection.clone()),
                    );
                    wrapper.insert("fields".to_string(), reddb::json::Value::Object(row_obj));
                    let line = reddb::json::Value::Object(wrapper).to_string_compact();
                    buf.push_str(&line);
                    buf.push('\n');
                    total_rows += 1;
                }
            }

            if let Some(auth_store) = auth_store.as_ref() {
                match auth_store.vault_kv_export_encrypted() {
                    Ok(Some(blob)) => {
                        let mut keys = auth_store.vault_kv_keys();
                        keys.sort();
                        secret_keys = keys.len();
                        let keys_json = keys
                            .iter()
                            .map(|key| reddb::json::Value::String(key.clone()))
                            .collect();
                        let mut wrapper = reddb::json::Map::new();
                        wrapper.insert(
                            "kind".to_string(),
                            reddb::json::Value::String("reddb.vault_kv.v1".to_string()),
                        );
                        wrapper.insert("encrypted".to_string(), reddb::json::Value::Bool(true));
                        wrapper.insert("keys".to_string(), reddb::json::Value::Array(keys_json));
                        wrapper.insert("blob".to_string(), reddb::json::Value::String(blob));
                        buf.push_str(&reddb::json::Value::Object(wrapper).to_string_compact());
                        buf.push('\n');
                    }
                    Ok(None) => {}
                    Err(err) => {
                        if json_mode {
                            json_error("dump", &err.to_string());
                        }
                        eprintln!("dump error: {err}");
                        std::process::exit(1);
                    }
                }
            }

            match output_path {
                Some(path) if !path.is_empty() => {
                    if let Err(e) = std::fs::write(&path, &buf) {
                        if json_mode {
                            json_error("dump", &format!("write failed: {e}"));
                        }
                        eprintln!("write failed: {e}");
                        std::process::exit(1);
                    }
                    if json_mode {
                        json_ok(
                            "dump",
                            &format!(
                                "{{\"path\":\"{}\",\"rows\":{},\"collections\":{},\"secret_keys\":{}}}",
                                path,
                                total_rows,
                                targets.len(),
                                secret_keys
                            ),
                        );
                    } else {
                        println!(
                            "dumped {} rows from {} collection(s), {} secret key(s) to {}",
                            total_rows,
                            targets.len(),
                            secret_keys,
                            path
                        );
                    }
                }
                _ => {
                    // Stdout stream.
                    print!("{}", buf);
                    if json_mode {
                        json_ok(
                            "dump",
                            &format!(
                                "{{\"rows\":{},\"collections\":{},\"secret_keys\":{}}}",
                                total_rows,
                                targets.len(),
                                secret_keys
                            ),
                        );
                    }
                }
            }
        }

        "restore" => {
            // red restore [--path file] -i FILE [--collection NAME]
            //
            // Reads JSONL produced by `red dump`. Each line has a `collection`
            // and a `fields` object — we rebuild an INSERT per row. The
            // --collection flag overrides the embedded collection name,
            // useful for renames or partial imports.
            let json_mode = wants_json(&result.flags);
            let input_path = match flag_string(&result.flags, "input") {
                Some(p) if !p.is_empty() => p,
                _ => {
                    if json_mode {
                        json_error("restore", "--input / -i is required");
                    }
                    eprintln!("Usage: red restore -i FILE [--collection NAME] [--path DB]");
                    std::process::exit(1);
                }
            };
            let override_collection = flag_string(&result.flags, "collection");

            let file_text = std::fs::read_to_string(&input_path).unwrap_or_else(|e| {
                if json_mode {
                    json_error("restore", &format!("read failed: {e}"));
                }
                eprintln!("read failed: {e}");
                std::process::exit(1);
            });

            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("restore", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });

            let mut restored = 0usize;
            let mut errors = 0usize;
            let mut restored_secret_keys = 0usize;
            let mut auth_store: Option<std::sync::Arc<reddb::auth::AuthStore>> = None;
            for (line_no, line) in file_text.lines().enumerate() {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let parsed: reddb::json::Value = match reddb::json::from_str(trimmed) {
                    Ok(v) => v,
                    Err(_) => {
                        errors += 1;
                        eprintln!("line {}: invalid JSON", line_no + 1);
                        continue;
                    }
                };
                if let reddb::json::Value::Object(map) = &parsed {
                    if map.get("kind").and_then(|v| v.as_str()) == Some("reddb.vault_kv.v1") {
                        let keys: Vec<String> = map
                            .get("keys")
                            .and_then(|value| value.as_array())
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(|value| value.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let blob = match map.get("blob").and_then(|value| value.as_str()) {
                            Some(blob) => blob,
                            None => {
                                errors += 1;
                                eprintln!("line {}: missing vault export blob", line_no + 1);
                                continue;
                            }
                        };
                        if auth_store.is_none() {
                            match attach_cli_vault(&rt, true) {
                                Ok(store) => auth_store = store,
                                Err(err) => {
                                    errors += 1;
                                    eprintln!("line {}: {}", line_no + 1, err);
                                    continue;
                                }
                            }
                        }
                        let Some(auth) = auth_store.as_ref() else {
                            errors += 1;
                            eprintln!("line {}: vault is unavailable", line_no + 1);
                            continue;
                        };
                        match reddb::auth::vault::Vault::unseal_logical_export(blob) {
                            Ok(state) => match auth.vault_kv_try_import(state.kv) {
                                Ok(count) => restored_secret_keys += count,
                                Err(err) => {
                                    errors += 1;
                                    eprintln!("line {}: vault import failed: {}", line_no + 1, err);
                                }
                            },
                            Err(err) => {
                                errors += 1;
                                eprintln!(
                                    "line {}: vault import failed: {}; importing false placeholders",
                                    line_no + 1,
                                    err
                                );
                                match auth.vault_kv_try_import_placeholders(&keys) {
                                    Ok(count) => restored_secret_keys += count,
                                    Err(placeholder_err) => {
                                        eprintln!(
                                            "line {}: vault placeholder import failed: {}",
                                            line_no + 1,
                                            placeholder_err
                                        );
                                    }
                                }
                            }
                        }
                        continue;
                    }
                }
                let (collection, fields) = match &parsed {
                    reddb::json::Value::Object(map) => {
                        let embedded_collection = map
                            .get("collection")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let coll = if embedded_collection.as_deref() == Some("red_config") {
                            embedded_collection
                        } else {
                            override_collection.clone().or(embedded_collection)
                        };
                        let fields = map.get("fields").cloned();
                        match (coll, fields) {
                            (Some(c), Some(f)) => (c, f),
                            _ => {
                                errors += 1;
                                continue;
                            }
                        }
                    }
                    _ => {
                        errors += 1;
                        continue;
                    }
                };
                // Build INSERT INTO {collection} (cols) VALUES (vals)
                let obj = match fields {
                    reddb::json::Value::Object(m) => m,
                    _ => {
                        errors += 1;
                        continue;
                    }
                };
                let mut cols = Vec::new();
                let mut vals = Vec::new();
                for (k, v) in obj.iter() {
                    cols.push(k.clone());
                    vals.push(match v {
                        reddb::json::Value::String(s) => format!("'{}'", s.replace('\'', "''")),
                        reddb::json::Value::Number(n) => n.to_string(),
                        reddb::json::Value::Bool(b) => b.to_string(),
                        reddb::json::Value::Null => "NULL".to_string(),
                        other => format!("'{}'", other.to_string_compact().replace('\'', "''")),
                    });
                }
                let sql = format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    collection,
                    cols.join(", "),
                    vals.join(", ")
                );
                match rt.execute_query(&sql) {
                    Ok(_) => restored += 1,
                    Err(e) => {
                        errors += 1;
                        eprintln!("line {}: {}", line_no + 1, e);
                    }
                }
            }
            checkpoint_local_runtime(&rt);

            if json_mode {
                json_ok(
                    "restore",
                    &format!(
                        "{{\"restored\":{},\"secret_keys\":{},\"errors\":{}}}",
                        restored, restored_secret_keys, errors
                    ),
                );
            } else {
                println!(
                    "restored {} rows, {} secret key(s) ({} errors)",
                    restored, restored_secret_keys, errors
                );
            }
        }

        "pitr-list" => {
            // red pitr-list --snapshot-prefix DIR --wal-prefix DIR
            //
            // Enumerate restore points by reading the snapshot archive.
            // Phase 2.4 uses the LocalBackend adapter so callers point at a
            // filesystem directory; remote backends (S3/Turso/D1) hook in
            // the same way once credentials are threaded through the CLI.
            let json_mode = wants_json(&result.flags);
            let snapshot_prefix = flag_string(&result.flags, "snapshot-prefix")
                .unwrap_or_else(|| "./data/snapshots".to_string());
            let wal_prefix = flag_string(&result.flags, "wal-prefix")
                .unwrap_or_else(|| "./data/wal-archive".to_string());

            let backend = std::sync::Arc::new(reddb::storage::backend::local::LocalBackend)
                as std::sync::Arc<dyn reddb::storage::backend::RemoteBackend>;
            let pitr =
                reddb::storage::wal::PointInTimeRecovery::new(backend, snapshot_prefix, wal_prefix);

            match pitr.list_restore_points() {
                Ok(points) => {
                    if json_mode {
                        let mut out = String::from("[");
                        for (i, p) in points.iter().enumerate() {
                            if i > 0 {
                                out.push(',');
                            }
                            out.push_str(&format!(
                                "{{\"snapshot_id\":{},\"snapshot_time\":{},\"wal_segments\":{},\"latest_recoverable_time\":{}}}",
                                p.snapshot_id,
                                p.snapshot_time,
                                p.wal_segment_count,
                                p.latest_recoverable_time
                            ));
                        }
                        out.push(']');
                        json_ok("pitr-list", &out);
                    } else if points.is_empty() {
                        println!("no restore points found");
                    } else {
                        println!(
                            "{:<15} {:<24} {:<14} {:<24}",
                            "snapshot_id",
                            "snapshot_time (unix ms)",
                            "wal_segments",
                            "latest_recoverable_time"
                        );
                        for p in &points {
                            println!(
                                "{:<15} {:<24} {:<14} {:<24}",
                                p.snapshot_id,
                                p.snapshot_time,
                                p.wal_segment_count,
                                p.latest_recoverable_time
                            );
                        }
                    }
                }
                Err(err) => {
                    if json_mode {
                        json_error("pitr-list", &err.to_string());
                    }
                    eprintln!("pitr-list error: {err}");
                    std::process::exit(1);
                }
            }
        }

        "pitr-restore" => {
            // red pitr-restore --target-time UNIX_MS --dest PATH
            //                  --snapshot-prefix DIR --wal-prefix DIR
            //
            // Picks the latest snapshot whose `snapshot_time <= target_time`,
            // downloads it into --dest, then replays WAL segments until
            // target_time. target_time=0 means "replay everything available".
            let json_mode = wants_json(&result.flags);
            let target_time: u64 = flag_string(&result.flags, "target-time")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let dest = match flag_string(&result.flags, "dest") {
                Some(p) if !p.is_empty() => p,
                _ => {
                    if json_mode {
                        json_error("pitr-restore", "--dest is required");
                    }
                    eprintln!(
                        "Usage: red pitr-restore --dest PATH --target-time MS \
                               --snapshot-prefix DIR --wal-prefix DIR"
                    );
                    std::process::exit(1);
                }
            };
            let snapshot_prefix = flag_string(&result.flags, "snapshot-prefix")
                .unwrap_or_else(|| "./data/snapshots".to_string());
            let wal_prefix = flag_string(&result.flags, "wal-prefix")
                .unwrap_or_else(|| "./data/wal-archive".to_string());

            let backend = std::sync::Arc::new(reddb::storage::backend::local::LocalBackend)
                as std::sync::Arc<dyn reddb::storage::backend::RemoteBackend>;
            let pitr =
                reddb::storage::wal::PointInTimeRecovery::new(backend, snapshot_prefix, wal_prefix);

            match pitr.restore_to(target_time, std::path::Path::new(&dest)) {
                Ok(res) => {
                    if json_mode {
                        json_ok(
                            "pitr-restore",
                            &format!(
                                "{{\"snapshot_used\":{},\"wal_segments_replayed\":{},\"records_applied\":{},\"recovered_to_lsn\":{},\"recovered_to_time\":{}}}",
                                res.snapshot_used,
                                res.wal_segments_replayed,
                                res.records_applied,
                                res.recovered_to_lsn,
                                res.recovered_to_time
                            ),
                        );
                    } else {
                        println!(
                            "restored to {} at lsn {} (snapshot {}, {} WAL segments, {} records applied)",
                            res.recovered_to_time,
                            res.recovered_to_lsn,
                            res.snapshot_used,
                            res.wal_segments_replayed,
                            res.records_applied,
                        );
                    }
                }
                Err(err) => {
                    if json_mode {
                        json_error("pitr-restore", &err.to_string());
                    }
                    eprintln!("pitr-restore error: {err}");
                    std::process::exit(1);
                }
            }
        }

        "vcs" => {
            run_vcs_command(&result.flags, remaining);
        }

        "admin" => {
            run_admin_command(&result.flags, remaining);
        }

        "ui" => {
            run_ui_command(&result.flags, remaining);
        }

        _ => {
            if wants_json(&result.flags) {
                json_error("unknown", &format!("Unknown command: {}", cmd));
            }
            eprintln!("Unknown command: {}", cmd);
            eprintln!();
            print!("{}", cli::commands::main_help_text());
            std::process::exit(1);
        }
    }
}

/// Canonicalize a `file://` URI (or a bare path) to an absolute
/// `file:///…` target. A relative path is resolved against the current
/// working directory and then lexically normalized (`.`/`..` segments
/// folded) — no filesystem access touches the target itself, so a missing
/// file canonicalizes fine and the engine surfaces the open error later.
///
/// Issue #1042 / PRD #1041 (ADR 0047). Only the local-file forms are
/// supported here — `file://./rel.rdb` and `file:///abs/x.rdb`; a
/// `file://host/path` authority form is treated as a path, since the
/// bridge serves local databases only.
fn canonicalize_file_uri(input: &str) -> Result<String, String> {
    use std::path::{Component, Path, PathBuf};

    let path_part = input.strip_prefix("file://").unwrap_or(input);
    if path_part.is_empty() {
        return Err("file:// URI has no path".to_string());
    }

    let raw = Path::new(path_part);
    let absolute = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        let cwd = std::env::current_dir().map_err(|e| format!("resolve current directory: {e}"))?;
        cwd.join(raw)
    };

    // Lexical normalization: fold `.` and resolve `..` without touching the
    // filesystem, so canonicalization never depends on the target existing.
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }

    let rendered = normalized
        .to_str()
        .ok_or_else(|| "resolved path is not valid UTF-8".to_string())?;
    Ok(format!("file://{rendered}"))
}

/// Open `url` in the platform default browser. Best-effort: a spawn
/// failure (no `xdg-open`, headless box) is returned so the caller can
/// fall back to printing the URL.
fn open_in_browser(url: &str) -> Result<(), String> {
    let (cmd, args): (&str, Vec<&str>) = if cfg!(target_os = "macos") {
        ("open", vec![url])
    } else if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start", "", url])
    } else {
        ("xdg-open", vec![url])
    };
    std::process::Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// What the `red ui` command fronts, resolved from the target URI.
enum UiBackend {
    /// `file://` / bare path — the embedded engine opened in-process.
    /// Boxed: `RedDBServer` is large and would otherwise make this enum's
    /// largest variant dominate its size (clippy `large_enum_variant`).
    Embedded(Box<reddb::server::RedDBServer>),
    /// `red://` / `reds://` — a remote RedWire-over-TCP/TLS endpoint.
    Remote(reddb::server::ui_bridge::RemoteRedwireTarget),
    /// `red+wss://` / `red+ws://` — browser connects directly; no relay.
    Direct { ws_url: String },
}

fn ui_handoff_wait_duration() -> Duration {
    env_string("RED_UI_HANDOFF_WAIT_MS")
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_secs(60))
}

async fn wait_for_ui_handoff(server: &reddb::server::ui_auth::HandoffServer) {
    let _ = tokio::time::timeout(ui_handoff_wait_duration(), async {
        while !server.is_consumed() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
}

/// `red ui <uri> [--server] [--desktop] [--ui-dir DIR] [--port N]
/// [--tls-ca PEM] [--token TOKEN] [--no-browser]` — the spine of the red-ui integration
/// (issues #1042 / #1044 / #1046, PRD #1041, ADR 0051).
///
/// By default it prefers the installed desktop app: when the `redui://`
/// handler is registered it hands off via `xdg-open redui://?connect=<uri>`
/// (the URI canonicalized, carrying the target only — never a credential)
/// and exits. With no handler registered it falls back to the served browser
/// path and nudges the user to install the desktop app. `--server` forces the
/// browser path; `--desktop` forces the desktop deep link.
///
/// On the browser path: for a `file://` target it opens the embedded engine
/// from the file; for a `red://` / `reds://` target it fronts a remote
/// RedWire-over-TCP/TLS instance (e.g. a container). In both cases it stands
/// up a loopback RedWire-over-WS bridge that serves the UI bundle and the WS
/// endpoint the served page connects to, opens the browser at it, and tears
/// the bridge down cleanly on Ctrl-C.
fn run_ui_command(flags: &HashMap<String, FlagValue>, remaining: &[String]) {
    let json_mode = wants_json(flags);
    let token = flag_string(flags, "token")
        .or_else(|| env_string("RED_UI_TOKEN"))
        .filter(|value| !value.is_empty());

    let uri = match remaining.first() {
        Some(value) => value.clone(),
        None => {
            let msg = "red ui requires a <uri> positional (e.g. file://./data.rdb)";
            if json_mode {
                json_error("ui", msg);
            }
            eprintln!("error: {msg}");
            eprintln!("Run 'red ui --help' for usage.");
            std::process::exit(1);
        }
    };

    // Deep-link dispatch (issue #1046, ADR 0051): the default `red ui <uri>`
    // (no `--server`) prefers the installed desktop app via the `redui://`
    // scheme and only falls back to the served browser bridge when no handler
    // is registered. `--server` forces the browser path; `--desktop` forces
    // the desktop path. The decision + canonical deep-link string live behind
    // a testable seam in `reddb::server::ui_deeplink`.
    let mode = reddb::server::ui_deeplink::DispatchMode::from_flags(
        flag_bool(flags, "server"),
        flag_bool(flags, "desktop"),
    );
    if mode != reddb::server::ui_deeplink::DispatchMode::Server {
        let cwd = std::env::current_dir().unwrap_or_else(|err| {
            let msg = format!("resolve current directory: {err}");
            if json_mode {
                json_error("ui", &msg);
            }
            eprintln!("error: {msg}");
            std::process::exit(1);
        });
        let canonical = reddb::server::ui_deeplink::canonicalize_target_uri(&uri, &cwd)
            .unwrap_or_else(|err| {
                if json_mode {
                    json_error("ui", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
        let env = reddb::server::ui_deeplink::OsDeepLinkEnv;
        if let Some(token) = token.clone() {
            if reddb::server::ui_deeplink::DeepLinkEnv::handler_registered(&env) {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("tokio runtime");
                let handoff = rt.block_on(async {
                    let server = reddb::server::ui_auth::spawn_handoff_server(token)
                        .await
                        .map_err(|err| format!("start UI credential handoff: {err}"))?;
                    let handoff_url = server.handoff_url();
                    let deep_link = reddb::server::ui_deeplink::build_deep_link_with_handoff(
                        &canonical,
                        &handoff_url,
                    );
                    if let Err(err) =
                        reddb::server::ui_deeplink::DeepLinkEnv::open_url(&env, &deep_link)
                    {
                        server.shutdown().await;
                        return Err(err);
                    }
                    wait_for_ui_handoff(&server).await;
                    server.shutdown().await;
                    Ok::<_, String>(deep_link)
                });
                let deep_link = handoff.unwrap_or_else(|err| {
                    let msg = format!("deep-link dispatch: {err}");
                    if json_mode {
                        json_error("ui", &msg);
                    }
                    eprintln!("error: {msg}");
                    std::process::exit(1);
                });
                if json_mode {
                    json_ok(
                        "ui",
                        &format!(
                            "{{\"dispatch\":\"desktop\",\"auth\":\"handoff\",\"deep_link\":\"{}\",\"target\":\"{}\"}}",
                            json_escape(&deep_link),
                            json_escape(&canonical),
                        ),
                    );
                } else {
                    println!("red ui: handing off to the desktop app");
                    println!("  {deep_link}");
                }
                return;
            }
        }
        match reddb::server::ui_deeplink::dispatch(mode, &canonical, &env) {
            Ok(reddb::server::ui_deeplink::DispatchOutcome::HandedOff { deep_link }) => {
                if json_mode {
                    json_ok(
                        "ui",
                        &format!(
                            "{{\"dispatch\":\"desktop\",\"deep_link\":\"{}\",\"target\":\"{}\"}}",
                            json_escape(&deep_link),
                            json_escape(&canonical),
                        ),
                    );
                } else {
                    println!("red ui: handing off to the desktop app");
                    println!("  {deep_link}");
                }
                return;
            }
            Ok(reddb::server::ui_deeplink::DispatchOutcome::DesktopNotInstalled) => {
                let msg = "no redui:// handler registered; the desktop app is not installed. \
                           Install it from https://reddb.io/download, or run \
                           `red ui --server <uri>` to use the browser.";
                if json_mode {
                    json_error("ui", msg);
                }
                eprintln!("error: {msg}");
                std::process::exit(1);
            }
            Ok(reddb::server::ui_deeplink::DispatchOutcome::ServeBrowser { upsell }) => {
                if upsell && !json_mode {
                    // Auto fallback: nudge the native shell, then serve.
                    eprintln!(
                        "note: desktop app not installed — serving the browser UI. \
                         Install it from https://reddb.io/download for a faster native shell."
                    );
                }
                // Fall through to the served browser-bridge path below.
            }
            Err(err) => {
                let msg = format!("deep-link dispatch: {err}");
                if json_mode {
                    json_error("ui", &msg);
                }
                eprintln!("error: {msg}");
                std::process::exit(1);
            }
        }
    }

    // Classify the target: `file://` / bare path → embedded engine;
    // `red://` / `reds://` → remote RedWire endpoint fronted by the
    // bridge (issue #1044, ADR 0047 / 0049). Either way the served UI
    // only ever talks to the loopback WS endpoint.
    let target = reddb::server::ui_bridge::classify_ui_target(&uri).unwrap_or_else(|err| {
        if json_mode {
            json_error("ui", &err);
        }
        eprintln!("error: {err}");
        std::process::exit(1);
    });

    // `target_label` is what we report as the bridged target; `backend`
    // is what the bridge fronts.
    let (target_label, backend) = match target {
        reddb::server::ui_bridge::UiTarget::File => {
            let file_uri = canonicalize_file_uri(&uri).unwrap_or_else(|err| {
                if json_mode {
                    json_error("ui", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let db_path = file_uri
                .strip_prefix("file://")
                .unwrap_or(&file_uri)
                .to_string();
            let runtime =
                reddb::RedDBRuntime::with_options(reddb::api::RedDBOptions::persistent(&db_path))
                    .unwrap_or_else(|err| {
                        let msg = format!("open {db_path}: {err}");
                        if json_mode {
                            json_error("ui", &msg);
                        }
                        eprintln!("error: {msg}");
                        std::process::exit(1);
                    });
            let server = reddb::server::RedDBServer::new(runtime);
            (file_uri, UiBackend::Embedded(Box::new(server)))
        }
        reddb::server::ui_bridge::UiTarget::Remote(spec) => {
            // Optional `--tls-ca <pem>` is trusted on top of the webpki
            // system roots for a self-signed / private-CA `reds://` target.
            let ca_pem = match flag_string(flags, "tls-ca").filter(|v| !v.is_empty()) {
                Some(path) => match std::fs::read(&path) {
                    Ok(bytes) => Some(bytes),
                    Err(err) => {
                        let msg = format!("read --tls-ca {path}: {err}");
                        if json_mode {
                            json_error("ui", &msg);
                        }
                        eprintln!("error: {msg}");
                        std::process::exit(1);
                    }
                },
                None => None,
            };
            let target = reddb::server::ui_bridge::RemoteRedwireTarget {
                host: spec.host,
                port: spec.port,
                tls: spec.tls,
                ca_pem,
            };
            (uri.clone(), UiBackend::Remote(target))
        }
        reddb::server::ui_bridge::UiTarget::Direct { ws_url } => {
            if token.is_some() {
                let msg = "red ui --token cannot be injected for red+ws:// direct targets; \
                           use red:// or reds:// so the loopback bridge can hold the token";
                if json_mode {
                    json_error("ui", msg);
                }
                eprintln!("error: {msg}");
                std::process::exit(1);
            }
            // ADR 0047: browser-reachable WS target — no loopback relay.
            // Serve the UI bundle locally and let the browser connect
            // directly to ws_url (already a wss:// or ws:// URL).
            (uri.clone(), UiBackend::Direct { ws_url })
        }
    };

    let ui_dir = if let Some(explicit) = flag_string(flags, "ui-dir").filter(|v| !v.is_empty()) {
        // Explicit --ui-dir: use as-is (no download).
        Some(PathBuf::from(explicit))
    } else {
        // No --ui-dir: resolve the pinned bundle from the local cache,
        // downloading it on first use (ADR 0050 / issue #1043).
        let cache_root = reddb::server::ui_bundle_resolver::reddb_user_cache_root()
            .unwrap_or_else(|_| std::env::temp_dir().join("reddb"));
        match reddb::server::ui_bundle_resolver::resolve_ui_bundle(
            &cache_root,
            &reddb::server::ui_bundle_resolver::HttpFetcher,
        ) {
            Ok(bundle_dir) => Some(bundle_dir),
            Err(err) => {
                let msg = format!("fetch red-ui bundle: {err}");
                if json_mode {
                    json_error("ui", &msg);
                }
                eprintln!("error: {msg}");
                std::process::exit(1);
            }
        }
    };
    let port = flag_string(flags, "port")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(0);
    let no_browser = flag_bool(flags, "no-browser") || env_truthy("RED_UI_NO_BROWSER");

    let auth_mode = reddb::server::ui_auth::UiAuthMode::resolve(token.is_some(), false);
    let config = reddb::server::ui_bridge::UiBridgeConfig {
        ui_dir,
        port,
        injected_token: token,
        auth_mode,
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async move {
        let spawn_result = match backend {
            UiBackend::Embedded(server) => {
                reddb::server::ui_bridge::spawn_ui_bridge(*server, config).await
            }
            UiBackend::Remote(remote) => {
                reddb::server::ui_bridge::spawn_ui_bridge_remote(remote, config).await
            }
            UiBackend::Direct { ws_url } => {
                reddb::server::ui_bridge::spawn_direct_ui_server(ws_url, config).await
            }
        };
        let bridge = match spawn_result {
            Ok(bridge) => bridge,
            Err(err) => {
                let msg = format!("start ui bridge: {err}");
                if json_mode {
                    json_error("ui", &msg);
                }
                eprintln!("error: {msg}");
                std::process::exit(1);
            }
        };

        let ui_url = bridge.ui_url();
        let ws_url = bridge.ws_url();
        if json_mode {
            json_ok(
                "ui",
                &format!(
                    "{{\"target\":\"{}\",\"ui_url\":\"{}\",\"ws_url\":\"{}\"}}",
                    json_escape(&target_label),
                    json_escape(&ui_url),
                    json_escape(&ws_url),
                ),
            );
        } else {
            println!("red ui: serving {target_label}");
            println!("  UI:      {ui_url}");
            println!("  RedWire: {ws_url}");
            println!("Press Ctrl-C to stop.");
        }

        if !no_browser {
            if let Err(err) = open_in_browser(&ui_url) {
                eprintln!("note: could not open browser ({err}); open {ui_url} manually");
            }
        }

        let _ = tokio::signal::ctrl_c().await;
        if !json_mode {
            println!("\nred ui: shutting down…");
        }
        bridge.shutdown().await;
    });
}

fn run_migrate_from_redis_command(flags: &HashMap<String, FlagValue>) -> i32 {
    let json_mode = wants_json(flags);
    let dry_run = flag_bool(flags, "dry-run");
    let phase = flag_string(flags, "phase")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "dry-run".to_string());
    let namespace = flag_string(flags, "namespace")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "redis-migration".to_string());

    if phase == "dual-write" {
        let msg = "red migrate-from-redis does not own the dual-write shadow phase; use the documented application-owned helper pattern in docs/guides/migrate-redis-to-blob-cache.md";
        if json_mode {
            json_error("migrate-from-redis", msg);
        }
        eprintln!("migrate-from-redis: {msg}");
        return 2;
    }

    if phase != "dry-run" {
        let msg =
            format!("unsupported migration phase '{phase}'; supported phases: dry-run, dual-write");
        if json_mode {
            json_error("migrate-from-redis", &msg);
        }
        eprintln!("migrate-from-redis: {msg}");
        return 2;
    }

    if !dry_run {
        let msg =
            "only --dry-run is supported; dual-write must stay in the application-owned helper";
        if json_mode {
            json_error("migrate-from-redis", msg);
        }
        eprintln!("migrate-from-redis: {msg}");
        return 2;
    }

    let redis_url = match flag_string(flags, "redis-url").filter(|value| !value.is_empty()) {
        Some(value) => value,
        None => {
            let msg = "--redis-url is required for migrate-from-redis --dry-run";
            if json_mode {
                json_error("migrate-from-redis", msg);
            }
            eprintln!("migrate-from-redis: {msg}");
            return 2;
        }
    };

    if let Err(err) = validate_redis_tcp_connectivity(&redis_url) {
        let msg = format!("Redis connectivity check failed: {err}");
        if json_mode {
            json_error("migrate-from-redis", &msg);
        }
        eprintln!("migrate-from-redis: {msg}");
        return 1;
    }

    if let Err(err) = open_local_runtime(flags) {
        let msg = format!("RedDB connectivity check failed: {err}");
        if json_mode {
            json_error("migrate-from-redis", &msg);
        }
        eprintln!("migrate-from-redis: {msg}");
        return 1;
    }

    let data = format!(
        "{{\"mode\":\"dry-run\",\"redis_reachable\":true,\"reddb_reachable\":true,\"namespace\":\"{}\",\"entries_scanned\":0,\"entries_written\":0,\"mismatch_count\":0}}",
        json_escape(&namespace)
    );
    if json_mode {
        json_ok("migrate-from-redis", &data);
    } else {
        println!("migrate-from-redis dry-run ok");
        println!("redis_reachable: true");
        println!("reddb_reachable: true");
        println!("namespace: {}", namespace);
        println!("entries_scanned: 0");
        println!("entries_written: 0");
        println!("mismatch_count: 0");
    }
    0
}

fn run_salvage_command(flags: &HashMap<String, FlagValue>) -> i32 {
    let json_mode = wants_json(flags);
    let Some(source) = flag_string(flags, "source").filter(|value| !value.is_empty()) else {
        let msg = "--source is required for salvage";
        if json_mode {
            json_error("salvage", msg);
        }
        eprintln!("salvage: {msg}");
        return 2;
    };
    let Some(destination) = flag_string(flags, "destination").filter(|value| !value.is_empty())
    else {
        let msg = "--destination is required for salvage";
        if json_mode {
            json_error("salvage", msg);
        }
        eprintln!("salvage: {msg}");
        return 2;
    };

    match reddb_file::salvage_embedded_store(&source, &destination) {
        Ok(report) => {
            if json_mode {
                match report.machine_json() {
                    Ok(data) => json_ok("salvage", &data),
                    Err(err) => {
                        json_error("salvage", &err.to_string());
                    }
                }
            } else {
                println!("{}", report.human_summary());
            }
            0
        }
        Err(err) => {
            let msg = err.to_string();
            if json_mode {
                json_error("salvage", &msg);
            }
            eprintln!("salvage: {msg}");
            1
        }
    }
}

fn run_migrate_pager_zone_command(flags: &HashMap<String, FlagValue>) -> i32 {
    let json_mode = wants_json(flags);
    let Some(path) = flag_string(flags, "path").filter(|value| !value.is_empty()) else {
        let msg = "--path is required for migrate-pager-zone";
        if json_mode {
            json_error("migrate-pager-zone", msg);
        }
        eprintln!("migrate-pager-zone: {msg}");
        return 2;
    };

    match reddb::pager_zone_migration::migrate_to_zoned(std::path::Path::new(&path)) {
        Ok(report) => {
            if json_mode {
                let data_path = json_escape(&report.data_path.to_string_lossy());
                let backup_path = json_escape(&report.backup_path.to_string_lossy());
                json_ok(
                    "migrate-pager-zone",
                    &format!(
                        "{{\"data_path\":\"{data_path}\",\"backup_path\":\"{backup_path}\",\"removed_sidecars\":{},\"header_recovered_from_shadow\":{},\"manifest_recovered_from_shadow\":{}}}",
                        report.removed_sidecars.len(),
                        report.header_recovered_from_shadow,
                        report.manifest_recovered_from_shadow,
                    ),
                );
            } else {
                println!(
                    "migrated {} to the zoned .rdb format; rollback copy retained at {}",
                    report.data_path.display(),
                    report.backup_path.display()
                );
            }
            0
        }
        Err(err) => {
            let msg = err.to_string();
            if json_mode {
                json_error("migrate-pager-zone", &msg);
            }
            eprintln!("migrate-pager-zone: {msg}");
            1
        }
    }
}

fn validate_redis_tcp_connectivity(redis_url: &str) -> Result<(), String> {
    let addr = redis_socket_addr(redis_url)?;
    let mut addrs = addr
        .to_socket_addrs()
        .map_err(|err| format!("resolve {addr}: {err}"))?;
    let Some(sockaddr) = addrs.next() else {
        return Err(format!("resolve {addr}: no addresses"));
    };
    std::net::TcpStream::connect_timeout(&sockaddr, Duration::from_secs(1))
        .map(|_| ())
        .map_err(|err| format!("connect {addr}: {err}"))
}

fn redis_socket_addr(redis_url: &str) -> Result<String, String> {
    let without_scheme = if let Some(rest) = redis_url.strip_prefix("redis://") {
        rest
    } else if redis_url.contains("://") {
        return Err("only redis:// URLs are supported for dry-run validation".to_string());
    } else {
        redis_url
    };
    let authority = without_scheme.split('/').next().unwrap_or_default();
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    if host_port.is_empty() {
        return Err("missing Redis host".to_string());
    }
    if !host_port
        .rsplit(':')
        .next()
        .is_some_and(|port| !port.is_empty() && port.as_bytes().iter().all(|b| b.is_ascii_digit()))
    {
        return Err("Redis URL must include host:port".to_string());
    }
    Ok(host_port.to_string())
}

// ---------------------------------------------------------------------------
// VCS command implementation
// ---------------------------------------------------------------------------

fn run_vcs_command(flags: &HashMap<String, FlagValue>, remaining: &[String]) {
    let json_mode = wants_json(flags);
    let subcommand = remaining.first().map(|s| s.as_str()).unwrap_or("help");
    let args: Vec<&str> = remaining.iter().skip(1).map(|s| s.as_str()).collect();

    let rt = match open_local_runtime(flags) {
        Ok(rt) => rt,
        Err(err) => {
            if json_mode {
                json_error("vcs", &err);
            }
            eprintln!("vcs error: {err}");
            std::process::exit(1);
        }
    };

    let connection_id = flag_string(flags, "connection")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1);
    let author = reddb::application::Author {
        name: flag_string(flags, "author").unwrap_or_else(|| "reddb".to_string()),
        email: flag_string(flags, "email").unwrap_or_else(|| "reddb@localhost".to_string()),
    };

    let vcs = reddb::application::VcsUseCases::new(&rt);

    let outcome: Result<String, String> = match subcommand {
        "commit" => {
            let message = flag_string(flags, "message")
                .or_else(|| args.first().map(|s| s.to_string()))
                .unwrap_or_else(|| "no message".to_string());
            vcs.commit(reddb::application::CreateCommitInput {
                connection_id,
                message,
                author,
                committer: None,
                amend: false,
                allow_empty: true,
            })
            .map(|c| {
                if json_mode {
                    format!(
                        "{{\"hash\":\"{}\",\"height\":{},\"parents\":{}}}",
                        json_escape(&c.hash),
                        c.height,
                        format_args!("[{}]", c.parents.iter().map(|p| format!("\"{}\"", json_escape(p))).collect::<Vec<_>>().join(","))
                    )
                } else {
                    format!("commit {}\nHeight {}\nMessage: {}\n", c.hash, c.height, c.message)
                }
            })
            .map_err(|e| e.to_string())
        }
        "branch" => match args.first() {
            None => Err("usage: red vcs branch <name> [--from ref]".to_string()),
            Some(name) => vcs
                .branch_create(reddb::application::CreateBranchInput {
                    name: name.to_string(),
                    from: flag_string(flags, "from"),
                    connection_id,
                })
            .map(|r| {
                if json_mode {
                    format!(
                        "{{\"name\":\"{}\",\"target\":\"{}\"}}",
                        json_escape(&r.name),
                        json_escape(&r.target)
                    )
                } else {
                    format!("branch {} -> {}\n", r.name, r.target)
                }
            })
            .map_err(|e| e.to_string()),
        },
        "branches" => {
            vcs.branch_list()
                .map(|refs| {
                    if json_mode {
                        let items: Vec<String> = refs
                            .iter()
                            .map(|r| format!(
                                "{{\"name\":\"{}\",\"target\":\"{}\"}}",
                                json_escape(&r.name),
                                json_escape(&r.target)
                            ))
                            .collect();
                        format!("[{}]", items.join(","))
                    } else {
                        let mut out = String::new();
                        for r in refs {
                            out.push_str(&format!("{}\t{}\n", r.name, r.target));
                        }
                        out
                    }
                })
                .map_err(|e| e.to_string())
        }
        "tag" => match args.first() {
            None => Err("usage: red vcs tag <name> [target]".to_string()),
            Some(name) => {
                let target = args
                    .get(1)
                    .map(|s| s.to_string())
                    .or_else(|| flag_string(flags, "from"))
                    .unwrap_or_else(|| "main".to_string());
                vcs.tag(reddb::application::CreateTagInput {
                    name: name.to_string(),
                    target,
                    annotation: None,
                })
            .map(|r| {
                if json_mode {
                    format!(
                        "{{\"name\":\"{}\",\"target\":\"{}\"}}",
                        json_escape(&r.name),
                        json_escape(&r.target)
                    )
                } else {
                    format!("tag {} -> {}\n", r.name, r.target)
                }
            })
            .map_err(|e| e.to_string())
            }
        },
        "tags" => {
            vcs.tag_list()
                .map(|refs| {
                    if json_mode {
                        let items: Vec<String> = refs
                            .iter()
                            .map(|r| format!(
                                "{{\"name\":\"{}\",\"target\":\"{}\"}}",
                                json_escape(&r.name),
                                json_escape(&r.target)
                            ))
                            .collect();
                        format!("[{}]", items.join(","))
                    } else {
                        let mut out = String::new();
                        for r in refs {
                            out.push_str(&format!("{}\t{}\n", r.name, r.target));
                        }
                        out
                    }
                })
                .map_err(|e| e.to_string())
        }
        "checkout" => match args.first() {
            None => Err("usage: red vcs checkout <branch|tag|commit>".to_string()),
            Some(target) => {
                let target = target.to_string();
                let kind = if target.len() == 64
                    && target.chars().all(|c| c.is_ascii_hexdigit())
                {
                    reddb::application::CheckoutTarget::Commit(target.clone())
                } else if target.starts_with("refs/tags/") {
                    reddb::application::CheckoutTarget::Tag(target.clone())
                } else {
                    reddb::application::CheckoutTarget::Branch(target.clone())
                };
                vcs.checkout(reddb::application::CheckoutInput {
                    connection_id,
                    target: kind,
                    force: false,
                })
            .map(|r| {
                if json_mode {
                    format!("{{\"ref\":\"{}\"}}", json_escape(&r.name))
                } else {
                    format!("switched to {}\n", r.name)
                }
            })
            .map_err(|e| e.to_string())
            }
        },
        "merge" => {
            let from_opt = args
                .first()
                .map(|s| s.to_string())
                .or_else(|| flag_string(flags, "from"));
            let Some(from) = from_opt else {
                return emit_vcs_result(
                    &rt,
                    "merge",
                    json_mode,
                    Err("usage: red vcs merge <branch>".to_string()),
                );
            };
            let strategy = if flag_bool(flags, "ff-only") {
                reddb::application::MergeStrategy::FastForwardOnly
            } else if flag_bool(flags, "no-ff") {
                reddb::application::MergeStrategy::NoFastForward
            } else {
                reddb::application::MergeStrategy::Auto
            };
            vcs.merge(reddb::application::MergeInput {
                connection_id,
                from,
                opts: reddb::application::MergeOpts {
                    strategy,
                    message: flag_string(flags, "message"),
                    abort_on_conflict: false,
                },
                author,
            })
            .map(|outcome| {
                if json_mode {
                    format!(
                        "{{\"fast_forward\":{},\"conflicts\":{},\"commit\":{}}}",
                        outcome.fast_forward,
                        outcome.conflicts.len(),
                        outcome
                            .merge_commit
                            .as_ref()
                            .map(|c| format!("\"{}\"", json_escape(&c.hash)))
                            .unwrap_or_else(|| "null".to_string())
                    )
                } else if outcome.fast_forward {
                    "fast-forward\n".to_string()
                } else {
                    format!(
                        "merged (non-ff)\ncommit {}\nmerge_state {}\n",
                        outcome.merge_commit.as_ref().map(|c| c.hash.as_str()).unwrap_or("?"),
                        outcome.merge_state_id.as_deref().unwrap_or("?")
                    )
                }
            })
            .map_err(|e| e.to_string())
        }
        "log" => {
            let limit = flag_string(flags, "limit")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(20);
            vcs.log(reddb::application::LogInput {
                connection_id,
                range: reddb::application::LogRange {
                    to: flag_string(flags, "to").or_else(|| flag_string(flags, "branch")),
                    from: flag_string(flags, "from"),
                    limit: Some(limit),
                    skip: None,
                    no_merges: false,
                },
            })
            .map(|commits| {
                if json_mode {
                    let items: Vec<String> = commits
                        .iter()
                        .map(|c| format!(
                            "{{\"hash\":\"{}\",\"height\":{},\"message\":\"{}\",\"author\":\"{}\"}}",
                            json_escape(&c.hash),
                            c.height,
                            json_escape(&c.message),
                            json_escape(&c.author.name)
                        ))
                        .collect();
                    format!("[{}]", items.join(","))
                } else {
                    let mut out = String::new();
                    for c in commits {
                        out.push_str(&format!(
                            "commit {}\nAuthor: {} <{}>\n\n    {}\n\n",
                            c.hash, c.author.name, c.author.email, c.message
                        ));
                    }
                    out
                }
            })
            .map_err(|e| e.to_string())
        }
        "status" => {
            vcs.status(reddb::application::StatusInput { connection_id })
                .map(|s| {
                    if json_mode {
                        format!(
                            "{{\"head_ref\":{},\"head_commit\":{},\"detached\":{}}}",
                            s.head_ref.as_deref().map(|r| format!("\"{}\"", json_escape(r))).unwrap_or_else(|| "null".to_string()),
                            s.head_commit.as_deref().map(|h| format!("\"{}\"", json_escape(h))).unwrap_or_else(|| "null".to_string()),
                            s.detached
                        )
                    } else {
                        format!(
                            "On branch {}\nHead commit {}\n",
                            s.head_ref.as_deref().unwrap_or("(detached)"),
                            s.head_commit.as_deref().unwrap_or("(none)")
                        )
                    }
                })
                .map_err(|e| e.to_string())
        }
        "lca" => {
            let (Some(a), Some(b)) = (args.first(), args.get(1)) else {
                return emit_vcs_result(
                    &rt,
                    "lca",
                    json_mode,
                    Err("usage: red vcs lca <a> <b>".to_string()),
                );
            };
            vcs.lca(a, b)
                .map(|hash| {
                    if json_mode {
                        format!(
                            "{{\"lca\":{}}}",
                            hash.as_ref().map(|h| format!("\"{}\"", json_escape(h))).unwrap_or_else(|| "null".to_string())
                        )
                    } else {
                        hash.map(|h| format!("{h}\n")).unwrap_or_else(|| "(no common ancestor)\n".to_string())
                    }
                })
                .map_err(|e| e.to_string())
        }
        "resolve" => {
            let Some(spec) = args.first() else {
                return emit_vcs_result(
                    &rt,
                    "resolve",
                    json_mode,
                    Err("usage: red vcs resolve <ref|hash|prefix>".to_string()),
                );
            };
            vcs.resolve_commitish(spec)
                .map(|hash| {
                    if json_mode {
                        format!("{{\"hash\":\"{}\"}}", json_escape(&hash))
                    } else {
                        format!("{hash}\n")
                    }
                })
                .map_err(|e| e.to_string())
        }
        "versioned" => {
            let action = args.first().copied().unwrap_or("list");
            match action {
                "list" => vcs
                    .list_versioned()
                    .map(|list| {
                        if json_mode {
                            let items: Vec<String> = list
                                .iter()
                                .map(|s| format!("\"{}\"", json_escape(s)))
                                .collect();
                            format!("[{}]", items.join(","))
                        } else if list.is_empty() {
                            "(no versioned collections)\n".to_string()
                        } else {
                            list.into_iter()
                                .map(|s| format!("{s}\n"))
                                .collect::<String>()
                        }
                    })
                    .map_err(|e| e.to_string()),
                "on" | "enable" | "add" => match args.get(1) {
                    None => Err("usage: red vcs versioned on <collection>".to_string()),
                    Some(coll) => vcs
                        .set_versioned(coll, true)
                        .map(|()| {
                            if json_mode {
                                format!(
                                    "{{\"collection\":\"{}\",\"versioned\":true}}",
                                    json_escape(coll)
                                )
                            } else {
                                format!("opted in: {coll}\n")
                            }
                        })
                        .map_err(|e| e.to_string()),
                },
                "off" | "disable" | "remove" => match args.get(1) {
                    None => Err("usage: red vcs versioned off <collection>".to_string()),
                    Some(coll) => vcs
                        .set_versioned(coll, false)
                        .map(|()| {
                            if json_mode {
                                format!(
                                    "{{\"collection\":\"{}\",\"versioned\":false}}",
                                    json_escape(coll)
                                )
                            } else {
                                format!("opted out: {coll}\n")
                            }
                        })
                        .map_err(|e| e.to_string()),
                },
                "check" => match args.get(1) {
                    None => Err("usage: red vcs versioned check <collection>".to_string()),
                    Some(coll) => vcs
                        .is_versioned(coll)
                        .map(|b| {
                            if json_mode {
                                format!(
                                    "{{\"collection\":\"{}\",\"versioned\":{}}}",
                                    json_escape(coll),
                                    b
                                )
                            } else if b {
                                format!("{coll}: versioned\n")
                            } else {
                                format!("{coll}: NOT versioned\n")
                            }
                        })
                        .map_err(|e| e.to_string()),
                },
                _ => Err(format!(
                    "usage: red vcs versioned [list|on|off|check] <collection>\n\
                     got: {action}"
                )),
            }
        }
        "reset" => {
            let Some(target) = args.first() else {
                return emit_vcs_result(
                    &rt,
                    "reset",
                    json_mode,
                    Err("usage: red vcs reset <ref|hash> [--mode soft|mixed|hard]".to_string()),
                );
            };
            let mode_str = flag_string(flags, "mode").unwrap_or_else(|| "mixed".to_string());
            let mode = match mode_str.as_str() {
                "soft" => reddb::application::ResetMode::Soft,
                "hard" => reddb::application::ResetMode::Hard,
                _ => reddb::application::ResetMode::Mixed,
            };
            vcs.reset(reddb::application::ResetInput {
                connection_id,
                target: target.to_string(),
                mode,
            })
            .map(|()| {
                if json_mode {
                    "{\"ok\":true}".to_string()
                } else {
                    format!("reset ({mode_str}) to {target}\n")
                }
            })
            .map_err(|e| e.to_string())
        }
        _ => Err(format!(
            "Unknown vcs subcommand `{subcommand}`\n\n\
             Usage: red vcs <commit|branch|branches|tag|tags|checkout|merge|reset|log|status|lca|resolve|versioned> [args] [flags]\n"
        )),
    };

    emit_vcs_result(&rt, subcommand, json_mode, outcome);
}

fn emit_vcs_result(
    rt: &reddb::RedDBRuntime,
    subcommand: &str,
    json_mode: bool,
    outcome: Result<String, String>,
) {
    checkpoint_local_runtime(rt);
    match outcome {
        Ok(text) => {
            if json_mode {
                json_ok(&format!("vcs.{subcommand}"), &text);
            } else {
                print!("{text}");
            }
        }
        Err(err) => {
            if json_mode {
                json_error(&format!("vcs.{subcommand}"), &err);
            }
            eprintln!("vcs {subcommand} error: {err}");
            std::process::exit(1);
        }
    }
}

/// Identify the command name from raw args (first non-flag argument).
fn identify_command(args: &[String]) -> Option<String> {
    for arg in args {
        if arg == "--" {
            break;
        }
        if !arg.starts_with('-') {
            return Some(arg.clone());
        }
    }
    None
}

/// Build the completion tree for runtime tab-completion.
#[allow(clippy::type_complexity)]
fn build_completion_tree() -> Vec<(String, Vec<(String, Vec<String>)>)> {
    vec![
        ("server".to_string(), vec![]),
        ("service".to_string(), vec![]),
        ("replica".to_string(), vec![]),
        ("query".to_string(), vec![]),
        ("insert".to_string(), vec![]),
        ("get".to_string(), vec![]),
        ("delete".to_string(), vec![]),
        ("tick".to_string(), vec![]),
        ("migrate-from-redis".to_string(), vec![]),
        ("migrate-pager-zone".to_string(), vec![]),
        ("salvage".to_string(), vec![]),
        ("health".to_string(), vec![]),
        (
            "admin".to_string(),
            vec![
                (
                    "collections".to_string(),
                    vec![
                        "list".to_string(),
                        "show".to_string(),
                        "stats".to_string(),
                        "drop".to_string(),
                        "truncate".to_string(),
                    ],
                ),
                ("indices".to_string(), vec!["list".to_string()]),
                ("policies".to_string(), vec!["list".to_string()]),
                ("query".to_string(), vec![]),
                ("cache".to_string(), vec![]),
            ],
        ),
        ("status".to_string(), vec![]),
        ("mcp".to_string(), vec![]),
        ("connect".to_string(), vec![]),
        ("version".to_string(), vec![]),
    ]
}

/// Extract the TCP port from a bind-address string (`0.0.0.0:5050`,
/// `[::]:5050`, `host:5050`). `None` for portless forms (unix sockets,
/// bare paths) — those can never collide on a TCP port.
fn bind_addr_port(addr: &str) -> Option<u16> {
    addr.rsplit(':').next().and_then(|p| p.parse::<u16>().ok())
}

fn build_server_config(
    flags: &HashMap<String, FlagValue>,
    forced_role: Option<&str>,
) -> Result<ServerCommandConfig, String> {
    let grpc_flag = flag_bool(flags, "grpc");
    let http_flag = flag_bool(flags, "http");
    let explicit_grpc_bind_from_flag =
        flag_string(flags, "grpc-bind").filter(|value| !value.is_empty());
    let explicit_grpc_bind_from_env = env_string("REDDB_GRPC_BIND_ADDR");
    let explicit_grpc_bind = explicit_grpc_bind_from_flag
        .clone()
        .or(explicit_grpc_bind_from_env.clone());
    let explicit_http_bind_from_flag =
        flag_string(flags, "http-bind").filter(|value| !value.is_empty());
    let explicit_http_bind_from_env = env_string("REDDB_HTTP_BIND_ADDR");
    let explicit_http_bind = explicit_http_bind_from_flag
        .clone()
        .or(explicit_http_bind_from_env.clone());
    let legacy_bind_from_flag = flag_string(flags, "bind").filter(|value| !value.is_empty());
    let legacy_bind_from_env = if explicit_grpc_bind.is_none() && explicit_http_bind.is_none() {
        env_string("REDDB_BIND_ADDR")
    } else {
        None
    };
    let legacy_bind = flag_string(flags, "bind")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            if explicit_grpc_bind.is_none() && explicit_http_bind.is_none() {
                env_string("REDDB_BIND_ADDR")
            } else {
                None
            }
        });
    let explicit_wire_bind_from_flag =
        flag_string(flags, "wire-bind").filter(|value| !value.is_empty());
    let explicit_wire_bind_from_env = env_string("REDDB_WIRE_BIND_ADDR");
    let mut wire_bind_addr = explicit_wire_bind_from_flag
        .clone()
        .or(explicit_wire_bind_from_env.clone());
    let wire_tls_bind_addr = flag_string(flags, "wire-tls-bind").filter(|v| !v.is_empty());
    // Plaintext/TLS wire-port collision (follow-up to #1588, found via
    // reddb-io/rio-lair#255): the release Docker image bakes
    // `REDDB_WIRE_BIND_ADDR=0.0.0.0:5050` as a default, so a container
    // started with `--wire-tls-bind [::]:5050` (and no `--wire-bind`)
    // would spin up an env-derived plaintext listener that wins the port
    // and non-fatally kills the TLS listener — TLS clients get a reset
    // while the server reports healthy. The explicit CLI TLS flag owns
    // the port: suppress the env-derived plaintext default when both
    // target the same port. Two explicit *flags* on one port stay a hard
    // error — that is an operator contradiction, not an image default.
    if let (Some(tls_addr), Some(wire_addr)) = (&wire_tls_bind_addr, &wire_bind_addr) {
        let same_port = match (bind_addr_port(tls_addr), bind_addr_port(wire_addr)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        };
        if same_port {
            if explicit_wire_bind_from_flag.is_some() {
                return Err(format!(
                    "--wire-bind {wire_addr} and --wire-tls-bind {tls_addr} target the same port; pick distinct ports"
                ));
            }
            eprintln!(
                "warning: REDDB_WIRE_BIND_ADDR={wire_addr} targets the same port as --wire-tls-bind {tls_addr}; \
                 suppressing the plaintext wire listener (the TLS listener owns the port)"
            );
            wire_bind_addr = None;
        }
    }
    let pg_bind_addr = flag_string(flags, "pg-bind").filter(|v| !v.is_empty());
    let router_bind_addr = if explicit_grpc_bind.is_none()
        && explicit_http_bind.is_none()
        && wire_bind_addr.is_none()
        && wire_tls_bind_addr.is_none()
        && pg_bind_addr.is_none()
        && !grpc_flag
        && !http_flag
    {
        Some(
            legacy_bind
                .clone()
                .unwrap_or_else(|| reddb::service_cli::DEFAULT_ROUTER_BIND_ADDR.to_string()),
        )
    } else {
        None
    };
    let should_resolve_grpc_http = grpc_flag
        || http_flag
        || explicit_grpc_bind.is_some()
        || explicit_http_bind.is_some()
        || legacy_bind.is_some();
    let (grpc_bind_addr, http_bind_addr) = if router_bind_addr.is_some() {
        (None, None)
    } else if should_resolve_grpc_http {
        resolve_server_binds(flags)?
    } else {
        (None, None)
    };
    let legacy_bind_explicit = legacy_bind_from_flag.is_some() || legacy_bind_from_env.is_some();
    let grpc_bind_explicit = explicit_grpc_bind_from_flag.is_some()
        || explicit_grpc_bind_from_env.is_some()
        || (legacy_bind_explicit && grpc_bind_addr.is_some() && http_bind_addr.is_none());
    let http_bind_explicit = explicit_http_bind_from_flag.is_some()
        || explicit_http_bind_from_env.is_some()
        || (legacy_bind_explicit && http_bind_addr.is_some() && grpc_bind_addr.is_none());
    let path = resolve_server_path(flags).map(PathBuf::from);
    let bootstrap = resolve_operational_bootstrap(flags, forced_role)?;
    let storage_profile = bootstrap.storage_profile;
    let role = bootstrap.process_role;
    let no_auth =
        flag_bool(flags, "no-auth") || flag_bool(flags, "dev") || env_truthy("REDDB_NO_AUTH");
    validate_auth_bootstrap_env_for_cluster_shape(bootstrap.topology, storage_profile, no_auth)?;

    let workers = flag_string(flags, "workers").and_then(|v| v.parse::<usize>().ok());

    let wire_tls_cert = flag_string(flags, "wire-tls-cert")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let wire_tls_key = flag_string(flags, "wire-tls-key")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);

    // Phase 6 logging: assemble the TelemetryConfig from --log-* flags,
    // falling back to a path-derived default when flags are absent.
    let telemetry = build_telemetry_config(flags, path.as_deref());

    // gRPC TLS knobs come exclusively via env (REDDB_GRPC_TLS_BIND /
    // REDDB_GRPC_TLS_CERT / REDDB_GRPC_TLS_KEY / REDDB_GRPC_TLS_CLIENT_CA,
    // each with the standard `_FILE` companion). The CLI surface stays
    // unchanged for now; flags can be added later without breaking the
    // env-driven path.
    let grpc_tls_bind_addr = std::env::var("REDDB_GRPC_TLS_BIND")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let grpc_tls_cert = std::env::var("REDDB_GRPC_TLS_CERT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from);
    let grpc_tls_key = std::env::var("REDDB_GRPC_TLS_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from);
    let grpc_tls_client_ca = std::env::var("REDDB_GRPC_TLS_CLIENT_CA")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from);

    // HTTP TLS knobs. CLI flags win; otherwise read REDDB_HTTP_TLS_*
    // (each with the standard `_FILE` companion expanded earlier in
    // boot via `expand_file_env`).
    let http_tls_bind_addr = flag_string(flags, "http-tls-bind")
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("REDDB_HTTP_TLS_BIND")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    let http_tls_cert = flag_string(flags, "http-tls-cert")
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("REDDB_HTTP_TLS_CERT")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .map(PathBuf::from);
    let http_tls_key = flag_string(flags, "http-tls-key")
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("REDDB_HTTP_TLS_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .map(PathBuf::from);
    let http_tls_client_ca = flag_string(flags, "http-tls-client-ca")
        .filter(|v| !v.is_empty())
        .or_else(|| {
            std::env::var("REDDB_HTTP_TLS_CLIENT_CA")
                .ok()
                .filter(|v| !v.trim().is_empty())
        })
        .map(PathBuf::from);

    let http_limits_cli = build_http_limits_cli_input(flags)?;
    let bootstrap = build_bootstrap_config(flags)?;

    Ok(ServerCommandConfig {
        path,
        router_bind_addr,
        router_bind_explicit: legacy_bind_explicit,
        grpc_bind_addr,
        grpc_bind_explicit,
        grpc_tls_bind_addr,
        grpc_tls_cert,
        grpc_tls_key,
        grpc_tls_client_ca,
        http_bind_addr,
        http_bind_explicit,
        http_tls_bind_addr,
        http_tls_cert,
        http_tls_key,
        http_tls_client_ca,
        wire_bind_addr,
        wire_bind_explicit: explicit_wire_bind_from_flag.is_some()
            || explicit_wire_bind_from_env.is_some(),
        wire_tls_bind_addr,
        wire_tls_cert,
        wire_tls_key,
        pg_bind_addr,
        create_if_missing: !flag_bool(flags, "no-create-if-missing"),
        read_only: flag_bool(flags, "read-only"),
        role,
        primary_addr: flag_string(flags, "primary-addr")
            .filter(|value| !value.is_empty())
            .or_else(|| env_string("REDDB_PRIMARY_ADDR")),
        storage_profile,
        auth: flag_bool(flags, "auth") || env_truthy("REDDB_AUTH"),
        require_auth: flag_bool(flags, "require-auth") || env_truthy("REDDB_REQUIRE_AUTH"),
        vault: flag_bool(flags, "vault") || env_truthy("REDDB_VAULT"),
        no_auth,
        workers,
        telemetry: Some(telemetry),
        http_limits_cli,
        // `red server --ui` (#1047, ADR 0051): serve the pinned red-ui
        // bundle on the HTTP surface. `--ui-dir <DIR>` overrides the
        // resolved/cached bundle with an explicit directory.
        ui: flag_bool(flags, "ui"),
        ui_dir: flag_string(flags, "ui-dir")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from),
        bootstrap,
    })
}

fn validate_auth_bootstrap_env_for_cluster_shape(
    topology: reddb::operational_bootstrap::OperationalTopology,
    storage_profile: reddb::storage::StorageProfileSelection,
    no_auth: bool,
) -> Result<(), String> {
    let cluster_shape = topology == reddb::operational_bootstrap::OperationalTopology::Cluster
        || storage_profile.deploy_profile == reddb::storage::DeployProfile::Cluster;
    if no_auth || !cluster_shape {
        return Ok(());
    }

    let bootstrap_vars = [
        "REDDB_PRESET",
        "REDDB_BOOTSTRAP_PRESET",
        "REDDB_BOOTSTRAP_MANIFEST",
        "REDDB_USERNAME",
        "REDDB_USERNAME_FILE",
        "REDDB_PASSWORD",
        "REDDB_PASSWORD_FILE",
    ];
    if let Some(var) = bootstrap_vars
        .iter()
        .find(|var| std::env::var(var).is_ok_and(|value| !value.trim().is_empty()))
    {
        return Err(format!(
            "{var} is not supported in cluster-shaped boots yet; cluster auth bootstrap needs a concrete writer/volume owner. Use --no-auth for anonymous cluster-shaped boot, or bootstrap auth after the cluster writer path exists."
        ));
    }

    Ok(())
}

fn build_bootstrap_config(flags: &HashMap<String, FlagValue>) -> Result<BootstrapConfig, String> {
    Ok(BootstrapConfig {
        preset: flag_string(flags, "bootstrap-preset")
            .filter(|value| !value.is_empty())
            .or_else(|| env_string("REDDB_BOOTSTRAP_PRESET"))
            .or_else(|| env_string("REDDB_PRESET")),
        manifest: flag_string(flags, "bootstrap-manifest")
            .filter(|value| !value.is_empty())
            .or_else(|| env_string("REDDB_BOOTSTRAP_MANIFEST"))
            .map(PathBuf::from),
        admin_username: flag_string(flags, "bootstrap-admin").filter(|value| !value.is_empty()),
        admin_password: flag_or_file_secret(
            flags,
            "bootstrap-admin-password",
            "bootstrap-admin-password-file",
        )?,
        cloud_head_admin: flag_string(flags, "cloud-head-admin").filter(|value| !value.is_empty()),
        cloud_head_admin_password: flag_or_file_secret(
            flags,
            "cloud-head-admin-password",
            "cloud-head-admin-password-file",
        )?,
        customer_admin: flag_string(flags, "customer-admin").filter(|value| !value.is_empty()),
        customer_admin_password: flag_or_file_secret(
            flags,
            "customer-admin-password",
            "customer-admin-password-file",
        )?,
        cert_out: flag_string(flags, "bootstrap-cert-out")
            .filter(|value| !value.is_empty())
            .or_else(|| env_string("REDDB_BOOTSTRAP_CERT_OUT"))
            .map(PathBuf::from),
    })
}

fn flag_or_file_secret(
    flags: &HashMap<String, FlagValue>,
    value_flag: &str,
    file_flag: &str,
) -> Result<Option<String>, String> {
    let inline = flag_string(flags, value_flag).filter(|value| !value.is_empty());
    let file = flag_string(flags, file_flag).filter(|value| !value.is_empty());
    match (inline, file) {
        (Some(_), Some(_)) => Err(format!(
            "--{value_flag} and --{file_flag} cannot both be set"
        )),
        (Some(value), None) => Ok(Some(value)),
        (None, Some(path)) => {
            let value = std::fs::read_to_string(&path)
                .map_err(|err| format!("read --{file_flag} {path}: {err}"))?;
            let value = value.trim_end_matches(['\n', '\r']).to_string();
            if value.is_empty() {
                Err(format!("--{file_flag} {path} is empty"))
            } else {
                Ok(Some(value))
            }
        }
        (None, None) => Ok(None),
    }
}

fn resolve_storage_profile(
    flags: &HashMap<String, FlagValue>,
    role: &str,
) -> Result<reddb::storage::StorageProfileSelection, String> {
    let forced_role = matches!(role, "standalone" | "primary" | "replica").then(|| role);
    Ok(resolve_operational_bootstrap_without_topology_env(flags, forced_role)?.storage_profile)
}

fn resolve_operational_bootstrap(
    flags: &HashMap<String, FlagValue>,
    forced_role: Option<&str>,
) -> Result<reddb::operational_bootstrap::OperationalBootstrapPlan, String> {
    let mut input = operational_bootstrap_input(flags, forced_role);
    input.topology = env_string("REDDB_TOPOLOGY");
    input.node_role = env_string("REDDB_NODE_ROLE");
    input.config_file_path = env_string("REDDB_CONFIG_FILE");
    reddb::operational_bootstrap::resolve_operational_bootstrap(input)
}

fn resolve_operational_bootstrap_without_topology_env(
    flags: &HashMap<String, FlagValue>,
    forced_role: Option<&str>,
) -> Result<reddb::operational_bootstrap::OperationalBootstrapPlan, String> {
    reddb::operational_bootstrap::resolve_operational_bootstrap(operational_bootstrap_input(
        flags,
        forced_role,
    ))
}

fn operational_bootstrap_input(
    flags: &HashMap<String, FlagValue>,
    forced_role: Option<&str>,
) -> reddb::operational_bootstrap::OperationalBootstrapInput {
    reddb::operational_bootstrap::OperationalBootstrapInput {
        forced_role: forced_role.map(str::to_string),
        role_flag: flag_string(flags, "role").filter(|value| !value.is_empty()),
        topology: None,
        node_role: None,
        storage_preset: flag_string(flags, "storage-preset")
            .filter(|value| !value.is_empty())
            .or_else(|| env_string("REDDB_STORAGE_PRESET")),
        storage_profile: flag_string(flags, "storage-profile")
            .filter(|value| !value.is_empty())
            .or_else(|| {
                env_string("REDDB_STORAGE_PROFILE").or_else(|| env_string("REDDB_DEPLOY_PROFILE"))
            }),
        storage_packaging: flag_string(flags, "storage-packaging")
            .filter(|value| !value.is_empty())
            .or_else(|| env_string("REDDB_STORAGE_PACKAGING")),
        replica_count: flag_string(flags, "replica-count")
            .filter(|value| !value.is_empty())
            .or_else(|| env_string("REDDB_REPLICA_COUNT")),
        managed_backup: flag_bool(flags, "managed-backup") || env_truthy("REDDB_MANAGED_BACKUP"),
        wal_retention: flag_bool(flags, "wal-retention") || env_truthy("REDDB_WAL_RETENTION"),
        config_file_path: None,
    }
}

/// Read the three slice-5 HTTP limiter knobs from CLI flags and env
/// vars, validate each, and pack them into a `HttpLimitsCliInput` for
/// the resolver. Validation failures abort boot with a clear message
/// (acceptance: "Invalid input fails fast with a clear error").
fn build_http_limits_cli_input(
    flags: &HashMap<String, FlagValue>,
) -> Result<reddb::server::HttpLimitsCliInput, String> {
    use reddb::server::http_limits::{
        validate_handler_timeout_ms, validate_max_handlers, validate_max_inflight_per_principal,
        validate_retry_after_secs,
    };

    fn parse_usize_validated<V>(
        source: &str,
        raw: Option<String>,
        validate: V,
    ) -> Result<Option<usize>, String>
    where
        V: Fn(usize) -> Result<usize, String>,
    {
        let Some(raw) = raw else {
            return Ok(None);
        };
        let parsed: usize = raw
            .trim()
            .parse()
            .map_err(|err| format!("{source}: invalid integer `{raw}`: {err}"))?;
        let validated = validate(parsed).map_err(|err| format!("{source}: {err}"))?;
        Ok(Some(validated))
    }

    fn parse_u64_validated<V>(
        source: &str,
        raw: Option<String>,
        validate: V,
    ) -> Result<Option<u64>, String>
    where
        V: Fn(u64) -> Result<u64, String>,
    {
        let Some(raw) = raw else {
            return Ok(None);
        };
        let parsed: u64 = raw
            .trim()
            .parse()
            .map_err(|err| format!("{source}: invalid integer `{raw}`: {err}"))?;
        let validated = validate(parsed).map_err(|err| format!("{source}: {err}"))?;
        Ok(Some(validated))
    }

    let max_handlers_flag = parse_usize_validated(
        "--http-max-handlers",
        flag_string(flags, "http-max-handlers").filter(|v| !v.is_empty()),
        validate_max_handlers,
    )?;
    let max_handlers_env = parse_usize_validated(
        "REDDB_HTTP_MAX_HANDLERS",
        env_string("REDDB_HTTP_MAX_HANDLERS"),
        validate_max_handlers,
    )?;

    let handler_timeout_ms_flag = parse_u64_validated(
        "--http-handler-timeout-ms",
        flag_string(flags, "http-handler-timeout-ms").filter(|v| !v.is_empty()),
        validate_handler_timeout_ms,
    )?;
    let handler_timeout_ms_env = parse_u64_validated(
        "REDDB_HTTP_HANDLER_TIMEOUT_MS",
        env_string("REDDB_HTTP_HANDLER_TIMEOUT_MS"),
        validate_handler_timeout_ms,
    )?;

    let retry_after_secs_flag = parse_u64_validated(
        "--http-retry-after-secs",
        flag_string(flags, "http-retry-after-secs").filter(|v| !v.is_empty()),
        validate_retry_after_secs,
    )?;
    let retry_after_secs_env = parse_u64_validated(
        "REDDB_HTTP_RETRY_AFTER_SECS",
        env_string("REDDB_HTTP_RETRY_AFTER_SECS"),
        validate_retry_after_secs,
    )?;

    let max_inflight_per_principal_flag = parse_usize_validated(
        "--http-max-inflight-per-principal",
        flag_string(flags, "http-max-inflight-per-principal").filter(|v| !v.is_empty()),
        validate_max_inflight_per_principal,
    )?;
    let max_inflight_per_principal_env = parse_usize_validated(
        "REDDB_HTTP_MAX_INFLIGHT_PER_PRINCIPAL",
        env_string("REDDB_HTTP_MAX_INFLIGHT_PER_PRINCIPAL"),
        validate_max_inflight_per_principal,
    )?;

    Ok(reddb::server::HttpLimitsCliInput {
        max_handlers_flag,
        max_handlers_env,
        handler_timeout_ms_flag,
        handler_timeout_ms_env,
        retry_after_secs_flag,
        retry_after_secs_env,
        max_inflight_per_principal_flag,
        max_inflight_per_principal_env,
    })
}

fn build_telemetry_config(
    flags: &HashMap<String, FlagValue>,
    db_path: Option<&std::path::Path>,
) -> reddb::telemetry::TelemetryConfig {
    let mut base = reddb::service_cli::default_telemetry_for_path(db_path);

    if let Some(dir) = flag_string(flags, "log-dir").filter(|v| !v.is_empty()) {
        base.log_dir = Some(PathBuf::from(dir));
        base.log_dir_explicit = true;
    }
    if flag_bool(flags, "no-log-file") {
        base.log_dir = None;
        base.log_file_disabled = true;
    }
    if let Some(level) = flag_string(flags, "log-level").filter(|v| !v.is_empty()) {
        base.level_filter = level;
        base.level_explicit = true;
    }
    if let Some(fmt) = flag_string(flags, "log-format").filter(|v| !v.is_empty()) {
        if let Some(parsed) = reddb::telemetry::LogFormat::parse(&fmt) {
            base.format = parsed;
            base.format_explicit = true;
        }
    }
    if let Some(prefix) = flag_string(flags, "log-file-prefix").filter(|v| !v.is_empty()) {
        base.file_prefix = prefix;
        base.file_prefix_explicit = true;
    }
    if let Some(keep) = flag_string(flags, "log-keep-days").and_then(|v| v.parse::<u16>().ok()) {
        base.rotation_keep_days = keep;
        base.rotation_keep_days_explicit = true;
    }

    base
}

fn build_systemd_service_config(
    flags: &HashMap<String, FlagValue>,
) -> Result<SystemdServiceConfig, String> {
    let grpc_flag = flag_bool(flags, "grpc");
    let http_flag = flag_bool(flags, "http");
    let legacy_bind = flag_string(flags, "bind").filter(|value| !value.is_empty());
    let explicit_grpc_bind = flag_string(flags, "grpc-bind").filter(|value| !value.is_empty());
    let explicit_http_bind = flag_string(flags, "http-bind").filter(|value| !value.is_empty());
    let router_bind_addr =
        if explicit_grpc_bind.is_none() && explicit_http_bind.is_none() && !grpc_flag && !http_flag
        {
            Some(
                legacy_bind
                    .clone()
                    .unwrap_or_else(|| reddb::service_cli::DEFAULT_ROUTER_BIND_ADDR.to_string()),
            )
        } else {
            None
        };
    let (grpc_bind_addr, http_bind_addr) = if router_bind_addr.is_some() {
        (None, None)
    } else {
        resolve_server_binds(flags)?
    };
    let binary_path = flag_string(flags, "binary")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/local/bin/red"));
    let data_path = flag_string(flags, "path")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/reddb/data.rdb"));

    Ok(SystemdServiceConfig {
        service_name: flag_string(flags, "service-name")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "reddb".to_string()),
        binary_path,
        run_user: flag_string(flags, "user")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "reddb".to_string()),
        run_group: flag_string(flags, "group")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "reddb".to_string()),
        data_path,
        router_bind_addr,
        grpc_bind_addr,
        http_bind_addr,
    })
}

fn resolve_server_binds(
    flags: &HashMap<String, FlagValue>,
) -> Result<(Option<String>, Option<String>), String> {
    let grpc = flag_bool(flags, "grpc");
    let http = flag_bool(flags, "http");
    let mut grpc_bind = flag_string(flags, "grpc-bind")
        .filter(|value| !value.is_empty())
        .or_else(|| env_string("REDDB_GRPC_BIND_ADDR"));
    let mut http_bind = flag_string(flags, "http-bind")
        .filter(|value| !value.is_empty())
        .or_else(|| env_string("REDDB_HTTP_BIND_ADDR"));
    let legacy_bind = flag_string(flags, "bind")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            if grpc_bind.is_none() && http_bind.is_none() {
                env_string("REDDB_BIND_ADDR")
            } else {
                None
            }
        });

    if legacy_bind.is_some() && (grpc_bind.is_some() || http_bind.is_some()) {
        return Err("use either --bind or the explicit --grpc-bind/--http-bind flags".to_string());
    }

    if let Some(bind_addr) = legacy_bind {
        match (grpc, http) {
            (true, true) => {
                return Err(
                    "--bind is ambiguous when both --grpc and --http are enabled; use --grpc-bind and --http-bind".to_string(),
                )
            }
            (false, true) => http_bind = Some(bind_addr),
            _ => grpc_bind = Some(bind_addr),
        }
    } else {
        if grpc {
            grpc_bind.get_or_insert_with(|| ServerTransport::Grpc.default_bind_addr().to_string());
        }
        if http {
            http_bind.get_or_insert_with(|| ServerTransport::Http.default_bind_addr().to_string());
        }
    }

    if grpc_bind.is_none() && http_bind.is_none() {
        grpc_bind = Some(ServerTransport::Grpc.default_bind_addr().to_string());
    }

    Ok((grpc_bind, http_bind))
}

fn select_transport(flags: &HashMap<String, FlagValue>) -> Result<ServerTransport, String> {
    let grpc = flag_bool(flags, "grpc");
    let http = flag_bool(flags, "http");

    match (grpc, http) {
        (true, true) => Err("use only one of --grpc or --http".to_string()),
        (false, true) => Ok(ServerTransport::Http),
        _ => Ok(ServerTransport::Grpc),
    }
}

fn flag_bool(flags: &HashMap<String, FlagValue>, name: &str) -> bool {
    flags
        .get(name)
        .map(|value| value.is_truthy())
        .unwrap_or(false)
}

fn flag_string(flags: &HashMap<String, FlagValue>, name: &str) -> Option<String> {
    flags.get(name).map(|value| value.as_str_value())
}

fn admin_token_from_flags_or_env(flags: &HashMap<String, FlagValue>) -> Option<String> {
    flag_string(flags, "token")
        .or_else(|| reddb::utils::env_with_file_fallback("RED_ADMIN_TOKEN"))
        .filter(|token| !token.trim().is_empty())
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn env_truthy(name: &str) -> bool {
    env_string(name)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn resolve_server_path(flags: &HashMap<String, FlagValue>) -> Option<String> {
    let env_path = env_string("REDDB_DATA_PATH");
    match flag_string(flags, "path").filter(|value| !value.is_empty()) {
        Some(path) if path == "./data/reddb.rdb" => env_path.or(Some(path)),
        Some(path) => Some(path),
        None => env_path,
    }
}

fn json_optional_string(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", json_escape(value)),
        None => "null".to_string(),
    }
}

fn server_command_json(command: &str, config: &ServerCommandConfig) -> String {
    format!(
        "{{\"ok\":true,\"command\":\"{}\",\"data\":{{\"router_bind\":{},\"grpc_bind\":{},\"http_bind\":{},\"wire_bind\":{}}}}}",
        json_escape(command),
        json_optional_string(config.router_bind_addr.as_deref()),
        json_optional_string(config.grpc_bind_addr.as_deref()),
        json_optional_string(config.http_bind_addr.as_deref()),
        json_optional_string(config.wire_bind_addr.as_deref()),
    )
}

fn build_tick_payload(operations: Option<&str>, dry_run: bool) -> JsonValue {
    let mut fields = Vec::new();
    if let Some(operations) = operations {
        let operations = operations
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(JsonValue::string)
            .collect::<Vec<_>>();
        if !operations.is_empty() {
            fields.push(("operations", JsonValue::array(operations)));
        }
    }
    fields.push(("dry_run", JsonValue::bool(dry_run)));
    JsonValue::object(fields)
}

// ---------------------------------------------------------------------------
// Inspect command implementation
// ---------------------------------------------------------------------------

/// `red inspect catalog --path <FILE> [--at <SEQ>]`
///
/// Loads the physical metadata for an on-disk database and prints the
/// catalog snapshot as JSON. With `--at <seq>` reads the journal
/// snapshot for that sequence; returns an explicit error when the
/// journal is unavailable. Substitutes the historical auto-written
/// `.meta.json` sidecar for tiers that no longer emit it.
fn run_inspect_command(flags: &HashMap<String, FlagValue>, remaining: &[String]) {
    let json_mode = wants_json(flags);
    let subcommand = remaining.first().map(|s| s.as_str()).unwrap_or("");

    if subcommand != "catalog" {
        let msg = "Usage: red inspect catalog --path <FILE> [--at <SEQ>]";
        if json_mode {
            json_error("inspect", msg);
        }
        eprintln!("{msg}");
        std::process::exit(1);
    }

    let path = match flag_string(flags, "path").filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => {
            let msg = "inspect catalog requires --path <FILE>";
            if json_mode {
                json_error("inspect.catalog", msg);
            }
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    };

    let data_path = PathBuf::from(&path);
    let at_seq = flag_string(flags, "at")
        .filter(|v| !v.is_empty())
        .map(|v| v.parse::<u64>())
        .transpose();
    let at_seq = match at_seq {
        Ok(opt) => opt,
        Err(err) => {
            let msg = format!("--at must be a non-negative integer: {err}");
            if json_mode {
                json_error("inspect.catalog", &msg);
            }
            eprintln!("error: {msg}");
            std::process::exit(1);
        }
    };

    let metadata = match at_seq {
        Some(seq) => {
            let journal_path =
                reddb::PhysicalMetadataFile::metadata_journal_path_for(&data_path, seq);
            if !journal_path.exists() {
                let msg = format!(
                    "catalog snapshot for seq={seq} not available (journal missing: {})",
                    journal_path.display()
                );
                if json_mode {
                    json_error("inspect.catalog", &msg);
                }
                eprintln!("error: {msg}");
                std::process::exit(1);
            }
            match reddb::PhysicalMetadataFile::load_from_binary_path(&journal_path) {
                Ok(meta) => meta,
                Err(err) => {
                    let msg = format!("failed to load journal {}: {err}", journal_path.display());
                    if json_mode {
                        json_error("inspect.catalog", &msg);
                    }
                    eprintln!("error: {msg}");
                    std::process::exit(1);
                }
            }
        }
        None => match reddb::PhysicalMetadataFile::load_for_data_path(&data_path) {
            Ok(meta) => meta,
            Err(err) => {
                let msg = format!("failed to load catalog for {path}: {err}");
                if json_mode {
                    json_error("inspect.catalog", &msg);
                }
                eprintln!("error: {msg}");
                std::process::exit(1);
            }
        },
    };

    let pretty = metadata.to_json_value().to_string_pretty();
    if json_mode {
        json_ok("inspect.catalog", &pretty);
    } else {
        println!("{pretty}");
    }
}

// ---------------------------------------------------------------------------
// Admin command implementation
// ---------------------------------------------------------------------------

fn operator_http_client(
    bind: &str,
    token: Option<&str>,
) -> Result<(tokio::runtime::Runtime, HttpClient), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("failed to build async runtime: {err}"))?;
    let base_url = if bind.contains("://") {
        bind.trim_end_matches('/').to_string()
    } else {
        format!("http://{}", bind.trim_end_matches('/'))
    };
    let mut options = HttpOptions::new(base_url);
    if let Some(token) = token {
        options = options.with_token(token);
    }
    let client = HttpClient::new(options).map_err(|err| err.to_string())?;
    Ok((runtime, client))
}

/// Operator HTTP handle that materialises on first request.
///
/// Usage errors, help text, and argument validation must all work with no
/// server reachable, so nothing is built at dispatch time; the runtime and
/// the driver handle appear when a command actually issues a request.
struct OperatorClient {
    bind: String,
    token: Option<String>,
    handle: OnceCell<(tokio::runtime::Runtime, HttpClient)>,
}

impl OperatorClient {
    fn new(bind: &str, token: Option<&str>) -> Self {
        Self {
            bind: bind.to_string(),
            token: token.map(ToString::to_string),
            handle: OnceCell::new(),
        }
    }

    /// Runtime + driver handle for `command`, built on first call.
    /// Terminates the process when the handle cannot be built at all.
    fn get(&self, command: &str, json_mode: bool) -> &(tokio::runtime::Runtime, HttpClient) {
        self.handle.get_or_init(|| {
            operator_http_client(&self.bind, self.token.as_deref()).unwrap_or_else(|err| {
                if json_mode {
                    json_error(command, &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            })
        })
    }
}

fn run_admin_command(flags: &HashMap<String, FlagValue>, remaining: &[String]) {
    let json_mode = wants_json(flags);
    let bind = flag_string(flags, "bind").unwrap_or_else(|| "127.0.0.1:5000".to_string());
    let token = admin_token_from_flags_or_env(flags);
    let subcommand = remaining.first().map(String::as_str).unwrap_or("help");
    let args: Vec<&str> = remaining.iter().skip(1).map(String::as_str).collect();
    let client = OperatorClient::new(&bind, token.as_deref());

    match subcommand {
        "cache" => run_admin_cache_command(&client, json_mode, &args),
        "collections" => run_admin_collections_command(flags, &client, json_mode, &args),
        "indices" => run_admin_indices_command(flags, &client, json_mode, &args),
        "policies" => run_admin_policies_command(flags, &client, json_mode, &args),
        "query" => run_admin_query_command(flags, &client, json_mode, &args),
        _ => {
            if json_mode {
                json_ok(
                    "admin",
                    "{\"subcommands\":[\"cache\",\"collections\",\"indices\",\"policies\",\"query\"],\"message\":\"use a subcommand, e.g. red admin collections list\"}",
                );
            } else {
                println!("Usage: red admin <subcommand>");
                println!();
                println!("Subcommands:");
                println!("  cache        Blob cache admin operations");
                println!("  collections  Collection catalog queries via red.collections/red.columns/red.stats");
                println!("  indices      Index catalog queries via red.indices");
                println!("  policies     Policy catalog queries via red.policies");
                println!("  query        Run a native SQL catalog query via /query");
                println!();
                println!("Flags:");
                println!("  --bind <addr>   Server HTTP address (default: 127.0.0.1:5000, env: REDDB_BIND_ADDR)");
                println!("  --token <tok>   Admin bearer token (env: RED_ADMIN_TOKEN)");
                println!("  --json          JSON output");
                println!("  --csv           CSV output for tabular commands");
                println!("  --limit <n>     Limit rows for list/stats/query commands");
                println!(
                    "  --no-color      Accepted for compatibility; admin output is never colored"
                );
            }
        }
    }
}

fn run_admin_collections_command(
    flags: &HashMap<String, FlagValue>,
    client: &OperatorClient,
    json_mode: bool,
    args: &[&str],
) {
    let subcommand = args.first().copied().unwrap_or("help");
    let sub_args = args.get(1..).unwrap_or_default();
    let format = admin_output_format(flags, json_mode);

    match subcommand {
        "list" => {
            let mut filters = Vec::new();
            if !flag_bool(flags, "include-internal") {
                filters.push("internal = false".to_string());
            }
            if let Some(model) = flag_string(flags, "type").filter(|value| !value.is_empty()) {
                filters.push(format!("model = '{}'", sql_string_literal(&model)));
            }
            let mut sql = "SELECT * FROM red.collections".to_string();
            if !filters.is_empty() {
                sql.push_str(" WHERE ");
                sql.push_str(&filters.join(" AND "));
            }
            push_limit(&mut sql, flags);
            emit_admin_query_result("admin.collections.list", client, &sql, format, json_mode);
        }
        "show" => {
            let Some(name) = sub_args.first().copied().filter(|name| !name.is_empty()) else {
                admin_usage_error(
                    "admin.collections.show",
                    "collection name is required",
                    "usage: red admin collections show <name>",
                    json_mode,
                );
            };
            emit_admin_collection_show(client, name, format, json_mode);
        }
        "stats" => {
            let mut sql = "SELECT * FROM red.stats".to_string();
            if let Some(name) = sub_args.first().copied().filter(|name| !name.is_empty()) {
                sql.push_str(" WHERE collection = '");
                sql.push_str(&sql_string_literal(name));
                sql.push('\'');
            }
            push_limit(&mut sql, flags);
            emit_admin_query_result("admin.collections.stats", client, &sql, format, json_mode);
        }
        "drop" | "truncate" => {
            let command = format!("admin.collections.{subcommand}");
            // `drop` confirms interactively, so only it advertises --yes.
            let usage = if subcommand == "drop" {
                "usage: red admin collections drop <name> [--if-exists] [--yes] [--json]"
            } else {
                "usage: red admin collections truncate <name> [--if-exists] [--json]"
            };
            let Some(name) = sub_args.first().copied().filter(|name| !name.is_empty()) else {
                admin_usage_error(&command, "collection name is required", usage, json_mode);
            };
            if subcommand == "drop" && !flag_bool(flags, "yes") && !json_mode {
                eprint!("Drop collection '{name}'? This is irreversible. Type 'yes' to confirm: ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer).unwrap_or_default();
                if answer.trim() != "yes" {
                    eprintln!("Aborted.");
                    std::process::exit(1);
                }
            }
            let if_exists = if flag_bool(flags, "if-exists") {
                " IF EXISTS"
            } else {
                ""
            };
            let sql = format!(
                "{} COLLECTION{if_exists} {name}",
                subcommand.to_ascii_uppercase()
            );
            admin_execute_ddl_command(
                &command,
                client,
                &sql,
                name,
                if subcommand == "drop" {
                    "dropped"
                } else {
                    "truncated"
                },
                json_mode,
            );
        }
        _ => {
            if json_mode {
                json_ok(
                    "admin.collections",
                    "{\"subcommands\":[\"list\",\"show\",\"stats\",\"drop\",\"truncate\"]}",
                );
            } else {
                println!("Usage: red admin collections <list|show|stats|drop|truncate> [args]");
                println!();
                println!("Subcommands:");
                println!("  list [--type table|queue|vector|document|timeseries|graph|kv] [--include-internal]");
                println!("  show <name>");
                println!("  stats [<name>]");
                println!("  drop <name> [--if-exists] [--yes]");
                println!("  truncate <name> [--if-exists]");
                println!();
                println!("Flags: --json --csv --limit <n> --no-color --bind <addr> --token <tok>");
            }
        }
    }
}

fn run_admin_indices_command(
    flags: &HashMap<String, FlagValue>,
    client: &OperatorClient,
    json_mode: bool,
    args: &[&str],
) {
    if args.first().copied().unwrap_or("help") != "list" {
        admin_catalog_usage("indices", json_mode);
        return;
    }
    let mut sql = "SELECT * FROM red.indices".to_string();
    if let Some(collection) = flag_string(flags, "collection").filter(|value| !value.is_empty()) {
        sql.push_str(" WHERE collection = '");
        sql.push_str(&sql_string_literal(&collection));
        sql.push('\'');
    }
    push_limit(&mut sql, flags);
    emit_admin_query_result(
        "admin.indices.list",
        client,
        &sql,
        admin_output_format(flags, json_mode),
        json_mode,
    );
}

fn run_admin_policies_command(
    flags: &HashMap<String, FlagValue>,
    client: &OperatorClient,
    json_mode: bool,
    args: &[&str],
) {
    if args.first().copied().unwrap_or("help") != "list" {
        admin_catalog_usage("policies", json_mode);
        return;
    }
    let mut sql = "SELECT * FROM red.policies".to_string();
    if let Some(collection) = flag_string(flags, "collection").filter(|value| !value.is_empty()) {
        sql.push_str(" WHERE collection = '");
        sql.push_str(&sql_string_literal(&collection));
        sql.push('\'');
    }
    push_limit(&mut sql, flags);
    emit_admin_query_result(
        "admin.policies.list",
        client,
        &sql,
        admin_output_format(flags, json_mode),
        json_mode,
    );
}

/// Usage block for the single-subcommand catalog groups (`indices`,
/// `policies`). `--json` gets the machine envelope, not the human text.
fn admin_catalog_usage(group: &str, json_mode: bool) {
    if json_mode {
        json_ok(&format!("admin.{group}"), "{\"subcommands\":[\"list\"]}");
        return;
    }
    println!("Usage: red admin {group} list [--collection <name>]");
    println!("Flags: --json --csv --limit <n> --no-color --bind <addr> --token <tok>");
}

fn run_admin_query_command(
    flags: &HashMap<String, FlagValue>,
    client: &OperatorClient,
    json_mode: bool,
    args: &[&str],
) {
    let Some(sql) = args.first().copied().filter(|sql| !sql.is_empty()) else {
        admin_usage_error(
            "admin.query",
            "SQL argument is required",
            "usage: red admin query \"SELECT * FROM red.collections\"",
            json_mode,
        );
    };
    let mut sql = sql.to_string();
    push_limit(&mut sql, flags);
    emit_admin_query_result(
        "admin.query",
        client,
        &sql,
        admin_output_format(flags, json_mode),
        json_mode,
    );
}

fn admin_output_format(flags: &HashMap<String, FlagValue>, json_mode: bool) -> RowFormat {
    if json_mode {
        RowFormat::Json
    } else if flag_bool(flags, "csv") || flag_string(flags, "output").as_deref() == Some("csv") {
        RowFormat::Csv
    } else {
        RowFormat::Table
    }
}

fn admin_usage_error(command: &str, message: &str, usage: &str, json_mode: bool) -> ! {
    if json_mode {
        json_error(command, message);
    }
    eprintln!("error: {message}");
    eprintln!("{usage}");
    std::process::exit(1);
}

fn sql_string_literal(value: &str) -> String {
    value.replace('\'', "''")
}

fn push_limit(sql: &mut String, flags: &HashMap<String, FlagValue>) {
    let Some(limit) = flag_string(flags, "limit")
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u64>().ok())
    else {
        return;
    };
    if !sql.to_ascii_lowercase().contains(" limit ") {
        sql.push_str(" LIMIT ");
        sql.push_str(&limit.to_string());
    }
}

fn emit_admin_query_result(
    command: &str,
    client: &OperatorClient,
    sql: &str,
    format: RowFormat,
    json_mode: bool,
) {
    let (runtime, http) = client.get(command, json_mode);
    match runtime.block_on(http.query(sql)) {
        Ok(result) => std::io::stdout()
            .write_all(&format_query_result(&result, format))
            .expect("write stdout"),
        Err(err) => {
            if json_mode {
                json_error(command, &err.to_string());
            }
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}

fn emit_admin_collection_show(
    client: &OperatorClient,
    name: &str,
    format: RowFormat,
    json_mode: bool,
) {
    let (runtime, http) = client.get("admin.collections.show", json_mode);
    let escaped = sql_string_literal(name);
    let queries = [
        (
            "collection",
            format!("SELECT * FROM red.collections WHERE name = '{escaped}'"),
        ),
        (
            "schema",
            format!("SELECT * FROM red.columns WHERE collection = '{escaped}'"),
        ),
        (
            "indices",
            format!("SELECT * FROM red.indices WHERE collection = '{escaped}'"),
        ),
        (
            "policies",
            format!("SELECT * FROM red.policies WHERE collection = '{escaped}'"),
        ),
        (
            "stats",
            format!("SELECT * FROM red.stats WHERE collection = '{escaped}'"),
        ),
    ];
    let mut sections = Vec::new();
    for (label, sql) in queries {
        match runtime.block_on(http.query(&sql)) {
            Ok(result) => sections.push((label, result)),
            Err(err) => {
                if json_mode {
                    json_error("admin.collections.show", &err.to_string());
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            }
        }
    }

    if format == RowFormat::Json {
        print!("{{");
        for (index, (label, result)) in sections.iter().enumerate() {
            if index > 0 {
                print!(",");
            }
            let rows = format_query_result(result, RowFormat::Json);
            print!(
                "{}:{}",
                JsonValue::string(*label).to_json_string(),
                String::from_utf8_lossy(&rows).trim_end()
            );
        }
        println!("}}");
        return;
    }

    if format == RowFormat::Csv {
        // Five tables share one CSV stream, so each section carries a leading
        // `section` column; without it the concatenated blocks are ambiguous.
        for (label, result) in sections {
            std::io::stdout()
                .write_all(&format_query_result(
                    &with_section_column(label, &result),
                    RowFormat::Csv,
                ))
                .expect("write stdout");
        }
        return;
    }

    println!("Collection: {name}");
    for (label, result) in sections {
        println!("\n{label}");
        std::io::stdout()
            .write_all(&format_query_result(&result, format))
            .expect("write stdout");
    }
}

/// Prepend a constant `section` column to every row of `result`.
fn with_section_column(section: &str, result: &QueryResult) -> QueryResult {
    // A non-empty column list suppresses the driver's own inference, so
    // inherit the inferred names here when the response carried none.
    let base: Vec<String> = if result.columns.is_empty() {
        result
            .rows
            .first()
            .map(|row| row.iter().map(|(key, _)| key.clone()).collect())
            .unwrap_or_default()
    } else {
        result.columns.clone()
    };
    let mut columns = Vec::with_capacity(base.len() + 1);
    columns.push("section".to_string());
    columns.extend(base);
    let rows = result
        .rows
        .iter()
        .map(|row| {
            let mut out = Vec::with_capacity(row.len() + 1);
            out.push((
                "section".to_string(),
                reddb_client::ValueOut::String(section.to_string()),
            ));
            out.extend(row.iter().cloned());
            out
        })
        .collect();
    QueryResult {
        statement: result.statement.clone(),
        affected: result.affected,
        columns,
        rows,
        notice: result.notice.clone(),
    }
}

fn admin_execute_ddl_command(
    command: &str,
    client: &OperatorClient,
    sql: &str,
    name: &str,
    verb: &str,
    json_mode: bool,
) {
    let (runtime, http) = client.get(command, json_mode);
    match runtime.block_on(http.query(sql)) {
        Ok(result) => {
            if json_mode {
                println!(
                    "{{\"ok\":true,\"command\":\"{command}\",\"collection\":\"{}\",\"affected_rows\":{}}}",
                    json_escape(name),
                    result.affected,
                );
            } else {
                println!(
                    "Collection '{name}' {verb}. ({} rows affected)",
                    result.affected
                );
            }
        }
        Err(err) => {
            if json_mode {
                json_error(command, &err.to_string());
            }
            eprintln!("error: {err}");
            std::process::exit(1);
        }
    }
}
fn run_admin_cache_command(client: &OperatorClient, json_mode: bool, args: &[&str]) {
    let subcommand = args.first().copied().unwrap_or("help");
    let sub_args = args.get(1..).unwrap_or_default();
    let command = format!("admin.cache.{subcommand}");
    // Text mode announces what happened before echoing the server body.
    let mut prelude: Option<String> = None;
    let result = match subcommand {
        "stats" => {
            let (runtime, http) = client.get(&command, json_mode);
            runtime.block_on(http.get_text("/admin/blob_cache/stats"))
        }
        "flush-namespace" => {
            let namespace = sub_args.first().copied().unwrap_or("");
            if namespace.is_empty() {
                admin_usage_error(
                    "admin.cache.flush-namespace",
                    "namespace argument is required",
                    "usage: red admin cache flush-namespace <namespace>",
                    json_mode,
                );
            }
            prelude = Some(format!("flushed namespace: {namespace}"));
            let body = JsonValue::object([("namespace", JsonValue::string(namespace))]);
            let (runtime, http) = client.get(&command, json_mode);
            runtime.block_on(http.post_json("/admin/blob_cache/flush_namespace", &body))
        }
        "sweep" => {
            let value = |name: &str| {
                sub_args
                    .windows(2)
                    .find(|pair| pair[0] == name)
                    .and_then(|pair| pair[1].parse::<u64>().ok())
            };
            let mut fields = Vec::new();
            if let Some(limit) = value("--limit-entries") {
                fields.push(("limit_entries", JsonValue::number(limit as f64)));
            }
            if let Some(limit) = value("--limit-millis") {
                fields.push(("limit_millis", JsonValue::number(limit as f64)));
            }
            prelude = Some("sweep complete".to_string());
            let (runtime, http) = client.get(&command, json_mode);
            runtime.block_on(http.post_json("/admin/blob_cache/sweep", &JsonValue::object(fields)))
        }
        "compare-and-set" => {
            let value = |name: &str| {
                sub_args
                    .windows(2)
                    .find(|pair| pair[0] == name)
                    .map(|pair| pair[1])
            };
            let namespace = value("--namespace").unwrap_or_else(|| {
                admin_usage_error(
                    "admin.cache.compare-and-set",
                    "--namespace is required",
                    "usage: red admin cache compare-and-set --namespace NS --key KEY --new-version N --value FILE",
                    json_mode,
                )
            });
            let key = value("--key").unwrap_or_else(|| {
                admin_usage_error(
                    "admin.cache.compare-and-set",
                    "--key is required",
                    "usage: red admin cache compare-and-set --namespace NS --key KEY --new-version N --value FILE",
                    json_mode,
                )
            });
            let new_version = value("--new-version")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or_else(|| {
                    admin_usage_error(
                        "admin.cache.compare-and-set",
                        "--new-version (u64) is required",
                        "usage: red admin cache compare-and-set --namespace NS --key KEY --new-version N --value FILE",
                        json_mode,
                    )
                });
            let value_path = value("--value").unwrap_or_else(|| {
                admin_usage_error(
                    "admin.cache.compare-and-set",
                    "--value FILE is required",
                    "usage: red admin cache compare-and-set --namespace NS --key KEY --new-version N --value FILE",
                    json_mode,
                )
            });
            let bytes = std::fs::read(value_path).unwrap_or_else(|err| {
                if json_mode {
                    json_error(
                        "admin.cache.compare-and-set",
                        &format!("failed to read {value_path}: {err}"),
                    );
                }
                eprintln!("error: failed to read {value_path}: {err}");
                std::process::exit(1);
            });
            let expected_version =
                value("--expected-version").and_then(|value| value.parse::<u64>().ok());
            let (runtime, http) = client.get(&command, json_mode);
            runtime.block_on(http.blob_cache_compare_and_set(
                namespace,
                key,
                &bytes,
                new_version,
                expected_version,
            ))
        }
        _ => {
            if json_mode {
                json_ok(
                    "admin.cache",
                    "{\"subcommands\":[\"stats\",\"flush-namespace\",\"sweep\",\"compare-and-set\"]}",
                );
            } else {
                println!("Usage: red admin cache <subcommand>");
                println!();
                println!("Subcommands:");
                println!("  stats                          GET /admin/blob_cache/stats");
                println!("  flush-namespace <ns>           POST /admin/blob_cache/flush_namespace");
                println!("  sweep [--limit-entries N]      POST /admin/blob_cache/sweep");
                println!("        [--limit-millis N]");
                println!("  compare-and-set                POST /admin/cache/compare-and-set");
                println!("    --namespace ns --key k");
                println!("    --new-version V --value <file>");
                println!("    [--expected-version V]");
                println!();
                println!("Env vars:");
                println!("  REDDB_BIND_ADDR   Server HTTP address (overrides --bind)");
                println!("  RED_ADMIN_TOKEN   Admin bearer token (overrides --token)");
            }
            return;
        }
    };

    let body = result.unwrap_or_else(|err| {
        if json_mode {
            json_error(&command, &err.to_string());
        }
        eprintln!("error: {err}");
        std::process::exit(1);
    });

    if json_mode {
        print!("{body}");
        return;
    }
    if let Some(prelude) = prelude {
        println!("{prelude}");
    }
    if subcommand == "stats" {
        print!("{}", format_cache_stats_pretty(&body));
    } else {
        println!("{body}");
    }
}

/// Aligned `Metric / Value` view of the blob-cache stats body.
///
/// Falls back to the raw body when the response is not a JSON object, so an
/// error page still reaches the operator verbatim.
fn format_cache_stats_pretty(body: &str) -> String {
    let fields: &[(&str, &str)] = &[
        ("hits", "Hits"),
        ("misses", "Misses"),
        ("insertions", "Insertions"),
        ("evictions", "Evictions"),
        ("expirations", "Expirations"),
        ("invalidations", "Invalidations"),
        ("namespace_flushes", "Namespace flushes"),
        ("version_mismatches", "Version mismatches"),
        ("entries", "Entries"),
        ("bytes_in_use", "L1 bytes in use"),
        ("l1_bytes_max", "L1 bytes max"),
        ("l2_bytes_in_use", "L2 bytes in use"),
        ("l2_bytes_max", "L2 bytes max"),
        ("namespaces", "Namespaces"),
        ("max_namespaces", "Max namespaces"),
        ("l2_compression_ratio_observed", "L2 compression ratio"),
        ("l2_bytes_saved_total", "L2 bytes saved total"),
    ];
    let parsed: Option<reddb::json::Value> = reddb::json::from_str(body).ok();
    if let Some(obj) = parsed.as_ref().and_then(|v| v.as_object()) {
        let mut out = format!("{:<30} {}\n{}\n", "Metric", "Value", "-".repeat(50));
        for (key, label) in fields {
            if let Some(val) = obj.get(*key) {
                out.push_str(&format!("{:<30} {}\n", label, val));
            }
        }
        out
    } else {
        format!("{body}\n")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DoctorSeverity {
    Ok,
    Warn,
    Crit,
}

impl DoctorSeverity {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Crit => "crit",
        }
    }
    fn exit_code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::Warn => 1,
            Self::Crit => 2,
        }
    }
}

struct DoctorCheck {
    name: &'static str,
    severity: DoctorSeverity,
    detail: String,
}

/// Pull a numeric metric line out of a Prometheus exposition body.
/// Returns the first match for `metric_name<labels?> <value>`.
fn parse_prom_metric(body: &str, metric_name: &str) -> Option<f64> {
    for line in body.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let head = line.split_whitespace().next()?;
        let name = head.split('{').next().unwrap_or(head);
        if name == metric_name {
            return line.split_whitespace().last().and_then(|v| v.parse().ok());
        }
    }
    None
}

fn parse_prom_metric_with_label(
    body: &str,
    metric_name: &str,
    label_key: &str,
    label_value: &str,
) -> Option<f64> {
    let needle = format!("{label_key}=\"{label_value}\"");
    for line in body.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let head = line.split_whitespace().next()?;
        let name = head.split('{').next().unwrap_or(head);
        if name == metric_name && head.contains(&needle) {
            return line.split_whitespace().last().and_then(|v| v.parse().ok());
        }
    }
    None
}

/// PLAN.md Phase 5.5 — `red doctor`: hit /admin/status + /metrics
/// against a running server, evaluate operator thresholds, print a
/// rollup, and exit 0/1/2. The check set covers the signals
/// dashboards alert on (backup age, WAL archive lag, lease state,
/// replica apply health).
/// Dispatcher for `red bootstrap`. Returns the process exit code.
fn run_bootstrap_command(flags: &HashMap<String, FlagValue>) -> i32 {
    use reddb::cli::bootstrap::{render_success, run, BootstrapArgs};

    let json_mode = wants_json(flags);

    let path = match flag_string(flags, "path").filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => {
            let msg = "bootstrap requires --path <FILE>";
            if json_mode {
                json_error("bootstrap", msg);
            }
            eprintln!("error: {msg}");
            return 2;
        }
    };

    let username = flag_string(flags, "username")
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("REDDB_USERNAME").ok())
        .unwrap_or_default();

    let args = BootstrapArgs {
        path,
        vault: flag_bool(flags, "vault"),
        username,
        password: flag_string(flags, "password").filter(|s| !s.is_empty()),
        password_stdin: flag_bool(flags, "password-stdin"),
        print_certificate: flag_bool(flags, "print-certificate"),
        json: json_mode,
    };

    match run(args) {
        Ok(outcome) => {
            // BootstrapArgs is moved into run(); rebuild a thin view
            // for render_success. Keeping this local avoids cloning
            // the password into the outcome.
            let render_args = reddb::cli::bootstrap::BootstrapArgs {
                path: PathBuf::new(),
                vault: true,
                username: outcome.username.clone(),
                password: None,
                password_stdin: false,
                print_certificate: flag_bool(flags, "print-certificate"),
                json: json_mode,
            };
            render_success(&outcome, &render_args);
            0
        }
        Err(err) => {
            if json_mode {
                json_error("bootstrap", &err);
            }
            eprintln!("bootstrap error: {err}");
            1
        }
    }
}

fn run_doctor(result: &reddb::cli::schema::SchemaResult) -> i32 {
    let json_mode = wants_json(&result.flags);
    let bind = flag_string(&result.flags, "bind").unwrap_or_else(|| "127.0.0.1:5000".to_string());
    let token = admin_token_from_flags_or_env(&result.flags);

    let backup_warn: f64 = flag_string(&result.flags, "backup-age-warn-secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(600.0);
    let backup_crit: f64 = flag_string(&result.flags, "backup-age-crit-secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600.0);
    let wal_warn: f64 = flag_string(&result.flags, "wal-lag-warn")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000.0);
    let wal_crit: f64 = flag_string(&result.flags, "wal-lag-crit")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10000.0);

    let mut checks: Vec<DoctorCheck> = Vec::new();
    let driver = operator_http_client(&bind, token.as_deref());

    // 1) Server reachability via /metrics (also catches admin-token
    // misconfiguration since /metrics is gated when token is set), so the
    // probe reports the HTTP status rather than a generic request failure.
    let metrics = match &driver {
        Ok((runtime, client)) => match runtime.block_on(client.get_text_with_status("/metrics")) {
            Ok((200, body)) => Some(body),
            Ok((status, _)) => {
                checks.push(DoctorCheck {
                    name: "reachability",
                    severity: DoctorSeverity::Crit,
                    detail: format!(
                        "GET /metrics returned HTTP {status} (token mismatch or service down)"
                    ),
                });
                None
            }
            Err(err) => {
                checks.push(DoctorCheck {
                    name: "reachability",
                    severity: DoctorSeverity::Crit,
                    detail: format!("connect to {bind} failed: {err}"),
                });
                None
            }
        },
        Err(err) => {
            checks.push(DoctorCheck {
                name: "reachability",
                severity: DoctorSeverity::Crit,
                detail: format!("connect to {bind} failed: {err}"),
            });
            None
        }
    };

    // 2) /admin/status JSON.
    let status_json = driver.as_ref().ok().and_then(|(runtime, client)| {
        runtime
            .block_on(client.get_text_with_status("/admin/status"))
            .ok()
            .filter(|(status, _)| *status == 200)
            .and_then(|(_, body)| {
                reddb::serde_json::from_str::<reddb::serde_json::Value>(&body).ok()
            })
    });
    if status_json.is_none() {
        checks.push(DoctorCheck {
            name: "admin_status",
            severity: DoctorSeverity::Warn,
            detail: "GET /admin/status not parseable; downstream checks rely on /metrics only"
                .to_string(),
        });
    }

    // 3) Backup age.
    if let Some(body) = &metrics {
        if let Some(age) = parse_prom_metric(body, "reddb_backup_age_seconds") {
            let sev = if age >= backup_crit {
                DoctorSeverity::Crit
            } else if age >= backup_warn {
                DoctorSeverity::Warn
            } else {
                DoctorSeverity::Ok
            };
            checks.push(DoctorCheck {
                name: "backup_age",
                severity: sev,
                detail: format!(
                    "{age:.0}s since last successful backup (warn={backup_warn}s crit={backup_crit}s)"
                ),
            });
        } else {
            checks.push(DoctorCheck {
                name: "backup_age",
                severity: DoctorSeverity::Warn,
                detail: "no successful backup recorded yet (reddb_backup_age_seconds absent)"
                    .to_string(),
            });
        }
    }

    // 4) WAL archive lag.
    if let Some(body) = &metrics {
        if let Some(lag) = parse_prom_metric(body, "reddb_wal_archive_lag_records") {
            let sev = if lag >= wal_crit {
                DoctorSeverity::Crit
            } else if lag >= wal_warn {
                DoctorSeverity::Warn
            } else {
                DoctorSeverity::Ok
            };
            checks.push(DoctorCheck {
                name: "wal_archive_lag",
                severity: sev,
                detail: format!(
                    "{lag:.0} records between current LSN and last archived (warn={wal_warn} crit={wal_crit})"
                ),
            });
        }
    }

    // 5) Writer lease state — `not_held` is critical when the role is
    // primary (silent split-brain risk). `not_required` is fine.
    if let Some(json) = &status_json {
        if let Some(lease) = json.get("writer_lease").and_then(|v| v.as_str()) {
            let sev = match lease {
                "not_held" => DoctorSeverity::Crit,
                _ => DoctorSeverity::Ok,
            };
            checks.push(DoctorCheck {
                name: "writer_lease",
                severity: sev,
                detail: format!("lease state: {lease}"),
            });
        }
    }

    // 6) Replica apply health (replica only).
    if let Some(json) = &status_json {
        if let Some(health) = json
            .get("replica")
            .and_then(|v| v.get("apply_health"))
            .and_then(|v| v.as_str())
        {
            let sev = match health {
                "ok" | "healthy" | "connecting" => DoctorSeverity::Ok,
                "stalled_gap" | "divergence" => DoctorSeverity::Crit,
                _ => DoctorSeverity::Warn,
            };
            checks.push(DoctorCheck {
                name: "replica_apply_health",
                severity: sev,
                detail: format!("replica apply state: {health}"),
            });
        }
    }

    // 7) Read-only flag — informational. Surfaces as warn so an
    // operator that didn't expect it sees it.
    if let Some(json) = &status_json {
        if let Some(true) = json.get("read_only").and_then(|v| v.as_bool()) {
            checks.push(DoctorCheck {
                name: "read_only",
                severity: DoctorSeverity::Warn,
                detail: "instance is read-only; writes will be rejected".to_string(),
            });
        }
    }

    // 8) Normal-KV operation counters — exposed as an OK check so
    // `red doctor` output carries the same operator snapshot as /stats.
    if let Some(body) = &metrics {
        let puts =
            parse_prom_metric_with_label(body, "reddb_kv_ops_total", "verb", "put").unwrap_or(0.0);
        let gets =
            parse_prom_metric_with_label(body, "reddb_kv_ops_total", "verb", "get").unwrap_or(0.0);
        let deletes = parse_prom_metric_with_label(body, "reddb_kv_ops_total", "verb", "delete")
            .unwrap_or(0.0);
        let incrs =
            parse_prom_metric_with_label(body, "reddb_kv_ops_total", "verb", "incr").unwrap_or(0.0);
        let watch_active = parse_prom_metric(body, "reddb_kv_watch_streams_active").unwrap_or(0.0);
        let watch_drops = parse_prom_metric(body, "reddb_kv_watch_drops_total").unwrap_or(0.0);
        checks.push(DoctorCheck {
            name: "kv_stats",
            severity: DoctorSeverity::Ok,
            detail: format!(
                "puts={puts:.0} gets={gets:.0} deletes={deletes:.0} incrs={incrs:.0} watch_active={watch_active:.0} watch_drops={watch_drops:.0}"
            ),
        });
    }

    let worst = checks
        .iter()
        .map(|c| c.severity)
        .max()
        .unwrap_or(DoctorSeverity::Ok);

    if json_mode {
        let mut buf = String::from("{\"checks\":[");
        for (i, c) in checks.iter().enumerate() {
            if i > 0 {
                buf.push(',');
            }
            buf.push_str(&format!(
                "{{\"name\":\"{}\",\"severity\":\"{}\",\"detail\":\"{}\"}}",
                json_escape(c.name),
                c.severity.label(),
                json_escape(&c.detail)
            ));
        }
        buf.push_str(&format!("],\"worst\":\"{}\"}}", worst.label()));
        json_ok("doctor", &buf);
    } else {
        for c in &checks {
            let icon = match c.severity {
                DoctorSeverity::Ok => "[ok]  ",
                DoctorSeverity::Warn => "[warn]",
                DoctorSeverity::Crit => "[crit]",
            };
            println!("{icon} {} — {}", c.name, c.detail);
        }
        println!("\nworst: {}", worst.label());
    }
    worst.exit_code()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn doctor_parses_simple_metric_line() {
        let body = "\
# HELP reddb_uptime_seconds Seconds since boot.\n\
# TYPE reddb_uptime_seconds gauge\n\
reddb_uptime_seconds 12345.6\n";
        assert_eq!(
            parse_prom_metric(body, "reddb_uptime_seconds"),
            Some(12345.6)
        );
    }

    #[test]
    fn doctor_parses_metric_with_labels() {
        let body = "\
# HELP reddb_writer_lease_state ...\n\
# TYPE reddb_writer_lease_state gauge\n\
reddb_writer_lease_state{state=\"held\"} 1\n";
        assert_eq!(
            parse_prom_metric(body, "reddb_writer_lease_state"),
            Some(1.0)
        );
    }

    #[test]
    fn doctor_parses_metric_with_specific_label() {
        let body = "\
reddb_kv_ops_total{verb=\"put\"} 2\n\
reddb_kv_ops_total{verb=\"get\"} 3\n";
        assert_eq!(
            parse_prom_metric_with_label(body, "reddb_kv_ops_total", "verb", "get"),
            Some(3.0)
        );
    }

    #[test]
    fn doctor_returns_first_match_when_multiple_label_sets() {
        let body = "\
reddb_replica_lag_records{replica_id=\"a\"} 100\n\
reddb_replica_lag_records{replica_id=\"b\"} 250\n";
        assert_eq!(
            parse_prom_metric(body, "reddb_replica_lag_records"),
            Some(100.0)
        );
    }

    #[test]
    fn doctor_misses_unknown_metric() {
        let body = "reddb_uptime_seconds 1\n";
        assert_eq!(parse_prom_metric(body, "reddb_does_not_exist"), None);
    }

    #[test]
    fn doctor_skips_help_and_type_lines() {
        let body = "\
# HELP reddb_uptime_seconds Time since boot.\n\
# TYPE reddb_uptime_seconds gauge\n";
        assert_eq!(parse_prom_metric(body, "reddb_uptime_seconds"), None);
    }

    #[test]
    fn doctor_severity_orders_ok_warn_crit() {
        assert!(DoctorSeverity::Ok < DoctorSeverity::Warn);
        assert!(DoctorSeverity::Warn < DoctorSeverity::Crit);
        assert_eq!(DoctorSeverity::Ok.exit_code(), 0);
        assert_eq!(DoctorSeverity::Warn.exit_code(), 1);
        assert_eq!(DoctorSeverity::Crit.exit_code(), 2);
    }

    fn bool_flag(value: bool) -> FlagValue {
        FlagValue::Bool(value)
    }

    fn str_flag(value: &str) -> FlagValue {
        FlagValue::Str(value.to_string())
    }

    #[test]
    fn migrate_pager_zone_converts_a_closed_legacy_store() {
        let dir = std::env::temp_dir().join(format!(
            "reddb-migrate-pager-zone-cli-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.rdb");

        let pager = reddb::storage::engine::pager::Pager::open_default(&path).unwrap();
        pager.sync().unwrap();
        drop(pager);
        reddb::pager_zone_migration::revert_to_sidecars(&path).unwrap();

        let flags = HashMap::from([(
            "path".to_string(),
            str_flag(path.to_string_lossy().as_ref()),
        )]);
        assert_eq!(run_migrate_pager_zone_command(&flags), 0);
        assert!(path.with_extension("rdb.pre-migration").exists());
        assert!(!PathBuf::from(format!("{}-hdr", path.display())).exists());
        assert!(!PathBuf::from(format!("{}-meta", path.display())).exists());

        let reopened = reddb::storage::engine::pager::Pager::open_default(&path).unwrap();
        drop(reopened);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn query_params_accept_short_p_alias() {
        let args = vec![
            "query".to_string(),
            "SELECT $1, $2".to_string(),
            "-p".to_string(),
            "42".to_string(),
            "-p=alice".to_string(),
        ];
        let params = collect_query_params(&args).unwrap();
        assert_eq!(params[0], reddb_client::Value::Int(42));
        assert_eq!(params[1], reddb_client::Value::Text("alice".to_string()));
    }

    #[test]
    fn query_short_p_parses_as_param_not_path() {
        let args = vec![
            "query".to_string(),
            "SELECT $1".to_string(),
            "-p".to_string(),
            "42".to_string(),
            "--path".to_string(),
            "/tmp/data.rdb".to_string(),
        ];
        let tokens = cli::token::tokenize(&args);
        let parser =
            cli::schema::SchemaParser::new(cli::commands::flags_for_command(Some("query")));
        let result = parser.parse(&tokens);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(
            result.flags.get("path").unwrap().as_str_value(),
            "/tmp/data.rdb"
        );
        assert_eq!(result.flags.get("param").unwrap().as_str_value(), "42");
    }

    #[test]
    fn mcp_without_uri_or_env_uses_legacy_runtime_fallback() {
        let _lock = env_lock().lock().unwrap();
        let _clear = EnvGuard::clear(&["REDDB_MCP_URI"]);

        assert!(resolve_mcp_client_options(&HashMap::new())
            .unwrap()
            .is_none());
    }

    #[test]
    fn mcp_uri_reads_env_file_target() {
        let _lock = env_lock().lock().unwrap();
        let _env = EnvGuard::set(&[("REDDB_MCP_URI", "file:///tmp/reddb-env.rdb")]);

        let options = resolve_mcp_client_options(&HashMap::new())
            .unwrap()
            .unwrap();

        assert!(matches!(
            options.target,
            reddb_wire::ConnectionTarget::File { ref path }
                if path == &PathBuf::from("/tmp/reddb-env.rdb")
        ));
    }

    #[test]
    fn mcp_uri_prefers_flag_over_url_and_env() {
        let _lock = env_lock().lock().unwrap();
        let _env = EnvGuard::set(&[("REDDB_MCP_URI", "red://env.example:5050")]);
        let flags = HashMap::from([
            ("uri".to_string(), str_flag("file:///tmp/reddb-agent.rdb")),
            ("url".to_string(), str_flag("reds://url.example:5050")),
        ]);

        let options = resolve_mcp_client_options(&flags).unwrap().unwrap();

        assert!(matches!(
            options.target,
            reddb_wire::ConnectionTarget::File { ref path }
                if path == &PathBuf::from("/tmp/reddb-agent.rdb")
        ));
    }

    #[test]
    fn mcp_path_fallback_does_not_mask_uri_resolution() {
        let _lock = env_lock().lock().unwrap();
        let _clear = EnvGuard::clear(&["REDDB_MCP_URI"]);
        let mut flags = HashMap::new();
        flags.insert("path".to_string(), str_flag("/tmp/reddb-legacy.rdb"));

        assert!(resolve_mcp_client_options(&flags).unwrap().is_none());
    }

    #[test]
    fn mcp_url_flag_wins_over_env_and_defaults_timeout() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[("REDDB_MCP_URI", "https://env.example:443")]);
        let flags = HashMap::from([(
            "url".to_string(),
            str_flag("reds://user:pass@cli.example:5050"),
        )]);

        let options = resolve_mcp_client_options(&flags).unwrap().unwrap();

        assert_eq!(
            options.redacted_uri,
            "reds://<redacted>:<redacted>@cli.example:5050"
        );
        assert_eq!(options.timeout, Duration::from_secs(20));
    }

    #[test]
    fn mcp_url_env_is_used_when_flag_absent_and_timeout_env_overrides_default() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_MCP_URI", "https://user:pass@env.example:443"),
            ("REDDB_MCP_TIMEOUT_S", "7"),
        ]);

        let options = resolve_mcp_client_options(&HashMap::new())
            .unwrap()
            .unwrap();

        assert_eq!(
            options.redacted_uri,
            "https://<redacted>:<redacted>@env.example:443"
        );
        assert_eq!(options.timeout, Duration::from_secs(7));
    }

    #[test]
    fn mcp_url_query_timeout_overrides_env_timeout() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[("REDDB_MCP_TIMEOUT_S", "7")]);
        let flags = HashMap::from([("url".to_string(), str_flag("https://user:pass@h?timeout=3"))]);

        let options = resolve_mcp_client_options(&flags).unwrap().unwrap();

        assert_eq!(options.timeout, Duration::from_secs(3));
    }

    #[test]
    fn mcp_bearer_token_rejects_cleartext_and_redacts_error() {
        let flags = HashMap::from([
            ("url".to_string(), str_flag("http://host:5000")),
            ("token".to_string(), str_flag("secret-token-1")),
        ]);

        let err = resolve_mcp_client_options(&flags).unwrap_err();

        assert!(err.contains("bearer token requires TLS"), "{err}");
        assert!(!err.contains("secret-token-1"), "{err}");
    }

    #[test]
    fn mcp_remote_uri_requires_credentials() {
        let flags = HashMap::from([("url".to_string(), str_flag("https://db.example"))]);

        let error = resolve_mcp_client_options(&flags).unwrap_err();

        assert!(error.contains("remote MCP requires credentials"), "{error}");
    }

    fn assert_anonymous_remote_mcp_tools_are_denied(tool_names: &[&str]) {
        let options = McpClientOptions {
            redacted_uri: "https://db.example".to_string(),
            target: reddb_wire::ConnectionTarget::Http {
                base_url: "https://db.example".to_string(),
            },
            auth: reddb_wire::ConnectionAuth::Anonymous,
            timeout: Duration::from_secs(1),
        };
        let server = RemoteMcpServer::new(options);

        for tool_name in tool_names {
            let mut params = reddb::json::Map::new();
            params.insert(
                "name".to_string(),
                reddb::json::Value::String((*tool_name).to_string()),
            );
            params.insert(
                "arguments".to_string(),
                reddb::json::Value::Object(reddb::json::Map::new()),
            );
            let params = reddb::json::Value::Object(params);
            let response =
                server.handle_tools_call(Some(&reddb::json::Value::Number(1.0)), Some(&params));
            let parsed: reddb::json::Value = reddb::json::from_str(&response).unwrap();
            let result = parsed.get("result").expect("MCP result");
            assert_eq!(
                result.get("isError").and_then(reddb::json::Value::as_bool),
                Some(true),
                "anonymous remote call unexpectedly reached {tool_name}: {response}"
            );
            let text = result
                .get("content")
                .and_then(reddb::json::Value::as_array)
                .and_then(|content| content.first())
                .and_then(|item| item.get("text"))
                .and_then(reddb::json::Value::as_str)
                .expect("MCP error text");
            assert!(
                text.contains("remote MCP requires credentials"),
                "unexpected denial for {tool_name}: {text}"
            );
        }
    }

    #[test]
    fn anonymous_remote_mcp_rejects_every_vault_tool() {
        assert_anonymous_remote_mcp_tools_are_denied(&[
            "reddb_vault_get",
            "reddb_vault_put",
            "reddb_vault_unseal",
        ]);
    }

    #[test]
    fn anonymous_remote_mcp_rejects_every_auth_tool() {
        assert_anonymous_remote_mcp_tools_are_denied(&[
            "reddb_auth_bootstrap",
            "reddb_auth_create_user",
            "reddb_auth_login",
            "reddb_auth_create_api_key",
            "reddb_auth_list_users",
        ]);
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&'static str, &str)]) -> Self {
            let mut saved = Vec::new();
            let mut dedup = BTreeMap::new();
            for (key, value) in vars {
                dedup.insert(*key, *value);
            }
            for (key, value) in dedup {
                saved.push((key, std::env::var(key).ok()));
                unsafe {
                    std::env::set_var(key, value);
                }
            }
            Self { saved }
        }

        fn clear(keys: &[&'static str]) -> Self {
            let mut saved = Vec::new();
            let mut dedup = BTreeMap::new();
            for key in keys {
                dedup.insert(*key, ());
            }
            for (key, _) in dedup {
                saved.push((key, std::env::var(key).ok()));
                unsafe {
                    std::env::remove_var(key);
                }
            }
            Self { saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..).rev() {
                match value {
                    Some(value) => unsafe {
                        std::env::set_var(key, value);
                    },
                    None => unsafe {
                        std::env::remove_var(key);
                    },
                }
            }
        }
    }

    #[test]
    fn admin_token_reads_file_env_and_flag_wins() {
        let _lock = env_lock().lock().unwrap();
        let dir =
            std::env::temp_dir().join(format!("reddb-admin-token-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("token");
        std::fs::write(&path, "file-token\n").unwrap();
        let _clear = EnvGuard::clear(&["RED_ADMIN_TOKEN", "RED_ADMIN_TOKEN_FILE"]);
        let path_str = path.to_string_lossy().to_string();
        let _file = EnvGuard::set(&[("RED_ADMIN_TOKEN_FILE", path_str.as_str())]);

        assert_eq!(
            admin_token_from_flags_or_env(&HashMap::new()).as_deref(),
            Some("file-token")
        );

        let mut flags = HashMap::new();
        flags.insert("token".to_string(), str_flag("flag-token"));
        assert_eq!(
            admin_token_from_flags_or_env(&flags).as_deref(),
            Some("flag-token")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_server_binds_defaults_to_grpc() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
            "REDDB_VAULT",
        ]);
        let flags = HashMap::new();
        let (grpc_bind, http_bind) = resolve_server_binds(&flags).unwrap();
        assert_eq!(
            grpc_bind.as_deref(),
            Some(reddb::service_cli::ServerTransport::Grpc.default_bind_addr())
        );
        assert_eq!(http_bind, None);
    }

    #[test]
    fn resolve_server_binds_supports_dual_stack_defaults() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
            "REDDB_VAULT",
        ]);
        let flags = HashMap::from([
            ("grpc".to_string(), bool_flag(true)),
            ("http".to_string(), bool_flag(true)),
        ]);
        let (grpc_bind, http_bind) = resolve_server_binds(&flags).unwrap();
        assert_eq!(
            grpc_bind.as_deref(),
            Some(reddb::service_cli::ServerTransport::Grpc.default_bind_addr())
        );
        assert_eq!(
            http_bind.as_deref(),
            Some(reddb::service_cli::ServerTransport::Http.default_bind_addr())
        );
    }

    #[test]
    fn resolve_server_binds_rejects_ambiguous_legacy_bind() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
            "REDDB_VAULT",
        ]);
        let flags = HashMap::from([
            ("grpc".to_string(), bool_flag(true)),
            ("http".to_string(), bool_flag(true)),
            ("bind".to_string(), str_flag("0.0.0.0:9999")),
        ]);
        let error = resolve_server_binds(&flags).unwrap_err();
        assert!(error.contains("--bind is ambiguous"));
    }

    #[test]
    fn resolve_server_binds_accepts_explicit_dual_addresses() {
        let flags = HashMap::from([
            ("grpc-bind".to_string(), str_flag("0.0.0.0:55055")),
            ("http-bind".to_string(), str_flag("0.0.0.0:5000")),
        ]);
        let (grpc_bind, http_bind) = resolve_server_binds(&flags).unwrap();
        assert_eq!(grpc_bind.as_deref(), Some("0.0.0.0:55055"));
        assert_eq!(http_bind.as_deref(), Some("0.0.0.0:5000"));
    }

    #[test]
    fn build_http_limits_flag_overrides_env() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_HTTP_MAX_HANDLERS", "99"),
            ("REDDB_HTTP_HANDLER_TIMEOUT_MS", "7000"),
            ("REDDB_HTTP_RETRY_AFTER_SECS", "9"),
        ]);
        let flags = HashMap::from([
            ("http-max-handlers".to_string(), str_flag("16")),
            ("http-handler-timeout-ms".to_string(), str_flag("5000")),
            ("http-retry-after-secs".to_string(), str_flag("3")),
        ]);
        let limits = build_http_limits_cli_input(&flags).unwrap();
        assert_eq!(limits.max_handlers_flag, Some(16));
        assert_eq!(limits.handler_timeout_ms_flag, Some(5_000));
        assert_eq!(limits.retry_after_secs_flag, Some(3));
        assert_eq!(limits.max_handlers_env, Some(99));
        assert_eq!(limits.handler_timeout_ms_env, Some(7_000));
        assert_eq!(limits.retry_after_secs_env, Some(9));
    }

    #[test]
    fn build_http_limits_rejects_zero_cap_flag() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&["REDDB_HTTP_MAX_HANDLERS"]);
        let flags = HashMap::from([("http-max-handlers".to_string(), str_flag("0"))]);
        let err = build_http_limits_cli_input(&flags).unwrap_err();
        assert!(err.contains("--http-max-handlers"), "got: {err}");
        assert!(err.contains(">= 1"), "got: {err}");
    }

    #[test]
    fn build_http_limits_rejects_too_short_timeout_flag() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&["REDDB_HTTP_HANDLER_TIMEOUT_MS"]);
        let flags = HashMap::from([("http-handler-timeout-ms".to_string(), str_flag("10"))]);
        let err = build_http_limits_cli_input(&flags).unwrap_err();
        assert!(err.contains("handler_timeout_ms"), "got: {err}");
    }

    #[test]
    fn build_http_limits_rejects_out_of_range_retry_after_flag() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&["REDDB_HTTP_RETRY_AFTER_SECS"]);
        let flags = HashMap::from([("http-retry-after-secs".to_string(), str_flag("99"))]);
        let err = build_http_limits_cli_input(&flags).unwrap_err();
        assert!(err.contains("retry_after_secs"), "got: {err}");
        assert!(err.contains("[1, 30]"), "got: {err}");
    }

    #[test]
    fn build_http_limits_rejects_garbage_env() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[("REDDB_HTTP_MAX_HANDLERS", "not-a-number")]);
        let flags: HashMap<String, FlagValue> = HashMap::new();
        let err = build_http_limits_cli_input(&flags).unwrap_err();
        assert!(err.contains("REDDB_HTTP_MAX_HANDLERS"), "got: {err}");
    }

    #[test]
    fn build_server_config_defaults_to_router_on_5050() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
            "REDDB_VAULT",
        ]);
        let flags = HashMap::new();
        let config = build_server_config(&flags, None).unwrap();
        assert_eq!(
            config.router_bind_addr.as_deref(),
            Some(reddb::service_cli::DEFAULT_ROUTER_BIND_ADDR)
        );
        assert_eq!(config.grpc_bind_addr, None);
        assert_eq!(config.http_bind_addr, None);
        assert_eq!(config.wire_bind_addr, None);
    }

    #[test]
    fn build_server_config_defaults_ui_off() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
        ]);
        let config = build_server_config(&HashMap::new(), None).unwrap();
        assert!(!config.ui, "--ui is off by default");
        assert_eq!(config.ui_dir, None);
    }

    #[test]
    fn build_server_config_threads_ui_flags() {
        // `red server --ui --ui-dir <dir>` (issue #1047): the flag enables
        // the served bundle and the explicit directory overrides the
        // resolved/cached pinned bundle.
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
        ]);
        let flags = HashMap::from([
            ("ui".to_string(), bool_flag(true)),
            ("ui-dir".to_string(), str_flag("/srv/red-ui/dist")),
        ]);
        let config = build_server_config(&flags, None).unwrap();
        assert!(config.ui);
        assert_eq!(
            config.ui_dir.as_deref(),
            Some(std::path::Path::new("/srv/red-ui/dist"))
        );
    }

    #[test]
    fn build_server_config_maps_legacy_bind_to_router_when_no_transport_is_selected() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
        ]);
        let flags = HashMap::from([("bind".to_string(), str_flag("0.0.0.0:5050"))]);
        let config = build_server_config(&flags, None).unwrap();
        assert_eq!(config.router_bind_addr.as_deref(), Some("0.0.0.0:5050"));
        assert_eq!(config.grpc_bind_addr, None);
        assert_eq!(config.http_bind_addr, None);
    }

    #[test]
    fn build_server_config_uses_docker_env_defaults() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_DATA_PATH", "/data/data.rdb"),
            ("REDDB_WIRE_BIND_ADDR", "0.0.0.0:5050"),
            ("REDDB_GRPC_BIND_ADDR", "0.0.0.0:55055"),
            ("REDDB_HTTP_BIND_ADDR", "0.0.0.0:5000"),
            ("REDDB_VAULT", "true"),
        ]);

        let flags = HashMap::from([("path".to_string(), str_flag("./data/reddb.rdb"))]);
        let config = build_server_config(&flags, None).unwrap();

        assert_eq!(
            config.path.as_deref(),
            Some(std::path::Path::new("/data/data.rdb"))
        );
        assert_eq!(config.router_bind_addr, None);
        assert_eq!(config.wire_bind_addr.as_deref(), Some("0.0.0.0:5050"));
        assert_eq!(config.grpc_bind_addr.as_deref(), Some("0.0.0.0:55055"));
        assert_eq!(config.http_bind_addr.as_deref(), Some("0.0.0.0:5000"));
        assert!(config.vault);
    }

    // reddb-io/rio-lair#255 — the Docker image's REDDB_WIRE_BIND_ADDR
    // default must lose the port to an explicit --wire-tls-bind instead
    // of booting a plaintext listener that kills the TLS one.
    #[test]
    fn build_server_config_suppresses_env_wire_bind_when_wire_tls_flag_owns_the_port() {
        let _lock = env_lock().lock().unwrap();
        let _cleared = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
        ]);
        let _guard = EnvGuard::set(&[("REDDB_WIRE_BIND_ADDR", "0.0.0.0:5050")]);
        let flags = HashMap::from([
            ("http-bind".to_string(), str_flag("[::]:5055")),
            ("grpc-bind".to_string(), str_flag("[::]:5555")),
            ("wire-tls-bind".to_string(), str_flag("[::]:5050")),
        ]);
        let config = build_server_config(&flags, None).unwrap();
        assert_eq!(config.wire_bind_addr, None);
        assert_eq!(config.wire_tls_bind_addr.as_deref(), Some("[::]:5050"));
    }

    #[test]
    fn build_server_config_keeps_env_wire_bind_when_ports_differ_from_wire_tls() {
        let _lock = env_lock().lock().unwrap();
        let _cleared = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
        ]);
        let _guard = EnvGuard::set(&[("REDDB_WIRE_BIND_ADDR", "0.0.0.0:5050")]);
        let flags = HashMap::from([("wire-tls-bind".to_string(), str_flag("[::]:5051"))]);
        let config = build_server_config(&flags, None).unwrap();
        assert_eq!(config.wire_bind_addr.as_deref(), Some("0.0.0.0:5050"));
        assert_eq!(config.wire_tls_bind_addr.as_deref(), Some("[::]:5051"));
    }

    #[test]
    fn build_server_config_rejects_explicit_wire_and_wire_tls_flags_on_same_port() {
        let _lock = env_lock().lock().unwrap();
        let _cleared = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
        ]);
        let flags = HashMap::from([
            ("wire-bind".to_string(), str_flag("0.0.0.0:5050")),
            ("wire-tls-bind".to_string(), str_flag("[::]:5050")),
        ]);
        let err = build_server_config(&flags, None).unwrap_err();
        assert!(
            err.contains("same port"),
            "expected a same-port error, got: {err}"
        );
    }

    #[test]
    fn build_server_config_prefers_cli_flags_over_env_defaults() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_DATA_PATH", "/data/data.rdb"),
            ("REDDB_GRPC_BIND_ADDR", "0.0.0.0:55055"),
            ("REDDB_HTTP_BIND_ADDR", "0.0.0.0:5000"),
        ]);

        let flags = HashMap::from([
            ("path".to_string(), str_flag("/tmp/override.rdb")),
            ("http-bind".to_string(), str_flag("127.0.0.1:18080")),
        ]);
        let config = build_server_config(&flags, None).unwrap();

        assert_eq!(
            config.path.as_deref(),
            Some(std::path::Path::new("/tmp/override.rdb"))
        );
        assert_eq!(config.grpc_bind_addr.as_deref(), Some("0.0.0.0:55055"));
        assert_eq!(config.http_bind_addr.as_deref(), Some("127.0.0.1:18080"));
    }

    #[test]
    fn build_server_config_reads_auth_and_bootstrap_env() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_AUTH", "true"),
            ("REDDB_REQUIRE_AUTH", "true"),
            ("REDDB_VAULT", "true"),
            ("REDDB_BOOTSTRAP_PRESET", "production"),
            ("REDDB_BOOTSTRAP_MANIFEST", "/etc/reddb/bootstrap.json"),
        ]);

        let config = build_server_config(&HashMap::new(), None).unwrap();

        assert!(config.auth);
        assert!(config.require_auth);
        assert!(config.vault);
        assert_eq!(config.bootstrap.preset.as_deref(), Some("production"));
        assert_eq!(
            config.bootstrap.manifest.as_deref(),
            Some(std::path::Path::new("/etc/reddb/bootstrap.json"))
        );
    }

    #[test]
    fn build_server_config_prefers_cli_bootstrap_preset_over_env() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[("REDDB_BOOTSTRAP_PRESET", "production")]);
        let flags = HashMap::from([
            ("bootstrap-preset".to_string(), str_flag("cloud")),
            ("bootstrap-admin".to_string(), str_flag("head")),
            (
                "bootstrap-admin-password".to_string(),
                str_flag("head-pass"),
            ),
            ("customer-admin".to_string(), str_flag("customer")),
            (
                "customer-admin-password".to_string(),
                str_flag("customer-pass"),
            ),
        ]);

        let config = build_server_config(&flags, None).unwrap();

        assert_eq!(config.bootstrap.preset.as_deref(), Some("cloud"));
        assert_eq!(config.bootstrap.admin_username.as_deref(), Some("head"));
        assert_eq!(
            config.bootstrap.admin_password.as_deref(),
            Some("head-pass")
        );
        assert_eq!(config.bootstrap.customer_admin.as_deref(), Some("customer"));
        assert_eq!(
            config.bootstrap.customer_admin_password.as_deref(),
            Some("customer-pass")
        );
    }

    #[test]
    fn build_server_config_defaults_primary_role_to_dev_primary_replica_profile() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
            "REDDB_TOPOLOGY",
            "REDDB_NODE_ROLE",
            "REDDB_PRIMARY_ADDR",
        ]);
        let flags = HashMap::from([("role".to_string(), str_flag("primary"))]);
        let config = build_server_config(&flags, None).unwrap();
        assert_eq!(
            config.storage_profile.deploy_profile,
            reddb::storage::DeployProfile::PrimaryReplica
        );
        assert_eq!(
            config.storage_profile.packaging,
            reddb::storage::StoragePackaging::SingleFile
        );
    }

    #[test]
    fn build_server_config_uses_topology_env_for_storage_default() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_TOPOLOGY", "serverless"),
            ("REDDB_NODE_ROLE", "serverless"),
        ]);
        let _clear = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
            "REDDB_PRESET",
            "REDDB_BOOTSTRAP_MANIFEST",
            "REDDB_USERNAME",
            "REDDB_USERNAME_FILE",
            "REDDB_PASSWORD",
            "REDDB_PASSWORD_FILE",
            "REDDB_PRIMARY_ADDR",
        ]);

        let config = build_server_config(&HashMap::new(), None).unwrap();

        assert_eq!(config.role, "standalone");
        assert_eq!(
            config.storage_profile.deploy_profile,
            reddb::storage::DeployProfile::Serverless
        );
    }

    #[test]
    fn operational_bootstrap_uses_config_file_env() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[("REDDB_CONFIG_FILE", "/etc/reddb/custom.json")]);
        let _clear = EnvGuard::clear(&[
            "REDDB_TOPOLOGY",
            "REDDB_NODE_ROLE",
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
        ]);

        let plan = resolve_operational_bootstrap(&HashMap::new(), None).unwrap();

        assert_eq!(plan.config_file_path, "/etc/reddb/custom.json");
    }

    #[test]
    fn build_server_config_uses_node_role_env_for_primary_replica() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_TOPOLOGY", "primary-replica"),
            ("REDDB_NODE_ROLE", "replica"),
        ]);
        let _clear = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
            "REDDB_PRIMARY_ADDR",
        ]);

        let config = build_server_config(&HashMap::new(), None).unwrap();

        assert_eq!(config.role, "replica");
        assert_eq!(config.primary_addr.as_deref(), None);
        assert_eq!(
            config.storage_profile.deploy_profile,
            reddb::storage::DeployProfile::PrimaryReplica
        );
    }

    #[test]
    fn build_server_config_reads_primary_addr_env_for_replica() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_TOPOLOGY", "primary-replica"),
            ("REDDB_NODE_ROLE", "replica"),
            ("REDDB_PRIMARY_ADDR", "http://primary:55055"),
        ]);
        let _clear = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
        ]);

        let config = build_server_config(&HashMap::new(), None).unwrap();

        assert_eq!(config.role, "replica");
        assert_eq!(config.primary_addr.as_deref(), Some("http://primary:55055"));
    }

    #[test]
    fn build_server_config_keeps_cluster_member_on_standalone_process_role() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_TOPOLOGY", "cluster"),
            ("REDDB_NODE_ROLE", "cluster-member"),
        ]);
        let _clear = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
            "REDDB_PRIMARY_ADDR",
        ]);

        let config = build_server_config(&HashMap::new(), None).unwrap();

        assert_eq!(config.role, "standalone");
        assert_eq!(
            config.storage_profile.deploy_profile,
            reddb::storage::DeployProfile::Cluster
        );
    }

    #[test]
    fn build_server_config_rejects_cluster_auth_bootstrap_env() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_TOPOLOGY", "cluster"),
            ("REDDB_NODE_ROLE", "cluster-member"),
            ("REDDB_PRESET", "production"),
            ("REDDB_USERNAME", "ops"),
            ("REDDB_PASSWORD", "hunter2"),
        ]);
        let _clear = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
        ]);

        let err = build_server_config(&HashMap::new(), None).unwrap_err();

        assert!(err.contains("REDDB_PRESET"), "got: {err}");
        assert!(err.contains("cluster-shaped boots"), "got: {err}");
        assert!(err.contains("writer/volume owner"), "got: {err}");
    }

    #[test]
    fn build_server_config_rejects_auth_bootstrap_env_with_cluster_storage_preset() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_STORAGE_PRESET", "cluster"),
            ("REDDB_BOOTSTRAP_MANIFEST", "/etc/reddb/bootstrap.json"),
        ]);
        let _clear = EnvGuard::clear(&[
            "REDDB_TOPOLOGY",
            "REDDB_NODE_ROLE",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
            "REDDB_PRESET",
            "REDDB_USERNAME",
            "REDDB_USERNAME_FILE",
            "REDDB_PASSWORD",
            "REDDB_PASSWORD_FILE",
        ]);

        let err = build_server_config(&HashMap::new(), None).unwrap_err();

        assert!(err.contains("REDDB_BOOTSTRAP_MANIFEST"), "got: {err}");
        assert!(err.contains("cluster-shaped boots"), "got: {err}");
    }

    #[test]
    fn build_server_config_allows_cluster_bootstrap_env_under_no_auth() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_TOPOLOGY", "cluster"),
            ("REDDB_NODE_ROLE", "cluster-member"),
            ("REDDB_PRESET", "production"),
            ("REDDB_USERNAME", "ops"),
            ("REDDB_PASSWORD", "hunter2"),
        ]);
        let _clear = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
        ]);
        let flags = HashMap::from([("no-auth".to_string(), bool_flag(true))]);

        let config = build_server_config(&flags, None).unwrap();

        assert_eq!(config.role, "standalone");
        assert!(config.no_auth);
        assert_eq!(
            config.storage_profile.deploy_profile,
            reddb::storage::DeployProfile::Cluster
        );
    }

    #[test]
    fn build_server_config_accepts_production_ha_storage_preset() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
            "REDDB_TOPOLOGY",
            "REDDB_NODE_ROLE",
            "REDDB_PRIMARY_ADDR",
        ]);
        let flags = HashMap::from([(
            "storage-preset".to_string(),
            str_flag("primary-replica-production-ha"),
        )]);
        let config = build_server_config(&flags, None).unwrap();
        assert_eq!(
            config.storage_profile.deploy_profile,
            reddb::storage::DeployProfile::PrimaryReplica
        );
        assert_eq!(
            config.storage_profile.packaging,
            reddb::storage::StoragePackaging::OperationalDirectory
        );
        assert_eq!(config.storage_profile.replica_count, 2);
    }

    #[test]
    fn build_server_config_rejects_primary_replica_backup_single_file() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
            "REDDB_TOPOLOGY",
            "REDDB_NODE_ROLE",
        ]);
        let flags = HashMap::from([
            ("storage-profile".to_string(), str_flag("primary-replica")),
            ("storage-packaging".to_string(), str_flag("single-file")),
            ("managed-backup".to_string(), bool_flag(true)),
        ]);
        let err = build_server_config(&flags, None).unwrap_err();
        assert!(err.contains("production primary-replica"), "got: {err}");
        assert!(err.contains("operational-directory"), "got: {err}");
    }

    #[test]
    fn build_server_config_rejects_cluster_single_file() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_STORAGE_PRESET",
            "REDDB_STORAGE_PROFILE",
            "REDDB_DEPLOY_PROFILE",
            "REDDB_STORAGE_PACKAGING",
            "REDDB_REPLICA_COUNT",
            "REDDB_MANAGED_BACKUP",
            "REDDB_WAL_RETENTION",
            "REDDB_TOPOLOGY",
            "REDDB_NODE_ROLE",
        ]);
        let flags = HashMap::from([
            ("storage-profile".to_string(), str_flag("cluster")),
            ("storage-packaging".to_string(), str_flag("single-file")),
        ]);
        let err = build_server_config(&flags, None).unwrap_err();
        assert!(err.contains("cluster"), "got: {err}");
        assert!(err.contains("embedded single-file"), "got: {err}");
    }

    #[test]
    fn parser_default_path_yields_to_docker_env_path() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_DATA_PATH", "/data/data.rdb"),
            ("REDDB_WIRE_BIND_ADDR", "0.0.0.0:5050"),
            ("REDDB_GRPC_BIND_ADDR", "0.0.0.0:55055"),
            ("REDDB_HTTP_BIND_ADDR", "0.0.0.0:5000"),
        ]);

        let args = vec!["server".to_string()];
        let tokens = cli::token::tokenize(&args);
        let parser =
            cli::schema::SchemaParser::new(cli::commands::flags_for_command(Some("server")));
        let result = parser.parse(&tokens);
        assert!(result.errors.is_empty());

        let config = build_server_config(&result.flags, None).unwrap();
        assert_eq!(
            config.path.as_deref(),
            Some(std::path::Path::new("/data/data.rdb"))
        );
        assert_eq!(config.wire_bind_addr.as_deref(), Some("0.0.0.0:5050"));
        assert_eq!(config.grpc_bind_addr.as_deref(), Some("0.0.0.0:55055"));
        assert_eq!(config.http_bind_addr.as_deref(), Some("0.0.0.0:5000"));
    }

    #[test]
    fn parser_accepts_pg_bind_and_no_log_file_server_flags() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
        ]);

        let args = vec![
            "server".to_string(),
            "--pg-bind".to_string(),
            "127.0.0.1:55432".to_string(),
            "--no-log-file".to_string(),
        ];
        let tokens = cli::token::tokenize(&args);
        let parser =
            cli::schema::SchemaParser::new(cli::commands::flags_for_command(Some("server")));
        let result = parser.parse(&tokens);
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let config = build_server_config(&result.flags, None).unwrap();
        assert_eq!(config.pg_bind_addr.as_deref(), Some("127.0.0.1:55432"));
        assert_eq!(config.router_bind_addr, None);
        assert_eq!(config.grpc_bind_addr, None);
        assert_eq!(config.http_bind_addr, None);
        assert!(
            config
                .telemetry
                .as_ref()
                .expect("telemetry config")
                .log_file_disabled
        );
    }

    #[test]
    fn parser_accepts_first_boot_server_flags() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_AUTH",
            "REDDB_REQUIRE_AUTH",
            "REDDB_VAULT",
            "REDDB_BOOTSTRAP_PRESET",
            "REDDB_PRESET",
        ]);

        let args = vec![
            "server".to_string(),
            "--auth".to_string(),
            "--require-auth".to_string(),
            "--vault".to_string(),
            "--bootstrap-preset".to_string(),
            "cloud".to_string(),
            "--cloud-head-admin".to_string(),
            "head".to_string(),
            "--cloud-head-admin-password".to_string(),
            "head-pass".to_string(),
            "--customer-admin".to_string(),
            "customer".to_string(),
            "--customer-admin-password".to_string(),
            "customer-pass".to_string(),
        ];
        let tokens = cli::token::tokenize(&args);
        let parser =
            cli::schema::SchemaParser::new(cli::commands::flags_for_command(Some("server")));
        let result = parser.parse(&tokens);
        assert!(result.errors.is_empty(), "{:?}", result.errors);

        let config = build_server_config(&result.flags, None).unwrap();
        assert!(config.auth);
        assert!(config.require_auth);
        assert!(config.vault);
        assert_eq!(config.bootstrap.preset.as_deref(), Some("cloud"));
        assert_eq!(config.bootstrap.cloud_head_admin.as_deref(), Some("head"));
        assert_eq!(
            config.bootstrap.cloud_head_admin_password.as_deref(),
            Some("head-pass")
        );
        assert_eq!(config.bootstrap.customer_admin.as_deref(), Some("customer"));
    }

    #[test]
    fn build_systemd_service_config_defaults_to_router_on_5050() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
        ]);
        let flags = HashMap::new();
        let config = build_systemd_service_config(&flags).unwrap();
        assert_eq!(
            config.router_bind_addr.as_deref(),
            Some(reddb::service_cli::DEFAULT_ROUTER_BIND_ADDR)
        );
        assert_eq!(config.grpc_bind_addr, None);
        assert_eq!(config.http_bind_addr, None);
    }

    #[test]
    fn build_systemd_service_config_keeps_explicit_http_bind() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
            "REDDB_WIRE_BIND_ADDR",
        ]);
        let flags = HashMap::from([("http-bind".to_string(), str_flag("0.0.0.0:5000"))]);
        let config = build_systemd_service_config(&flags).unwrap();
        assert_eq!(config.router_bind_addr, None);
        assert_eq!(config.grpc_bind_addr, None);
        assert_eq!(config.http_bind_addr.as_deref(), Some("0.0.0.0:5000"));
    }

    // Prior art: the client URI parser test module.
    // ----------------------------------------------------------------

    #[test]
    fn file_uri_relative_is_resolved_to_absolute() {
        let out = canonicalize_file_uri("file://./relative.rdb").expect("canonicalizes");
        assert!(
            out.starts_with("file:///"),
            "relative target must become an absolute file:/// URI, got {out}"
        );
        assert!(
            out.ends_with("/relative.rdb"),
            "the file name must be preserved, got {out}"
        );
        // Resolved against the real cwd → matches it exactly.
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(out, format!("file://{}/relative.rdb", cwd.display()));
    }

    #[test]
    fn file_uri_bare_relative_path_is_resolved() {
        // A bare path (no scheme) is accepted and resolved the same way.
        let out = canonicalize_file_uri("./data/x.rdb").expect("canonicalizes");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(out, format!("file://{}/data/x.rdb", cwd.display()));
    }

    #[test]
    fn file_uri_absolute_is_preserved_and_normalized() {
        assert_eq!(
            canonicalize_file_uri("file:///var/lib/reddb/data.rdb").unwrap(),
            "file:///var/lib/reddb/data.rdb"
        );
        // `.` and `..` segments fold lexically without touching disk.
        assert_eq!(
            canonicalize_file_uri("file:///var/lib/../lib/./reddb/data.rdb").unwrap(),
            "file:///var/lib/reddb/data.rdb"
        );
    }

    #[test]
    fn file_uri_empty_path_is_rejected() {
        assert!(canonicalize_file_uri("file://").is_err());
    }

    // --- admin cache stats text output ---

    #[test]
    fn format_cache_stats_pretty_renders_header_and_known_fields() {
        let body = r#"{"ok":true,"hits":10,"misses":2,"entries":5,"bytes_in_use":1024}"#;
        let out = format_cache_stats_pretty(body);
        assert!(out.contains("Metric"), "missing header row");
        assert!(out.contains("Value"), "missing header row");
        assert!(out.contains("Hits"), "missing Hits row");
        assert!(out.contains("10"), "missing hits value");
        assert!(out.contains("Misses"), "missing Misses row");
        assert!(out.contains("2"), "missing misses value");
        assert!(out.contains("Entries"), "missing Entries row");
        assert!(out.contains("L1 bytes in use"), "missing bytes_in_use row");
        // fields absent from JSON are silently omitted
        assert!(!out.contains("Evictions"), "unexpected Evictions row");
    }

    #[test]
    fn format_cache_stats_pretty_falls_back_to_raw_on_invalid_json() {
        let body = "not json at all";
        let out = format_cache_stats_pretty(body);
        assert_eq!(out.trim(), "not json at all");
    }

    #[test]
    fn format_cache_stats_pretty_renders_separator_line() {
        let body = r#"{"hits":0}"#;
        let out = format_cache_stats_pretty(body);
        assert!(out.contains("--------------------------------------------------"));
    }

    #[test]
    fn format_cache_stats_pretty_aligns_metric_and_value_columns() {
        let body = r#"{"hits":10,"misses":2}"#;
        assert_eq!(
            format_cache_stats_pretty(body),
            format!(
                "{:<30} {}\n{}\n{:<30} {}\n{:<30} {}\n",
                "Metric",
                "Value",
                "-".repeat(50),
                "Hits",
                10,
                "Misses",
                2
            )
        );
    }

    // --- admin catalog output format (driver-rendered) ---

    fn sample_admin_result() -> QueryResult {
        QueryResult {
            statement: "SELECT * FROM red.collections".to_string(),
            affected: 0,
            columns: vec![
                "name".to_string(),
                "model".to_string(),
                "internal".to_string(),
            ],
            rows: vec![
                admin_row("users", "table", false),
                admin_row("red.collections", "table", true),
            ],
            notice: None,
        }
    }

    fn admin_row(name: &str, model: &str, internal: bool) -> Vec<(String, reddb_client::ValueOut)> {
        vec![
            (
                "name".to_string(),
                reddb_client::ValueOut::String(name.to_string()),
            ),
            (
                "model".to_string(),
                reddb_client::ValueOut::String(model.to_string()),
            ),
            (
                "internal".to_string(),
                reddb_client::ValueOut::Bool(internal),
            ),
        ]
    }

    fn rendered(result: &QueryResult, format: RowFormat) -> String {
        String::from_utf8(format_query_result(result, format)).expect("utf-8 output")
    }

    #[test]
    fn admin_table_renders_aligned_plain_columns() {
        let out = rendered(&sample_admin_result(), RowFormat::Table);
        assert_eq!(
            out,
            concat!(
                "name             model  internal\n",
                "---------------  -----  --------\n",
                "users            table  false\n",
                "red.collections  table  true\n",
            )
        );
        assert!(!out.contains('\u{1b}'), "admin table must not emit ANSI");
    }

    #[test]
    fn admin_rows_json_outputs_bare_array_for_jq() {
        let out = rendered(&sample_admin_result(), RowFormat::Json);
        assert!(out.starts_with('['));
        assert!(out.contains(r#""name":"users""#));
        assert!(!out.contains(r#""ok""#));
    }

    #[test]
    fn admin_csv_escapes_commas_and_quotes() {
        let result = QueryResult {
            statement: "SELECT * FROM red.collections".to_string(),
            affected: 0,
            columns: vec!["name".to_string(), "model".to_string()],
            rows: vec![vec![
                (
                    "name".to_string(),
                    reddb_client::ValueOut::String("weird,\"name\"".to_string()),
                ),
                (
                    "model".to_string(),
                    reddb_client::ValueOut::String("table".to_string()),
                ),
            ]],
            notice: None,
        };
        assert_eq!(
            rendered(&result, RowFormat::Csv),
            "name,model\n\"weird,\"\"name\"\"\",table\n"
        );
    }

    #[test]
    fn admin_section_csv_carries_a_leading_section_column() {
        let sectioned = with_section_column("schema", &sample_admin_result());
        assert_eq!(
            rendered(&sectioned, RowFormat::Csv),
            concat!(
                "section,name,model,internal\n",
                "schema,users,table,false\n",
                "schema,red.collections,table,true\n",
            )
        );
    }

    #[test]
    fn admin_section_csv_infers_columns_when_the_response_omits_them() {
        let mut result = sample_admin_result();
        result.columns.clear();
        let sectioned = with_section_column("stats", &result);
        assert_eq!(
            sectioned.columns,
            vec!["section", "name", "model", "internal"]
        );
    }
}
