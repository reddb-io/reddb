use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use reddb_server::replication::primary::PrimaryReplication;
use reddb_server::{RedDBOptions, RedDBRuntime, RedDBServer, ReplicationConfig};

const SLOT_CRASH_CHILD_ENV: &str = "REDDB_PRIMARY_REPLICA_SLOT_RUNTIME_CRASH_CHILD";
const SLOT_CRASH_DATA_PATH_ENV: &str = "REDDB_PRIMARY_REPLICA_SLOT_RUNTIME_CRASH_DATA_PATH";
const PRIMARY_REPLICA_CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

/// Auto-cleaning data path: holds the [`tempfile::TempDir`] guard so the temp
/// directory and all WAL/sidecar artifacts are removed on drop (incl. panic).
struct TempDataPath {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl std::ops::Deref for TempDataPath {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for TempDataPath {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

fn temp_data_path(name: &str) -> TempDataPath {
    let dir = tempfile::Builder::new()
        .prefix(&format!("reddb-test-{name}-"))
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join(format!("{name}.rdb"));
    TempDataPath { _dir: dir, path }
}

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, &'static str)]) -> Self {
        let previous = vars
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect();
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.iter().rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

fn post_query_once(runtime: &RedDBRuntime, sql: &str) -> (u16, String) {
    post_query_once_with_commit_policy(runtime, sql, None)
}

fn post_query_once_with_commit_policy(
    runtime: &RedDBRuntime,
    sql: &str,
    commit_policy: Option<&str>,
) -> (u16, String) {
    let server = RedDBServer::new(runtime.clone());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind query listener");
    let addr = listener.local_addr().expect("query addr");
    let handle = thread::spawn(move || server.serve_one_on(listener));
    let escaped_sql = sql.replace('\\', "\\\\").replace('"', "\\\"");
    let body = match commit_policy {
        Some(policy) => format!(
            "{{\"query\":\"{}\",\"commit_policy\":\"{}\"}}",
            escaped_sql,
            policy.replace('\\', "\\\\").replace('"', "\\\"")
        ),
        None => format!("{{\"query\":\"{}\"}}", escaped_sql),
    };
    let request = format!(
        "POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );

    let mut stream = TcpStream::connect(addr).expect("connect query listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set query read timeout");
    stream
        .write_all(request.as_bytes())
        .expect("write query request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read query response");
    handle
        .join()
        .expect("query server thread joined")
        .expect("query request served");

    let (head, body) = response.split_once("\r\n\r\n").expect("http framing");
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .expect("parse status");
    (status, body.to_string())
}

fn scrape_metrics(runtime: &RedDBRuntime) -> String {
    let server = RedDBServer::new(runtime.clone());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind metrics listener");
    let addr = listener.local_addr().expect("metrics addr");
    let handle = thread::spawn(move || server.serve_one_on(listener));

    let mut stream = TcpStream::connect(addr).expect("connect metrics");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set metrics read timeout");
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .expect("write metrics request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read metrics response");
    handle
        .join()
        .expect("metrics server thread joined")
        .expect("metrics request served");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "metrics scrape should return 200, got {response:?}"
    );
    response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default()
}

#[test]
fn primary_persists_reddb_file_replication_slot_catalog() {
    let data_path = temp_data_path("primary_replica_slot_catalog");

    {
        let primary = PrimaryReplication::new(Some(&data_path));
        primary.register_replica("replica-a".to_string());
        primary.note_replica_pull("replica-a", 12);
        primary.ack_replica_lsn("replica-a", 10, 8);
    }

    let catalog_path = PrimaryReplication::slot_catalog_path_for(&data_path);
    let catalog = reddb_file::ReplicationSlotCatalog::read_from_path(&catalog_path)
        .expect("read binary slot catalog");
    let slot = catalog
        .slots
        .iter()
        .find(|slot| slot.replica_id == "replica-a")
        .expect("replica-a slot");
    assert_eq!(slot.restart_lsn, 8);
    assert_eq!(slot.confirmed_write_lsn, 10);
    assert_eq!(slot.confirmed_flush_lsn, 8);
    assert_eq!(slot.confirmed_apply_lsn, 8);
    assert!(slot.active);
}

#[test]
fn primary_runtime_slot_catalog_write_survives_atomic_crash_points() {
    if std::env::var(SLOT_CRASH_CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "atomic_after_tmp_write",
        "atomic_after_tmp_sync",
        "atomic_after_rename",
        "atomic_after_dir_sync",
    ] {
        let data_path = temp_data_path(&format!("primary_replica_slot_runtime_crash_{point}"));

        {
            let primary = PrimaryReplication::new(Some(&data_path));
            primary.register_replica("replica-a".to_string());
            primary.ack_replica_lsn("replica-a", 10, 8);
        }
        let catalog_path = PrimaryReplication::slot_catalog_path_for(&data_path);
        let initial = reddb_file::ReplicationSlotCatalog::read_from_path(&catalog_path)
            .expect("read initial binary slot catalog");

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("primary_runtime_slot_catalog_crash_child")
            .arg("--nocapture")
            .env(SLOT_CRASH_CHILD_ENV, "1")
            .env(SLOT_CRASH_DATA_PATH_ENV, &data_path)
            .env(PRIMARY_REPLICA_CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        let catalog = reddb_file::ReplicationSlotCatalog::read_from_path(&catalog_path)
            .expect("slot catalog remains decodable");
        let slot = catalog
            .slots
            .iter()
            .find(|slot| slot.replica_id == "replica-a")
            .expect("replica-a slot");
        assert!(
            slot.confirmed_flush_lsn == 8 || slot.confirmed_flush_lsn == 80,
            "slot catalog must be old or new after {point}, got flush_lsn={}",
            slot.confirmed_flush_lsn
        );
        assert_eq!(slot.confirmed_apply_lsn, slot.confirmed_flush_lsn);
        if slot.confirmed_flush_lsn == 8 {
            assert_eq!(catalog, initial);
        }

        let reopened = PrimaryReplication::new(Some(&data_path));
        let resume = reopened.register_replica("replica-a".to_string());
        assert!(
            resume == 8 || resume == 80,
            "reopened primary should resume from old or new durable slot after {point}, got {resume}"
        );
    }
}

#[test]
fn primary_runtime_slot_catalog_crash_child() -> ExitCode {
    if std::env::var(SLOT_CRASH_CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let data_path = PathBuf::from(std::env::var(SLOT_CRASH_DATA_PATH_ENV).expect("data path env"));
    let primary = PrimaryReplication::new(Some(&data_path));
    primary.ack_replica_lsn("replica-a", 100, 80);
    ExitCode::from(1)
}

#[test]
fn primary_can_reopen_slots_from_binary_catalog_without_legacy_json() {
    let data_path = temp_data_path("primary_replica_slot_catalog_binary_only");

    {
        let primary = PrimaryReplication::new(Some(&data_path));
        primary.register_replica("replica-a".to_string());
        primary.ack_replica_lsn("replica-a", 17, 13);
    }
    fs::remove_file(PrimaryReplication::slot_path_for(&data_path)).expect("remove legacy json");

    let reopened = PrimaryReplication::new(Some(&data_path));
    let slot = reopened
        .slot_snapshots()
        .into_iter()
        .find(|slot| slot.replica_id == "replica-a")
        .expect("replica-a slot loaded from binary catalog");
    assert_eq!(slot.restart_lsn, 13);
    assert_eq!(slot.confirmed_lsn(), 17);
    assert_eq!(reopened.register_replica("replica-a".to_string()), 13);
}

#[test]
fn primary_falls_back_to_legacy_slots_when_binary_catalog_is_corrupt() {
    let data_path = temp_data_path("primary_replica_slot_catalog_corrupt_binary");

    {
        let primary = PrimaryReplication::new(Some(&data_path));
        primary.register_replica("replica-a".to_string());
        primary.ack_replica_lsn("replica-a", 21, 14);
    }
    fs::write(
        PrimaryReplication::slot_catalog_path_for(&data_path),
        b"corrupt redslots",
    )
    .expect("corrupt binary slot catalog");

    let reopened = PrimaryReplication::new(Some(&data_path));
    let slot = reopened
        .slot_snapshots()
        .into_iter()
        .find(|slot| slot.replica_id == "replica-a")
        .expect("replica-a slot recovered from legacy json");
    assert_eq!(slot.restart_lsn, 14);
    assert_eq!(slot.confirmed_lsn(), 21);
    assert_eq!(reopened.register_replica("replica-a".to_string()), 14);
}

#[test]
fn primary_runtime_materializes_reddb_file_wal_segments_for_commits() {
    let data_path = temp_data_path("primary_replica_redwal_runtime");

    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&data_path).with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime boots");
    runtime
        .execute_query("INSERT INTO redwal_items (id, name) VALUES (1, 'alpha')")
        .expect("insert row");
    runtime
        .execute_query("INSERT INTO redwal_items (id, name) VALUES (2, 'beta')")
        .expect("insert second row");

    let head_lsn = runtime.primary_logical_head_lsn();
    assert!(head_lsn >= 2, "primary should advance logical WAL");
    let plan = runtime
        .primary_replica_file_plan()
        .expect("primary-replica file plan");
    let segment =
        reddb_file::PrimaryReplicaWalSegment::read_from_path(plan.wal_segment_path(head_lsn))
            .expect("read primary-replica wal segment");
    assert_eq!(segment.timeline, reddb_file::TimelineId::initial());
    assert!(
        segment.records.iter().any(|record| record.lsn == head_lsn),
        "segment should contain the latest logical WAL record"
    );
    assert!(
        segment.records.len() >= 2,
        "two commits should append to the same redwal segment"
    );
}

#[test]
fn http_mutation_ack_n_commit_policy_fails_closed_without_replica_ack() {
    let _env_lock = env_lock().lock().expect("env lock");
    let _env = EnvGuard::set(&[
        ("RED_PRIMARY_COMMIT_POLICY", "ack_n=1"),
        ("RED_REPLICATION_ACK_TIMEOUT_MS", "20"),
        ("RED_COMMIT_FAIL_ON_TIMEOUT", "true"),
    ]);
    let data_path = temp_data_path("primary_replica_http_ack_n_timeout");

    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&data_path).with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime boots");

    let (status, body) = post_query_once(
        &runtime,
        "INSERT INTO ack_items (id, name) VALUES (1, 'alpha')",
    );
    assert_eq!(
        status, 504,
        "ack_n without replica ack must fail closed, body={body}"
    );
    assert!(
        body.contains("commit policy timed out") && body.contains("RED_COMMIT_FAIL_ON_TIMEOUT"),
        "error body should identify commit policy timeout, got {body}"
    );
    assert!(
        runtime.cdc_current_lsn() > 0,
        "local mutation should have advanced CDC before the response failed"
    );
}

#[test]
fn http_request_commit_policy_can_strengthen_and_wait_for_ack() {
    let _env_lock = env_lock().lock().expect("env lock");
    let _env = EnvGuard::set(&[
        ("RED_PRIMARY_COMMIT_POLICY", "local"),
        ("RED_REPLICATION_ACK_TIMEOUT_MS", "20"),
        ("RED_COMMIT_FAIL_ON_TIMEOUT", "true"),
    ]);
    let data_path = temp_data_path("primary_replica_http_request_ack_n_timeout");

    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&data_path).with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime boots");

    let (status, body) = post_query_once_with_commit_policy(
        &runtime,
        "INSERT INTO request_ack_items (id, name) VALUES (1, 'alpha')",
        Some("ack_n=1"),
    );
    assert_eq!(
        status, 504,
        "request ack_n without replica ack must fail closed, body={body}"
    );
    assert!(
        body.contains("commit policy timed out") && body.contains("RED_COMMIT_FAIL_ON_TIMEOUT"),
        "error body should identify request commit wait, got {body}"
    );
}

#[test]
fn http_request_commit_policy_rejects_weaker_than_floor() {
    let _env_lock = env_lock().lock().expect("env lock");
    let _env = EnvGuard::set(&[("RED_PRIMARY_COMMIT_POLICY", "quorum")]);
    let runtime = RedDBRuntime::in_memory().expect("runtime boots");

    let (status, body) = post_query_once_with_commit_policy(
        &runtime,
        "INSERT INTO request_floor_items (id, name) VALUES (1, 'alpha')",
        Some("local"),
    );
    assert_eq!(
        status, 422,
        "weaker request policy must be rejected: {body}"
    );
    assert!(
        body.contains("COMMIT_POLICY_BELOW_FLOOR"),
        "typed floor violation should be surfaced, got {body}"
    );
}

#[test]
fn primary_runtime_writes_redwal_to_promoted_timeline() {
    let data_path = temp_data_path("primary_replica_redwal_promoted_timeline");

    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&data_path).with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime boots");
    runtime
        .execute_query("INSERT INTO promoted_wal_items (id, name) VALUES (1, 'before')")
        .expect("insert before promotion");
    runtime
        .record_failover_timeline_promotion("replica-a", runtime.primary_logical_head_lsn())
        .expect("record promotion timeline");

    let promoted_plan = runtime
        .primary_replica_file_plan()
        .expect("promoted primary-replica file plan");
    assert_eq!(promoted_plan.timeline, reddb_file::TimelineId(2));

    runtime
        .execute_query("INSERT INTO promoted_wal_items (id, name) VALUES (2, 'after')")
        .expect("insert after promotion");
    let head_lsn = runtime.primary_logical_head_lsn();
    let promoted_segment = reddb_file::PrimaryReplicaWalSegment::read_from_path(
        promoted_plan.wal_segment_path(head_lsn),
    )
    .expect("read promoted timeline wal segment");
    assert_eq!(promoted_segment.timeline, reddb_file::TimelineId(2));
    assert!(
        promoted_segment
            .records
            .iter()
            .any(|record| record.lsn == head_lsn),
        "post-promotion commit should append to promoted timeline"
    );

    let initial_plan = reddb_file::PrimaryReplicaFilePlan::new(
        reddb_server::replication::primary::PrimaryReplication::primary_replica_root_for(
            &data_path,
        ),
        reddb_file::TimelineId::initial(),
    );
    let initial_segment = reddb_file::PrimaryReplicaWalSegment::read_from_path(
        initial_plan.wal_segment_path(head_lsn),
    )
    .expect("read initial timeline wal segment");
    assert!(
        !initial_segment
            .records
            .iter()
            .any(|record| record.lsn == head_lsn),
        "post-promotion commit must not append to initial timeline"
    );
}

#[test]
fn runtime_reports_primary_replica_wal_retention_plan_from_slot_catalog() {
    let data_path = temp_data_path("primary_replica_wal_retention_plan");

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let plan = runtime
        .primary_replica_file_plan()
        .expect("primary-replica file plan");

    let mut catalog = reddb_file::ReplicationSlotCatalog::new(reddb_file::TimelineId::initial());
    catalog
        .upsert(reddb_file::ReplicationSlot::new(
            "replica-a",
            reddb_file::TimelineId::initial(),
            0,
        ))
        .expect("upsert slot");
    catalog
        .write_to_path(plan.slots_path())
        .expect("write slot catalog");

    let wal_path = plan.wal_segment_path(0);
    fs::create_dir_all(wal_path.parent().expect("wal parent")).expect("create wal dir");
    fs::write(&wal_path, b"segment").expect("write fake wal segment");

    let retention = runtime
        .primary_replica_wal_retention_plan()
        .expect("retention plan call")
        .expect("retention plan");
    assert_eq!(retention.oldest_required_lsn, Some(0));
    assert_eq!(retention.retained_bytes_before_prune, 7);
    assert_eq!(retention.retained_bytes_after_prune, 7);
    assert!(retention.removable_segments.is_empty());

    let pruned = runtime
        .prune_primary_replica_wal_segments()
        .expect("prune call")
        .expect("prune result");
    assert_eq!(pruned.retained_bytes_before_prune, 7);
    assert_eq!(pruned.retained_bytes_after_prune, 7);
    assert!(pruned.removed_segments.is_empty());
    assert!(wal_path.exists());
}

#[test]
fn metrics_exports_primary_replica_wal_retention_plan() {
    let data_path = temp_data_path("primary_replica_wal_retention_metrics");

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let plan = runtime
        .primary_replica_file_plan()
        .expect("primary-replica file plan");
    let mut catalog = reddb_file::ReplicationSlotCatalog::new(reddb_file::TimelineId::initial());
    catalog
        .upsert(reddb_file::ReplicationSlot::new(
            "replica-a",
            reddb_file::TimelineId::initial(),
            0,
        ))
        .expect("upsert slot");
    catalog
        .write_to_path(plan.slots_path())
        .expect("write slot catalog");
    let wal_path = plan.wal_segment_path(0);
    fs::create_dir_all(wal_path.parent().expect("wal parent")).expect("create wal dir");
    fs::write(&wal_path, b"segment").expect("write fake wal segment");

    let metrics = scrape_metrics(&runtime);
    assert!(metrics.contains("reddb_primary_replica_wal_retained_bytes 7"));
    assert!(metrics.contains("reddb_primary_replica_wal_retained_after_prune_bytes 7"));
    assert!(metrics.contains("reddb_primary_replica_wal_oldest_required_lsn 0"));
    assert!(metrics.contains("reddb_primary_replica_wal_removable_segments 0"));
    assert!(metrics.contains("reddb_primary_replica_wal_retention_error 0"));
}

#[test]
fn metrics_surfaces_primary_replica_wal_retention_errors() {
    let data_path = temp_data_path("primary_replica_wal_retention_metrics_error");

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let path = runtime
        .primary_replica_timeline_history_path()
        .expect("timeline history path");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create timeline parent");
    }
    fs::write(&path, b"corrupt timeline history").expect("write corrupt timeline");

    let metrics = scrape_metrics(&runtime);
    assert!(
        metrics.contains("reddb_primary_replica_wal_retention_error 1"),
        "metrics should surface retention computation errors, got {metrics}"
    );
}

#[test]
fn runtime_prunes_primary_replica_redwal_segments_after_replica_ack() {
    let data_path = temp_data_path("primary_replica_wal_prune_on_ack");

    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&data_path).with_replication(ReplicationConfig::primary()),
    )
    .expect("runtime boots");
    let plan = runtime
        .primary_replica_file_plan()
        .expect("primary-replica file plan");
    for index in 0..=18 {
        let path = plan.wal_segment_path(index * plan.segment_bytes);
        fs::create_dir_all(path.parent().expect("wal parent")).expect("create wal dir");
        fs::write(&path, [index as u8]).expect("write fake redwal segment");
    }

    let ack_lsn = 18 * plan.segment_bytes;
    let pruned = runtime
        .ack_primary_replica_lsn_and_prune("replica-a", ack_lsn, ack_lsn, 0, 0)
        .expect("ack and prune")
        .expect("prune result");

    assert_eq!(pruned.oldest_required_lsn, Some(ack_lsn));
    assert_eq!(pruned.retained_bytes_before_prune, 19);
    assert_eq!(pruned.retained_bytes_after_prune, 16);
    assert_eq!(pruned.removed_segments.len(), 3);
    for index in 0..3 {
        assert!(
            !plan.wal_segment_path(index * plan.segment_bytes).exists(),
            "acked slot should release old segment {index}"
        );
    }
    for index in 3..=18 {
        assert!(
            plan.wal_segment_path(index * plan.segment_bytes).exists(),
            "segment {index} should remain inside the minimum retention window"
        );
    }
}
