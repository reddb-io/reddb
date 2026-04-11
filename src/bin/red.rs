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
                                "{{\"unit\":\"{}\",\"path\":\"{}\",\"grpc_bind\":{},\"http_bind\":{}}}",
                                json_escape(&unit_name),
                                json_escape(&config.unit_path().display().to_string()),
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
                    let help = "Usage: red service <install|print-unit> [flags]\n\nExamples:\n  sudo red service install --binary /usr/local/bin/red --grpc-bind 0.0.0.0:50051 --http-bind 0.0.0.0:8080 --path /var/lib/reddb/data.rdb\n  red service print-unit --http --path /var/lib/reddb/data.rdb --bind 127.0.0.1:8080\n";
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

        "query" => {
            let json_mode = wants_json(&result.flags);
            let sql = remaining.first().map(|s| s.as_str()).unwrap_or("");
            if sql.is_empty() {
                if json_mode {
                    json_error("query", "Usage: red query <sql>");
                }
                eprintln!("Usage: red query <sql>");
                eprintln!("Example: red query \"SELECT * FROM users\"");
                std::process::exit(1);
            }
            if json_mode {
                json_ok(
                    "query",
                    &format!(
                        "{{\"sql\":\"{}\",\"result\":null,\"message\":\"query execution not yet wired\"}}",
                        json_escape(sql)
                    ),
                );
            } else {
                println!("Query: {}", sql);
                // TODO: Wire to actual query execution
                eprintln!("Query execution not yet wired.");
            }
        }

        "insert" => {
            let json_mode = wants_json(&result.flags);
            if remaining.len() < 2 {
                if json_mode {
                    json_error("insert", "Usage: red insert <collection> <json>");
                }
                eprintln!("Usage: red insert <collection> <json>");
                eprintln!("Example: red insert users '{{\"name\": \"Alice\"}}'");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let json_data = &remaining[1];
            if json_mode {
                json_ok(
                    "insert",
                    &format!(
                        "{{\"collection\":\"{}\",\"document\":{},\"result\":null,\"message\":\"insert not yet wired\"}}",
                        json_escape(collection),
                        json_data
                    ),
                );
            } else {
                println!("Inserting into '{}': {}", collection, json_data);
                // TODO: Wire to actual insert
                eprintln!("Insert not yet wired.");
            }
        }

        "get" => {
            let json_mode = wants_json(&result.flags);
            if remaining.len() < 2 {
                if json_mode {
                    json_error("get", "Usage: red get <collection> <id>");
                }
                eprintln!("Usage: red get <collection> <id>");
                eprintln!("Example: red get users abc123");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let id = &remaining[1];
            if json_mode {
                json_ok(
                    "get",
                    &format!(
                        "{{\"collection\":\"{}\",\"id\":\"{}\",\"entity\":null,\"message\":\"get not yet wired\"}}",
                        json_escape(collection),
                        json_escape(id)
                    ),
                );
            } else {
                println!("Getting from '{}': {}", collection, id);
                // TODO: Wire to actual get
                eprintln!("Get not yet wired.");
            }
        }

        "delete" => {
            let json_mode = wants_json(&result.flags);
            if remaining.len() < 2 {
                if json_mode {
                    json_error("delete", "Usage: red delete <collection> <id>");
                }
                eprintln!("Usage: red delete <collection> <id>");
                eprintln!("Example: red delete users abc123");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let id = &remaining[1];
            if json_mode {
                json_ok(
                    "delete",
                    &format!(
                        "{{\"collection\":\"{}\",\"id\":\"{}\",\"deleted\":false,\"message\":\"delete not yet wired\"}}",
                        json_escape(collection),
                        json_escape(id)
                    ),
                );
            } else {
                println!("Deleting from '{}': {}", collection, id);
                // TODO: Wire to actual delete
                eprintln!("Delete not yet wired.");
            }
        }

        "health" => {
            let json_mode = wants_json(&result.flags);
            let transport = select_transport(&result.flags).unwrap_or_else(|err| {
                if json_mode {
                    json_error("health", &err);
                }
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let bind_addr = flag_string(&result.flags, "bind")
                .unwrap_or_else(|| transport.default_bind_addr().to_string());
            let ok = probe_listener(&bind_addr, Duration::from_secs(1));
            if json_mode {
                json_ok(
                    "health",
                    &format!(
                        "{{\"healthy\":{},\"transport\":\"{}\",\"address\":\"{}\"}}",
                        ok,
                        json_escape(transport.as_str()),
                        json_escape(&bind_addr)
                    ),
                );
                if !ok {
                    std::process::exit(1);
                }
            } else if ok {
                println!("ok {} {}", transport.as_str(), bind_addr);
            } else {
                eprintln!("unreachable {} {}", transport.as_str(), bind_addr);
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
            if wants_json(&result.flags) {
                json_ok(
                    "status",
                    "{\"status\":null,\"message\":\"status check not yet wired\"}",
                );
            } else {
                println!("Checking replication status...");
                // TODO: Wire to actual status
                eprintln!("Status check not yet wired.");
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
                    .with_description("Bind address (host:port) for legacy single-transport mode"),
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
                    .with_description("Bind address (host:port) for legacy single-transport mode"),
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
                    .with_description("Bind address (host:port) for legacy single-transport mode"),
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
        }
        Some("health") => {
            flags.extend(vec![
                cli::types::FlagSchema::new("bind")
                    .with_short('b')
                    .with_description("Server bind address; defaults by transport"),
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
    let (grpc_bind_addr, http_bind_addr) = resolve_server_binds(flags)?;
    let path = flag_string(flags, "path")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let role = forced_role
        .map(|value| value.to_string())
        .or_else(|| flag_string(flags, "role"))
        .unwrap_or_else(|| "standalone".to_string());

    let workers = flag_string(flags, "workers").and_then(|v| v.parse::<usize>().ok());

    let wire_bind_addr = flag_string(flags, "wire-bind").filter(|v| !v.is_empty());
    let wire_tls_bind_addr = flag_string(flags, "wire-tls-bind").filter(|v| !v.is_empty());
    let wire_tls_cert = flag_string(flags, "wire-tls-cert")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let wire_tls_key = flag_string(flags, "wire-tls-key")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);

    Ok(ServerCommandConfig {
        path,
        grpc_bind_addr,
        http_bind_addr,
        wire_bind_addr,
        wire_tls_bind_addr,
        wire_tls_cert,
        wire_tls_key,
        create_if_missing: !flag_bool(flags, "no-create-if-missing"),
        read_only: flag_bool(flags, "read-only"),
        role,
        primary_addr: flag_string(flags, "primary-addr").filter(|value| !value.is_empty()),
        vault: flag_bool(flags, "vault"),
        workers,
    })
}

fn build_systemd_service_config(
    flags: &HashMap<String, FlagValue>,
) -> Result<SystemdServiceConfig, String> {
    let (grpc_bind_addr, http_bind_addr) = resolve_server_binds(flags)?;
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
        grpc_bind_addr,
        http_bind_addr,
    })
}

fn resolve_server_binds(
    flags: &HashMap<String, FlagValue>,
) -> Result<(Option<String>, Option<String>), String> {
    let grpc = flag_bool(flags, "grpc");
    let http = flag_bool(flags, "http");
    let legacy_bind = flag_string(flags, "bind").filter(|value| !value.is_empty());
    let mut grpc_bind = flag_string(flags, "grpc-bind").filter(|value| !value.is_empty());
    let mut http_bind = flag_string(flags, "http-bind").filter(|value| !value.is_empty());

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

fn json_optional_string(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{}\"", json_escape(value)),
        None => "null".to_string(),
    }
}

fn server_command_json(command: &str, config: &ServerCommandConfig) -> String {
    format!(
        "{{\"ok\":true,\"command\":\"{}\",\"data\":{{\"grpc_bind\":{},\"http_bind\":{}}}}}",
        json_escape(command),
        json_optional_string(config.grpc_bind_addr.as_deref()),
        json_optional_string(config.http_bind_addr.as_deref()),
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

    fn bool_flag(value: bool) -> FlagValue {
        FlagValue::Bool(value)
    }

    fn str_flag(value: &str) -> FlagValue {
        FlagValue::Str(value.to_string())
    }

    #[test]
    fn resolve_server_binds_defaults_to_grpc() {
        let flags = HashMap::new();
        let (grpc_bind, http_bind) = resolve_server_binds(&flags).unwrap();
        assert_eq!(grpc_bind.as_deref(), Some("127.0.0.1:50051"));
        assert_eq!(http_bind, None);
    }

    #[test]
    fn resolve_server_binds_supports_dual_stack_defaults() {
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
}
