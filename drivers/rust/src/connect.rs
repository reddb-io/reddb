//! Connection-string parser. Drivers in every language share this
//! mapping (see PLAN_DRIVERS.md, "Spec do protocolo stdio").

use std::path::PathBuf;

use url::Url;

use crate::error::{ClientError, ErrorCode, Result};

/// What kind of backend the user asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `memory://` — ephemeral, in-memory backend.
    Memory,
    /// `file:///abs/path` — embedded engine on disk.
    File { path: PathBuf },
    /// `grpc://host:port` — remote tonic client (not yet implemented).
    Grpc { endpoint: String },
}

/// Parse a connection URI. Pure function, no side effects.
pub fn parse(uri: &str) -> Result<Target> {
    if uri.is_empty() {
        return Err(ClientError::new(
            ErrorCode::InvalidUri,
            "empty connection string",
        ));
    }

    // Special-case `memory://` because url::Url won't parse it without a host.
    if uri == "memory://" || uri == "memory:" {
        return Ok(Target::Memory);
    }

    // url::Url handles file:// nicely on Unix; on Windows users can pass either
    // file:///c:/foo or file://c:/foo and it should work.
    if let Some(rest) = uri.strip_prefix("file://") {
        if rest.is_empty() {
            return Err(ClientError::new(
                ErrorCode::InvalidUri,
                "file:// URI is missing a path",
            ));
        }
        return Ok(Target::File {
            path: PathBuf::from(rest),
        });
    }

    let parsed = Url::parse(uri)
        .map_err(|e| ClientError::new(ErrorCode::InvalidUri, format!("{e}: {uri}")))?;

    match parsed.scheme() {
        "grpc" => {
            let host = parsed.host_str().ok_or_else(|| {
                ClientError::new(ErrorCode::InvalidUri, "grpc:// URI is missing a host")
            })?;
            let port = parsed.port().unwrap_or(50051);
            Ok(Target::Grpc {
                endpoint: format!("http://{host}:{port}"),
            })
        }
        other => Err(ClientError::unsupported_scheme(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_memory() {
        assert_eq!(parse("memory://").unwrap(), Target::Memory);
        assert_eq!(parse("memory:").unwrap(), Target::Memory);
    }

    #[test]
    fn parses_file_with_absolute_path() {
        let target = parse("file:///var/lib/reddb/data.rdb").unwrap();
        match target {
            Target::File { path } => assert_eq!(path, PathBuf::from("/var/lib/reddb/data.rdb")),
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn parses_grpc_with_default_port() {
        let target = parse("grpc://primary.svc.cluster.local").unwrap();
        match target {
            Target::Grpc { endpoint } => {
                assert_eq!(endpoint, "http://primary.svc.cluster.local:50051")
            }
            _ => panic!("expected Grpc"),
        }
    }

    #[test]
    fn parses_grpc_with_explicit_port() {
        let target = parse("grpc://primary:6000").unwrap();
        match target {
            Target::Grpc { endpoint } => assert_eq!(endpoint, "http://primary:6000"),
            _ => panic!("expected Grpc"),
        }
    }

    #[test]
    fn rejects_unknown_scheme() {
        let err = parse("mongodb://localhost").unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedScheme);
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(parse("").unwrap_err().code, ErrorCode::InvalidUri);
    }

    #[test]
    fn rejects_file_without_path() {
        assert_eq!(parse("file://").unwrap_err().code, ErrorCode::InvalidUri);
    }
}
