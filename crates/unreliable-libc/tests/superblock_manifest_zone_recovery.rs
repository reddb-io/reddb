//! DST crash campaign over the superblock ping-pong and manifest publication
//! (ADR 0038 §4 phase 1 exit criterion (b); fault classes from ADR 0074 §1).
//!
//! Two lanes, both required by the phase:
//!
//! * **Crash anywhere.** Sweep the power-cut budget across *every* durability
//!   syscall of the workload, so the process dies between and during the two
//!   superblock writes and around manifest publication. At each index the
//!   shared [`recover_and_check`] oracle must hold: one valid superblock always
//!   survives, and the manifest is the pre-update value, the post-update value,
//!   or absent — never a torn mixture.
//!
//! * **Fault classes.** Torn write, misdirected write, lost write and bit rot,
//!   applied to the superblock zone and the manifest zone of a real embedded
//!   `.rdb` and to a real paged superblock zone. Each must surface as detection
//!   per ADR 0074 §2: a damaged copy is skipped, and a zone with nothing left to
//!   trust fails the open didactically by name.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use reddb_file::dst::SECTOR_BYTES;
use reddb_file::{
    seal_paged_superblock_slot, select_paged_superblock, EmbeddedRdbArtifact, FaultClass,
    EMBEDDED_RDB_MANIFEST_0_OFFSET, EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
    EMBEDDED_RDB_SUPERBLOCK_1_OFFSET, EMBEDDED_RDB_SUPERBLOCK_SIZE, PAGED_SUPERBLOCK_SLOT_COUNT,
    PAGED_SUPERBLOCK_SLOT_SIZE, PAGED_SUPERBLOCK_ZONE_SIZE,
};
use unreliable_libc::vfs::POWER_CUT_MESSAGE;
use unreliable_libc::wal_workload::{MANIFEST_FILE_NAME, SUPERBLOCK_FILE_NAME};
use unreliable_libc::{
    decode_manifest, recover_and_check, run_wal_workload_on, SimFaultConfig, SimVfs,
};

const SIM_DIR: &str = "/db";

/// One superblock copy, as a length. Mirrors `EMBEDDED_RDB_SUPERBLOCK_SIZE`
/// without a truncating `u64 as usize` cast; the assertion keeps them in step.
const SUPERBLOCK_LEN: usize = 4096;

/// How many durability syscalls to walk the power-cut across. The workload's
/// fault-free run issues fewer than this, so the sweep covers every point —
/// including the two superblock slot writes and the manifest rename — plus a
/// tail of no-ops that simply run to completion.
const CRASH_POINT_SWEEP: u64 = 120;

/// The seed fixes the workload shape; only the crash point varies, so a failure
/// names the exact syscall index that broke the invariant.
const CAMPAIGN_SEED: u64 = 0x5B10_C4A5;

#[test]
fn the_superblock_copy_length_matches_the_file_layer() {
    assert_eq!(
        u64::try_from(SUPERBLOCK_LEN),
        Ok(EMBEDDED_RDB_SUPERBLOCK_SIZE)
    );
}

fn power_cut_only(after: u64) -> SimFaultConfig {
    SimFaultConfig {
        power_cut_after: Some(after),
        ..SimFaultConfig::none()
    }
}

// ── Lane 1: crash at every point of the update sequence ────────────────────

#[test]
fn crash_at_every_point_of_the_superblock_and_manifest_sequence_recovers() {
    for crash_after in 1..=CRASH_POINT_SWEEP {
        let vfs = SimVfs::new(CAMPAIGN_SEED, power_cut_only(crash_after));
        match run_wal_workload_on(&vfs, Path::new(SIM_DIR), CAMPAIGN_SEED) {
            Ok(_) => {}
            Err(err) => assert!(
                err.to_string().contains(POWER_CUT_MESSAGE),
                "crash_after={crash_after}: unexpected error: {err}"
            ),
        }

        let crash_dir = tempfile::tempdir().expect("tempdir() should succeed");
        vfs.materialize(crash_dir.path())
            .expect("materialize() should succeed");

        let report = recover_and_check(crash_dir.path()).unwrap_or_else(|err| {
            panic!(
                "recovery invariant violated crashing after {crash_after} durability syscalls: \
                 {err}\nreproduce: SEED={CAMPAIGN_SEED} crash_after={crash_after}"
            )
        });

        // The manifest is rooted by the superblock, so it can never claim a
        // generation the superblock has not reached. A torn manifest fails its
        // CRC and decodes to `None`, which is the "absent" outcome — what must
        // never happen is a manifest that decodes to a mixture of both updates.
        let manifest = std::fs::read(crash_dir.path().join(MANIFEST_FILE_NAME)).unwrap_or_default();
        if let Some((generation, committed_lsn)) = decode_manifest(&manifest) {
            assert!(
                committed_lsn <= report.last_committed_lsn,
                "crash_after={crash_after}: manifest claims commit {committed_lsn} \
                 beyond the WAL frontier {}",
                report.last_committed_lsn
            );
            if let Some(sb_generation) = report.superblock_generation {
                assert!(
                    generation <= sb_generation,
                    "crash_after={crash_after}: manifest generation {generation} is ahead \
                     of the superblock that roots it ({sb_generation})"
                );
            }
        }
    }
}

#[test]
fn once_both_slots_exist_no_crash_point_leaves_the_superblock_rootless() {
    for crash_after in 1..=CRASH_POINT_SWEEP {
        let vfs = SimVfs::new(CAMPAIGN_SEED, power_cut_only(crash_after));
        let _ = run_wal_workload_on(&vfs, Path::new(SIM_DIR), CAMPAIGN_SEED);

        let crash_dir = tempfile::tempdir().expect("tempdir() should succeed");
        vfs.materialize(crash_dir.path())
            .expect("materialize() should succeed");

        let superblock =
            std::fs::read(crash_dir.path().join(SUPERBLOCK_FILE_NAME)).unwrap_or_default();
        if superblock.len() < 2 * 64 {
            // The pair has not been fully written yet: genesis, nothing to root.
            continue;
        }
        // `recover_and_check` fails with SuperblockBothCorrupt if the ping-pong
        // invariant ever breaks; reaching here means one copy survived.
        recover_and_check(crash_dir.path()).unwrap_or_else(|err| {
            panic!("crash_after={crash_after}: superblock pair lost its last valid copy: {err}")
        });
    }
}

// ── Lane 2: fault classes against the embedded `.rdb` zones ────────────────

fn read_at(path: &Path, offset: u64, len: usize) -> Vec<u8> {
    let mut file = File::open(path).expect("open() should succeed");
    let mut buf = vec![0u8; len];
    file.seek(SeekFrom::Start(offset))
        .expect("seek() should succeed");
    file.read_exact(&mut buf)
        .expect("read_exact() should succeed");
    buf
}

fn write_at(path: &Path, offset: u64, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open() should succeed");
    file.seek(SeekFrom::Start(offset))
        .expect("seek() should succeed");
    file.write_all(bytes).expect("write_all() should succeed");
    file.sync_all().expect("sync_all() should succeed");
}

fn temp_store(label: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("reddb-dst-zone-{label}-"))
        .tempdir()
        .expect("tempdir() should succeed");
    let path = dir.path().join("data.rdb");
    (dir, path)
}

/// Apply one modeled fault class (ADR 0074 §1, the #1956 vocabulary) to the
/// region `[offset, offset + len)`, which holds one copy of a two-copy zone.
///
/// `stale` is the region's contents one generation ago: a torn write leaves the
/// tail at those bytes, and a lost write leaves the whole region there. The
/// `.rdb` zones are written through `std::fs`, not the `SimVfs`, so the same
/// decisions the simulator would make are applied to the real file here.
fn apply_fault(path: &Path, offset: u64, len: usize, class: FaultClass, stale: &[u8]) {
    match class {
        // Persist only a sector-aligned prefix; the tail keeps the old bytes.
        FaultClass::TornWrite => {
            let cut = usize::try_from(SECTOR_BYTES).unwrap_or(512).min(len);
            let resume = u64::try_from(cut).unwrap_or(0);
            write_at(path, offset + resume, &stale[cut..]);
        }
        // Right data, wrong offset. The wrong offset is another copy's, so the
        // caller supplies that copy's bytes rather than the region's own past.
        FaultClass::MisdirectedWrite => write_at(path, offset, stale),
        // One flipped bit under the checksum; everything else pristine.
        FaultClass::BitRot => {
            let mut byte = read_at(path, offset + 40, 1);
            byte[0] ^= 0x01;
            write_at(path, offset + 40, &byte);
        }
        // The write never reached the platter, but success was reported.
        FaultClass::LostWrite => write_at(path, offset, stale),
    }
}

#[test]
fn every_fault_class_against_one_superblock_copy_is_detected_and_survived() {
    for class in [
        FaultClass::TornWrite,
        FaultClass::BitRot,
        FaultClass::LostWrite,
    ] {
        let (_dir, path) = temp_store(class.name());
        EmbeddedRdbArtifact::create_with_snapshot(&path, b"RDST-v1")
            .expect("create_with_snapshot() should succeed");
        // Snapshot both copies at genesis so `lost` can revert the copy the
        // checkpoint is about to update back to *its own* prior contents —
        // that is what an acknowledged-but-absent write leaves behind.
        let genesis = [
            read_at(&path, EMBEDDED_RDB_SUPERBLOCK_0_OFFSET, SUPERBLOCK_LEN),
            read_at(&path, EMBEDDED_RDB_SUPERBLOCK_1_OFFSET, SUPERBLOCK_LEN),
        ];

        let checkpointed = EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-v2")
            .expect("write_snapshot() should succeed");
        let newest = checkpointed.selected_superblock.copy_index;
        let newest_offset = if newest == 0 {
            EMBEDDED_RDB_SUPERBLOCK_0_OFFSET
        } else {
            EMBEDDED_RDB_SUPERBLOCK_1_OFFSET
        };

        apply_fault(
            &path,
            newest_offset,
            SUPERBLOCK_LEN,
            class,
            &genesis[usize::from(newest)],
        );

        // Detection: the damaged copy is skipped, and the surviving copy roots
        // the store. The reader never sees data the zone cannot vouch for.
        let recovered = EmbeddedRdbArtifact::open(&path)
            .unwrap_or_else(|err| panic!("{class} on one copy must survive: {err}"));
        assert_ne!(
            recovered.selected_superblock.copy_index, newest,
            "{class}: the damaged copy must not be selected"
        );
        let snapshot = EmbeddedRdbArtifact::read_snapshot(&recovered)
            .expect("operation should succeed")
            .expect("operation should succeed");
        assert_eq!(snapshot, b"RDST-v1".to_vec(), "{class}");
    }
}

#[test]
fn a_misdirected_superblock_write_is_detected_not_read_as_an_older_generation() {
    let (_dir, path) = temp_store("misdirected");
    EmbeddedRdbArtifact::create_with_snapshot(&path, b"RDST-v1")
        .expect("create_with_snapshot() should succeed");
    EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-v2")
        .expect("write_snapshot() should succeed");

    // Right data, wrong offset: copy 0's sealed bytes land on copy 1.
    let copy_zero = read_at(&path, EMBEDDED_RDB_SUPERBLOCK_0_OFFSET, SUPERBLOCK_LEN);
    write_at(&path, EMBEDDED_RDB_SUPERBLOCK_1_OFFSET, &copy_zero);

    // The copy index inside the slot no longer matches where it sits, so the
    // misdirected copy is rejected rather than mistaken for a valid generation.
    let recovered = EmbeddedRdbArtifact::open(&path).expect("copy 0 still roots the store");
    assert_eq!(recovered.selected_superblock.copy_index, 0);
}

#[test]
fn both_superblock_copies_damaged_fails_the_open_by_name() {
    let (_dir, path) = temp_store("both_copies");
    EmbeddedRdbArtifact::create_with_snapshot(&path, b"RDST-v1")
        .expect("create_with_snapshot() should succeed");

    for offset in [
        EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
        EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
    ] {
        apply_fault(&path, offset, SUPERBLOCK_LEN, "bit_rot", &[]);
    }

    let err = EmbeddedRdbArtifact::open(&path).expect_err("a rootless store must not open");
    let message = err.to_string();
    assert!(message.contains("superblock zone"), "{message}");
    assert!(message.contains("salvage"), "{message}");
}

#[test]
fn bit_rot_in_the_manifest_zone_fails_the_open_by_name() {
    let (_dir, path) = temp_store("manifest_rot");
    EmbeddedRdbArtifact::create_with_snapshot(&path, b"RDST-v1")
        .expect("create_with_snapshot() should succeed");

    apply_fault(
        &path,
        EMBEDDED_RDB_MANIFEST_0_OFFSET,
        4096,
        FaultClass::BitRot,
        &[],
    );

    let err = EmbeddedRdbArtifact::open(&path).expect_err("a rotted manifest must not open");
    let message = err.to_string();
    assert!(message.contains("manifest zone"), "{message}");
    assert!(message.contains("salvage"), "{message}");
}

// ── Lane 3: the same fault classes against the paged superblock zone ───────

/// Build a paged superblock zone with both copies sealed, the way
/// `Pager::initialize` publishes it.
fn paged_zone(generations: [u64; 2]) -> Vec<u8> {
    let mut zone = vec![0u8; PAGED_SUPERBLOCK_ZONE_SIZE];
    for copy_index in 0..PAGED_SUPERBLOCK_SLOT_COUNT {
        let start = copy_index * PAGED_SUPERBLOCK_SLOT_SIZE;
        let slot = &mut zone[start..start + PAGED_SUPERBLOCK_SLOT_SIZE];
        reddb_file::init_database_header_page(slot, 3)
            .expect("init_database_header_page() should succeed");
        seal_paged_superblock_slot(slot, copy_index, generations[copy_index])
            .expect("seal_paged_superblock_slot() should succeed");
    }
    zone
}

#[test]
fn paged_zone_fault_classes_leave_exactly_one_trustworthy_copy() {
    let pristine = paged_zone([4, 5]);
    assert_eq!(
        select_paged_superblock(&pristine)
            .expect("select_paged_superblock() should succeed")
            .generation,
        5
    );

    // Torn write on the newest copy: its tail never landed.
    let mut torn = pristine.clone();
    torn[PAGED_SUPERBLOCK_SLOT_SIZE + PAGED_SUPERBLOCK_SLOT_SIZE / 2..].fill(0);
    let selected = select_paged_superblock(&torn).expect("the stale copy survives a torn write");
    assert_eq!((selected.copy_index, selected.generation), (0, 4));

    // Bit rot anywhere under the CRC.
    let mut rotted = pristine.clone();
    rotted[PAGED_SUPERBLOCK_SLOT_SIZE + 40] ^= 0x01;
    let selected = select_paged_superblock(&rotted).expect("the stale copy survives bit rot");
    assert_eq!((selected.copy_index, selected.generation), (0, 4));

    // Misdirected write: copy 0's sealed bytes land at copy 1's offset. The
    // slot's own copy index betrays the misdirection.
    let mut misdirected = pristine.clone();
    let (copy_zero, copy_one) = misdirected.split_at_mut(PAGED_SUPERBLOCK_SLOT_SIZE);
    copy_one.copy_from_slice(copy_zero);
    let selected = select_paged_superblock(&misdirected).expect("copy 0 still roots the store");
    assert_eq!((selected.copy_index, selected.generation), (0, 4));

    // Lost write: the newest copy silently reverted to an older generation.
    // Recovery is still consistent — it just rolls back to that generation.
    let lost = paged_zone([4, 3]);
    let selected = select_paged_superblock(&lost).expect("a lost write is not corruption");
    assert_eq!((selected.copy_index, selected.generation), (0, 4));

    // Both copies damaged: nothing left to trust.
    let mut both = pristine;
    both[40] ^= 0x01;
    both[PAGED_SUPERBLOCK_SLOT_SIZE + 40] ^= 0x01;
    assert!(select_paged_superblock(&both).is_none());
}
