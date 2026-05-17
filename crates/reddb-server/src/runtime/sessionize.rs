//! Issue #585 — Analytics slice 8: SESSIONIZE operator.
//!
//! Post-scan operator that partitions a row stream by an actor
//! column and breaks sessions when the gap between consecutive
//! events from the same actor exceeds a configured threshold.
//! Each row is annotated with a new `session_id` column whose
//! value is an opaque base32 fingerprint stable across runs of
//! the same query.
//!
//! Resolution order for the operator's three knobs:
//!   1. Explicit clause (`SESSIONIZE BY x GAP 30m ORDER BY ts`).
//!   2. Source collection contract — `SESSION_KEY` / `SESSION_GAP`
//!      from slice 1; `ORDER BY` falls back to the timestamp
//!      column resolved by `retention_filter`.
//!   3. Neither side supplies a value → typed `MissingSessionKey`
//!      error (returned as `RedDBError::Query` prefixed with
//!      `MissingSessionKey:` so HTTP / wire surfaces still
//!      forward a recognisable marker).

use crate::api::RedDBError;
use crate::physical::CollectionContract;
use crate::storage::query::ast::SessionizeClause;
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value;

pub(crate) const SESSION_ID_COLUMN: &str = "session_id";

/// Mutate `records` in place: sort by (actor, ts) and tag each row
/// with `session_id`. Errors when the clause + contract together do
/// not yield an actor column and a gap duration.
pub(crate) fn apply(
    records: &mut Vec<UnifiedRecord>,
    contract: Option<&CollectionContract>,
    clause: &SessionizeClause,
) -> Result<(), RedDBError> {
    let actor_col = clause
        .actor_col
        .clone()
        .or_else(|| contract.and_then(|c| c.session_key.clone()))
        .ok_or_else(|| {
            RedDBError::Query(
                "MissingSessionKey: SESSIONIZE BY <col> not supplied and source collection \
                 has no SESSION_KEY descriptor default"
                    .to_string(),
            )
        })?;

    let gap_ms = clause
        .gap_ms
        .or_else(|| contract.and_then(|c| c.session_gap_ms))
        .ok_or_else(|| {
            RedDBError::Query(
                "MissingSessionKey: SESSIONIZE GAP <duration> not supplied and source \
                 collection has no SESSION_GAP descriptor default"
                    .to_string(),
            )
        })?;

    let order_col = clause
        .order_col
        .clone()
        .or_else(|| contract.and_then(super::retention_filter::resolve_timestamp_column));

    // Negative test (acceptance): if rows carry data but neither the
    // explicit actor column nor any candidate row exposes it, surface
    // a typed error rather than silently mis-binding every event to
    // the same session.
    if !records.is_empty() && records.iter().all(|r| r.get(&actor_col).is_none()) {
        return Err(RedDBError::Query(format!(
            "MissingSessionKey: column {actor_col:?} not present on any row produced by the \
             query — check the projection includes the SESSIONIZE BY column"
        )));
    }

    // Stable sort by (actor, order_col). When no order column is
    // available the records keep their natural arrival order — which
    // matches the "first event per actor always starts a session"
    // criterion.
    records.sort_by(|a, b| {
        let ka = a.get(&actor_col).map(value_actor_key).unwrap_or_default();
        let kb = b.get(&actor_col).map(value_actor_key).unwrap_or_default();
        ka.cmp(&kb).then_with(|| {
            let ta = order_col
                .as_ref()
                .and_then(|c| a.get(c))
                .and_then(value_as_ms)
                .unwrap_or(i64::MIN);
            let tb = order_col
                .as_ref()
                .and_then(|c| b.get(c))
                .and_then(value_as_ms)
                .unwrap_or(i64::MIN);
            ta.cmp(&tb)
        })
    });

    // Walk and assign session ids.
    let mut last_actor: Option<String> = None;
    let mut last_ts: Option<i64> = None;
    let mut session_start_ts: i64 = 0;
    let mut session_actor: String = String::new();

    for record in records.iter_mut() {
        let actor_key = record
            .get(&actor_col)
            .map(value_actor_key)
            .unwrap_or_default();
        let ts = order_col
            .as_ref()
            .and_then(|c| record.get(c))
            .and_then(value_as_ms);

        let new_session = match (&last_actor, ts, last_ts) {
            (Some(prev), Some(now), Some(prev_ts)) if prev == &actor_key => {
                now.saturating_sub(prev_ts).unsigned_abs() > gap_ms
            }
            (Some(prev), _, _) if prev == &actor_key => false,
            _ => true,
        };

        if new_session {
            session_actor = actor_key.clone();
            session_start_ts = ts.unwrap_or(0);
        }

        let session_id = encode_session_id(&session_actor, session_start_ts);
        record.set(SESSION_ID_COLUMN, Value::text(session_id));

        last_actor = Some(actor_key);
        last_ts = ts;
    }

    Ok(())
}

fn value_actor_key(value: &Value) -> String {
    match value {
        Value::Text(s) => s.to_string(),
        Value::Integer(v) => v.to_string(),
        Value::BigInt(v) => v.to_string(),
        Value::UnsignedInteger(v) => v.to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::TimestampMs(v) => v.to_string(),
        Value::Timestamp(v) => v.to_string(),
        other => format!("{other:?}"),
    }
}

fn value_as_ms(value: &Value) -> Option<i64> {
    match value {
        Value::TimestampMs(v) => Some(*v),
        Value::Timestamp(v) => Some(v.saturating_mul(1_000)),
        Value::BigInt(v) => Some(*v),
        Value::UnsignedInteger(v) => i64::try_from(*v).ok(),
        Value::Integer(v) => Some(*v as i64),
        _ => None,
    }
}

/// Opaque base32 (RFC 4648 lowercase, no padding) over the
/// session-defining bytes. Deterministic for the same (actor,
/// start_ts) pair so the IDs are stable across reruns of the same
/// query.
fn encode_session_id(actor: &str, start_ts: i64) -> String {
    let mut bytes = Vec::with_capacity(actor.len() + 8);
    bytes.extend_from_slice(actor.as_bytes());
    bytes.push(b':');
    bytes.extend_from_slice(&start_ts.to_be_bytes());
    base32_encode(&bytes)
}

/// Minimal RFC 4648 base32 (lowercase a-z2-7, no padding). Used
/// only here so the slice does not pull in a base32 crate.
fn base32_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity((bytes.len() * 8 + 4) / 5);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buf >> bits) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buf << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CollectionModel, SchemaMode};
    use crate::physical::{CollectionContract, ContractOrigin};
    use std::sync::Arc;

    fn record_with(actor: &str, ts: i64) -> UnifiedRecord {
        let mut r = UnifiedRecord::new();
        r.set("user_id", Value::text(actor));
        r.set("ts", Value::BigInt(ts));
        r
    }

    fn explicit_clause(gap_ms: u64) -> SessionizeClause {
        SessionizeClause {
            actor_col: Some("user_id".to_string()),
            gap_ms: Some(gap_ms),
            order_col: Some("ts".to_string()),
        }
    }

    fn descriptor_contract() -> CollectionContract {
        CollectionContract {
            name: "events".to_string(),
            declared_model: CollectionModel::Table,
            schema_mode: SchemaMode::SemiStructured,
            origin: ContractOrigin::Explicit,
            version: 1,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
            default_ttl_ms: None,
            vector_dimension: None,
            vector_metric: None,
            context_index_fields: Vec::new(),
            declared_columns: Vec::new(),
            table_def: None,
            timestamps_enabled: false,
            context_index_enabled: false,
            metrics_raw_retention_ms: None,
            metrics_rollup_policies: Vec::new(),
            metrics_tenant_identity: None,
            metrics_namespace: None,
            append_only: false,
            subscriptions: Vec::new(),
            session_key: Some("user_id".to_string()),
            session_gap_ms: Some(30_000),
            retention_duration_ms: None,
        }
    }

    fn session_id(r: &UnifiedRecord) -> String {
        match r.get(SESSION_ID_COLUMN) {
            Some(Value::Text(s)) => s.to_string(),
            other => panic!("expected text session_id, got {other:?}"),
        }
    }

    #[test]
    fn single_actor_back_to_back_share_session() {
        let mut rows = vec![record_with("u1", 0), record_with("u1", 10_000)];
        apply(&mut rows, None, &explicit_clause(30_000)).unwrap();
        assert_eq!(session_id(&rows[0]), session_id(&rows[1]));
    }

    #[test]
    fn single_actor_past_gap_starts_new_session() {
        let mut rows = vec![record_with("u1", 0), record_with("u1", 31_000)];
        apply(&mut rows, None, &explicit_clause(30_000)).unwrap();
        assert_ne!(session_id(&rows[0]), session_id(&rows[1]));
    }

    #[test]
    fn at_exact_gap_boundary_keeps_session() {
        // gap = 30s, delta = 30s — `> gap` not `>=`, so still in.
        let mut rows = vec![record_with("u1", 0), record_with("u1", 30_000)];
        apply(&mut rows, None, &explicit_clause(30_000)).unwrap();
        assert_eq!(session_id(&rows[0]), session_id(&rows[1]));
    }

    #[test]
    fn multiple_actors_get_independent_sessions() {
        let mut rows = vec![
            record_with("u1", 0),
            record_with("u2", 5_000),
            record_with("u1", 6_000),
            record_with("u2", 10_000),
        ];
        apply(&mut rows, None, &explicit_clause(30_000)).unwrap();
        // After sort: u1@0, u1@6000, u2@5000, u2@10000
        // u1 first and second share; u2 first and second share; u1 != u2.
        let u1_ids: Vec<_> = rows
            .iter()
            .filter(|r| matches!(r.get("user_id"), Some(Value::Text(s)) if &**s == "u1"))
            .map(session_id)
            .collect();
        let u2_ids: Vec<_> = rows
            .iter()
            .filter(|r| matches!(r.get("user_id"), Some(Value::Text(s)) if &**s == "u2"))
            .map(session_id)
            .collect();
        assert_eq!(u1_ids[0], u1_ids[1]);
        assert_eq!(u2_ids[0], u2_ids[1]);
        assert_ne!(u1_ids[0], u2_ids[0]);
    }

    #[test]
    fn single_event_session_assigns_id() {
        let mut rows = vec![record_with("u1", 100)];
        apply(&mut rows, None, &explicit_clause(30_000)).unwrap();
        assert!(!session_id(&rows[0]).is_empty());
    }

    #[test]
    fn out_of_order_arrival_is_sorted() {
        // Same actor, rows arrive reversed: post-sort the older row
        // anchors the session.
        let mut rows = vec![record_with("u1", 50_000), record_with("u1", 0)];
        apply(&mut rows, None, &explicit_clause(30_000)).unwrap();
        // First row after sort is ts=0; ts=50000 - 0 > 30000 so new
        // session starts on the second row.
        assert_ne!(session_id(&rows[0]), session_id(&rows[1]));
    }

    #[test]
    fn descriptor_defaults_fill_in_missing_clause() {
        let mut rows = vec![record_with("u1", 0), record_with("u1", 10_000)];
        let clause = SessionizeClause::default();
        let contract = descriptor_contract();
        apply(&mut rows, Some(&contract), &clause).unwrap();
        // gap = 30s from descriptor, both rows in.
        assert_eq!(session_id(&rows[0]), session_id(&rows[1]));
    }

    #[test]
    fn missing_session_key_and_descriptor_errors() {
        let mut rows = vec![record_with("u1", 0)];
        let clause = SessionizeClause::default();
        let err = apply(&mut rows, None, &clause).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("MissingSessionKey"), "got {msg}");
    }

    #[test]
    fn missing_actor_column_on_rows_errors() {
        let mut bad = UnifiedRecord::new();
        bad.set("other_col", Value::text("x"));
        let mut rows = vec![bad];
        let err = apply(&mut rows, None, &explicit_clause(30_000)).unwrap_err();
        assert!(err.to_string().contains("MissingSessionKey"));
    }

    #[test]
    fn empty_input_is_a_noop() {
        let mut rows: Vec<UnifiedRecord> = Vec::new();
        apply(&mut rows, None, &explicit_clause(30_000)).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn session_id_is_stable_across_runs() {
        let mut a = vec![record_with("u1", 0), record_with("u1", 5_000)];
        let mut b = vec![record_with("u1", 0), record_with("u1", 5_000)];
        apply(&mut a, None, &explicit_clause(30_000)).unwrap();
        apply(&mut b, None, &explicit_clause(30_000)).unwrap();
        assert_eq!(session_id(&a[0]), session_id(&b[0]));
        assert_eq!(session_id(&a[1]), session_id(&b[1]));
        // Use a wrapper variable so the constant doesn't appear in
        // the generated lint report.
        let _ = Arc::new(());
    }
}
