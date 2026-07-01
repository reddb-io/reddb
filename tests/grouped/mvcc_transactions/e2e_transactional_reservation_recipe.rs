//! Canonical transactional reservation recipe.
//!
//! The recipe is intentionally expressed through public SQL. It combines
//! `UPDATE ... CLAIM`, an application-owned idempotency table, and queue work
//! inside one explicit transaction boundary.

#[path = "../../support/mod.rs"]
mod support;

use std::path::Path;

use reddb::api::DurabilityMode;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::storage::wal::{WalReader, WalRecord};
use reddb::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

const TRANSACTIONS_DOC: &str = include_str!("../../../docs/query/transactions.md");

#[derive(Debug, PartialEq)]
enum ReservationAttempt {
    Created {
        reservation_id: String,
        units_claimed: i64,
    },
    Existing {
        reservation_id: String,
        units_claimed: i64,
    },
}

fn in_memory_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn persistent_runtime(db: &support::TempDbFile) -> RedDBRuntime {
    RedDBRuntime::with_options(
        RedDBOptions::persistent(db.path())
            .with_durability_mode(DurabilityMode::WalDurableGrouped)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
            .expect("primary-replica operational profile"),
    )
    .expect("persistent runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn setup_reservation_schema(rt: &RedDBRuntime) {
    exec(
        rt,
        "CREATE TABLE inventory_units \
         (sku TEXT, unit_id INT PRIMARY KEY, status TEXT, reservation_key TEXT)",
    );
    // ADR 0063: the reservation claim orders by `unit_id`, which must be
    // served through a compatible index.
    exec(
        rt,
        "CREATE INDEX idx_inventory_units_unit_id ON inventory_units (unit_id)",
    );
    exec(
        rt,
        "CREATE TABLE reservation_idempotency \
         (idempotency_key TEXT PRIMARY KEY, reservation_id TEXT, units_claimed INT)",
    );
    exec(rt, "CREATE QUEUE reservation_work");
    exec(
        rt,
        "INSERT INTO inventory_units (sku, unit_id, status, reservation_key) VALUES \
         ('sku-1', 1, 'available', ''), \
         ('sku-1', 2, 'available', ''), \
         ('sku-1', 3, 'available', '')",
    );
}

fn text(record: &reddb::storage::query::UnifiedRecord, column: &str) -> String {
    match record.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text for {column}, got {other:?}"),
    }
}

fn int(record: &reddb::storage::query::UnifiedRecord, column: &str) -> i64 {
    match record.get(column) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected integer for {column}, got {other:?}"),
    }
}

fn row_count(rt: &RedDBRuntime, sql: &str) -> usize {
    exec(rt, sql).result.records.len()
}

fn existing_reservation(rt: &RedDBRuntime, key: &str) -> Option<(String, i64)> {
    let existing = exec(
        rt,
        &format!(
            "SELECT reservation_id, units_claimed FROM reservation_idempotency \
             WHERE idempotency_key = '{key}'"
        ),
    );
    existing
        .result
        .records
        .first()
        .map(|record| (text(record, "reservation_id"), int(record, "units_claimed")))
}

fn reserve_units(rt: &RedDBRuntime, key: &str, requested: i64) -> ReservationAttempt {
    if let Some((reservation_id, units_claimed)) = existing_reservation(rt, key) {
        return ReservationAttempt::Existing {
            reservation_id,
            units_claimed,
        };
    }

    let reservation_id = format!("resv-{key}");
    exec(rt, "BEGIN");
    let claimed = exec(
        rt,
        &format!(
            "UPDATE inventory_units \
             SET status = 'claimed', reservation_key = '{key}' \
             WHERE sku = 'sku-1' AND status = 'available' \
             CLAIM EXACT {requested} ORDER BY unit_id ASC RETURNING unit_id"
        ),
    );
    assert_eq!(
        claimed.affected_rows, requested as u64,
        "recipe should use CLAIM EXACT so partial reservations are not committed"
    );
    exec(
        rt,
        &format!(
            "INSERT INTO reservation_idempotency \
             (idempotency_key, reservation_id, units_claimed) \
             VALUES ('{key}', '{reservation_id}', {requested})"
        ),
    );
    exec(
        rt,
        &format!(
            "QUEUE PUSH reservation_work \
             {{kind: 'reservation_claimed', reservation_id: '{reservation_id}'}}"
        ),
    );
    exec(rt, "COMMIT");

    ReservationAttempt::Created {
        reservation_id,
        units_claimed: requested,
    }
}

fn store_commit_batches(wal_path: &Path) -> Vec<Vec<Vec<u8>>> {
    WalReader::open(wal_path)
        .expect("wal opens")
        .iter()
        .map(|record| record.expect("wal record decodes").1)
        .filter_map(|record| match record {
            WalRecord::TxCommitBatch { actions, .. } => Some(actions),
            _ => None,
        })
        .collect()
}

fn action_contains_text(action: &[u8], needle: &str) -> bool {
    action
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn batch_contains_text(actions: &[Vec<u8>], needle: &str) -> bool {
    actions
        .iter()
        .any(|action| action_contains_text(action, needle))
}

/// Tear the final `bytes` off the WAL tail, corrupting the last commit
/// batch so recovery must drop it — the deterministic proxy for a crash
/// mid-commit, before the batch was fully durable.
fn truncate_wal_tail(path: &Path, bytes: u64) {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open wal");
    let len = file.metadata().expect("wal metadata").len();
    file.set_len(len - bytes).expect("truncate wal");
    file.sync_all().expect("sync truncated wal");
}

#[test]
fn transactions_doc_includes_canonical_reservation_recipe_without_external_coordinator() {
    assert!(
        TRANSACTIONS_DOC.contains("## Canonical reservation recipe"),
        "transactions docs must include the canonical reservation recipe"
    );
    assert!(
        TRANSACTIONS_DOC.contains("idempotency key"),
        "recipe must name the application-defined idempotency key"
    );
    assert!(
        !TRANSACTIONS_DOC.contains("Redis"),
        "recipe must not present Redis as required coordination infrastructure"
    );
}

#[test]
fn reservation_recipe_claims_units_key_and_queue_work_in_one_transaction() {
    let rt = in_memory_runtime();
    setup_reservation_schema(&rt);

    set_current_connection_id(145601);
    let first = reserve_units(&rt, "reserve-1", 2);
    assert_eq!(
        first,
        ReservationAttempt::Created {
            reservation_id: "resv-reserve-1".to_string(),
            units_claimed: 2,
        }
    );

    let retry = reserve_units(&rt, "reserve-1", 2);
    assert_eq!(
        retry,
        ReservationAttempt::Existing {
            reservation_id: "resv-reserve-1".to_string(),
            units_claimed: 2,
        }
    );

    assert_eq!(
        row_count(
            &rt,
            "SELECT unit_id FROM inventory_units WHERE reservation_key = 'reserve-1'"
        ),
        2,
        "retry must not claim duplicate resource units"
    );
    assert_eq!(
        row_count(
            &rt,
            "SELECT reservation_id FROM reservation_idempotency \
             WHERE idempotency_key = 'reserve-1'"
        ),
        1,
        "retry must not create a duplicate idempotency outcome"
    );
    assert_eq!(
        exec(&rt, "QUEUE PEEK reservation_work 10")
            .result
            .records
            .len(),
        1,
        "retry must not enqueue duplicate downstream work"
    );
    clear_current_connection_id();
}

#[test]
fn reservation_rollback_discards_claim_key_and_queue_work() {
    let rt = in_memory_runtime();
    setup_reservation_schema(&rt);

    set_current_connection_id(145602);
    exec(&rt, "BEGIN");
    exec(
        &rt,
        "UPDATE inventory_units \
         SET status = 'claimed', reservation_key = 'rollback-key' \
         WHERE sku = 'sku-1' AND status = 'available' \
         CLAIM EXACT 1 ORDER BY unit_id ASC RETURNING unit_id",
    );
    exec(
        &rt,
        "INSERT INTO reservation_idempotency \
         (idempotency_key, reservation_id, units_claimed) \
         VALUES ('rollback-key', 'resv-rollback-key', 1)",
    );
    exec(
        &rt,
        "QUEUE PUSH reservation_work \
         {kind: 'reservation_claimed', reservation_id: 'resv-rollback-key'}",
    );
    exec(&rt, "ROLLBACK");

    assert_eq!(
        row_count(
            &rt,
            "SELECT unit_id FROM inventory_units WHERE reservation_key = 'rollback-key'"
        ),
        0,
        "rollback must remove claimed resource state"
    );
    assert_eq!(
        row_count(
            &rt,
            "SELECT reservation_id FROM reservation_idempotency \
             WHERE idempotency_key = 'rollback-key'"
        ),
        0,
        "rollback must remove the idempotency outcome"
    );
    assert_eq!(
        exec(&rt, "QUEUE PEEK reservation_work 10")
            .result
            .records
            .len(),
        0,
        "rollback must remove queued downstream work"
    );
    clear_current_connection_id();
}

#[test]
fn reservation_commit_has_no_wal_prefix_with_claim_without_key_or_work() {
    let db = support::temp_db_file("transactional-reservation-recipe");
    let wal_path = reddb_file::unified_wal_path(db.path());

    let rt = persistent_runtime(&db);
    setup_reservation_schema(&rt);
    let before_batches = store_commit_batches(&wal_path).len();

    set_current_connection_id(145603);
    let outcome = reserve_units(&rt, "reserve-wal", 1);
    assert_eq!(
        outcome,
        ReservationAttempt::Created {
            reservation_id: "resv-reserve-wal".to_string(),
            units_claimed: 1,
        }
    );
    clear_current_connection_id();

    let batches = store_commit_batches(&wal_path);
    let new_batches = &batches[before_batches..];
    assert_eq!(
        new_batches.len(),
        1,
        "one reservation transaction should append exactly one durable commit batch"
    );
    let reservation_batch = &new_batches[0];
    assert!(
        batch_contains_text(reservation_batch, "inventory_units"),
        "commit batch must include the claimed resource table"
    );
    assert!(
        batch_contains_text(reservation_batch, "reservation_idempotency"),
        "commit batch must include the idempotency key table"
    );
    assert!(
        batch_contains_text(reservation_batch, "reservation_work"),
        "commit batch must include downstream queue work"
    );
}

#[test]
fn reservation_commit_survives_wal_crash_recovery() {
    // After a clean COMMIT, a simulated crash (drop with no graceful close)
    // leaves the WAL as the only record of the transaction. Reopening the
    // store replays that batch and all three effects — claim, idempotency
    // key, queue work — come back together.
    let db = support::temp_db_file("transactional-reservation-crash-recover");

    {
        let rt = persistent_runtime(&db);
        setup_reservation_schema(&rt);
        set_current_connection_id(145604);
        let outcome = reserve_units(&rt, "reserve-crash", 2);
        assert_eq!(
            outcome,
            ReservationAttempt::Created {
                reservation_id: "resv-reserve-crash".to_string(),
                units_claimed: 2,
            }
        );
        clear_current_connection_id();
        // Scope ends: the runtime drops without an explicit close, exactly
        // as after a process crash — only the durable WAL remains.
    }

    let rt = persistent_runtime(&db);
    set_current_connection_id(145614);
    assert_eq!(
        row_count(
            &rt,
            "SELECT unit_id FROM inventory_units \
             WHERE reservation_key = 'reserve-crash' AND status = 'claimed'"
        ),
        2,
        "recovery must restore the claimed resource units"
    );
    assert_eq!(
        row_count(
            &rt,
            "SELECT reservation_id FROM reservation_idempotency \
             WHERE idempotency_key = 'reserve-crash'"
        ),
        1,
        "recovery must restore the idempotency key row"
    );
    assert_eq!(
        exec(&rt, "QUEUE PEEK reservation_work 10")
            .result
            .records
            .len(),
        1,
        "recovery must restore the queued downstream work"
    );
    clear_current_connection_id();
}

#[test]
fn reservation_torn_commit_recovers_to_no_partial_state() {
    // A crash mid-commit — the reservation transaction's single commit batch
    // torn at the WAL tail — must recover to no partial state. None of the
    // three effects survive: the resource stays available, no idempotency key
    // exists, and the queue is empty.
    let db = support::temp_db_file("transactional-reservation-torn-commit");
    let wal_path = reddb_file::unified_wal_path(db.path());
    let stable_image = db.path().with_extension("stable-copy");

    {
        let rt = persistent_runtime(&db);
        setup_reservation_schema(&rt);
        // Fold the schema and seeded rows into the data file, then snapshot
        // it, so the WAL tail carries only the reservation commit batch.
        rt.checkpoint().expect("checkpoint pre-reservation prefix");
        std::fs::copy(db.path(), &stable_image).expect("copy stable image");

        set_current_connection_id(145605);
        let outcome = reserve_units(&rt, "reserve-torn", 2);
        assert_eq!(
            outcome,
            ReservationAttempt::Created {
                reservation_id: "resv-reserve-torn".to_string(),
                units_claimed: 2,
            }
        );
        clear_current_connection_id();
    }

    // Restore the pre-reservation data file and tear the tail of the
    // reservation commit batch: the crash happened before it was durable.
    std::fs::copy(&stable_image, db.path()).expect("restore stable image");
    let _ = std::fs::remove_file(&stable_image);
    truncate_wal_tail(&wal_path, 1);

    let rt = persistent_runtime(&db);
    set_current_connection_id(145615);
    assert_eq!(
        row_count(
            &rt,
            "SELECT unit_id FROM inventory_units WHERE status = 'available'"
        ),
        3,
        "a torn commit must leave every resource unit available"
    );
    assert_eq!(
        row_count(
            &rt,
            "SELECT unit_id FROM inventory_units WHERE reservation_key = 'reserve-torn'"
        ),
        0,
        "a torn commit must leave no claimed resource state"
    );
    assert_eq!(
        row_count(
            &rt,
            "SELECT reservation_id FROM reservation_idempotency \
             WHERE idempotency_key = 'reserve-torn'"
        ),
        0,
        "a torn commit must leave no idempotency key row"
    );
    assert_eq!(
        exec(&rt, "QUEUE PEEK reservation_work 10")
            .result
            .records
            .len(),
        0,
        "a torn commit must leave no queued downstream work"
    );
    clear_current_connection_id();
}
