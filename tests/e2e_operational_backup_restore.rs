//! Operational checkpoint backup + target-LSN restore tracer.

use reddb::json::Value as JsonValue;
use reddb::replication::primary::LogicalWalSpool;
use reddb::storage::wal::{create_operational_backup, restore_operational_backup_to_lsn};
use reddb::{RedDBOptions, RedDBRuntime, ReplicationConfig};
use std::path::{Path, PathBuf};

fn temp_dir(prefix: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("reddb-operational-backup-{prefix}-{pid}-{nanos}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn open_primary(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(
        RedDBOptions::persistent(path).with_replication(ReplicationConfig::primary()),
    )
    .expect("open primary")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn rows(path: &Path) -> usize {
    let db = reddb::storage::RedDB::open(path).expect("open restored db");
    db.store()
        .get_collection("accounts")
        .map(|manager| manager.query_all(|_| true).len())
        .unwrap_or(0)
}

fn sha(path: &Path) -> String {
    reddb::storage::wal::sha256_file_hex(path).expect("hash file")
}

fn rewrite_manifest_for_retained_wal(backup_dir: &Path, current_lsn: u64) {
    let manifest_path = backup_dir.join("MANIFEST.json");
    let text = std::fs::read_to_string(&manifest_path).unwrap();
    let mut value: JsonValue = reddb::json::from_str(&text).unwrap();
    let wal_path = backup_dir.join("logical.wal");
    let wal_size = std::fs::metadata(&wal_path).unwrap().len();
    let wal_sha = sha(&wal_path);

    let obj = match &mut value {
        JsonValue::Object(obj) => obj,
        _ => panic!("manifest must be object"),
    };
    obj.insert(
        "current_lsn".to_string(),
        JsonValue::Number(current_lsn as f64),
    );
    let files = obj
        .get_mut("files")
        .and_then(|v| match v {
            JsonValue::Array(files) => Some(files),
            _ => None,
        })
        .expect("manifest files");
    let wal = files
        .iter_mut()
        .find(|file| file.get("role").and_then(JsonValue::as_str) == Some("logical_wal"))
        .expect("logical wal file entry");
    let wal_obj = match wal {
        JsonValue::Object(obj) => obj,
        _ => panic!("wal entry object"),
    };
    wal_obj.insert("size_bytes".to_string(), JsonValue::Number(wal_size as f64));
    wal_obj.insert("sha256".to_string(), JsonValue::String(wal_sha));

    std::fs::write(&manifest_path, value.to_string_pretty()).unwrap();
    std::fs::write(
        backup_dir.join("MANIFEST.sha256"),
        format!("{}\n", sha(&manifest_path)),
    )
    .unwrap();
}

#[test]
fn checkpoint_backup_records_boundary_manifest_and_file_checksums() {
    let work = temp_dir("checkpoint");
    let db_path = work.join("primary.rdb");
    let backup_dir = work.join("backup");
    let rt = open_primary(&db_path);

    exec(&rt, "CREATE TABLE accounts (id INTEGER, name TEXT)");
    exec(&rt, "INSERT INTO accounts (id, name) VALUES (1, 'alice')");

    let backup = create_operational_backup(&rt, &backup_dir).expect("backup");

    assert!(backup.checkpoint_lsn > 0);
    assert_eq!(backup.wal_start_lsn, backup.checkpoint_lsn);
    assert!(backup.manifest_path.exists());
    assert_eq!(backup.manifest_sha256, sha(&backup.manifest_path));
    assert_eq!(
        std::fs::read_to_string(backup_dir.join("MANIFEST.sha256"))
            .unwrap()
            .trim(),
        backup.manifest_sha256
    );

    let manifest: JsonValue =
        reddb::json::from_str(&std::fs::read_to_string(&backup.manifest_path).unwrap()).unwrap();
    assert_eq!(
        manifest.get("checkpoint_lsn").and_then(JsonValue::as_u64),
        Some(backup.checkpoint_lsn)
    );
    let files = manifest
        .get("files")
        .and_then(JsonValue::as_array)
        .expect("manifest files");
    assert!(files.iter().any(|file| {
        file.get("role").and_then(JsonValue::as_str) == Some("data")
            && file.get("sha256").and_then(JsonValue::as_str).is_some()
    }));
    assert!(backup_dir.join("data.rdb").exists());
    assert!(backup_dir.join("logical.wal").exists());

    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn restore_replays_retained_wal_to_requested_target_lsn() {
    let work = temp_dir("target-lsn");
    let db_path = work.join("primary.rdb");
    let backup_dir = work.join("backup");
    let restore_one = work.join("restore-one.rdb");
    let restore_two = work.join("restore-two.rdb");
    let rt = open_primary(&db_path);

    exec(&rt, "CREATE TABLE accounts (id INTEGER, name TEXT)");
    exec(&rt, "INSERT INTO accounts (id, name) VALUES (1, 'base')");
    let backup = create_operational_backup(&rt, &backup_dir).expect("backup");

    exec(
        &rt,
        "INSERT INTO accounts (id, name) VALUES (2, 'tail-one')",
    );
    exec(
        &rt,
        "INSERT INTO accounts (id, name) VALUES (3, 'tail-two')",
    );
    rt.db().flush().expect("flush primary");

    let source_spool = LogicalWalSpool::open(&db_path).expect("open source spool");
    let retained = source_spool
        .read_since(backup.checkpoint_lsn, usize::MAX)
        .expect("read retained records");
    assert_eq!(retained.len(), 2, "two post-backup records retained");
    let first_tail_lsn = retained[0].0;
    let second_tail_lsn = retained[1].0;
    let backup_spool = LogicalWalSpool::open(&backup_dir.join("logical.wal")).unwrap();
    for (lsn, bytes) in retained {
        backup_spool.append(lsn, &bytes).unwrap();
    }
    rewrite_manifest_for_retained_wal(&backup_dir, second_tail_lsn);

    let r1 = restore_operational_backup_to_lsn(&backup_dir, &restore_one, first_tail_lsn)
        .expect("restore to first tail");
    assert_eq!(r1.recovered_to_lsn, first_tail_lsn);
    assert_eq!(r1.records_applied, 1);
    assert_eq!(rows(&restore_one), 2);

    let r2 = restore_operational_backup_to_lsn(&backup_dir, &restore_two, second_tail_lsn)
        .expect("restore to second tail");
    assert_eq!(r2.recovered_to_lsn, second_tail_lsn);
    assert_eq!(r2.records_applied, 2);
    assert_eq!(rows(&restore_two), 3);

    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn restore_fails_closed_on_manifest_checksum_mismatch() {
    let work = temp_dir("bad-manifest");
    let db_path = work.join("primary.rdb");
    let backup_dir = work.join("backup");
    let restore_path = work.join("restore.rdb");
    let rt = open_primary(&db_path);

    exec(&rt, "CREATE TABLE accounts (id INTEGER, name TEXT)");
    exec(&rt, "INSERT INTO accounts (id, name) VALUES (1, 'base')");
    create_operational_backup(&rt, &backup_dir).expect("backup");

    std::fs::write(backup_dir.join("MANIFEST.json"), b"{\"corrupt\":true}").unwrap();

    let err = restore_operational_backup_to_lsn(&backup_dir, &restore_path, u64::MAX)
        .expect_err("manifest checksum mismatch must fail closed");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("manifest") && msg.contains("checksum"),
        "expected manifest checksum error, got {msg}"
    );
    assert!(!restore_path.exists(), "restore must not open destination");

    let _ = std::fs::remove_dir_all(work);
}

#[test]
fn restore_fails_closed_on_file_checksum_mismatch() {
    let work = temp_dir("bad-file");
    let db_path = work.join("primary.rdb");
    let backup_dir = work.join("backup");
    let restore_path = work.join("restore.rdb");
    let rt = open_primary(&db_path);

    exec(&rt, "CREATE TABLE accounts (id INTEGER, name TEXT)");
    exec(&rt, "INSERT INTO accounts (id, name) VALUES (1, 'base')");
    create_operational_backup(&rt, &backup_dir).expect("backup");

    let data_path = backup_dir.join("data.rdb");
    let mut bytes = std::fs::read(&data_path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x55;
    std::fs::write(&data_path, bytes).unwrap();

    let err = restore_operational_backup_to_lsn(&backup_dir, &restore_path, u64::MAX)
        .expect_err("file checksum mismatch must fail closed");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("file") && msg.contains("checksum"),
        "expected file checksum error, got {msg}"
    );
    assert!(!restore_path.exists(), "restore must not open destination");

    let _ = std::fs::remove_dir_all(work);
}
