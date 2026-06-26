//! `unreliable-libc` — DST Fatia 0 (#1351).
//!
//! Two reusable pieces ship here, alongside the standalone `LD_PRELOAD` shim
//! compiled by `build.rs` from `csrc/unreliable_libc.c`:
//!
//! * [`wal_workload`] — a seed-driven, representative WAL write workload that
//!   reuses the real [`reddb_file`] WAL framing (`wal_header` + `wal_record`)
//!   and a dual-superblock checkpoint, plus a small alternating-slot superblock
//!   contract. The `wal_workload` binary runs this under the shim.
//! * [`oracle`] — the **shared recovery-invariant assertion oracle** that later
//!   DST slices reuse: WAL recovers as the longest valid prefix, monotonic LSN,
//!   no torn/partial committed record visible after recovery, intact dual
//!   superblocks, and CRC/checksum integrity.
//! * [`vfs`] — the in-process counterpart to the shim (DST Fatia, #1355): a
//!   minimal `Vfs` / `VfsFile` durable-I/O trait pair with a production-default
//!   [`StdVfs`](vfs::StdVfs) and a seed-driven, fault-injecting
//!   [`SimVfs`](vfs::SimVfs) (torn writes, dropped / reordered `fsync`,
//!   `ENOSPC`, partial rename). The workload routes every durable write through
//!   it, so an in-process power-cut enumeration can reuse the same `oracle`.
//!
//! The shim makes the real libc durability path (`write`/`pwrite`/`fsync`/
//! `fdatasync`/`rename`) fail with `EIO` and short writes, plus a seed-driven
//! "freeze after N syscalls then SIGKILL" power-cut. Everything is controlled by
//! a single seed, so any discovered failure reproduces exactly via `SEED=<n>`.

// The workspace denies `unwrap`/truncating casts on new code; this is internal
// test tooling that mirrors `reddb-file`'s legacy allows for the same lints.
#![allow(clippy::unwrap_used)]

pub mod oracle;
pub mod prng;
pub mod superblock;
pub mod vfs;
pub mod wal_workload;

pub use oracle::{recover_and_check, RecoveryError, RecoveryReport};
pub use prng::SplitMix64;
pub use vfs::{OpenMode, SimFaultConfig, SimVfs, StdVfs, Vfs, VfsFile};
pub use wal_workload::{
    decode_manifest, run_wal_workload, run_wal_workload_on, WorkloadOutcome, MANIFEST_FILE_NAME,
};
