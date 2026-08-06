//! Connection-string parser. Delegates to [`reddb_wire`], the
//! canonical workspace parser, and maps its errors onto the driver's
//! public error vocabulary.

use reddb_wire::{parse as wire_parse, ConnectionTarget, ParseErrorKind};

use crate::error::{ClientError, ErrorCode, Result};

/// Parse a connection URI. Pure function, no side effects.
pub fn parse(uri: &str) -> Result<ConnectionTarget> {
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
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_memory() {
        assert_eq!(parse("memory://").unwrap(), ConnectionTarget::Memory);
        assert_eq!(parse("memory:").unwrap(), ConnectionTarget::Memory);
    }

    #[test]
    fn parses_file_with_absolute_path() {
        let target = parse("file:///var/lib/reddb/data.rdb").unwrap();
        match target {
            ConnectionTarget::File { path } => {
                assert_eq!(path, PathBuf::from("/var/lib/reddb/data.rdb"))
            }
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn parses_grpc_with_default_port() {
        let target = parse("grpc://primary.svc.cluster.local").unwrap();
        match target {
            ConnectionTarget::Grpc { endpoint } => {
                assert_eq!(endpoint, "http://primary.svc.cluster.local:55055")
            }
            _ => panic!("expected Grpc"),
        }
    }

    #[test]
    fn parses_grpcs_with_default_tls_port() {
        let target = parse("grpcs://primary.svc.cluster.local").unwrap();
        match target {
            ConnectionTarget::Grpc { endpoint } => {
                assert_eq!(endpoint, "http://primary.svc.cluster.local:55555")
            }
            _ => panic!("expected Grpc"),
        }
    }

    #[test]
    fn parses_red_with_default_port() {
        let target = parse("red://primary.svc.cluster.local").unwrap();
        match target {
            ConnectionTarget::RedWire { host, port, tls } => {
                assert_eq!(host, "primary.svc.cluster.local");
                assert_eq!(port, 5050);
                assert!(!tls);
            }
            other => panic!("expected RedWire, got {other:?}"),
        }
    }

    #[test]
    fn parses_reds_as_tls_redwire() {
        assert_eq!(
            parse("reds://primary:5051").unwrap(),
            ConnectionTarget::RedWire {
                host: "primary".into(),
                port: 5051,
                tls: true,
            }
        );
    }

    #[test]
    fn parses_grpc_with_explicit_port() {
        let target = parse("grpc://primary:6000").unwrap();
        match target {
            ConnectionTarget::Grpc { endpoint } => assert_eq!(endpoint, "http://primary:6000"),
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
            ConnectionTarget::GrpcCluster {
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
    fn grpc_cluster_inherits_default_port() {
        match parse("grpc://a,b").unwrap() {
            ConnectionTarget::GrpcCluster {
                primary, replicas, ..
            } => {
                assert_eq!(primary, "http://a:55055");
                assert_eq!(replicas, vec!["http://b:55055"]);
            }
            other => panic!("expected GrpcCluster, got {other:?}"),
        }
    }

    #[test]
    fn red_cluster_does_not_fold_to_grpc() {
        assert_eq!(parse("red://a,b").unwrap_err().code, ErrorCode::InvalidUri);
    }

    #[test]
    fn cluster_per_host_port_overrides_default() {
        match parse("grpc://a:7000,b:7001,c").unwrap() {
            ConnectionTarget::GrpcCluster {
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
            ConnectionTarget::GrpcCluster {
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
            ConnectionTarget::Grpc { endpoint } => assert_eq!(endpoint, "http://primary:55055"),
            other => panic!("expected Grpc (single host), got {other:?}"),
        }
    }
}
