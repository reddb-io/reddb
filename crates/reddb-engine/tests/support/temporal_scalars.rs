//! Canonical time conversion and overflow boundaries in native and WASM.
use reddb_engine::query::runtime::temporal::*;
use reddb_types::Value;

pub const CASES: &[(&str, fn())] = &[
    (
        "temporal_duration_units_and_overflow",
        duration_units_and_overflow,
    ),
    (
        "temporal_timestamp_conversion_and_bounds",
        timestamp_conversion_and_bounds,
    ),
    (
        "temporal_bucket_zero_and_wide_values",
        bucket_zero_and_wide_values,
    ),
];

fn duration_units_and_overflow() {
    for (value, expected) in [
        (Value::text("raw"), Some(0)),
        (Value::text("5ms"), Some(5_000_000)),
        (Value::text("2 seconds"), Some(2_000_000_000)),
        (Value::UnsignedInteger(u64::MAX), Some(u64::MAX)),
        (Value::Integer(100), Some(100)),
        (Value::BigInt(100), Some(100)),
        (Value::text("100"), None),
        (Value::Integer(-1), None),
        (Value::Duration(100), None),
        (Value::Null, None),
        (Value::text("18446744073709551615s"), None),
        (Value::text("18446744073709551615 seconds"), None),
    ] {
        assert_eq!(time_bucket_duration(&value), expected, "{value:?}");
    }
}

fn timestamp_conversion_and_bounds() {
    for (value, expected) in [
        (Value::UnsignedInteger(u64::MAX), Some(u64::MAX)),
        (Value::Integer(100), Some(100)),
        (Value::BigInt(100), Some(100)),
        (Value::Float(1.5), Some(1)),
        (Value::Float(f64::INFINITY), Some(u64::MAX)),
        (Value::Float(f64::NAN), None),
        (Value::Float(-1.0), None),
        (Value::Timestamp(2), Some(2_000_000_000)),
        (Value::TimestampMs(2), Some(2_000_000)),
        (
            Value::Timestamp(18_446_744_073),
            Some(18_446_744_073_000_000_000),
        ),
        (Value::Timestamp(18_446_744_074), None),
        (
            Value::TimestampMs(18_446_744_073_709),
            Some(18_446_744_073_709_000_000),
        ),
        (Value::TimestampMs(18_446_744_073_710), None),
        (Value::Timestamp(-1), None),
        (Value::TimestampMs(-1), None),
        (Value::Integer(-1), None),
        (Value::text("100"), None),
        (Value::Null, None),
    ] {
        assert_eq!(value_to_bucket_timestamp_ns(&value), expected, "{value:?}");
    }
}

fn bucket_zero_and_wide_values() {
    for (width, stamp, expected) in [
        (0, u64::MAX, u64::MAX),
        (1, u64::MAX, u64::MAX),
        (5, 17, 15),
        (5_000_000_000, 17_000_000_000, 15_000_000_000),
        (u64::MAX, u64::MAX - 1, 0),
        (u64::MAX, u64::MAX, u64::MAX),
    ] {
        assert_eq!(bucket_start_ns(width, stamp), expected);
    }
    assert_eq!(time_ms_since_midnight(0), 0);
    assert_eq!(time_ms_since_midnight(172_800_123), 123);
    assert_eq!(time_ms_since_midnight(-1), 86_399_999);
    assert_eq!(time_ms_since_midnight(-86_400_000), 0);
}
