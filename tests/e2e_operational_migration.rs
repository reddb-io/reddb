mod support;

use reddb::api::RedDBOptions;
use reddb::runtime::RedDBRuntime;
use reddb::storage::operational_migration::{
    load_operational_migration_manifest, migrate_embedded_to_operational,
    validate_operational_migration,
};
use reddb::storage::schema::Value;
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
        "reddb_operational_migration_{prefix}_{}_{}",
        std::process::id(),
        unique
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn open_operational(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::operational_directory(path))
        .unwrap_or_else(|err| panic!("operational runtime opens at {}: {err:?}", path.display()))
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
fn closed_checkpointed_embedded_rdb_migrates_to_operational_directory() {
    let source = PersistentDbPath::new("operational_migration_source");
    let rt = source.open_runtime();
    rt.execute_query("CREATE TABLE op_accounts (id INT, name TEXT, score INT)")
        .unwrap();
    rt.execute_query(
        "INSERT INTO op_accounts (id, name, score) VALUES \
         (1, 'Ada', 10), (2, 'Grace', 20), (3, 'Linus', 30)",
    )
    .unwrap();
    rt.execute_query("INSERT INTO op_settings KV (key, value) VALUES ('feature', 'enabled')")
        .unwrap();
    rt.checkpoint().unwrap();
    drop(rt);

    let operational_dir = temp_dir("success").join("operational");
    let manifest = migrate_embedded_to_operational(source.path(), &operational_dir)
        .expect("offline migration succeeds");
    assert!(manifest.one_way);
    assert!(manifest.offline_required);
    assert!(manifest
        .files
        .iter()
        .any(|file| file.id == "canonical-data"));

    let loaded = load_operational_migration_manifest(&operational_dir).unwrap();
    validate_operational_migration(&operational_dir, &loaded).expect("manifest validates");

    let source_rt = source.open_runtime();
    let operational_rt = open_operational(&operational_dir);
    assert_eq!(
        query_rows(
            &source_rt,
            "SELECT id, name, score FROM op_accounts ORDER BY id",
            &["id", "name", "score"]
        ),
        query_rows(
            &operational_rt,
            "SELECT id, name, score FROM op_accounts ORDER BY id",
            &["id", "name", "score"]
        )
    );
    assert_eq!(
        query_rows(
            &source_rt,
            "SELECT key, value FROM op_settings ORDER BY key",
            &["key", "value"]
        ),
        query_rows(
            &operational_rt,
            "SELECT key, value FROM op_settings ORDER BY key",
            &["key", "value"]
        )
    );
}

#[test]
fn migration_refuses_open_source_database() {
    let source = PersistentDbPath::new("operational_migration_open_source");
    let rt = source.open_runtime();
    rt.execute_query("CREATE TABLE op_open (id INT)").unwrap();
    rt.execute_query("INSERT INTO op_open (id) VALUES (1)")
        .unwrap();
    rt.checkpoint().unwrap();

    let operational_dir = temp_dir("open").join("operational");
    let err = migrate_embedded_to_operational(source.path(), &operational_dir).unwrap_err();
    assert!(
        err.to_string().contains("source .rdb is open"),
        "open-source refusal should be clear: {err}"
    );
    drop(rt);
}

#[test]
fn migration_refuses_corrupt_source_checksum() {
    let source = PersistentDbPath::new("operational_migration_corrupt_source");
    let rt = source.open_runtime();
    rt.execute_query("CREATE TABLE op_corrupt (id INT)")
        .unwrap();
    rt.execute_query("INSERT INTO op_corrupt (id) VALUES (1)")
        .unwrap();
    rt.checkpoint().unwrap();
    drop(rt);

    let mut file = OpenOptions::new().write(true).open(source.path()).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"!").unwrap();
    file.sync_all().unwrap();

    let operational_dir = temp_dir("corrupt").join("operational");
    let err = migrate_embedded_to_operational(source.path(), &operational_dir).unwrap_err();
    assert!(
        err.to_string().contains("checksum") || err.to_string().contains("header"),
        "corrupt source should fail validation: {err}"
    );
}
