//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "../e2e_append_only.rs"]
mod e2e_append_only;

#[path = "../e2e_compound_assignment.rs"]
mod e2e_compound_assignment;

#[path = "../e2e_concurrent_writes.rs"]
mod e2e_concurrent_writes;

#[path = "../e2e_explicit_update_targets.rs"]
mod e2e_explicit_update_targets;

#[path = "../e2e_hot_update.rs"]
mod e2e_hot_update;

#[path = "../e2e_isolation_levels.rs"]
mod e2e_isolation_levels;

#[path = "../e2e_issue_545_transport_listener_readiness.rs"]
mod e2e_issue_545_transport_listener_readiness;

#[path = "../e2e_issue_547_cross_transport_envelope.rs"]
mod e2e_issue_547_cross_transport_envelope;

#[path = "../e2e_issue_745_red_typed_model_relations.rs"]
mod e2e_issue_745_red_typed_model_relations;

#[path = "../e2e_issue_751_json_patch_path_helpers.rs"]
mod e2e_issue_751_json_patch_path_helpers;

#[path = "../e2e_meta_json_sidecar_policy.rs"]
mod e2e_meta_json_sidecar_policy;

#[path = "../e2e_multimodel_flow.rs"]
mod e2e_multimodel_flow;

#[path = "../e2e_ordered_multimodel_update_batches.rs"]
mod e2e_ordered_multimodel_update_batches;

#[path = "../e2e_reserved_system_fields.rs"]
mod e2e_reserved_system_fields;

#[path = "../e2e_show_sample.rs"]
mod e2e_show_sample;

#[path = "../e2e_slo_descriptor_catalog.rs"]
mod e2e_slo_descriptor_catalog;

#[path = "../e2e_vcs.rs"]
mod e2e_vcs;

#[path = "../e2e_vcs_as_of_enforce.rs"]
mod e2e_vcs_as_of_enforce;

#[path = "../e2e_vcs_as_of_parse.rs"]
mod e2e_vcs_as_of_parse;

#[path = "../e2e_vcs_opt_in.rs"]
mod e2e_vcs_opt_in;

#[path = "../e2e_vcs_phase5.rs"]
mod e2e_vcs_phase5;

#[path = "../integration_external_env.rs"]
mod integration_external_env;

#[path = "../integration_persistent_grimms_scale.rs"]
mod integration_persistent_grimms_scale;

#[path = "../integration_persistent_multimodel.rs"]
mod integration_persistent_multimodel;

#[path = "../integration_tree.rs"]
mod integration_tree;

#[path = "../unit_hot_update.rs"]
mod unit_hot_update;
