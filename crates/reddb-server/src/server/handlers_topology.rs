//! HTTP handler for `GET /v1/topology/graph` (issue #803).
//!
//! Refreshes the built-in `red.topology.cluster` graph from live cluster state,
//! then serves the aggregated PRD #794 document (`nodes`, `edges`, `groups`,
//! `metadata`). Read-only and cacheable at the edge; `metadata.cache_status`
//! lets a polling client tell a reused materialisation (`hit`) from a cold
//! recompute (`cold`).

use super::transport::{json_error, json_response, HttpResponse};
use super::RedDBServer;
use crate::application::topology_collections as topo;

impl RedDBServer {
    pub(crate) fn handle_topology_graph(&self) -> HttpResponse {
        let outcome = match topo::refresh_from_runtime(&self.runtime) {
            Ok(outcome) => outcome,
            Err(err) => return json_error(500, err.to_string()),
        };
        match topo::build_graph_doc(&self.runtime, outcome.cache_status()) {
            Ok(doc) => json_response(200, doc.to_json()),
            Err(err) => json_error(500, err.to_string()),
        }
    }
}
