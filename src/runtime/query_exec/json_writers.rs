//! Hand-rolled JSON writers used by the runtime query executor hot path.
//!
//! These helpers bypass `serde_json` and write directly into a `Vec<u8>`
//! because they sit on the query serialization path and every allocation
//! matters. The format is intentionally identical to what a naive
//! `serde_json::to_string` on the equivalent struct would produce — see
//! the `json_value_to_wire` unit tests in `rpc_stdio` for shape coverage.
//!
//! All functions are `pub(super)` so only `query_exec.rs` can reach them.

use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, EntityKind, UnifiedEntity};

/// Wrap a `HashMap<String, String>` tag set as a `Value::Json` blob so it
/// round-trips through the schema layer unchanged. Used by both the
/// entity JSON writer above and by the aggregate executor.
pub(crate) fn timeseries_tags_json_value(
    tags: &std::collections::HashMap<String, String>,
) -> Value {
    let object = tags
        .iter()
        .map(|(key, value)| (key.clone(), crate::json::Value::String(value.clone())))
        .collect();
    let json = crate::json::Value::Object(object);
    Value::Json(crate::json::to_vec(&json).unwrap_or_default())
}

/// Write a u64 as decimal digits.
#[inline(always)]
pub(crate) fn write_u64(buf: &mut Vec<u8>, n: u64) {
    let mut b = itoa::Buffer::new();
    let s = b.format(n);
    buf.extend_from_slice(s.as_bytes());
}

/// Emit `"created_at":X,"updated_at":Y` for an entity, preferring the
/// declared row columns (e.g. from `CREATE TABLE ... WITH timestamps =
/// true`) over the entity's internal `created_at`/`updated_at` fields.
///
/// The caller is responsible for the leading `,` before the block.
#[inline(always)]
pub(crate) fn write_timestamp_fields_json(buf: &mut Vec<u8>, entity: &UnifiedEntity) {
    let (row_created_at, row_updated_at) = match &entity.data {
        EntityData::Row(row) => {
            let mut created = None;
            let mut updated = None;
            for (key, value) in row.iter_fields() {
                match key {
                    "created_at" => created = Some(value.clone()),
                    "updated_at" => updated = Some(value.clone()),
                    _ => {}
                }
                if created.is_some() && updated.is_some() {
                    break;
                }
            }
            (created, updated)
        }
        _ => (None, None),
    };
    buf.extend_from_slice(b"\"created_at\":");
    if let Some(v) = &row_created_at {
        write_value_bytes(buf, v);
    } else {
        write_u64(buf, entity.created_at);
    }
    buf.extend_from_slice(b",\"updated_at\":");
    if let Some(v) = &row_updated_at {
        write_value_bytes(buf, v);
    } else {
        write_u64(buf, entity.updated_at);
    }
}

/// Write an entity's fields as JSON bytes into a Vec<u8> buffer.
///
/// `created_at` and `updated_at` are de-duplicated: if the row carries
/// them as declared user columns (e.g. from `CREATE TABLE ... WITH
/// timestamps = true`), we emit the row's value and skip the iter
/// pass for those keys. Otherwise we fall back to the entity's
/// internal `created_at`/`updated_at` stamps.
#[inline(always)]
pub(crate) fn write_entity_json_bytes(buf: &mut Vec<u8>, entity: &UnifiedEntity) {
    buf.push(b'{');
    buf.extend_from_slice(b"\"red_entity_id\":");
    write_u64(buf, entity.id.raw());
    buf.extend_from_slice(b",\"red_collection\":");
    write_json_bytes(buf, entity.kind.collection().as_bytes());
    buf.extend_from_slice(b",\"red_kind\":");
    write_json_bytes(buf, entity.kind.storage_type().as_bytes());
    buf.push(b',');
    write_timestamp_fields_json(buf, entity);
    buf.extend_from_slice(b",\"red_sequence_id\":");
    write_u64(buf, entity.sequence_id);
    let (entity_type, capabilities) = match &entity.data {
        EntityData::Row(_) => ("table", "structured,table"),
        EntityData::TimeSeries(_) => ("timeseries", "timeseries,metric,temporal"),
        _ => ("unknown", "unknown"),
    };
    buf.extend_from_slice(b",\"red_entity_type\":");
    write_json_bytes(buf, entity_type.as_bytes());
    buf.extend_from_slice(b",\"red_capabilities\":");
    write_json_bytes(buf, capabilities.as_bytes());

    if let EntityKind::TableRow { row_id, .. } = &entity.kind {
        buf.extend_from_slice(b",\"row_id\":");
        write_u64(buf, *row_id);
    }

    match &entity.data {
        EntityData::Row(row) => {
            for (key, value) in row.iter_fields() {
                // Skip the two auto-timestamp columns — already emitted
                // once at the top of the JSON from either the row value
                // itself or `entity.created_at/updated_at`.
                if key == "created_at" || key == "updated_at" {
                    continue;
                }
                buf.push(b',');
                write_json_bytes(buf, key.as_bytes());
                buf.push(b':');
                write_value_bytes(buf, value);
            }
        }
        EntityData::TimeSeries(ts) => {
            buf.extend_from_slice(b",\"metric\":");
            write_json_bytes(buf, ts.metric.as_bytes());
            buf.extend_from_slice(b",\"timestamp_ns\":");
            write_u64(buf, ts.timestamp_ns);
            buf.extend_from_slice(b",\"timestamp\":");
            write_u64(buf, ts.timestamp_ns);
            buf.extend_from_slice(b",\"time\":");
            write_u64(buf, ts.timestamp_ns);
            buf.extend_from_slice(b",\"value\":");
            write_value_bytes(buf, &Value::Float(ts.value));
            if !ts.tags.is_empty() {
                buf.extend_from_slice(b",\"tags\":");
                write_value_bytes(buf, &timeseries_tags_json_value(&ts.tags));
            }
        }
        _ => {}
    }
    buf.push(b'}');
}

/// Write a JSON-quoted string to bytes. Assumes most strings are ASCII-safe.
#[inline(always)]
pub(crate) fn write_json_bytes(buf: &mut Vec<u8>, s: &[u8]) {
    buf.push(b'"');
    for &b in s {
        match b {
            b'"' => buf.extend_from_slice(b"\\\""),
            b'\\' => buf.extend_from_slice(b"\\\\"),
            b'\n' => buf.extend_from_slice(b"\\n"),
            b'\r' => buf.extend_from_slice(b"\\r"),
            b'\t' => buf.extend_from_slice(b"\\t"),
            b if b < 0x20 => {
                // \u00XX escape for control characters
                buf.extend_from_slice(b"\\u00");
                let hi = b >> 4;
                let lo = b & 0xf;
                buf.push(if hi < 10 { b'0' + hi } else { b'a' + hi - 10 });
                buf.push(if lo < 10 { b'0' + lo } else { b'a' + lo - 10 });
            }
            _ => buf.push(b),
        }
    }
    buf.push(b'"');
}

/// Write a Value as JSON bytes.
#[inline(always)]
pub(crate) fn write_value_bytes(buf: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => buf.extend_from_slice(b"null"),
        Value::Boolean(true) => buf.extend_from_slice(b"true"),
        Value::Boolean(false) => buf.extend_from_slice(b"false"),
        Value::Integer(n) => {
            let mut b = itoa::Buffer::new();
            let s = b.format(*n);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::UnsignedInteger(n) => {
            let mut b = itoa::Buffer::new();
            let s = b.format(*n);
            buf.extend_from_slice(s.as_bytes());
        }
        Value::Float(f) => {
            if f.is_finite() {
                // std::fmt gives shortest-round-trip output for f64
                // via the same grisu-based algorithm that ryu used.
                // Writing directly into `Vec<u8>` avoids an extra
                // String allocation.
                use std::io::Write;
                let _ = write!(buf, "{}", f);
            } else {
                buf.extend_from_slice(b"null");
            }
        }
        Value::Text(s) => write_json_bytes(buf, s.as_bytes()),
        Value::Array(values) => {
            buf.push(b'[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    buf.push(b',');
                }
                write_value_bytes(buf, value);
            }
            buf.push(b']');
        }
        Value::Json(value) => {
            if std::str::from_utf8(value).is_ok() {
                buf.extend_from_slice(value);
            } else {
                buf.extend_from_slice(b"null");
            }
        }
        Value::Timestamp(t) => {
            let mut b = itoa::Buffer::new();
            let s = b.format(*t);
            buf.extend_from_slice(s.as_bytes());
        }
        _ => buf.extend_from_slice(b"null"),
    }
}

/// Serialize a single entity to the full result JSON wrapper.
pub(crate) fn execute_runtime_serialize_single_entity(entity: &UnifiedEntity) -> String {
    let mut buf = Vec::with_capacity(512);
    buf.extend_from_slice(
        b"{\"columns\":[],\"record_count\":1,\"selection\":{\"scope\":\"any\"},\"records\":[",
    );
    write_entity_json_bytes(&mut buf, entity);
    buf.extend_from_slice(b"]}");
    // SAFETY: we only wrote valid UTF-8 bytes
    unsafe { String::from_utf8_unchecked(buf) }
}
