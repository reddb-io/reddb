//! End-to-end tests for the Tier-3 spectral layout hint on
//! `GET /v1/topology/graph` (#804).
//!
//! Covers the acceptance criteria layered on top of #803:
//!   - `build_graph_doc(.., include_hint = true)` surfaces a normalised
//!     `hint.{x,y}` per node, and the JSON document carries it (AC2);
//!   - identical clusters across a server restart produce byte-identical hints
//!     — the embedding is deterministic, no random initialisation (AC1/AC3);
//!   - the hint is non-authoritative and optional: with `include_hint = false`
//!     no node carries a hint and the JSON `hint` field is omitted entirely
//!     (AC5 — clients may ignore it).

mod support;

use reddb::application::topology_collections as topo;
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

/// 4-node cluster with a real layout to exercise: primary + two reachable
/// replicas + one unreachable replica (its own island).
fn cluster() -> Vec<topo::ClusterMember> {
    vec![
        member("primary:5050", topo::MemberRole::Primary, true, 100),
        member("replica-a:5050", topo::MemberRole::Replica, true, 95),
        member("replica-b:5050", topo::MemberRole::Replica, true, 90),
        member("replica-c:5050", topo::MemberRole::Replica, false, 40),
    ]
}

#[test]
fn hint_is_surfaced_per_node_and_normalised() {
    // AC2: every node carries a normalised [0,1]² hint when enabled.
    let db = PersistentDbPath::new("issue_804_surface");
    let rt = db.open_runtime();
    let outcome = topo::refresh(&rt, &cluster()).expect("refresh topology");
    let doc = topo::build_graph_doc(&rt, outcome.cache_status(), true).expect("build graph doc");

    assert_eq!(doc.nodes.len(), 4, "one node per member");
    for node in &doc.nodes {
        let hint = node
            .hint
            .unwrap_or_else(|| panic!("node {} should carry a hint", node.id));
        assert!(
            (0.0..=1.0).contains(&hint.x) && (0.0..=1.0).contains(&hint.y),
            "node {} hint ({}, {}) must be normalised to [0,1]²",
            node.id,
            hint.x,
            hint.y
        );
    }

    // The JSON document exposes the hint object too.
    let json = doc.to_json();
    let node0 = json["nodes"].as_array().expect("nodes array")[0]
        .as_object()
        .expect("node object");
    assert!(node0.contains_key("hint"), "JSON node carries hint");
}

#[test]
fn identical_cluster_across_reload_yields_identical_hints() {
    // AC1/AC3: determinism — reopen the same persistent path, re-materialise the
    // same topology, and the per-node hints must be byte-identical (no random
    // initialisation; same seed every time).
    let db = PersistentDbPath::new("issue_804_determinism");

    let first: Vec<(String, (f64, f64))> = {
        let rt = db.open_runtime();
        let outcome = topo::refresh(&rt, &cluster()).expect("first refresh");
        let doc =
            topo::build_graph_doc(&rt, outcome.cache_status(), true).expect("first graph doc");
        doc.nodes
            .iter()
            .map(|n| {
                let h = n.hint.expect("hint present");
                (n.id.clone(), (h.x, h.y))
            })
            .collect()
    };

    // Restart: a fresh runtime over the same WAL-backed store.
    let rt = db.open_runtime();
    let outcome = topo::refresh(&rt, &cluster()).expect("second refresh");
    let doc = topo::build_graph_doc(&rt, outcome.cache_status(), true).expect("second graph doc");
    let second: Vec<(String, (f64, f64))> = doc
        .nodes
        .iter()
        .map(|n| {
            let h = n.hint.expect("hint present");
            (n.id.clone(), (h.x, h.y))
        })
        .collect();

    assert_eq!(
        first, second,
        "identical clusters across a reload must produce identical hints"
    );
}

#[test]
fn hint_is_optional_and_omitted_when_disabled() {
    // AC5: the hint is non-authoritative and optional. With it disabled, no node
    // carries a hint and the JSON `hint` field is absent — a client that ignores
    // hints sees the unchanged PRD #794 shape.
    let db = PersistentDbPath::new("issue_804_optional");
    let rt = db.open_runtime();
    let outcome = topo::refresh(&rt, &cluster()).expect("refresh topology");
    let doc = topo::build_graph_doc(&rt, outcome.cache_status(), false).expect("build graph doc");

    assert!(
        doc.nodes.iter().all(|n| n.hint.is_none()),
        "no node carries a hint when the embedding is disabled"
    );

    let json = doc.to_json();
    for node in json["nodes"].as_array().expect("nodes array") {
        let obj = node.as_object().expect("node object");
        assert!(
            !obj.contains_key("hint"),
            "the optional hint field is omitted when disabled"
        );
    }
}
