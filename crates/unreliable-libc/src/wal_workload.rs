//! A seed-driven, representative WAL write workload.
//!
//! Reuses the engine's real WAL framing ([`reddb_file::wal_header`] +
//! [`reddb_file::wal_record`]) so the durability path under test is the one the
//! engine actually ships. Each transaction is `Begin` → one or more `PageWrite`
//! → `Commit`, with `Commit` followed by an `fsync`; periodic checkpoints stamp
//! the dual superblock with the highest durably-committed LSN and write an
//! atomically-renamed `commit.manifest` (the embedded `.rdb` manifest analog).
//!
//! Every durable byte is written with `write_all`/`sync_all` through a [`Vfs`],
//! so the same workload runs against the real filesystem ([`StdVfs`], the
//! production default) and against the in-process fault-injecting [`SimVfs`].
//! Under the `unreliable-libc` `LD_PRELOAD` shim, a short write torns the
//! trailing record and an `EIO` surfaces as an [`io::Error`] that stops the
//! workload *without* advancing the committed frontier or the superblock —
//! exactly how a correct writer behaves. A power-cut simply terminates the
//! workload mid-write.

use crate::prng::SplitMix64;
use crate::superblock::{self, Superblock};
use crate::vfs::{OpenMode, StdVfs, Vfs, VfsFile};
use reddb_file::wal_header::{encode_wal_file_header, WAL_FILE_VERSION};
use reddb_file::wal_record::{encode_main_wal_record_frame, MainWalRecordFrame};
use std::io::{self, SeekFrom};
use std::path::Path;

/// File names produced by the workload inside the working directory.
pub const WAL_FILE_NAME: &str = "wal.log";
pub const SUPERBLOCK_FILE_NAME: &str = "super.block";
/// Atomically-renamed checkpoint manifest (the embedded `.rdb` manifest analog).
pub const MANIFEST_FILE_NAME: &str = "commit.manifest";
/// Temp file the manifest is staged in before the atomic rename.
pub const MANIFEST_TEMP_NAME: &str = "commit.manifest.tmp";

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

/// Drive a representative WAL workload in `dir` on the real filesystem,
/// deriving every choice from `seed`. This is the production-default path and
/// its on-disk bytes are unchanged from a direct `std::fs` writer.
pub fn run_wal_workload(dir: &Path, seed: u64) -> io::Result<WorkloadOutcome> {
    run_wal_workload_on(&StdVfs, dir, seed)
}

/// Drive the workload routing every durable write through `vfs`. The same code
/// path serves [`StdVfs`] (production) and [`SimVfs`](crate::vfs::SimVfs)
/// (in-process fault injection), so the durability protocol under test is
/// identical in both.
pub fn run_wal_workload_on<V: Vfs>(vfs: &V, dir: &Path, seed: u64) -> io::Result<WorkloadOutcome> {
    let mut rng = SplitMix64::new(seed ^ 0x5741_4C5F_5345_4544); // "WAL_SEED"

    let wal_path = dir.join(WAL_FILE_NAME);
    let sb_path = dir.join(SUPERBLOCK_FILE_NAME);

    let mut wal = vfs.open(&wal_path, OpenMode::create_truncate())?;
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
            write_superblock(vfs, &sb_path, generation, last_committed_lsn)?;
            write_manifest_atomic(vfs, dir, generation, last_committed_lsn)?;
            generation += 1;
            checkpoints += 1;
        }
    }

    // Final checkpoint so the superblock + manifest reflect the last commit.
    write_superblock(vfs, &sb_path, generation, last_committed_lsn)?;
    write_manifest_atomic(vfs, dir, generation, last_committed_lsn)?;
    checkpoints += 1;

    Ok(WorkloadOutcome {
        transactions_committed: committed,
        last_committed_lsn,
        checkpoints,
        format_version: WAL_FILE_VERSION,
    })
}

fn append_frame<F: VfsFile>(wal: &mut F, frame: &MainWalRecordFrame) -> io::Result<()> {
    // Encode to a single buffer and write it in one `write_all`, so a short
    // write torns within a record boundary rather than between fields.
    let bytes = encode_main_wal_record_frame(frame, WORKLOAD_TERM)?;
    wal.write_all(&bytes)
}

fn write_superblock<V: Vfs>(
    vfs: &V,
    path: &Path,
    generation: u64,
    committed_lsn: u64,
) -> io::Result<()> {
    let slot = Superblock {
        generation,
        committed_lsn,
    }
    .encode();
    // Do not truncate: each checkpoint overwrites only its own slot and must
    // preserve the other slot's durable copy.
    let mut file = vfs.open(path, OpenMode::create_keep())?;
    file.seek(SeekFrom::Start(superblock::slot_offset(generation)))?;
    file.write_all(&slot)?;
    file.sync_all()
}

/// Write the checkpoint manifest atomically: stage into a temp file, fsync it,
/// rename it over the live manifest, then fsync the directory. This is the
/// embedded `.rdb` manifest's durability protocol; under [`SimVfs`] it exercises
/// the partial-rename fault, and a crash must always leave the manifest as the
/// old value, the new value, or absent — never a fabricated commit ahead of the
/// WAL.
fn write_manifest_atomic<V: Vfs>(
    vfs: &V,
    dir: &Path,
    generation: u64,
    committed_lsn: u64,
) -> io::Result<()> {
    let temp = dir.join(MANIFEST_TEMP_NAME);
    let live = dir.join(MANIFEST_FILE_NAME);
    let bytes = encode_manifest(generation, committed_lsn);

    let mut file = vfs.open(&temp, OpenMode::create_truncate())?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    vfs.rename(&temp, &live)?;
    vfs.sync_dir(dir)
}

/// Fixed-size manifest record: magic, generation, committed LSN, CRC, padding.
pub const MANIFEST_BYTES: usize = 32;
const MANIFEST_MAGIC: &[u8; 8] = b"DSTMNFS1";

/// Encode a checkpoint manifest.
pub fn encode_manifest(generation: u64, committed_lsn: u64) -> [u8; MANIFEST_BYTES] {
    let mut out = [0u8; MANIFEST_BYTES];
    out[0..8].copy_from_slice(MANIFEST_MAGIC);
    out[8..16].copy_from_slice(&generation.to_le_bytes());
    out[16..24].copy_from_slice(&committed_lsn.to_le_bytes());
    let crc = crc32(&out[0..24]);
    out[24..28].copy_from_slice(&crc.to_le_bytes());
    out
}

/// Decode a manifest, returning `(generation, committed_lsn)` only when the
/// magic and CRC both match. A torn or absent manifest decodes to `None`.
pub fn decode_manifest(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.len() < 28 || &bytes[0..8] != MANIFEST_MAGIC {
        return None;
    }
    let stored = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    if crc32(&bytes[0..24]) != stored {
        return None;
    }
    let generation = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let committed_lsn = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    Some((generation, committed_lsn))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
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
        assert!(dir.path().join(MANIFEST_FILE_NAME).exists());
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

    #[test]
    fn sim_and_std_produce_identical_wal_without_faults() {
        use crate::vfs::{SimFaultConfig, SimVfs};
        let std_dir = tempfile::tempdir().unwrap();
        run_wal_workload(std_dir.path(), 4242).unwrap();
        let std_wal = std::fs::read(std_dir.path().join(WAL_FILE_NAME)).unwrap();

        let sim = SimVfs::new(0, SimFaultConfig::none());
        run_wal_workload_on(&sim, Path::new("/sim"), 4242).unwrap();
        let sim_wal = sim
            .crash_image()
            .get(Path::new("/sim").join(WAL_FILE_NAME).as_path())
            .cloned()
            .unwrap();

        assert_eq!(std_wal, sim_wal, "Vfs routing must not change WAL bytes");
    }

    #[test]
    fn manifest_roundtrips_and_rejects_tears() {
        let bytes = encode_manifest(7, 42);
        assert_eq!(decode_manifest(&bytes), Some((7, 42)));
        let mut torn = bytes;
        torn[18] ^= 0xFF;
        assert_eq!(decode_manifest(&torn), None);
        assert_eq!(decode_manifest(&[]), None);
    }
}
