//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "graph_analytics/e2e_graph_compound_updates.rs"]
mod e2e_graph_compound_updates;

#[path = "graph_analytics/e2e_graph_public_envelope.rs"]
mod e2e_graph_public_envelope;

#[path = "graph_analytics/e2e_issue_544_graph_insert_returns_labels_ids.rs"]
mod e2e_issue_544_graph_insert_returns_labels_ids;

#[path = "graph_analytics/e2e_issue_553_graph_edge_property_projection.rs"]
mod e2e_issue_553_graph_edge_property_projection;

#[path = "graph_analytics/e2e_issue_757_graph_policy_aware.rs"]
mod e2e_issue_757_graph_policy_aware;

#[path = "runtime_persistence/e2e_issue_795_components_tvf.rs"]
mod e2e_issue_795_components_tvf;

#[path = "graph_analytics/e2e_issue_797_centrality_tvfs.rs"]
mod e2e_issue_797_centrality_tvfs;

#[path = "graph_analytics/e2e_issue_798_shortest_path_tvf.rs"]
mod e2e_issue_798_shortest_path_tvf;

#[path = "graph_analytics/e2e_issue_799_inline_graph_tvf.rs"]
mod e2e_issue_799_inline_graph_tvf;

#[path = "graph_analytics/e2e_issue_802_graph_cache.rs"]
mod e2e_issue_802_graph_cache;

#[path = "graph_analytics/e2e_issue_803_topology_graph.rs"]
mod e2e_issue_803_topology_graph;

#[path = "graph_analytics/e2e_issue_804_topology_hint.rs"]
mod e2e_issue_804_topology_hint;

#[path = "graph_analytics/integration_graph_ops.rs"]
mod integration_graph_ops;
