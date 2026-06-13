//! Issue #824 — primary-authoritative TTL expiry under replication.
//!
//! TTL/expiry decisions are owned by the primary. Replicas replay the
//! primary's delete record, and their read path hides records whose
//! absolute expiry has already passed while they are still waiting for
//! that delete to arrive.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::path::Path;

use reddb::application::NativeUseCases;
use reddb::replication::cdc::{ChangeOperation, ChangeRecord};
use reddb::replication::logical::{ApplyMode, LogicalChangeApplier};
use reddb::replication::primary::LogicalWalSpool;
use reddb::replication::ReplicationConfig;
use reddb::storage::RedDB;
use reddb::{RedDBOptions, RedDBRuntime};

fn temp_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
}

fn read_spool(path: &Path) -> Vec<ChangeRecord> {
    let spool = LogicalWalSpool::open(path).expect("open logical WAL spool");
    spool
        .read_since(0, usize::MAX)
        .expect("read logical WAL")
        .into_iter()
        .map(|(lsn, bytes)| {
            ChangeRecord::decode(&bytes)
                .unwrap_or_else(|err| panic!("decode logical record lsn={lsn}: {err}"))
        })
        .collect()
}

fn replay(replica: &RedDB, records: impl IntoIterator<Item = ChangeRecord>) {
    let applier = LogicalChangeApplier::new(0);
    for record in records {
        applier
            .apply(replica, &record, ApplyMode::Replica)
            .unwrap_or_else(|err| panic!("apply lsn={} failed: {err}", record.lsn));
    }
}

fn physical_count(db: &RedDB, collection: &str) -> usize {
    db.store()
        .get_collection(collection)
        .map(|manager| manager.query_all(|_| true).len())
        .unwrap_or(0)
}

#[test]
fn primary_ttl_sweep_replicates_delete_to_replica() {
    let primary_path = temp_path("primary-delete");
    let replica_path = temp_path("replica-delete");

    {
        let primary = RedDBRuntime::with_options(
            RedDBOptions::persistent(&primary_path).with_replication(ReplicationConfig::primary()),
        )
        .expect("open primary");
        primary
            .execute_query("CREATE TABLE sessions (id INT, token TEXT)")
            .expect("create table");
        primary
            .execute_query("INSERT INTO sessions (id, token, _ttl_ms) VALUES (1, 'a', 1)")
            .expect("insert ttl row");

        NativeUseCases::new(&primary)
            .apply_retention_policy()
            .expect("primary retention sweep");
        primary.db().flush().expect("flush primary");
    }

    let records = read_spool(&primary_path);
    assert!(
        records.iter().any(|record| {
            record.operation == ChangeOperation::Delete && record.collection == "sessions"
        }),
        "primary TTL sweep must materialize expiry as a logical delete: {records:?}"
    );

    let replica = RedDB::open(&replica_path).expect("open replica");
    replay(&replica, records);
    assert_eq!(
        physical_count(&replica, "sessions"),
        0,
        "replica must converge by applying the primary's delete"
    );

    drop(replica);
}

#[test]
fn lagging_replica_filters_expired_row_before_delete_arrives() {
    let primary_path = temp_path("primary-filter");
    let replica_path = temp_path("replica-filter");

    {
        let primary = RedDBRuntime::with_options(
            RedDBOptions::persistent(&primary_path).with_replication(ReplicationConfig::primary()),
        )
        .expect("open primary");
        primary
            .execute_query("CREATE TABLE sessions (id INT, token TEXT)")
            .expect("create table");
        primary
            .execute_query("INSERT INTO sessions (id, token, _expires_at) VALUES (1, 'a', 1)")
            .expect("insert expired row");
        primary.db().flush().expect("flush primary");
    }

    let insert_records = read_spool(&primary_path)
        .into_iter()
        .filter(|record| record.operation == ChangeOperation::Insert)
        .collect::<Vec<_>>();
    assert_eq!(insert_records.len(), 1, "expected one insert record");

    {
        let replica = RedDB::open(&replica_path).expect("open replica storage");
        replay(&replica, insert_records);
        replica.flush().expect("flush replica");
    }

    let replica = RedDBRuntime::with_options(
        RedDBOptions::persistent(&replica_path)
            .with_replication(ReplicationConfig::replica("http://primary:50051")),
    )
    .expect("open replica runtime");

    let read = replica
        .execute_query("SELECT * FROM sessions")
        .expect("replica select");
    assert_eq!(
        read.result.records.len(),
        0,
        "lagging replica must not serve a row past its absolute expiry"
    );
    assert_eq!(
        physical_count(&replica.db(), "sessions"),
        1,
        "read-time filtering must not locally delete the lagging row"
    );
    assert!(
        NativeUseCases::new(&replica)
            .apply_retention_policy()
            .is_err(),
        "replica retention sweep must not mutate state from its own clock"
    );

    drop(replica);
}

#[test]
fn replica_hypertable_expiry_sweep_is_read_only_noop() {
    let path = temp_path("hypertable-replica");

    {
        let seed = RedDB::open(&path).expect("seed replica path");
        seed.flush().expect("flush seed");
    }

    let replica = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path)
            .with_replication(ReplicationConfig::replica("http://primary:50051")),
    )
    .expect("open replica");
    let replica_db = replica.db();
    let registry = replica_db.hypertables();
    registry.register(
        reddb::storage::timeseries::HypertableSpec::new("metrics", "ts", 3_600_000_000_000)
            .with_ttl_ns(3_600_000_000_000),
    );
    registry
        .route("metrics", 0)
        .expect("route replica chunk fixture");

    let sweep = replica
        .execute_query("SELECT HYPERTABLE_SWEEP_EXPIRED('metrics', 7200000000000) AS dropped")
        .expect("replica sweep scalar");
    assert_eq!(
        sweep.result.records[0].get("dropped"),
        Some(&reddb::storage::schema::Value::Integer(0)),
        "replica sweep scalar must report no local drops"
    );

    let chunks = replica
        .execute_query("SELECT HYPERTABLE_SHOW_CHUNKS('metrics') AS chunks")
        .expect("show chunks");
    let chunk_count = match chunks.result.records[0].get("chunks") {
        Some(reddb::storage::schema::Value::Array(items)) => items.len(),
        other => panic!("expected chunks array, got {other:?}"),
    };
    assert_eq!(
        chunk_count, 1,
        "replica hypertable expiry sweep must not drop local chunks"
    );

    drop(replica);
}
