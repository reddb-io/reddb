//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "config_tier/shared.rs"]
mod config_tier_shared;

#[path = "chaos/chaos_ack_n_timeout_fail_closed.rs"]
mod chaos_ack_n_timeout_fail_closed;

#[path = "chaos/chaos_backend_unavailable_restore.rs"]
mod chaos_backend_unavailable_restore;

#[path = "chaos/chaos_concurrent_lease_acquire.rs"]
mod chaos_concurrent_lease_acquire;

#[path = "chaos/chaos_migration_batch_resume.rs"]
mod chaos_migration_batch_resume;

#[path = "chaos/chaos_migration_sigkill_resume.rs"]
mod chaos_migration_sigkill_resume;

#[path = "chaos/chaos_promote_refused_when_lease_held.rs"]
mod chaos_promote_refused_when_lease_held;

#[path = "chaos/chaos_wal_chain_break.rs"]
mod chaos_wal_chain_break;

#[path = "chaos/chaos_wal_segment_corruption.rs"]
mod chaos_wal_segment_corruption;

#[path = "chaos/chaos_wal_segment_missing.rs"]
mod chaos_wal_segment_missing;

#[path = "pitr_drills/drill_backup_restore_round_trip.rs"]
mod drill_backup_restore_round_trip;

#[path = "pitr_drills/drill_pitr_byte_identical.rs"]
mod drill_pitr_byte_identical;

#[path = "pitr_drills/drill_pitr_chain_break_within_window.rs"]
mod drill_pitr_chain_break_within_window;

#[path = "pitr_drills/drill_pitr_target_time.rs"]
mod drill_pitr_target_time;

#[path = "storage_durability/e2e_backup_restore.rs"]
mod e2e_backup_restore;

#[path = "storage_durability/e2e_fold_dwb_into_wal_crash.rs"]
mod e2e_fold_dwb_into_wal_crash;

#[path = "runtime_persistence/e2e_fold_dwb_into_wal_policy.rs"]
mod e2e_fold_dwb_into_wal_policy;

#[path = "runtime_persistence/e2e_fold_pager_meta_policy.rs"]
mod e2e_fold_pager_meta_policy;

#[path = "config_tier/e2e_issue_480_tier_default_promotion_contract.rs"]
mod e2e_issue_480_tier_default_promotion_contract;

#[path = "storage_durability/e2e_logical_wal_crash.rs"]
mod e2e_logical_wal_crash;

#[path = "storage_durability/e2e_migrations_bootstrap.rs"]
mod e2e_migrations_bootstrap;

#[path = "storage_durability/e2e_storage_integrity_errors.rs"]
mod e2e_storage_integrity_errors;

#[path = "runtime_persistence/e2e_seqn_journal_policy.rs"]
mod e2e_seqn_journal_policy;

#[path = "config_tier/e2e_shm_provisioning.rs"]
mod e2e_shm_provisioning;

#[path = "config_tier/e2e_tier_wiring.rs"]
mod e2e_tier_wiring;

#[path = "mvcc_transactions/e2e_txcommitbatch_wal.rs"]
mod e2e_txcommitbatch_wal;

#[path = "runtime_persistence/fold_dwb_into_wal_bench.rs"]
mod fold_dwb_into_wal_bench;

#[path = "http_grpc_auth/lease_atomic_http_opt_in.rs"]
mod lease_atomic_http_opt_in;

#[path = "storage_durability/wal_crash_harness.rs"]
mod wal_crash_harness;
