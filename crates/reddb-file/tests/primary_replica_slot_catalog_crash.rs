use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reddb_file::{
    PrimaryReplicaFilePlan, ReplicaAck, ReplicationSlot, ReplicationSlotCatalog, TimelineId,
};

const CHILD_ENV: &str = "REDDB_PRIMARY_REPLICA_SLOT_CRASH_CHILD";
const ROOT_ENV: &str = "REDDB_PRIMARY_REPLICA_SLOT_CRASH_ROOT";
const CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

#[test]
fn slot_catalog_write_survives_atomic_crash_points() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "atomic_after_tmp_write",
        "atomic_after_tmp_sync",
        "atomic_after_rename",
        "atomic_after_dir_sync",
    ] {
        let root = temp_root(point);
        let plan = PrimaryReplicaFilePlan::new(root.path(), TimelineId(1));
        initial_catalog()
            .write_to_path(plan.slots_path())
            .expect("write initial slot catalog");

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("primary_replica_slot_catalog_crash_child")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .env(ROOT_ENV, root.path())
            .env(CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        let catalog =
            ReplicationSlotCatalog::read_from_path(plan.slots_path()).expect("catalog decodes");
        let slot = catalog
            .slots
            .iter()
            .find(|slot| slot.replica_id == "replica-a")
            .expect("replica-a slot");
        assert!(
            slot.confirmed_flush_lsn == 10 || slot.confirmed_flush_lsn == 80,
            "catalog must be old or new after {point}, got flush_lsn={}",
            slot.confirmed_flush_lsn
        );
        assert_eq!(slot.confirmed_flush_lsn, slot.confirmed_apply_lsn);
    }
}

#[test]
fn primary_replica_slot_catalog_crash_child() -> ExitCode {
    if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
    let _ = updated_catalog().write_to_path(plan.slots_path());
    ExitCode::from(1)
}

fn initial_catalog() -> ReplicationSlotCatalog {
    let mut catalog = ReplicationSlotCatalog::new(TimelineId(1));
    catalog
        .upsert(ReplicationSlot::new("replica-a", TimelineId(1), 10))
        .expect("upsert initial slot");
    catalog
}

fn updated_catalog() -> ReplicationSlotCatalog {
    let mut slot = ReplicationSlot::new("replica-a", TimelineId(1), 10);
    slot.update_ack(
        &ReplicaAck::with_positions("replica-a", TimelineId(1), 100, 90, 80, 80)
            .expect("ack positions"),
    )
    .expect("update ack");
    let mut catalog = ReplicationSlotCatalog::new(TimelineId(1));
    catalog.upsert(slot).expect("upsert updated slot");
    catalog
}

/// Auto-cleaning temp root: the returned [`tempfile::TempDir`] guard removes the
/// directory and all artifacts under it on drop, including on panic. The caller
/// keeps the binding alive across the crash child + assertions and reads the
/// path via `root.path()`.
fn temp_root(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-primary-slot-crash-{label}-"))
        .tempdir()
        .expect("temp dir")
}
