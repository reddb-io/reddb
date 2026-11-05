//! Native time-bucket conversion and query-clock helpers.
use crate::query::unified::UnifiedRecord;
use reddb_rql::ast::Projection;
use reddb_types::Value;

pub use crate::clock::current_unix_ms;

pub fn time_ms_since_midnight(unix_ms: i64) -> u32 {
    unix_ms.rem_euclid(86_400_000) as u32
}

/// Zero-width buckets preserve the input timestamp, as in the native runtime.
pub fn bucket_start_ns(bucket_ns: u64, timestamp_ns: u64) -> u64 {
    timestamp_ns
        .checked_div(bucket_ns)
        .map(|bucket| bucket * bucket_ns)
        .unwrap_or(timestamp_ns)
}

/// Typed calls accept numeric nanoseconds; legacy projection calls require a literal duration.
pub fn time_bucket_duration(value: &Value) -> Option<u64> {
    match value {
        Value::Text(text) => reddb_types::duration::parse_duration_ns(text),
        Value::UnsignedInteger(value) => Some(*value),
        Value::Integer(value) if *value >= 0 => Some(*value as u64),
        Value::BigInt(value) if *value >= 0 => Some(*value as u64),
        _ => None,
    }
}

pub fn resolve_time_bucket_duration(args: &[Projection], index: usize) -> Option<u64> {
    let Projection::Column(column) = args.get(index)? else {
        return None;
    };
    let literal = column.strip_prefix("LIT:")?;
    reddb_types::duration::parse_duration_ns(literal)
}

/// Preserve legacy raw-field fallback for an unresolved explicit argument.
/// A resolved NULL/invalid value stops resolution. Callers admit source records;
/// timestamp field names do not infer units or apply another input policy.
pub fn resolve_time_bucket_timestamp(
    args: &[Projection],
    source: &UnifiedRecord,
    resolve: &impl Fn(&Projection) -> Option<Value>,
) -> Option<u64> {
    if let Some(value) = args.get(1).and_then(resolve) {
        return value_to_bucket_timestamp_ns(&value);
    }

    source
        .get("timestamp_ns")
        .and_then(value_to_bucket_timestamp_ns)
        .or_else(|| {
            source
                .get("timestamp_ms")
                .and_then(value_to_bucket_timestamp_ns)
        })
        .or_else(|| {
            source
                .get("timestamp")
                .and_then(value_to_bucket_timestamp_ns)
        })
}

pub fn value_to_bucket_timestamp_ns(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        Value::BigInt(v) if *v >= 0 => Some(*v as u64),
        Value::Float(v) if *v >= 0.0 => Some(*v as u64),
        Value::Timestamp(v) if *v >= 0 => (*v as u64).checked_mul(1_000_000_000),
        Value::TimestampMs(v) if *v >= 0 => (*v as u64).checked_mul(1_000_000),
        _ => None,
    }
}
