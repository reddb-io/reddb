use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reddb_server::{RedDBOptions, RedDBRuntime};

#[path = "support/primary_replica_file.rs"]
mod primary_replica_file;

const RELAY_CRASH_CHILD_ENV: &str = "REDDB_REPLICA_RELAY_RUNTIME_CRASH_CHILD";
const RELAY_CRASH_DATA_PATH_ENV: &str = "REDDB_REPLICA_RELAY_RUNTIME_CRASH_DATA_PATH";
const PRIMARY_REPLICA_CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

#[test]
fn replica_persists_relay_manifest_for_applied_batches() {
    let data_path = primary_replica_file::temp_data_path("replica_relay_manifest");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .record_replica_relay_batch(
            "replica-a",
            &[(1, b"first".to_vec()), (2, b"second".to_vec())],
            2,
        )
        .expect("record first relay batch");
    runtime
        .record_replica_relay_batch("replica-a", &[(3, b"third".to_vec())], 3)
        .expect("record second relay batch");

    let manifest_path = runtime
        .replica_relay_manifest_path("replica-a")
        .expect("relay manifest path");
    let manifest = reddb_file::ReplicaRelayLogManifest::read_from_path(&manifest_path)
        .expect("read relay manifest");
    assert_eq!(manifest.replica_id, "replica-a");
    assert_eq!(manifest.timeline, reddb_file::TimelineId::initial());
    assert_eq!(manifest.received_lsn, 3);
    assert_eq!(manifest.flushed_lsn, 3);
    assert_eq!(manifest.applied_lsn, 3);
    assert_eq!(manifest.segments.len(), 2);
    assert_eq!(manifest.segments[0].start_lsn, 1);
    assert_eq!(manifest.segments[0].end_lsn, 2);
    assert_eq!(manifest.segments[1].start_lsn, 3);
    assert_eq!(manifest.segments[1].end_lsn, 3);
    for segment_ref in &manifest.segments {
        let segment_path = manifest_path
            .parent()
            .expect("relay manifest parent")
            .join(&segment_ref.relative_path);
        let segment = reddb_file::ReplicaRelayLogSegment::read_from_path(&segment_path)
            .expect("read relay segment");
        assert_eq!(segment.timeline, manifest.timeline);
        assert_eq!(segment.start_lsn, segment_ref.start_lsn);
        assert_eq!(segment.end_lsn, segment_ref.end_lsn);
        assert_eq!(
            segment.checksum().expect("relay segment checksum"),
            segment_ref.checksum
        );
    }

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn replica_relay_manifest_corruption_fails_closed() {
    let data_path = primary_replica_file::temp_data_path("replica_relay_manifest_corrupt");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .record_replica_relay_batch("replica-a", &[(1, b"first".to_vec())], 1)
        .expect("record first relay batch");
    let manifest_path = runtime
        .replica_relay_manifest_path("replica-a")
        .expect("relay manifest path");
    fs::write(&manifest_path, b"corrupt relay manifest").expect("corrupt relay manifest");

    let err = runtime
        .record_replica_relay_batch("replica-a", &[(2, b"second".to_vec())], 2)
        .expect_err("corrupt relay manifest must fail closed");
    let message = err.to_string();
    assert!(
        message.contains("relay") || message.contains("checksum") || message.contains("invalid"),
        "error should identify relay manifest corruption, got {message}"
    );
    assert_eq!(
        fs::read(&manifest_path).expect("read corrupt relay manifest"),
        b"corrupt relay manifest",
        "failed closed path must not replace corrupt manifest with a reset state"
    );

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn replica_relay_missing_segment_fails_closed() {
    let data_path = primary_replica_file::temp_data_path("replica_relay_missing_segment");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .record_replica_relay_batch("replica-a", &[(1, b"first".to_vec())], 1)
        .expect("record first relay batch");
    let manifest_path = runtime
        .replica_relay_manifest_path("replica-a")
        .expect("relay manifest path");
    let manifest = reddb_file::ReplicaRelayLogManifest::read_from_path(&manifest_path)
        .expect("read relay manifest");
    let relay_dir = manifest_path.parent().expect("relay manifest parent");
    let segment_path = relay_dir.join(&manifest.segments[0].relative_path);
    fs::remove_file(&segment_path).expect("remove relay segment");

    let err = runtime
        .record_replica_relay_batch("replica-a", &[(2, b"second".to_vec())], 2)
        .expect_err("missing relay segment must fail closed");
    let message = err.to_string();
    assert!(
        message.contains("No such file") || message.contains("not found"),
        "error should identify missing relay segment, got {message}"
    );
    let unchanged = reddb_file::ReplicaRelayLogManifest::read_from_path(&manifest_path)
        .expect("read unchanged relay manifest");
    assert_eq!(
        unchanged, manifest,
        "failed closed path must not advance relay manifest"
    );
    assert!(
        !relay_dir
            .join(reddb_file::layout::relay_segment_relative_path(2, 2))
            .exists(),
        "failed closed path must not write a new relay segment"
    );

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn replica_relay_corrupt_segment_fails_closed() {
    let data_path = primary_replica_file::temp_data_path("replica_relay_corrupt_segment");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .record_replica_relay_batch("replica-a", &[(1, b"first".to_vec())], 1)
        .expect("record first relay batch");
    let manifest_path = runtime
        .replica_relay_manifest_path("replica-a")
        .expect("relay manifest path");
    let manifest = reddb_file::ReplicaRelayLogManifest::read_from_path(&manifest_path)
        .expect("read relay manifest");
    let segment_path = manifest_path
        .parent()
        .expect("relay manifest parent")
        .join(&manifest.segments[0].relative_path);
    fs::write(&segment_path, b"corrupt relay segment").expect("corrupt relay segment");

    let err = runtime
        .record_replica_relay_batch("replica-a", &[(2, b"second".to_vec())], 2)
        .expect_err("corrupt relay segment must fail closed");
    let message = err.to_string();
    assert!(
        message.contains("relay") || message.contains("checksum") || message.contains("invalid"),
        "error should identify corrupt relay segment, got {message}"
    );
    let unchanged = reddb_file::ReplicaRelayLogManifest::read_from_path(&manifest_path)
        .expect("read unchanged relay manifest");
    assert_eq!(
        unchanged, manifest,
        "failed closed path must not advance relay manifest"
    );

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn replica_relay_runtime_write_survives_atomic_crash_points() {
    if std::env::var(RELAY_CRASH_CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "atomic_after_tmp_write",
        "atomic_after_tmp_sync",
        "atomic_after_rename",
        "atomic_after_dir_sync",
    ] {
        let data_path =
            primary_replica_file::temp_data_path(&format!("replica_relay_runtime_crash_{point}"));
        primary_replica_file::cleanup(&data_path);

        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("runtime boots");
        runtime
            .record_replica_relay_batch("replica-a", &[(1, b"first".to_vec())], 1)
            .expect("record first relay batch");
        let manifest_path = runtime
            .replica_relay_manifest_path("replica-a")
            .expect("relay manifest path");
        let initial = reddb_file::ReplicaRelayLogManifest::read_from_path(&manifest_path)
            .expect("read initial relay manifest");

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("replica_relay_runtime_crash_child")
            .arg("--exact")
            .arg("--nocapture")
            .env(RELAY_CRASH_CHILD_ENV, "1")
            .env(RELAY_CRASH_DATA_PATH_ENV, &data_path)
            .env(PRIMARY_REPLICA_CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        let manifest = reddb_file::ReplicaRelayLogManifest::read_from_path(&manifest_path)
            .expect("relay manifest remains decodable");
        assert!(
            manifest.flushed_lsn == initial.flushed_lsn || manifest.flushed_lsn == 2,
            "relay manifest must be old or new after {point}, got flushed_lsn={}",
            manifest.flushed_lsn
        );
        assert_eq!(manifest.applied_lsn, manifest.flushed_lsn);
        manifest
            .validate_segments(manifest_path.parent().expect("relay manifest parent"))
            .expect("visible relay manifest references valid segments");
        if manifest.flushed_lsn == 2 {
            assert_eq!(manifest.segments.len(), 2);
            assert_eq!(manifest.segments[1].start_lsn, 2);
            assert_eq!(manifest.segments[1].end_lsn, 2);
        } else {
            assert_eq!(manifest, initial);
        }

        primary_replica_file::cleanup(&data_path);
    }
}

#[test]
fn replica_relay_runtime_crash_child() -> ExitCode {
    if std::env::var(RELAY_CRASH_CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let data_path = PathBuf::from(std::env::var(RELAY_CRASH_DATA_PATH_ENV).expect("data path env"));
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let _ = runtime.record_replica_relay_batch("replica-a", &[(2, b"second".to_vec())], 2);
    ExitCode::from(1)
}
