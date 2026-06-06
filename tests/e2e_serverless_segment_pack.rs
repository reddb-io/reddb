mod support;

use reddb::api::RedDBOptions;
use reddb::runtime::RedDBRuntime;
use reddb::storage::schema::Value;
use reddb::storage::segment_pack::{
    export_segment_pack_with_part_size, hydrate_segment_pack, load_segment_pack_manifest,
    validate_segment_pack,
};
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use support::PersistentDbPath;

fn temp_dir(prefix: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "reddb_segment_pack_{prefix}_{}_{}",
        std::process::id(),
        unique
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_runtime(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path))
        .unwrap_or_else(|err| panic!("runtime opens at {}: {err:?}", path.display()))
}

fn query_rows(rt: &RedDBRuntime, sql: &str, columns: &[&str]) -> Vec<Vec<Value>> {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql} succeeds: {err:?}"))
        .result
        .records
        .into_iter()
        .map(|row| {
            columns
                .iter()
                .map(|column| {
                    row.get(column)
                        .unwrap_or_else(|| panic!("column {column} exists in {row:?}"))
                        .clone()
                })
                .collect()
        })
        .collect()
}

#[test]
fn checkpointed_rdb_exports_to_segment_pack_and_hydrates_equivalent_state() {
    let source = PersistentDbPath::new("serverless_segment_pack_source");
    let rt = source.open_runtime();
    rt.execute_query("CREATE TABLE pack_accounts (id INT, name TEXT, score INT)")
        .unwrap();
    rt.execute_query(
        "INSERT INTO pack_accounts (id, name, score) VALUES \
         (1, 'Ada', 10), (2, 'Grace', 20), (3, 'Linus', 30)",
    )
    .unwrap();
    rt.execute_query("INSERT INTO pack_settings KV (key, value) VALUES ('feature', 'enabled')")
        .unwrap();
    rt.checkpoint().unwrap();
    drop(rt);

    let work = temp_dir("roundtrip");
    let pack_dir = work.join("pack");
    let manifest = export_segment_pack_with_part_size(source.path(), &pack_dir, 1024)
        .expect("export segment pack");

    assert!(
        manifest.parts.len() > 1,
        "test should exercise immutable parts"
    );
    assert_eq!(manifest.recovery_boundary.kind, "checkpointed-rdb");
    assert_eq!(manifest.recovery_boundary.wal_segments_required, 0);
    assert_eq!(
        manifest.source_size_bytes,
        fs::metadata(source.path()).unwrap().len()
    );

    let hydrated_path = work.join("hydrated").join("data.rdb");
    hydrate_segment_pack(&pack_dir, &hydrated_path).expect("hydrate segment pack");

    let source_rt = source.open_runtime();
    let hydrated_rt = open_runtime(&hydrated_path);
    assert_eq!(
        query_rows(
            &source_rt,
            "SELECT id, name, score FROM pack_accounts ORDER BY id",
            &["id", "name", "score"]
        ),
        query_rows(
            &hydrated_rt,
            "SELECT id, name, score FROM pack_accounts ORDER BY id",
            &["id", "name", "score"]
        )
    );
    assert_eq!(
        query_rows(
            &source_rt,
            "SELECT key, value FROM pack_settings ORDER BY key",
            &["key", "value"]
        ),
        query_rows(
            &hydrated_rt,
            "SELECT key, value FROM pack_settings ORDER BY key",
            &["key", "value"]
        )
    );
}

#[test]
fn corrupt_or_incomplete_segment_pack_fails_validation() {
    let source = PersistentDbPath::new("serverless_segment_pack_invalid");
    let rt = source.open_runtime();
    rt.execute_query("CREATE TABLE pack_invalid (id INT, body TEXT)")
        .unwrap();
    rt.execute_query(
        "INSERT INTO pack_invalid (id, body) VALUES \
         (1, 'alpha'), (2, 'beta'), (3, 'gamma')",
    )
    .unwrap();
    rt.checkpoint().unwrap();
    drop(rt);

    let work = temp_dir("invalid");
    let pack_dir = work.join("pack");
    let manifest = export_segment_pack_with_part_size(source.path(), &pack_dir, 1024)
        .expect("export segment pack");
    validate_segment_pack(&pack_dir, &manifest).expect("fresh pack validates");

    let missing = temp_dir("missing");
    copy_dir(&pack_dir, &missing);
    let missing_manifest = load_segment_pack_manifest(&missing).unwrap();
    fs::remove_file(missing.join("parts").join(&missing_manifest.parts[0].name)).unwrap();
    let err = validate_segment_pack(&missing, &missing_manifest).unwrap_err();
    assert!(
        err.to_string().contains("missing"),
        "missing part error should be clear: {err}"
    );

    let corrupt = temp_dir("corrupt");
    copy_dir(&pack_dir, &corrupt);
    let corrupt_manifest = load_segment_pack_manifest(&corrupt).unwrap();
    let first_part = corrupt.join("parts").join(&corrupt_manifest.parts[0].name);
    let mut file = OpenOptions::new().write(true).open(&first_part).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"!").unwrap();
    file.sync_all().unwrap();
    let err = hydrate_segment_pack(&corrupt, corrupt.join("hydrated.rdb")).unwrap_err();
    assert!(
        err.to_string().contains("checksum"),
        "corrupt part error should mention checksum: {err}"
    );
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &dest);
        } else {
            fs::copy(&source, &dest).unwrap();
        }
    }
}
