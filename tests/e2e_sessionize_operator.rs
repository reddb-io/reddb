//! Issue #585 — Analytics slice 8: SESSIONIZE operator.
//!
//! End-to-end coverage for `SELECT ... FROM <collection> SESSIONIZE
//! BY <ident> GAP <duration> [ORDER BY <ident>]`. The operator emits
//! an opaque `session_id` column whose values are stable for the
//! same row sequence; consecutive events from the same actor within
//! `gap` share an id, events further apart get a fresh id, and the
//! first event per actor always starts a session.

use reddb::application::ExecuteQueryInput;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};
use std::collections::HashSet;

fn session_id(row: &reddb::storage::query::unified::UnifiedRecord) -> String {
    match row.get("session_id").expect("session_id column") {
        Value::Text(s) => s.to_string(),
        other => panic!("expected text session_id, got {other:?}"),
    }
}

fn id_int(row: &reddb::storage::query::unified::UnifiedRecord) -> i64 {
    match row.get("id").expect("id column") {
        Value::Integer(v) => *v,
        Value::BigInt(v) => *v,
        Value::UnsignedInteger(v) => *v as i64,
        other => panic!("expected integer id, got {other:?}"),
    }
}

fn setup_events() -> RedDBRuntime {
    let rt = RedDBRuntime::in_memory().expect("in-memory runtime");
    let q = QueryUseCases::new(&rt);
    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE events (id INTEGER, user_id TEXT, ts BIGINT)".into(),
    })
    .expect("create events table");
    rt
}

fn insert(rt: &RedDBRuntime, id: i64, user: &str, ts: i64) {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: format!("INSERT INTO events (id, user_id, ts) VALUES ({id}, '{user}', {ts})"),
        })
        .expect("insert event");
}

#[test]
fn single_actor_back_to_back_shares_session_id() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 0);
    insert(&rt, 2, "u1", 5_000);
    insert(&rt, 3, "u1", 10_000);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, user_id, ts, session_id FROM events \
                    SESSIONIZE BY user_id GAP 30 s ORDER BY ts"
                .into(),
        })
        .expect("sessionize select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 3);
    let s0 = session_id(&rows[0]);
    let s1 = session_id(&rows[1]);
    let s2 = session_id(&rows[2]);
    assert_eq!(s0, s1, "consecutive events within gap share session");
    assert_eq!(s1, s2);
}

#[test]
fn gap_boundary_break_starts_new_session() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 0);
    insert(&rt, 2, "u1", 60_000); // 60s after first — > 30s gap

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, session_id FROM events \
                    SESSIONIZE BY user_id GAP 30 s ORDER BY ts"
                .into(),
        })
        .expect("sessionize select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 2);
    assert_ne!(
        session_id(&rows[0]),
        session_id(&rows[1]),
        "events past the gap get distinct session ids"
    );
}

#[test]
fn multiple_actors_get_independent_sessions() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 0);
    insert(&rt, 2, "u2", 5_000);
    insert(&rt, 3, "u1", 6_000);
    insert(&rt, 4, "u2", 10_000);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, user_id, session_id FROM events \
                    SESSIONIZE BY user_id GAP 30 s ORDER BY ts"
                .into(),
        })
        .expect("sessionize select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 4);

    let mut by_user: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for row in rows {
        let user = match row.get("user_id").expect("user_id col") {
            Value::Text(s) => s.to_string(),
            other => panic!("expected text user_id, got {other:?}"),
        };
        by_user.entry(user).or_default().push(session_id(row));
    }

    assert_eq!(by_user["u1"][0], by_user["u1"][1], "u1's two events share");
    assert_eq!(by_user["u2"][0], by_user["u2"][1], "u2's two events share");
    assert_ne!(by_user["u1"][0], by_user["u2"][0], "different actors split");
}

#[test]
fn single_event_session_assigns_an_id() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 100);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, session_id FROM events \
                    SESSIONIZE BY user_id GAP 30 s ORDER BY ts"
                .into(),
        })
        .expect("sessionize select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 1);
    assert!(!session_id(&rows[0]).is_empty());
}

#[test]
fn out_of_order_arrival_within_query_is_sorted() {
    let rt = setup_events();
    // Insert in shuffled order; SESSIONIZE must sort by ts before
    // assigning session ids.
    insert(&rt, 1, "u1", 50_000);
    insert(&rt, 2, "u1", 0);
    insert(&rt, 3, "u1", 100_000);
    insert(&rt, 4, "u1", 25_000);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, ts, session_id FROM events \
                    SESSIONIZE BY user_id GAP 30 s ORDER BY ts"
                .into(),
        })
        .expect("sessionize select");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 4);
    // After post-sort: ts = 0, 25000, 50000, 100000.
    // 0 -> session A; 25000 - 0 = 25000 <= 30000 -> still A;
    // 50000 - 25000 = 25000 <= 30000 -> still A;
    // 100000 - 50000 = 50000 > 30000 -> session B.
    let s: Vec<_> = rows.iter().map(session_id).collect();
    assert_eq!(s[0], s[1]);
    assert_eq!(s[1], s[2]);
    assert_ne!(s[2], s[3]);
}

// NOTE: descriptor-default resolution (omit BY/GAP, descriptor
// supplies the values) is covered exhaustively in the unit test
// `runtime::sessionize::tests::descriptor_defaults_fill_in_missing_clause`.
// An e2e variant would require the slice-1 descriptor to live on a
// collection that also accepts arbitrary user-id-like columns at
// INSERT time, which TIMESERIES (the only place the descriptor can
// be declared today) does not — its column whitelist is
// metric/value/tags/timestamp/timestamp_ns/time. Leaving the e2e
// shape to whichever slice opens descriptor declaration on
// non-timeseries collections.

#[test]
fn missing_descriptor_and_clause_yields_typed_error() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 0);

    let err = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            // No BY, no GAP, no descriptor on the table — must error.
            query: "SELECT id, session_id FROM events SESSIONIZE ORDER BY ts".into(),
        })
        .expect_err("expected MissingSessionKey error");
    let msg = format!("{err}");
    assert!(
        msg.contains("MissingSessionKey"),
        "error should be tagged MissingSessionKey, got: {msg}"
    );
}

#[test]
fn missing_actor_column_in_projection_yields_typed_error() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 0);

    // Project only id+ts, omit user_id — operator can't find the
    // actor column on any row, must error.
    let err = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "SELECT id, ts, session_id FROM events \
                    SESSIONIZE BY missing_col GAP 30 s ORDER BY ts"
                .into(),
        })
        .expect_err("expected MissingSessionKey error for absent column");
    let msg = format!("{err}");
    assert!(
        msg.contains("MissingSessionKey"),
        "error should be tagged MissingSessionKey, got: {msg}"
    );
}

#[test]
fn composes_with_where_filter_before_sessionize() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 0);
    insert(&rt, 2, "u1", 5_000);
    insert(&rt, 3, "u2", 1_000);
    insert(&rt, 4, "u2", 7_000);

    let res = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            // WHERE applies before the operator — only u1's rows
            // reach SESSIONIZE.
            query: "SELECT id, user_id, session_id FROM events \
                    SESSIONIZE BY user_id GAP 30 s ORDER BY ts \
                    WHERE user_id = 'u1'"
                .into(),
        })
        .expect("where + sessionize compose");
    let rows = &res.result.records;
    assert_eq!(rows.len(), 2);
    let ids: HashSet<i64> = rows.iter().map(id_int).collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    // Both u1 rows fall inside the same session.
    assert_eq!(session_id(&rows[0]), session_id(&rows[1]));
}

#[test]
fn session_ids_are_stable_across_repeated_queries() {
    let rt = setup_events();
    insert(&rt, 1, "u1", 0);
    insert(&rt, 2, "u1", 5_000);

    let run = || -> Vec<String> {
        let res = QueryUseCases::new(&rt)
            .execute(ExecuteQueryInput {
                query: "SELECT id, session_id FROM events \
                        SESSIONIZE BY user_id GAP 30 s ORDER BY ts"
                    .into(),
            })
            .expect("sessionize select");
        res.result.records.iter().map(session_id).collect()
    };

    let first = run();
    let second = run();
    assert_eq!(first, second, "session ids must be stable across runs");
}
