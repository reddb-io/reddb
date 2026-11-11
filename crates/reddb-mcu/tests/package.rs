use reddb_mcu::{
    file::{OpenMode, StdVfs, Vfs},
    types::Value,
    Database, PICO_2,
};

#[test]
fn package_uses_canonical_sql_and_rdb() {
    let directory = tempfile::tempdir().expect("temporary package database");
    let path = directory.path().join("mcu.rdb");
    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("create file");
    let mut database = Database::create(file).expect("create paged database");
    assert_eq!(database.cache_pages(), PICO_2.cache_pages);
    database
        .query(
            "CREATE TABLE samples (observed BIGINT NOT NULL, value DOUBLE NOT NULL)",
            1_000,
        )
        .expect("canonical DDL");
    database
        .query("INSERT INTO samples VALUES (1, 20.5), (2, 21.0)", 2_000)
        .expect("canonical INSERT");
    drop(database.into_inner());

    let file = StdVfs
        .open(&path, OpenMode::create_keep())
        .expect("reopen file");
    let database = Database::open(file).expect("recover canonical .rdb");
    let mut rows = Vec::new();
    let summary = database
        .query_stream("SELECT observed, value FROM samples", &[], &mut |row| {
            rows.push(row.to_vec());
            Ok(true)
        })
        .expect("canonical streaming SELECT");
    assert_eq!(summary.rows, 2);
    assert_eq!(
        rows,
        [
            vec![Value::BigInt(1), Value::Float(20.5)],
            vec![Value::BigInt(2), Value::Float(21.0)],
        ]
    );
}
