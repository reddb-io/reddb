//! Regression: in the Embedded + SingleFile storage profile (the default for
//! `RedDBOptions::persistent`), a collection's declared model must survive a
//! restart. The contracts that carry `declared_model` live only in RedDB's
//! in-memory cache; before the fix they were never persisted, so on reopen the
//! catalog re-inferred the model from the stored entities. A KV collection is
//! physically stored as table rows, so it came back as a `Table` instead of
//! `Kv` — the bug reported against 1.12.0.

use reddb_server::catalog::CollectionModel;
use reddb_server::{RedDBOptions, RedDBRuntime};

fn declared_model(rt: &RedDBRuntime, collection: &str) -> Option<CollectionModel> {
    rt.db()
        .collection_contract(collection)
        .map(|contract| contract.declared_model)
}

#[test]
fn kv_collection_model_survives_single_file_restart() {
    let dir = tempfile::Builder::new()
        .prefix("reddb-kv-model-recovery-")
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join("data.rdb");

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("runtime boots persistent (single-file)");
        rt.execute_query("CREATE KV sessions")
            .expect("create kv collection");
        // Store a value so the collection holds table-row entities — exactly
        // the shape that made recovery re-infer the model as a table.
        rt.execute_query("KV PUT sessions.token = 'abc123'")
            .expect("kv put");

        assert_eq!(
            declared_model(&rt, "sessions"),
            Some(CollectionModel::Kv),
            "model should be Kv before restart"
        );
        // Drop closes the runtime and flushes the single-file artifact.
    }

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("runtime reopens persistent (single-file)");
        assert_eq!(
            declared_model(&rt, "sessions"),
            Some(CollectionModel::Kv),
            "KV collection must not come back as a table after a restart"
        );
    }
}

/// #2536 contract regression. Parent kills the writer only after acknowledged DDL
/// and KV writes; no Drop or bootstrap checkpoint can rescue the missing catalog.
/// This is a minimal contract fixture, not the private Hunter/RedWire reproducer.
#[test]
fn kv_model_and_value_survive_kill_without_bootstrap_checkpoint() {
    qualify_kv_kill(
        "embedded",
        "kv_model_and_value_survive_kill_without_bootstrap_checkpoint",
    );
}

#[test]
fn paged_kv_model_and_value_survive_kill_without_bootstrap_checkpoint() {
    qualify_kv_kill(
        "paged",
        "paged_kv_model_and_value_survive_kill_without_bootstrap_checkpoint",
    );
}

#[test]
fn legacy_paged_kv_model_and_value_survive_kill_without_bootstrap_checkpoint() {
    qualify_kv_kill(
        "embedded_on_paged",
        "legacy_paged_kv_model_and_value_survive_kill_without_bootstrap_checkpoint",
    );
}

fn qualify_kv_kill(mode: &str, test_name: &str) {
    use reddb_server::StorageDeployPreset;
    use std::time::{Duration, Instant};
    const CHILD: &str = "REDDB_KV_KILL_PATH";
    const MODE: &str = "REDDB_KV_KILL_MODE";
    let options = |path: &std::path::Path, mode: &str| {
        let options = RedDBOptions::persistent(path);
        if mode == "paged" {
            options
                .with_storage_profile(StorageDeployPreset::Serverless.selection())
                .unwrap()
        } else {
            options
        }
    };
    if let Some(path) = std::env::var_os(CHILD) {
        let path = std::path::PathBuf::from(path);
        let mode = std::env::var(MODE).unwrap();
        let runtime = RedDBRuntime::with_options(options(&path, &mode)).unwrap();
        runtime.execute_query("CREATE KV sessions").unwrap();
        runtime
            .execute_query("KV PUT sessions.token = 'acknowledged'")
            .unwrap();
        assert_eq!(
            declared_model(&runtime, "sessions"),
            Some(CollectionModel::Kv)
        );
        std::fs::write(path.with_extension("ready"), b"acknowledged").unwrap();
        loop {
            std::thread::park();
        }
    }
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.rdb");
        if mode == "embedded_on_paged" {
            let runtime = RedDBRuntime::with_options(options(&path, "paged")).unwrap();
            runtime.checkpoint().unwrap();
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD, &path)
            .env(MODE, mode)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while !path.with_extension("ready").exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "{mode}: writer exited before ACK"
            );
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("{mode}: writer did not acknowledge fixture within 30 seconds");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        assert!(
            !child.wait().unwrap().success(),
            "writer must die uncleanly"
        );
        for reopen in 0..3 {
            let runtime = RedDBRuntime::with_options(options(&path, mode)).unwrap();
            assert_eq!(
                declared_model(&runtime, "sessions"),
                Some(CollectionModel::Kv),
                "{mode}: reopen {reopen} lost native model"
            );
            let result = runtime.execute_query("KV GET sessions.token").unwrap();
            assert!(
                !result.result.records.is_empty(),
                "{mode}: lost acknowledged KV value"
            );
            assert!(
                result
                    .result
                    .records
                    .iter()
                    .any(|record| record.get("value")
                        == Some(&reddb_types::Value::text("acknowledged"))),
                "{mode}: wrong KV payload: {:?}",
                result.result.records
            );
            runtime.checkpoint().unwrap();
        }
    }
}
