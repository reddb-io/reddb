//! Runtime adapters for ports that carry application invariants.
//!
//! Entity enforces collection contracts, graph resolves projections and limits,
//! and VCS owns repository consistency. Schema, tree, admin, and native retain
//! command validation and lifecycle transitions rather than mirroring runtime calls.

pub(crate) use super::*;

#[path = "ports_impls_admin.rs"]
mod admin;
#[path = "ports_impls_entity.rs"]
mod entity;
pub(crate) use crate::application::collection_contract_enforcer::build_row_update_contract_plan;
pub(crate) use crate::application::collection_contract_enforcer::normalize_row_update_assignment_with_plan;
pub(crate) use crate::application::collection_contract_enforcer::normalize_row_update_value_for_rule;
pub(crate) use entity::entity_row_fields_snapshot;
#[path = "ports_impls_graph.rs"]
mod graph;
#[path = "ports_impls_native.rs"]
mod native;
#[path = "ports_impls_schema.rs"]
mod schema;
#[path = "ports_impls_tree.rs"]
mod tree;
#[path = "ports_impls_vcs.rs"]
mod vcs;
