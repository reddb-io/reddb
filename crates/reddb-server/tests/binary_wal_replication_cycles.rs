use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reddb_server::auth::policies::Policy;
use reddb_server::auth::store::PrincipalRef;
use reddb_server::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb_server::replication::ReplicationConfig;
use reddb_server::{GrpcServerOptions, RedDBGrpcServer, RedDBOptions, RedDBRuntime};
use reddb_types::Value;

const CHILD_ENV: &str = "REDDB_BINARY_WAL_CYCLE_CHILD";
const ENDPOINT_ENV: &str = "REDDB_BINARY_WAL_CYCLE_ENDPOINT";
const REPLICA_PATH_ENV: &str = "REDDB_BINARY_WAL_CYCLE_REPLICA_PATH";
const PHASE_ENV: &str = "REDDB_BINARY_WAL_CYCLE_PHASE";
const TARGET_LSN_ENV: &str = "REDDB_BINARY_WAL_CYCLE_TARGET_LSN";
const READY_PATH_ENV: &str = "REDDB_BINARY_WAL_CYCLE_READY_PATH";
const RELEASE_PATH_ENV: &str = "REDDB_BINARY_WAL_CYCLE_RELEASE_PATH";
const EXPECTED_STATE_ENV: &str = "REDDB_BINARY_WAL_CYCLE_EXPECTED_STATE";
const REPLICA_LOOP_CRASH_ENV: &str = "REDDB_REPLICA_LOOP_CRASH_AT";
const REPLICA_LOOP_CRASH_LSN_ENV: &str = "REDDB_REPLICA_LOOP_CRASH_LSN";
const AFTER_CURSOR_BEFORE_ACK: &str = "after_cursor_before_ack";
const REPLICA_ID: &str = "follower";

fn install_replication_policy(store: &AuthStore) {
    let policy = Policy::from_json_str(
        r#"{"id":"binary_cycle_replication","version":1,"statements":[{"effect":"allow","actions":["cluster:replication:stream","cluster:replication:ack"],"resources":["cluster:replication"]}]}"#,
    )
    .expect("replication policy parses");
    store.put_policy(policy).expect("put replication policy");
    store
        .attach_policy(
            PrincipalRef::User(UserId::platform(REPLICA_ID)),
            "binary_cycle_replication",
        )
        .expect("attach replication policy");
}

async fn wait_for_port(port: u16) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("gRPC server never came up on port {port}");
}

fn config_u64(runtime: &RedDBRuntime, key: &str) -> Option<u64> {
    match runtime.db().store().get_config(key) {
        Some(Value::Integer(value)) => u64::try_from(value).ok(),
        Some(Value::UnsignedInteger(value)) => Some(value),
        Some(Value::Text(value)) => value.parse().ok(),
        _ => None,
    }
}

fn config_text(runtime: &RedDBRuntime, key: &str) -> Option<String> {
    match runtime.db().store().get_config(key) {
        Some(Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn query_count(runtime: &RedDBRuntime, sql: &str) -> Option<usize> {
    runtime
        .execute_query(sql)
        .ok()
        .map(|result| result.result.records.len())
}

fn phase_visible(runtime: &RedDBRuntime, phase: u8) -> bool {
    match phase {
        1 => {
            query_count(runtime, "SELECT id FROM cycle_items") == Some(6)
                && query_count(
                    runtime,
                    "SELECT id FROM cycle_items WHERE id = 'phase1-4' AND value = 14",
                ) == Some(1)
        }
        2 => {
            query_count(runtime, "SELECT id FROM cycle_items") == Some(8)
                && query_count(
                    runtime,
                    "SELECT id FROM cycle_items WHERE id = 'phase1-0' AND value = 100",
                ) == Some(1)
                && query_count(runtime, "SELECT id FROM cycle_items WHERE id = 'before'") == Some(0)
                && query_count(runtime, "SELECT id FROM cycle_items WHERE id = 'phase2-2'")
                    == Some(1)
        }
        _ => false,
    }
}

fn child_replica_cycle() {
    let endpoint = std::env::var(ENDPOINT_ENV).expect("child endpoint");
    let replica_path = PathBuf::from(std::env::var_os(REPLICA_PATH_ENV).expect("child path"));
    let phase = std::env::var(PHASE_ENV)
        .expect("child phase")
        .parse::<u8>()
        .expect("numeric child phase");
    let target_lsn = std::env::var(TARGET_LSN_ENV)
        .expect("child target LSN")
        .parse::<u64>()
        .expect("numeric child target LSN");
    let ready_path = PathBuf::from(std::env::var_os(READY_PATH_ENV).expect("child ready path"));
    let release_path =
        PathBuf::from(std::env::var_os(RELEASE_PATH_ENV).expect("child release path"));
    let expected_state = std::env::var(EXPECTED_STATE_ENV).ok();

    let mut replication = ReplicationConfig::replica(endpoint);
    replication.max_batch_size = 2;
    replication.poll_interval_ms = 20;
    let replica = RedDBRuntime::with_options(
        RedDBOptions::persistent(replica_path).with_replication(replication),
    )
    .expect("replica runtime starts");
    std::fs::write(&ready_path, b"ready").expect("publish replica process readiness");
    let crash_expected =
        std::env::var(REPLICA_LOOP_CRASH_ENV).ok().as_deref() == Some(AFTER_CURSOR_BEFORE_ACK);

    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let cursor = config_u64(&replica, "replication.last_applied_lsn").unwrap_or(0);
        let relay_applied = replica
            .replica_relay_manifest(REPLICA_ID)
            .expect("read replica relay manifest")
            .map(|manifest| manifest.applied_lsn)
            .unwrap_or(0);
        let state = config_text(&replica, "replication.state");
        let healthy = state.as_deref() == Some("healthy");
        if let Some(expected_state) = expected_state.as_deref() {
            if state.as_deref() == Some(expected_state) {
                return;
            }
        }
        if expected_state.is_none()
            && !crash_expected
            && cursor >= target_lsn
            && relay_applied >= target_lsn
            && healthy
            && phase_visible(&replica, phase)
            && release_path.exists()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "replica phase {phase} did not converge: cursor={cursor} relay={relay_applied} target={target_lsn} state={:?}",
            state
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn native_replica_cycle_child() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        child_replica_cycle();
    }
}

#[derive(Clone)]
struct ReplicaChildContext {
    endpoint: String,
    replica_path: PathBuf,
    api_key: String,
}

fn spawn_replica_child(
    context: ReplicaChildContext,
    phase: u8,
    target_lsn: u64,
    ready_path: PathBuf,
    release_path: PathBuf,
    crash_before_ack: bool,
    expected_state: Option<String>,
) -> tokio::task::JoinHandle<ExitStatus> {
    tokio::task::spawn_blocking(move || {
        let mut command = Command::new(std::env::current_exe().expect("current test executable"));
        command
            .arg("native_replica_cycle_child")
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .env(ENDPOINT_ENV, context.endpoint)
            .env(REPLICA_PATH_ENV, context.replica_path)
            .env(PHASE_ENV, phase.to_string())
            .env(TARGET_LSN_ENV, target_lsn.to_string())
            .env(READY_PATH_ENV, ready_path)
            .env(RELEASE_PATH_ENV, release_path)
            .env("REDDB_REPLICATION_USERNAME", REPLICA_ID)
            .env("REDDB_REPLICATION_API_KEY", context.api_key)
            .env_remove("REDDB_REPLICATION_USERNAME_FILE")
            .env_remove("REDDB_REPLICATION_API_KEY_FILE");
        if crash_before_ack {
            command
                .env(REPLICA_LOOP_CRASH_ENV, AFTER_CURSOR_BEFORE_ACK)
                .env(REPLICA_LOOP_CRASH_LSN_ENV, target_lsn.to_string());
        } else {
            command
                .env_remove(REPLICA_LOOP_CRASH_ENV)
                .env_remove(REPLICA_LOOP_CRASH_LSN_ENV);
        }
        if let Some(expected_state) = expected_state {
            command.env(EXPECTED_STATE_ENV, expected_state);
        } else {
            command.env_remove(EXPECTED_STATE_ENV);
        }
        command.status().expect("run replica child process")
    })
}

async fn wait_for_ready_file(path: &Path) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while !path.exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "native replica process did not finish bootstrap"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn release_child(path: &Path) {
    std::fs::write(path, b"release").expect("release replica child process");
}

async fn wait_for_slot(primary: &RedDBRuntime) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if primary
            .primary_replica_snapshots()
            .iter()
            .any(|slot| slot.id == REPLICA_ID)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "native replica did not register its slot"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_ack(primary: &RedDBRuntime, target_lsn: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let acked = primary
            .primary_replica_snapshots()
            .iter()
            .find(|slot| slot.id == REPLICA_ID)
            .map(|slot| slot.last_acked_lsn)
            .unwrap_or(0);
        if acked >= target_lsn {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "native replica did not ACK target LSN {target_lsn}; last ACK was {acked}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn replica_ack_lsn(primary: &RedDBRuntime) -> u64 {
    primary
        .primary_replica_snapshots()
        .iter()
        .find(|slot| slot.id == REPLICA_ID)
        .map(|slot| slot.last_acked_lsn)
        .unwrap_or(0)
}

async fn join_child(child: tokio::task::JoinHandle<ExitStatus>, phase: &str, expected_code: i32) {
    let status = tokio::time::timeout(Duration::from_secs(30), child)
        .await
        .unwrap_or_else(|_| panic!("replica phase {phase} child timed out"))
        .expect("replica child task joins");
    assert_eq!(
        status.code(),
        Some(expected_code),
        "replica phase {phase} returned {status}"
    );
}

fn assert_persisted_replica(path: &Path, phase: u8, target_lsn: u64, minimum_segments: usize) {
    let replica = RedDBRuntime::with_options(RedDBOptions::persistent(path))
        .expect("reopen persisted replica without follower thread");
    assert!(phase_visible(&replica, phase), "phase {phase} rows persist");
    assert_eq!(
        config_u64(&replica, "replication.last_applied_lsn"),
        Some(target_lsn),
        "phase {phase} cursor must persist across process restart"
    );
    let manifest = replica
        .replica_relay_manifest(REPLICA_ID)
        .expect("read persisted relay manifest")
        .expect("persisted relay manifest exists");
    assert_eq!(manifest.received_lsn, target_lsn);
    assert_eq!(manifest.flushed_lsn, target_lsn);
    assert_eq!(manifest.applied_lsn, target_lsn);
    assert!(
        manifest.segments.len() >= minimum_segments,
        "phase {phase} must persist at least {minimum_segments} bounded relay batches, got {}",
        manifest.segments.len()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_binary_replica_resumes_after_process_restart_with_durable_cursor_and_relay() {
    let directory = tempfile::tempdir().expect("test directory");
    let primary_path = directory.path().join("primary.rdb");
    let replica_path = directory.path().join("replica.rdb");
    let primary = RedDBRuntime::with_options(
        RedDBOptions::persistent(primary_path).with_replication(ReplicationConfig::primary()),
    )
    .expect("primary runtime");
    primary
        .execute_query("CREATE TABLE cycle_items (id TEXT, value INTEGER)")
        .expect("create cycle table");
    primary
        .execute_query("INSERT INTO cycle_items (id, value) VALUES ('before', 1)")
        .expect("seed primary snapshot");

    let store = Arc::new(AuthStore::new(AuthConfig {
        enabled: true,
        require_auth: true,
        ..AuthConfig::default()
    }));
    store
        .create_user(REPLICA_ID, "test-password", Role::Read)
        .expect("create replica principal");
    let api_key = store
        .create_api_key(REPLICA_ID, "binary-cycle", Role::Read)
        .expect("create replica API key");
    install_replication_policy(&store);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind gRPC listener");
    let port = listener.local_addr().expect("listener address").port();
    let server = RedDBGrpcServer::with_options(
        primary.clone(),
        GrpcServerOptions {
            bind_addr: format!("127.0.0.1:{port}"),
            tls: None,
        },
        store.clone(),
    );
    let server_task = tokio::spawn(async move {
        server.serve_on(listener).await.expect("gRPC server");
    });
    wait_for_port(port).await;
    let endpoint = format!("http://127.0.0.1:{port}");
    let child_context = ReplicaChildContext {
        endpoint,
        replica_path: replica_path.clone(),
        api_key: api_key.key,
    };

    let phase1_baseline = primary.primary_logical_head_lsn();
    let phase1_target = phase1_baseline.saturating_add(5);
    let phase1_ready = directory.path().join("phase1.ready");
    let phase1_release = directory.path().join("phase1.release");
    let phase1_child = spawn_replica_child(
        child_context.clone(),
        1,
        phase1_target,
        phase1_ready.clone(),
        phase1_release,
        true,
        None,
    );
    wait_for_slot(&primary).await;
    wait_for_ready_file(&phase1_ready).await;
    assert_eq!(
        primary.primary_logical_head_lsn(),
        phase1_baseline,
        "phase 1 writes must start only after snapshot bootstrap"
    );
    for index in 0..5 {
        primary
            .execute_query(&format!(
                "INSERT INTO cycle_items (id, value) VALUES ('phase1-{index}', {})",
                index + 10
            ))
            .expect("write phase 1 mutation");
    }
    assert_eq!(
        primary.primary_logical_head_lsn(),
        phase1_target,
        "each phase 1 mutation must publish one logical WAL record"
    );
    join_child(phase1_child, "1 crash", 177).await;
    assert!(
        replica_ack_lsn(&primary) < phase1_target,
        "crash point must stop the process after cursor persistence and before ACK"
    );
    assert_persisted_replica(&replica_path, 1, phase1_target, 3);

    let recovery_child = spawn_replica_child(
        child_context.clone(),
        1,
        phase1_target,
        directory.path().join("phase1-recovery.ready"),
        directory.path().join("phase1-recovery.release"),
        false,
        None,
    );
    wait_for_ack(&primary, phase1_target).await;
    release_child(&directory.path().join("phase1-recovery.release"));
    join_child(recovery_child, "1 recovery", 0).await;
    assert_persisted_replica(&replica_path, 1, phase1_target, 3);

    primary
        .execute_query("UPDATE cycle_items SET value = 100 WHERE id = 'phase1-0'")
        .expect("phase 2 update");
    primary
        .execute_query("DELETE FROM cycle_items WHERE id = 'before'")
        .expect("phase 2 delete");
    for index in 0..3 {
        primary
            .execute_query(&format!(
                "INSERT INTO cycle_items (id, value) VALUES ('phase2-{index}', {})",
                index + 20
            ))
            .expect("write phase 2 insert");
    }
    let phase2_target = primary.primary_logical_head_lsn();
    assert_eq!(
        phase2_target,
        phase1_target + 5,
        "each phase 2 mutation must publish one logical WAL record"
    );

    let phase2_child = spawn_replica_child(
        child_context.clone(),
        2,
        phase2_target,
        directory.path().join("phase2.ready"),
        directory.path().join("phase2.release"),
        false,
        None,
    );
    wait_for_ack(&primary, phase2_target).await;
    release_child(&directory.path().join("phase2.release"));
    join_child(phase2_child, "2 restart", 0).await;
    assert_persisted_replica(&replica_path, 2, phase2_target, 6);

    server_task.abort();
    let _ = server_task.await;

    let behind_primary = RedDBRuntime::with_options(
        RedDBOptions::persistent(directory.path().join("behind-primary.rdb"))
            .with_replication(ReplicationConfig::primary()),
    )
    .expect("behind primary runtime");
    assert_eq!(behind_primary.primary_logical_head_lsn(), 0);
    let behind_listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind behind-primary gRPC listener");
    let behind_port = behind_listener
        .local_addr()
        .expect("behind-primary listener address")
        .port();
    let behind_server = RedDBGrpcServer::with_options(
        behind_primary.clone(),
        GrpcServerOptions {
            bind_addr: format!("127.0.0.1:{behind_port}"),
            tls: None,
        },
        store,
    );
    let behind_server_task = tokio::spawn(async move {
        behind_server
            .serve_on(behind_listener)
            .await
            .expect("behind-primary gRPC server");
    });
    wait_for_port(behind_port).await;
    let behind_child = spawn_replica_child(
        ReplicaChildContext {
            endpoint: format!("http://127.0.0.1:{behind_port}"),
            ..child_context
        },
        2,
        phase2_target,
        directory.path().join("behind-primary.ready"),
        directory.path().join("behind-primary.release"),
        false,
        Some("rejoin_primary_behind".to_string()),
    );
    wait_for_ready_file(&directory.path().join("behind-primary.ready")).await;
    join_child(behind_child, "behind-primary fence", 0).await;
    assert_eq!(
        replica_ack_lsn(&behind_primary),
        0,
        "a primary must not accept or self-register progress beyond its own head"
    );
    behind_server_task.abort();
}
