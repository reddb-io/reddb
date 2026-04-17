//! PostgreSQL type OID mapping (Phase 3.1 PG parity).
//!
//! PG clients identify column types by OID (see `pg_catalog.pg_type`).
//! RedDB's `Value` enum is richer than PG's canonical set, so we collapse
//! domain-specific variants (`Email`, `Phone`, `Money`, ...) onto their
//! closest PG equivalent — TEXT, NUMERIC, etc. This keeps generic clients
//! working; clients that need the fine-grained types call the native
//! gRPC path.
//!
//! Reference: PostgreSQL source `src/include/catalog/pg_type_d.h`.

use crate::storage::schema::Value;

/// A subset of PG type OIDs that cover every case we need to encode.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PgOid {
    Bool = 16,
    Bytea = 17,
    Int8 = 20,
    Int2 = 21,
    Int4 = 23,
    Text = 25,
    Oid = 26,
    Json = 114,
    Float4 = 700,
    Float8 = 701,
    Unknown = 705,
    Varchar = 1043,
    Date = 1082,
    Time = 1083,
    Timestamp = 1114,
    TimestampTz = 1184,
    Numeric = 1700,
    Uuid = 2950,
    Jsonb = 3802,
}

impl PgOid {
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    /// Preferred type OID for a runtime `Value`. Used by
    /// `RowDescription` to tell the client what each column is.
    pub fn from_value(value: &Value) -> Self {
        match value {
            Value::Null => PgOid::Text,
            Value::Boolean(_) => PgOid::Bool,
            Value::Integer(_) => PgOid::Int8,
            Value::UnsignedInteger(_) => PgOid::Int8,
            Value::BigInt(_) => PgOid::Int8,
            Value::Float(_) => PgOid::Float8,
            Value::Text(_) => PgOid::Text,
            Value::Blob(_) => PgOid::Bytea,
            Value::Json(_) => PgOid::Jsonb,
            Value::Uuid(_) => PgOid::Uuid,
            Value::Date(_) => PgOid::Date,
            Value::Timestamp(_) => PgOid::TimestampTz,
            Value::TimestampMs(_) => PgOid::TimestampTz,
            // Domain / richer types collapse to TEXT so psql can render them.
            _ => PgOid::Text,
        }
    }
}

/// Encode a `Value` as the UTF-8 text representation PG's text-mode
/// protocol expects. Binary format is opt-in via a flag in the client's
/// `Bind` message — we don't advertise binary support yet, so simple
/// text encoding is sufficient for every supported client.
///
/// Returns `None` for `Value::Null` (the caller emits a `-1` length).
pub fn value_to_pg_wire_bytes(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Null => None,
        Value::Boolean(b) => Some((if *b { "t" } else { "f" }).as_bytes().to_vec()),
        Value::Integer(n) => Some(n.to_string().into_bytes()),
        Value::UnsignedInteger(n) => Some(n.to_string().into_bytes()),
        Value::BigInt(n) => Some(n.to_string().into_bytes()),
        Value::Float(f) => Some(f.to_string().into_bytes()),
        Value::Text(s) => Some(s.as_bytes().to_vec()),
        Value::Blob(b) => {
            // PG bytea text format: `\xHEX...`. Two chars per byte.
            let mut out = Vec::with_capacity(2 + b.len() * 2);
            out.extend_from_slice(b"\\x");
            for byte in b {
                out.extend_from_slice(format!("{byte:02x}").as_bytes());
            }
            Some(out)
        }
        Value::Json(bytes) => Some(bytes.clone()),
        // Everything else renders via Display — catches Uuid, Date,
        // Timestamp, Email, Phone, Money, GeoPoint, etc. PG clients see
        // these as TEXT columns (OID 25) and can render them verbatim.
        other => Some(other.to_string().into_bytes()),
    }
}
