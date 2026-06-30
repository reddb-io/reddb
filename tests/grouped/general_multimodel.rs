//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "schema_query_core/e2e_append_only.rs"]
mod e2e_append_only;

#[path = "dml_updates/e2e_compound_assignment.rs"]
mod e2e_compound_assignment;

#[path = "locking_concurrency/e2e_concurrent_writes.rs"]
mod e2e_concurrent_writes;

#[path = "dml_updates/e2e_explicit_update_targets.rs"]
mod e2e_explicit_update_targets;

#[path = "dml_updates/e2e_hot_update.rs"]
mod e2e_hot_update;

#[path = "mvcc_transactions/e2e_isolation_levels.rs"]
mod e2e_isolation_levels;

#[path = "cli_transport/e2e_issue_545_transport_listener_readiness.rs"]
mod e2e_issue_545_transport_listener_readiness;

#[path = "http_grpc_auth/e2e_issue_547_cross_transport_envelope.rs"]
mod e2e_issue_547_cross_transport_envelope;

#[path = "tenancy_policy/e2e_issue_745_red_typed_model_relations.rs"]
mod e2e_issue_745_red_typed_model_relations;

#[path = "multimodel_query/e2e_issue_751_json_patch_path_helpers.rs"]
mod e2e_issue_751_json_patch_path_helpers;

#[path = "runtime_persistence/e2e_meta_json_sidecar_policy.rs"]
mod e2e_meta_json_sidecar_policy;

#[path = "multimodel_query/e2e_multimodel_flow.rs"]
mod e2e_multimodel_flow;

#[path = "dml_updates/e2e_ordered_multimodel_update_batches.rs"]
mod e2e_ordered_multimodel_update_batches;

#[path = "schema_query_core/e2e_reserved_system_fields.rs"]
mod e2e_reserved_system_fields;

#[path = "sql_window/e2e_show_sample.rs"]
mod e2e_show_sample;

#[path = "timeseries_metrics/e2e_slo_descriptor_catalog.rs"]
mod e2e_slo_descriptor_catalog;

#[path = "vcs/e2e_vcs.rs"]
mod e2e_vcs;

#[path = "vcs/e2e_vcs_as_of_enforce.rs"]
mod e2e_vcs_as_of_enforce;

#[path = "vcs/e2e_vcs_as_of_parse.rs"]
mod e2e_vcs_as_of_parse;

#[path = "vcs/e2e_vcs_kv_mvcc_history.rs"]
mod e2e_vcs_kv_mvcc_history;

#[path = "vcs/e2e_vcs_kv_connection_independent.rs"]
mod e2e_vcs_kv_connection_independent;

#[path = "vcs/e2e_vcs_kv_write_conflict.rs"]
mod e2e_vcs_kv_write_conflict;

#[path = "vcs/e2e_vcs_document_mvcc_history.rs"]
mod e2e_vcs_document_mvcc_history;

#[path = "vcs/e2e_vcs_graph_mvcc_history.rs"]
mod e2e_vcs_graph_mvcc_history;

#[path = "vcs/e2e_vcs_vector_mvcc_history.rs"]
mod e2e_vcs_vector_mvcc_history;

#[path = "vcs/e2e_vcs_opt_in.rs"]
mod e2e_vcs_opt_in;

#[path = "vcs/e2e_vcs_phase5.rs"]
mod e2e_vcs_phase5;

#[path = "vcs/e2e_vcs_rql_working_set.rs"]
mod e2e_vcs_rql_working_set;

#[path = "ai_local_vector/integration_external_env.rs"]
mod integration_external_env;

#[path = "runtime_persistence/integration_persistent_grimms_scale.rs"]
mod integration_persistent_grimms_scale;

#[path = "runtime_persistence/integration_persistent_multimodel.rs"]
mod integration_persistent_multimodel;

#[path = "../integration_tree.rs"]
mod integration_tree;

#[path = "dml_updates/unit_hot_update.rs"]
mod unit_hot_update;
