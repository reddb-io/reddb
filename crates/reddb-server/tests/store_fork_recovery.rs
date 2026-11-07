use reddb_server::api::DurabilityMode;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use std::path::Path;
use std::time::{Duration, Instant};

struct CrashChild(std::process::Child);

impl Drop for CrashChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn options(path: &Path, mode: &str) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("profile")
        .with_durability_mode(match mode {
            "strict" => DurabilityMode::Strict,
            "grouped" => DurabilityMode::WalDurableGrouped,
            "async" => DurabilityMode::Async,
            _ => panic!("fixture mode"),
        })
}

fn rows(runtime: &RedDBRuntime) -> usize {
    runtime
        .execute_query("SELECT id FROM items")
        .expect("data")
        .result
        .records
        .len()
}

#[test]
fn fork_crash_child() {
    let Some(path) = std::env::var_os("REDDB_FORK_CHILD") else {
        return;
    };
    let mode = std::env::var("REDDB_FORK_MODE").expect("mode");
    let operation = std::env::var("REDDB_FORK_OPERATION").expect("operation");
    let point = std::env::var("REDDB_FORK_POINT").expect("point");
    let runtime = RedDBRuntime::with_options(options(Path::new(&path), &mode)).expect("runtime");
    runtime
        .execute_query("CREATE TABLE items (id INT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO items (id) VALUES (1)")
        .expect("data");
    if operation != "create" {
        runtime.fork_store("candidate").expect("fork");
    }
    std::env::set_var("REDDB_STORE_FORK_CRASH_AT", point);
    match operation.as_str() {
        "create" => {
            runtime.fork_store("candidate").expect("fork");
        }
        "drop" => {
            runtime.drop_fork_store("candidate").expect("drop");
        }
        "detach" => {
            runtime.detach_fork_store("candidate").expect("detach");
        }
        "promote" => {
            runtime.promote_fork_store("candidate").expect("promote");
        }
        _ => panic!("fixture operation"),
    }
    // An acknowledged operation must survive without running destructors/checkpoint.
    std::process::exit(0);
}

fn campaign(operation: &str, mode: &str, points: &[&str]) {
    for point in points {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("data.rdb");
        let mut child = CrashChild(
            std::process::Command::new(std::env::current_exe().expect("binary"))
                .args(["--exact", "fork_crash_child", "--nocapture"])
                .env("REDDB_FORK_CHILD", &path)
                .env("REDDB_FORK_MODE", mode)
                .env("REDDB_FORK_OPERATION", operation)
                .env("REDDB_FORK_POINT", point)
                .spawn()
                .expect("child"),
        );
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.0.try_wait().expect("child status") {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "fork child stalled: {operation}/{point}/{mode}"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(
            status.code(),
            Some(if *point == "acknowledged" { 0 } else { 173 }),
            "{operation}/{point}/{mode}"
        );
        for _ in 0..3 {
            let runtime = RedDBRuntime::with_options(options(&path, mode)).expect("recover parent");
            assert_eq!(
                rows(&runtime),
                1,
                "parent survives {operation}/{point}/{mode}"
            );
            let ready = operation == "create" && !matches!(*point, "create_intent" | "create_copy");
            assert_eq!(
                runtime.list_store_forks().expect("listing").len(),
                usize::from(ready)
            );
            if ready || matches!(operation, "detach" | "promote") {
                let child_path = runtime
                    .fork_store_path("candidate")
                    .expect("path")
                    .expect("retained fork");
                let child =
                    RedDBRuntime::with_options(options(&child_path, mode)).expect("recover fork");
                assert_eq!(rows(&child), 1);
                assert!(child
                    .list_store_forks()
                    .expect("no parent fork catalog")
                    .is_empty());
            } else {
                assert!(runtime
                    .fork_store_path("candidate")
                    .expect("absent fork")
                    .is_none());
                let databases = std::fs::read_dir(directory.path())
                    .expect("directory")
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "rdb")
                    })
                    .count();
                assert_eq!(databases, 1, "unfinished/dropped fork must be reclaimed");
            }
            runtime.checkpoint().expect("checkpoint recovery");
            assert!(!reddb_file::physical_metadata_binary_path(&path).exists());
            assert!(!reddb_file::OperationalManifest::for_db_path(&path)
                .current_manifest_path_for_test()
                .exists());
        }
    }
}

// Keep each durability matrix independently bounded by the ordinary test timeout.
macro_rules! recovery_modes {
    ($name:ident, $operation:literal, $points:expr) => {
        mod $name {
            use super::*;
            #[test]
            fn strict() {
                campaign($operation, "strict", $points);
            }
            #[test]
            fn grouped() {
                campaign($operation, "grouped", $points);
            }
            #[test]
            fn asynchronous() {
                campaign($operation, "async", $points);
            }
        }
    };
}

recovery_modes!(
    create,
    "create",
    &[
        "create_intent",
        "create_copy",
        "create_origin",
        "create_snapshot",
        "create_ready",
        "acknowledged"
    ]
);
recovery_modes!(
    discard,
    "drop",
    &["drop_intent", "drop_ready", "acknowledged"]
);
recovery_modes!(
    detach,
    "detach",
    &["release_intent", "release_ready", "acknowledged"]
);
recovery_modes!(
    promote,
    "promote",
    &["release_intent", "release_ready", "acknowledged"]
);
