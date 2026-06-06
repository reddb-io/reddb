//! Issue #813 — slice of PRD #811 (replication convergence).
//!
//! A replica that lost its persisted cursor (crash, wiped config) and
//! re-pulls a WAL prefix it already applied must converge to the same
//! entity set — never accumulate duplicates. The stateful LSN machine
//! in `LogicalChangeApplier` does NOT protect this case: a wiped cursor
//! means a fresh applier anchored at LSN 0, so every record re-runs
//! through `apply_record`. The only thing standing between a re-pull and
//! a duplicated entity set is `apply_record`'s upsert logic.
//!
//! ## The 22×-inflation mechanism (pinned by
//! `repull_with_updates_converges_to_primary_live_set`)
//!
//! Table rows are MVCC-versioned: an `UPDATE` does not mutate the row in
//! place — it installs a NEW physical version (a fresh `EntityId`) that
//! shares the row's stable `logical_id`, and marks the prior version
//! superseded (`xmax != 0`) so snapshot reads skip it. The primary emits
//! a single `Update` logical-WAL record carrying only the new version;
//! the old version's supersession (`xmax`) is implicit and never sent on
//! the wire. The replica therefore inserted the new version while
//! leaving every prior version LIVE — so each update to a row left a
//! stale live duplicate behind, and a full re-pull replayed them all.
//! A row updated N times became N live rows on the replica: the observed
//! 22× divergence.
//!
//! These tests harvest the exact logical-WAL records the primary spools
//! for real SQL writes, then replay that prefix against a replica
//! through fresh appliers anchored at 0 (the way a cursor-wiped replica
//! reconnects) and assert the replica's live row set matches the
//! primary's and stays flat across repeated re-pulls.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::application::{ExecuteQueryInput, QueryUseCases};
use reddb::replication::cdc::ChangeRecord;
use reddb::replication::logical::{ApplyMode, LogicalChangeApplier};
use reddb::replication::primary::LogicalWalSpool;
use reddb::replication::ReplicationConfig;
use reddb::storage::{EntityKind, RedDB, UnifiedStore};
use reddb::{RedDBOptions, RedDBRuntime};

fn temp_path(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "reddb-813-{prefix}-{}-{}.rdb",
        std::process::id(),
        nanos
    ))
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    for ext in ["-wal", "-hdr", "-meta", "-dwb", "-uwal", ".logical.wal"] {
        let mut p = path.to_path_buf().into_os_string();
        p.push(ext);
        let _ = std::fs::remove_file(PathBuf::from(p));
    }
}

fn exec(query: &QueryUseCases<'_, RedDBRuntime>, sql: &str) {
    query
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"));
}

/// Count live entities in a collection on a replica store.
fn entity_count(replica: &RedDB, collection: &str) -> usize {
    replica
        .store()
        .get_collection(collection)
        .map(|manager| manager.query_all(|_| true).len())
        .unwrap_or(0)
}

/// Snapshot the LIVE (xmax == 0) table rows of a collection keyed by
/// stable logical id, each carrying its named-field projection. This is
/// the user-visible row set — the contract a replica must converge to.
/// Panics if two live versions share a logical id (a duplicate live
/// row, the exact corruption this issue is about).
fn live_rows(store: &Arc<UnifiedStore>, collection: &str) -> BTreeMap<u64, Vec<(String, String)>> {
    let Some(manager) = store.get_collection(collection) else {
        return BTreeMap::new();
    };
    let mut rows: BTreeMap<u64, Vec<(String, String)>> = BTreeMap::new();
    for entity in manager.query_all(|e| matches!(e.kind, EntityKind::TableRow { .. })) {
        if entity.xmax != 0 {
            continue; // superseded version — not part of the live set
        }
        let Some(row) = entity.data.as_row() else {
            continue;
        };
        let names: Vec<String> = if let Some(schema) = &row.schema {
            schema.as_ref().clone()
        } else if let Some(named) = &row.named {
            named.keys().cloned().collect()
        } else {
            continue;
        };
        let mut fields: Vec<(String, String)> = names
            .into_iter()
            .filter_map(|name| row.get_field(&name).map(|v| (name, format!("{v:?}"))))
            .collect();
        fields.sort();
        let logical = entity.logical_id().raw();
        assert!(
            rows.insert(logical, fields).is_none(),
            "two live versions share logical_id {logical} in '{collection}' — \
             duplicate live row (issue #813 inflation)"
        );
    }
    rows
}

/// Drive a primary through real SQL writes. Returns the logical-WAL
/// records it spooled (in LSN order — the exact bytes a wire replica
/// pulls) together with the primary's live-row snapshot, so tests can
/// assert the replica converges to the *primary's* state rather than a
/// hand-computed count.
fn drive_primary(
    primary_path: &std::path::Path,
    collection: &str,
    setup: &[&str],
) -> (Vec<ChangeRecord>, BTreeMap<u64, Vec<(String, String)>>) {
    let primary_rt = {
        let opts = RedDBOptions::persistent(&primary_path.to_string_lossy().to_string())
            .with_replication(ReplicationConfig::primary());
        RedDBRuntime::with_options(opts).expect("open primary")
    };
    {
        let query = QueryUseCases::new(&primary_rt);
        for sql in setup {
            exec(&query, sql);
        }
    }
    primary_rt.db().flush().expect("flush primary");

    let primary_live = live_rows(&primary_rt.db().store(), collection);

    let spool = LogicalWalSpool::open(primary_path).expect("open spool");
    let raw = spool.read_since(0, usize::MAX).expect("read spool");
    drop(primary_rt);

    let records = raw
        .into_iter()
        .map(|(lsn, bytes)| {
            ChangeRecord::decode(&bytes)
                .unwrap_or_else(|err| panic!("decode spool record lsn={lsn}: {err}"))
        })
        .collect();
    (records, primary_live)
}

/// Apply a full record prefix against a replica through a FRESH applier
/// anchored at LSN 0 — exactly what a cursor-wiped replica does when it
/// reconnects and re-pulls from the start.
fn replay_from_zero(replica: &RedDB, records: &[ChangeRecord]) {
    let applier = LogicalChangeApplier::new(0);
    for record in records {
        applier
            .apply(replica, record, ApplyMode::Replica)
            .unwrap_or_else(|err| panic!("apply lsn={} failed: {err}", record.lsn));
    }
}

fn replay_from_zero_with_indexes(replica: &RedDBRuntime, records: &[ChangeRecord]) {
    let applier = LogicalChangeApplier::new(0);
    for record in records {
        applier
            .apply_with_index_store(
                replica.db().as_ref(),
                replica.index_store_ref(),
                record,
                ApplyMode::Replica,
            )
            .unwrap_or_else(|err| panic!("apply lsn={} failed: {err}", record.lsn));
    }
}

/// Pure inserts: a 5-row table replayed from LSN 0, then re-pulled from
/// LSN 0 (cursor wiped), must not inflate the replica's entity count.
#[test]
fn repull_from_lsn_zero_does_not_inflate_table_rows() {
    let primary_path = temp_path("primary-rows");
    let replica_path = temp_path("replica-rows");

    let (records, _) = drive_primary(
        &primary_path,
        "accounts",
        &[
            "CREATE TABLE accounts (id INTEGER, name TEXT)",
            "INSERT INTO accounts (id, name) VALUES (1, 'a')",
            "INSERT INTO accounts (id, name) VALUES (2, 'b')",
            "INSERT INTO accounts (id, name) VALUES (3, 'c')",
            "INSERT INTO accounts (id, name) VALUES (4, 'd')",
            "INSERT INTO accounts (id, name) VALUES (5, 'e')",
        ],
    );
    assert!(!records.is_empty(), "primary must spool the inserts");

    let replica = RedDB::open(&replica_path).expect("open replica");

    replay_from_zero(&replica, &records);
    let after_first = entity_count(&replica, "accounts");
    assert_eq!(after_first, 5, "first replay must land exactly 5 rows");

    replay_from_zero(&replica, &records);
    let after_second = entity_count(&replica, "accounts");
    assert_eq!(
        after_second, after_first,
        "re-pull from LSN 0 must converge: expected {after_first} rows, got {after_second}"
    );

    drop(replica);
    cleanup(&primary_path);
    cleanup(&replica_path);
}

/// Reconnect storm: re-pulling from 0 repeatedly must keep the live row
/// set flat, not multiply it.
#[test]
fn repeated_repull_from_zero_stays_flat() {
    let primary_path = temp_path("primary-flap");
    let replica_path = temp_path("replica-flap");

    let (records, primary_live) = drive_primary(
        &primary_path,
        "widgets",
        &[
            "CREATE TABLE widgets (id INTEGER, label TEXT)",
            "INSERT INTO widgets (id, label) VALUES (10, 'x')",
            "INSERT INTO widgets (id, label) VALUES (20, 'y')",
            "INSERT INTO widgets (id, label) VALUES (30, 'z')",
        ],
    );

    let replica = RedDB::open(&replica_path).expect("open replica");

    for _ in 0..5 {
        replay_from_zero(&replica, &records);
        assert_eq!(
            live_rows(&replica.store(), "widgets"),
            primary_live,
            "every re-pull must converge to the primary's live row set"
        );
    }

    drop(replica);
    cleanup(&primary_path);
    cleanup(&replica_path);
}

/// THE 22×-inflation regression. A table row that is UPDATEd installs a
/// new MVCC version (fresh `EntityId`, same `logical_id`) on the primary
/// and supersedes the prior version. The replica receives only the new
/// version, so before the fix it left the prior version live — every
/// re-pull replaying the insert+update prefix grew the live set. After
/// the fix the replica marks superseded versions on apply, so its live
/// row set equals the primary's and stays flat across re-pulls.
#[test]
fn repull_with_updates_converges_to_primary_live_set() {
    let primary_path = temp_path("primary-upd");
    let replica_path = temp_path("replica-upd");

    let (records, primary_live) = drive_primary(
        &primary_path,
        "inventory",
        &[
            "CREATE TABLE inventory (id INTEGER, qty INTEGER)",
            "INSERT INTO inventory (id, qty) VALUES (1, 100)",
            "INSERT INTO inventory (id, qty) VALUES (2, 200)",
            "UPDATE inventory SET qty = 999 WHERE id = 1",
            "UPDATE inventory SET qty = 777 WHERE id = 1",
            "UPDATE inventory SET qty = 555 WHERE id = 2",
        ],
    );

    // Primary's live set is the ground truth: two rows, latest values.
    assert_eq!(primary_live.len(), 2, "primary must show two live rows");

    let replica = RedDB::open(&replica_path).expect("open replica");

    // First replay: replica must match the primary's live row set
    // exactly — no stale pre-update versions left live.
    replay_from_zero(&replica, &records);
    assert_eq!(
        live_rows(&replica.store(), "inventory"),
        primary_live,
        "after first replay the replica's live rows must equal the primary's \
         (a stale live version would mean the pre-update row was never superseded)"
    );

    // Cursor wiped → re-pull the same prefix from LSN 0 with a brand-new
    // applier. Must still converge, not double the live set.
    replay_from_zero(&replica, &records);
    assert_eq!(
        live_rows(&replica.store(), "inventory"),
        primary_live,
        "re-pull from LSN 0 must remain convergent — no duplicate live rows"
    );

    drop(replica);
    cleanup(&primary_path);
    cleanup(&replica_path);
}

#[test]
fn logical_stream_replay_maintains_declared_replica_indexes() {
    let primary_path = temp_path("primary-indexed");
    let replica_path = temp_path("replica-indexed");

    let (records, _) = drive_primary(
        &primary_path,
        "users",
        &[
            "CREATE TABLE users (id INTEGER, age INTEGER, city TEXT)",
            "INSERT INTO users (id, age, city) VALUES (1, 31, 'NYC')",
            "INSERT INTO users (id, age, city) VALUES (2, 29, 'NYC')",
            "INSERT INTO users (id, age, city) VALUES (3, 44, 'LA')",
            "UPDATE users SET city = 'SF', age = 35 WHERE id = 2",
        ],
    );
    assert!(
        records.iter().all(|record| record.entity_bytes.is_some()),
        "replication stream must carry semantic entity records, not physical WAL frames"
    );

    let replica = RedDBRuntime::with_options(RedDBOptions::persistent(
        &replica_path.to_string_lossy().to_string(),
    ))
    .expect("open replica runtime");
    replica
        .execute_query("CREATE TABLE users (id INTEGER, age INTEGER, city TEXT)")
        .unwrap();
    replica
        .execute_query("CREATE INDEX idx_city ON users (city) USING HASH")
        .unwrap();
    replica
        .execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
        .unwrap();

    replay_from_zero_with_indexes(&replica, &records);

    let live = live_rows(&replica.db().store(), "users");
    assert!(
        live.values().any(|fields| fields
            .iter()
            .any(|(name, value)| name == "city" && value.contains("SF"))),
        "semantic replay must install the updated SF row before index lookup; live={live:?}"
    );
    let sf_ids = replica
        .index_store_ref()
        .hash_lookup("users", "idx_city", b"SF")
        .expect("idx_city hash lookup");
    assert_eq!(
        sf_ids.len(),
        1,
        "replica index replay must populate idx_city for SF exactly once; live={live:?}"
    );
    let age_35_ids = replica
        .index_store_ref()
        .hash_lookup("users", "idx_age_hash", &35i64.to_le_bytes())
        .expect("idx_age_hash lookup");
    assert_eq!(
        age_35_ids.len(),
        1,
        "replica BTree replay must maintain the equality helper for age=35"
    );

    drop(replica);
    cleanup(&primary_path);
    cleanup(&replica_path);
}

#[test]
fn replica_progress_exposes_applied_and_durable_promotion_watermarks() {
    let replica = RedDBRuntime::with_options(
        RedDBOptions::in_memory()
            .with_replication(ReplicationConfig::replica("http://primary:50051")),
    )
    .expect("open replica runtime");

    replica.db().store().set_config_tree(
        "red.replication",
        &reddb::json!({
            "last_applied_lsn": 40,
            "last_durable_lsn": 39,
            "last_seen_primary_lsn": 42
        }),
    );

    assert_eq!(replica.replica_durable_lsn(), 39);
    assert_eq!(replica.replica_required_promotion_lsn(), 42);
    assert!(
        reddb::replication::check_promotion_watermark(
            replica.replica_durable_lsn(),
            replica.replica_required_promotion_lsn(),
            reddb::replication::CommitPolicy::AckN(1),
        )
        .is_err(),
        "normal promotion must reject a replica below the durable target"
    );
}
