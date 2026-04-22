//! Pin for savepoint-aware UPDATE reversal (Eixo 1, task 2).
//!
//! PG semantics: `ROLLBACK TO SAVEPOINT sp1` must restore the
//! pre-update value of any row mutated after `sp1`. Today reddb
//! UPDATE overwrites in place without writing a new version chain
//! entry tagged by sub-xid, so the pre-image is lost. This test
//! encodes the target behaviour and is gated with `#[ignore]`
//! until the per-connection update pre-image journal (or full
//! MVCC row-level UPDATE) lands.
//!
//! When the fix is in place, remove `#[ignore]` and the test
//! should start passing.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
#[ignore = "savepoint-aware UPDATE reversal not yet implemented — see docs/query/transactions.md"]
fn rollback_to_savepoint_restores_pre_update_value() {
    let rt = rt();
    set_current_connection_id(7701);

    exec(&rt, "CREATE TABLE sp_upd (id INT, label TEXT)");
    exec(&rt, "INSERT INTO sp_upd (id, label) VALUES (1, 'before')");

    exec(&rt, "BEGIN");
    exec(&rt, "SAVEPOINT sp1");
    exec(&rt, "UPDATE sp_upd SET label = 'after' WHERE id = 1");
    exec(&rt, "ROLLBACK TO SAVEPOINT sp1");
    exec(&rt, "COMMIT");

    let after = rt
        .execute_query("SELECT label FROM sp_upd WHERE id = 1")
        .expect("select after rollback");
    let rec = &after.result.records[0];
    let label = rec
        .values
        .get("label")
        .expect("label column present")
        .to_string();
    assert!(
        label.contains("before"),
        "label should be rolled back to 'before', got {label}"
    );

    clear_current_connection_id();
}
