//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "../audit_query_endpoint.rs"]
mod audit_query_endpoint;

#[path = "../audit_rotation.rs"]
mod audit_rotation;

#[path = "../audit_structured.rs"]
mod audit_structured;

#[path = "../control_evidence_matrix_docs.rs"]
mod control_evidence_matrix_docs;

#[path = "../docs_transaction_guarantees.rs"]
mod docs_transaction_guarantees;

#[path = "../e2e_audit_slow_routing.rs"]
mod e2e_audit_slow_routing;

#[path = "../e2e_config_crud.rs"]
mod e2e_config_crud;

#[path = "../e2e_config_matrix.rs"]
mod e2e_config_matrix;

#[path = "../e2e_control_events_operational.rs"]
mod e2e_control_events_operational;

#[path = "../e2e_events_backfill.rs"]
mod e2e_events_backfill;

#[path = "../e2e_events_cdc_rid.rs"]
mod e2e_events_cdc_rid;

#[path = "../e2e_events_foundation.rs"]
mod e2e_events_foundation;

#[path = "../e2e_evidence_export.rs"]
mod e2e_evidence_export;

#[path = "../e2e_feedback_regression_pack.rs"]
mod e2e_feedback_regression_pack;

#[path = "../e2e_issue_548_error_message_audit.rs"]
mod e2e_issue_548_error_message_audit;

#[path = "../e2e_issue_593_materialized_view_persistence.rs"]
mod e2e_issue_593_materialized_view_persistence;

#[path = "../e2e_issue_594_materialized_view_backing.rs"]
mod e2e_issue_594_materialized_view_backing;

#[path = "../e2e_issue_595_materialized_view_atomic_refresh.rs"]
mod e2e_issue_595_materialized_view_atomic_refresh;

#[path = "../e2e_materialized_view_refresh_every.rs"]
mod e2e_materialized_view_refresh_every;

#[path = "../e2e_query_audit.rs"]
mod e2e_query_audit;

#[path = "../e2e_views.rs"]
mod e2e_views;

#[path = "../feedback_regression.rs"]
mod feedback_regression;

#[path = "../public_surface_contract_matrix.rs"]
mod public_surface_contract_matrix;

#[path = "../regress.rs"]
mod regress;
