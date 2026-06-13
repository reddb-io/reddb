//! Issue gh-475: `<data>.shm` shared-memory file provisioning + crash
//! detection substrate. Covers happy path (create + reattach), multi-
//! reader coexistence, and crash recovery when the prior owner pid is
//! dead. Mmap wiring + tier auto-enable are deferred — see ADR-0018.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::{
    provision_shm, read_shm_header, set_shm_provisioning_enabled, shm_path_for,
    shm_provisioning_enabled, ShmProvisionState, SHM_FILE_SIZE, SHM_HEADER_SIZE, SHM_MAGIC,
    SHM_VERSION,
};
use std::sync::Mutex;

// Tests poke a process-global toggle; serialise them.
static POLICY_GUARD: Mutex<()> = Mutex::new(());

fn reset_policy() {
    set_shm_provisioning_enabled(false);
    std::env::remove_var("REDDB_SHM_PROVISION");
}

#[test]
fn provisioning_disabled_by_default() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_policy();
    assert!(!shm_provisioning_enabled());
    set_shm_provisioning_enabled(true);
    assert!(shm_provisioning_enabled());
    reset_policy();
}

#[test]
fn provision_creates_shm_with_canonical_header() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    let data = support::temp_db_file("shm-create");

    let handle = provision_shm(&data).expect("shm provisions cleanly");
    let shm_path = shm_path_for(&data);

    assert_eq!(handle.state, ShmProvisionState::Created);
    assert!(shm_path.exists(), "shm file must exist: {shm_path:?}");
    let meta = std::fs::metadata(&shm_path).expect("stat shm");
    assert_eq!(
        meta.len(),
        SHM_FILE_SIZE,
        "shm file size must equal the fixed page size",
    );

    let bytes = std::fs::read(&shm_path).expect("read shm");
    assert!(bytes.len() >= SHM_HEADER_SIZE);
    assert_eq!(&bytes[0..8], SHM_MAGIC, "shm magic must be RDBSHM01");

    let header = read_shm_header(&data)
        .expect("read header")
        .expect("header present");
    assert_eq!(header.version, SHM_VERSION);
    assert_eq!(header.owner_pid, std::process::id());
    assert_eq!(header.generation, 1);
    assert_eq!(header.reader_count, 0);

    drop(handle);
}

#[test]
fn reopen_by_same_process_attaches_without_bumping_generation() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    let data = support::temp_db_file("shm-reattach");

    let first = provision_shm(&data).expect("first open");
    let gen_after_create = first.generation();
    drop(first);

    let second = provision_shm(&data).expect("second open");
    assert_eq!(
        second.state,
        ShmProvisionState::AttachedToLiveOwner,
        "same-pid reopen must attach, not heal",
    );
    assert_eq!(
        second.generation(),
        gen_after_create,
        "generation must not bump for a same-pid reopen",
    );
}

#[test]
fn multiple_readers_can_attach_concurrently() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    let data = support::temp_db_file("shm-readers");

    let mut owner = provision_shm(&data).expect("owner open");
    let count_a = owner.attach_reader().expect("attach reader 1");
    let count_b = owner.attach_reader().expect("attach reader 2");
    let count_c = owner.attach_reader().expect("attach reader 3");

    assert_eq!(count_a, 1);
    assert_eq!(count_b, 2);
    assert_eq!(count_c, 3);

    let header = read_shm_header(&data)
        .expect("read after attaches")
        .expect("header present");
    assert_eq!(
        header.reader_count, 3,
        "reader_count must persist to disk so a fresh opener sees it",
    );

    let after_detach = owner.detach_reader().expect("detach reader");
    assert_eq!(after_detach, 2);
}

#[test]
fn crash_of_prior_owner_is_detected_and_state_cleaned() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    let data = support::temp_db_file("shm-crash");

    // Bootstrap a valid shm with a fabricated dead-pid owner.
    let mut handle = provision_shm(&data).expect("seed shm");
    handle.attach_reader().expect("seed reader count");
    drop(handle);

    // Forge a dead owner pid + non-zero reader count.
    let shm_path = shm_path_for(&data);
    let mut bytes = std::fs::read(&shm_path).expect("read seed");
    // `i32::MAX as u32` is above the Linux pid_max ceiling so the
    // liveness probe sees ESRCH (definitely dead) without aliasing
    // the kill(2) broadcast semantics that `-1`/`u32::MAX` would.
    let dead_pid: u32 = i32::MAX as u32;
    bytes[12..16].copy_from_slice(&dead_pid.to_le_bytes());
    // Recompute checksum so the header still validates.
    let mut acc: u64 = 0xcbf29ce484222325;
    for &b in &bytes[..56] {
        acc ^= b as u64;
        acc = acc.wrapping_mul(0x100000001b3);
    }
    bytes[56..64].copy_from_slice(&acc.to_le_bytes());
    std::fs::write(&shm_path, &bytes).expect("rewrite header");

    // Sanity: header now reports a dead owner with stale reader_count.
    let pre = read_shm_header(&data)
        .expect("read forged")
        .expect("header present");
    assert_eq!(pre.owner_pid, dead_pid);
    assert!(pre.reader_count >= 1);
    let prior_gen = pre.generation;

    // Recovery path: provisioning must detect the dead owner, take
    // ownership, clear reader_count, and bump generation.
    let recovered = provision_shm(&data).expect("recover from crash");
    assert_eq!(
        recovered.state,
        ShmProvisionState::RecoveredFromCrash,
        "dead prior owner must be reported as a crash recovery",
    );
    assert_eq!(recovered.header.owner_pid, std::process::id());
    assert_eq!(
        recovered.header.reader_count, 0,
        "stale reader handles must be cleared on takeover",
    );
    assert!(
        recovered.header.generation > prior_gen,
        "generation must bump on takeover (was {prior_gen}, now {})",
        recovered.header.generation,
    );
}

#[test]
fn corrupt_header_is_healed_in_place() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    let data = support::temp_db_file("shm-corrupt");

    // Plant a file that exists but has no valid magic.
    let shm_path = shm_path_for(&data);
    std::fs::write(&shm_path, b"not-a-valid-shm-header").expect("plant corrupt");

    let healed = provision_shm(&data).expect("heal corrupt");
    assert_eq!(healed.state, ShmProvisionState::HealedCorruptHeader);
    assert_eq!(healed.header.owner_pid, std::process::id());

    let header = read_shm_header(&data)
        .expect("re-read")
        .expect("header present after heal");
    assert_eq!(header.version, SHM_VERSION);
}
