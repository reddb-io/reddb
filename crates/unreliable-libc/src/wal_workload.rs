//! A seed-driven, representative WAL write workload.
//!
//! Reuses the engine's real WAL framing ([`reddb_file::wal_header`] +
//! [`reddb_file::wal_record`]) so the durability path under test is the one the
//! engine actually ships. Each transaction is `Begin` → one or more `PageWrite`
//! → `Commit`, with `Commit` followed by an `fsync`; periodic checkpoints stamp
//! the dual superblock with the highest durably-committed LSN.
//!
//! Every durable byte is written with `write_all`/`sync_all`, so under the
//! `unreliable-libc` shim a short write torns the trailing record and an `EIO`
//! surfaces as an [`io::Error`] that stops the workload *without* advancing the
//! committed frontier or the superblock — exactly how a correct writer behaves.
//! A power-cut (`SIGKILL`) simply terminates the process mid-write.

use crate::prng::SplitMix64;
use crate::superblock::{self, Superblock};
use reddb_file::wal_header::{encode_wal_file_header, WAL_FILE_VERSION};
use reddb_file::wal_record::{encode_main_wal_record_frame, MainWalRecordFrame};
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

/// File names produced by the workload inside the working directory.
pub const WAL_FILE_NAME: &str = "wal.log";
pub const SUPERBLOCK_FILE_NAME: &str = "super.block";

/// The fixed term stamped on every record (term fencing is a later DST slice).
const WORKLOAD_TERM: u64 = 1;

/// What the workload believed it durably persisted (only meaningful for a
/// fault-free run; under faults the process may be killed before returning).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadOutcome {
    pub transactions_committed: u64,
    pub last_committed_lsn: u64,
    pub checkpoints: u64,
    pub format_version: u8,
}

/// Drive a representative WAL workload in `dir`, deriving every choice from
/// `seed`. Returns the durable outcome, or the first I/O error that stops it.
pub fn run_wal_workload(dir: &Path, seed: u64) -> io::Result<WorkloadOutcome> {
    let mut rng = SplitMix64::new(seed ^ 0x5741_4C5F_5345_4544); // "WAL_SEED"

    let wal_path = dir.join(WAL_FILE_NAME);
    let sb_path = dir.join(SUPERBLOCK_FILE_NAME);

    let mut wal = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&wal_path)?;
    wal.write_all(&encode_wal_file_header())?;
    wal.sync_all()?;

    let num_tx = 8 + rng.below(24);
    let checkpoint_interval = 3 + rng.below(3);

    let mut last_committed_lsn = 0u64;
    let mut generation = 0u64;
    let mut checkpoints = 0u64;
    let mut committed = 0u64;

    for tx_id in 1..=num_tx {
        append_frame(&mut wal, &MainWalRecordFrame::Begin { tx_id })?;

        let pages = 1 + rng.below(4);
        for page_index in 0..pages {
            let page_id = u32::try_from(tx_id * 16 + page_index).unwrap_or(u32::MAX);
            let payload_len = 1 + usize::try_from(rng.below(48)).unwrap_or(0);
            let mut data = vec![0u8; payload_len];
            rng.fill_bytes(&mut data);
            append_frame(
                &mut wal,
                &MainWalRecordFrame::PageWrite {
                    tx_id,
                    page_id,
                    data,
                },
            )?;
        }

        append_frame(&mut wal, &MainWalRecordFrame::Commit { tx_id })?;
        // The commit is durable only once the fsync returns success.
        wal.sync_all()?;
        last_committed_lsn = tx_id;
        committed = tx_id;

        if tx_id % checkpoint_interval == 0 {
            append_frame(&mut wal, &MainWalRecordFrame::Checkpoint { lsn: tx_id })?;
            wal.sync_all()?;
            write_superblock(&sb_path, generation, last_committed_lsn)?;
            generation += 1;
            checkpoints += 1;
        }
    }

    // Final checkpoint so the superblock reflects the last commit.
    write_superblock(&sb_path, generation, last_committed_lsn)?;
    checkpoints += 1;

    Ok(WorkloadOutcome {
        transactions_committed: committed,
        last_committed_lsn,
        checkpoints,
        format_version: WAL_FILE_VERSION,
    })
}

fn append_frame(wal: &mut File, frame: &MainWalRecordFrame) -> io::Result<()> {
    // Encode to a single buffer and write it in one `write_all`, so a short
    // write torns within a record boundary rather than between fields.
    let bytes = encode_main_wal_record_frame(frame, WORKLOAD_TERM)?;
    wal.write_all(&bytes)
}

fn write_superblock(path: &Path, generation: u64, committed_lsn: u64) -> io::Result<()> {
    let slot = Superblock {
        generation,
        committed_lsn,
    }
    .encode();
    // Do not truncate: each checkpoint overwrites only its own slot and must
    // preserve the other slot's durable copy.
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)?;
    file.seek(SeekFrom::Start(superblock::slot_offset(generation)))?;
    file.write_all(&slot)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fault_free_workload_commits_everything() {
        let dir = tempfile::tempdir().unwrap();
        let outcome = run_wal_workload(dir.path(), 123).unwrap();
        assert!(outcome.transactions_committed >= 8);
        assert_eq!(outcome.last_committed_lsn, outcome.transactions_committed);
        assert!(dir.path().join(WAL_FILE_NAME).exists());
        assert!(dir.path().join(SUPERBLOCK_FILE_NAME).exists());
    }

    #[test]
    fn same_seed_produces_identical_wal_bytes() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        run_wal_workload(dir_a.path(), 777).unwrap();
        run_wal_workload(dir_b.path(), 777).unwrap();
        let wal_a = std::fs::read(dir_a.path().join(WAL_FILE_NAME)).unwrap();
        let wal_b = std::fs::read(dir_b.path().join(WAL_FILE_NAME)).unwrap();
        assert_eq!(wal_a, wal_b, "same seed must yield byte-identical WAL");
    }
}
