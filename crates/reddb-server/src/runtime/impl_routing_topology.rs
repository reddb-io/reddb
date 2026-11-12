//! Client discovery is a projection of replication state, never write authority.

use reddb_wire::routing::{routing_endpoint, RoutingEndpoints, ROUTING_PROTOCOLS};
use reddb_wire::topology::{Endpoint, Topology};

use super::RedDBRuntime;
use crate::auth::middleware::AuthResult;
use crate::replication::{LagConfig, ReplicationRole, TopologyAdvertiser};

impl RedDBRuntime {
    /// Operators advertise reachable addresses explicitly: listener bind
    /// addresses can be wildcard, private, or behind protocol-specific proxies.
    pub(crate) fn routing_endpoints(&self) -> RoutingEndpoints {
        ROUTING_PROTOCOLS
            .iter()
            .filter_map(|protocol| {
                let key = format!("routing.{protocol}.advertise_addr");
                let legacy = match *protocol {
                    "grpc" => self.config_string("grpc.advertise_addr", ""),
                    "red" => self.config_string("redwire.advertise_addr", ""),
                    _ => String::new(),
                };
                let address = self.config_string(&key, &legacy);
                routing_endpoint(protocol, &address).map(|address| (protocol.to_string(), address))
            })
            .collect()
    }

    pub(crate) fn client_topology(&self, protocol: &str, auth: &AuthResult) -> Topology {
        let db = self.db();
        let is_replica = matches!(
            db.options().replication.role,
            ReplicationRole::Replica { .. }
        );
        let endpoints = if is_replica {
            self.inner.routing_primary_endpoints.read().clone()
        } else {
            self.routing_endpoints()
        };
        let primary = Endpoint {
            addr: endpoints.get(protocol).cloned().unwrap_or_default(),
            region: db.options().replication.region.clone(),
        };
        let Some(replication) = db.replication.as_ref().filter(|_| !is_replica) else {
            return Topology {
                epoch: 0,
                primary,
                replicas: Vec::new(),
            };
        };
        let mut topology = TopologyAdvertiser::advertise(
            &replication.replica_snapshots(),
            auth,
            replication.topology_epoch(),
            primary,
            replication.wal_buffer.current_lsn(),
            &LagConfig::from_now(),
        );
        let addresses = replication
            .routing_endpoints
            .read()
            .expect("replica routing endpoint map lock must not be poisoned");
        topology.replicas.retain_mut(|replica| {
            let Some(address) = addresses
                .get(&replica.addr)
                .and_then(|node| node.get(protocol))
            else {
                return false;
            };
            replica.addr.clone_from(address);
            true
        });
        topology
    }

    pub(crate) fn register_routing_endpoints(&self, replica_id: &str, endpoints: RoutingEndpoints) {
        let db = self.db();
        let Some(replication) = db.replication.as_ref() else {
            return;
        };
        let mut addresses = replication
            .routing_endpoints
            .write()
            .expect("replica routing endpoint map lock must not be poisoned");
        if addresses.get(replica_id) != Some(&endpoints) {
            addresses.insert(replica_id.to_string(), endpoints);
            replication.bump_topology_epoch();
        }
    }

    pub(crate) fn cache_routing_primary_endpoints(&self, endpoints: Option<RoutingEndpoints>) {
        if let Some(endpoints) = endpoints {
            *self.inner.routing_primary_endpoints.write() = endpoints;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_registered_endpoints_for_every_protocol_and_redacts_anonymous() {
        let mut options = crate::RedDBOptions::in_memory();
        options.replication = crate::replication::ReplicationConfig::primary();
        let runtime = RedDBRuntime::with_options(options).expect("primary runtime");
        let mut configuration = crate::json::Map::new();
        let mut endpoints = RoutingEndpoints::new();
        for (index, protocol) in ROUTING_PROTOCOLS.iter().enumerate() {
            configuration.insert(
                protocol.to_string(),
                crate::json!({
                    "advertise_addr": format!("{protocol}://primary:{}", 5000 + index),
                }),
            );
            endpoints.insert(
                protocol.to_string(),
                format!("{protocol}://replica:{}", 6000 + index),
            );
        }
        runtime
            .db()
            .store()
            .set_config_tree("routing", &crate::json::Value::Object(configuration));
        runtime
            .db()
            .replication
            .as_ref()
            .expect("replication")
            .register_replica("replica-id".to_string());
        runtime.register_routing_endpoints("replica-id", endpoints.clone());
        let auth = AuthResult::password("reader", crate::auth::Role::Read);
        for protocol in ROUTING_PROTOCOLS {
            let topology = runtime.client_topology(protocol, &auth);
            assert_eq!(topology.replicas.len(), 1, "{protocol}");
            assert_eq!(topology.replicas[0].addr, endpoints[protocol]);
            assert!(topology
                .primary
                .addr
                .starts_with(&format!("{protocol}://primary:")));
            assert!(runtime
                .client_topology(protocol, &AuthResult::Anonymous)
                .replicas
                .is_empty());
        }
    }

    #[test]
    fn replica_discovery_returns_upstream_addresses_without_guessing_ports() {
        let mut options = crate::RedDBOptions::in_memory();
        options.replication =
            crate::replication::ReplicationConfig::replica("http://primary:55055".to_string());
        let runtime = RedDBRuntime::with_options(options).expect("replica runtime");
        runtime.cache_routing_primary_endpoints(Some(RoutingEndpoints::from([
            ("https".to_string(), "https://public:8443".to_string()),
            ("red".to_string(), "red://private:5050".to_string()),
        ])));
        assert_eq!(
            runtime
                .client_topology("https", &AuthResult::Anonymous)
                .primary
                .addr,
            "https://public:8443"
        );
        assert!(runtime
            .client_topology("wss", &AuthResult::Anonymous)
            .primary
            .addr
            .is_empty());
    }
}
