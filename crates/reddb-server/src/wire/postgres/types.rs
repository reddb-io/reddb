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
    /// RedDB-reserved synthetic vector OID. PostgreSQL extension OIDs are
    /// cluster-local; RedDB uses a stable high value for wire clients that
    /// want to bind vector parameters explicitly.
    Vector = 38000,
}

impl PgOid {
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    pub fn from_u32(oid: u32) -> Self {
        match oid {
            16 => PgOid::Bool,
            17 => PgOid::Bytea,
            20 => PgOid::Int8,
            21 => PgOid::Int2,
            23 => PgOid::Int4,
            25 => PgOid::Text,
            26 => PgOid::Oid,
            114 => PgOid::Json,
            700 => PgOid::Float4,
            701 => PgOid::Float8,
            705 => PgOid::Unknown,
            1043 => PgOid::Varchar,
            1082 => PgOid::Date,
            1083 => PgOid::Time,
            1114 => PgOid::Timestamp,
            1184 => PgOid::TimestampTz,
            1700 => PgOid::Numeric,
            2950 => PgOid::Uuid,
            3802 => PgOid::Jsonb,
            38000 => PgOid::Vector,
            _ => PgOid::Unknown,
        }
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
            Value::Vector(_) => PgOid::Vector,
            // Domain / richer types collapse to TEXT so psql can render them.
            _ => PgOid::Text,
        }
    }
}

pub fn pg_param_to_value(
    oid: u32,
    format_code: i16,
    bytes: Option<&[u8]>,
) -> Result<Value, String> {
    let Some(bytes) = bytes else {
        return Ok(Value::Null);
    };
    match format_code {
        0 => pg_text_param_to_value(PgOid::from_u32(oid), bytes),
        1 => pg_binary_param_to_value(PgOid::from_u32(oid), bytes),
        other => Err(format!("unsupported PG parameter format code {other}")),
    }
}

fn pg_text_param_to_value(oid: PgOid, bytes: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("invalid UTF-8 parameter: {e}"))?;
    match oid {
        PgOid::Bool => match text.to_ascii_lowercase().as_str() {
            "t" | "true" | "1" | "yes" | "on" => Ok(Value::Boolean(true)),
            "f" | "false" | "0" | "no" | "off" => Ok(Value::Boolean(false)),
            _ => Err(format!("invalid bool parameter {text:?}")),
        },
        PgOid::Int2 | PgOid::Int4 | PgOid::Int8 | PgOid::Oid => text
            .parse::<i64>()
            .map(Value::Integer)
            .map_err(|e| format!("invalid integer parameter {text:?}: {e}")),
        PgOid::Float4 | PgOid::Float8 | PgOid::Numeric => text
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|e| format!("invalid float parameter {text:?}: {e}")),
        PgOid::Bytea => parse_bytea_text(text).map(Value::Blob),
        PgOid::Json | PgOid::Jsonb => Ok(Value::Json(bytes.to_vec())),
        PgOid::Timestamp | PgOid::TimestampTz => text
            .parse::<i64>()
            .map(Value::Timestamp)
            .or_else(|_| Ok(Value::Text(std::sync::Arc::from(text)))),
        PgOid::Uuid => parse_uuid_text(text).map(Value::Uuid),
        PgOid::Vector => parse_vector_text(text).map(Value::Vector),
        PgOid::Text | PgOid::Varchar | PgOid::Unknown | PgOid::Date | PgOid::Time => {
            Ok(Value::Text(std::sync::Arc::from(text)))
        }
    }
}

fn pg_binary_param_to_value(oid: PgOid, bytes: &[u8]) -> Result<Value, String> {
    match oid {
        PgOid::Bool if bytes.len() == 1 => Ok(Value::Boolean(bytes[0] != 0)),
        PgOid::Int2 if bytes.len() == 2 => {
            Ok(Value::Integer(
                i16::from_be_bytes([bytes[0], bytes[1]]) as i64
            ))
        }
        PgOid::Int4 | PgOid::Oid if bytes.len() == 4 => {
            Ok(Value::Integer(
                i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as i64,
            ))
        }
        PgOid::Int8 if bytes.len() == 8 => Ok(Value::Integer(i64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))),
        PgOid::Float4 if bytes.len() == 4 => {
            Ok(Value::Float(
                f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64,
            ))
        }
        PgOid::Float8 if bytes.len() == 8 => Ok(Value::Float(f64::from_be_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))),
        PgOid::Bytea => Ok(Value::Blob(bytes.to_vec())),
        PgOid::Json | PgOid::Jsonb => Ok(Value::Json(bytes.to_vec())),
        PgOid::Uuid if bytes.len() == 16 => {
            let mut out = [0u8; 16];
            out.copy_from_slice(bytes);
            Ok(Value::Uuid(out))
        }
        PgOid::Timestamp | PgOid::TimestampTz if bytes.len() == 8 => Ok(Value::Timestamp(
            pg_microseconds_to_unix_seconds(i64::from_be_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])),
        )),
        PgOid::Vector => parse_vector_binary(bytes).map(Value::Vector),
        _ => Err(format!(
            "unsupported binary parameter for OID {} with {} bytes",
            oid.as_u32(),
            bytes.len()
        )),
    }
}

fn parse_bytea_text(text: &str) -> Result<Vec<u8>, String> {
    let Some(hex) = text.strip_prefix("\\x") else {
        return Ok(text.as_bytes().to_vec());
    };
    if hex.len() % 2 != 0 {
        return Err("invalid bytea hex length".to_string());
    }
    (0..hex.len())
        .step_by(2)
        .map(|idx| {
            u8::from_str_radix(&hex[idx..idx + 2], 16)
                .map_err(|e| format!("invalid bytea hex: {e}"))
        })
        .collect()
}

fn parse_vector_text(text: &str) -> Result<Vec<f32>, String> {
    let parsed: crate::json::Value =
        crate::json::from_str(text).map_err(|e| format!("invalid vector parameter: {e}"))?;
    let crate::json::Value::Array(items) = parsed else {
        return Err("invalid vector parameter: expected JSON number array".to_string());
    };
    items
        .iter()
        .map(|item| {
            item.as_f64().map(|value| value as f32).ok_or_else(|| {
                "invalid vector parameter: array must contain only numbers".to_string()
            })
        })
        .collect()
}

fn parse_vector_binary(bytes: &[u8]) -> Result<Vec<f32>, String> {
    if bytes.len() < 4 {
        return Err("invalid binary vector parameter: payload too short".to_string());
    }
    let dims = i16::from_be_bytes([bytes[0], bytes[1]]);
    if dims < 0 {
        return Err("invalid binary vector parameter: negative dimension".to_string());
    }
    let dims = dims as usize;
    let expected = 4 + dims * 4;
    if bytes.len() != expected {
        return Err(format!(
            "invalid binary vector parameter: expected {expected} bytes, got {}",
            bytes.len()
        ));
    }
    (0..dims)
        .map(|idx| {
            let off = 4 + idx * 4;
            Ok(f32::from_be_bytes([
                bytes[off],
                bytes[off + 1],
                bytes[off + 2],
                bytes[off + 3],
            ]))
        })
        .collect()
}

fn parse_uuid_text(text: &str) -> Result<[u8; 16], String> {
    let compact = text.replace('-', "");
    if compact.len() != 32 {
        return Err(format!("invalid uuid parameter {text:?}"));
    }
    let bytes = (0..compact.len())
        .step_by(2)
        .map(|idx| {
            u8::from_str_radix(&compact[idx..idx + 2], 16)
                .map_err(|e| format!("invalid uuid parameter {text:?}: {e}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut out = [0u8; 16];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn pg_microseconds_to_unix_seconds(pg_micros: i64) -> i64 {
    // PostgreSQL binary timestamps are microseconds since 2000-01-01.
    const PG_UNIX_EPOCH_OFFSET_SECONDS: i64 = 946_684_800;
    PG_UNIX_EPOCH_OFFSET_SECONDS + pg_micros / 1_000_000
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_text_params_decode_common_oids() {
        assert_eq!(
            pg_param_to_value(PgOid::Bool.as_u32(), 0, Some(b"t")).unwrap(),
            Value::Boolean(true)
        );
        assert_eq!(
            pg_param_to_value(PgOid::Int4.as_u32(), 0, Some(b"42")).unwrap(),
            Value::Integer(42)
        );
        assert_eq!(
            pg_param_to_value(PgOid::Float8.as_u32(), 0, Some(b"1.5")).unwrap(),
            Value::Float(1.5)
        );
        assert_eq!(
            pg_param_to_value(PgOid::Bytea.as_u32(), 0, Some(br"\xdeadbeef")).unwrap(),
            Value::Blob(vec![0xde, 0xad, 0xbe, 0xef])
        );
        assert_eq!(
            pg_param_to_value(PgOid::Jsonb.as_u32(), 0, Some(br#"{"a":1}"#)).unwrap(),
            Value::Json(br#"{"a":1}"#.to_vec())
        );
        assert_eq!(
            pg_param_to_value(
                PgOid::Uuid.as_u32(),
                0,
                Some(b"00112233-4455-6677-8899-aabbccddeeff")
            )
            .unwrap(),
            Value::Uuid([
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ])
        );
    }

    #[test]
    fn pg_binary_params_decode_numeric_and_uuid_oids() {
        assert_eq!(
            pg_param_to_value(PgOid::Int2.as_u32(), 1, Some(&7i16.to_be_bytes())).unwrap(),
            Value::Integer(7)
        );
        assert_eq!(
            pg_param_to_value(PgOid::Int8.as_u32(), 1, Some(&42i64.to_be_bytes())).unwrap(),
            Value::Integer(42)
        );
        assert_eq!(
            pg_param_to_value(PgOid::Float4.as_u32(), 1, Some(&1.5f32.to_be_bytes())).unwrap(),
            Value::Float(1.5)
        );
        assert_eq!(
            pg_param_to_value(PgOid::Uuid.as_u32(), 1, Some(&[0x11; 16])).unwrap(),
            Value::Uuid([0x11; 16])
        );
        let mut vector = Vec::new();
        vector.extend_from_slice(&2i16.to_be_bytes());
        vector.extend_from_slice(&0i16.to_be_bytes());
        vector.extend_from_slice(&1.0f32.to_be_bytes());
        vector.extend_from_slice(&(-0.5f32).to_be_bytes());
        assert_eq!(
            pg_param_to_value(PgOid::Vector.as_u32(), 1, Some(&vector)).unwrap(),
            Value::Vector(vec![1.0, -0.5])
        );
    }

    #[test]
    fn pg_null_param_decodes_to_value_null() {
        assert_eq!(
            pg_param_to_value(PgOid::Text.as_u32(), 0, None).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn pg_vector_text_param_decodes_json_array() {
        assert_eq!(
            pg_param_to_value(PgOid::Vector.as_u32(), 0, Some(b"[1.0, -0.5]")).unwrap(),
            Value::Vector(vec![1.0, -0.5])
        );
    }
}
