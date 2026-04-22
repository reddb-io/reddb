//! End-to-end: CREATE HYPERTABLE DDL registers a HypertableSpec on
//! the runtime's shared registry + creates the backing collection.

use reddb::application::ExecuteQueryInput;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

#[test]
fn create_hypertable_registers_spec() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'".into(),
    })
    .expect("create hypertable ok");

    let db = rt.db();
    let reg = db.hypertables();
    let spec = reg.get("metrics").expect("hypertable registered");
    assert_eq!(spec.time_column, "ts");
    assert_eq!(spec.chunk_interval_ns, 86_400_000_000_000);
    assert!(spec.default_ttl_ns.is_none());
}

#[test]
fn create_hypertable_with_ttl() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE events TIME_COLUMN ts CHUNK_INTERVAL '1h' TTL '7d'".into(),
    })
    .expect("create hypertable with ttl ok");

    let db = rt.db();
    let spec = db.hypertables().get("events").expect("registered");
    assert_eq!(spec.chunk_interval_ns, 3_600_000_000_000);
    assert_eq!(spec.default_ttl_ns, Some(7 * 86_400_000_000_000));
}

#[test]
fn create_hypertable_requires_time_column() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let err = q
        .execute(ExecuteQueryInput {
            query: "CREATE HYPERTABLE bad CHUNK_INTERVAL '1d'".into(),
        })
        .unwrap_err()
        .to_string();
    assert!(err.contains("TIME_COLUMN"), "unexpected error: {err}");
}

#[test]
fn create_hypertable_requires_chunk_interval() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let err = q
        .execute(ExecuteQueryInput {
            query: "CREATE HYPERTABLE bad TIME_COLUMN ts".into(),
        })
        .unwrap_err()
        .to_string();
    assert!(err.contains("CHUNK_INTERVAL"), "unexpected error: {err}");
}

#[test]
fn create_hypertable_backing_collection_exists() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '5m'".into(),
    })
    .expect("create hypertable ok");

    // Backing collection is created so INSERTs would land somewhere.
    let db = rt.db();
    assert!(
        db.store().get_collection("metrics").is_some(),
        "backing collection must exist after CREATE HYPERTABLE"
    );
}

#[test]
fn list_hypertables_surfaces_registered_entries() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'".into(),
    })
    .expect("ok");
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE events TIME_COLUMN ts CHUNK_INTERVAL '1h'".into(),
    })
    .expect("ok");
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT LIST_HYPERTABLES() AS names".into(),
        })
        .expect("list ok");
    let names = r.result.records[0].values.get("names").expect("names");
    use reddb::storage::schema::Value;
    let arr = match names {
        Value::Array(v) => v,
        other => panic!("expected Array, got {other:?}"),
    };
    assert_eq!(arr.len(), 2, "two hypertables registered");
    let mut got: Vec<String> = arr
        .iter()
        .filter_map(|v| {
            if let Value::Text(s) = v {
                Some(s.to_string())
            } else {
                None
            }
        })
        .collect();
    got.sort();
    assert_eq!(got, vec!["events".to_string(), "metrics".to_string()]);
}

#[test]
fn drop_hypertable_removes_registry_entry() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE HYPERTABLE metrics TIME_COLUMN ts CHUNK_INTERVAL '1d'".into(),
    })
    .expect("create ok");
    let db = rt.db();
    assert!(db.hypertables().get("metrics").is_some());

    q.execute(ExecuteQueryInput {
        query: "DROP HYPERTABLE metrics".into(),
    })
    .expect("drop ok");
    let db = rt.db();
    assert!(
        db.hypertables().get("metrics").is_none(),
        "registry should be cleared after DROP HYPERTABLE"
    );
    assert!(
        db.store().get_collection("metrics").is_none(),
        "backing collection should be gone after DROP HYPERTABLE"
    );
}

#[test]
fn plain_create_timeseries_does_not_register_hypertable() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TIMESERIES legacy RETENTION 30 DAYS".into(),
    })
    .expect("create timeseries ok");
    let db = rt.db();
    assert!(db.hypertables().get("legacy").is_none());
}
