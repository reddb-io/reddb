//! Driver-side parameter values for `query_with(sql, &[Value])`.
//!
//! Tracer-bullet implementation of issue #364 (Rust leg of PRD #351).
//! Mirrors the same `Value` taxonomy the Go (`drivers/go/redwire/value.go`)
//! and JS (`drivers/js/src/redwire.js`) drivers ship: 10 variants that map
//! 1:1 to the engine's binder slots through `reddb_server::storage::schema::Value`.
//!
//! Deep module pattern: this module owns *only* parameter serialization.
//! Transports import it; they don't reimplement type mapping. Two conversions
//! are exposed:
//!
//! - [`Value::into_json_param`] → `serde_json::Value` for HTTP (POST /query)
//!   and any future JSON-RPC transport. Uses the `{"$bytes": ...}` / `{"$ts":
//!   ...}` / `{"$uuid": ...}` envelope agreed in ADR 0001 / PRD #351.
//! - [`Value::into_schema_value`] (cfg `embedded`) → `SchemaValue` for the
//!   in-process binder. Avoids JSON round-trip on the hot embedded path.
//!
//! `IntoValue` covers the natural Rust → `Value` conversions called out by
//! the issue: primitives, `Vec<f32>`, `&[u8]`, `serde_json::Value`,
//! `chrono::DateTime` (when `chrono` is in the caller's deps — we accept a
//! plain `i64` seconds-since-epoch so we don't force a chrono dep on the
//! client crate), and `Uuid` (16-byte raw form — callers using the `uuid`
//! crate convert via `Uuid::as_bytes`).

use crate::types::JsonValue;

/// One parameter value for a `query_with(sql, params)` call.
///
/// Variants mirror the engine's binder slots; see
/// `crates/reddb-server/src/storage/query/user_params.rs` for the
/// authoritative per-slot type rules.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    /// f32 vector for similarity / vector slots.
    Vector(Vec<f32>),
    /// Structured JSON (object / array / scalar).
    Json(JsonValue),
    /// Seconds since Unix epoch.
    Timestamp(i64),
    /// Raw 16-byte UUID.
    Uuid([u8; 16]),
}

impl Value {
    /// Compatibility constructor for issue #386 examples and callers coming
    /// from drivers that spell the integer wire value as `Int64`.
    #[allow(non_snake_case)]
    pub fn Int64(value: i64) -> Self {
        Self::Int(value)
    }

    /// Canonical JSON envelope used by HTTP `POST /query`'s `params`
    /// field and any future JSON-RPC transport. Matches the shape
    /// `crates/reddb-server/src/server/handlers_query.rs` accepts.
    pub fn into_json_param(self) -> serde_json::Value {
        match self {
            Value::Null => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(b),
            Value::Int(n) => serde_json::Value::Number(n.into()),
            Value::Float(n) => serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null),
            Value::Text(s) => serde_json::Value::String(s),
            Value::Bytes(b) => serde_json::json!({ "$bytes": base64_encode(&b) }),
            Value::Vector(v) => serde_json::Value::Array(
                v.into_iter()
                    .map(|f| {
                        serde_json::Number::from_f64(f as f64)
                            .map(serde_json::Value::Number)
                            .unwrap_or(serde_json::Value::Null)
                    })
                    .collect(),
            ),
            Value::Json(j) => json_to_serde(&j),
            Value::Timestamp(secs) => serde_json::json!({ "$ts": secs }),
            Value::Uuid(bytes) => serde_json::json!({ "$uuid": format_uuid(&bytes) }),
        }
    }

    /// In-process conversion to the engine's `SchemaValue`. Skips JSON
    /// round-trip on the embedded path. Available only when the crate
    /// is built with `embedded`.
    #[cfg(feature = "embedded")]
    pub fn into_schema_value(self) -> reddb_server::storage::schema::Value {
        use reddb_server::storage::schema::Value as SV;
        match self {
            Value::Null => SV::Null,
            Value::Bool(b) => SV::Boolean(b),
            Value::Int(n) => SV::Integer(n),
            Value::Float(n) => SV::Float(n),
            Value::Text(s) => SV::Text(std::sync::Arc::from(s.as_str())),
            Value::Bytes(b) => SV::Blob(b),
            Value::Vector(v) => SV::Vector(v),
            Value::Json(j) => SV::Json(j.to_json_string().into_bytes()),
            Value::Timestamp(secs) => SV::Timestamp(secs),
            Value::Uuid(bytes) => SV::Uuid(bytes),
        }
    }
}

/// Ergonomic conversions so callers can write
/// `db.query_with(sql, &[42i64.into(), "alice".into()])`.
pub trait IntoValue {
    fn into_value(self) -> Value;
}

impl IntoValue for Value {
    fn into_value(self) -> Value {
        self
    }
}

impl IntoValue for bool {
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
}

macro_rules! int_into_value {
    ($($t:ty),*) => {
        $(
            impl IntoValue for $t {
                fn into_value(self) -> Value { Value::Int(self as i64) }
            }
        )*
    };
}
int_into_value!(i8, i16, i32, i64, u8, u16, u32);

impl IntoValue for u64 {
    fn into_value(self) -> Value {
        // u64 > i64::MAX is currently out of band — match Go driver's
        // overflow contract and surface it as a runtime error at
        // serialize time rather than silently wrapping. The simplest
        // path is to clamp via `try_from` and panic on overflow; the
        // typed `query_with` API takes already-built `Value`s, so the
        // caller can route through `Value::Int` directly when they need
        // explicit handling.
        Value::Int(i64::try_from(self).expect("u64 param > i64::MAX"))
    }
}

impl IntoValue for f32 {
    fn into_value(self) -> Value {
        Value::Float(self as f64)
    }
}

impl IntoValue for f64 {
    fn into_value(self) -> Value {
        Value::Float(self)
    }
}

impl IntoValue for &str {
    fn into_value(self) -> Value {
        Value::Text(self.to_string())
    }
}

impl IntoValue for String {
    fn into_value(self) -> Value {
        Value::Text(self)
    }
}

impl IntoValue for Vec<u8> {
    fn into_value(self) -> Value {
        Value::Bytes(self)
    }
}

impl IntoValue for &[u8] {
    fn into_value(self) -> Value {
        Value::Bytes(self.to_vec())
    }
}

impl IntoValue for Vec<f32> {
    fn into_value(self) -> Value {
        Value::Vector(self)
    }
}

impl IntoValue for &[f32] {
    fn into_value(self) -> Value {
        Value::Vector(self.to_vec())
    }
}

impl IntoValue for serde_json::Value {
    fn into_value(self) -> Value {
        Value::Json(serde_to_json(&self))
    }
}

impl IntoValue for JsonValue {
    fn into_value(self) -> Value {
        Value::Json(self)
    }
}

impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self) -> Value {
        match self {
            Some(v) => v.into_value(),
            None => Value::Null,
        }
    }
}

/// Convert user-facing parameter containers into the driver Value list.
///
/// This trait is sealed so the accepted parameter shapes remain explicit and
/// consistent across transports.
pub trait IntoParams: sealed::Sealed {
    fn into_params(self) -> Vec<Value>;
}

mod sealed {
    pub trait Sealed {}
}

impl sealed::Sealed for () {}

impl IntoParams for () {
    fn into_params(self) -> Vec<Value> {
        Vec::new()
    }
}

impl<V: IntoValue> sealed::Sealed for Vec<V> {}

impl<V: IntoValue> IntoParams for Vec<V> {
    fn into_params(self) -> Vec<Value> {
        self.into_iter().map(IntoValue::into_value).collect()
    }
}

impl<V: IntoValue + Clone> sealed::Sealed for &[V] {}

impl<V: IntoValue + Clone> IntoParams for &[V] {
    fn into_params(self) -> Vec<Value> {
        self.iter().cloned().map(IntoValue::into_value).collect()
    }
}

impl<V: IntoValue + Clone> sealed::Sealed for &Vec<V> {}

impl<V: IntoValue + Clone> IntoParams for &Vec<V> {
    fn into_params(self) -> Vec<Value> {
        self.as_slice().into_params()
    }
}

impl<V: IntoValue + Clone, const N: usize> sealed::Sealed for &[V; N] {}

impl<V: IntoValue + Clone, const N: usize> IntoParams for &[V; N] {
    fn into_params(self) -> Vec<Value> {
        self.as_slice().into_params()
    }
}

impl<V: IntoValue, const N: usize> sealed::Sealed for [V; N] {}

impl<V: IntoValue, const N: usize> IntoParams for [V; N] {
    fn into_params(self) -> Vec<Value> {
        self.into_iter().map(IntoValue::into_value).collect()
    }
}

macro_rules! tuple_into_params {
    ($($name:ident),+) => {
        impl<$($name: IntoValue),+> sealed::Sealed for ($($name,)+) {}

        impl<$($name: IntoValue),+> IntoParams for ($($name,)+) {
            #[allow(non_snake_case)]
            fn into_params(self) -> Vec<Value> {
                let ($($name,)+) = self;
                vec![$($name.into_value()),+]
            }
        }
    };
}

tuple_into_params!(A);
tuple_into_params!(A, B);
tuple_into_params!(A, B, C);
tuple_into_params!(A, B, C, D);
tuple_into_params!(A, B, C, D, E);
tuple_into_params!(A, B, C, D, E, F);
tuple_into_params!(A, B, C, D, E, F, G);
tuple_into_params!(A, B, C, D, E, F, G, H);

fn json_to_serde(v: &JsonValue) -> serde_json::Value {
    match v {
        JsonValue::Null => serde_json::Value::Null,
        JsonValue::Bool(b) => serde_json::Value::Bool(*b),
        JsonValue::Number(n) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        JsonValue::String(s) => serde_json::Value::String(s.clone()),
        JsonValue::Array(items) => {
            serde_json::Value::Array(items.iter().map(json_to_serde).collect())
        }
        JsonValue::Object(entries) => {
            let mut map = serde_json::Map::with_capacity(entries.len());
            for (k, v) in entries {
                map.insert(k.clone(), json_to_serde(v));
            }
            serde_json::Value::Object(map)
        }
    }
}

fn serde_to_json(v: &serde_json::Value) -> JsonValue {
    match v {
        serde_json::Value::Null => JsonValue::Null,
        serde_json::Value::Bool(b) => JsonValue::Bool(*b),
        serde_json::Value::Number(n) => JsonValue::Number(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => JsonValue::String(s.clone()),
        serde_json::Value::Array(items) => {
            JsonValue::Array(items.iter().map(serde_to_json).collect())
        }
        serde_json::Value::Object(map) => JsonValue::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), serde_to_json(v)))
                .collect(),
        ),
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for c in chunks.by_ref() {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
        out.push(TABLE[(n & 0x3F) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        0 => {}
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(TABLE[((n >> 18) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 12) & 0x3F) as usize] as char);
            out.push(TABLE[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => unreachable!(),
    }
    out
}

fn format_uuid(bytes: &[u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    for (i, b) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn into_value_primitives() {
        assert_eq!(true.into_value(), Value::Bool(true));
        assert_eq!(42i32.into_value(), Value::Int(42));
        assert_eq!((-1i64).into_value(), Value::Int(-1));
        assert_eq!(2.5f64.into_value(), Value::Float(2.5));
        assert_eq!("hi".into_value(), Value::Text("hi".to_string()));
        assert_eq!(String::from("x").into_value(), Value::Text("x".to_string()));
    }

    #[test]
    fn into_value_bytes_and_vector() {
        assert_eq!(vec![1u8, 2, 3].into_value(), Value::Bytes(vec![1, 2, 3]));
        let slice: &[u8] = &[9, 8];
        assert_eq!(slice.into_value(), Value::Bytes(vec![9, 8]));
        assert_eq!(
            vec![0.1f32, 0.2].into_value(),
            Value::Vector(vec![0.1, 0.2])
        );
    }

    #[test]
    fn into_value_option_maps_to_null() {
        let none: Option<i64> = None;
        assert_eq!(none.into_value(), Value::Null);
        let some: Option<i64> = Some(7);
        assert_eq!(some.into_value(), Value::Int(7));
    }

    #[test]
    fn json_param_envelope_for_bytes() {
        let v = Value::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]);
        let j = v.into_json_param();
        assert_eq!(j["$bytes"].as_str().unwrap(), "3q2+7w==");
    }

    #[test]
    fn json_param_envelope_for_timestamp() {
        let j = Value::Timestamp(1_700_000_000).into_json_param();
        assert_eq!(j["$ts"].as_i64().unwrap(), 1_700_000_000);
    }

    #[test]
    fn json_param_envelope_for_uuid() {
        let bytes = [
            0x55, 0x0e, 0x84, 0x00, 0xe2, 0x9b, 0x41, 0xd4, 0xa7, 0x16, 0x44, 0x66, 0x55, 0x44,
            0x00, 0x00,
        ];
        let j = Value::Uuid(bytes).into_json_param();
        assert_eq!(
            j["$uuid"].as_str().unwrap(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn json_param_vector_is_plain_array() {
        let j = Value::Vector(vec![0.0, 1.0, -1.5]).into_json_param();
        let arr = j.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert!((arr[0].as_f64().unwrap() - 0.0).abs() < 1e-6);
        assert!((arr[2].as_f64().unwrap() - -1.5).abs() < 1e-6);
    }

    #[test]
    fn base64_encode_known_vectors() {
        // RFC 4648 §10 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[cfg(feature = "embedded")]
    #[test]
    fn into_schema_value_covers_all_variants() {
        use reddb_server::storage::schema::Value as SV;
        assert!(matches!(Value::Null.into_schema_value(), SV::Null));
        assert!(matches!(
            Value::Bool(true).into_schema_value(),
            SV::Boolean(true)
        ));
        assert!(matches!(Value::Int(7).into_schema_value(), SV::Integer(7)));
        assert!(
            matches!(Value::Float(1.5).into_schema_value(), SV::Float(f) if (f - 1.5).abs() < 1e-9)
        );
        let SV::Text(s) = Value::Text("x".into()).into_schema_value() else {
            panic!()
        };
        assert_eq!(s.as_ref(), "x");
        assert!(
            matches!(Value::Bytes(vec![1, 2]).into_schema_value(), SV::Blob(b) if b == vec![1, 2])
        );
        assert!(matches!(
            Value::Vector(vec![0.1, 0.2]).into_schema_value(),
            SV::Vector(v) if v == vec![0.1f32, 0.2]
        ));
        assert!(matches!(
            Value::Timestamp(99).into_schema_value(),
            SV::Timestamp(99)
        ));
        let SV::Uuid(b) = Value::Uuid([0u8; 16]).into_schema_value() else {
            panic!()
        };
        assert_eq!(b, [0u8; 16]);
    }
}
