#![allow(dead_code)]

#[path = "grouped/chaos/chaos_ack_n_timeout_fail_closed.rs"]
mod chaos_ack_n_timeout_fail_closed;

#[path = "grouped/chaos/chaos_backend_unavailable_restore.rs"]
mod chaos_backend_unavailable_restore;

#[path = "grouped/chaos/chaos_concurrent_lease_acquire.rs"]
mod chaos_concurrent_lease_acquire;

#[path = "grouped/chaos/chaos_migration_batch_resume.rs"]
mod chaos_migration_batch_resume;

#[path = "grouped/chaos/chaos_migration_sigkill_resume.rs"]
mod chaos_migration_sigkill_resume;

#[path = "grouped/chaos/chaos_promote_refused_when_lease_held.rs"]
mod chaos_promote_refused_when_lease_held;

#[path = "grouped/chaos/chaos_replica_apply_divergence.rs"]
mod chaos_replica_apply_divergence;

#[path = "grouped/chaos/chaos_replica_apply_gap.rs"]
mod chaos_replica_apply_gap;

#[path = "grouped/chaos/chaos_wal_chain_break.rs"]
mod chaos_wal_chain_break;

#[path = "grouped/chaos/chaos_wal_segment_corruption.rs"]
mod chaos_wal_segment_corruption;

#[path = "grouped/chaos/chaos_wal_segment_missing.rs"]
mod chaos_wal_segment_missing;
