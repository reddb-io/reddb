//! Store-fork lifecycle power-cut campaign (#1784).

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Stdio};

use reddb_file::{ForkHydrationState, OperationalManifest};
use unreliable_libc::recover_and_check;

const SHIM_SO: &str = env!("UNRELIABLE_LIBC_SO");
const WORKLOAD_BIN: &str = env!("CARGO_BIN_EXE_store_fork_workload");
const DB_FILE: &str = "store.rdb";
const FORK_NAME: &str = "exp";
const OLD_PRIMARY: &[u8] = b"old-primary";
const NEW_PRIMARY: &[u8] = b"new-primary";
const KILL_AFTER_LIMIT: u64 = 64;

#[derive(Clone, Copy, Debug)]
enum Stage {
    Create,
    Hydrate,
    Promote,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Hydrate => "hydrate",
            Self::Promote => "promote",
        }
    }

    fn all() -> [Self; 3] {
        [Self::Create, Self::Hydrate, Self::Promote]
    }
}

#[test]
fn store_fork_lifecycle_power_cuts_recover_consistently() {
    assert!(Path::new(SHIM_SO).exists(), "shim .so missing at {SHIM_SO}");

    for stage in Stage::all() {
        for kill_after in crash_points() {
            let dir = tempfile::tempdir().unwrap();
            prepare_stage(dir.path(), stage);
            spawn_stage(stage, dir.path(), kill_after);
            assert_recovered(stage, dir.path(), kill_after);
        }
    }
}

fn crash_points() -> Vec<u64> {
    match std::env::var("SEED") {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(seed) => vec![seed.max(1)],
            Err(_) => documented_crash_points(),
        },
        Err(_) => documented_crash_points(),
    }
}

fn documented_crash_points() -> Vec<u64> {
    (1..=KILL_AFTER_LIMIT).collect()
}

fn prepare_stage(dir: &Path, stage: Stage) {
    let parent = manifest(dir);
    parent
        .recover_or_bootstrap(&["users".to_string(), "orders".to_string()])
        .unwrap();
    write_collection(&parent, "users", OLD_PRIMARY);
    write_collection(&parent, "orders", b"orders");

    match stage {
        Stage::Create => {}
        Stage::Hydrate => parent.create_fork(FORK_NAME, 42).unwrap(),
        Stage::Promote => {
            parent.create_fork(FORK_NAME, 42).unwrap();
            let fork = parent.fork_handle(FORK_NAME);
            fork.hydrate_collection("users").unwrap();
            fork.hydrate_collection("orders").unwrap();
            write_collection(&fork, "users", NEW_PRIMARY);
        }
    }
}

fn spawn_stage(stage: Stage, dir: &Path, kill_after: u64) {
    Command::new(WORKLOAD_BIN)
        .arg(stage.name())
        .env("LD_PRELOAD", SHIM_SO)
        .env("UNRELIABLE_SEED", kill_after.to_string())
        .env("UNRELIABLE_DIR", dir)
        .env("UNRELIABLE_POWERCUT", "1")
        .env("UNRELIABLE_KILL_AFTER", kill_after.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|err| {
            panic!(
                "failed to spawn store_fork_workload stage={} kill_after={kill_after}: {err}",
                stage.name()
            )
        });
}

fn assert_recovered(stage: Stage, dir: &Path, kill_after: u64) {
    recover_and_check(dir).unwrap_or_else(|err| {
        panic!(
            "WAL oracle failed for stage={} kill_after={kill_after}: {err}",
            stage.name()
        )
    });

    let parent = manifest(dir);
    parent.recover_or_bootstrap(&[]).unwrap_or_else(|err| {
        panic!(
            "manifest recovery failed for stage={} kill_after={kill_after}: {err}",
            stage.name()
        )
    });

    match stage {
        Stage::Create => assert_create_recovered(&parent),
        Stage::Hydrate => assert_hydrate_recovered(&parent),
        Stage::Promote => assert_promote_recovered(&parent),
    }
}

fn assert_create_recovered(parent: &OperationalManifest) {
    assert_eq!(read_collection(parent, "users"), OLD_PRIMARY);
    let forks = parent.list_forks().unwrap();
    assert!(
        forks.len() <= 1,
        "create crash must leave zero or one live fork, got {forks:?}"
    );
    if forks.is_empty() {
        return;
    }
    let fork = parent.fork_handle(FORK_NAME);
    fork.recover_or_bootstrap(&[]).unwrap();
    fork.hydrate_collection("users").unwrap();
    assert_eq!(read_collection(&fork, "users"), OLD_PRIMARY);
}

fn assert_hydrate_recovered(parent: &OperationalManifest) {
    assert_eq!(read_collection(parent, "users"), OLD_PRIMARY);
    let forks = parent.list_forks().unwrap();
    assert_eq!(forks.len(), 1, "hydrate crash must retain the live fork");
    let fork = parent.fork_handle(FORK_NAME);
    fork.recover_or_bootstrap(&[]).unwrap();
    if forks[0].hydration_state != ForkHydrationState::Hydrated {
        fork.hydrate_collection("users").unwrap();
    }
    assert_eq!(read_collection(&fork, "users"), OLD_PRIMARY);
}

fn assert_promote_recovered(parent: &OperationalManifest) {
    let forks = parent.list_forks().unwrap();
    match forks.len() {
        0 => {
            assert_eq!(read_collection(parent, "users"), NEW_PRIMARY);
            assert!(parent.fork_origin().unwrap().is_none());
        }
        1 => {
            assert_eq!(read_collection(parent, "users"), OLD_PRIMARY);
            let fork = parent.fork_handle(FORK_NAME);
            fork.recover_or_bootstrap(&[]).unwrap();
            assert_eq!(read_collection(&fork, "users"), NEW_PRIMARY);
        }
        other => panic!("promote crash must leave zero or one live fork, got {other}"),
    }
}

fn manifest(dir: &Path) -> OperationalManifest {
    OperationalManifest::for_db_path(&dir.join(DB_FILE))
}

fn write_collection(manifest: &OperationalManifest, name: &str, bytes: &[u8]) {
    std::fs::write(manifest.collection_path_for_test(name), bytes).unwrap();
}

fn read_collection(manifest: &OperationalManifest, name: &str) -> Vec<u8> {
    std::fs::read(manifest.collection_path_for_test(name)).unwrap()
}
