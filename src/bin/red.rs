/// `red` -- RedDB unified CLI binary.
///
/// Parses argv using the schema-driven CLI parser, routes to the
/// appropriate command, and dispatches execution.
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use reddb::cli;
use reddb::cli::types::FlagValue;
use reddb::service_cli::{
    install_systemd_service, probe_listener, render_systemd_unit, run_server_with_large_stack,
    ServerCommandConfig, ServerTransport, SystemdServiceConfig,
};

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
            reddb::RedDBRuntime::with_options(reddb::api::RedDBOptions::persistent(&path))
                .map_err(|e| format!("open {path}: {e}"))
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

/// Convert a RedDB `Value` to a minimal JSON fragment. Numbers and
/// booleans come out unquoted; everything else is a JSON string.
/// `Value::Password` / `Value::Secret` are intentionally rendered as
/// the masked `"***"` placeholder.
fn value_to_json_fragment(value: &reddb::storage::schema::Value) -> String {
    use reddb::storage::schema::Value;
    match value {
        Value::Null => "null".to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Integer(n) => n.to_string(),
        Value::UnsignedInteger(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::TimestampMs(n) | Value::Timestamp(n) | Value::Duration(n) | Value::Decimal(n) => {
            n.to_string()
        }
        Value::Password(_) | Value::Secret(_) => "\"***\"".to_string(),
        Value::Text(s) => format!("\"{}\"", json_escape(s.as_ref())),
        Value::Email(s) | Value::Url(s) | Value::NodeRef(s) | Value::EdgeRef(s) => {
            format!("\"{}\"", json_escape(s))
        }
        other => format!("\"{}\"", json_escape(&format!("{other}"))),
    }
}

/// Render a `RuntimeQueryResult` as a compact human-readable string.
/// Format: one row per line, `key=value` pairs joined with spaces,
/// plus a trailing stats line. Value::Password / Value::Secret rely
/// on the `Display` impl which already masks them as `***`.
fn format_result_pretty(result: &reddb::runtime::RuntimeQueryResult) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    if result.statement_type != "select" {
        let _ = writeln!(
            out,
            "{} ok ({} row{} affected)",
            result.statement_type,
            result.affected_rows,
            if result.affected_rows == 1 { "" } else { "s" },
        );
        return out;
    }
    if result.result.records.is_empty() {
        out.push_str("(no rows)\n");
        return out;
    }
    for (i, record) in result.result.records.iter().enumerate() {
        let mut entries: Vec<(&str, &reddb::storage::schema::Value)> =
            record.iter_fields().map(|(k, v)| (k.as_ref(), v)).collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let mut line = format!("{}.", i + 1);
        for (key, value) in entries {
            let _ = write!(line, " {key}={value}");
        }
        out.push_str(&line);
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "({} row{})",
        result.result.records.len(),
        if result.result.records.len() == 1 {
            ""
        } else {
            "s"
        }
    );
    out
}

/// Render a `RuntimeQueryResult` as a JSON object with a `rows`
/// array. Values are emitted as proper JSON scalars via
/// [`value_to_json_fragment`], which masks Password and Secret
/// columns as `"***"`.
fn format_result_json(result: &reddb::runtime::RuntimeQueryResult) -> String {
    use std::fmt::Write;
    let mut out = String::from("{\"statement\":\"");
    out.push_str(result.statement_type);
    out.push_str("\",\"affected\":");
    let _ = write!(out, "{}", result.affected_rows);
    out.push_str(",\"rows\":[");
    for (i, record) in result.result.records.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('{');
        let mut entries: Vec<(&str, &reddb::storage::schema::Value)> =
            record.iter_fields().map(|(k, v)| (k.as_ref(), v)).collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (j, (key, value)) in entries.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                "\"{}\":{}",
                json_escape(key),
                value_to_json_fragment(value)
            );
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

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
    let flags = build_flags_for_command(command.as_deref());

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
            if let Err(err) = run_server_with_large_stack(config) {
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
                    let help = "Usage: red service <install|print-unit> [flags]\n\nExamples:\n  sudo red service install --binary /usr/local/bin/red --bind 0.0.0.0:5050 --path /var/lib/reddb/data.rdb\n  red service print-unit --http --path /var/lib/reddb/data.rdb --bind 127.0.0.1:8080\n";
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
            if let Err(err) = run_server_with_large_stack(config) {
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

        "query" => {
            let json_mode = wants_json(&result.flags);
            let sql = remaining.first().map(|s| s.as_str()).unwrap_or("");
            if sql.is_empty() {
                if json_mode {
                    json_error("query", "Usage: red query [--path file] <sql>");
                }
                eprintln!("Usage: red query [--path file] <sql>");
                eprintln!("Example: red query \"SELECT * FROM users\"");
                std::process::exit(1);
            }
            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("query", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            match rt.execute_query(sql) {
                Ok(qr) => {
                    checkpoint_local_runtime(&rt);
                    if json_mode {
                        json_ok("query", &format_result_json(&qr));
                    } else {
                        print!("{}", format_result_pretty(&qr));
                    }
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

        "insert" => {
            let json_mode = wants_json(&result.flags);
            if remaining.len() < 2 {
                if json_mode {
                    json_error(
                        "insert",
                        "Usage: red insert [--path file] <collection> <json>",
                    );
                }
                eprintln!("Usage: red insert [--path file] <collection> <json>");
                eprintln!("Example: red insert users '{{\"name\": \"Alice\"}}'");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let json_data = &remaining[1];
            let parsed: reddb::json::Value =
                reddb::json::from_str(json_data).unwrap_or_else(|err| {
                    if json_mode {
                        json_error("insert", &format!("invalid JSON: {err}"));
                    }
                    eprintln!("invalid JSON: {err}");
                    std::process::exit(1);
                });
            let object = match parsed {
                reddb::json::Value::Object(map) => map,
                _ => {
                    if json_mode {
                        json_error("insert", "expected a JSON object");
                    }
                    eprintln!("expected a JSON object");
                    std::process::exit(1);
                }
            };
            // Build INSERT INTO <collection> (cols) VALUES (vals)
            let mut cols = Vec::new();
            let mut vals = Vec::new();
            for (k, v) in object.iter() {
                cols.push(k.clone());
                vals.push(match v {
                    reddb::json::Value::String(s) => format!("'{}'", s.replace('\'', "''")),
                    reddb::json::Value::Number(n) => n.to_string(),
                    reddb::json::Value::Bool(b) => b.to_string(),
                    reddb::json::Value::Null => "NULL".to_string(),
                    other => format!("'{}'", other.to_string().replace('\'', "''")),
                });
            }
            let sql = format!(
                "INSERT INTO {collection} ({}) VALUES ({})",
                cols.join(", "),
                vals.join(", "),
            );
            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("insert", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            match rt.execute_query(&sql) {
                Ok(qr) => {
                    checkpoint_local_runtime(&rt);
                    if json_mode {
                        json_ok("insert", &format_result_json(&qr));
                    } else {
                        print!("{}", format_result_pretty(&qr));
                    }
                }
                Err(err) => {
                    if json_mode {
                        json_error("insert", &err.to_string());
                    }
                    eprintln!("insert error: {err}");
                    std::process::exit(1);
                }
            }
        }

        "get" => {
            let json_mode = wants_json(&result.flags);
            if remaining.len() < 2 {
                if json_mode {
                    json_error("get", "Usage: red get [--path file] <collection> <id>");
                }
                eprintln!("Usage: red get [--path file] <collection> <id>");
                eprintln!("Example: red get users 42");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let id = &remaining[1];
            let sql = format!("SELECT * FROM {collection} WHERE _entity_id = {id}");
            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("get", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            match rt.execute_query(&sql) {
                Ok(qr) => {
                    if json_mode {
                        json_ok("get", &format_result_json(&qr));
                    } else {
                        print!("{}", format_result_pretty(&qr));
                    }
                }
                Err(err) => {
                    if json_mode {
                        json_error("get", &err.to_string());
                    }
                    eprintln!("get error: {err}");
                    std::process::exit(1);
                }
            }
        }

        "delete" => {
            let json_mode = wants_json(&result.flags);
            if remaining.len() < 2 {
                if json_mode {
                    json_error(
                        "delete",
                        "Usage: red delete [--path file] <collection> <id>",
                    );
                }
                eprintln!("Usage: red delete [--path file] <collection> <id>");
                eprintln!("Example: red delete users 42");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let id = &remaining[1];
            let sql = format!("DELETE FROM {collection} WHERE _entity_id = {id}");
            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("delete", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            match rt.execute_query(&sql) {
                Ok(qr) => {
                    checkpoint_local_runtime(&rt);
                    if json_mode {
                        json_ok("delete", &format_result_json(&qr));
                    } else {
                        print!("{}", format_result_pretty(&qr));
                    }
                }
                Err(err) => {
                    if json_mode {
                        json_error("delete", &err.to_string());
                    }
                    eprintln!("delete error: {err}");
                    std::process::exit(1);
                }
            }
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
                flag_string(&result.flags, "bind").unwrap_or_else(|| "127.0.0.1:8080".to_string());
            let operations = flag_string(&result.flags, "operations");
            let dry_run = flag_bool(&result.flags, "dry-run");

            let payload = build_tick_payload(operations.as_deref(), dry_run);
            let body = post_json_to_http(&bind_addr, "/tick", &payload).unwrap_or_else(|err| {
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
            if json_mode {
                json_ok(
                    "status",
                    &format!(
                        "{{\"uptime_ms\":{},\"collections\":{},\"entities\":{},\"pid\":{}}}",
                        uptime_ms,
                        stats.store.collection_count,
                        stats.store.total_entities,
                        stats.system.pid,
                    ),
                );
            } else {
                println!("uptime_ms:   {}", uptime_ms);
                println!("collections: {}", stats.store.collection_count);
                println!("entities:    {}", stats.store.total_entities);
                println!("pid:         {}", stats.system.pid);
                println!("hostname:    {}", stats.system.hostname);
                println!("os/arch:     {}/{}", stats.system.os, stats.system.arch);
            }
        }

        "mcp" => {
            let path = result
                .flags
                .get("path")
                .map(|v| v.as_str_value())
                .unwrap_or_default();
            let runtime = if path.is_empty() {
                reddb::runtime::RedDBRuntime::in_memory().unwrap()
            } else {
                reddb::runtime::RedDBRuntime::with_options(reddb::api::RedDBOptions::persistent(
                    &path,
                ))
                .unwrap()
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
            // line. `restore` reads the same format back. When --collection
            // is not provided, every collection in the database is dumped.
            let json_mode = wants_json(&result.flags);
            let rt = open_local_runtime(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("dump", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let store = rt.db().store();
            let targets: Vec<String> = match flag_string(&result.flags, "collection") {
                Some(name) if !name.is_empty() => vec![name],
                _ => store.list_collections(),
            };
            let output_path = flag_string(&result.flags, "output");

            let mut buf = String::new();
            let mut total_rows = 0usize;
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
                                "{{\"path\":\"{}\",\"rows\":{},\"collections\":{}}}",
                                path,
                                total_rows,
                                targets.len()
                            ),
                        );
                    } else {
                        println!(
                            "dumped {} rows from {} collection(s) to {}",
                            total_rows,
                            targets.len(),
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
                                "{{\"rows\":{},\"collections\":{}}}",
                                total_rows,
                                targets.len()
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
                let (collection, fields) = match &parsed {
                    reddb::json::Value::Object(map) => {
                        let coll = override_collection.clone().or_else(|| {
                            map.get("collection")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string())
                        });
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
                    &format!("{{\"restored\":{},\"errors\":{}}}", restored, errors),
                );
            } else {
                println!("restored {} rows ({} errors)", restored, errors);
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
            run_vcs_command(&result.flags, &remaining);
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
        _ => Err(format!(
            "Unknown vcs subcommand `{subcommand}`\n\n\
             Usage: red vcs <commit|branch|branches|tag|tags|checkout|merge|log|status|lca|resolve> [args] [flags]\n"
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

/// Build the flag schema for a given command, merging global + command-specific flags.
fn build_flags_for_command(command: Option<&str>) -> Vec<cli::types::FlagSchema> {
    let mut flags = cli::types::global_flags();

    match command {
        Some("server") => {
            flags.extend(vec![
                cli::types::FlagSchema::new("path")
                    .with_short('d')
                    .with_description("Persistent database file path (omit for in-memory)")
                    .with_default("./data/reddb.rdb"),
                cli::types::FlagSchema::new("bind")
                    .with_short('b')
                    .with_description("Bind address (host:port) for the default routed front-door or legacy single-transport mode"),
                cli::types::FlagSchema::boolean("grpc").with_description("Enable the gRPC API"),
                cli::types::FlagSchema::boolean("http").with_description("Serve the HTTP API"),
                cli::types::FlagSchema::new("grpc-bind")
                    .with_description("Explicit gRPC bind address (host:port)"),
                cli::types::FlagSchema::new("http-bind")
                    .with_description("Explicit HTTP bind address (host:port)"),
                cli::types::FlagSchema::new("wire-bind")
                    .with_description("Wire protocol TCP bind address (host:port)"),
                cli::types::FlagSchema::new("wire-tls-bind")
                    .with_description("Wire protocol TLS bind address (host:port)"),
                cli::types::FlagSchema::new("wire-tls-cert")
                    .with_description("Path to TLS certificate PEM (auto-generated if omitted)"),
                cli::types::FlagSchema::new("wire-tls-key")
                    .with_description("Path to TLS private key PEM"),
                cli::types::FlagSchema::new("role")
                    .with_short('r')
                    .with_description("Server role")
                    .with_choices(&["standalone", "primary", "replica"])
                    .with_default("standalone"),
                cli::types::FlagSchema::new("primary-addr")
                    .with_description("Primary gRPC address for replica mode"),
                cli::types::FlagSchema::boolean("read-only")
                    .with_description("Open the database in read-only mode"),
                cli::types::FlagSchema::boolean("no-create-if-missing")
                    .with_description("Fail instead of creating the database file"),
                cli::types::FlagSchema::boolean("vault")
                    .with_description("Enable the encrypted auth vault"),
                cli::types::FlagSchema::new("workers")
                    .with_short('w')
                    .with_description("Worker thread count (default: auto-detect from CPUs)"),
            ]);
        }
        Some("replica") => {
            flags.extend(vec![
                cli::types::FlagSchema::new("path")
                    .with_short('d')
                    .with_description("Persistent database file path for the replica")
                    .with_default("./data/reddb.rdb"),
                cli::types::FlagSchema::new("bind")
                    .with_short('b')
                    .with_description("Bind address (host:port) for the default routed front-door or legacy single-transport mode"),
                cli::types::FlagSchema::new("primary-addr")
                    .with_short('p')
                    .with_description("Primary gRPC address for replication"),
                cli::types::FlagSchema::boolean("grpc").with_description("Enable the gRPC API"),
                cli::types::FlagSchema::boolean("http").with_description("Serve the HTTP API"),
                cli::types::FlagSchema::new("grpc-bind")
                    .with_description("Explicit gRPC bind address (host:port)"),
                cli::types::FlagSchema::new("http-bind")
                    .with_description("Explicit HTTP bind address (host:port)"),
                cli::types::FlagSchema::new("wire-bind")
                    .with_description("Wire protocol TCP bind address (host:port)"),
                cli::types::FlagSchema::boolean("vault")
                    .with_description("Enable the encrypted auth vault"),
            ]);
        }
        Some("service") => {
            flags.extend(vec![
                cli::types::FlagSchema::new("binary")
                    .with_description("Path to the red binary")
                    .with_default("/usr/local/bin/red"),
                cli::types::FlagSchema::new("service-name")
                    .with_description("systemd unit name")
                    .with_default("reddb"),
                cli::types::FlagSchema::new("user")
                    .with_description("Service user")
                    .with_default("reddb"),
                cli::types::FlagSchema::new("group")
                    .with_description("Service group")
                    .with_default("reddb"),
                cli::types::FlagSchema::new("path")
                    .with_short('d')
                    .with_description("Persistent database file path")
                    .with_default("/var/lib/reddb/data.rdb"),
                cli::types::FlagSchema::new("bind")
                    .with_short('b')
                    .with_description("Bind address (host:port) for the default routed front-door or legacy single-transport mode"),
                cli::types::FlagSchema::boolean("grpc")
                    .with_description("Enable the gRPC API in the service"),
                cli::types::FlagSchema::boolean("http").with_description("Install an HTTP service"),
                cli::types::FlagSchema::new("grpc-bind")
                    .with_description("Explicit gRPC bind address (host:port)"),
                cli::types::FlagSchema::new("http-bind")
                    .with_description("Explicit HTTP bind address (host:port)"),
            ]);
        }
        Some("mcp") => {
            flags.push(
                cli::types::FlagSchema::new("path")
                    .with_short('d')
                    .with_description("Data directory path (omit for in-memory)")
                    .with_default(""),
            );
        }
        Some("query") | Some("insert") | Some("get") | Some("delete") | Some("status") => {
            flags.push(
                cli::types::FlagSchema::new("bind")
                    .with_short('b')
                    .with_description("Server address")
                    .with_default("0.0.0.0:6380"),
            );
            flags.push(
                cli::types::FlagSchema::new("path")
                    .with_short('p')
                    .with_description("Open a local .rdb file in embedded mode"),
            );
        }
        Some("health") => {
            flags.extend(vec![
                cli::types::FlagSchema::new("bind")
                    .with_short('b')
                    .with_description("Server bind address; defaults to the router on 127.0.0.1:5050 when no transport is selected"),
                cli::types::FlagSchema::boolean("grpc")
                    .with_description("Probe a gRPC listener (default transport)"),
                cli::types::FlagSchema::boolean("http").with_description("Probe an HTTP listener"),
            ]);
        }
        Some("tick") => {
            flags.extend(vec![
                cli::types::FlagSchema::new("bind")
                    .with_short('b')
                    .with_description("Server HTTP bind address")
                    .with_default("127.0.0.1:8080"),
                cli::types::FlagSchema::new("operations").with_description(
                    "Comma-separated operations: maintenance,retention,checkpoint",
                ),
                cli::types::FlagSchema::boolean("dry-run")
                    .with_description("Validate operations without applying"),
            ]);
        }
        Some("rpc") => {
            flags.extend(vec![
                cli::types::FlagSchema::boolean("stdio")
                    .with_description("Speak JSON-RPC 2.0 line-delimited over stdin/stdout"),
                cli::types::FlagSchema::new("path")
                    .with_short('d')
                    .with_description("Persistent database file path (omit for in-memory)"),
                cli::types::FlagSchema::new("connect")
                    .with_short('c')
                    .with_description("Proxy to a remote gRPC server (e.g. grpc://host:50051)"),
                cli::types::FlagSchema::new("token")
                    .with_short('t')
                    .with_description("Auth token forwarded to the remote server"),
            ]);
        }
        Some("connect") => {
            flags.extend(vec![
                cli::types::FlagSchema::new("token")
                    .with_short('t')
                    .with_description("Auth token (session or API key)"),
                cli::types::FlagSchema::new("query")
                    .with_short('q')
                    .with_description("Execute a single query and exit"),
                cli::types::FlagSchema::new("user")
                    .with_short('u')
                    .with_description("Username for login"),
                cli::types::FlagSchema::new("password")
                    .with_short('p')
                    .with_description("Password for login"),
            ]);
        }
        _ => {}
    }

    flags
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
        ("health".to_string(), vec![]),
        ("status".to_string(), vec![]),
        ("mcp".to_string(), vec![]),
        ("connect".to_string(), vec![]),
        ("version".to_string(), vec![]),
    ]
}

fn build_server_config(
    flags: &HashMap<String, FlagValue>,
    forced_role: Option<&str>,
) -> Result<ServerCommandConfig, String> {
    let grpc_flag = flag_bool(flags, "grpc");
    let http_flag = flag_bool(flags, "http");
    let explicit_grpc_bind = flag_string(flags, "grpc-bind")
        .filter(|value| !value.is_empty())
        .or_else(|| env_string("REDDB_GRPC_BIND_ADDR"));
    let explicit_http_bind = flag_string(flags, "http-bind")
        .filter(|value| !value.is_empty())
        .or_else(|| env_string("REDDB_HTTP_BIND_ADDR"));
    let legacy_bind = flag_string(flags, "bind")
        .filter(|value| !value.is_empty())
        .or_else(|| {
            if explicit_grpc_bind.is_none() && explicit_http_bind.is_none() {
                env_string("REDDB_BIND_ADDR")
            } else {
                None
            }
        });
    let wire_bind_addr = flag_string(flags, "wire-bind").filter(|v| !v.is_empty());
    let wire_tls_bind_addr = flag_string(flags, "wire-tls-bind").filter(|v| !v.is_empty());
    let router_bind_addr = if explicit_grpc_bind.is_none()
        && explicit_http_bind.is_none()
        && wire_bind_addr.is_none()
        && wire_tls_bind_addr.is_none()
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
    let (grpc_bind_addr, http_bind_addr) = if router_bind_addr.is_some() {
        (None, None)
    } else {
        resolve_server_binds(flags)?
    };
    let path = resolve_server_path(flags).map(PathBuf::from);
    let role = forced_role
        .map(|value| value.to_string())
        .or_else(|| flag_string(flags, "role"))
        .unwrap_or_else(|| "standalone".to_string());

    let workers = flag_string(flags, "workers").and_then(|v| v.parse::<usize>().ok());

    let wire_tls_cert = flag_string(flags, "wire-tls-cert")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let wire_tls_key = flag_string(flags, "wire-tls-key")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);

    let pg_bind_addr = flag_string(flags, "pg-bind").filter(|v| !v.is_empty());

    // Phase 6 logging: assemble the TelemetryConfig from --log-* flags,
    // falling back to a path-derived default when flags are absent.
    let telemetry = build_telemetry_config(flags, path.as_deref());

    Ok(ServerCommandConfig {
        path,
        router_bind_addr,
        grpc_bind_addr,
        http_bind_addr,
        wire_bind_addr,
        wire_tls_bind_addr,
        wire_tls_cert,
        wire_tls_key,
        pg_bind_addr,
        create_if_missing: !flag_bool(flags, "no-create-if-missing"),
        read_only: flag_bool(flags, "read-only"),
        role,
        primary_addr: flag_string(flags, "primary-addr").filter(|value| !value.is_empty()),
        vault: flag_bool(flags, "vault"),
        workers,
        telemetry: Some(telemetry),
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

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
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

fn build_tick_payload(operations: Option<&str>, dry_run: bool) -> String {
    match operations {
        None => {
            format!("{{\"dry_run\":{}}}", if dry_run { "true" } else { "false" })
        }
        Some(raw) => {
            let normalized: Vec<String> = raw
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(json_escape)
                .map(|value| format!("\"{value}\""))
                .collect();

            if normalized.is_empty() {
                return format!("{{\"dry_run\":{}}}", if dry_run { "true" } else { "false" });
            }

            format!(
                "{{\"operations\":[{}],\"dry_run\":{}}}",
                normalized.join(","),
                if dry_run { "true" } else { "false" }
            )
        }
    }
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn post_json_to_http(bind_addr: &str, path: &str, payload: &str) -> Result<String, String> {
    let bind_addr = bind_addr
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let mut stream = TcpStream::connect(bind_addr)
        .map_err(|err| format!("failed to connect to {bind_addr}: {err}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| format!("failed to set read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|err| format!("failed to set write timeout: {err}"))?;

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {content_length}\r\n\
         Connection: close\r\n\
         \r\n\
         {payload}",
        path = path,
        host = bind_addr,
        content_length = payload.len(),
        payload = payload
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("failed to write request: {err}"))?;

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| format!("failed to read response: {err}"))?;

    let mut lines = response.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| "empty response from server".to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| format!("invalid response status line: {status_line}"))?;

    let body = response
        .split_once("\r\n\r\n")
        .map(|(_headers, body)| body.to_string())
        .unwrap_or_default();
    if status >= 400 {
        Err(format!("server responded with {status}: {body}"))
    } else {
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    fn bool_flag(value: bool) -> FlagValue {
        FlagValue::Bool(value)
    }

    fn str_flag(value: &str) -> FlagValue {
        FlagValue::Str(value.to_string())
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
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
    fn resolve_server_binds_defaults_to_grpc() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
        ]);
        let flags = HashMap::new();
        let (grpc_bind, http_bind) = resolve_server_binds(&flags).unwrap();
        assert_eq!(grpc_bind.as_deref(), Some("127.0.0.1:50051"));
        assert_eq!(http_bind, None);
    }

    #[test]
    fn resolve_server_binds_supports_dual_stack_defaults() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
        ]);
        let flags = HashMap::from([
            ("grpc".to_string(), bool_flag(true)),
            ("http".to_string(), bool_flag(true)),
        ]);
        let (grpc_bind, http_bind) = resolve_server_binds(&flags).unwrap();
        assert_eq!(grpc_bind.as_deref(), Some("127.0.0.1:50051"));
        assert_eq!(http_bind.as_deref(), Some("127.0.0.1:8080"));
    }

    #[test]
    fn resolve_server_binds_rejects_ambiguous_legacy_bind() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
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
            ("grpc-bind".to_string(), str_flag("0.0.0.0:50051")),
            ("http-bind".to_string(), str_flag("0.0.0.0:8080")),
        ]);
        let (grpc_bind, http_bind) = resolve_server_binds(&flags).unwrap();
        assert_eq!(grpc_bind.as_deref(), Some("0.0.0.0:50051"));
        assert_eq!(http_bind.as_deref(), Some("0.0.0.0:8080"));
    }

    #[test]
    fn build_server_config_defaults_to_router_on_5050() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
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
    fn build_server_config_maps_legacy_bind_to_router_when_no_transport_is_selected() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
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
            ("REDDB_GRPC_BIND_ADDR", "0.0.0.0:50051"),
            ("REDDB_HTTP_BIND_ADDR", "0.0.0.0:8080"),
            ("REDDB_BIND_ADDR", "0.0.0.0:50051"),
        ]);

        let flags = HashMap::from([("path".to_string(), str_flag("./data/reddb.rdb"))]);
        let config = build_server_config(&flags, None).unwrap();

        assert_eq!(
            config.path.as_deref(),
            Some(std::path::Path::new("/data/data.rdb"))
        );
        assert_eq!(config.router_bind_addr, None);
        assert_eq!(config.grpc_bind_addr.as_deref(), Some("0.0.0.0:50051"));
        assert_eq!(config.http_bind_addr.as_deref(), Some("0.0.0.0:8080"));
    }

    #[test]
    fn build_server_config_prefers_cli_flags_over_env_defaults() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_DATA_PATH", "/data/data.rdb"),
            ("REDDB_GRPC_BIND_ADDR", "0.0.0.0:50051"),
            ("REDDB_HTTP_BIND_ADDR", "0.0.0.0:8080"),
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
        assert_eq!(config.grpc_bind_addr.as_deref(), Some("0.0.0.0:50051"));
        assert_eq!(config.http_bind_addr.as_deref(), Some("127.0.0.1:18080"));
    }

    #[test]
    fn parser_default_path_yields_to_docker_env_path() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::set(&[
            ("REDDB_DATA_PATH", "/data/data.rdb"),
            ("REDDB_GRPC_BIND_ADDR", "0.0.0.0:50051"),
            ("REDDB_HTTP_BIND_ADDR", "0.0.0.0:8080"),
            ("REDDB_BIND_ADDR", "0.0.0.0:50051"),
        ]);

        let args = vec!["server".to_string()];
        let tokens = cli::token::tokenize(&args);
        let parser = cli::schema::SchemaParser::new(build_flags_for_command(Some("server")));
        let result = parser.parse(&tokens);
        assert!(result.errors.is_empty());

        let config = build_server_config(&result.flags, None).unwrap();
        assert_eq!(
            config.path.as_deref(),
            Some(std::path::Path::new("/data/data.rdb"))
        );
        assert_eq!(config.grpc_bind_addr.as_deref(), Some("0.0.0.0:50051"));
        assert_eq!(config.http_bind_addr.as_deref(), Some("0.0.0.0:8080"));
    }

    #[test]
    fn build_systemd_service_config_defaults_to_router_on_5050() {
        let _lock = env_lock().lock().unwrap();
        let _guard = EnvGuard::clear(&[
            "REDDB_BIND_ADDR",
            "REDDB_GRPC_BIND_ADDR",
            "REDDB_HTTP_BIND_ADDR",
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
        ]);
        let flags = HashMap::from([("http-bind".to_string(), str_flag("0.0.0.0:8080"))]);
        let config = build_systemd_service_config(&flags).unwrap();
        assert_eq!(config.router_bind_addr, None);
        assert_eq!(config.grpc_bind_addr, None);
        assert_eq!(config.http_bind_addr.as_deref(), Some("0.0.0.0:8080"));
    }
}
