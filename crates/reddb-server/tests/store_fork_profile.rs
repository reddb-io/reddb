use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn external_forks_are_rejected_without_creating_a_manifest_in_every_profile() {
    for preset in [
        StorageDeployPreset::Embedded,
        StorageDeployPreset::Serverless,
        StorageDeployPreset::PrimaryReplicaProductionHa,
    ] {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("data.rdb");
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_storage_profile(preset.selection())
                .expect("profile"),
        )
        .expect("runtime");
        runtime
            .execute_query("CREATE TABLE users (id INT)")
            .expect("collection");
        runtime
            .execute_query("INSERT INTO users (id) VALUES (1)")
            .expect("data");
        let manifest = reddb_file::OperationalManifest::for_db_path(&path);
        for error in [
            runtime.fork_store("experiment").unwrap_err(),
            runtime.detach_fork_store("experiment").unwrap_err(),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("external manifests are disabled"),
                "{error}"
            );
        }
        for sql in [
            "FORK STORE AS experiment",
            "DROP FORK experiment",
            "PROMOTE FORK experiment",
        ] {
            let error = runtime
                .execute_query(sql)
                .err()
                .expect("external fork disabled");
            assert!(
                error
                    .to_string()
                    .contains("external manifests are disabled"),
                "{error}"
            );
        }
        assert!(!manifest.current_manifest_path_for_test().exists());
        assert!(manifest.list_forks().expect("read-only listing").is_empty());
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM users")
                .expect("unchanged data")
                .result
                .len(),
            1
        );
    }
}
