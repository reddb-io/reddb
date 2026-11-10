//! Protocol-specific discovery without guessing ports or weakening TLS.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::topology::Topology;

pub const ROUTING_PROTOCOLS: [&str; 8] =
    ["red", "reds", "grpc", "grpcs", "http", "https", "ws", "wss"];
pub const FEATURE_TOPOLOGY: u32 = 0x0000_0002;
pub const TOPOLOGY_TRANSPORT_METADATA_KEY: &str = "x-reddb-topology-transport";

/// URI details needed to retain proxy paths, auth and TLS options while dialing peers.
pub struct RoutingUri {
    pub protocol: String,
    pub path: String,
    pub parameters: Vec<(String, String)>,
}
impl RoutingUri {
    pub fn query_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.parameters
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
    pub fn path(&self) -> &str {
        &self.path
    }
}
pub fn routing_uri(
    uri: &str,
    target: &crate::ConnectionTarget,
) -> Result<RoutingUri, crate::ParseError> {
    let mut parsed = url::Url::parse(match target {
        crate::ConnectionTarget::GrpcCluster { primary, .. } => primary,
        _ => uri,
    })
    .map_err(|_| crate::ParseError::new(crate::ParseErrorKind::InvalidUri, "invalid remote URI"))?;
    if matches!(target, crate::ConnectionTarget::GrpcCluster { .. }) {
        parsed.set_query(
            uri.split_once('?')
                .map(|(_, query)| query.split('#').next().unwrap_or_default()),
        );
    }
    let parameters: Vec<_> = parsed
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let scheme = uri
        .trim()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let protocol = match scheme.as_str() {
        "red+ws" => "ws",
        "red+wss" => "wss",
        "red"
            if parameters
                .iter()
                .any(|(key, value)| key == "tls" && matches!(value.as_str(), "true" | "1")) =>
        {
            "reds"
        }
        other => other,
    }
    .to_owned();
    Ok(RoutingUri {
        protocol,
        path: parsed.path().to_owned(),
        parameters,
    })
}
pub fn websocket_routing_url(address: &str) -> Option<String> {
    let mut parsed = url::Url::parse(address).ok()?;
    if !matches!(parsed.scheme(), "ws" | "wss") {
        return None;
    }
    if matches!(parsed.path(), "" | "/") {
        parsed.set_path(crate::redwire::REDWIRE_WS_PATH);
    }
    Some(parsed.into())
}

pub type RoutingEndpoints = BTreeMap<String, String>;

/// Ignore unknown optional fields, but never advertise credentials or a URI
/// for a different transport. In particular, TLS routes cannot downgrade.
pub fn routing_endpoints(value: &Value) -> RoutingEndpoints {
    ROUTING_PROTOCOLS
        .iter()
        .filter_map(|protocol| {
            let address = value.get(*protocol)?.as_str()?;
            let normalized = routing_endpoint(protocol, address)?;
            Some((protocol.to_string(), normalized))
        })
        .collect()
}

pub fn routing_endpoint(protocol: &str, address: &str) -> Option<String> {
    if !ROUTING_PROTOCOLS.contains(&protocol) || address.len() > 4096 || address.is_empty() {
        return None;
    }
    let address = if address.contains("://") {
        address.to_string()
    } else {
        format!("{protocol}://{address}")
    };
    let parsed = url::Url::parse(&address).ok()?;
    if parsed.scheme() != protocol
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    Some(address.trim_end_matches('/').to_string())
}

/// HTTP carries the same membership as the binary topology RPC. Decimal
/// strings preserve epochs and LSNs in languages whose numbers are f64.
pub fn topology_json(topology: &Topology) -> Value {
    json!({
        "version": 1,
        "epoch": topology.epoch.to_string(),
        "primary": { "addr": topology.primary.addr, "region": topology.primary.region },
        "replicas": topology.replicas.iter().map(|replica| json!({
            "addr": replica.addr,
            "region": replica.region,
            "healthy": replica.healthy,
            "lag_ms": replica.lag_ms,
            "last_applied_lsn": replica.last_applied_lsn.to_string(),
            "rebootstrapping": replica.rebootstrapping,
        })).collect::<Vec<_>>(),
    })
}

pub fn topology_from_json(value: &Value) -> Option<Topology> {
    use crate::topology::{Endpoint, ReplicaInfo};
    if value.get("version")?.as_u64()? != 1 {
        return None;
    }
    let integer = |value: &Value| value.as_u64().or_else(|| value.as_str()?.parse().ok());
    let endpoint = value.get("primary")?;
    let replicas = value.get("replicas")?.as_array()?;
    if replicas.len() > 256 {
        return None;
    }
    Some(Topology {
        epoch: integer(value.get("epoch")?)?,
        primary: Endpoint {
            addr: endpoint.get("addr")?.as_str()?.to_string(),
            region: endpoint
                .get("region")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        replicas: replicas
            .iter()
            .map(|replica| {
                Some(ReplicaInfo {
                    addr: replica.get("addr")?.as_str()?.to_string(),
                    region: replica
                        .get("region")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    healthy: replica.get("healthy")?.as_bool()?,
                    lag_ms: u32::try_from(replica.get("lag_ms")?.as_u64()?).ok()?,
                    last_applied_lsn: integer(replica.get("last_applied_lsn")?)?,
                    rebootstrapping: replica
                        .get("rebootstrapping")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                })
            })
            .collect::<Option<Vec<_>>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_keeps_protocol_and_tls_boundaries() {
        let endpoints = routing_endpoints(&json!({
            "red": "node:5050",
            "http": "http://node:5000",
            "https": "http://node:5000",
            "wss": "wss://node:8443/redwire",
            "grpc": "grpc://secret:password@node:55055",
            "reds": "reds://node:5051?token=secret",
            "unknown": "anything",
        }));
        assert_eq!(endpoints.len(), 3);
        assert_eq!(endpoints["red"], "red://node:5050");
        assert_eq!(endpoints["wss"], "wss://node:8443/redwire");
    }

    #[test]
    fn topology_count_is_validated_before_allocation() {
        let mut body = vec![0; 20];
        body[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut bytes = vec![1];
        bytes.extend_from_slice(&20u32.to_le_bytes());
        bytes.extend_from_slice(&body);
        assert_eq!(
            crate::decode_topology(&bytes),
            Err(crate::TopologyError::Truncated)
        );
    }

    #[test]
    fn cluster_auth_preserves_distinct_ports_without_embedding_credentials_in_routes() {
        let spec = crate::parse_with_auth(
            "grpc://alice:password@primary:55055,replica:55056?route=primary",
        )
        .expect("cluster URI");
        assert_eq!(
            spec.auth,
            crate::ConnectionAuth::Basic {
                user: "alice".into(),
                pass: "password".into()
            }
        );
        assert_eq!(
            spec.target,
            crate::ConnectionTarget::GrpcCluster {
                primary: "http://primary:55055".into(),
                replicas: vec!["http://replica:55056".into()],
                force_primary: true,
            }
        );
    }
}
