//! Runtime-facing alias for the file-owned operational manifest contract.
//!
//! The persistent manifest format and atomic publish rules live in
//! `reddb-file`; the server only orchestrates when to call them.

pub(crate) use reddb_file::OperationalManifest;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::storage::unified::UnifiedStore;

    #[test]
    fn store_reuses_manifest_replay_state_across_operations() {
        let directory = tempfile::tempdir().expect("temp directory");
        let path = directory.path().join("cached.rdb");
        let store = UnifiedStore::new();
        let manifest = store.operational_manifest_for_path(&path);
        let names = (0..200)
            .map(|id| format!("collection-{id}"))
            .collect::<Vec<_>>();
        manifest.recover_or_bootstrap(&names).expect("bootstrap");
        drop(manifest);
        let warm = store.operational_manifest_for_path(&path);
        let mut reader = warm.begin_incremental_read().expect("open").expect("head");
        assert!(reader.step(512).expect("cached replay").complete);
        let cold = OperationalManifest::for_db_path(&path);
        let mut reader = cold.begin_incremental_read().expect("open").expect("head");
        assert!(!reader.step(512).expect("cold bounded replay").complete);
    }

    fn temp_db_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "reddb_operational_manifest_{name}_{}_{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join(reddb_file::DEFAULT_DATABASE_FILE_NAME)
    }

    #[test]
    fn manifest_update_ignores_interrupted_next_generation_publish() {
        let path = temp_db_path("interrupted_update");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("users").unwrap();
        }

        let manifest = OperationalManifest::for_db_path(&path);
        manifest.recover_or_bootstrap(&["users".into()]).unwrap();
        let generation = manifest.read_generation_for_test().unwrap();
        manifest.write_next_manifest_for_test("ghost").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("users").is_some());
        assert!(reopened.get_collection("ghost").is_none());
        assert_eq!(manifest.read_generation_for_test().unwrap(), generation);
    }

    #[test]
    fn interrupted_create_preserves_legacy_unpublished_file() {
        let path = temp_db_path("interrupted_create");
        {
            let _store = UnifiedStore::open(&path).unwrap();
        }
        let manifest = OperationalManifest::for_db_path(&path);
        let orphan = manifest.collection_path_for_test("half_created");
        fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        fs::write(&orphan, b"prepared but unpublished").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("half_created").is_none());
        assert_eq!(fs::read(&orphan).unwrap(), b"prepared but unpublished");
        assert!(!manifest
            .quarantine_path_for_test("half_created.rcol")
            .exists());
    }

    #[test]
    fn interrupted_drop_pending_state_is_completed_on_recovery() {
        let path = temp_db_path("interrupted_drop");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("gone").unwrap();
        }
        let manifest = OperationalManifest::for_db_path(&path);
        manifest.recover_or_bootstrap(&["gone".into()]).unwrap();
        let _drop = manifest.prepare_collection_drop("gone").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("gone").is_none());
        assert!(manifest.collection_path_for_test("gone").exists());
        reopened.create_collection("gone").unwrap();
        reopened.persist().unwrap();
        drop(reopened);
        let recovered = UnifiedStore::open(&path).unwrap();
        assert!(
            recovered.get_collection("gone").is_some(),
            "imported drop must not repeat"
        );
    }

    #[test]
    fn recovery_does_not_rewrite_unreferenced_legacy_files() {
        let path = temp_db_path("orphan_quarantine");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("live").unwrap();
        }
        let manifest = OperationalManifest::for_db_path(&path);
        manifest.recover_or_bootstrap(&["live".into()]).unwrap();
        let orphan = manifest.collection_path_for_test("orphan");
        fs::write(&orphan, b"unreferenced").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("live").is_some());
        assert_eq!(fs::read(&orphan).unwrap(), b"unreferenced");
        assert!(!manifest.quarantine_path_for_test("orphan.rcol").exists());
    }

    #[test]
    fn catalogue_proven_collision_keeps_survivor_artifact_and_is_idempotent() {
        let path = temp_db_path("collision_reconcile");
        let manifest = OperationalManifest::for_db_path(&path);
        manifest.create_collection("red_config").unwrap();
        manifest.create_collection("red.config").unwrap();
        let survivor_path = manifest.collection_path_for_test("red_config");
        let loser_path = manifest.collection_path_for_test("red.config");
        fs::write(&survivor_path, b"survivor").unwrap();
        fs::write(&loser_path, b"loser").unwrap();

        manifest
            .reconcile_collection_collisions(&[("red_config", "red.config", "red.config")])
            .unwrap();
        let generation = manifest.read_generation_for_test().unwrap();
        let current: serde_json::Value =
            serde_json::from_slice(&manifest.legacy_manifest_bytes_for_test().unwrap()).unwrap();
        let collections = current["collections"].as_object().unwrap();
        assert!(!collections.contains_key("red_config"));
        assert_eq!(
            collections["red.config"]["path"].as_str(),
            Some("red_config.rcol")
        );
        assert_eq!(fs::read(&survivor_path).unwrap(), b"survivor");
        assert!(!loser_path.exists());
        assert_eq!(
            fs::read(manifest.quarantine_path_for_test("red.config.rcol")).unwrap(),
            b"loser"
        );

        manifest
            .reconcile_collection_collisions(&[("red_config", "red.config", "red.config")])
            .unwrap();
        assert_eq!(manifest.read_generation_for_test().unwrap(), generation);
        assert_eq!(fs::read(&survivor_path).unwrap(), b"survivor");
    }

    #[test]
    fn checksum_validation_rejects_corrupted_current_manifest() {
        let path = temp_db_path("checksum");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("users").unwrap();
        }
        let manifest = OperationalManifest::for_db_path(&path);
        manifest.recover_or_bootstrap(&["users".into()]).unwrap();
        let manifest_path = manifest.current_manifest_path_for_test();
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let generation = document["generation"].as_u64().unwrap();
        document["generation"] = serde_json::Value::from(generation + 1);
        fs::write(&manifest_path, serde_json::to_vec(&document).unwrap()).unwrap();

        let err = match UnifiedStore::open(&path) {
            Ok(_) => panic!("corrupt manifest must fail closed"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("checksum mismatch"),
            "unexpected error: {err}"
        );
    }
}
