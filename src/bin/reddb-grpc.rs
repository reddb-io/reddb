use std::env;
use std::path::PathBuf;
use std::process;

use reddb::{GrpcServerOptions, RedDBGrpcServer, RedDBOptions, StorageMode};

#[tokio::main]
async fn main() {
    if let Err(err) = run().await {
        eprintln!("reddb-grpc: {err}");
        process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = CliConfig::parse(env::args().skip(1))?;

    if config.help {
        print_usage();
        return Ok(());
    }

    let db_options = config.to_db_options();
    let grpc_options = GrpcServerOptions {
        bind_addr: config.bind_addr.clone(),
    };

    let server = RedDBGrpcServer::from_database_options(db_options, grpc_options)?;
    eprintln!("reddb gRPC listening on {}", server.options().bind_addr);
    server.serve().await?;
    Ok(())
}

#[derive(Debug, Clone)]
struct CliConfig {
    path: Option<PathBuf>,
    bind_addr: String,
    create_if_missing: bool,
    read_only: bool,
    help: bool,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            path: None,
            bind_addr: "127.0.0.1:50051".to_string(),
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
                "--help" | "-h" => config.help = true,
                "--path" => config.path = Some(PathBuf::from(next_arg(&mut args, "--path")?)),
                "--bind" => config.bind_addr = next_arg(&mut args, "--bind")?,
                "--read-only" => config.read_only = true,
                "--no-create-if-missing" => config.create_if_missing = false,
                unexpected => return Err(format!("unknown argument: {unexpected}").into()),
            }
        }

        Ok(config)
    }

    fn to_db_options(&self) -> RedDBOptions {
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
        options
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
reddb-grpc

USAGE:
  reddb-grpc [--path <file>] [--bind <addr>] [--read-only] [--no-create-if-missing]

OPTIONS:
  --path <file>              persistent database path; omit for in-memory mode
  --bind <addr>              bind address for the gRPC server (default: 127.0.0.1:50051)
  --read-only                open the database in read-only mode
  --no-create-if-missing     do not create the database file automatically
  -h, --help                 show this help
"
    );
}
