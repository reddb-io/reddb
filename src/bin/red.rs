/// `red` -- RedDB unified CLI binary.
///
/// Parses argv using the schema-driven CLI parser, routes to the
/// appropriate command, and dispatches execution.
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use reddb::cli;
use reddb::cli::types::FlagValue;
use reddb::service_cli::{
    probe_listener, run_server_with_large_stack, ServerCommandConfig, ServerTransport,
};

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
    if result.flags.get("help").map_or(false, |v| v.is_truthy()) {
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
    if result.flags.get("version").map_or(false, |v| v.is_truthy()) {
        println!("reddb {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    // Check for parse errors.
    if !result.errors.is_empty() {
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
            println!("reddb {}", env!("CARGO_PKG_VERSION"));
        }

        "server" => {
            let config = build_server_config(&result.flags, None).unwrap_or_else(|err| {
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            if let Err(err) = run_server_with_large_stack(config) {
                eprintln!("red server: {err}");
                std::process::exit(1);
            }
        }

        "replica" => {
            let config =
                build_server_config(&result.flags, Some("replica")).unwrap_or_else(|err| {
                    eprintln!("error: {err}");
                    std::process::exit(1);
                });
            if let Err(err) = run_server_with_large_stack(config) {
                eprintln!("red replica: {err}");
                std::process::exit(1);
            }
        }

        "query" => {
            let sql = remaining.first().map(|s| s.as_str()).unwrap_or("");
            if sql.is_empty() {
                eprintln!("Usage: red query <sql>");
                eprintln!("Example: red query \"SELECT * FROM users\"");
                std::process::exit(1);
            }
            println!("Query: {}", sql);
            // TODO: Wire to actual query execution
            eprintln!("Query execution not yet wired.");
        }

        "insert" => {
            if remaining.len() < 2 {
                eprintln!("Usage: red insert <collection> <json>");
                eprintln!("Example: red insert users '{{\"name\": \"Alice\"}}'");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let json_data = &remaining[1];
            println!("Inserting into '{}': {}", collection, json_data);
            // TODO: Wire to actual insert
            eprintln!("Insert not yet wired.");
        }

        "get" => {
            if remaining.len() < 2 {
                eprintln!("Usage: red get <collection> <id>");
                eprintln!("Example: red get users abc123");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let id = &remaining[1];
            println!("Getting from '{}': {}", collection, id);
            // TODO: Wire to actual get
            eprintln!("Get not yet wired.");
        }

        "delete" => {
            if remaining.len() < 2 {
                eprintln!("Usage: red delete <collection> <id>");
                eprintln!("Example: red delete users abc123");
                std::process::exit(1);
            }
            let collection = &remaining[0];
            let id = &remaining[1];
            println!("Deleting from '{}': {}", collection, id);
            // TODO: Wire to actual delete
            eprintln!("Delete not yet wired.");
        }

        "health" => {
            let transport = select_transport(&result.flags).unwrap_or_else(|err| {
                eprintln!("error: {err}");
                std::process::exit(1);
            });
            let bind_addr = flag_string(&result.flags, "bind")
                .unwrap_or_else(|| transport.default_bind_addr().to_string());
            let ok = probe_listener(&bind_addr, Duration::from_secs(1));
            if ok {
                println!("ok {} {}", transport.as_str(), bind_addr);
            } else {
                eprintln!("unreachable {} {}", transport.as_str(), bind_addr);
                std::process::exit(1);
            }
        }

        "status" => {
            println!("Checking replication status...");
            // TODO: Wire to actual status
            eprintln!("Status check not yet wired.");
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
            let subcommand = result
                .positionals
                .first()
                .map(|s| s.as_str())
                .unwrap_or("help");
            let rt = reddb::RedDBRuntime::in_memory().expect("failed to create runtime");
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
                            eprintln!("error: --password is required for bootstrap");
                            std::process::exit(1);
                        });

                    match auth_store.bootstrap(&user, &password) {
                        Ok(br) => {
                            println!(
                                "Admin user '{}' created (role: {})",
                                br.user.username,
                                br.user.role.as_str()
                            );
                            println!("API Key: {}", br.api_key.key);

                            if let Some(cert) = br.certificate {
                                println!();
                                println!("CERTIFICATE (save this — required to unseal the vault):");
                                println!("  {}", cert);
                                println!();
                                println!("Without this certificate, the vault cannot be decrypted after restart.");
                            } else {
                                println!();
                                println!("Save this API key — it will not be shown again.");
                            }
                        }
                        Err(e) => {
                            eprintln!("error: {e}");
                            std::process::exit(1);
                        }
                    }
                }
                _ => {
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
                    println!("  red auth create-user --user alice --password pass --role write");
                }
            }
        }

        "connect" => {
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
                        eprintln!("Failed to connect to {}: {}", addr, e);
                        std::process::exit(1);
                    }
                };

                if let Some(query) = one_shot_query {
                    // One-shot mode: execute a single query and exit
                    match client.query(&query).await {
                        Ok(json) => println!("{}", json),
                        Err(e) => {
                            eprintln!("error: {}", e);
                            std::process::exit(1);
                        }
                    }
                } else {
                    // Interactive REPL
                    reddb::client::repl::run_repl(&mut client).await;
                }
            });
        }

        _ => {
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
                    .with_description("Bind address (host:port); defaults by transport"),
                cli::types::FlagSchema::boolean("grpc")
                    .with_description("Serve the gRPC API (default transport)"),
                cli::types::FlagSchema::boolean("http").with_description("Serve the HTTP API"),
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
                    .with_description("Bind address (host:port); defaults by transport"),
                cli::types::FlagSchema::new("primary-addr")
                    .with_short('p')
                    .with_description("Primary gRPC address for replication"),
                cli::types::FlagSchema::boolean("grpc")
                    .with_description("Serve the gRPC API (default transport)"),
                cli::types::FlagSchema::boolean("http").with_description("Serve the HTTP API"),
                cli::types::FlagSchema::boolean("vault")
                    .with_description("Enable the encrypted auth vault"),
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
fn build_completion_tree() -> Vec<(String, Vec<(String, Vec<String>)>)> {
    vec![
        ("server".to_string(), vec![]),
        ("replica".to_string(), vec![]),
        ("query".to_string(), vec![]),
        ("insert".to_string(), vec![]),
        ("get".to_string(), vec![]),
        ("delete".to_string(), vec![]),
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
    let transport = select_transport(flags)?;
    let path = flag_string(flags, "path")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let bind_addr = flag_string(flags, "bind")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| transport.default_bind_addr().to_string());
    let role = forced_role
        .map(|value| value.to_string())
        .or_else(|| flag_string(flags, "role"))
        .unwrap_or_else(|| "standalone".to_string());

    let workers = flag_string(flags, "workers").and_then(|v| v.parse::<usize>().ok());

    Ok(ServerCommandConfig {
        transport,
        path,
        bind_addr,
        create_if_missing: !flag_bool(flags, "no-create-if-missing"),
        read_only: flag_bool(flags, "read-only"),
        role,
        primary_addr: flag_string(flags, "primary-addr").filter(|value| !value.is_empty()),
        vault: flag_bool(flags, "vault"),
        workers,
    })
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
