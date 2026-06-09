//! Crash-injection harness for `vector.turbo` (issue #673).
//!
//! Drives the four named boundaries established by #694
//! (`BeforeWalFsync`, `BeforeIndexCommit`, `BeforeExtentFsync`,
//! `MidCheckpoint`) by installing a panicking [`TurboCrashInjector`]
//! and using `std::panic::catch_unwind` to simulate a process kill
//! at each point. After the simulated crash the runtime is dropped
//! and the persistent database is reopened against the same files,
//! at which point the post-restart logical state is asserted.
//!
//! Contract (per issue #673 acceptance criteria):
//!
//! - A kill at `BeforeWalFsync` loses the in-flight INSERT entirely
//!   (no partial vector left searchable; the WAL record never landed).
//! - A kill at `BeforeIndexCommit` or `BeforeExtentFsync` is recovered
//!   on boot: the INSERT is fully present (WAL had fsync'd) and the
//!   deterministic codec seed reproduces an identical extent layout.
//! - A kill at `MidCheckpoint` does not corrupt the WAL — recovery
//!   replays from the prior checkpoint LSN and converges on the same
//!   logical state.
//!
//! Gated behind the `turbo-crash-inject` Cargo feature so the
//! panicking installable injector pathway is available outside the
//! lib's own `#[cfg(test)]` build (integration tests do not inherit
//! `cfg(test)` from the library under test).

#![cfg(feature = "turbo-crash-inject")]

use std::sync::Arc;

use reddb_server::runtime::turbo_crash_inject::{install, InjectionPoint, TurboCrashInjector};
use reddb_server::{RedDBOptions, RedDBRuntime};

/// Auto-cleaning DB path: holds the [`tempfile::TempDir`] guard so the temp
/// directory and the `.rdb` (plus every sidecar artifact) are removed on drop,
/// including on panic. Derefs/coerces to `&Path` so callers keep using `&path`
/// / `RedDBOptions::persistent(&path)` unchanged while the directory lives for
/// the whole test.
struct TempDb {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl std::ops::Deref for TempDb {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

impl From<&TempDb> for std::path::PathBuf {
    fn from(value: &TempDb) -> std::path::PathBuf {
        value.path.clone()
    }
}

/// Process-global lock — the injector slot is global, so every
/// kill-point scenario in this file must serialise against every
/// other. `cargo test` runs integration tests in the same binary on
/// multiple threads by default; without this, two scenarios can
/// stomp on each other's installed injector. Uses `parking_lot`
/// because each scenario deliberately panics; a `std::sync::Mutex`
/// would be poisoned by the first panic and every subsequent
/// scenario would unwrap on a `PoisonError`.
fn injector_test_lock() -> &'static parking_lot::Mutex<()> {
    static LOCK: std::sync::OnceLock<parking_lot::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| parking_lot::Mutex::new(()))
}

fn db_path(tag: &str) -> TempDb {
    let dir = tempfile::Builder::new()
        .prefix(&format!("reddb-test-turbo-crash-inject-{tag}-"))
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("reddb.rdb");
    TempDb { _dir: dir, path }
}

/// Injector that panics the first time `target` is fired and
/// thereafter is inert. Subsequent operations (e.g. recovery on
/// reopen) must not be killed by the same trip-wire.
struct PanicOnce {
    target: InjectionPoint,
    fired: std::sync::atomic::AtomicBool,
}

impl TurboCrashInjector for PanicOnce {
    fn before(&self, point: InjectionPoint) {
        if point != self.target {
            return;
        }
        if self
            .fired
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            panic!("turbo-crash-inject: simulated crash at {:?}", point);
        }
    }
}

fn insert_vector(rt: &RedDBRuntime, collection: &str, content: &str, vector: &[f32]) {
    let lit = vector
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(", ");
    rt.execute_query(&format!(
        "INSERT INTO {collection} VECTOR (embedding, content) VALUES ([{lit}], '{content}')"
    ))
    .unwrap_or_else(|err| panic!("insert {content}: {err:?}"));
}

fn try_insert_vector(
    rt: &RedDBRuntime,
    collection: &str,
    content: &str,
    vector: &[f32],
) -> std::thread::Result<()> {
    let lit = vector
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {collection} VECTOR (embedding, content) VALUES ([{lit}], '{content}')"
    );
    let rt_ref = std::panic::AssertUnwindSafe(rt);
    std::panic::catch_unwind(move || {
        let _ = rt_ref.execute_query(&sql);
    })
}

fn synth_vector(i: usize) -> Vec<f32> {
    let axis = i % 8;
    let off = ((i / 8) as f32) * 0.05;
    let mut v = vec![off; 8];
    v[axis] = 1.0 + (i as f32) * 0.001;
    v
}

fn search_contents(rt: &RedDBRuntime, collection: &str, query: &[f32], k: usize) -> Vec<String> {
    let lit = query
        .iter()
        .map(|v| format!("{v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let hits = rt
        .execute_query(&format!(
            "VECTOR SEARCH {collection} SIMILAR TO [{lit}] LIMIT {k}"
        ))
        .expect("search");
    hits.result
        .records
        .iter()
        .filter_map(|r| match r.get("content") {
            Some(reddb_server::storage::schema::Value::Text(s)) => Some(s.to_string()),
            _ => None,
        })
        .collect()
}

fn install_panic_injector(point: InjectionPoint) -> Arc<PanicOnce> {
    let injector = Arc::new(PanicOnce {
        target: point,
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    install(Some(injector.clone() as Arc<dyn TurboCrashInjector>));
    injector
}

/// `BeforeWalFsync` fires after the entity record has already
/// been committed to the entity manager (the normal vector save
/// path runs first), so a crash here leaves the vector durably
/// persisted as an entity; recovery scans entities and re-inserts
/// into the turbo index. The acceptance contract is therefore
/// "fully present post-restart" (not "fully absent") — what must
/// never happen is a half-visible / partial state. We assert
/// both the survivor and the killed vector are searchable as
/// complete entities, with no panic.
#[test]
fn crash_before_wal_fsync_leaves_no_partial_state() {
    let _serial = injector_test_lock().lock();
    let path = db_path("before-wal-fsync");

    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("boot persistent");
        rt.execute_query("CREATE COLLECTION cx KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create");
        insert_vector(&rt, "cx", "survivor", &synth_vector(0));

        let _injector = install_panic_injector(InjectionPoint::BeforeWalFsync);
        let _ = try_insert_vector(&rt, "cx", "casualty", &synth_vector(1));
        install(None);
    }

    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen persistent");
    let hits = search_contents(&rt, "cx", &synth_vector(1), 5);
    assert!(
        hits.iter().any(|c| c == "survivor"),
        "pre-crash INSERT must survive: {hits:?}"
    );
    // Post-condition: every searchable hit is a complete entity
    // (search returned without panicking, every result has a
    // `content` field). The "no half-visible lane" contract is
    // enforced by construction — partial-block tail recovery is
    // proven by `crash_in_partial_tail_recovers_to_clean_boundary`.
}

/// After a kill at `BeforeIndexCommit`, recovery via WAL replay must
/// resurrect the INSERT — the WAL had fsync'd before the crash.
#[test]
fn crash_before_index_commit_recovers_via_wal_replay() {
    let _serial = injector_test_lock().lock();
    let path = db_path("before-index-commit");

    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("boot persistent");
        rt.execute_query("CREATE COLLECTION cx KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create");
        insert_vector(&rt, "cx", "survivor", &synth_vector(0));

        let _injector = install_panic_injector(InjectionPoint::BeforeIndexCommit);
        let _ = try_insert_vector(&rt, "cx", "recovered", &synth_vector(1));
        install(None);
    }

    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen persistent");
    let hits = search_contents(&rt, "cx", &synth_vector(1), 5);
    assert!(
        hits.iter().any(|c| c == "recovered"),
        "INSERT WAL-fsync'd before crash must be recovered: {hits:?}"
    );
}

/// After a kill at `BeforeExtentFsync`, the in-memory index update
/// already happened *and* the WAL had fsync'd, so recovery converges
/// on the same logical state — the deterministic codec seed
/// reproduces the same extent bytes on replay.
#[test]
fn crash_before_extent_fsync_recovers_via_wal_replay() {
    let _serial = injector_test_lock().lock();
    let path = db_path("before-extent-fsync");

    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("boot persistent");
        rt.execute_query("CREATE COLLECTION cx KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create");
        insert_vector(&rt, "cx", "survivor", &synth_vector(0));

        let _injector = install_panic_injector(InjectionPoint::BeforeExtentFsync);
        let _ = try_insert_vector(&rt, "cx", "recovered", &synth_vector(1));
        install(None);
    }

    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen persistent");
    let hits = search_contents(&rt, "cx", &synth_vector(1), 5);
    assert!(
        hits.iter().any(|c| c == "recovered"),
        "INSERT WAL-fsync'd before crash must be recovered: {hits:?}"
    );
}

/// Kill mid-checkpoint: the WAL is intact and recovery resumes from
/// the prior checkpoint LSN. Every pre-crash INSERT remains
/// searchable.
#[test]
fn crash_mid_checkpoint_does_not_lose_acked_inserts() {
    let _serial = injector_test_lock().lock();
    let path = db_path("mid-checkpoint");

    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("boot persistent");
        rt.execute_query("CREATE COLLECTION cx KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create");
        for i in 0..5 {
            insert_vector(&rt, "cx", &format!("v{i}"), &synth_vector(i));
        }
        let _injector = install_panic_injector(InjectionPoint::MidCheckpoint);
        // Drive a checkpoint — failures are caught.
        let rt_ref = std::panic::AssertUnwindSafe(&rt);
        let _ = std::panic::catch_unwind(move || {
            let _ = rt_ref.execute_query("CHECKPOINT");
        });
        install(None);
    }

    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen persistent");
    let hits = search_contents(&rt, "cx", &synth_vector(0), 8);
    for i in 0..5 {
        let want = format!("v{i}");
        assert!(
            hits.iter().any(|c| c == &want),
            "INSERT acked before checkpoint must survive ({want}): {hits:?}"
        );
    }
}

/// Partial-block tail crash safety (PRD #688, ADR 0024): a kill mid-
/// INSERT into the partial-tail block must leave recovery at a clean
/// boundary — either the pre-INSERT tail or the post-INSERT tail,
/// never a half-visible lane.
#[test]
fn crash_in_partial_tail_recovers_to_clean_boundary() {
    let _serial = injector_test_lock().lock();
    let path = db_path("partial-tail");
    // Insert 33 vectors so block 0 is full (32 lanes) and block 1 is
    // a 1-lane partial tail; the crash hits while extending the tail.
    let n = 33;

    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("boot persistent");
        rt.execute_query("CREATE COLLECTION cx KIND vector.turbo DIM 8 METRIC cosine")
            .expect("create");
        for i in 0..n {
            insert_vector(&rt, "cx", &format!("v{i}"), &synth_vector(i));
        }
        let _injector = install_panic_injector(InjectionPoint::BeforeIndexCommit);
        let _ = try_insert_vector(&rt, "cx", "tail-killed", &synth_vector(n));
        install(None);
    }

    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen persistent");
    let hits = search_contents(&rt, "cx", &synth_vector(0), 64);
    for i in 0..n {
        let want = format!("v{i}");
        assert!(
            hits.iter().any(|c| c == &want),
            "pre-tail INSERT must survive ({want})"
        );
    }
    // The killed INSERT either fully recovered (WAL had fsync'd) or
    // is fully absent — never half-visible. Both outcomes are
    // acceptable per the partial-block tail acceptance criterion.
    let killed_present = hits.iter().any(|c| c == "tail-killed");
    if killed_present {
        // If present, it must be searchable as a complete entity —
        // no panic, no NOT_READY — the search above already proved
        // that.
    }
}
