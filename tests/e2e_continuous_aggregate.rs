//! End-to-end: CA_REGISTER / CA_DROP / CA_STATE / CA_LIST via SQL.
//!
//! Exposes the continuous-aggregate engine through scalar functions.
//! Full SELECT-based `CREATE CONTINUOUS AGGREGATE` DDL + driven
//! refresh over a hypertable source is tracked as a follow-up.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

#[test]
fn register_then_list_surfaces_aggregate() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "SELECT CA_REGISTER('five_min_load', 'metrics', '5m', \
                'avg_load', 'avg', 'load') AS ok"
            .into(),
    })
    .expect("register ok");

    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT CA_LIST() AS names".into(),
        })
        .expect("list ok");
    let names = r.result.records[0].values.get("names").expect("names");
    match names {
        Value::Array(items) => {
            assert_eq!(items.len(), 1);
            assert!(matches!(&items[0], Value::Text(s) if s.as_ref() == "five_min_load"));
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn state_returns_initial_watermark() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "SELECT CA_REGISTER('ca1', 'metrics', '1h', 'c', 'count', 'v') AS ok".into(),
    })
    .expect("register ok");

    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT CA_STATE('ca1') AS st".into(),
        })
        .expect("state ok");
    let st = r.result.records[0].values.get("st").expect("st");
    match st {
        Value::Text(s) => {
            assert!(
                s.contains("last_refreshed_bucket_ns") && s.contains("bucket_count"),
                "unexpected state: {s}"
            );
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn drop_removes_aggregate_from_list() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "SELECT CA_REGISTER('ca2', 'metrics', '1m', 'c', 'sum', 'v') AS ok".into(),
    })
    .expect("register ok");
    q.execute(ExecuteQueryInput {
        query: "SELECT CA_DROP('ca2') AS ok".into(),
    })
    .expect("drop ok");
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT CA_LIST() AS names".into(),
        })
        .expect("list ok");
    let names = r.result.records[0].values.get("names").expect("names");
    match names {
        Value::Array(items) => assert!(items.is_empty()),
        other => panic!("expected empty array, got {other:?}"),
    }
}

#[test]
fn refresh_absorbs_rows_and_query_returns_aggregate() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);

    // Source collection with a `ts` (ns) column and a `load` float.
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE metrics (ts BIGINT, load DOUBLE)".into(),
    })
    .expect("create source ok");

    // Two rows in the first bucket (ts=0 .. 5_000_000_000), both load values folded into avg.
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO metrics (ts, load) VALUES (1000000000, 10.0)".into(),
    })
    .expect("insert row1");
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO metrics (ts, load) VALUES (2000000000, 20.0)".into(),
    })
    .expect("insert row2");

    // Register a 5-second bucket aggregate with lag 0 so every landed
    // row is eligible.
    q.execute(ExecuteQueryInput {
        query: "SELECT CA_REGISTER('avg_load', 'metrics', '5s', \
                'avg_load', 'avg', 'load', '0s', '365d') AS ok"
            .into(),
    })
    .expect("register ok");

    // Refresh with now_ns = 1 hour — both rows should land in bucket 0.
    let now_ns: i64 = 3_600_000_000_000;
    let r = q
        .execute(ExecuteQueryInput {
            query: format!("SELECT CA_REFRESH('avg_load', {now_ns}) AS absorbed"),
        })
        .expect("refresh ok");
    let absorbed = r.result.records[0]
        .values
        .get("absorbed")
        .expect("absorbed");
    assert!(
        matches!(absorbed, Value::Integer(n) if *n >= 2),
        "expected >= 2 rows absorbed, got {absorbed:?}"
    );

    // CA_QUERY at bucket 0 should return 15.0 (avg of 10 + 20).
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT CA_QUERY('avg_load', 0, 'avg_load') AS v".into(),
        })
        .expect("query ok");
    let v = r.result.records[0].values.get("v").expect("v");
    match v {
        Value::Float(f) => assert!(
            (*f - 15.0).abs() < 0.01,
            "expected ~15.0, got {f}"
        ),
        other => panic!("expected Float, got {other:?}"),
    }
}

#[test]
fn state_returns_null_for_unknown() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT CA_STATE('no_such_aggregate') AS st".into(),
        })
        .expect("state ok");
    let st = r.result.records[0].values.get("st").expect("st");
    assert!(matches!(st, Value::Null), "expected Null, got {st:?}");
}
