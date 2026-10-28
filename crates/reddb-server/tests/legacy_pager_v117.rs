//! Cross-version migration from a real synthetic 1.17.0 paged store.

use std::fs;
use std::path::Path;

use reddb_file::layout::retired;
use reddb_server::pager_zone_migration::{backup_path_for, migrate_to_zoned, ZoneMigrationError};
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

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

fn fixture_rows(runtime: &RedDBRuntime) -> Vec<(i64, String)> {
    let result = runtime
        .execute_query("SELECT * FROM compatibility_fixture")
        .expect("read migrated fixture");
    let mut rows: Vec<_> = result
        .result
        .records
        .iter()
        .map(|record| {
            let Some(Value::Integer(id)) = record.get("c0").or_else(|| record.get("id")) else {
                panic!("expected integer value");
            };
            let Some(Value::Text(name)) = record.get("c1").or_else(|| record.get("name")) else {
                panic!("expected string value");
            };
            (*id, name.to_string())
        })
        .collect();
    rows.sort();
    rows
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

#[test]
fn v117_paged_store_migrates_and_preserves_rows_across_reopen() {
    let dir = tempfile::tempdir().expect("temporary database");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy-v117");
    copy_tree(&fixture, dir.path());
    let path = dir.path().join("data.rdb");

    let report = migrate_to_zoned(&path).expect("migrate closed v1.17 store");
    assert!(report.backup_path.exists(), "preserve the source data file");
    assert!(retired::first_present_phase1_sidecar(&path).is_none());
    assert!(retired::first_present_phase3_dwb_sidecar(&path).is_none());

    let expected = vec![
        (1, "alpha".to_owned()),
        (2, "beta".to_owned()),
        (3, "gamma".to_owned()),
    ];
    for _ in 0..2 {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("open migrated store");
        assert_eq!(fixture_rows(&runtime), expected);
        runtime.flush().expect("checkpoint migrated store");
    }
}
