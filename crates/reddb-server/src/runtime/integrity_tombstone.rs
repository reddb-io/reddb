//! Issue #765 / S6 — opt-in SHA-256 integrity tombstones for input streams.
//!
//! Per ADR 0029 ("Integrity") and PRD #759: input streams support opt-in
//! end-to-end SHA-256. The client streams a rolling hash over the row
//! payloads and emits the expected digest in the terminal frame; the server
//! recomputes the same hash and compares. Because S4 commits per chunk, rows
//! are already durable when a mismatch is detected — rollback is impossible.
//! Instead the server marks the affected RID range with an **integrity
//! tombstone** in the collection metadata. Default reads filter tombstoned
//! RIDs out of result sets.
//!
//! This module owns the durable representation (a JSON list persisted under
//! a single `red_config` key) and the pure helpers the runtime + stream
//! handler use. The runtime caches the parsed ranges in-memory so the common
//! no-tombstone read path pays only a single relaxed atomic load.

use crate::storage::unified::UnifiedStore;
use crate::{RedDBError, RedDBResult};
use reddb_types::Value;

/// Single dot-notation key under which the whole tombstone list is stored.
/// [`load_ranges`] uses the fallible store-config reader, which resolves the
/// latest row by entity id and reads both resident and paged generations.
pub const TOMBSTONE_KEY: &str = "stream.integrity.tombstones";

/// Verification mode requested for an input stream. `None` is the default
/// (`stream.integrity.default_verify`) and incurs no hashing overhead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifyMode {
    #[default]
    None,
    Sha256,
}

impl VerifyMode {
    /// Parse the wire token. Unknown / empty values fall back to `None` so a
    /// malformed opt-in never terminates a stream that would otherwise run.
    pub fn parse(token: &str) -> VerifyMode {
        match token.trim().to_ascii_lowercase().as_str() {
            "sha256" => VerifyMode::Sha256,
            _ => VerifyMode::None,
        }
    }

    pub fn is_enabled(self) -> bool {
        matches!(self, VerifyMode::Sha256)
    }
}

/// One tombstoned RID range, inclusive on both ends, scoped to a collection.
/// RIDs (entity logical ids) are globally unique, but we still carry the
/// table so reads filter precisely and forensic tooling can attribute the
/// range to its origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneRange {
    pub table: String,
    pub lo: u64,
    pub hi: u64,
}

impl TombstoneRange {
    pub fn new(table: impl Into<String>, lo: u64, hi: u64) -> Self {
        Self {
            table: table.into(),
            lo,
            hi,
        }
    }

    /// True when `rid` falls inside this tombstoned range. RIDs (entity
    /// logical ids) are drawn from a single global counter and never reused,
    /// so a RID identifies exactly one row across the whole store — the read
    /// filter can therefore match on RID alone and stays correct even for
    /// projections that drop the `collection` system field. The `table` is
    /// retained for the error envelope and forensic attribution.
    pub fn covers_rid(&self, rid: u64) -> bool {
        self.lo <= rid && rid <= self.hi
    }
}

/// Serialize the range list to the compact JSON array persisted in
/// `red_config`. Table names are SQL identifiers (validated at OpenStream),
/// so the only escaping needed is the JSON string quote — kept explicit here
/// rather than pulling a serializer for a three-field record.
pub fn serialize_ranges(ranges: &[TombstoneRange]) -> String {
    let mut out = String::with_capacity(2 + ranges.len() * 40);
    out.push('[');
    for (i, r) in ranges.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"table\":\"");
        for ch in r.table.chars() {
            if ch == '"' || ch == '\\' {
                out.push('\\');
            }
            out.push(ch);
        }
        out.push_str(&format!("\",\"lo\":{},\"hi\":{}}}", r.lo, r.hi));
    }
    out.push(']');
    out
}

/// A malformed persisted list cannot safely be treated as an empty list:
/// that would expose rows already quarantined after a failed integrity check.
pub fn parse_ranges(json: &str) -> RedDBResult<Vec<TombstoneRange>> {
    let invalid = || RedDBError::Query("invalid persisted stream integrity tombstones".into());
    let value: crate::json::Value =
        crate::json::from_slice(json.as_bytes()).map_err(|_| invalid())?;
    let arr = value.as_array().ok_or_else(invalid)?;
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let table = entry
            .get("table")
            .and_then(crate::json::Value::as_str)
            .ok_or_else(invalid)?;
        let lo = entry
            .get("lo")
            .and_then(parse_range_bound)
            .ok_or_else(invalid)?;
        let hi = entry
            .get("hi")
            .and_then(parse_range_bound)
            .ok_or_else(invalid)?;
        if lo > hi {
            return Err(invalid());
        }
        out.push(TombstoneRange::new(table, lo, hi));
    }
    Ok(out)
}

// Persisted RID bounds are integer literals. Reject floating point values
// instead of truncating a fractional/corrupted bound into another row's ID.
fn parse_range_bound(value: &crate::json::Value) -> Option<u64> {
    match value {
        crate::json::Value::Integer(value) => u64::try_from(*value).ok(),
        crate::json::Value::Decimal(value) => value.parse().ok(),
        _ => None,
    }
}

/// Load the current persisted ranges, propagating paging, codec and I/O errors.
pub fn load_ranges(store: &UnifiedStore) -> RedDBResult<Vec<TombstoneRange>> {
    match store.try_get_config(TOMBSTONE_KEY)? {
        None => Ok(Vec::new()),
        Some(Value::Text(json)) => parse_ranges(&json),
        Some(_) => Err(RedDBError::Query(
            "invalid persisted stream integrity tombstones".into(),
        )),
    }
}

/// Persist the full range list through the ordinary WAL-ordered config path.
pub fn persist_ranges(store: &UnifiedStore, ranges: &[TombstoneRange]) -> RedDBResult<()> {
    let json = serialize_ranges(ranges);
    store.try_set_config_tree(TOMBSTONE_KEY, &crate::serde_json::Value::String(json))?;
    Ok(())
}

/// Extract a record's RID (logical entity id) as exposed by SELECT scans.
/// Mirrors the system-field convention in `record_search` where every
/// scanned record carries `rid` as an unsigned integer.
pub fn record_rid(record: &crate::storage::query::unified::UnifiedRecord) -> Option<u64> {
    match record.get("rid")? {
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        Value::UnsignedInteger(v) => Some(*v),
        _ => None,
    }
}

/// True when a record is tombstoned by any range, matched on RID alone
/// (RIDs are globally unique — see [`TombstoneRange::covers_rid`]). A record
/// without a resolvable RID is never filtered (fail-open on read — a scan
/// that cannot identify a row must not silently drop it).
pub fn record_tombstoned(
    ranges: &[TombstoneRange],
    record: &crate::storage::query::unified::UnifiedRecord,
) -> bool {
    let Some(rid) = record_rid(record) else {
        return false;
    };
    ranges.iter().any(|r| r.covers_rid(rid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_parse_round_trip() {
        let ranges = vec![
            TombstoneRange::new("orders", 10, 12),
            TombstoneRange::new("events", 5, 5),
        ];
        let json = serialize_ranges(&ranges);
        let parsed = parse_ranges(&json).expect("valid ranges");
        assert_eq!(parsed, ranges);
    }

    #[test]
    fn parse_rejects_malformed_entries_without_losing_other_ranges() {
        let json = r#"[{"table":"a","lo":1,"hi":3},{"table":"b","lo":4}]"#;
        assert!(parse_ranges(json).is_err());
        assert!(parse_ranges(r#"[{"table":"a","lo":3,"hi":1}]"#).is_err());
    }

    #[test]
    fn parse_garbage_is_an_error() {
        assert!(parse_ranges("not json").is_err());
        assert!(parse_ranges("{}").is_err());
        assert!(parse_ranges("[]").expect("empty list").is_empty());
    }

    #[test]
    fn covers_rid_is_inclusive() {
        let r = TombstoneRange::new("t", 4, 6);
        assert!(r.covers_rid(4));
        assert!(r.covers_rid(5));
        assert!(r.covers_rid(6));
        assert!(!r.covers_rid(3));
        assert!(!r.covers_rid(7));
    }

    #[test]
    fn verify_mode_parse_is_lenient() {
        assert_eq!(VerifyMode::parse("sha256"), VerifyMode::Sha256);
        assert_eq!(VerifyMode::parse("SHA256"), VerifyMode::Sha256);
        assert_eq!(VerifyMode::parse("none"), VerifyMode::None);
        assert_eq!(VerifyMode::parse(""), VerifyMode::None);
        assert_eq!(VerifyMode::parse("bogus"), VerifyMode::None);
        assert!(VerifyMode::Sha256.is_enabled());
        assert!(!VerifyMode::None.is_enabled());
    }

    #[test]
    fn table_name_with_quote_is_escaped() {
        // Identifiers are validated at OpenStream so this can't occur in
        // practice, but the serializer must not produce invalid JSON.
        let ranges = vec![TombstoneRange::new("a\"b", 1, 2)];
        let json = serialize_ranges(&ranges);
        let parsed = parse_ranges(&json).expect("valid ranges");
        assert_eq!(parsed, ranges);
    }
    #[test]
    fn integrity_exact_numeric_ranges_reject_fractional_and_overflowed_ids() {
        for rid in [(1_u64 << 53) + 1, (i64::MAX as u64) + 1, u64::MAX] {
            let expected = vec![TombstoneRange::new("events", rid, rid)];
            assert_eq!(
                parse_ranges(&serialize_ranges(&expected)).expect("exact integer bounds"),
                expected
            );
        }
        for invalid in [
            r#"[{"table":"events","lo":1.5,"hi":2}]"#,
            r#"[{"table":"events","lo":1,"hi":2.5}]"#,
            r#"[{"table":"events","lo":-1,"hi":2}]"#,
            r#"[{"table":"events","lo":1,"hi":18446744073709551616}]"#,
        ] {
            assert!(
                parse_ranges(invalid).is_err(),
                "invalid RID bounds: {invalid}"
            );
        }
    }
}
