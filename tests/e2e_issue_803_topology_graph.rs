//! End-to-end tests for the built-in `red.topology.cluster` graph collection
//! and the `GET /v1/topology/graph` aggregation (#803).
//!
//! Exercises the full bootstrap → materialise → analytics → aggregate path:
//!   - `red.topology.cluster` exists after a fresh boot, declared WITH ANALYTICS
//!     (communities, components, centrality), and survives a restart (AC1);
//!   - the materialiser writes one node per member and a `replicates_to` edge to
//!     every reachable replica with per-edge `weight` / `kind` / `lag_lsn` (AC2);
//!   - the aggregated document matches the PRD #794 schema (AC3);
//!   - a synthetic 3-node cluster (primary + 2 replicas, one unreachable) puts
//!     the unreachable replica on its own `island_id` (AC4);
//!   - a topology mutation advances `graph_version` + `computed_at` and a no-op
//!     refresh does not, surfacing `cache_status` hit vs cold (AC5/AC6).

mod support;

use reddb::application::topology_collections as topo;
use reddb::catalog::CollectionModel;
use reddb::RedDBRuntime;
use support::PersistentDbPath;

fn member(addr: &str, role: topo::MemberRole, healthy: bool, lsn: u64) -> topo::ClusterMember {
    topo::ClusterMember {
        addr: addr.to_string(),
        region: "us-east-1".to_string(),
        role,
        healthy,
        last_applied_lsn: lsn,
    }
}

/// 3-node cluster: primary (lsn 100), one reachable replica (lsn 95), one
/// unreachable replica (cut off from the primary).
fn three_node_cluster() -> Vec<topo::ClusterMember> {
    vec![
        member("primary:5050", topo::MemberRole::Primary, true, 100),
        member("replica-a:5050", topo::MemberRole::Replica, true, 95),
        member("replica-b:5050", topo::MemberRole::Replica, false, 40),
    ]
}

fn topology_contract_outputs(rt: &RedDBRuntime) -> Option<Vec<String>> {
    rt.db()
        .collection_contracts()
        .into_iter()
        .find(|c| c.name == topo::CLUSTER)
        .map(|c| {
            assert_eq!(
                c.declared_model,
                CollectionModel::Graph,
                "red.topology.cluster must be a graph model"
            );
            let mut outputs: Vec<String> = c
                .analytics_config
                .iter()
                .map(|view| view.output.as_str().to_string())
                .collect();
            outputs.sort();
            outputs
        })
}

#[test]
fn topology_collection_bootstrapped_with_analytics_and_survives_restart() {
    // AC1: exists after a fresh boot, declared WITH ANALYTICS, survives restart.
    let db = PersistentDbPath::new("issue_803_bootstrap");
    {
        let rt = db.open_runtime();
        assert!(
            rt.db().store().get_collection(topo::CLUSTER).is_some(),
            "red.topology.cluster must exist after a fresh boot"
        );
        let outputs = topology_contract_outputs(&rt).expect("topology contract present after boot");
        assert_eq!(
            outputs,
            vec![
                "centrality".to_string(),
                "communities".to_string(),
                "components".to_string()
            ],
            "all three analytics outputs declared"
        );
    }

    // Reopen the same persistent path — the WAL-backed contract must recover.
    let rt = db.open_runtime();
    assert!(
        rt.db().store().get_collection(topo::CLUSTER).is_some(),
        "red.topology.cluster must survive a restart"
    );
    let outputs =
        topology_contract_outputs(&rt).expect("topology contract recovered after restart");
    assert_eq!(outputs.len(), 3, "analytics config survives restart");
}

#[test]
fn unreachable_replica_lands_on_its_own_island() {
    // AC2 + AC4: nodes/edges/lag_lsn populate; the unreachable replica is
    // separated into its own connected component.
    let db = PersistentDbPath::new("issue_803_island");
    let rt = db.open_runtime();

    let outcome = topo::refresh(&rt, &three_node_cluster()).expect("refresh topology");
    assert!(outcome.changed, "first materialisation is a change");

    let doc = topo::build_graph_doc(&rt, outcome.cache_status()).expect("build graph doc");

    // Three members → three nodes.
    assert_eq!(doc.nodes.len(), 3, "one node per cluster member");
    let primary = doc
        .nodes
        .iter()
        .find(|n| n.id == "primary:5050")
        .expect("primary node");
    assert_eq!(primary.kind, "primary");
    let replica_a = doc
        .nodes
        .iter()
        .find(|n| n.id == "replica-a:5050")
        .expect("replica-a node");
    assert!(replica_a.healthy, "reachable replica is healthy");
    let replica_b = doc
        .nodes
        .iter()
        .find(|n| n.id == "replica-b:5050")
        .expect("replica-b node");
    assert!(!replica_b.healthy, "unreachable replica is unhealthy");

    // Exactly one edge: primary → reachable replica, with lag_lsn = 100 - 95.
    assert_eq!(doc.edges.len(), 1, "only the reachable replica is linked");
    let edge = &doc.edges[0];
    assert_eq!(edge.source, "primary:5050");
    assert_eq!(edge.target, "replica-a:5050");
    assert_eq!(edge.kind, "replicates_to");
    assert_eq!(edge.weight, 1.0);
    assert_eq!(edge.lag_lsn, 5, "lag_lsn = primary lsn - replica lsn");

    // Island separation: primary + replica-a connected, replica-b isolated.
    assert_eq!(
        primary.island_id, replica_a.island_id,
        "primary and reachable replica share an island"
    );
    assert_ne!(
        primary.island_id, replica_b.island_id,
        "the unreachable replica is on its own island (AC4)"
    );
    assert_eq!(doc.metadata.island_count, 2, "two islands total");
}

#[test]
fn aggregated_document_matches_prd_794_schema() {
    // AC3: pin the JSON document shape (ADR-0011 snapshot discipline).
    let db = PersistentDbPath::new("issue_803_schema");
    let rt = db.open_runtime();
    let outcome = topo::refresh(&rt, &three_node_cluster()).expect("refresh topology");
    let doc = topo::build_graph_doc(&rt, outcome.cache_status()).expect("build graph doc");
    let json = doc.to_json();

    let obj = json.as_object().expect("document is a JSON object");
    let mut top_keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
    top_keys.sort();
    assert_eq!(top_keys, vec!["edges", "groups", "metadata", "nodes"]);

    let node = json["nodes"].as_array().expect("nodes array")[0]
        .as_object()
        .expect("node object");
    let mut node_keys: Vec<&str> = node.keys().map(|k| k.as_str()).collect();
    node_keys.sort();
    assert_eq!(
        node_keys,
        vec![
            "community_id",
            "healthy",
            "id",
            "island_id",
            "kind",
            "lsn",
            "region"
        ]
    );

    let edge = json["edges"].as_array().expect("edges array")[0]
        .as_object()
        .expect("edge object");
    let mut edge_keys: Vec<&str> = edge.keys().map(|k| k.as_str()).collect();
    edge_keys.sort();
    assert_eq!(
        edge_keys,
        vec!["kind", "lag_lsn", "source", "target", "weight"]
    );

    let group = json["groups"].as_array().expect("groups array")[0]
        .as_object()
        .expect("group object");
    let mut group_keys: Vec<&str> = group.keys().map(|k| k.as_str()).collect();
    group_keys.sort();
    assert_eq!(group_keys, vec!["community_id", "members"]);

    let metadata = json["metadata"].as_object().expect("metadata object");
    let mut meta_keys: Vec<&str> = metadata.keys().map(|k| k.as_str()).collect();
    meta_keys.sort();
    assert_eq!(
        meta_keys,
        vec![
            "cache_status",
            "computed_at",
            "edge_count",
            "graph_version",
            "island_count",
            "node_count"
        ]
    );
}

#[test]
fn mutation_advances_version_and_no_op_is_a_cache_hit() {
    // AC5 + AC6: a topology change advances graph_version + computed_at and
    // reports `cold`; an unchanged refresh reuses the materialisation (`hit`).
    let db = PersistentDbPath::new("issue_803_versioning");
    let rt = db.open_runtime();

    let first = topo::refresh(&rt, &three_node_cluster()).expect("first refresh");
    assert!(first.changed);
    assert_eq!(first.graph_version, 1);
    assert_eq!(first.cache_status(), "cold");

    // Identical topology → no rewrite, version frozen, served from cache.
    let again = topo::refresh(&rt, &three_node_cluster()).expect("no-op refresh");
    assert!(!again.changed, "identical topology is a no-op");
    assert_eq!(
        again.graph_version, 1,
        "version frozen when nothing changed"
    );
    assert_eq!(again.cache_status(), "hit");

    // Mutate the topology: replica-a falls behind (lsn change).
    let mut mutated = three_node_cluster();
    mutated[1].last_applied_lsn = 70;
    let third = topo::refresh(&rt, &mutated).expect("refresh after mutation");
    assert!(third.changed, "a real topology change rewrites the graph");
    assert!(
        third.graph_version > first.graph_version,
        "graph_version advances on mutation"
    );
    assert!(
        third.computed_at >= first.computed_at,
        "computed_at advances (or holds) on mutation"
    );
    assert_eq!(third.cache_status(), "cold");

    // The lag now reflects the mutation: 100 - 70 = 30.
    let doc = topo::build_graph_doc(&rt, third.cache_status()).expect("doc after mutation");
    let edge = doc
        .edges
        .iter()
        .find(|e| e.target == "replica-a:5050")
        .expect("edge to replica-a");
    assert_eq!(edge.lag_lsn, 30, "lag_lsn reflects the new replica lsn");
}
