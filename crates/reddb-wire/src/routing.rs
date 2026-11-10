//! Protocol-specific discovery without guessing ports or weakening TLS.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::topology::Topology;

pub const ROUTING_PROTOCOLS: [&str; 8] = [
    "red", "reds", "grpc", "grpcs", "http", "https", "ws", "wss",
];

pub type RoutingEndpoints = BTreeMap<String, String>;

pub fn routing_payload(
    payload: &[u8],
    field: &str,
    endpoints: &RoutingEndpoints,
) -> Result<String, serde_json::Error> {
    let mut value: Value = serde_json::from_slice(payload)?;
    value[field] = json!(endpoints);
    serde_json::to_string(&value)
}

pub fn routing_payload_endpoints(payload: &[u8], field: &str) -> Option<RoutingEndpoints> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    value.get(field).map(routing_endpoints)
}

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
    if value.get("version")?.as_u64()? != 1 { return None; }
    let integer = |value: &Value| value.as_u64().or_else(|| value.as_str()?.parse().ok());
    let endpoint = value.get("primary")?;
    let replicas = value.get("replicas")?.as_array()?;
    if replicas.len() > 256 { return None; }
    Some(Topology {
        epoch: integer(value.get("epoch")?)?,
        primary: Endpoint {
            addr: endpoint.get("addr")?.as_str()?.to_string(),
            region: endpoint.get("region").and_then(Value::as_str).unwrap_or_default().to_string(),
        },
        replicas: replicas.iter().map(|replica| Some(ReplicaInfo {
            addr: replica.get("addr")?.as_str()?.to_string(),
            region: replica.get("region").and_then(Value::as_str).unwrap_or_default().to_string(),
            healthy: replica.get("healthy")?.as_bool()?,
            lag_ms: u32::try_from(replica.get("lag_ms")?.as_u64()?).ok()?,
            last_applied_lsn: integer(replica.get("last_applied_lsn")?)?,
            rebootstrapping: replica.get("rebootstrapping").and_then(Value::as_bool).unwrap_or(false),
        })).collect::<Option<Vec<_>>>()?,
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
}
