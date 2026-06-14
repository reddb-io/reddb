//! Issue #596 — slice 9d of #575.
//!
//! Wires the `RefreshCollection` WAL action (issue #595) into
//! `LogicalChangeApplier` so a replica deterministically replays a
//! primary `REFRESH MATERIALIZED VIEW v` and ends up with the same
//! backing-collection contents as the primary.
//!
//! Three contracts, three tests:
//!
//! 1. **Replay is row-for-row deterministic.** A primary runtime
//!    creates a materialized view, inserts rows, and refreshes it.
//!    The serialized record bytes the primary writes into the
//!    `RefreshCollection` WAL action are fed to a fresh replica
//!    database through `LogicalChangeApplier::apply` — the replica's
//!    backing collection has exactly the same entities afterwards.
//!
//! 2. **Replay is idempotent.** Re-applying the same `Refresh`
//!    change record on an already-up-to-date replica is a no-op
//!    (`ApplyOutcome::Idempotent`), not a divergence and not a
//!    re-execution.
//!
//! 3. **Primary REFRESH emits a `Refresh` CDC event.** The runtime
//!    surfaces a `ChangeOperation::Refresh` event on the CDC ring
//!    after `REFRESH MATERIALIZED VIEW v` so downstream consumers
//!    (and the replica fetcher) see the boundary explicitly instead
//!    of having to infer it from a flurry of inserts.

#[allow(dead_code)]
mod support;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::replication::cdc::{ChangeOperation, ChangeRecord};
use reddb::replication::logical::{ApplyMode, ApplyOutcome, LogicalChangeApplier};
use reddb::replication::ReplicationConfig;
use reddb::storage::schema::Value;
use reddb::storage::{
    EntityData, EntityId, EntityKind, RedDB, RowData, UnifiedEntity, UnifiedStore,
};
use reddb::{RedDBOptions, RedDBRuntime};

const BACKING: &str = "paid_orders_596";

fn temp_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
}

/// Build an entity in the same shape as the runtime's REFRESH path
/// would: a table row with `id` already allocated by the caller.
fn table_row(id: u64, status: &str, total: i64) -> UnifiedEntity {
    UnifiedEntity::new(
        EntityId::new(id),
        EntityKind::TableRow {
            table: Arc::from(BACKING),
            row_id: 0,
        },
        EntityData::Row(RowData::with_names(
            vec![
                Value::UnsignedInteger(id),
                Value::text(status),
                Value::Integer(total),
            ],
            vec!["id".to_string(), "status".to_string(), "total".to_string()],
        )),
    )
}

/// Sorted (entity_id, status, total) tuples — the per-row shape we
/// use to compare primary vs. replica backing-collection contents
/// row-for-row. Sorted because `query_all` doesn't promise an order
/// and we only care about set equality between the two sides.
/// Build a comparable per-row snapshot of a collection. Each row is
/// keyed by entity id and carries the named-field projection — a
/// content comparison that's stable regardless of the in-memory
/// column ordering the segment manager normalises to (the named
/// map is the surface SQL reads through, so equality here is what
/// the user-visible contract guarantees).
fn snapshot(store: &Arc<UnifiedStore>, collection: &str) -> Vec<(u64, Vec<(String, String)>)> {
    let Some(manager) = store.get_collection(collection) else {
        return Vec::new();
    };
    let mut rows: Vec<(u64, Vec<(String, String)>)> = manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            let names: Vec<String> = if let Some(schema) = &row.schema {
                schema.as_ref().clone()
            } else if let Some(named) = &row.named {
                named.keys().cloned().collect()
            } else {
                return None;
            };
            let mut fields: Vec<(String, String)> = names
                .into_iter()
                .filter_map(|name| {
                    let value = row.get_field(&name)?;
                    Some((name, format!("{value:?}")))
                })
                .collect();
            fields.sort();
            Some((entity.id.raw(), fields))
        })
        .collect();
    rows.sort_by_key(|(id, _)| *id);
    rows
}

fn run_with_large_stack(name: &str, f: fn()) {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
        .expect("spawn materialized view replica replay test")
        .join()
        .expect("materialized view replica replay test panicked");
}

#[test]
fn replica_replay_of_refresh_collection_matches_primary_row_for_row() {
    run_with_large_stack(
        "replica-replay-of-refresh-collection-matches-primary-row-for-row",
        replica_replay_of_refresh_collection_matches_primary_row_for_row_impl,
    );
}

fn replica_replay_of_refresh_collection_matches_primary_row_for_row_impl() {
    let primary_path = temp_path("primary-rowforrow");
    let replica_path = temp_path("replica-rowforrow");

    // ---- Primary side: open with primary replication config so the
    // refresh path takes the same code branch the wire fetcher
    // would observe, then publish a 2-row refresh.
    let primary_rt = {
        let opts =
            RedDBOptions::persistent(&primary_path).with_replication(ReplicationConfig::primary());
        RedDBRuntime::with_options(opts).expect("open primary")
    };
    let primary_store = primary_rt.db().store();

    let entities = vec![table_row(1, "paid", 100), table_row(2, "paid", 200)];

    let serialized = primary_store
        .refresh_collection(BACKING, entities)
        .expect("primary refresh_collection succeeds");
    assert_eq!(
        serialized.len(),
        2,
        "primary refresh must write exactly the supplied row count"
    );

    let primary_snapshot = snapshot(&primary_store, BACKING);
    assert_eq!(primary_snapshot.len(), 2);

    // ---- Replica side: independent storage instance. Apply the
    // refresh through the same code path the wire replica uses.
    let replica = RedDB::open(&replica_path).expect("open replica");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let refresh_record =
        ChangeRecord::for_refresh(42, timestamp, BACKING.to_string(), serialized.clone());

    let applier = LogicalChangeApplier::new(0);
    assert_eq!(
        applier
            .apply(&replica, &refresh_record, ApplyMode::Replica)
            .expect("replica apply must succeed"),
        ApplyOutcome::Applied
    );
    assert_eq!(applier.last_applied_lsn(), 42);

    let replica_snapshot = snapshot(&replica.store(), BACKING);
    assert_eq!(
        replica_snapshot, primary_snapshot,
        "replica's backing collection must match primary's row-for-row \
         after a single RefreshCollection replay"
    );

    drop(primary_rt);
    drop(replica);
}

#[test]
fn replica_replay_of_refresh_collection_is_idempotent() {
    run_with_large_stack(
        "replica-replay-of-refresh-collection-is-idempotent",
        replica_replay_of_refresh_collection_is_idempotent_impl,
    );
}

fn replica_replay_of_refresh_collection_is_idempotent_impl() {
    let path = temp_path("idempotent");
    let primary_path = temp_path("idempotent-primary");

    let primary_rt = {
        let opts =
            RedDBOptions::persistent(&primary_path).with_replication(ReplicationConfig::primary());
        RedDBRuntime::with_options(opts).expect("open primary")
    };
    let primary_store = primary_rt.db().store();

    let serialized = primary_store
        .refresh_collection(
            BACKING,
            vec![table_row(1, "paid", 100), table_row(2, "paid", 200)],
        )
        .expect("primary refresh");
    let primary_snapshot = snapshot(&primary_store, BACKING);

    let replica = RedDB::open(&path).expect("open replica");
    let timestamp = 1_700_000_000_000u64;
    let refresh_record = ChangeRecord::for_refresh(7, timestamp, BACKING.to_string(), serialized);

    let applier = LogicalChangeApplier::new(0);
    assert_eq!(
        applier
            .apply(&replica, &refresh_record, ApplyMode::Replica)
            .expect("first replay"),
        ApplyOutcome::Applied
    );

    // Re-apply the same WAL frame — primary network burped, replica
    // re-fetched. Must NOT replay (which would be a side-effect-free
    // rebuild but still a wasted refresh) and must NOT diverge.
    assert_eq!(
        applier
            .apply(&replica, &refresh_record, ApplyMode::Replica)
            .expect("idempotent replay"),
        ApplyOutcome::Idempotent
    );
    assert_eq!(applier.last_applied_lsn(), 7);

    let replica_snapshot = snapshot(&replica.store(), BACKING);
    assert_eq!(
        replica_snapshot, primary_snapshot,
        "idempotent replay must leave backing contents byte-equivalent to the primary"
    );

    drop(primary_rt);
    drop(replica);
}

#[test]
fn primary_refresh_materialized_view_emits_refresh_cdc_event() {
    run_with_large_stack(
        "primary-refresh-materialized-view-emits-refresh-cdc-event",
        primary_refresh_materialized_view_emits_refresh_cdc_event_impl,
    );
}

fn primary_refresh_materialized_view_emits_refresh_cdc_event_impl() {
    let primary_path = temp_path("primary-cdc");
    let opts =
        RedDBOptions::persistent(&primary_path).with_replication(ReplicationConfig::primary());
    let rt = RedDBRuntime::with_options(opts).expect("open primary");

    rt.execute_query("CREATE TABLE orders_596 (id INT, total INT, status TEXT)")
        .expect("CREATE TABLE");
    rt.execute_query(
        "INSERT INTO orders_596 (id, total, status) VALUES \
           (1, 100, 'paid'), \
           (2, 200, 'paid'), \
           (3, 300, 'pending')",
    )
    .expect("INSERT");
    rt.execute_query(
        "CREATE MATERIALIZED VIEW mv_596 AS \
         SELECT id, total FROM orders_596 WHERE status = 'paid'",
    )
    .expect("CREATE MATERIALIZED VIEW");

    let cdc_before = rt.cdc_current_lsn();

    rt.execute_query("REFRESH MATERIALIZED VIEW mv_596")
        .expect("REFRESH");

    // The runtime must surface a Refresh CDC event on the backing
    // collection — that's the boundary marker the replica fetcher
    // hands to `LogicalChangeApplier::apply` with a Refresh
    // ChangeRecord. Downstream CDC consumers also rely on this to
    // distinguish "atomic swap" from a burst of row inserts.
    let events = rt.cdc_poll(cdc_before, 64);
    let refresh_events: Vec<_> = events
        .iter()
        .filter(|event| event.operation == ChangeOperation::Refresh)
        .collect();
    assert_eq!(
        refresh_events.len(),
        1,
        "expected exactly one Refresh CDC event after REFRESH MATERIALIZED VIEW; got {events:?}"
    );
    let refresh_event = refresh_events[0];
    assert_eq!(
        refresh_event.collection, "mv_596",
        "Refresh CDC event must target the materialized view's backing collection name"
    );

    drop(rt);
}
