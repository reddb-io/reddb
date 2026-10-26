#[path = "support/codec_corpus.rs"]
#[allow(dead_code)]
mod codec_corpus;
#[path = "support/pager.rs"]
#[allow(dead_code)]
mod pager;

use reddb_engine::query::{local_database::LocalDatabase, local_read::LocalReadDatabase};
use reddb_types::Value;

type Database = LocalDatabase<pager::MemoryFile>;
fn create() -> Database {
    Database::create(pager::MemoryFile::default(), "sql.rdb", 1000).expect("create local database")
}
fn run(database: &mut Database, sql: &str) {
    database.query(sql, 2000).expect(sql);
}
fn rows(database: &mut Database, sql: &str) -> Vec<Vec<Value>> {
    database.query(sql, 2000).expect(sql).rows
}
fn seed() -> Database {
    let mut database = create();
    run(&mut database, "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT NOT NULL, total INTEGER DEFAULT = 10)");
    run(
        &mut database,
        "INSERT INTO people (id, name) VALUES (1, 'Ana'), (2, 'Bia')",
    );
    database
}

#[test]
fn sql_crud_recovers_canonical_artifact() {
    let mut database = seed();
    assert_eq!(
        rows(&mut database, "SELECT rid FROM people ORDER BY id"),
        [
            vec![Value::UnsignedInteger(1024)],
            vec![Value::UnsignedInteger(1025)]
        ]
    );
    run(
        &mut database,
        "UPDATE people SET name = UPPER(name), total = total + 5 WHERE id = 1",
    );
    run(&mut database, "DELETE FROM people WHERE id = 2");
    let expected = vec![vec![
        Value::Integer(1),
        Value::text("ANA"),
        Value::Integer(15),
    ]];
    assert_eq!(
        rows(&mut database, "SELECT id, name, total FROM people"),
        expected
    );
    let mut database =
        Database::open(database.into_inner(), "sql.rdb").expect("reopen snapshot and WAL");
    assert_eq!(
        rows(&mut database, "SELECT id, name, total FROM people"),
        expected
    );
    let read = LocalReadDatabase::open(database.into_inner(), "sql.rdb")
        .expect("canonical reader interoperability");
    assert_eq!(
        read.query("SELECT id, name, total FROM people")
            .expect("shared SELECT")
            .rows,
        expected
    );
}

#[test]
fn commit_rollback_and_close_have_real_transaction_boundaries() {
    let mut database = seed();
    run(&mut database, "BEGIN");
    run(&mut database, "UPDATE people SET total = 99 WHERE id = 1");
    assert_eq!(
        rows(&mut database, "SELECT total FROM people WHERE id = 1"),
        [vec![Value::Integer(99)]]
    );
    run(&mut database, "ROLLBACK");
    assert_eq!(
        rows(&mut database, "SELECT total FROM people WHERE id = 1"),
        [vec![Value::Integer(10)]]
    );
    run(&mut database, "BEGIN");
    run(&mut database, "UPDATE people SET total = 22 WHERE id = 2");
    run(&mut database, "DELETE FROM people WHERE id = 1");
    run(&mut database, "COMMIT");
    run(&mut database, "BEGIN");
    run(&mut database, "UPDATE people SET total = 999");
    let mut database =
        Database::open(database.into_inner(), "sql.rdb").expect("close discards uncommitted state");
    assert_eq!(
        rows(&mut database, "SELECT id, total FROM people"),
        [vec![Value::Integer(2), Value::Integer(22)]]
    );
}

#[test]
fn schema_and_rows_commit_or_rollback_together() {
    let mut database = seed();
    run(&mut database, "BEGIN");
    run(&mut database, "CREATE TABLE extra (id INTEGER PRIMARY KEY)");
    run(&mut database, "INSERT INTO extra VALUES (7)");
    run(&mut database, "ROLLBACK");
    assert!(database.query("SELECT * FROM extra", 2000).is_err());
    run(&mut database, "BEGIN");
    run(&mut database, "CREATE TABLE extra (id INTEGER PRIMARY KEY)");
    run(&mut database, "INSERT INTO extra VALUES (8)");
    run(&mut database, "UPDATE people SET total = 55");
    run(&mut database, "COMMIT");
    let mut database = Database::open(database.into_inner(), "sql.rdb")
        .expect("catalog + rows recovered together");
    assert_eq!(
        rows(&mut database, "SELECT id FROM extra"),
        [vec![Value::Integer(8)]]
    );
    assert_eq!(
        rows(&mut database, "SELECT total FROM people ORDER BY id"),
        [vec![Value::Integer(55)], vec![Value::Integer(55)]]
    );
}

#[test]
fn rejected_batches_are_atomic_and_abort_explicit_transactions() {
    let mut database = seed();
    for sql in [
        "INSERT INTO people (id, name) VALUES (3, 'ok'), (1, 'duplicate')",
        "INSERT INTO people (id, name) VALUES (3, 'ok'), (4, NULL)",
        "UPDATE people SET id = 1",
        "UPDATE people SET total = 1 / (id - 2)",
        "UPDATE people SET missing = 1",
    ] {
        assert!(database.query(sql, 2000).is_err(), "{sql}");
    }
    assert_eq!(
        rows(&mut database, "SELECT id, total FROM people ORDER BY id"),
        [
            vec![Value::Integer(1), Value::Integer(10)],
            vec![Value::Integer(2), Value::Integer(10)]
        ]
    );
    run(&mut database, "BEGIN");
    run(
        &mut database,
        "INSERT INTO people (id, name) VALUES (3, 'staged')",
    );
    assert!(database
        .query(
            "INSERT INTO people (id, name) VALUES (1, 'duplicate')",
            2000
        )
        .is_err());
    assert!(database.query("COMMIT", 2000).is_err());
    assert!(database.query("SELECT * FROM people", 2000).is_err());
    run(&mut database, "ROLLBACK");
    assert_eq!(
        rows(&mut database, "SELECT id FROM people ORDER BY id"),
        [vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}

#[test]
fn native_constraints_defaults_and_generated_fields_are_shared() {
    let mut database = create();
    run(&mut database, "CREATE TABLE items (id INTEGER PRIMARY KEY, qty INTEGER DEFAULT = 3 CHECK (qty > 0), doubled INTEGER GENERATED ALWAYS AS (qty * 2) STORED)");
    run(&mut database, "INSERT INTO items (id) VALUES (1)");
    assert_eq!(
        rows(&mut database, "SELECT qty, doubled FROM items"),
        [vec![Value::Integer(3), Value::Integer(6)]]
    );
    assert!(database
        .query("INSERT INTO items (id, qty) VALUES (2, -1)", 2000)
        .is_err());
    assert!(database
        .query("INSERT INTO items (id, doubled) VALUES (2, 7)", 2000)
        .is_err());
    run(&mut database, "UPDATE items SET qty = 5 WHERE id = 1");
    assert_eq!(
        rows(&mut database, "SELECT qty, doubled FROM items"),
        [vec![Value::Integer(5), Value::Integer(10)]]
    );
}

fn stage_schema_transaction(database: &mut Database) {
    run(database, "BEGIN");
    run(database, "CREATE TABLE extra (id INTEGER)");
    run(database, "INSERT INTO extra VALUES (8)");
    run(database, "UPDATE people SET total = 55");
}

#[test]
fn catalog_transaction_survives_every_interrupted_publication_barrier() {
    let baseline = seed().into_inner().durable.into_inner();
    let file = pager::MemoryFile::from_bytes(baseline.clone());
    let operations = file.operations.clone();
    let mut successful = Database::open(file, "sql.rdb").expect("baseline");
    stage_schema_transaction(&mut successful);
    operations.borrow_mut().clear();
    run(&mut successful, "COMMIT");
    let count = operations.borrow().len();
    assert!(count > 0);
    for fail_at in 1..=count {
        let mut file = pager::MemoryFile::from_bytes(baseline.clone());
        file.fail_at = Some(fail_at);
        let mut database = Database::open(file, "sql.rdb").expect("open before failure");
        stage_schema_transaction(&mut database);
        assert!(
            database.query("COMMIT", 2000).is_err(),
            "failure at {fail_at}"
        );
        assert!(
            database.query("SELECT * FROM people", 2000).is_err(),
            "failed I/O must fence the handle"
        );
        let file = database.into_inner();
        for bytes in [file.bytes.into_inner(), file.durable.into_inner()] {
            let mut recovered = Database::open(pager::MemoryFile::from_bytes(bytes), "sql.rdb")
                .expect("old or new root must recover");
            let totals = rows(&mut recovered, "SELECT total FROM people ORDER BY id");
            let extra = recovered.query("SELECT id FROM extra", 2000);
            match extra {
                Ok(extra) => {
                    assert_eq!(extra.rows, [vec![Value::Integer(8)]]);
                    assert_eq!(totals, [vec![Value::Integer(55)], vec![Value::Integer(55)]]);
                }
                Err(_) => assert_eq!(totals, [vec![Value::Integer(10)], vec![Value::Integer(10)]]),
            }
        }
    }
}

#[test]
fn wal_transaction_never_recovers_a_partial_commit() {
    let baseline = seed().into_inner().durable.into_inner();
    let stage = |database: &mut Database| {
        run(database, "BEGIN");
        run(database, "UPDATE people SET total = 42 WHERE id = 1");
        run(database, "DELETE FROM people WHERE id = 2");
        run(
            database,
            "INSERT INTO people (id, name, total) VALUES (3, 'Caio', 77)",
        );
    };
    let file = pager::MemoryFile::from_bytes(baseline.clone());
    let operations = file.operations.clone();
    let mut database = Database::open(file, "sql.rdb").expect("baseline");
    stage(&mut database);
    operations.borrow_mut().clear();
    run(&mut database, "COMMIT");
    let count = operations.borrow().len();
    assert!(count > 0);
    for fail_at in 1..=count {
        let mut file = pager::MemoryFile::from_bytes(baseline.clone());
        file.fail_at = Some(fail_at);
        let mut database =
            Database::open(file, "sql.rdb").expect("baseline before interrupted WAL");
        stage(&mut database);
        assert!(database.query("COMMIT", 2000).is_err());
        assert!(database.query("SELECT * FROM people", 2000).is_err());
        let file = database.into_inner();
        for bytes in [file.bytes.into_inner(), file.durable.into_inner()] {
            let mut recovered = Database::open(pager::MemoryFile::from_bytes(bytes), "sql.rdb")
                .expect("recover interrupted WAL");
            let rows = rows(&mut recovered, "SELECT id, total FROM people ORDER BY id");
            let old = vec![
                vec![Value::Integer(1), Value::Integer(10)],
                vec![Value::Integer(2), Value::Integer(10)],
            ];
            let new = vec![
                vec![Value::Integer(1), Value::Integer(42)],
                vec![Value::Integer(3), Value::Integer(77)],
            ];
            assert!(
                rows == old || rows == new,
                "partial transaction after failure {fail_at}: {rows:?}"
            );
        }
    }
}

#[test]
fn unsupported_writes_fail_before_creating_catalog_or_rows() {
    let mut database = seed();
    for sql in [
        "CREATE TABLE bad (rid INTEGER)",
        "CREATE TABLE bad (value SECRET)",
        "CREATE TABLE red.bad (id INTEGER)",
        "INSERT INTO people (id, name) VALUES (3, 'x') RETURNING id",
        "INSERT INTO people (id, name) VALUES ($1, 'x')",
        "UPDATE people SET total = ABS()",
        "UPDATE people SET total = 12 LIMIT 1",
        "DELETE FROM people RETURNING id",
    ] {
        assert!(database.query(sql, 2000).is_err(), "{sql}");
    }
    assert!(database.query("SELECT * FROM bad", 2000).is_err());
    assert_eq!(
        rows(&mut database, "SELECT id FROM people ORDER BY id"),
        [vec![Value::Integer(1)], vec![Value::Integer(2)]]
    );
}
