//! Process-level serverless writer and failover drills.
//!
//! Each contender owns an independent runtime and local database. They share
//! only the remote snapshot and the filesystem-backed CAS lease store, matching
//! separate serverless instances on a shared object backend.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::sync::Arc;
use std::time::{Duration, Instant};

use reddb_server::replication::lease::LeaseStore;
use reddb_server::runtime::lease_lifecycle::LeaseLifecycle;
use reddb_server::storage::backend::{BackendError, LocalBackend, RemoteBackend};
use reddb_server::{RedDBOptions, RedDBRuntime};

const CHILD_MODE_ENV: &str = "REDDB_SERVERLESS_MULTIPROCESS_CHILD";
const ROOT_ENV: &str = "REDDB_SERVERLESS_MULTIPROCESS_ROOT";
const CONTENDER_ENV: &str = "REDDB_SERVERLESS_MULTIPROCESS_CONTENDER";
const CRASH_PHASE_ENV: &str = "REDDB_SERVERLESS_MULTIPROCESS_CRASH_PHASE";
const DATABASE_KEY: &str = "serverless-multiprocess";
const CONTENDERS: usize = 4;
const CRASHED_WRITER_TTL_MS: u64 = 1_200;
const SUCCESSOR_TTL_MS: u64 = 30_000;

fn open_runtime(
    local_path: &Path,
    remote_backend: Arc<dyn RemoteBackend>,
    atomic_backend: Arc<LocalBackend>,
    remote_key: &Path,
) -> RedDBRuntime {
    RedDBRuntime::with_options(
        RedDBOptions::persistent(local_path)
            .with_remote_backend(remote_backend, remote_key.to_string_lossy())
            .with_atomic_remote_backend(atomic_backend),
    )
    .expect("serverless runtime boots")
}

fn attach_lease(
    runtime: &RedDBRuntime,
    root: &Path,
    holder: impl Into<String>,
    ttl_ms: u64,
) -> Arc<LeaseLifecycle> {
    let prefix = format!(
        "{}{}",
        root.join("leases").display(),
        std::path::MAIN_SEPARATOR
    );
    let store = Arc::new(LeaseStore::new(Arc::new(LocalBackend)).with_prefix(prefix));
    let lifecycle = Arc::new(LeaseLifecycle::new(
        store,
        runtime.write_gate_arc(),
        runtime.audit_log_arc(),
        Arc::new(|| {}),
        Arc::new(|| 1),
        holder.into(),
        DATABASE_KEY.to_string(),
        ttl_ms,
    ));
    assert!(
        runtime.set_lease_lifecycle(lifecycle.clone()).is_ok(),
        "one lease lifecycle per runtime"
    );
    lifecycle
}

fn wait_for(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {path:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn spawn_child(test: &str, root: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("current test executable"));
    command
        .arg("--exact")
        .arg(test)
        .arg("--nocapture")
        .env(CHILD_MODE_ENV, "1")
        .env(ROOT_ENV, root);
    command
}

fn wait_for_children(children: Vec<Child>) {
    for mut child in children {
        let status = child.wait().expect("wait for contender child");
        assert!(status.success(), "contender child failed: {status}");
    }
}

fn row_exists(runtime: &RedDBRuntime, id: u64) -> bool {
    runtime
        .execute_query(&format!(
            "SELECT id FROM serverless_process_writes WHERE id = {id}"
        ))
        .expect("query serverless process row")
        .result
        .len()
        == 1
}

#[test]
fn independent_processes_elect_one_writer_and_fence_loser_flushes() {
    if std::env::var(CHILD_MODE_ENV).ok().as_deref() == Some("1") {
        return;
    }

    let root = tempfile::tempdir().expect("private serverless process directory");
    let mut children = Vec::with_capacity(CONTENDERS);
    for contender in 0..CONTENDERS {
        let child = spawn_child("serverless_multiprocess_contender_child", root.path())
            .env(CONTENDER_ENV, contender.to_string())
            .spawn()
            .expect("spawn contender child");
        children.push(child);
    }

    for contender in 0..CONTENDERS {
        wait_for(
            &root.path().join(format!("ready-{contender}")),
            Duration::from_secs(20),
        );
    }
    fs::write(root.path().join("start"), b"go").expect("release contenders");
    for contender in 0..CONTENDERS {
        wait_for(
            &root.path().join(format!("result-{contender}")),
            Duration::from_secs(20),
        );
    }

    let results: Vec<_> = (0..CONTENDERS)
        .map(|contender| {
            fs::read_to_string(root.path().join(format!("result-{contender}")))
                .expect("read contender result")
        })
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_str() == "winner")
            .count(),
        1,
        "exactly one process must hold the writer lease: {results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_str() == "loser")
            .count(),
        CONTENDERS - 1,
        "all other processes must be fenced: {results:?}"
    );

    fs::write(root.path().join("release"), b"done").expect("release winner");
    wait_for_children(children);

    let remote_key = root.path().join("remote").join("main.rdb");
    let observer = open_runtime(
        &root.path().join("observer").join("data.rdb"),
        Arc::new(LocalBackend),
        Arc::new(LocalBackend),
        &remote_key,
    );
    assert_eq!(
        observer
            .execute_query("SELECT * FROM serverless_process_writes")
            .expect("read elected writer snapshot")
            .result
            .len(),
        1,
        "loser flushes must remain local and never replace the winner snapshot"
    );
}

#[test]
fn serverless_multiprocess_contender_child() -> ExitCode {
    if std::env::var(CHILD_MODE_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    let contender: usize = std::env::var(CONTENDER_ENV)
        .expect("contender env")
        .parse()
        .expect("numeric contender");
    let remote_key = root.join("remote").join("main.rdb");
    let runtime = open_runtime(
        &root.join(format!("contender-{contender}")).join("data.rdb"),
        Arc::new(LocalBackend),
        Arc::new(LocalBackend),
        &remote_key,
    );
    let lease = attach_lease(&runtime, &root, format!("writer-{contender}"), 10_000);
    fs::write(root.join(format!("ready-{contender}")), b"ready").expect("publish ready");
    wait_for(&root.join("start"), Duration::from_secs(20));

    if lease.try_acquire().is_ok() {
        runtime
            .execute_query(&format!(
                "INSERT INTO serverless_process_writes (id, holder) VALUES ({contender}, 'writer-{contender}')"
            ))
            .expect("lease winner writes");
        runtime.flush().expect("lease winner publishes snapshot");
        fs::write(root.join("winner-published"), b"published").expect("publish winner marker");
        fs::write(root.join(format!("result-{contender}")), b"winner")
            .expect("publish winner result");
        wait_for(&root.join("release"), Duration::from_secs(20));
        lease.release().expect("winner releases lease");
    } else {
        let error = runtime
            .execute_query(&format!(
                "INSERT INTO serverless_process_writes (id, holder) VALUES ({contender}, 'writer-{contender}')"
            ))
            .expect_err("lease loser write must be fenced")
            .to_string();
        assert!(
            error.contains("writer lease not held"),
            "unexpected fence: {error}"
        );
        wait_for(&root.join("winner-published"), Duration::from_secs(20));
        runtime
            .flush()
            .expect("loser flush persists locally without remote publication");
        fs::write(root.join(format!("result-{contender}")), b"loser")
            .expect("publish loser result");
    }
    ExitCode::SUCCESS
}

#[derive(Clone, Copy)]
enum BlockPhase {
    BeforeUpload,
    AfterUpload,
}

struct BlockingUploadBackend {
    marker: PathBuf,
    phase: BlockPhase,
}

impl RemoteBackend for BlockingUploadBackend {
    fn name(&self) -> &str {
        "blocking-local"
    }

    fn download(&self, remote_key: &str, local_path: &Path) -> Result<bool, BackendError> {
        LocalBackend.download(remote_key, local_path)
    }

    fn upload(&self, local_path: &Path, remote_key: &str) -> Result<(), BackendError> {
        if matches!(self.phase, BlockPhase::AfterUpload) {
            LocalBackend.upload(local_path, remote_key)?;
        }
        fs::write(&self.marker, b"upload-boundary")
            .map_err(|err| BackendError::Transport(format!("write marker: {err}")))?;
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn exists(&self, remote_key: &str) -> Result<bool, BackendError> {
        LocalBackend.exists(remote_key)
    }

    fn delete(&self, remote_key: &str) -> Result<(), BackendError> {
        LocalBackend.delete(remote_key)
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        LocalBackend.list(prefix)
    }
}

#[test]
fn killed_checkpoint_recovers_at_atomic_upload_boundaries_after_lease_expiry() {
    if std::env::var(CHILD_MODE_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for (phase, crashed_row_is_remote) in [("before", false), ("after", true)] {
        let root = tempfile::tempdir().expect("private crash drill directory");
        let remote_key = root.path().join("remote").join("main.rdb");
        let seed = open_runtime(
            &root.path().join("seed").join("data.rdb"),
            Arc::new(LocalBackend),
            Arc::new(LocalBackend),
            &remote_key,
        );
        seed.execute_query("INSERT INTO serverless_process_writes (id, holder) VALUES (0, 'seed')")
            .expect("seed remote snapshot");
        seed.checkpoint().expect("publish seed snapshot");
        drop(seed);

        let mut crashed = spawn_child("serverless_checkpoint_crash_child", root.path())
            .env(CRASH_PHASE_ENV, phase)
            .spawn()
            .expect("spawn checkpoint child");
        wait_for(
            &root.path().join("upload-boundary"),
            Duration::from_secs(20),
        );
        crashed.kill().expect("kill writer during checkpoint");
        let status = crashed.wait().expect("reap killed checkpoint writer");
        assert!(
            !status.success(),
            "checkpoint writer must be killed: {status}"
        );

        let successor = open_runtime(
            &root.path().join("successor").join("data.rdb"),
            Arc::new(LocalBackend),
            Arc::new(LocalBackend),
            &remote_key,
        );
        let successor_lease = attach_lease(
            &successor,
            root.path(),
            "writer-successor",
            SUCCESSOR_TTL_MS,
        );
        assert!(
            successor_lease.try_acquire().is_err(),
            "successor must remain fenced before the killed writer's lease expires"
        );
        let acquire_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if successor_lease.try_acquire().is_ok() {
                break;
            }
            assert!(
                Instant::now() < acquire_deadline,
                "successor did not acquire after lease expiry"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(
            row_exists(&successor, 0),
            "seed row must survive {phase} crash"
        );
        assert_eq!(
            row_exists(&successor, 1),
            crashed_row_is_remote,
            "remote snapshot must be all-old or all-new at {phase} boundary"
        );
        successor
            .execute_query(
                "INSERT INTO serverless_process_writes (id, holder) VALUES (2, 'successor')",
            )
            .expect("successor writes after takeover");
        successor
            .checkpoint()
            .expect("successor publishes snapshot");
        successor_lease.release().expect("successor releases lease");
        drop(successor);

        let observer = open_runtime(
            &root.path().join("observer").join("data.rdb"),
            Arc::new(LocalBackend),
            Arc::new(LocalBackend),
            &remote_key,
        );
        assert!(row_exists(&observer, 0));
        assert_eq!(row_exists(&observer, 1), crashed_row_is_remote);
        assert!(row_exists(&observer, 2));
    }
}

#[test]
fn serverless_checkpoint_crash_child() -> ExitCode {
    if std::env::var(CHILD_MODE_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    let phase = match std::env::var(CRASH_PHASE_ENV).as_deref() {
        Ok("before") => BlockPhase::BeforeUpload,
        Ok("after") => BlockPhase::AfterUpload,
        other => panic!("unexpected crash phase: {other:?}"),
    };
    let remote_key = root.join("remote").join("main.rdb");
    let runtime = open_runtime(
        &root.join("crashed-writer").join("data.rdb"),
        Arc::new(BlockingUploadBackend {
            marker: root.join("upload-boundary"),
            phase,
        }),
        Arc::new(LocalBackend),
        &remote_key,
    );
    let lease = attach_lease(&runtime, &root, "writer-crashed", CRASHED_WRITER_TTL_MS);
    lease.try_acquire().expect("crashed writer acquires lease");
    runtime
        .execute_query("INSERT INTO serverless_process_writes (id, holder) VALUES (1, 'crashed')")
        .expect("crashed writer mutates locally");
    runtime
        .checkpoint()
        .expect("parent kills process before checkpoint returns");
    ExitCode::from(1)
}
