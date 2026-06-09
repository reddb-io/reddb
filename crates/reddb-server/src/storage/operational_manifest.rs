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
        let generation = manifest.read_generation_for_test().unwrap();
        manifest.write_next_manifest_for_test("ghost").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("users").is_some());
        assert!(reopened.get_collection("ghost").is_none());
        assert_eq!(manifest.read_generation_for_test().unwrap(), generation);
    }

    #[test]
    fn interrupted_create_quarantines_prepared_unpublished_file() {
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
        assert!(!orphan.exists());
        assert!(manifest
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
        manifest.begin_drop_collection("gone").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("gone").is_none());
        assert!(!manifest.collection_path_for_test("gone").exists());
    }

    #[test]
    fn recovery_quarantines_unreferenced_physical_files() {
        let path = temp_db_path("orphan_quarantine");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("live").unwrap();
        }
        let manifest = OperationalManifest::for_db_path(&path);
        let orphan = manifest.collection_path_for_test("orphan");
        fs::write(&orphan, b"unreferenced").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("live").is_some());
        assert!(!orphan.exists());
        assert!(manifest.quarantine_path_for_test("orphan.rcol").exists());
    }

    #[test]
    fn checksum_validation_rejects_corrupted_current_manifest() {
        let path = temp_db_path("checksum");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("users").unwrap();
        }
        let manifest_path =
            OperationalManifest::for_db_path(&path).current_manifest_path_for_test();
        let mut text = fs::read_to_string(&manifest_path).unwrap();
        text = text.replace("\"generation\": 1", "\"generation\": 9");
        fs::write(&manifest_path, text).unwrap();

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
