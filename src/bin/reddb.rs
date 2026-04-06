use std::env;
use std::io;
use std::path::PathBuf;
use std::process;

use reddb::{RedDBOptions, RedDBServer, ServerOptions, StorageMode};

fn main() {
    if let Err(err) = run() {
        eprintln!("reddb: {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = CliConfig::parse(env::args().skip(1))?;

    if config.help {
        print_usage();
        return Ok(());
    }

    let db_options = config.to_db_options()?;
    let server_options = config.to_server_options();
    let bind_addr = server_options.bind_addr.clone();

    let server = RedDBServer::from_database_options(db_options, server_options)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err.to_string()))?;

    eprintln!("reddb server listening on {bind_addr}");
    server.serve()?;
    Ok(())
}

#[derive(Debug, Clone)]
struct CliConfig {
    path: Option<PathBuf>,
    bind_addr: String,
    max_body_bytes: usize,
    read_timeout_ms: u64,
    write_timeout_ms: u64,
    max_scan_limit: usize,
    auth_token: Option<String>,
    write_token: Option<String>,
    create_if_missing: bool,
    read_only: bool,
    help: bool,
}

impl Default for CliConfig {
    fn default() -> Self {
        let server_defaults = ServerOptions::default();
        Self {
            path: None,
            bind_addr: server_defaults.bind_addr,
            max_body_bytes: server_defaults.max_body_bytes,
            read_timeout_ms: server_defaults.read_timeout_ms,
            write_timeout_ms: server_defaults.write_timeout_ms,
            max_scan_limit: server_defaults.max_scan_limit,
            auth_token: server_defaults.auth_token,
            write_token: server_defaults.write_token,
            create_if_missing: true,
            read_only: false,
            help: false,
        }
    }
}

impl CliConfig {
    fn parse<I>(args: I) -> Result<Self, Box<dyn std::error::Error>>
    where
        I: IntoIterator<Item = String>,
    {
        let mut config = Self::default();
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => {
                    config.help = true;
                }
                "--path" => {
                    let value = next_arg(&mut args, "--path")?;
                    config.path = Some(PathBuf::from(value));
                }
                "--bind" => {
                    config.bind_addr = next_arg(&mut args, "--bind")?;
                }
                "--max-body-bytes" => {
                    config.max_body_bytes =
                        next_arg(&mut args, "--max-body-bytes")?.parse()?;
                }
                "--read-timeout-ms" => {
                    config.read_timeout_ms =
                        next_arg(&mut args, "--read-timeout-ms")?.parse()?;
                }
                "--write-timeout-ms" => {
                    config.write_timeout_ms =
                        next_arg(&mut args, "--write-timeout-ms")?.parse()?;
                }
                "--max-scan-limit" => {
                    config.max_scan_limit =
                        next_arg(&mut args, "--max-scan-limit")?.parse()?;
                }
                "--auth-token" => {
                    config.auth_token = Some(next_arg(&mut args, "--auth-token")?);
                }
                "--write-token" => {
                    config.write_token = Some(next_arg(&mut args, "--write-token")?);
                }
                "--read-only" => {
                    config.read_only = true;
                }
                "--no-create-if-missing" => {
                    config.create_if_missing = false;
                }
                unexpected => {
                    return Err(format!("unknown argument: {unexpected}").into());
                }
            }
        }

        Ok(config)
    }

    fn to_db_options(&self) -> Result<RedDBOptions, Box<dyn std::error::Error>> {
        let mut options = match &self.path {
            Some(path) => RedDBOptions::persistent(path),
            None => RedDBOptions::in_memory(),
        };

        options.mode = if self.path.is_some() {
            StorageMode::Persistent
        } else {
            StorageMode::InMemory
        };
        options.create_if_missing = self.create_if_missing;
        options.read_only = self.read_only;

        Ok(options)
    }

    fn to_server_options(&self) -> ServerOptions {
        ServerOptions {
            bind_addr: self.bind_addr.clone(),
            max_body_bytes: self.max_body_bytes,
            read_timeout_ms: self.read_timeout_ms,
            write_timeout_ms: self.write_timeout_ms,
            max_scan_limit: self.max_scan_limit,
            auth_token: self.auth_token.clone(),
            write_token: self.write_token.clone(),
        }
    }
}

fn next_arg<I>(args: &mut I, flag: &str) -> Result<String, Box<dyn std::error::Error>>
where
    I: Iterator<Item = String>,
{
    args.next()
        .ok_or_else(|| format!("missing value for {flag}").into())
}

fn print_usage() {
    println!(
        "\
reddb server

USAGE:
  reddb [--path <file>] [--bind <addr>] [--read-only] [--no-create-if-missing]

OPTIONS:
  --path <file>              persistent database path; omit for in-memory mode
  --bind <addr>              bind address for the HTTP server (default: 127.0.0.1:8080)
  --max-body-bytes <bytes>   maximum request body size
  --read-timeout-ms <ms>     socket read timeout
  --write-timeout-ms <ms>    socket write timeout
  --max-scan-limit <n>       maximum page size for scan endpoints
  --auth-token <token>       require Authorization: Bearer <token> on server routes
  --write-token <token>      require a different bearer token for write routes
  --read-only                open the database in read-only mode
  --no-create-if-missing     do not create the database file automatically
  -h, --help                 show this help
"
    );
}
