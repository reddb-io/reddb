//! Serverless multi-instance writer drill.
//!
//! RedDB serverless is deliberately lease-fenced rather than multi-WAL: many
//! instances may race to become the writer, but only one may mutate or publish
//! the shared remote database at a time. This test composes the real runtime,
//! write gate, CAS lease store, remote snapshot transport, and cold reopen.

use std::path::Path;
use std::sync::{Arc, Barrier};

use reddb_server::replication::lease::LeaseStore;
use reddb_server::runtime::lease_lifecycle::LeaseLifecycle;
use reddb_server::storage::backend::LocalBackend;
use reddb_server::{RedDBOptions, RedDBRuntime};

const DATABASE_KEY: &str = "serverless-multi-writer";
const CONTENDERS: usize = 4;

#[derive(Clone)]
struct WriterNode {
    holder: String,
    runtime: RedDBRuntime,
    lifecycle: Arc<LeaseLifecycle>,
}

#[derive(Debug)]
struct WriterAttempt {
    index: usize,
    acquire: Result<(), String>,
    write: Result<(), String>,
}

fn open_runtime(local_path: &Path, backend: Arc<LocalBackend>, remote_key: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(
        RedDBOptions::persistent(local_path)
            .with_remote_backend(backend.clone(), remote_key.to_string_lossy())
            .with_atomic_remote_backend(backend),
    )
    .expect("serverless runtime boots")
}

fn attach_writer(
    runtime: RedDBRuntime,
    store: Arc<LeaseStore>,
    holder: impl Into<String>,
) -> WriterNode {
    let holder = holder.into();
    let lifecycle = Arc::new(LeaseLifecycle::new(
        store,
        runtime.write_gate_arc(),
        runtime.audit_log_arc(),
        Arc::new(|| {}),
        Arc::new(|| 1),
        holder.clone(),
        DATABASE_KEY.to_string(),
        60_000,
    ));
    assert!(
        runtime.set_lease_lifecycle(lifecycle.clone()).is_ok(),
        "one lease lifecycle per runtime"
    );
    WriterNode {
        holder,
        runtime,
        lifecycle,
    }
}

fn row_count(runtime: &RedDBRuntime) -> usize {
    runtime
        .execute_query("SELECT * FROM serverless_writes")
        .expect("serverless rows are readable")
        .result
        .records
        .len()
}

#[test]
fn concurrent_instances_fence_losers_and_cold_successor_preserves_commits() {
    let root = tempfile::tempdir().expect("private serverless drill directory");
    let remote_key = root.path().join("remote").join("main.rdb");
    let lease_prefix = format!(
        "{}{}",
        root.path().join("remote").join("leases").display(),
        std::path::MAIN_SEPARATOR
    );
    let backend = Arc::new(LocalBackend);
    let store = Arc::new(LeaseStore::new(backend.clone()).with_prefix(lease_prefix));

    let nodes: Vec<_> = (0..CONTENDERS)
        .map(|index| {
            let local_path = root
                .path()
                .join(format!("contender-{index}"))
                .join("data.rdb");
            attach_writer(
                open_runtime(&local_path, backend.clone(), &remote_key),
                store.clone(),
                format!("writer-{index}"),
            )
        })
        .collect();

    let start = Arc::new(Barrier::new(CONTENDERS));
    let handles: Vec<_> = nodes
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, node)| {
            let start = start.clone();
            std::thread::spawn(move || {
                start.wait();
                let acquire = node.lifecycle.try_acquire().map_err(|err| err.to_string());
                let write = node
                    .runtime
                    .execute_query(&format!(
                        "INSERT INTO serverless_writes (id, holder) VALUES ({index}, '{}')",
                        node.holder
                    ))
                    .map(|_| ())
                    .map_err(|err| err.to_string());
                WriterAttempt {
                    index,
                    acquire,
                    write,
                }
            })
        })
        .collect();
    let attempts: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("writer contender does not panic"))
        .collect();

    let winners: Vec<_> = attempts
        .iter()
        .filter(|attempt| attempt.acquire.is_ok())
        .collect();
    assert_eq!(winners.len(), 1, "exactly one lease winner: {attempts:?}");
    let winner_index = winners[0].index;
    assert!(
        winners[0].write.is_ok(),
        "the lease holder must be able to write: {attempts:?}"
    );
    for loser in attempts
        .iter()
        .filter(|attempt| attempt.index != winner_index)
    {
        assert!(
            loser.acquire.is_err(),
            "loser acquired the lease: {loser:?}"
        );
        let error = loser.write.as_ref().expect_err("loser write must fail");
        assert!(
            error.contains("writer lease not held"),
            "loser must fail at the runtime write gate, got: {error}"
        );
    }

    let winner = &nodes[winner_index];
    winner
        .runtime
        .checkpoint()
        .expect("lease holder publishes the remote snapshot");
    assert!(
        remote_key.exists(),
        "winner published the shared remote database"
    );

    // A losing contender may checkpoint its local cache, but it must not
    // overwrite the winner's remote object while its lease gate is closed.
    let loser = nodes
        .iter()
        .enumerate()
        .find(|(index, _)| *index != winner_index)
        .map(|(_, node)| node)
        .expect("at least one losing contender");
    loser
        .runtime
        .checkpoint()
        .expect("loser checkpoint remains local-only");
    winner
        .lifecycle
        .release()
        .expect("winner releases the lease");

    let successor_path = root.path().join("successor").join("data.rdb");
    let successor = attach_writer(
        open_runtime(&successor_path, backend.clone(), &remote_key),
        store.clone(),
        "writer-successor",
    );
    successor
        .lifecycle
        .try_acquire()
        .expect("cold successor acquires the released lease");
    assert_eq!(
        row_count(&successor.runtime),
        1,
        "successor hydrates the winner's committed snapshot"
    );
    successor
        .runtime
        .execute_query(
            "INSERT INTO serverless_writes (id, holder) VALUES (100, 'writer-successor')",
        )
        .expect("successor writes after takeover");
    successor
        .runtime
        .checkpoint()
        .expect("successor publishes the next remote snapshot");
    successor
        .lifecycle
        .release()
        .expect("successor releases the lease");

    let observer_path = root.path().join("cold-observer").join("data.rdb");
    let observer = open_runtime(&observer_path, backend, &remote_key);
    assert_eq!(
        row_count(&observer),
        2,
        "a cold reopen observes both sequential writer generations"
    );
}
