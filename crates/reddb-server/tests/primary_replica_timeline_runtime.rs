use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reddb_server::{RedDBOptions, RedDBRuntime};

#[path = "support/primary_replica_file.rs"]
mod primary_replica_file;

const TIMELINE_CRASH_CHILD_ENV: &str = "REDDB_PRIMARY_REPLICA_TIMELINE_RUNTIME_CRASH_CHILD";
const TIMELINE_CRASH_DATA_PATH_ENV: &str = "REDDB_PRIMARY_REPLICA_TIMELINE_RUNTIME_CRASH_DATA_PATH";
const PRIMARY_REPLICA_CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

#[test]
fn runtime_persists_timeline_history_for_failover_promotion() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_timeline_history");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let promoted = runtime
        .record_failover_timeline_promotion("replica-a", 42)
        .expect("record promotion timeline");

    assert_eq!(promoted.current(), Some(reddb_file::TimelineId(2)));
    assert_eq!(promoted.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));

    let path = runtime
        .primary_replica_timeline_history_path()
        .expect("timeline history path");
    let from_disk = reddb_file::TimelineHistory::read_from_path(path).expect("read timeline");
    assert_eq!(from_disk.current(), Some(reddb_file::TimelineId(2)));
    assert_eq!(from_disk.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
    assert_eq!(from_disk.entries[1].reason, "promote replica-a");

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_appends_timeline_history_for_second_failover_promotion() {
    let data_path =
        primary_replica_file::temp_data_path("primary_replica_timeline_second_promotion");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .record_failover_timeline_promotion("replica-a", 42)
        .expect("first promotion timeline");
    let promoted = runtime
        .record_failover_timeline_promotion("replica-b", 77)
        .expect("second promotion timeline");

    assert_eq!(promoted.current(), Some(reddb_file::TimelineId(3)));
    assert_eq!(promoted.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
    assert_eq!(promoted.ancestor_lsn(reddb_file::TimelineId(3)), Some(77));
    let chain = promoted
        .descendant_chain_from(reddb_file::TimelineId(1))
        .expect("chain from initial timeline");
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].timeline, reddb_file::TimelineId(2));
    assert_eq!(chain[1].timeline, reddb_file::TimelineId(3));

    let path = runtime
        .primary_replica_timeline_history_path()
        .expect("timeline history path");
    let from_disk = reddb_file::TimelineHistory::read_from_path(path).expect("read timeline");
    assert_eq!(from_disk.current(), Some(reddb_file::TimelineId(3)));
    assert_eq!(from_disk.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
    assert_eq!(from_disk.ancestor_lsn(reddb_file::TimelineId(3)), Some(77));
    assert_eq!(from_disk.entries[1].reason, "promote replica-a");
    assert_eq!(from_disk.entries[2].reason, "promote replica-b");

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_timeline_promotion_survives_atomic_crash_points() {
    if std::env::var(TIMELINE_CRASH_CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "atomic_after_tmp_write",
        "atomic_after_tmp_sync",
        "atomic_after_rename",
        "atomic_after_dir_sync",
    ] {
        let data_path = primary_replica_file::temp_data_path(&format!(
            "primary_replica_timeline_crash_{point}"
        ));
        primary_replica_file::cleanup(&data_path);

        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("runtime boots");
        runtime
            .record_failover_timeline_promotion("replica-a", 42)
            .expect("initial promotion timeline");
        let path = runtime
            .primary_replica_timeline_history_path()
            .expect("timeline history path");
        let initial =
            reddb_file::TimelineHistory::read_from_path(&path).expect("read initial timeline");

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("runtime_timeline_promotion_crash_child")
            .arg("--nocapture")
            .env(TIMELINE_CRASH_CHILD_ENV, "1")
            .env(TIMELINE_CRASH_DATA_PATH_ENV, &data_path)
            .env(PRIMARY_REPLICA_CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        let history =
            reddb_file::TimelineHistory::read_from_path(&path).expect("timeline remains decodable");
        assert!(
            history.current() == Some(reddb_file::TimelineId(2))
                || history.current() == Some(reddb_file::TimelineId(3)),
            "timeline must be old or new after {point}, got {:?}",
            history.current()
        );
        if history.current() == Some(reddb_file::TimelineId(2)) {
            assert_eq!(history, initial);
        } else {
            assert_eq!(history.ancestor_lsn(reddb_file::TimelineId(3)), Some(77));
            assert_eq!(history.entries[2].reason, "promote replica-b");
        }

        primary_replica_file::cleanup(&data_path);
    }
}

#[test]
fn runtime_timeline_promotion_crash_child() -> ExitCode {
    if std::env::var(TIMELINE_CRASH_CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let data_path =
        PathBuf::from(std::env::var(TIMELINE_CRASH_DATA_PATH_ENV).expect("data path env"));
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let _ = runtime.record_failover_timeline_promotion("replica-b", 77);
    ExitCode::from(1)
}

#[test]
fn runtime_persists_rejoin_plan_from_timeline_history_chain() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_rejoin_plan");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .record_failover_timeline_promotion("replica-a", 42)
        .expect("first promotion timeline");
    runtime
        .record_failover_timeline_promotion("replica-b", 77)
        .expect("second promotion timeline");

    let decision = runtime
        .persist_primary_replica_rejoin_plan(reddb_file::TimelineId(1), 60, 40)
        .expect("persist rejoin plan")
        .expect("rejoin decision");
    assert_eq!(
        decision,
        reddb_file::RejoinDecision::Rewind {
            target_timeline: reddb_file::TimelineId(3),
            rewind_to_lsn: 42,
        }
    );
    assert!(
        primary_replica_file::show_config_value(&runtime, "red.replication.state")
            .contains("rejoin_rewind_required")
    );
    assert!(primary_replica_file::show_config_value(
        &runtime,
        "red.replication.rejoin_node_timeline"
    )
    .contains("1"));
    assert!(primary_replica_file::show_config_value(
        &runtime,
        "red.replication.rejoin_target_timeline"
    )
    .contains("3"));
    assert!(primary_replica_file::show_config_value(
        &runtime,
        "red.replication.rejoin_rewind_to_lsn"
    )
    .contains("42"));
    assert!(primary_replica_file::show_config_value(
        &runtime,
        "red.replication.rejoin_rewind_confirmed_lsn"
    )
    .contains("0"));
    assert!(primary_replica_file::show_config_value(
        &runtime,
        "red.replication.rejoin_rewind_confirmed_timeline"
    )
    .contains("0"));

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_failover_promotion_fails_closed_on_corrupt_timeline_history() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_timeline_corrupt");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let path = runtime
        .primary_replica_timeline_history_path()
        .expect("timeline history path");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create timeline parent");
    }
    fs::write(&path, b"corrupt timeline history").expect("write corrupt timeline");

    let err = match runtime.record_failover_timeline_promotion("replica-a", 42) {
        Ok(_) => panic!("corrupt timeline history must fail closed"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains("timeline") || message.contains("checksum") || message.contains("invalid"),
        "error should identify timeline history corruption, got {message}"
    );
    assert_eq!(
        fs::read(&path).expect("read corrupt timeline"),
        b"corrupt timeline history",
        "failed promotion must not replace corrupt timeline history"
    );

    primary_replica_file::cleanup(&data_path);
}
