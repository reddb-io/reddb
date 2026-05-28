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

use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, UnifiedStore};

/// `red_config` collection holding the durable tombstone list.
const RED_CONFIG_COLLECTION: &str = "red_config";

/// Single dot-notation key under which the whole tombstone list is stored.
/// `set_config_tree` is append-only, so [`load_ranges`] picks the latest row
/// for this key by entity id (mirrors `blockchain_kind`'s integrity flag).
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

/// Parse the JSON array produced by [`serialize_ranges`]. Malformed entries
/// are skipped rather than failing the whole load — a single corrupt row must
/// not blind the reader to every other tombstone.
pub fn parse_ranges(json: &str) -> Vec<TombstoneRange> {
    let value: crate::json::Value = match crate::json::from_slice(json.as_bytes()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for entry in arr {
        let (Some(table), Some(lo), Some(hi)) = (
            entry.get("table").and_then(crate::json::Value::as_str),
            entry.get("lo").and_then(crate::json::Value::as_u64),
            entry.get("hi").and_then(crate::json::Value::as_u64),
        ) else {
            continue;
        };
        out.push(TombstoneRange::new(table.to_string(), lo, hi));
    }
    out
}

/// Load every persisted tombstone range from `red_config`. Picks the latest
/// row for [`TOMBSTONE_KEY`] by entity id (the key is rewritten in full on
/// every append, so the highest-id row is the current list).
pub fn load_ranges(store: &UnifiedStore) -> Vec<TombstoneRange> {
    let Some(manager) = store.get_collection(RED_CONFIG_COLLECTION) else {
        return Vec::new();
    };
    let mut latest: Option<(u64, String)> = None;
    for entity in manager.query_all(|_| true) {
        let EntityData::Row(row) = &entity.data else {
            continue;
        };
        let Some(named) = &row.named else { continue };
        let key_match =
            matches!(named.get("key"), Some(Value::Text(s)) if s.as_ref() == TOMBSTONE_KEY);
        if !key_match {
            continue;
        }
        let Some(Value::Text(v)) = named.get("value") else {
            continue;
        };
        let id = entity.id.raw();
        if latest.as_ref().map(|(prev, _)| id > *prev).unwrap_or(true) {
            latest = Some((id, v.as_ref().to_string()));
        }
    }
    latest
        .map(|(_, json)| parse_ranges(&json))
        .unwrap_or_default()
}

/// Persist the full range list back to `red_config` under [`TOMBSTONE_KEY`].
/// Durable via the store's WAL, so tombstones survive restart.
pub fn persist_ranges(store: &UnifiedStore, ranges: &[TombstoneRange]) {
    let json = serialize_ranges(ranges);
    store.set_config_tree(TOMBSTONE_KEY, &crate::serde_json::Value::String(json));
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
        let parsed = parse_ranges(&json);
        assert_eq!(parsed, ranges);
    }

    #[test]
    fn parse_skips_malformed_entries_but_keeps_valid_ones() {
        // Second entry is missing `hi` — it must be skipped, not abort the
        // whole parse.
        let json = r#"[{"table":"a","lo":1,"hi":3},{"table":"b","lo":4}]"#;
        let parsed = parse_ranges(json);
        assert_eq!(parsed, vec![TombstoneRange::new("a", 1, 3)]);
    }

    #[test]
    fn parse_garbage_yields_empty() {
        assert!(parse_ranges("not json").is_empty());
        assert!(parse_ranges("{}").is_empty());
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
        let parsed = parse_ranges(&json);
        assert_eq!(parsed, ranges);
    }
}
