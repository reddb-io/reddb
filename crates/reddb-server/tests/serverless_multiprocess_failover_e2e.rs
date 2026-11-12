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
use reddb_server::runtime::serverless_snapshot_publication::SnapshotPublicationStore;
use reddb_server::storage::backend::{
    AtomicRemoteBackend, BackendError, BackendObjectVersion, ConditionalDelete, ConditionalPut,
    LocalBackend, RemoteBackend,
};
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

fn open_fenced_runtime(
    local_path: &Path,
    root: &Path,
    holder: &str,
    ttl_ms: u64,
    remote_backend: Arc<dyn RemoteBackend>,
    atomic_backend: Arc<dyn AtomicRemoteBackend>,
) -> (RedDBRuntime, Arc<LeaseLifecycle>) {
    let remote_key = root.join("remote").join("main.rdb");
    let remote_key_string = remote_key.to_string_lossy().to_string();
    let publication = Arc::new(SnapshotPublicationStore::new(
        atomic_backend.clone(),
        remote_key_string.clone(),
    ));
    let bootstrap_key = publication
        .observe_for_boot()
        .expect("observe serverless snapshot head");
    let mut options = RedDBOptions::persistent(local_path)
        .with_remote_backend(remote_backend, remote_key_string)
        .with_atomic_remote_backend(atomic_backend.clone());
    options.remote_bootstrap_key = bootstrap_key;
    let runtime = RedDBRuntime::with_options(options).expect("fenced serverless runtime boots");
    let prefix = format!(
        "{}{}",
        root.join("leases").display(),
        std::path::MAIN_SEPARATOR
    );
    let lifecycle = Arc::new(
        LeaseLifecycle::new(
            Arc::new(LeaseStore::new(atomic_backend).with_prefix(prefix)),
            runtime.write_gate_arc(),
            runtime.audit_log_arc(),
            Arc::new(|| {}),
            Arc::new(|| 1),
            holder.to_string(),
            DATABASE_KEY.to_string(),
            ttl_ms,
        )
        .with_snapshot_publication(publication, local_path),
    );
    assert!(runtime.set_lease_lifecycle(lifecycle.clone()).is_ok());
    (runtime, lifecycle)
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

struct PausedGenerationUploadBackend {
    marker: PathBuf,
    resume: PathBuf,
}

impl RemoteBackend for PausedGenerationUploadBackend {
    fn name(&self) -> &str {
        "paused-generation-local"
    }

    fn download(&self, remote_key: &str, local_path: &Path) -> Result<bool, BackendError> {
        LocalBackend.download(remote_key, local_path)
    }

    fn upload(&self, local_path: &Path, remote_key: &str) -> Result<(), BackendError> {
        LocalBackend.upload(local_path, remote_key)
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

impl AtomicRemoteBackend for PausedGenerationUploadBackend {
    fn object_version(
        &self,
        remote_key: &str,
    ) -> Result<Option<BackendObjectVersion>, BackendError> {
        LocalBackend.object_version(remote_key)
    }

    fn upload_conditional(
        &self,
        local_path: &Path,
        remote_key: &str,
        condition: ConditionalPut,
    ) -> Result<BackendObjectVersion, BackendError> {
        if remote_key.ends_with(".serverless/snapshots/1/2.rdb") {
            fs::write(&self.marker, b"paused")
                .map_err(|err| BackendError::Transport(format!("write pause marker: {err}")))?;
            wait_for(&self.resume, Duration::from_secs(20));
        }
        LocalBackend.upload_conditional(local_path, remote_key, condition)
    }

    fn delete_conditional(
        &self,
        remote_key: &str,
        condition: ConditionalDelete,
    ) -> Result<(), BackendError> {
        LocalBackend.delete_conditional(remote_key, condition)
    }
}

#[test]
fn successor_epoch_fences_an_old_upload_that_resumes_after_takeover() {
    if std::env::var(CHILD_MODE_ENV).ok().as_deref() == Some("1") {
        return;
    }
    let root = tempfile::tempdir().expect("private delayed-publication directory");
    let mut stale = spawn_child("serverless_delayed_stale_publication_child", root.path())
        .spawn()
        .expect("spawn stale publication child");
    wait_for(
        &root.path().join("stale-upload-paused"),
        Duration::from_secs(20),
    );

    let successor_path = root.path().join("successor-fenced").join("data.rdb");
    let (successor, successor_lease) = open_fenced_runtime(
        &successor_path,
        root.path(),
        "writer-successor",
        SUCCESSOR_TTL_MS,
        Arc::new(LocalBackend),
        Arc::new(LocalBackend),
    );
    let acquire_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if successor_lease.try_acquire().is_ok() {
            break;
        }
        assert!(
            Instant::now() < acquire_deadline,
            "successor did not acquire after stale writer lease expiry"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    fs::write(root.path().join("resume-stale-upload"), b"resume").expect("resume stale upload");
    wait_for(
        &root.path().join("stale-upload-result"),
        Duration::from_secs(20),
    );
    assert_eq!(
        fs::read_to_string(root.path().join("stale-upload-result"))
            .expect("read stale upload result"),
        "fenced"
    );
    assert!(
        stale.wait().expect("reap stale writer").success(),
        "stale child handles fencing without crashing"
    );

    successor
        .execute_query("INSERT INTO serverless_process_writes (id, holder) VALUES (2, 'successor')")
        .expect("successor writes after claiming the head");
    successor
        .checkpoint()
        .expect("successor advances the canonical head");
    successor_lease.release().expect("successor releases lease");
    drop(successor);

    let observer_path = root.path().join("fenced-observer").join("data.rdb");
    let remote_key = root.path().join("remote").join("main.rdb");
    let publication = SnapshotPublicationStore::new(
        Arc::new(LocalBackend),
        remote_key.to_string_lossy().to_string(),
    );
    let mut observer_options = RedDBOptions::persistent(&observer_path)
        .with_remote_backend(Arc::new(LocalBackend), remote_key.to_string_lossy())
        .with_atomic_remote_backend(Arc::new(LocalBackend));
    observer_options.remote_bootstrap_key = publication
        .observe_for_boot()
        .expect("observe final snapshot head");
    let observer = RedDBRuntime::with_options(observer_options)
        .expect("cold observer hydrates canonical head");
    assert!(row_exists(&observer, 2));
    assert!(!row_exists(&observer, 1));
}

#[test]
fn lease_only_profiles_preserve_native_files() {
    let root = tempfile::tempdir().expect("lease-only boot directory");
    let mut child = spawn_child("lease_only_native_boot_child", root.path())
        .env("RED_LEASE_REQUIRED", "true")
        .env("REDDB_LOCAL_BACKEND_ROOT", root.path())
        .spawn()
        .expect("run lease-only preparation child");
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("lease-only child status") {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop stalled lease-only preparation");
            child.wait().expect("join stopped lease-only child");
            panic!("lease-only preparation exceeded 60 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "lease-only preparation failed: {status}");
}

#[test]
fn lease_only_native_boot_child() -> ExitCode {
    if std::env::var(CHILD_MODE_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    use reddb_server::runtime::lease_loop::prepare_lease_loop_if_required;
    use reddb_server::storage::engine::{page::PageType, pager::Pager};
    use reddb_server::StorageDeployPreset;

    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    for preset in [
        StorageDeployPreset::PrimaryReplicaDev,
        StorageDeployPreset::Cluster,
    ] {
        let local = root.join(format!("{}.rdb", preset.as_str()));
        let pager = Pager::open_default(&local).expect("native authoritative file");
        let mut page = pager.allocate_page(PageType::BTreeLeaf).expect("data page");
        let page_id = page.page_id();
        page.insert_cell(b"owned", b"local-authority")
            .expect("native data");
        pager.write_page(page_id, page).expect("write data");
        pager.sync().expect("durable native data");
        drop(pager);
        let original = fs::read(&local).expect("preserved native image");

        let remote_key = format!("{}/remote.rdb", preset.as_str());
        let head = root.join(reddb_file::serverless_snapshot_head_key(&remote_key));
        fs::create_dir_all(head.parent().expect("head directory")).expect("remote directory");
        let unrelated = b"lease-only preparation must not decode this serverless head";
        fs::write(&head, unrelated).expect("unrelated serverless head");
        let mut options = RedDBOptions::persistent(&local)
            .with_storage_profile(preset.selection())
            .expect("valid local profile")
            .with_remote_backend(Arc::new(LocalBackend), remote_key)
            .with_atomic_remote_backend(Arc::new(LocalBackend));
        let boot = prepare_lease_loop_if_required(&mut options).expect("lease-only boot plan");
        assert!(boot.is_some(), "ownership fencing remains required");
        assert_eq!(options.data_path.as_ref(), Some(&local));
        assert!(options.remote_bootstrap_key.is_none());
        assert_eq!(
            fs::read(&local).expect("local image after preparation"),
            original
        );
        assert_eq!(fs::read(&head).expect("unrelated head remains"), unrelated);
        let pager = Pager::open_default(&local).expect("cold native reopen");
        let page = pager.read_page(page_id).expect("cold native data");
        assert_eq!(
            page.read_cell(0).expect("owned cell"),
            (&b"owned"[..], &b"local-authority"[..])
        );
    }
    ExitCode::SUCCESS
}

#[test]
fn operational_serverless_boot_recovers_canonical_snapshots() {
    let root = tempfile::tempdir().expect("operational boot directory");
    let status = spawn_child("operational_serverless_boot_child", root.path())
        .env("RED_LEASE_REQUIRED", "true")
        .env("RED_LEASE_TTL_SECS", "6")
        .env("RED_LEASE_PREFIX", "leases/")
        .env("REDDB_LOCAL_BACKEND_ROOT", root.path())
        .status()
        .expect("run production boot child");
    assert!(
        status.success(),
        "production serverless boot failed: {status}"
    );
}

#[test]
fn operational_serverless_boot_child() -> ExitCode {
    if std::env::var(CHILD_MODE_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    use reddb_server::runtime::lease_loop::{
        prepare_lease_loop_if_required, start_prepared_lease_loop,
    };
    use reddb_server::storage::profile::StorageDeployPreset;
    let _ = tracing_subscriber::fmt()
        .with_env_filter("reddb::lifecycle=debug,reddb::serverless=debug")
        .try_init();
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    let base_path = root.join("writer").join("data.rdb");
    let remote_key = "remote/main.rdb";
    for id in 1..=4 {
        let mut options = RedDBOptions::persistent(&base_path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("serverless preset")
            .with_remote_backend(Arc::new(LocalBackend), remote_key)
            .with_atomic_remote_backend(Arc::new(LocalBackend));
        let boot = prepare_lease_loop_if_required(&mut options).expect("prepare production boot");
        if id > 1 {
            assert_ne!(
                options.data_path.as_ref(),
                Some(&base_path),
                "old operational WAL must be isolated"
            );
        }
        let runtime = RedDBRuntime::with_options(options).expect("open native serverless cache");
        assert!(runtime.db().is_paged());
        eprintln!("operational boot {id}, path {:?}", runtime.db().path());
        start_prepared_lease_loop(&runtime, boot).expect("claim observed head before serving");
        for previous in 1..id {
            assert!(row_exists(&runtime, previous));
        }
        if id == 4 {
            runtime.graceful_shutdown(false).expect("close observer");
            break;
        }
        runtime
            .execute_query(&format!(
                "INSERT INTO serverless_process_writes (id, holder) VALUES ({id}, 'operational')"
            ))
            .expect("native writer commits");
        assert!(row_exists(&runtime, id));
        runtime.lifecycle().mark_draining();
        // The renewal thread must retain ownership while requests drain.
        // Crossing its ttl/3 refresh boundary used to release the lease here.
        std::thread::sleep(Duration::from_secs(3));
        runtime
            .graceful_shutdown(false)
            .expect("publish final native snapshot");
        let head = reddb_file::decode_serverless_snapshot_head_json(
            &fs::read(root.join(reddb_file::serverless_snapshot_head_key(remote_key)))
                .expect("head after shutdown"),
        )
        .expect("decode shutdown head");
        eprintln!("operational shutdown {id}, head {head:?}");
        assert!(
            head.publication_sequence >= id * 2,
            "shutdown must advance the canonical snapshot"
        );
    }
    ExitCode::SUCCESS
}

#[test]
fn clean_restarts_preserve_snapshots_and_shutdown_publication() {
    let root = tempfile::tempdir().expect("restart directory");
    let local = root.path().join("writer").join("data.rdb");
    let remote_key = root.path().join("remote").join("main.rdb");
    let head_key = reddb_file::serverless_snapshot_head_key(&remote_key.to_string_lossy());
    let mut published = Vec::new();
    for id in 1..=3 {
        let (runtime, lease) = open_fenced_runtime(
            &local,
            root.path(),
            "same-holder",
            SUCCESSOR_TTL_MS,
            Arc::new(LocalBackend),
            Arc::new(LocalBackend),
        );
        lease.try_acquire().expect("claim after clean release");
        assert_eq!(lease.current_lease().expect("held lease").generation, 1);
        for previous in 1..id {
            assert!(row_exists(&runtime, previous));
        }
        runtime
            .execute_query(&format!(
                "INSERT INTO serverless_process_writes (id, holder) VALUES ({id}, 'restart')"
            ))
            .expect("write in each boot");
        match id {
            1 => runtime.flush().expect("flush publishes"),
            2 => runtime.checkpoint().expect("checkpoint publishes"),
            _ => {
                runtime
                    .graceful_shutdown(false)
                    .expect("shutdown publishes");
            }
        }
        let head = reddb_file::decode_serverless_snapshot_head_json(
            &fs::read(&head_key).expect("read head"),
        )
        .expect("decode head");
        published.push((
            head.snapshot_key.clone(),
            fs::read(&head.snapshot_key).expect("payload"),
        ));
        lease.release().expect("release deletes lease object");
        drop(lease);
        drop(runtime);
    }
    let (observer, _) = open_fenced_runtime(
        &root.path().join("observer.rdb"),
        root.path(),
        "observer",
        SUCCESSOR_TTL_MS,
        Arc::new(LocalBackend),
        Arc::new(LocalBackend),
    );
    for id in 1..=3 {
        assert!(row_exists(&observer, id));
    }
    for (key, original) in published {
        assert_eq!(fs::read(key).expect("old payload remains"), original);
    }
}

#[test]
fn hydration_cannot_overwrite_a_live_local_writer() {
    let root = tempfile::tempdir().expect("local fence directory");
    let local = root.path().join("writer.rdb");
    let (runtime, lease) = open_fenced_runtime(
        &local,
        root.path(),
        "writer",
        SUCCESSOR_TTL_MS,
        Arc::new(LocalBackend),
        Arc::new(LocalBackend),
    );
    lease.try_acquire().expect("first writer acquires");
    runtime
        .execute_query("INSERT INTO serverless_process_writes (id, holder) VALUES (1, 'local')")
        .expect("local write newer than remote snapshot");
    let before = fs::read(&local).expect("local artifact before hydration attempt");
    let remote_key = root.path().join("remote").join("main.rdb");
    let publication =
        SnapshotPublicationStore::new(Arc::new(LocalBackend), remote_key.to_string_lossy());
    let mut options = RedDBOptions::persistent(&local)
        .with_remote_backend(Arc::new(LocalBackend), remote_key.to_string_lossy());
    options.remote_bootstrap_key = publication
        .observe_for_boot()
        .expect("observe old snapshot");
    assert!(RedDBRuntime::with_options(options).is_err());
    assert_eq!(
        fs::read(&local).expect("local artifact after rejection"),
        before
    );
    assert!(row_exists(&runtime, 1));
    lease.release().expect("release writer");
}

#[test]
fn serverless_delayed_stale_publication_child() -> ExitCode {
    if std::env::var(CHILD_MODE_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    let backend = Arc::new(PausedGenerationUploadBackend {
        marker: root.join("stale-upload-paused"),
        resume: root.join("resume-stale-upload"),
    });
    let local_path = root.join("stale-writer").join("data.rdb");
    let (runtime, lease) = open_fenced_runtime(
        &local_path,
        &root,
        "writer-stale",
        CRASHED_WRITER_TTL_MS,
        backend.clone(),
        backend,
    );
    lease
        .try_acquire()
        .expect("stale writer acquires first epoch");
    runtime
        .execute_query("INSERT INTO serverless_process_writes (id, holder) VALUES (1, 'stale')")
        .expect("stale writer mutates locally");
    let result = runtime.checkpoint();
    let outcome = match result {
        Err(err) if err.to_string().contains("publication") => "fenced",
        Err(err) => panic!("unexpected stale publication error: {err}"),
        Ok(()) => "published",
    };
    fs::write(root.join("stale-upload-result"), outcome).expect("publish stale upload result");
    ExitCode::SUCCESS
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
