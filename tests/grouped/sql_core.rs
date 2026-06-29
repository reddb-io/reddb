//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "locking_concurrency/e2e_advisory_locks.rs"]
mod e2e_advisory_locks;

#[path = "schema_query_core/e2e_composite_index.rs"]
mod e2e_composite_index;

#[path = "mvcc_transactions/e2e_cross_model_tx.rs"]
mod e2e_cross_model_tx;

#[path = "locking_concurrency/e2e_ddl_concurrency.rs"]
mod e2e_ddl_concurrency;

#[path = "../e2e_ddl_drop_foundation.rs"]
mod e2e_ddl_drop_foundation;

#[path = "schema_query_core/e2e_index_replay.rs"]
mod e2e_index_replay;

#[path = "tenancy_policy/e2e_issue_753_ddl_policy_aware.rs"]
mod e2e_issue_753_ddl_policy_aware;

#[path = "locking_concurrency/e2e_locking_reads.rs"]
mod e2e_locking_reads;

#[path = "mvcc_transactions/e2e_mvcc_delete_tombstones.rs"]
mod e2e_mvcc_delete_tombstones;

#[path = "mvcc_transactions/e2e_mvcc_dml_target_scans.rs"]
mod e2e_mvcc_dml_target_scans;

#[path = "mvcc_transactions/e2e_mvcc_first_committer_wins.rs"]
mod e2e_mvcc_first_committer_wins;

#[path = "mvcc_transactions/e2e_mvcc_index_recheck.rs"]
mod e2e_mvcc_index_recheck;

#[path = "mvcc_transactions/e2e_mvcc_logical_lookup.rs"]
mod e2e_mvcc_logical_lookup;

#[path = "mvcc_transactions/e2e_mvcc_read_resolver_conformance.rs"]
mod e2e_mvcc_read_resolver_conformance;

#[path = "mvcc_transactions/e2e_mvcc_vacuum.rs"]
mod e2e_mvcc_vacuum;

#[path = "dml_updates/e2e_ordered_row_update_batches.rs"]
mod e2e_ordered_row_update_batches;

#[path = "schema_query_core/e2e_red_collections_acceptance.rs"]
mod e2e_red_collections_acceptance;

#[path = "schema_query_core/e2e_red_schema.rs"]
mod e2e_red_schema;

#[path = "schema_query_core/e2e_repro_stale_index_post_insert.rs"]
mod e2e_repro_stale_index_post_insert;

#[path = "dml_updates/e2e_returning.rs"]
mod e2e_returning;

#[path = "schema_query_core/e2e_rid_row_envelope.rs"]
mod e2e_rid_row_envelope;

#[path = "mvcc_transactions/e2e_savepoint_update_reversal.rs"]
mod e2e_savepoint_update_reversal;

#[path = "mvcc_transactions/e2e_savepoints.rs"]
mod e2e_savepoints;

#[path = "schema_query_core/e2e_select_range_after_index.rs"]
mod e2e_select_range_after_index;

#[path = "sql_window/e2e_sql_cte.rs"]
mod e2e_sql_cte;

#[path = "schema_query_core/e2e_statement_execution_contract.rs"]
mod e2e_statement_execution_contract;

#[path = "../e2e_txcommitbatch_explicit_tx.rs"]
mod e2e_txcommitbatch_explicit_tx;

#[path = "mvcc_transactions/e2e_transactional_reservation_recipe.rs"]
mod e2e_transactional_reservation_recipe;

#[path = "../e2e_update_conformance_pack.rs"]
mod e2e_update_conformance_pack;

#[path = "multimodel_query/integration_create_table_partition.rs"]
mod integration_create_table_partition;

#[path = "multimodel_query/integration_entity_query.rs"]
mod integration_entity_query;

#[path = "locking_concurrency/unit_locking.rs"]
mod unit_locking;
