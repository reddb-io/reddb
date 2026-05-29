//! Built-in `red.topology.cluster` graph collection (issue #803).
//!
//! Exposes live cluster topology as a graph collection declared
//! `WITH ANALYTICS (communities, components, centrality)` and serves it through
//! `GET /v1/topology/graph`. The collection is bootstrapped once at server boot
//! (idempotent) and materialised from the live replica registry on demand.
//!
//! ## Why scoped analytics, not the `<graph>.<output>` SQL view
//!
//! The #800/#801 analytics-view resolver runs its algorithms over the *whole*
//! graph store (the documented v0 TVF limitation, #795-#800): a `SELECT FROM
//! red.topology.cluster.components` would fold in every other graph collection's
//! nodes. The topology endpoint instead computes communities + connected
//! components **scoped to this collection's own nodes and edges** via the same
//! pure [`graph_algorithms`](crate::storage::engine::graph_algorithms) the view
//! resolver dispatches to. That keeps `island_id` correct even in a server that
//! hosts other graphs, which is the whole point of closing the loop with the
//! degraded-cluster legibility bug (#793). The `WITH ANALYTICS` declaration
//! still lives on the collection, so the SQL views remain available for
//! single-graph deployments.
//!
//! ## Materialisation contract
//!
//! [`refresh`] writes one node per cluster member (primary + replicas) and one
//! `replicates_to` edge from the primary to every *reachable* replica. An
//! unreachable replica gets a node but no edge, so connected-components places
//! it on its own island — a degraded cluster stays legible. A topology
//! fingerprint guards the write: when nothing changed, the materialisation and
//! its `graph_version` / `computed_at` are left untouched and the result cache
//! is preserved (a cache *hit*); a real change rewrites the graph, advances the
//! version + timestamp, and invalidates the result cache (a *cold* compute).

use std::collections::HashMap;

use crate::api::{RedDBError, RedDBResult};
use crate::catalog::AnalyticsOutput;
use crate::replication::topology_advertiser::DEFAULT_REPLICA_TIMEOUT_MS;
use crate::storage::engine::graph_algorithms::{self, Weight};
use crate::storage::schema::Value;
use crate::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::RedDBRuntime;

/// Built-in graph collection name. Dotted on purpose so it reads as a
/// namespaced system object; the `red.` prefix routes creation through
/// [`RedDBRuntime::ensure_system_graph_with_analytics`] rather than SQL DDL.
pub const CLUSTER: &str = "red.topology.cluster";

/// Sidecar collection holding the single materialisation-metadata row
/// (`graph_version`, `computed_at`, `fingerprint`). Kept out of the graph so it
/// never pollutes the analytics computation.
pub const META: &str = "red_topology_meta";

/// Analytics outputs declared on the topology graph.
pub const OUTPUTS: &[AnalyticsOutput] = &[
    AnalyticsOutput::Communities,
    AnalyticsOutput::Components,
    AnalyticsOutput::Centrality,
];

/// Edge label for the primary → replica replication link.
const EDGE_KIND: &str = "replicates_to";

/// Role of a cluster member in the topology graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRole {
    Primary,
    Replica,
}

impl MemberRole {
    fn as_str(self) -> &'static str {
        match self {
            MemberRole::Primary => "primary",
            MemberRole::Replica => "replica",
        }
    }
}

/// One cluster member to render into the topology graph. Decoupled from
/// [`crate::replication::primary::ReplicaState`] so callers (and tests) can
/// describe a synthetic cluster without booting replication.
#[derive(Debug, Clone)]
pub struct ClusterMember {
    /// Dial address — doubles as the stable node id consumers route on.
    pub addr: String,
    pub region: String,
    pub role: MemberRole,
    /// Reachable from the primary right now. An unreachable replica is
    /// rendered as a disconnected node (its own island).
    pub healthy: bool,
    /// Last WAL LSN this member has applied. Drives per-edge `lag_lsn`.
    pub last_applied_lsn: u64,
}

/// Outcome of a [`refresh`] call.
#[derive(Debug, Clone, Copy)]
pub struct RefreshOutcome {
    /// `true` when the topology changed and the graph was rewritten.
    pub changed: bool,
    pub graph_version: u64,
    pub computed_at: u64,
}

impl RefreshOutcome {
    /// `red.metrics`-style cache-status: `cold` after a recompute, `hit` when
    /// the cached materialisation was reused.
    pub fn cache_status(&self) -> &'static str {
        if self.changed {
            "cold"
        } else {
            "hit"
        }
    }
}

// ---------------------------------------------------------------
// Aggregated document (PRD #794 schema)
// ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyNode {
    pub id: String,
    pub kind: String,
    pub region: String,
    pub healthy: bool,
    pub lsn: u64,
    pub island_id: u64,
    pub community_id: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyEdge {
    pub source: String,
    pub target: String,
    pub kind: String,
    pub weight: f64,
    pub lag_lsn: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyGroup {
    pub community_id: u64,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyMetadata {
    pub graph_version: u64,
    pub computed_at: u64,
    pub cache_status: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub island_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopologyGraphDoc {
    pub nodes: Vec<TopologyNode>,
    pub edges: Vec<TopologyEdge>,
    pub groups: Vec<TopologyGroup>,
    pub metadata: TopologyMetadata,
}

impl TopologyGraphDoc {
    /// Serialize to the PRD #794 JSON document. Single source of truth shared
    /// by the HTTP handler and the schema snapshot test (AC3, ADR-0011).
    pub fn to_json(&self) -> crate::json::Value {
        let nodes: Vec<crate::json::Value> = self
            .nodes
            .iter()
            .map(|n| {
                crate::json!({
                    "id": n.id,
                    "kind": n.kind,
                    "region": n.region,
                    "healthy": n.healthy,
                    "lsn": n.lsn,
                    "island_id": n.island_id,
                    "community_id": n.community_id,
                })
            })
            .collect();
        let edges: Vec<crate::json::Value> = self
            .edges
            .iter()
            .map(|e| {
                crate::json!({
                    "source": e.source,
                    "target": e.target,
                    "kind": e.kind,
                    "weight": e.weight,
                    "lag_lsn": e.lag_lsn,
                })
            })
            .collect();
        let groups: Vec<crate::json::Value> = self
            .groups
            .iter()
            .map(|g| {
                crate::json!({
                    "community_id": g.community_id,
                    "members": g.members,
                })
            })
            .collect();
        let metadata = crate::json!({
            "graph_version": self.metadata.graph_version,
            "computed_at": self.metadata.computed_at,
            "cache_status": self.metadata.cache_status,
            "node_count": self.metadata.node_count as u64,
            "edge_count": self.metadata.edge_count as u64,
            "island_count": self.metadata.island_count as u64,
        });
        crate::json!({
            "nodes": nodes,
            "edges": edges,
            "groups": groups,
            "metadata": metadata,
        })
    }
}

// ---------------------------------------------------------------
// Bootstrap
// ---------------------------------------------------------------

/// Ensure both the graph collection and its metadata sidecar exist. Idempotent;
/// safe to call on every boot.
pub fn ensure(rt: &RedDBRuntime) -> RedDBResult<()> {
    rt.ensure_system_graph_with_analytics(CLUSTER, OUTPUTS)?;
    rt.db().store().get_or_create_collection(META);
    Ok(())
}

// ---------------------------------------------------------------
// Materialisation
// ---------------------------------------------------------------

/// Materialise `members` into the topology graph. Returns whether the topology
/// changed (and, either way, the current `graph_version` / `computed_at`).
pub fn refresh(rt: &RedDBRuntime, members: &[ClusterMember]) -> RedDBResult<RefreshOutcome> {
    let edges = derive_edges(members);
    let fingerprint = fingerprint(members, &edges);

    let prev = read_meta(rt);
    let collection_populated = rt
        .db()
        .store()
        .get_collection(CLUSTER)
        .map(|c| !c.query_all(|_| true).is_empty())
        .unwrap_or(false);

    if let Some(prev) = prev.as_ref() {
        if prev.fingerprint == fingerprint && collection_populated {
            return Ok(RefreshOutcome {
                changed: false,
                graph_version: prev.graph_version,
                computed_at: prev.computed_at,
            });
        }
    }

    let store = rt.db().store();
    // Replace the whole materialisation atomically enough for a read-only
    // endpoint: clear, then re-insert. Topology is tiny (cluster cardinality),
    // so a full rewrite is cheaper than a diff.
    if let Some(manager) = store.get_collection(CLUSTER) {
        let ids: Vec<EntityId> = manager
            .query_all(|_| true)
            .into_iter()
            .map(|e| e.id)
            .collect();
        for id in ids {
            let _ = store.delete(CLUSTER, id);
        }
    }

    let node_entities: Vec<UnifiedEntity> = members
        .iter()
        .map(|m| {
            let mut props: HashMap<String, Value> = HashMap::new();
            props.insert("role".to_string(), Value::text(m.role.as_str()));
            props.insert("region".to_string(), Value::text(m.region.clone()));
            props.insert("healthy".to_string(), Value::Boolean(m.healthy));
            props.insert(
                "lsn".to_string(),
                Value::UnsignedInteger(m.last_applied_lsn),
            );
            UnifiedEntity::graph_node(EntityId::new(0), m.addr.clone(), m.role.as_str(), props)
        })
        .collect();
    store
        .bulk_insert(CLUSTER, node_entities)
        .map_err(|err| RedDBError::Internal(err.to_string()))?;

    // Map addr → assigned node id for edge endpoints. Re-read rather than trust
    // insert order so the edges always reference the persisted ids.
    let addr_to_id = node_id_by_addr(rt);
    let edge_entities: Vec<UnifiedEntity> = edges
        .iter()
        .filter_map(|e| {
            let from = addr_to_id.get(&e.source)?;
            let to = addr_to_id.get(&e.target)?;
            let mut props: HashMap<String, Value> = HashMap::new();
            props.insert("kind".to_string(), Value::text(EDGE_KIND));
            props.insert("lag_lsn".to_string(), Value::UnsignedInteger(e.lag_lsn));
            Some(UnifiedEntity::graph_edge(
                EntityId::new(0),
                EDGE_KIND,
                from.clone(),
                to.clone(),
                e.weight as f32,
                props,
            ))
        })
        .collect();
    if !edge_entities.is_empty() {
        store
            .bulk_insert(CLUSTER, edge_entities)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
    }

    let graph_version = prev.map(|p| p.graph_version).unwrap_or(0) + 1;
    let computed_at = crate::utils::now_unix_millis();
    write_meta(rt, graph_version, computed_at, &fingerprint);

    // A topology change invalidates any cached analytics-view results so the
    // next read re-resolves against the fresh graph (AC5).
    rt.invalidate_result_cache();

    Ok(RefreshOutcome {
        changed: true,
        graph_version,
        computed_at,
    })
}

/// Pull the live cluster state off the runtime and materialise it.
pub fn refresh_from_runtime(rt: &RedDBRuntime) -> RedDBResult<RefreshOutcome> {
    let db = rt.db();
    let primary_addr = rt.config_string("red.grpc.advertise_addr", "");
    let primary_region = db.options().replication.region.clone();
    let primary_lsn = db
        .replication
        .as_ref()
        .map(|repl| repl.wal_buffer.current_lsn())
        .unwrap_or(0);

    let now_ms = crate::utils::now_unix_millis() as u128;
    let mut members = Vec::new();
    members.push(ClusterMember {
        addr: if primary_addr.is_empty() {
            "primary".to_string()
        } else {
            primary_addr
        },
        region: if primary_region.is_empty() {
            "unknown".to_string()
        } else {
            primary_region
        },
        role: MemberRole::Primary,
        healthy: true,
        last_applied_lsn: primary_lsn,
    });
    for replica in rt.primary_replica_snapshots() {
        let healthy = now_ms < replica.last_seen_at_unix_ms
            || now_ms - replica.last_seen_at_unix_ms <= DEFAULT_REPLICA_TIMEOUT_MS;
        members.push(ClusterMember {
            addr: replica.id,
            region: replica.region.unwrap_or_else(|| "unknown".to_string()),
            role: MemberRole::Replica,
            healthy,
            last_applied_lsn: replica.last_acked_lsn,
        });
    }
    refresh(rt, &members)
}

// ---------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------

/// Read the materialised graph and assemble the PRD #794 document, computing
/// communities + connected components scoped to this collection's own subgraph.
/// `cache_status` is supplied by the caller from the preceding [`refresh`].
pub fn build_graph_doc(rt: &RedDBRuntime, cache_status: &str) -> RedDBResult<TopologyGraphDoc> {
    let store = rt.db().store();
    let manager = store
        .get_collection(CLUSTER)
        .ok_or_else(|| RedDBError::Internal(format!("{CLUSTER} collection missing")))?;
    let entities = manager.query_all(|_| true);

    // node id (string) → (addr, kind, region, healthy, lsn)
    let mut raw_nodes: Vec<(String, String, String, String, bool, u64)> = Vec::new();
    let mut id_to_addr: HashMap<String, String> = HashMap::new();
    let mut raw_edges: Vec<(String, String, f64, u64)> = Vec::new();
    for entity in &entities {
        match &entity.kind {
            EntityKind::GraphNode(node) => {
                let id = entity.id.raw().to_string();
                let props = match &entity.data {
                    EntityData::Node(n) => Some(n),
                    _ => None,
                };
                let role = props
                    .and_then(|p| text_prop(p.get("role")))
                    .unwrap_or_else(|| node.node_type.clone());
                let region = props
                    .and_then(|p| text_prop(p.get("region")))
                    .unwrap_or_else(|| "unknown".to_string());
                let healthy = props
                    .and_then(|p| bool_prop(p.get("healthy")))
                    .unwrap_or(true);
                let lsn = props.and_then(|p| u64_prop(p.get("lsn"))).unwrap_or(0);
                id_to_addr.insert(id.clone(), node.label.clone());
                raw_nodes.push((id, node.label.clone(), role, region, healthy, lsn));
            }
            EntityKind::GraphEdge(edge) => {
                let weight = match &entity.data {
                    EntityData::Edge(e) => e.weight as f64,
                    _ => 0.0,
                };
                let lag_lsn = match &entity.data {
                    EntityData::Edge(e) => u64_prop(e.properties.get("lag_lsn")).unwrap_or(0),
                    _ => 0,
                };
                raw_edges.push((
                    edge.from_node.clone(),
                    edge.to_node.clone(),
                    weight,
                    lag_lsn,
                ));
            }
            _ => {}
        }
    }

    // Build addr-keyed inputs for the scoped algorithms.
    let node_addrs: Vec<String> = raw_nodes.iter().map(|n| n.1.clone()).collect();
    let algo_edges: Vec<(String, String, Weight)> = raw_edges
        .iter()
        .filter_map(|(from, to, weight, _)| {
            let from_addr = id_to_addr.get(from)?.clone();
            let to_addr = id_to_addr.get(to)?.clone();
            Some((from_addr, to_addr, *weight as Weight))
        })
        .collect();

    let islands: HashMap<String, u64> =
        graph_algorithms::connected_components(&node_addrs, &algo_edges)
            .into_iter()
            .map(|(addr, island)| (addr, island as u64))
            .collect();
    let communities: HashMap<String, u64> =
        graph_algorithms::louvain(&node_addrs, &algo_edges, 1.0)
            .into_iter()
            .map(|(addr, community)| (addr, community as u64))
            .collect();

    let mut nodes: Vec<TopologyNode> = raw_nodes
        .into_iter()
        .map(|(_, addr, kind, region, healthy, lsn)| {
            let island_id = islands.get(&addr).copied().unwrap_or(0);
            let community_id = communities.get(&addr).copied().unwrap_or(0);
            TopologyNode {
                id: addr,
                kind,
                region,
                healthy,
                lsn,
                island_id,
                community_id,
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let mut edges: Vec<TopologyEdge> = algo_edges
        .iter()
        .zip(raw_edges.iter())
        .map(
            |((from_addr, to_addr, _), (_, _, weight, lag_lsn))| TopologyEdge {
                source: from_addr.clone(),
                target: to_addr.clone(),
                kind: EDGE_KIND.to_string(),
                weight: *weight,
                lag_lsn: *lag_lsn,
            },
        )
        .collect();
    edges.sort_by(|a, b| {
        (a.source.clone(), a.target.clone()).cmp(&(b.source.clone(), b.target.clone()))
    });

    // Groups reflect communities — one entry per distinct community id.
    let mut grouped: HashMap<u64, Vec<String>> = HashMap::new();
    for node in &nodes {
        grouped
            .entry(node.community_id)
            .or_default()
            .push(node.id.clone());
    }
    let mut groups: Vec<TopologyGroup> = grouped
        .into_iter()
        .map(|(community_id, mut members)| {
            members.sort();
            TopologyGroup {
                community_id,
                members,
            }
        })
        .collect();
    groups.sort_by_key(|g| g.community_id);

    let island_count = nodes
        .iter()
        .map(|n| n.island_id)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let meta = read_meta(rt);
    let metadata = TopologyMetadata {
        graph_version: meta.as_ref().map(|m| m.graph_version).unwrap_or(0),
        computed_at: meta.as_ref().map(|m| m.computed_at).unwrap_or(0),
        cache_status: cache_status.to_string(),
        node_count: nodes.len(),
        edge_count: edges.len(),
        island_count,
    };

    Ok(TopologyGraphDoc {
        nodes,
        edges,
        groups,
        metadata,
    })
}

// ---------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------

#[derive(Debug, Clone)]
struct DerivedEdge {
    source: String,
    target: String,
    weight: f64,
    lag_lsn: u64,
}

/// One `replicates_to` edge from the primary to every reachable replica.
fn derive_edges(members: &[ClusterMember]) -> Vec<DerivedEdge> {
    let Some(primary) = members.iter().find(|m| m.role == MemberRole::Primary) else {
        return Vec::new();
    };
    members
        .iter()
        .filter(|m| m.role == MemberRole::Replica && m.healthy)
        .map(|replica| DerivedEdge {
            source: primary.addr.clone(),
            target: replica.addr.clone(),
            weight: 1.0,
            lag_lsn: primary
                .last_applied_lsn
                .saturating_sub(replica.last_applied_lsn),
        })
        .collect()
}

/// Canonical, order-independent fingerprint of the topology. Any change to a
/// member's identity, role, health, or applied LSN — or to the derived edge set
/// — flips it, which is what gates a rewrite + version bump.
fn fingerprint(members: &[ClusterMember], edges: &[DerivedEdge]) -> String {
    let mut node_lines: Vec<String> = members
        .iter()
        .map(|m| {
            format!(
                "{}|{}|{}|{}|{}",
                m.addr,
                m.role.as_str(),
                m.region,
                m.healthy,
                m.last_applied_lsn
            )
        })
        .collect();
    node_lines.sort();
    let mut edge_lines: Vec<String> = edges
        .iter()
        .map(|e| format!("{}->{}|{}|{}", e.source, e.target, e.weight, e.lag_lsn))
        .collect();
    edge_lines.sort();
    format!("N[{}]E[{}]", node_lines.join(","), edge_lines.join(","))
}

/// Build addr → node-id-string map from the persisted graph nodes.
fn node_id_by_addr(rt: &RedDBRuntime) -> HashMap<String, String> {
    let store = rt.db().store();
    let Some(manager) = store.get_collection(CLUSTER) else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    for entity in manager.query_all(|_| true) {
        if let EntityKind::GraphNode(node) = &entity.kind {
            map.insert(node.label.clone(), entity.id.raw().to_string());
        }
    }
    map
}

#[derive(Debug, Clone)]
struct MetaRow {
    graph_version: u64,
    computed_at: u64,
    fingerprint: String,
}

fn read_meta(rt: &RedDBRuntime) -> Option<MetaRow> {
    let store = rt.db().store();
    let manager = store.get_collection(META)?;
    manager.query_all(|_| true).into_iter().find_map(|entity| {
        let EntityData::Row(row) = &entity.data else {
            return None;
        };
        let named = row.named.as_ref()?;
        Some(MetaRow {
            graph_version: u64_prop(named.get("graph_version")).unwrap_or(0),
            computed_at: u64_prop(named.get("computed_at")).unwrap_or(0),
            fingerprint: text_prop(named.get("fingerprint")).unwrap_or_default(),
        })
    })
}

fn write_meta(rt: &RedDBRuntime, graph_version: u64, computed_at: u64, fingerprint: &str) {
    let store = rt.db().store();
    // Single-row collection: clear then insert so the latest state always wins
    // (set_config_tree appends and would accumulate stale rows).
    if let Some(manager) = store.get_collection(META) {
        let ids: Vec<EntityId> = manager
            .query_all(|_| true)
            .into_iter()
            .map(|e| e.id)
            .collect();
        for id in ids {
            let _ = store.delete(META, id);
        }
    }
    let mut named: HashMap<String, Value> = HashMap::new();
    named.insert(
        "graph_version".to_string(),
        Value::UnsignedInteger(graph_version),
    );
    named.insert(
        "computed_at".to_string(),
        Value::UnsignedInteger(computed_at),
    );
    named.insert("fingerprint".to_string(), Value::text(fingerprint));
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: std::sync::Arc::from(META),
            row_id: 0,
        },
        EntityData::Row(RowData {
            columns: Vec::new(),
            named: Some(named),
            schema: None,
        }),
    );
    let _ = store.insert_auto(META, entity);
}

fn text_prop(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Text(s) => Some(s.to_string()),
        _ => None,
    }
}

fn bool_prop(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Boolean(b) => Some(*b),
        _ => None,
    }
}

fn u64_prop(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::UnsignedInteger(n) => Some(*n),
        Value::Integer(n) => Some((*n).max(0) as u64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(addr: &str, role: MemberRole, healthy: bool, lsn: u64) -> ClusterMember {
        ClusterMember {
            addr: addr.to_string(),
            region: "us-east-1".to_string(),
            role,
            healthy,
            last_applied_lsn: lsn,
        }
    }

    #[test]
    fn derive_edges_connects_only_healthy_replicas() {
        let members = vec![
            member("primary:5050", MemberRole::Primary, true, 100),
            member("replica-a:5050", MemberRole::Replica, true, 90),
            member("replica-b:5050", MemberRole::Replica, false, 0),
        ];
        let edges = derive_edges(&members);
        assert_eq!(edges.len(), 1, "only the reachable replica gets an edge");
        assert_eq!(edges[0].source, "primary:5050");
        assert_eq!(edges[0].target, "replica-a:5050");
        assert_eq!(edges[0].lag_lsn, 10, "lag_lsn = primary lsn - replica lsn");
    }

    #[test]
    fn derive_edges_without_primary_is_empty() {
        let members = vec![member("replica-a:5050", MemberRole::Replica, true, 90)];
        assert!(derive_edges(&members).is_empty());
    }

    #[test]
    fn fingerprint_is_order_independent_but_change_sensitive() {
        let a = vec![
            member("primary:5050", MemberRole::Primary, true, 100),
            member("replica-a:5050", MemberRole::Replica, true, 90),
        ];
        let mut b = a.clone();
        b.reverse();
        assert_eq!(
            fingerprint(&a, &derive_edges(&a)),
            fingerprint(&b, &derive_edges(&b)),
            "member order must not affect the fingerprint"
        );

        let mut c = a.clone();
        c[1].healthy = false;
        assert_ne!(
            fingerprint(&a, &derive_edges(&a)),
            fingerprint(&c, &derive_edges(&c)),
            "flipping a replica's health must change the fingerprint"
        );
    }
}
