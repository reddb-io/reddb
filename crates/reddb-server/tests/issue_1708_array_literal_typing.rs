//! Issue #1708 — array literals parse lossless; vector-vs-JSON typing resolves
//! from the target (PRD #1703, ADR 0067).
//!
//! A bare `[…]` literal used to commit to an f32 `Value::Vector` at parse time
//! whenever every element was numeric — silently corrupting large integers
//! destined for a JSON position long before the target type was known. The
//! parser now yields a lossless `Value::Array`, and the runtime resolves the
//! concrete shape from the target: a vector-typed position coerces to a
//! `Vec<f32>`, a JSON/KV position keeps the exact `Value::Array`.

use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

/// A JSON/KV position keeps every element's exact integer identity — including
/// `9007199254740993` (2^53 + 1), which cannot survive an f32 or f64 round-trip.
#[test]
fn json_position_array_round_trips_with_exact_integers() {
    let rt = runtime();

    rt.execute_query(
        "INSERT INTO settings KV (key, value) VALUES ('nums', [1, 2, 9007199254740993])",
    )
    .expect("insert array-valued kv pair");

    let read = rt
        .execute_query("SELECT key, value FROM settings WHERE key = 'nums'")
        .expect("read array-valued kv pair");

    let value = read.result.records[0]
        .get("value")
        .expect("value column present");

    assert_eq!(
        value,
        &Value::Array(vec![
            Value::Integer(1),
            Value::Integer(2),
            Value::Integer(9_007_199_254_740_993),
        ]),
        "large integers must survive the round-trip intact, not collapse to f32/f64"
    );
}

/// A mixed-type array (never a vector candidate) is preserved element-for-element
/// rather than being flattened through a lossy JSON encode at parse time.
#[test]
fn json_position_mixed_type_array_round_trips() {
    let rt = runtime();

    rt.execute_query(
        "INSERT INTO settings KV (key, value) VALUES ('mixed', ['a', 2, true, null])",
    )
    .expect("insert mixed-type array");

    let read = rt
        .execute_query("SELECT key, value FROM settings WHERE key = 'mixed'")
        .expect("read mixed-type array");

    let value = read.result.records[0]
        .get("value")
        .expect("value column present");

    assert_eq!(
        value,
        &Value::Array(vec![
            Value::text("a"),
            Value::Integer(2),
            Value::Boolean(true),
            Value::Null,
        ]),
    );
}

/// A numeric array literal at a vector-typed position still resolves to a
/// vector — existing vector-model flows stay green with unchanged syntax.
#[test]
fn vector_position_array_still_resolves_to_a_vector() {
    let rt = runtime();

    rt.execute_query("CREATE VECTOR v DIM 2 METRIC cosine")
        .expect("create vector collection");
    // Integer array literals (previously the corruption-prone case) resolve
    // cleanly to the vector column.
    rt.execute_query("INSERT INTO v VECTOR (embedding, content) VALUES ([1, 0], 'near')")
        .expect("insert integer-array vector");
    rt.execute_query("INSERT INTO v VECTOR (embedding, content) VALUES ([0, 1], 'far')")
        .expect("insert integer-array vector");

    let result = rt
        .execute_query("VECTOR SEARCH v SIMILAR TO [1, 0] LIMIT 1")
        .expect("vector search");

    let content = result.result.records[0]
        .get("content")
        .expect("content column present");
    assert_eq!(content, &Value::text("near"));
}
