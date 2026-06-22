//! Connection-string parser. Thin shim that delegates to
//! [`reddb_wire::conn_string`] (the canonical, workspace-shared
//! parser) and projects its richer [`ConnectionTarget`] vocabulary
//! onto the legacy [`Target`] enum exposed by this crate.
//!
//! Why the shim: the previously-published `reddb-client` driver
//! had its own copy of the parser that mapped `red://host:port` to
//! a gRPC endpoint. The shared parser exposes a separate
//! [`reddb_wire::ConnectionTarget::RedWire`] variant; keeping the
//! shim preserves the existing public API surface so downstream
//! `reddb_client::connect::Target` users keep compiling without
//! changes. Direct callers can opt into the richer vocabulary by
//! depending on `reddb-wire` and using `reddb_wire::parse` instead.

use std::path::PathBuf;

use reddb_wire::{parse as wire_parse, ConnectionTarget, ParseErrorKind};

use crate::error::{ClientError, ErrorCode, Result};

/// What kind of backend the user asked for.
///
/// Note: `red://` and `reds://` URIs are folded onto
/// [`Target::Grpc`] / [`Target::GrpcCluster`] for backwards
/// compatibility with the previous driver release. New code that
/// wants the RedWire variant explicitly should depend on
/// `reddb-wire` directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `memory://` — ephemeral, in-memory backend.
    Memory,
    /// `file:///abs/path` — embedded engine on disk.
    File { path: PathBuf },
    /// `grpc://host:port` — single-host remote tonic client.
    /// Also produced by `red://host:port` and `reds://host:port`
    /// for back-compat with the previous driver behaviour.
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
    let target = wire_parse(uri).map_err(|e| match e.kind {
        ParseErrorKind::Empty => ClientError::new(ErrorCode::InvalidUri, e.message),
        ParseErrorKind::InvalidUri => ClientError::new(ErrorCode::InvalidUri, e.message),
        ParseErrorKind::UnsupportedScheme => {
            // `e.message` is `"unsupported scheme: <scheme>"`; fall
            // back to the helper for the canonical wording.
            let scheme = e
                .message
                .strip_prefix("unsupported scheme: ")
                .unwrap_or(&e.message);
            ClientError::unsupported_scheme(scheme)
        }
        ParseErrorKind::LimitExceeded => {
            // DoS guardrails added in #90 (max URI bytes, max query
            // params, max cluster hosts). Surface as InvalidUri with
            // the structured message intact.
            ClientError::new(ErrorCode::InvalidUri, e.message)
        }
    })?;
    Ok(map_target(target))
}

fn map_target(t: ConnectionTarget) -> Target {
    match t {
        ConnectionTarget::Memory => Target::Memory,
        ConnectionTarget::File { path } => Target::File { path },
        ConnectionTarget::Grpc { endpoint } => Target::Grpc { endpoint },
        ConnectionTarget::GrpcCluster {
            primary,
            replicas,
            force_primary,
        } => Target::GrpcCluster {
            primary,
            replicas,
            force_primary,
        },
        ConnectionTarget::Http { base_url } => Target::Http { base_url },
        // Back-compat: previous driver routed `red://` / `reds://`
        // through the gRPC endpoint, not a separate RedWire path.
        // Preserve that mapping until downstream code migrates.
        ConnectionTarget::RedWire { host, port, tls } => {
            let _ = tls;
            Target::Grpc {
                endpoint: format!("http://{host}:{port}"),
            }
        }
        // `red+wss://` / `red+ws://` is a browser-native WS transport
        // (ADR 0047). The legacy client has no WS transport, so fold onto
        // the HTTP base URL — callers that need the WS path should use
        // `reddb_wire::parse` and the `WsNative` variant directly.
        ConnectionTarget::WsNative { host, port, tls } => {
            let scheme = if tls { "https" } else { "http" };
            Target::Http {
                base_url: format!("{scheme}://{host}:{port}"),
            }
        }
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
                assert_eq!(endpoint, "http://primary.svc.cluster.local:55055")
            }
            _ => panic!("expected Grpc"),
        }
    }

    #[test]
    fn parses_grpcs_with_default_tls_port() {
        let target = parse("grpcs://primary.svc.cluster.local").unwrap();
        match target {
            Target::Grpc { endpoint } => {
                assert_eq!(endpoint, "http://primary.svc.cluster.local:55555")
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
            _ => panic!("expected Grpc (back-compat for red://)"),
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
        let target = parse("grpc://primary:55055,replica1:55055,replica2:55055").unwrap();
        match target {
            Target::GrpcCluster {
                primary,
                replicas,
                force_primary,
            } => {
                assert_eq!(primary, "http://primary:55055");
                assert_eq!(
                    replicas,
                    vec!["http://replica1:55055", "http://replica2:55055"]
                );
                assert!(!force_primary);
            }
            other => panic!("expected GrpcCluster, got {other:?}"),
        }
    }

    #[test]
    fn cluster_inherits_default_port_per_scheme() {
        match parse("grpc://a,b").unwrap() {
            Target::GrpcCluster {
                primary, replicas, ..
            } => {
                assert_eq!(primary, "http://a:55055");
                assert_eq!(replicas, vec!["http://b:55055"]);
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
                assert_eq!(replicas, vec!["http://b:7001", "http://c:55055"]);
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
                assert_eq!(primary, "http://primary:55055");
                assert_eq!(replicas, vec!["http://replica:55055"]);
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
            parse("grpc://a:nope,b:55055").unwrap_err().code,
            ErrorCode::InvalidUri
        );
    }

    #[test]
    fn single_host_grpc_still_routes_to_grpc_target_not_cluster() {
        match parse("grpc://primary:55055").unwrap() {
            Target::Grpc { endpoint } => assert_eq!(endpoint, "http://primary:55055"),
            other => panic!("expected Grpc (single host), got {other:?}"),
        }
    }
}
