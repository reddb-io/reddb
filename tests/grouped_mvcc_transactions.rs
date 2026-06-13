#![allow(dead_code)]

#[path = "grouped/mvcc_transactions/docs_transaction_guarantees.rs"]
mod docs_transaction_guarantees;

#[path = "grouped/mvcc_transactions/e2e_cross_model_tx.rs"]
mod e2e_cross_model_tx;

#[path = "grouped/mvcc_transactions/e2e_isolation_levels.rs"]
mod e2e_isolation_levels;

#[path = "grouped/mvcc_transactions/e2e_mvcc_delete_tombstones.rs"]
mod e2e_mvcc_delete_tombstones;

#[path = "grouped/mvcc_transactions/e2e_mvcc_dml_target_scans.rs"]
mod e2e_mvcc_dml_target_scans;

#[path = "grouped/mvcc_transactions/e2e_mvcc_first_committer_wins.rs"]
mod e2e_mvcc_first_committer_wins;

#[path = "grouped/mvcc_transactions/e2e_mvcc_index_recheck.rs"]
mod e2e_mvcc_index_recheck;

#[path = "grouped/mvcc_transactions/e2e_mvcc_logical_lookup.rs"]
mod e2e_mvcc_logical_lookup;

#[path = "grouped/mvcc_transactions/e2e_mvcc_read_resolver_conformance.rs"]
mod e2e_mvcc_read_resolver_conformance;

#[path = "grouped/mvcc_transactions/e2e_mvcc_vacuum.rs"]
mod e2e_mvcc_vacuum;

#[path = "grouped/mvcc_transactions/e2e_savepoint_update_reversal.rs"]
mod e2e_savepoint_update_reversal;

#[path = "grouped/mvcc_transactions/e2e_savepoints.rs"]
mod e2e_savepoints;

#[path = "grouped/mvcc_transactions/e2e_txcommitbatch_wal.rs"]
mod e2e_txcommitbatch_wal;
