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
    /// `grpc://host:port` — single-host remote tonic client.
    Grpc { endpoint: String },
    /// `grpc://primary:port,replica1:port,replica2:port` — primary +
    /// read-replica fleet. Writes always go to `primary`; reads
    /// round-robin across `replicas` (or to `primary` when the
    /// replica set is empty / a `?route=primary` query param is set).
    GrpcCluster {
        primary: String,
        replicas: Vec<String>,
        force_primary: bool,
    },
    /// `http://host:port` / `https://host:port` — REST client.
    Http { base_url: String },
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

    // Cluster URIs (`grpc://primary,replica1,replica2:port`) carry a
    // comma in the authority part. `url::Url` rejects those, so handle
    // the parse before the standard path. Same syntax accepted on
    // `red://` and `reds://` for parity with the single-host form.
    if let Some(cluster) = try_parse_grpc_cluster(uri)? {
        return Ok(cluster);
    }

    let parsed = Url::parse(uri)
        .map_err(|e| ClientError::new(ErrorCode::InvalidUri, format!("{e}: {uri}")))?;

    match parsed.scheme() {
        "red" | "reds" => {
            let host = parsed.host_str().ok_or_else(|| {
                ClientError::new(ErrorCode::InvalidUri, "red:// URI is missing a host")
            })?;
            let port = parsed.port().unwrap_or(5050);
            Ok(Target::Grpc {
                endpoint: format!("http://{host}:{port}"),
            })
        }
        "grpc" => {
            let host = parsed.host_str().ok_or_else(|| {
                ClientError::new(ErrorCode::InvalidUri, "grpc:// URI is missing a host")
            })?;
            let port = parsed.port().unwrap_or(5055);
            Ok(Target::Grpc {
                endpoint: format!("http://{host}:{port}"),
            })
        }
        "http" | "https" => {
            let host = parsed.host_str().ok_or_else(|| {
                ClientError::new(ErrorCode::InvalidUri, "http(s):// URI is missing a host")
            })?;
            let scheme = parsed.scheme();
            let port = parsed.port().unwrap_or(if scheme == "https" { 443 } else { 80 });
            Ok(Target::Http {
                base_url: format!("{scheme}://{host}:{port}"),
            })
        }
        other => Err(ClientError::unsupported_scheme(other)),
    }
}

/// Try to parse a multi-host gRPC URI. `None` means "this is not a
/// cluster URI — fall through to the regular single-host parser".
/// `Some(...)` means we recognised the cluster shape and either
/// produced a `Target::GrpcCluster` or rejected it with a specific
/// error.
fn try_parse_grpc_cluster(uri: &str) -> Result<Option<Target>> {
    // Accept `grpc://`, `red://`, `reds://`. The default port
    // differs (5055 vs 5050) — keep parity with the single-host
    // branches.
    let (rest, default_port) = if let Some(r) = uri.strip_prefix("grpc://") {
        (r, 5055u16)
    } else if let Some(r) = uri.strip_prefix("red://").or_else(|| uri.strip_prefix("reds://"))
    {
        (r, 5050u16)
    } else {
        return Ok(None);
    };

    // Split off optional `?query=` suffix; we honour `route=primary`
    // by setting `force_primary = true`.
    let (host_part, query_part) = match rest.find('?') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };

    if !host_part.contains(',') {
        // Not a cluster — single host, let the normal parser handle it.
        return Ok(None);
    }

    let mut endpoints: Vec<String> = Vec::new();
    for raw in host_part.split(',') {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(ClientError::new(
                ErrorCode::InvalidUri,
                "grpc cluster URI has an empty host entry",
            ));
        }
        // Each entry may carry its own `:port`. Otherwise inherit the
        // scheme default.
        let (host, port) = match raw.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p.parse().map_err(|_| {
                    ClientError::new(
                        ErrorCode::InvalidUri,
                        format!("invalid port in cluster URI: {raw}"),
                    )
                })?;
                (h, port)
            }
            None => (raw, default_port),
        };
        if host.is_empty() {
            return Err(ClientError::new(
                ErrorCode::InvalidUri,
                "grpc cluster URI has an empty host entry",
            ));
        }
        endpoints.push(format!("http://{host}:{port}"));
    }

    let force_primary = query_part
        .map(|q| {
            q.split('&').any(|kv| {
                let mut parts = kv.splitn(2, '=');
                let k = parts.next().unwrap_or("");
                let v = parts.next().unwrap_or("");
                k.eq_ignore_ascii_case("route") && v.eq_ignore_ascii_case("primary")
            })
        })
        .unwrap_or(false);

    let mut iter = endpoints.into_iter();
    let primary = iter.next().expect("split on ',' yields at least one entry");
    let replicas: Vec<String> = iter.collect();

    Ok(Some(Target::GrpcCluster {
        primary,
        replicas,
        force_primary,
    }))
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
                assert_eq!(endpoint, "http://primary.svc.cluster.local:5055")
            }
            _ => panic!("expected Grpc"),
        }
    }

    #[test]
    fn parses_red_with_default_port() {
        let target = parse("red://primary.svc.cluster.local").unwrap();
        match target {
            Target::Grpc { endpoint } => {
                assert_eq!(endpoint, "http://primary.svc.cluster.local:5050")
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

    #[test]
    fn parses_grpc_cluster_with_explicit_ports() {
        let target = parse("grpc://primary:5055,replica1:5055,replica2:5055").unwrap();
        match target {
            Target::GrpcCluster {
                primary,
                replicas,
                force_primary,
            } => {
                assert_eq!(primary, "http://primary:5055");
                assert_eq!(replicas, vec!["http://replica1:5055", "http://replica2:5055"]);
                assert!(!force_primary);
            }
            other => panic!("expected GrpcCluster, got {other:?}"),
        }
    }

    #[test]
    fn cluster_inherits_default_port_per_scheme() {
        // `grpc://` default 5055, `red://` default 5050.
        match parse("grpc://a,b").unwrap() {
            Target::GrpcCluster {
                primary, replicas, ..
            } => {
                assert_eq!(primary, "http://a:5055");
                assert_eq!(replicas, vec!["http://b:5055"]);
            }
            other => panic!("expected GrpcCluster, got {other:?}"),
        }
        match parse("red://a,b").unwrap() {
            Target::GrpcCluster {
                primary, replicas, ..
            } => {
                assert_eq!(primary, "http://a:5050");
                assert_eq!(replicas, vec!["http://b:5050"]);
            }
            other => panic!("expected GrpcCluster, got {other:?}"),
        }
    }

    #[test]
    fn cluster_per_host_port_overrides_default() {
        match parse("grpc://a:7000,b:7001,c").unwrap() {
            Target::GrpcCluster {
                primary, replicas, ..
            } => {
                assert_eq!(primary, "http://a:7000");
                assert_eq!(replicas, vec!["http://b:7001", "http://c:5055"]);
            }
            other => panic!("expected GrpcCluster, got {other:?}"),
        }
    }

    #[test]
    fn cluster_route_primary_query_param_forces_primary() {
        match parse("grpc://primary,replica?route=primary").unwrap() {
            Target::GrpcCluster {
                primary,
                replicas,
                force_primary,
            } => {
                assert_eq!(primary, "http://primary:5055");
                assert_eq!(replicas, vec!["http://replica:5055"]);
                assert!(force_primary, "?route=primary must set force_primary");
            }
            other => panic!("expected GrpcCluster, got {other:?}"),
        }
    }

    #[test]
    fn cluster_rejects_empty_host_entry() {
        assert_eq!(
            parse("grpc://primary,,replica").unwrap_err().code,
            ErrorCode::InvalidUri
        );
        assert_eq!(parse("grpc://,b").unwrap_err().code, ErrorCode::InvalidUri);
    }

    #[test]
    fn cluster_rejects_invalid_port() {
        assert_eq!(
            parse("grpc://a:nope,b:5055").unwrap_err().code,
            ErrorCode::InvalidUri
        );
    }

    #[test]
    fn single_host_grpc_still_routes_to_grpc_target_not_cluster() {
        // No comma → not a cluster. The single-host branch handles it.
        match parse("grpc://primary:5055").unwrap() {
            Target::Grpc { endpoint } => assert_eq!(endpoint, "http://primary:5055"),
            other => panic!("expected Grpc (single host), got {other:?}"),
        }
    }
}
