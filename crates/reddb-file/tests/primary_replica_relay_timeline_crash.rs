use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reddb_file::{
    PrimaryReplicaFilePlan, RelayLogSegmentRef, ReplicaRelayLogManifest, ReplicaRelayLogRecord,
    ReplicaRelayLogSegment, TimelineHistory, TimelineId,
};

const CHILD_ENV: &str = "REDDB_PRIMARY_REPLICA_ATOMIC_CRASH_CHILD";
const ROOT_ENV: &str = "REDDB_PRIMARY_REPLICA_ATOMIC_CRASH_ROOT";
const ARTIFACT_ENV: &str = "REDDB_PRIMARY_REPLICA_ATOMIC_CRASH_ARTIFACT";
const CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

#[test]
fn relay_manifest_write_survives_atomic_crash_points() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in crash_points() {
        let root = temp_root("relay", point);
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
        initial_relay()
            .write_to_path(plan.relay_manifest_path("replica-a"))
            .expect("write initial relay manifest");

        run_child(&root, "relay", point);

        let manifest =
            ReplicaRelayLogManifest::read_from_path(plan.relay_manifest_path("replica-a"))
                .expect("relay manifest decodes");
        assert!(
            manifest.flushed_lsn == 10 || manifest.flushed_lsn == 80,
            "relay manifest must be old or new after {point}, got flushed_lsn={}",
            manifest.flushed_lsn
        );
        assert_eq!(manifest.applied_lsn, manifest.flushed_lsn);

        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn relay_segment_write_survives_atomic_crash_points() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in crash_points() {
        let root = temp_root("relay-segment", point);
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
        let segment_path = plan
            .relay_dir("replica-a")
            .join("relay-00000000000000000001-00000000000000000010.redwal");
        initial_relay_segment()
            .write_to_path(&segment_path)
            .expect("write initial relay segment");

        run_child(&root, "relay-segment", point);

        let segment =
            ReplicaRelayLogSegment::read_from_path(&segment_path).expect("relay segment decodes");
        assert!(
            segment.end_lsn == 10 || segment.end_lsn == 80,
            "relay segment must be old or new after {point}, got end_lsn={}",
            segment.end_lsn
        );
        if segment.end_lsn == 10 {
            assert_eq!(segment.records.len(), 1);
            assert_eq!(segment.records[0].payload, b"old".to_vec());
        } else {
            assert_eq!(segment.records.len(), 2);
            assert_eq!(segment.records[1].payload, b"new".to_vec());
        }

        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn timeline_history_write_survives_atomic_crash_points() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in crash_points() {
        let root = temp_root("timeline", point);
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
        TimelineHistory::new(1)
            .write_to_path(plan.timeline_history_path())
            .expect("write initial timeline history");

        run_child(&root, "timeline", point);

        let history =
            TimelineHistory::read_from_path(plan.timeline_history_path()).expect("history decodes");
        assert!(
            history.current() == Some(TimelineId(1)) || history.current() == Some(TimelineId(2)),
            "timeline history must be old or new after {point}, got {:?}",
            history.current()
        );
        if history.current() == Some(TimelineId(2)) {
            assert_eq!(history.ancestor_lsn(TimelineId(2)), Some(80));
        }

        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn primary_replica_atomic_crash_child() -> ExitCode {
    if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
    match std::env::var(ARTIFACT_ENV).expect("artifact env").as_str() {
        "relay" => {
            let _ = updated_relay().write_to_path(plan.relay_manifest_path("replica-a"));
        }
        "relay-segment" => {
            let segment_path = plan
                .relay_dir("replica-a")
                .join("relay-00000000000000000001-00000000000000000010.redwal");
            let _ = updated_relay_segment().write_to_path(segment_path);
        }
        "timeline" => {
            let _ = updated_timeline().write_to_path(plan.timeline_history_path());
        }
        other => panic!("unknown artifact {other}"),
    }
    ExitCode::from(1)
}

fn run_child(root: &PathBuf, artifact: &str, point: &str) {
    let child = Command::new(std::env::current_exe().expect("current test exe"))
        .arg("--exact")
        .arg("primary_replica_atomic_crash_child")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(ROOT_ENV, root)
        .env(ARTIFACT_ENV, artifact)
        .env(CRASH_ENV, point)
        .status()
        .expect("run crash child");
    assert_eq!(
        child.code(),
        Some(173),
        "child should crash at {point} for {artifact}, status={child:?}"
    );
}

fn crash_points() -> [&'static str; 4] {
    [
        "atomic_after_tmp_write",
        "atomic_after_tmp_sync",
        "atomic_after_rename",
        "atomic_after_dir_sync",
    ]
}

fn initial_relay() -> ReplicaRelayLogManifest {
    let mut manifest = ReplicaRelayLogManifest::new("replica-a", TimelineId(1));
    manifest
        .push_segment(RelayLogSegmentRef::new("relay-0001.redwal", 1, 10, 0x10).expect("segment"))
        .expect("push segment");
    manifest.mark_applied(10).expect("mark applied");
    manifest
}

fn updated_relay() -> ReplicaRelayLogManifest {
    let mut manifest = initial_relay();
    manifest
        .push_segment(RelayLogSegmentRef::new("relay-0002.redwal", 10, 80, 0x80).expect("segment"))
        .expect("push segment");
    manifest.mark_applied(80).expect("mark applied");
    manifest
}

fn initial_relay_segment() -> ReplicaRelayLogSegment {
    ReplicaRelayLogSegment::from_records(
        TimelineId(1),
        vec![ReplicaRelayLogRecord::new(10, b"old".to_vec())],
    )
    .expect("initial relay segment")
}

fn updated_relay_segment() -> ReplicaRelayLogSegment {
    ReplicaRelayLogSegment::from_records(
        TimelineId(1),
        vec![
            ReplicaRelayLogRecord::new(10, b"old".to_vec()),
            ReplicaRelayLogRecord::new(80, b"new".to_vec()),
        ],
    )
    .expect("updated relay segment")
}

fn updated_timeline() -> TimelineHistory {
    let mut history = TimelineHistory::new(1);
    history
        .fork(TimelineId(2), TimelineId(1), 80, 2, "promote replica-a")
        .expect("fork timeline");
    history
}

fn temp_root(artifact: &str, point: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "reddb-file-primary-{artifact}-crash-{point}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
