//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "docs_kv_queue/e2e_document_kv_compound_updates.rs"]
mod e2e_document_kv_compound_updates;

#[path = "docs_kv_queue/e2e_issue_1363_document_where_id_and_body.rs"]
mod e2e_issue_1363_document_where_id_and_body;

#[path = "docs_kv_queue/e2e_documents_first_class_crud.rs"]
mod e2e_documents_first_class_crud;

#[path = "docs_kv_queue/e2e_issue_535_red_queues_virtual_table.rs"]
mod e2e_issue_535_red_queues_virtual_table;

#[path = "docs_kv_queue/e2e_issue_540_documents_basics.rs"]
mod e2e_issue_540_documents_basics;

#[path = "docs_kv_queue/e2e_issue_541_kv_namespaced_keys.rs"]
mod e2e_issue_541_kv_namespaced_keys;

#[path = "docs_kv_queue/e2e_issue_550_documents_list_filter_pagination.rs"]
mod e2e_issue_550_documents_list_filter_pagination;

#[path = "docs_kv_queue/e2e_issue_551_documents_sql_json_access.rs"]
mod e2e_issue_551_documents_sql_json_access;

#[path = "docs_kv_queue/e2e_issue_552_documents_patch_set_intermediates.rs"]
mod e2e_issue_552_documents_patch_set_intermediates;

#[path = "docs_kv_queue/e2e_issue_555_documents_sql_aggregates.rs"]
mod e2e_issue_555_documents_sql_aggregates;

#[path = "tenancy_policy/e2e_issue_755_queue_policy_aware.rs"]
mod e2e_issue_755_queue_policy_aware;

#[path = "docs_kv_queue/e2e_kv_namespaced_keys.rs"]
mod e2e_kv_namespaced_keys;

#[path = "docs_kv_queue/e2e_queue_lifecycle_telemetry.rs"]
mod e2e_queue_lifecycle_telemetry;

#[path = "docs_kv_queue/e2e_red_queue_pending.rs"]
mod e2e_red_queue_pending;

#[path = "docs_kv_queue/integration_queue_timeseries.rs"]
mod integration_queue_timeseries;
