//! Graph storage stable contract surface — issue #744.
//!
//! Red UI's graph explorer must be able to request a bounded subgraph
//! (by collection, center node or filter, traversal depth, and result
//! limit) and render the response without reaching into
//! `engine::graph_store`, `unified::graph_dsl`, or the runtime's
//! `RuntimeGraphNeighborhoodResult` / `RuntimeGraphEdge` types. Those
//! internals churn faster than the UI contract can.
//!
//! This module is the stable mediation layer: plain-data request and
//! response types in [`viewport`] that Red UI binds to. Wiring the
//! runtime call that turns a [`viewport::ViewportRequest`] into a
//! populated [`viewport::Viewport`] over the live graph store is a
//! follow-up slice in PRD #735 — the contract types in this module are
//! what those wiring slices target and do not change when they land.
//!
//! Today it ships exactly one submodule:
//!
//! - [`viewport`] — request / response contract plus pure
//!   [`viewport::Viewport::from_visits`] builder that applies limit-
//!   based truncation deterministically.

pub mod viewport;
