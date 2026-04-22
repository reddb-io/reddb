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
