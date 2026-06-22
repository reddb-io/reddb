//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "../support/mod.rs"]
mod support;

#[path = "config_tier/shared.rs"]
mod config_tier_shared;

#[path = "../audit_query_endpoint.rs"]
mod audit_query_endpoint;

#[path = "audit/audit_rotation.rs"]
mod audit_rotation;

#[path = "audit/audit_structured.rs"]
mod audit_structured;

#[path = "control_feedback/control_evidence_matrix_docs.rs"]
mod control_evidence_matrix_docs;

#[path = "mvcc_transactions/docs_transaction_guarantees.rs"]
mod docs_transaction_guarantees;

#[path = "audit/e2e_audit_slow_routing.rs"]
mod e2e_audit_slow_routing;

#[path = "config_tier/e2e_config_crud.rs"]
mod e2e_config_crud;

#[path = "config_tier/e2e_config_matrix.rs"]
mod e2e_config_matrix;

#[path = "control_feedback/e2e_control_events_operational.rs"]
mod e2e_control_events_operational;

#[path = "events_retention/e2e_events_backfill.rs"]
mod e2e_events_backfill;

#[path = "events_retention/e2e_events_cdc_rid.rs"]
mod e2e_events_cdc_rid;

#[path = "events_retention/e2e_events_foundation.rs"]
mod e2e_events_foundation;

#[path = "control_feedback/e2e_evidence_export.rs"]
mod e2e_evidence_export;

#[path = "control_feedback/e2e_feedback_regression_pack.rs"]
mod e2e_feedback_regression_pack;

#[path = "control_feedback/e2e_issue_548_error_message_audit.rs"]
mod e2e_issue_548_error_message_audit;

#[path = "../e2e_issue_593_materialized_view_persistence.rs"]
mod e2e_issue_593_materialized_view_persistence;

#[path = "../e2e_issue_594_materialized_view_backing.rs"]
mod e2e_issue_594_materialized_view_backing;

#[path = "../e2e_issue_595_materialized_view_atomic_refresh.rs"]
mod e2e_issue_595_materialized_view_atomic_refresh;

#[path = "runtime_persistence/e2e_materialized_view_refresh_every.rs"]
mod e2e_materialized_view_refresh_every;

#[path = "runtime_persistence/e2e_query_audit.rs"]
mod e2e_query_audit;

#[path = "sql_window/e2e_views.rs"]
mod e2e_views;

#[path = "control_feedback/feedback_regression.rs"]
mod feedback_regression;

#[path = "surface_contracts/public_surface_contract_matrix.rs"]
mod public_surface_contract_matrix;

#[path = "surface_contracts/regress.rs"]
mod regress;
