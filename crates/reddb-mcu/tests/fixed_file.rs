use reddb_mcu::{
    file::{OpenMode, StdVfs, Vfs},
    fixed_file::{FixedCapacityFile, FixedFileState},
    types::Value,
    Database, VfsFile,
};
use std::{fs::OpenOptions, io::Write};

const CAPACITY: u64 = 8 * 1024 * 1024;

#[test]
fn preallocated_file_recovers_and_opens_through_the_canonical_vfs() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("data.rdb");
    let mut provisioned = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("create fixed-capacity file");
    provisioned
        .set_len(CAPACITY)
        .expect("preallocate host file");
    provisioned
        .write_all(&[0; 4096])
        .expect("initialize first block");

    let file = FixedCapacityFile::open(provisioned, CAPACITY, FixedFileState::Empty)
        .expect("open empty fixed-capacity file");
    assert_eq!(file.file_len().expect("empty logical length"), 0);
    file.reserve(0, CAPACITY).expect("reserve allocated file");
    assert!(file.reserve(CAPACITY, 1).is_err());

    let mut database = Database::create(file).expect("create database");
    database
        .query(
            "CREATE TABLE measurements (recorded_at BIGINT NOT NULL, temperature DOUBLE NOT NULL)",
            1,
        )
        .expect("create table");
    database
        .query(
            "INSERT INTO measurements VALUES (0, 18.5), (1, 19.0), (2, 19.5)",
            2,
        )
        .expect("insert measurements");
    drop(database);

    let reopened = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("reopen preallocated database");
    let file = FixedCapacityFile::open(reopened, CAPACITY, FixedFileState::Existing)
        .expect("open existing fixed-capacity file");
    let database = Database::open(file).expect("recover fixed-capacity database");
    let mut rows = Vec::new();
    database
        .query_stream(
            "SELECT recorded_at, temperature FROM measurements",
            &[],
            &mut |row| {
                rows.push(row.to_vec());
                Ok(true)
            },
        )
        .expect("stream recovered measurements");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2][0], Value::BigInt(2));
    drop(database);

    let canonical = StdVfs
        .open(&path, OpenMode::read_only())
        .expect("open through canonical VFS");
    let database = Database::open(canonical).expect("open canonical preallocated rdb");
    let result = database
        .query_stream("SELECT recorded_at FROM measurements", &[], &mut |_| {
            Ok(true)
        })
        .expect("query through canonical VFS");
    assert_eq!(result.rows, 3);
}
