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
                let key = format!("red.routing.{protocol}.advertise_addr");
                let legacy = match *protocol {
                    "grpc" => self.config_string("red.grpc.advertise_addr", ""),
                    "red" => self.config_string("red.redwire.advertise_addr", ""),
                    _ => String::new(),
                };
                let address = self.config_string(&key, &legacy);
                routing_endpoint(protocol, &address)
                    .map(|address| (protocol.to_string(), address))
            })
            .collect()
    }

    pub(crate) fn client_topology(&self, protocol: &str, auth: &AuthResult) -> Topology {
        let db = self.db();
        let is_replica = matches!(db.options().replication.role, ReplicationRole::Replica { .. });
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
            return Topology { epoch: 0, primary, replicas: Vec::new() };
        };
        let mut topology = TopologyAdvertiser::advertise(
            &replication.replica_snapshots(),
            auth,
            replication.topology_epoch(),
            primary,
            replication.wal_buffer.current_lsn(),
            &LagConfig::from_now(),
        );
        let addresses = replication.routing_endpoints.read()
            .unwrap_or_else(|error| error.into_inner());
        topology.replicas.retain_mut(|replica| {
            let Some(address) = addresses.get(&replica.addr).and_then(|node| node.get(protocol)) else {
                return false;
            };
            replica.addr.clone_from(address);
            true
        });
        topology
    }

    pub(crate) fn register_routing_endpoints(
        &self,
        replica_id: &str,
        endpoints: RoutingEndpoints,
    ) {
        let db = self.db();
        let Some(replication) = db.replication.as_ref() else { return; };
        let mut addresses = replication.routing_endpoints.write()
            .unwrap_or_else(|error| error.into_inner());
        if addresses.get(replica_id) != Some(&endpoints) {
            addresses.insert(replica_id.to_string(), endpoints);
            replication.bump_topology_epoch();
        }
    }

    pub(crate) fn cache_routing_primary_endpoints(&self, payload: &[u8]) {
        if let Some(endpoints) =
            reddb_wire::routing::routing_payload_endpoints(payload, "primary_endpoints")
        {
            *self.inner.routing_primary_endpoints.write() = endpoints;
        }
    }
}
