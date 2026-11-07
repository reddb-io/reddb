//! Safe refusal of a real synthetic 1.17.0 paged store.

use std::fs;
use std::path::Path;

use reddb_server::pager_zone_migration::{backup_path_for, migrate_to_zoned, ZoneMigrationError};

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create fixture directory");
    for entry in fs::read_dir(from).expect("read fixture directory") {
        let entry = entry.expect("read fixture entry");
        let kind = entry.file_type().expect("fixture file type");
        let dest = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            assert!(kind.is_file(), "fixture must not contain symlinks");
            fs::copy(entry.path(), dest).expect("copy fixture file");
        }
    }
}

fn snapshot_tree(root: &Path, current: &Path, files: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(current).expect("read fixture directory") {
        let entry = entry.expect("read fixture entry");
        let kind = entry.file_type().expect("fixture file type");
        if kind.is_dir() {
            snapshot_tree(root, &entry.path(), files);
        } else {
            assert!(kind.is_file(), "fixture must not contain symlinks");
            files.push((
                entry
                    .path()
                    .strip_prefix(root)
                    .expect("relative path")
                    .to_string_lossy()
                    .into_owned(),
                fs::read(entry.path()).expect("read fixture file"),
            ));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
}

#[test]
fn v117_store_is_refused_without_changing_any_file_until_conversion_exists() {
    let dir = tempfile::tempdir().expect("temporary database");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy-v117");
    copy_tree(&fixture, dir.path());
    let path = dir.path().join("data.rdb");
    let mut before = Vec::new();
    snapshot_tree(dir.path(), dir.path(), &mut before);

    let err = migrate_to_zoned(&path).expect_err("legacy phase-3 conversion is unavailable");
    assert!(matches!(
        err,
        ZoneMigrationError::PrePhase3Unsupported { .. }
    ));
    assert!(!backup_path_for(&path).exists());
    let mut after = Vec::new();
    snapshot_tree(dir.path(), dir.path(), &mut after);
    assert_eq!(
        before, after,
        "refusal must preserve every file byte-for-byte"
    );
}
