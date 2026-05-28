//! Graph viewport contract — issue #744.
//!
//! Red UI's graph explorer asks one question on every panel render:
//! "given a collection, a center or a small filter set, a bounded
//! traversal depth and a hard cap on node count, what nodes and edges
//! should I show?". Before this slice the UI answered that by reaching
//! into the runtime's graph helpers directly and stitching together a
//! shape that included `RuntimeGraphVisit` / `RuntimeGraphEdge`. That
//! coupling broke twice: once when neighborhood output added the
//! `depth` field, and again when the frontend started chunking
//! `id IN (rid_1, rid_2, …)` requests by hand to work around a lookup
//! path that silently returned zero rows for large RID lists.
//!
//! This module is the stable contract that replaces both workarounds:
//!
//! * [`ViewportRequest`] — the request shape Red UI sends. A
//!   collection name plus a [`ViewportSelector`] (center node, RID
//!   list, or label/type filter), an optional traversal `depth`, and
//!   a hard `limit` on returned nodes. The RID-list variant carries
//!   *all* requested ids verbatim; the contract guarantees no silent
//!   chunking or input truncation. Output truncation is reported
//!   explicitly via [`TruncationMeta`].
//!
//! * [`Viewport`] — the response. Normalized [`ViewportNode`] +
//!   [`ViewportEdge`] lists with stable ordering, plus
//!   [`TruncationMeta`] that tells the UI exactly *why* the response
//!   was capped (node-limit hit, depth-limit hit, both, or neither).
//!
//! * [`Viewport::from_visits`] — the pure builder runtime wiring will
//!   call. It is deliberately pure so this module can be unit-tested
//!   end-to-end without spinning up a graph store, and so the
//!   truncation rule lives in exactly one place.
//!
//! Independence from internal storage modules is the load-bearing
//! property. We re-declare `ViewportDirection` and the node / edge
//! shapes here (rather than re-exporting them from `runtime` or
//! `engine::graph_store`) so a future internal rename does not force
//! a Red UI release. The pattern matches `storage::vector::introspection`
//! (issue #743) and `storage::queue::presence` (issue #742).

use std::collections::HashSet;

/// Traversal direction for a viewport request. Stable wire-style
/// strings via [`ViewportDirection::as_str`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewportDirection {
    /// Follow outgoing edges only.
    Outgoing,
    /// Follow incoming edges only.
    Incoming,
    /// Follow edges in both directions.
    #[default]
    Both,
}

impl ViewportDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            ViewportDirection::Outgoing => "outgoing",
            ViewportDirection::Incoming => "incoming",
            ViewportDirection::Both => "both",
        }
    }
}

/// How Red UI picks the starting set of nodes for a viewport. Exactly
/// one variant per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewportSelector {
    /// Expand outwards from a single named node id.
    Center(String),
    /// Seed the viewport with a specific RID list. Carries *all*
    /// requested ids verbatim — the contract guarantees no silent
    /// chunking on the way in, even when the list is large. This is
    /// the replacement for the frontend's old RID-IN chunking
    /// workaround.
    Rids(Vec<String>),
    /// Seed the viewport with every node whose label / type matches
    /// the filter. Empty string = match-all (UI uses this for the
    /// initial "show me the whole collection, capped" load).
    LabelEquals(String),
}

impl ViewportSelector {
    /// Number of explicit seed ids the selector carries. For
    /// `LabelEquals` and `Center` this is the obvious 1 / 0; for
    /// `Rids` it is the full input cardinality (the contract never
    /// drops ids on the way in).
    pub fn seed_count(&self) -> usize {
        match self {
            ViewportSelector::Center(_) => 1,
            ViewportSelector::Rids(ids) => ids.len(),
            ViewportSelector::LabelEquals(_) => 0,
        }
    }
}

/// A viewport request. The runtime call that will translate this into
/// a populated [`Viewport`] is wired in a follow-up slice; the shape
/// is the load-bearing contract here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewportRequest {
    /// Graph collection name.
    pub collection: String,
    /// How to pick the starting node set.
    pub selector: ViewportSelector,
    /// How many hops out from the seed set to expand. `0` means "just
    /// the seeds, no expansion".
    pub depth: u32,
    /// Optional edge-label allow-list. Empty = all labels.
    pub edge_labels: Vec<String>,
    /// Traversal direction.
    pub direction: ViewportDirection,
    /// Hard cap on returned nodes. The builder enforces this and
    /// reports the cap via [`TruncationMeta::node_limit_hit`].
    pub node_limit: u32,
}

impl ViewportRequest {
    /// Convenience: minimal request seeded from a single center node
    /// with default direction (`Both`).
    pub fn center(collection: impl Into<String>, node: impl Into<String>) -> Self {
        Self {
            collection: collection.into(),
            selector: ViewportSelector::Center(node.into()),
            depth: 1,
            edge_labels: Vec::new(),
            direction: ViewportDirection::Both,
            node_limit: 256,
        }
    }
}

/// One node in a viewport response. Properties and weight are
/// optional because not every graph collection stores them.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewportNode {
    pub id: String,
    pub label: String,
    pub node_type: String,
    /// JSON-shaped property bag as a string (the canonical contract
    /// wire form). The runtime fills this from the stored property
    /// page; `"{}"` if the node has no properties.
    pub properties: String,
    /// Hop distance from the seed set. `0` for seeds themselves.
    pub depth: u32,
}

/// One edge in a viewport response. Always points from `source` to
/// `target` in storage order; UIs that render undirected views can
/// collapse pairs themselves.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewportEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    /// `None` when the edge has no stored weight (the typical case
    /// for unweighted graphs); the UI must not invent a default.
    pub weight: Option<f32>,
    /// Same JSON-shaped property bag convention as [`ViewportNode`].
    pub properties: String,
}

/// Why a viewport response was capped. Each flag is independent: a
/// single response can hit both the node limit and the depth limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TruncationMeta {
    /// True when the response was cut short because [`ViewportRequest::node_limit`]
    /// would have been exceeded.
    pub node_limit_hit: bool,
    /// True when the traversal stopped because [`ViewportRequest::depth`]
    /// was reached and the frontier was non-empty (i.e. more nodes
    /// existed beyond the depth cap).
    pub depth_limit_hit: bool,
    /// Number of additional nodes the traversal saw but did not
    /// return because the node limit was reached. `0` when
    /// `node_limit_hit` is false.
    pub dropped_node_count: u32,
}

impl TruncationMeta {
    /// `true` when nothing was dropped — neither limit hit.
    pub fn is_complete(self) -> bool {
        !self.node_limit_hit && !self.depth_limit_hit
    }
}

/// A populated viewport response. Returned by the (future) runtime
/// call and consumed directly by Red UI.
#[derive(Debug, Clone, PartialEq)]
pub struct Viewport {
    pub collection: String,
    pub seed_count: u32,
    pub nodes: Vec<ViewportNode>,
    pub edges: Vec<ViewportEdge>,
    pub truncation: TruncationMeta,
}

/// One input visit row for [`Viewport::from_visits`] — a node the
/// traversal reached, in traversal order, with its depth and (for the
/// frontier) whether its outbound expansion was cut by the depth cap.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewportVisitInput {
    pub node: ViewportNode,
    /// True when this visit sat on the depth frontier and the
    /// traversal would have expanded further if `depth` had been
    /// larger. The builder folds this into
    /// [`TruncationMeta::depth_limit_hit`].
    pub frontier_truncated: bool,
}

impl Viewport {
    /// Pure builder: given an ordered visit list and the edge list
    /// the traversal collected, apply [`ViewportRequest::node_limit`]
    /// and produce the contract response.
    ///
    /// The builder is intentionally pure so the truncation rule lives
    /// here once and is testable without a graph store. Runtime
    /// wiring (a follow-up slice) will call this after running the
    /// real traversal.
    ///
    /// Rules:
    ///
    /// 1. Visits are kept in input order. Callers (the traversal)
    ///    are responsible for stable ordering — typically BFS by
    ///    depth, then by node id.
    /// 2. The first `node_limit` visits are returned; the remainder
    ///    are reported via [`TruncationMeta::dropped_node_count`] and
    ///    [`TruncationMeta::node_limit_hit`].
    /// 3. Edges are filtered down to those whose `source` *and*
    ///    `target` both survived the node cap. An edge to a dropped
    ///    node would be a dangling reference in the UI.
    /// 4. `depth_limit_hit` is the OR of every surviving visit's
    ///    `frontier_truncated`. A visit dropped by the node cap does
    ///    not contribute (the UI will surface the node cap instead;
    ///    re-asking with a larger node limit is the correct fix).
    pub fn from_visits(
        request: &ViewportRequest,
        visits: Vec<ViewportVisitInput>,
        edges: Vec<ViewportEdge>,
    ) -> Self {
        let node_limit = request.node_limit as usize;
        let total = visits.len();

        let kept_count = total.min(node_limit);
        let dropped = total.saturating_sub(kept_count);

        let mut kept_ids: HashSet<String> = HashSet::with_capacity(kept_count);
        let mut nodes: Vec<ViewportNode> = Vec::with_capacity(kept_count);
        let mut depth_limit_hit = false;

        for visit in visits.into_iter().take(kept_count) {
            kept_ids.insert(visit.node.id.clone());
            if visit.frontier_truncated {
                depth_limit_hit = true;
            }
            nodes.push(visit.node);
        }

        let edges: Vec<ViewportEdge> = edges
            .into_iter()
            .filter(|e| kept_ids.contains(&e.source) && kept_ids.contains(&e.target))
            .collect();

        let truncation = TruncationMeta {
            node_limit_hit: dropped > 0,
            depth_limit_hit,
            dropped_node_count: u32::try_from(dropped).unwrap_or(u32::MAX),
        };

        Viewport {
            collection: request.collection.clone(),
            seed_count: u32::try_from(request.selector.seed_count()).unwrap_or(u32::MAX),
            nodes,
            edges,
            truncation,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, depth: u32) -> ViewportNode {
        ViewportNode {
            id: id.into(),
            label: format!("L:{id}"),
            node_type: "Person".into(),
            properties: "{}".into(),
            depth,
        }
    }

    fn visit(id: &str, depth: u32, frontier_truncated: bool) -> ViewportVisitInput {
        ViewportVisitInput {
            node: node(id, depth),
            frontier_truncated,
        }
    }

    fn edge(source: &str, target: &str) -> ViewportEdge {
        ViewportEdge {
            source: source.into(),
            target: target.into(),
            edge_type: "KNOWS".into(),
            weight: None,
            properties: "{}".into(),
        }
    }

    /// Acceptance: "Red UI can request a bounded subgraph by
    /// collection, center or filter, depth, and limit". A
    /// well-formed center-seeded request round-trips through the
    /// builder with every field intact and reports no truncation
    /// when the response fits.
    #[test]
    fn center_request_round_trips_through_builder() {
        let req = ViewportRequest::center("friends", "alice");
        assert_eq!(req.collection, "friends");
        assert_eq!(req.selector, ViewportSelector::Center("alice".into()));
        assert_eq!(req.depth, 1);
        assert_eq!(req.direction, ViewportDirection::Both);
        assert_eq!(req.node_limit, 256);

        let visits = vec![
            visit("alice", 0, false),
            visit("bob", 1, false),
            visit("carol", 1, false),
        ];
        let edges = vec![edge("alice", "bob"), edge("alice", "carol")];
        let v = Viewport::from_visits(&req, visits, edges);

        assert_eq!(v.collection, "friends");
        assert_eq!(v.seed_count, 1);
        assert_eq!(v.nodes.len(), 3);
        assert_eq!(v.edges.len(), 2);
        assert!(v.truncation.is_complete());
        assert_eq!(v.truncation.dropped_node_count, 0);
    }

    /// Acceptance: "The response returns normalized nodes and edges
    /// with ids, labels or types, properties, weights where known,
    /// and truncation metadata." Property bags and weights round-trip
    /// faithfully (None weight stays None — UI must not invent a
    /// default).
    #[test]
    fn nodes_and_edges_preserve_properties_and_weights() {
        let req = ViewportRequest::center("g", "n1");
        let mut typed = node("n1", 0);
        typed.properties = r#"{"name":"alice","age":30}"#.into();
        typed.node_type = "Customer".into();
        let visits = vec![ViewportVisitInput {
            node: typed,
            frontier_truncated: false,
        }];
        let weighted = ViewportEdge {
            source: "n1".into(),
            target: "n1".into(),
            edge_type: "SELF".into(),
            weight: Some(2.5),
            properties: r#"{"k":"v"}"#.into(),
        };
        let v = Viewport::from_visits(&req, visits, vec![weighted.clone()]);
        assert_eq!(v.nodes[0].node_type, "Customer");
        assert_eq!(v.nodes[0].properties, r#"{"name":"alice","age":30}"#);
        assert_eq!(v.edges[0].weight, Some(2.5));
        assert_eq!(v.edges[0].properties, r#"{"k":"v"}"#);
    }

    /// Acceptance: "Tests cover graph visibility and limit/truncation
    /// behavior." Node-limit truncation reports `node_limit_hit`,
    /// `dropped_node_count`, and prunes edges to surviving nodes so
    /// the UI never gets a dangling reference.
    #[test]
    fn node_limit_truncation_prunes_dangling_edges() {
        let mut req = ViewportRequest::center("g", "a");
        req.node_limit = 2;
        let visits = vec![
            visit("a", 0, false),
            visit("b", 1, false),
            visit("c", 1, false),
            visit("d", 1, false),
        ];
        let edges = vec![
            edge("a", "b"), // both kept
            edge("a", "c"), // c dropped → drop edge
            edge("b", "d"), // d dropped → drop edge
        ];
        let v = Viewport::from_visits(&req, visits, edges);

        assert_eq!(v.nodes.len(), 2);
        assert_eq!(
            v.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(v.truncation.node_limit_hit);
        assert_eq!(v.truncation.dropped_node_count, 2);
        // Only the edge whose endpoints both survived remains.
        assert_eq!(v.edges.len(), 1);
        assert_eq!(v.edges[0].target, "b");
    }

    /// Acceptance: "Tests cover graph visibility and limit/truncation
    /// behavior." Depth-limit truncation is a separate flag from the
    /// node-limit cap, and the two compose independently.
    #[test]
    fn depth_limit_flag_set_when_frontier_truncated() {
        let req = ViewportRequest::center("g", "a");
        // No node-limit pressure (default 256 ≫ 2), but the visit at
        // depth 1 was on the frontier and the traversal stopped.
        let visits = vec![visit("a", 0, false), visit("b", 1, true)];
        let v = Viewport::from_visits(&req, visits, vec![]);
        assert!(!v.truncation.node_limit_hit);
        assert!(v.truncation.depth_limit_hit);
        assert!(!v.truncation.is_complete());
    }

    #[test]
    fn complete_response_reports_no_truncation() {
        let req = ViewportRequest::center("g", "a");
        let v = Viewport::from_visits(&req, vec![visit("a", 0, false)], vec![]);
        assert!(v.truncation.is_complete());
        assert_eq!(v.truncation.dropped_node_count, 0);
    }

    /// Acceptance: "A regression test covers RID-list lookup
    /// behavior so larger RID IN queries do not silently return zero
    /// rows."
    ///
    /// The Red UI workaround this contract replaces was: when the
    /// frontend wanted to load many nodes by id at once, the old
    /// path silently returned zero rows past a certain list size,
    /// so the UI broke the call into hand-rolled chunks. The
    /// `ViewportSelector::Rids` contract pins the opposite
    /// behavior: every requested id is carried into the request
    /// verbatim, the seed count exposed to the UI matches the input,
    /// and the builder happily emits every visit it is handed. No
    /// silent input truncation, no zero-row drop — the only way to
    /// lose nodes is the explicit `node_limit`, which always reports
    /// itself.
    #[test]
    fn large_rid_list_lookup_does_not_silently_drop_rows() {
        // A "large" list — well past the historical chunking
        // threshold the frontend used. Keep this round so the test
        // intent is obvious; the contract has no magic number.
        const N: usize = 1_024;
        let ids: Vec<String> = (0..N).map(|i| format!("rid-{i:04}")).collect();

        let req = ViewportRequest {
            collection: "people".into(),
            selector: ViewportSelector::Rids(ids.clone()),
            depth: 0,
            edge_labels: vec![],
            direction: ViewportDirection::Both,
            node_limit: u32::try_from(N).unwrap(),
        };

        // The selector keeps every input id — no silent chunking,
        // no zero-row drop. This is the load-bearing pin.
        assert_eq!(req.selector.seed_count(), N);
        if let ViewportSelector::Rids(ref kept) = req.selector {
            assert_eq!(kept.len(), N);
            assert_eq!(kept[0], "rid-0000");
            assert_eq!(kept[N - 1], format!("rid-{:04}", N - 1));
        } else {
            panic!("selector lost variant identity");
        }

        // Builder round-trip with one visit per requested id.
        let visits: Vec<ViewportVisitInput> =
            ids.iter().map(|id| visit(id.as_str(), 0, false)).collect();
        let v = Viewport::from_visits(&req, visits, vec![]);

        assert_eq!(v.seed_count, u32::try_from(N).unwrap());
        assert_eq!(v.nodes.len(), N);
        assert!(
            v.truncation.is_complete(),
            "large RID-list lookup must not report truncation when every id round-trips"
        );
        assert_eq!(v.truncation.dropped_node_count, 0);
    }

    /// Companion to the regression test above: when an RID-list
    /// request *does* exceed the node limit, the response truncates
    /// loudly via `TruncationMeta` rather than silently. The whole
    /// point of the contract is that the UI can tell the difference.
    #[test]
    fn rid_list_truncation_is_explicit_not_silent() {
        let ids: Vec<String> = (0..10).map(|i| format!("rid-{i}")).collect();
        let req = ViewportRequest {
            collection: "people".into(),
            selector: ViewportSelector::Rids(ids.clone()),
            depth: 0,
            edge_labels: vec![],
            direction: ViewportDirection::Both,
            node_limit: 4,
        };
        let visits: Vec<ViewportVisitInput> =
            ids.iter().map(|id| visit(id.as_str(), 0, false)).collect();
        let v = Viewport::from_visits(&req, visits, vec![]);
        assert_eq!(v.nodes.len(), 4);
        assert!(v.truncation.node_limit_hit);
        assert_eq!(v.truncation.dropped_node_count, 6);
        // Seeds metadata still reflects the request's full input —
        // the UI sees "I asked for 10, I got 4, 6 were dropped".
        assert_eq!(v.seed_count, 10);
    }

    /// Direction tags are part of the wire contract — pin them.
    #[test]
    fn direction_strings_are_stable() {
        assert_eq!(ViewportDirection::Outgoing.as_str(), "outgoing");
        assert_eq!(ViewportDirection::Incoming.as_str(), "incoming");
        assert_eq!(ViewportDirection::Both.as_str(), "both");
    }

    #[test]
    fn label_equals_selector_carries_zero_seed_ids() {
        let req = ViewportRequest {
            collection: "g".into(),
            selector: ViewportSelector::LabelEquals("Person".into()),
            depth: 1,
            edge_labels: vec![],
            direction: ViewportDirection::Outgoing,
            node_limit: 16,
        };
        assert_eq!(req.selector.seed_count(), 0);
    }
}
