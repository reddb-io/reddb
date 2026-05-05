//! `red_client` — thin RedDB client binary.
//!
//! Connects to a running `red` server using the documented
//! connection-string vocabulary (`docs/clients/connection-strings.md`)
//! and rejects every embedded scheme. Memory and file modes belong
//! to the full `red` binary; this one is remote-only by design.
//!
//! Schemes accepted:
//!   - `red://host[:port]`   → RedWire / gRPC default port 5050
//!   - `reds://host[:port]`  → RedWire-over-TLS (same default port)
//!   - `grpc://host[:port]`  → gRPC plain   default port 5055
//!   - `grpcs://host[:port]` → gRPC + TLS
//!   - `http://host[:port]`  → REST plain   (not yet wired through
//!                              red_client; surfaces as a clear
//!                              "transport not implemented" error)
//!   - `https://host[:port]` → REST + TLS  (idem)
//!
//! Schemes rejected (point the user at `red`):
//!   - `memory://` / `memory:`     (in-memory embedded engine)
//!   - `file:///abs/path`          (file-backed embedded engine)
//!
//! Exit codes:
//!   - 0  success
//!   - 1  usage / argv parse error
//!   - 2  embedded scheme rejected (use `red`)
//!   - 3  connection failure
//!   - 4  query / RPC error
//!   - 5  transport not yet implemented in red_client

use std::env;
use std::process::ExitCode;

use reddb_client::http::{query_one_shot as http_query_one_shot, Auth as HttpAuth};
use reddb_client::redwire::{Auth as RedWireAuth, RedWireClient, RedWireError};
use reddb_client::{repl::run_repl, RedDBClient};
use reddb_wire::{parse, ConnectionTarget, ParseErrorKind};

const EXIT_USAGE: u8 = 1;
const EXIT_EMBEDDED_REJECTED: u8 = 2;
const EXIT_CONNECT_FAILED: u8 = 3;
const EXIT_RPC_ERROR: u8 = 4;
const EXIT_TRANSPORT_UNSUPPORTED: u8 = 5;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let parsed = match parse_argv(&args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            print_usage_to_stderr();
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let target = match resolve_target(&parsed.uri) {
        Ok(t) => t,
        Err(EndpointError::Embedded) => {
            eprintln!(
                "red_client: embedded schemes (memory:// / file://) are not supported.\n\
                 Use the full `red` binary for in-memory or file-backed engines.",
            );
            return ExitCode::from(EXIT_EMBEDDED_REJECTED);
        }
        Err(EndpointError::PgUnsupported) => {
            eprintln!(
                "red_client: PostgreSQL wire (?proto=pg) is server-side only.\n\
                 Connect with `psql` or any libpq-based client instead."
            );
            return ExitCode::from(EXIT_TRANSPORT_UNSUPPORTED);
        }
        Err(EndpointError::ParseFailed(msg)) => {
            eprintln!("red_client: {msg}");
            return ExitCode::from(EXIT_USAGE);
        }
        Err(EndpointError::ClusterUnsupported) => {
            eprintln!(
                "red_client: gRPC cluster URIs are not yet wired through red_client.\n\
                 Pass a single host:port for now."
            );
            return ExitCode::from(EXIT_TRANSPORT_UNSUPPORTED);
        }
    };

    match target {
        ResolvedTarget::Grpc(endpoint) => run_grpc(endpoint, parsed).await,
        ResolvedTarget::RedWire { host, port, tls } => {
            run_redwire(host, port, tls, parsed).await
        }
        ResolvedTarget::Http(base_url) => run_http(base_url, parsed).await,
    }
}

async fn run_http(base_url: String, parsed: ParsedArgs) -> ExitCode {
    let auth = match parsed.token.clone() {
        Some(t) => HttpAuth::Bearer(t),
        None => HttpAuth::Anonymous,
    };
    match parsed.command {
        Some(Command::OneShot(sql)) => {
            let result = tokio::task::spawn_blocking(move || {
                http_query_one_shot(&base_url, &sql, &auth)
            })
            .await;
            match result {
                Ok(Ok(body)) => {
                    println!("{body}");
                    ExitCode::SUCCESS
                }
                Ok(Err(e)) => {
                    eprintln!("red_client: {e}");
                    ExitCode::from(EXIT_RPC_ERROR)
                }
                Err(e) => {
                    eprintln!("red_client: blocking task failed: {e}");
                    ExitCode::from(EXIT_RPC_ERROR)
                }
            }
        }
        Some(Command::Repl) | None => {
            eprintln!(
                "red_client: REPL over HTTP is not yet wired; pass `-c \"<SQL>\"` for now."
            );
            ExitCode::from(EXIT_TRANSPORT_UNSUPPORTED)
        }
    }
}

async fn run_grpc(endpoint: String, parsed: ParsedArgs) -> ExitCode {
    let mut client = match RedDBClient::connect(&endpoint, parsed.token.clone()).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("red_client: failed to connect to {endpoint}: {e}");
            return ExitCode::from(EXIT_CONNECT_FAILED);
        }
    };
    match parsed.command {
        Some(Command::OneShot(sql)) => match client.query(&sql).await {
            Ok(out) => {
                println!("{out}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("red_client: {e}");
                ExitCode::from(EXIT_RPC_ERROR)
            }
        },
        Some(Command::Repl) | None => {
            run_repl(&mut client).await;
            ExitCode::SUCCESS
        }
    }
}

async fn run_redwire(host: String, port: u16, tls: bool, parsed: ParsedArgs) -> ExitCode {
    let auth = match parsed.token.clone() {
        Some(t) => RedWireAuth::Bearer(t),
        None => RedWireAuth::Anonymous,
    };
    let mut client = match RedWireClient::connect(&host, port, tls, auth).await {
        Ok(c) => c,
        Err(RedWireError::TlsNotImplemented) => {
            eprintln!("red_client: {}", RedWireError::TlsNotImplemented);
            return ExitCode::from(EXIT_TRANSPORT_UNSUPPORTED);
        }
        Err(e) => {
            eprintln!("red_client: redwire connect to {host}:{port} failed: {e}");
            return ExitCode::from(EXIT_CONNECT_FAILED);
        }
    };
    match parsed.command {
        Some(Command::OneShot(sql)) => match client.query(&sql).await {
            Ok(out) => {
                println!("{out}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("red_client: {e}");
                ExitCode::from(EXIT_RPC_ERROR)
            }
        },
        Some(Command::Repl) | None => {
            eprintln!(
                "red_client: REPL over RedWire is not yet wired; pass `-c \"<SQL>\"` for now."
            );
            ExitCode::from(EXIT_TRANSPORT_UNSUPPORTED)
        }
    }
}

#[derive(Debug)]
struct ParsedArgs {
    uri: String,
    token: Option<String>,
    command: Option<Command>,
}

#[derive(Debug)]
enum Command {
    OneShot(String),
    Repl,
}

fn parse_argv(args: &[String]) -> Result<ParsedArgs, String> {
    let mut uri: Option<String> = None;
    let mut token: Option<String> = None;
    let mut command: Option<Command> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-h" | "--help" => {
                print_usage_to_stdout();
                std::process::exit(0);
            }
            "-t" | "--token" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| format!("red_client: {a} requires a value"))?;
                token = Some(v.clone());
                i += 2;
            }
            "-c" | "--command" => {
                let v = args
                    .get(i + 1)
                    .ok_or_else(|| format!("red_client: {a} requires a SQL string"))?;
                command = Some(Command::OneShot(v.clone()));
                i += 2;
            }
            "--repl" => {
                command = Some(Command::Repl);
                i += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("red_client: unknown flag {other}"));
            }
            _ => {
                if uri.is_some() {
                    return Err(format!(
                        "red_client: unexpected positional argument: {a}"
                    ));
                }
                uri = Some(a.clone());
                i += 1;
            }
        }
    }
    let uri = uri.ok_or_else(|| "red_client: missing connection URI".to_string())?;
    Ok(ParsedArgs {
        uri,
        token,
        command,
    })
}

#[derive(Debug)]
enum EndpointError {
    Embedded,
    PgUnsupported,
    ClusterUnsupported,
    ParseFailed(String),
}

#[derive(Debug)]
enum ResolvedTarget {
    Grpc(String),
    RedWire { host: String, port: u16, tls: bool },
    Http(String),
}

fn resolve_target(uri: &str) -> Result<ResolvedTarget, EndpointError> {
    if is_embedded_uri(uri) {
        return Err(EndpointError::Embedded);
    }
    if uri.contains("?proto=pg") {
        return Err(EndpointError::PgUnsupported);
    }
    let target = parse(uri).map_err(|e| match e.kind {
        ParseErrorKind::Empty | ParseErrorKind::InvalidUri | ParseErrorKind::UnsupportedScheme => {
            EndpointError::ParseFailed(format!("{e}"))
        }
    })?;
    match target {
        ConnectionTarget::Memory | ConnectionTarget::File { .. } => Err(EndpointError::Embedded),
        ConnectionTarget::Http { base_url } => Ok(ResolvedTarget::Http(base_url)),
        ConnectionTarget::GrpcCluster { .. } => Err(EndpointError::ClusterUnsupported),
        ConnectionTarget::Grpc { endpoint } => Ok(ResolvedTarget::Grpc(endpoint)),
        ConnectionTarget::RedWire { host, port, tls } => {
            Ok(ResolvedTarget::RedWire { host, port, tls })
        }
    }
}

/// Heuristic for embedded URIs that the parser would otherwise route
/// to the gRPC branch. The doc form `red://` (no host) and the
/// SQLite-style `red://:memory:` aliases never resolve to a remote
/// target — they are explicit "use the embedded engine" requests.
fn is_embedded_uri(uri: &str) -> bool {
    let trimmed = uri.trim();
    matches!(trimmed, "red://" | "red:" | "red:///" | "red://:memory" | "red://:memory:")
        || trimmed.starts_with("red:///")
}

fn print_usage_to_stdout() {
    println!("{}", USAGE);
}

fn print_usage_to_stderr() {
    eprintln!("{}", USAGE);
}

const USAGE: &str = "\
red_client — thin RedDB client (remote-only)

USAGE:
    red_client <URI> [--token TOKEN] [--command SQL] [--repl]

EXAMPLES:
    red_client red://reddb.example.com:5050 --token sk-abc
    red_client reds://reddb.example.com:5050 --token sk-abc -c \"SELECT 1\"
    red_client grpcs://reddb.example.com:50052 --token sk-abc

ACCEPTED SCHEMES:
    red://, reds://, grpc://, grpcs://

REJECTED SCHEMES (use the full `red` binary):
    memory://, file:///path, red:///path, red://:memory:

EXIT CODES:
    0 success | 1 usage error | 2 embedded scheme rejected
    3 connect failed | 4 RPC error | 5 transport not yet implemented
";
