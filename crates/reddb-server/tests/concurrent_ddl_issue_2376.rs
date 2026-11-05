//! Regression for #2376: concurrent DDL corrupted the system B-trees
//! (`red.control_events`, `red_index_registry`, `red_stats`) and left
//! every later CREATE/ALTER TABLE failing.
//!
//! Several workers replay the same idempotent migration at once, the
//! shape that broke the engine in the field: all CREATE TABLEs, then
//! ALTER TABLE ADD COLUMN, then the indexes. Afterwards new DDL must
//! still work, and the database must reopen with its rows readable and
//! DDL still working.

use std::path::Path;
use std::sync::{Arc, Barrier};

use reddb_server::storage::{DeployProfile, StoragePackaging, StorageProfileSelection};
use reddb_server::{RedDBOptions, RedDBRuntime};

const TABLE_COUNT: usize = 35;
const ALTER_COUNT: usize = 17;
const WORKER_COUNT: usize = 4;
const ROUND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// The server's `operational-directory` packaging: a paged store whose
/// collections (system ones included) live in pager B-trees. The default
/// embedded single-file packaging has no pager and never raced.
fn open_paged(path: &Path) -> RedDBRuntime {
    let profile = StorageProfileSelection {
        deploy_profile: DeployProfile::Embedded,
        packaging: StoragePackaging::OperationalDirectory,
        replica_count: 0,
        managed_backup: false,
        wal_retention: false,
    };
    let options = RedDBOptions::persistent(path)
        .with_storage_profile(profile)
        .expect("operational-directory profile");
    RedDBRuntime::with_options(options).expect("runtime boots")
}

fn migration() -> Vec<String> {
    let mut statements = Vec::new();
    for i in 0..TABLE_COUNT {
        statements.push(format!(
            "CREATE TABLE IF NOT EXISTS t{i} (id TEXT, a TEXT, b TEXT, c TEXT)"
        ));
    }
    for i in 0..ALTER_COUNT {
        statements.push(format!("ALTER TABLE t{i} ADD COLUMN d TEXT"));
    }
    for i in 0..TABLE_COUNT {
        statements.push(format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS t{i}_id ON t{i} (id)"
        ));
    }
    for i in 0..TABLE_COUNT {
        statements.push(format!("CREATE INDEX IF NOT EXISTS t{i}_a ON t{i} (a)"));
    }
    statements
}

/// Replays the migration from `WORKER_COUNT` threads at once and returns
/// every error that is not the expected idempotency outcome of a racing
/// `ALTER TABLE ... ADD COLUMN` (only one worker can add column `d`).
fn replay_concurrently(runtime: &RedDBRuntime) -> Vec<String> {
    let statements = Arc::new(migration());
    let barrier = Arc::new(Barrier::new(WORKER_COUNT));
    let workers: Vec<_> = (0..WORKER_COUNT)
        .map(|_| {
            let runtime = runtime.clone();
            let statements = Arc::clone(&statements);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let mut errors = Vec::new();
                for statement in statements.iter() {
                    if let Err(err) = runtime.execute_query(statement) {
                        let message = err.to_string();
                        let duplicate_column = statement.starts_with("ALTER TABLE")
                            && message.contains("already exists");
                        if !duplicate_column {
                            errors.push(format!("{statement}: {message}"));
                        }
                    }
                }
                errors
            })
        })
        .collect();
    workers
        .into_iter()
        .flat_map(|worker| worker.join().expect("worker thread panicked"))
        .collect()
}

fn assert_ddl_and_reads_work(runtime: &RedDBRuntime, new_column: &str, label: &str) {
    for statement in [
        "CREATE TABLE IF NOT EXISTS after_race (id TEXT)".to_string(),
        format!("ALTER TABLE t20 ADD COLUMN {new_column} TEXT"),
        "CREATE INDEX IF NOT EXISTS after_race_id ON after_race (id)".to_string(),
        "DROP TABLE IF EXISTS after_race".to_string(),
        "SELECT * FROM t0".to_string(),
    ] {
        runtime
            .execute_query(&statement)
            .unwrap_or_else(|err| panic!("{label}: `{statement}` failed: {err}"));
    }
}

fn insert_one_row_per_table(runtime: &RedDBRuntime, label: &str) {
    for i in 0..TABLE_COUNT {
        runtime
            .execute_query(&format!(
                "INSERT INTO t{i} (id, a, b, c) VALUES ('row{i}', 'a', 'b', 'c')"
            ))
            .unwrap_or_else(|err| panic!("{label}: insert into t{i} failed: {err}"));
    }
}

fn assert_rows_intact(runtime: &RedDBRuntime, label: &str) {
    for i in 0..TABLE_COUNT {
        let rows = runtime
            .execute_query(&format!("SELECT id FROM t{i} WHERE id = 'row{i}'"))
            .unwrap_or_else(|err| panic!("{label}: select from t{i} failed: {err}"));
        assert_eq!(rows.result.records.len(), 1, "{label}: t{i} row lookup");
    }
}

fn replay_round(round: u32) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("data.rdb");
    {
        let runtime = open_paged(&db_path);
        let errors = replay_concurrently(&runtime);
        assert!(
            errors.is_empty(),
            "round {round}: concurrent migration replay produced errors:\n{}",
            errors.join("\n")
        );
        assert_ddl_and_reads_work(&runtime, "e", &format!("round {round} after race"));
        insert_one_row_per_table(&runtime, &format!("round {round} after race"));
        runtime.checkpoint().expect("checkpoint after race");
    }

    // Recovery: the file written under concurrent DDL must reopen with
    // healthy system trees, its rows readable and DDL still working.
    let runtime = open_paged(&db_path);
    assert_rows_intact(&runtime, &format!("round {round} after reopen"));
    assert_ddl_and_reads_work(&runtime, "f", &format!("round {round} after reopen"));
}

#[test]
fn concurrent_migration_replay_keeps_system_trees_and_ddl_healthy() {
    // Two rounds make the regression near-certain to surface. A torn tree
    // can also send the rebuild into an endless leaf walk (the server
    // "wedge" in #2376), so each round runs under a watchdog.
    for round in 0..2 {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            replay_round(round);
            let _ = done_tx.send(());
        });
        match done_rx.recv_timeout(ROUND_DEADLINE) {
            Ok(()) => worker.join().expect("round thread"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // The round panicked: surface its assertion message.
                if let Err(panic) = worker.join() {
                    std::panic::resume_unwind(panic);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("round {round}: engine wedged for {ROUND_DEADLINE:?} under concurrent DDL")
            }
        }
    }
}

#[test]
fn rejected_migration_keeps_existing_rows_and_subsequent_ddl_healthy() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("data.rdb");
    {
        let runtime = open_paged(&path);
        runtime
            .execute_query("CREATE TABLE accounts (id TEXT, balance INT)")
            .expect("create");
        runtime
            .execute_query("INSERT INTO accounts (id, balance) VALUES ('kept', 42)")
            .expect("insert");
        for statement in [
            "ALTER TABLE missing_accounts ADD COLUMN balance TEXT",
            "CREATE TABLE accounts (id TEXT)",
        ] {
            assert!(
                runtime.execute_query(statement).is_err(),
                "must reject {statement}"
            );
            let result = runtime
                .execute_query("SELECT balance FROM accounts WHERE id = 'kept'")
                .expect("read after failed DDL");
            assert_eq!(result.result.records.len(), 1);
        }
        runtime
            .execute_query("ALTER TABLE accounts ADD COLUMN label TEXT")
            .expect("successful DDL after failures");
        runtime
            .execute_query("INSERT INTO accounts (id, balance) VALUES ('after', 7)")
            .expect("write after failures");
        runtime.checkpoint().expect("checkpoint");
    }
    let runtime = open_paged(&path);
    let result = runtime
        .execute_query("SELECT * FROM accounts")
        .expect("read after reopen");
    assert_eq!(result.result.records.len(), 2);
    runtime
        .execute_query("CREATE INDEX accounts_id ON accounts (id)")
        .expect("DDL after reopen");
}

#[test]
fn migration_lock_timeout_rejects_the_statement_without_creating_its_table() {
    use reddb_server::runtime::lock_manager::LockMode;
    use reddb_server::runtime::locking::{LockerGuard, Resource};

    let directory = tempfile::tempdir().expect("tempdir");
    let runtime = open_paged(&directory.path().join("data.rdb"));
    let mut blocker = LockerGuard::new(runtime.lock_manager());
    blocker
        .acquire(Resource::Collection("blocked".into()), LockMode::Exclusive)
        .expect("hold conflicting lock");
    let error = runtime
        .execute_query("CREATE TABLE blocked (id TEXT)")
        .expect_err("DDL must not run without its lock");
    assert!(error.to_string().contains("retry the statement"), "{error}");
    runtime
        .execute_query("CREATE TABLE unrelated (id TEXT)")
        .expect("unrelated DDL remains usable");
    drop(blocker);
    runtime
        .execute_query("CREATE TABLE blocked (id TEXT)")
        .expect("timed out DDL made no partial table");
    runtime
        .execute_query("INSERT INTO blocked (id) VALUES ('after')")
        .expect("write after timeout");
    runtime.checkpoint().expect("checkpoint after timeout");
}

#[test]
fn interleaved_key_ranges_survive_split_delete_and_reinsert() {
    use reddb_server::storage::engine::{BTree, Pager};
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("tree.rdb");
    let pager = Arc::new(Pager::open_default(&path).expect("open pager"));
    let tree = BTree::new(pager);
    let value = vec![7u8; 96];
    let mut next = [0u32; 4];
    let mut state = 1u64;
    for step in 0..8_000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let writer = usize::try_from((state >> 33) % 4).expect("four writers");
        let key_number = next[writer] * 4 + u32::try_from(writer).expect("four writers");
        next[writer] += 1;
        let key = key_number.to_be_bytes();
        tree.insert(&key, &value).expect("insert");
        assert_eq!(
            tree.get(&key).expect("get"),
            Some(value.clone()),
            "step {step}, key {key_number}"
        );
        if next[writer] % 4 == 1 {
            assert!(
                tree.delete(&key).expect("delete"),
                "step {step}, key {key_number}"
            );
            tree.upsert(&key, &value).expect("reinsert");
        }
    }
    for (writer, count) in next.into_iter().enumerate() {
        for index in 0..count {
            let key = (index * 4 + u32::try_from(writer).expect("four writers")).to_be_bytes();
            assert_eq!(tree.get(&key).expect("final read"), Some(value.clone()));
        }
    }
}

#[test]
fn cyclic_leaf_chain_returns_corruption_instead_of_growing_a_scan_forever() {
    use reddb_server::storage::engine::page::HEADER_SIZE;
    use reddb_server::storage::engine::{BTree, PageType, Pager};

    let directory = tempfile::tempdir().expect("tempdir");
    let pager = Arc::new(Pager::open_default(directory.path().join("tree.rdb")).expect("pager"));
    let tree = BTree::new(Arc::clone(&pager));
    for key in 0u32..2_000 {
        tree.insert(&key.to_be_bytes(), &[7; 96]).expect("insert");
    }
    let mut first = 0;
    let mut last = 0;
    for id in 1..pager.page_count().expect("page count") {
        let page = pager.read_page(id).expect("page");
        if page.page_type().expect("type") != PageType::BTreeLeaf {
            continue;
        }
        let links = &page.as_bytes()[HEADER_SIZE..HEADER_SIZE + 8];
        if links[..4] == [0; 4] {
            first = id;
        }
        if links[4..] == [0; 4] {
            last = id;
        }
    }
    assert_ne!(first, 0);
    assert_ne!(last, first);
    let mut tail = pager.read_page(last).expect("last leaf");
    tail.as_bytes_mut()[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&first.to_le_bytes());
    pager
        .write_page(last, tail)
        .expect("inject checksummed logical cycle");
    let mut cursor = tree.cursor_first().expect("cursor");
    for _ in 0..=2_000 {
        match cursor.next() {
            Err(error) => {
                assert!(error.to_string().contains("leaf"), "{error}");
                return;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("cycle must be reported, not silently truncated"),
        }
    }
    panic!("scan followed a cyclic chain instead of returning a storage error");
}
