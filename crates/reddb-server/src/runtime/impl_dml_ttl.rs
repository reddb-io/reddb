//! SQL TTL metadata canonicalization helpers extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1634). Names and behaviour are unchanged
//! from `impl_dml`; `impl_dml` re-exports these two `pub(super)` free functions
//! so both its own call sites and `impl_dml_support`'s
//! `use super::impl_dml::{...}` import keep resolving unchanged.

use crate::storage::unified::MetadataValue;

const SQL_TTL_METADATA_COLUMNS: [&str; 3] = ["_ttl", "_ttl_ms", "_expires_at"];

pub(super) fn resolve_sql_ttl_metadata_key(column: &str) -> Option<&'static str> {
    if column.eq_ignore_ascii_case("_ttl") {
        Some(SQL_TTL_METADATA_COLUMNS[0])
    } else if column.eq_ignore_ascii_case("_ttl_ms") {
        Some(SQL_TTL_METADATA_COLUMNS[1])
    } else if column.eq_ignore_ascii_case("_expires_at") {
        Some(SQL_TTL_METADATA_COLUMNS[2])
    } else {
        None
    }
}

/// Canonicalize a SQL TTL metadata `(key, value)` pair so the retention
/// sweeper sees a single key (`_ttl_ms`) regardless of which legacy form
/// the operator wrote. `_ttl` is scaled from seconds to milliseconds;
/// `_ttl_ms` and `_expires_at` are passed through.
pub(super) fn canonicalize_sql_ttl_metadata(
    key: &'static str,
    value: MetadataValue,
) -> (&'static str, MetadataValue) {
    if key != "_ttl" {
        return (key, value);
    }
    let scaled = match value {
        MetadataValue::Int(s) => MetadataValue::Int(s.saturating_mul(1_000)),
        MetadataValue::Timestamp(ms_or_s) => {
            // Timestamp is already chosen for very large values; treat as
            // already-ms to avoid silent overflow.
            MetadataValue::Timestamp(ms_or_s)
        }
        MetadataValue::Float(f) => MetadataValue::Float(f * 1_000.0),
        other => other,
    };
    ("_ttl_ms", scaled)
}
