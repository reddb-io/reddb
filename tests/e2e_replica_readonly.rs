//! W1 — public-mutation gate enforcement.
//!
//! Asserts that a runtime booted with `ReplicationRole::Replica { .. }`
//! rejects every public mutation surface, while still allowing the
//! privileged internal logical-WAL apply path used by replica catch-up
//! and PITR replay.
//!
//! Surfaces covered here:
//!   * SQL DDL via `execute_query("CREATE TABLE ...")`
//!   * SQL DML via `execute_query("INSERT ... / UPDATE ... / DELETE ...")`
//!   * Entity port `create_row` (path used by HTTP / native wire / gRPC
//!     `EntityUseCases`)
//!   * Entity port `delete_entity`
//!
//! gRPC- and PostgreSQL-wire-specific surfaces have their own integration
//! suites; their dispatch funnels into either `execute_query` or the
//! entity port below, so the shared chokepoints here demonstrate the
//! gate fires at the right layer.

use reddb::application::{CreateRowInput, DeleteEntityInput, EntityUseCases};
use reddb::replication::ReplicationConfig;
use reddb::storage::EntityId;
use reddb::{RedDBError, RedDBOptions, RedDBRuntime};
use std::path::PathBuf;

fn unique_data_dir(prefix: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("reddb-{prefix}-{pid}-{nanos}"));
    p
}

fn assert_read_only_err<T: std::fmt::Debug>(
    res: Result<T, RedDBError>,
    surface: &str,
    expected_token: &str,
) {
    match res {
        Ok(value) => panic!("{surface} accepted a mutation on a replica — got {value:?}"),
        Err(RedDBError::ReadOnly(msg)) => assert!(
            msg.contains(expected_token),
            "{surface} returned ReadOnly but message lacked '{expected_token}': {msg}"
        ),
        Err(other) => panic!("{surface} returned {other:?}, expected ReadOnly"),
    }
}

#[test]
fn replica_rejects_sql_ddl_and_dml_on_every_surface() {
    let primary_path = unique_data_dir("replica-primary");

    // 1. Boot a standalone primary, create a table, insert one row.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&primary_path))
            .expect("primary open");
        rt.execute_query("CREATE TABLE accounts (id INT, name TEXT)")
            .expect("primary CREATE TABLE");
        rt.execute_query("INSERT INTO accounts (id, name) VALUES (1, 'alice')")
            .expect("primary INSERT");
    }

    // 2. Re-open the same data directory under a replica config. Public
    //    mutation surfaces must reject; reads must still work.
    let opts = RedDBOptions::persistent(&primary_path)
        .with_replication(ReplicationConfig::replica("http://primary:50051"));
    let rt = RedDBRuntime::with_options(opts).expect("replica open");

    let read = rt
        .execute_query("SELECT id, name FROM accounts")
        .expect("replica reads still work");
    assert!(
        !read.result.records.is_empty(),
        "replica must serve reads from the existing table"
    );

    assert_read_only_err(
        rt.execute_query("CREATE TABLE other (id INT)"),
        "SQL DDL CREATE TABLE",
        "replica",
    );
    assert_read_only_err(
        rt.execute_query("DROP TABLE accounts"),
        "SQL DDL DROP TABLE",
        "replica",
    );
    assert_read_only_err(
        rt.execute_query("CREATE INDEX idx_n ON accounts (name) USING HASH"),
        "SQL DDL CREATE INDEX",
        "replica",
    );
    assert_read_only_err(
        rt.execute_query("INSERT INTO accounts (id, name) VALUES (2, 'bob')"),
        "SQL DML INSERT",
        "replica",
    );
    assert_read_only_err(
        rt.execute_query("UPDATE accounts SET name = 'mallory' WHERE id = 1"),
        "SQL DML UPDATE",
        "replica",
    );
    assert_read_only_err(
        rt.execute_query("DELETE FROM accounts WHERE id = 1"),
        "SQL DML DELETE",
        "replica",
    );

    // 3. Entity port used by HTTP / native wire / gRPC EntityUseCases.
    let entity = EntityUseCases::new(&rt);
    assert_read_only_err(
        entity.create_row(CreateRowInput {
            collection: "accounts".into(),
            fields: vec![("id".into(), reddb::storage::schema::Value::Integer(99))],
            metadata: Vec::new(),
            node_links: Vec::new(),
            vector_links: Vec::new(),
        }),
        "EntityPort::create_row",
        "replica",
    );

    assert_read_only_err(
        entity.delete(DeleteEntityInput {
            collection: "accounts".into(),
            id: EntityId::new(1),
        }),
        "EntityUseCases::delete",
        "replica",
    );
}

#[test]
fn explicit_read_only_flag_rejects_writes_on_standalone() {
    let path = unique_data_dir("readonly-flag");

    // Seed a table on a writable instance so the read-only re-open has
    // something to read.
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("seed open");
        rt.execute_query("CREATE TABLE flag_test (id INT)")
            .expect("seed CREATE");
        rt.execute_query("INSERT INTO flag_test (id) VALUES (1)")
            .expect("seed INSERT");
    }

    let opts = RedDBOptions::persistent(&path).with_read_only(true);
    let rt = RedDBRuntime::with_options(opts).expect("read-only open");

    assert_read_only_err(
        rt.execute_query("INSERT INTO flag_test (id) VALUES (2)"),
        "SQL INSERT under read_only flag",
        "read_only",
    );
    assert_read_only_err(
        rt.execute_query("CREATE TABLE another (id INT)"),
        "SQL CREATE TABLE under read_only flag",
        "read_only",
    );
}

#[test]
fn standalone_primary_default_is_writable() {
    let path = unique_data_dir("primary-writable");

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("primary open");
    rt.execute_query("CREATE TABLE writable (id INT)")
        .expect("CREATE TABLE on standalone");
    rt.execute_query("INSERT INTO writable (id) VALUES (1)")
        .expect("INSERT on standalone");

    let writable_gate = !rt.write_gate().is_read_only();
    assert!(
        writable_gate,
        "standalone primary must report a writable gate"
    );
}

#[test]
fn replica_internal_apply_path_remains_privileged() {
    // The replica gate rejects every PUBLIC mutation, but the privileged
    // internal apply path used by `LogicalChangeApplier::apply_record`
    // talks to the store directly and never calls `check_write`. This
    // test asserts that bypass by using the same low-level surface the
    // replica uses to ingest records from the primary, and verifying
    // it succeeds even after the gate has rejected client writes.
    use reddb::storage::{EntityData, EntityKind, RowData, UnifiedEntity};

    let path = unique_data_dir("replica-privileged");
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("primary open");
        rt.execute_query("CREATE TABLE shipped (id INT, payload TEXT)")
            .expect("primary CREATE TABLE");
    }

    let opts = RedDBOptions::persistent(&path)
        .with_replication(ReplicationConfig::replica("http://primary:50051"));
    let rt = RedDBRuntime::with_options(opts).expect("replica open");

    // 1. Public surface is rejected.
    assert_read_only_err(
        rt.execute_query("INSERT INTO shipped (id, payload) VALUES (1, 'x')"),
        "SQL INSERT on replica",
        "replica",
    );

    // 2. Internal apply path: bypasses the gate by reaching the store
    //    directly. This mirrors what `LogicalChangeApplier::apply_record`
    //    does when ingesting WAL records from the primary.
    use std::sync::Arc;

    let store = rt.db().store();
    let table_arc: Arc<str> = Arc::from("shipped");
    let mut row = RowData::new(Vec::new());
    row.named = Some(
        vec![
            ("id".to_string(), reddb::storage::schema::Value::Integer(7)),
            (
                "payload".to_string(),
                reddb::storage::schema::Value::text("from-primary"),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let entity = UnifiedEntity::new(
        store.next_entity_id(),
        EntityKind::TableRow {
            table: table_arc,
            row_id: 0,
        },
        EntityData::Row(row),
    );
    let _ = store
        .insert_auto("shipped", entity)
        .expect("privileged store insert must succeed on replica");

    // 3. Reads still see the privileged-apply row. This proves the gate
    //    only blocks the public surface, not internal replica catch-up.
    let read = rt
        .execute_query("SELECT id, payload FROM shipped WHERE id = 7")
        .expect("replica read");
    assert_eq!(
        read.result.records.len(),
        1,
        "privileged-applied row must be visible to readers"
    );
}
