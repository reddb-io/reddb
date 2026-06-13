#![allow(dead_code)]

#[path = "grouped/locking_concurrency/e2e_advisory_locks.rs"]
mod e2e_advisory_locks;

#[path = "grouped/locking_concurrency/e2e_concurrent_writes.rs"]
mod e2e_concurrent_writes;

#[path = "grouped/locking_concurrency/e2e_ddl_concurrency.rs"]
mod e2e_ddl_concurrency;

#[path = "grouped/locking_concurrency/e2e_locking_reads.rs"]
mod e2e_locking_reads;

#[path = "grouped/locking_concurrency/unit_locking.rs"]
mod unit_locking;
