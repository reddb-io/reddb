#![allow(dead_code)]

#[path = "grouped/storage_durability/e2e_backup_restore.rs"]
mod e2e_backup_restore;

#[path = "grouped/storage_durability/e2e_fold_dwb_into_wal_crash.rs"]
mod e2e_fold_dwb_into_wal_crash;

#[path = "grouped/storage_durability/e2e_logical_wal_crash.rs"]
mod e2e_logical_wal_crash;

#[path = "grouped/storage_durability/e2e_migrations_bootstrap.rs"]
mod e2e_migrations_bootstrap;

#[path = "grouped/storage_durability/temp_db_cleanup_harness.rs"]
mod temp_db_cleanup_harness;

#[path = "grouped/storage_durability/wal_crash_harness.rs"]
mod wal_crash_harness;
