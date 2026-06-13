use crate::utils::json::{parse_json, JsonValue};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::ops::{Index, IndexMut};

pub type Map<K, V> = BTreeMap<K, V>;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Map<String, Value>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_string_compact())
    }
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(n) => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(n) if *n >= 0.0 => Some(*n as u64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(values) => Some(values.as_slice()),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Map<String, Value>> {
        match self {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        if let Value::Object(map) = self {
            map.get(key)
        } else {
            None
        }
    }

    pub fn to_string_compact(&self) -> String {
        let mut out = String::new();
        self.write_compact(&mut out);
        out
    }

    pub fn to_string_pretty(&self) -> String {
        let mut out = String::new();
        self.write_pretty(&mut out, 0);
        out
    }

    fn write_compact(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => {
                if n.fract() == 0.0 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{}", n));
                }
            }
            Value::String(s) => {
                out.push('"');
                out.push_str(&escape_string(s));
                out.push('"');
            }
            Value::Array(values) => {
                out.push('[');
                for (idx, value) in values.iter().enumerate() {
                    if idx > 0 {
                        out.push(',');
                    }
                    value.write_compact(out);
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                for (idx, (key, value)) in map.iter().enumerate() {
                    if idx > 0 {
                        out.push(',');
                    }
                    out.push('"');
                    out.push_str(&escape_string(key));
                    out.push('"');
                    out.push(':');
                    value.write_compact(out);
                }
                out.push('}');
            }
        }
    }

    fn write_pretty(&self, out: &mut String, indent: usize) {
        match self {
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                out.push_str(&self.to_string_compact());
            }
            Value::Array(values) => {
                out.push('[');
                if !values.is_empty() {
                    out.push('\n');
                    for (idx, value) in values.iter().enumerate() {
                        if idx > 0 {
                            out.push_str(",\n");
                        }
                        out.push_str(&"  ".repeat(indent + 1));
                        value.write_pretty(out, indent + 1);
                    }
                    out.push('\n');
                    out.push_str(&"  ".repeat(indent));
                }
                out.push(']');
            }
            Value::Object(map) => {
                out.push('{');
                if !map.is_empty() {
                    out.push('\n');
                    for (idx, (key, value)) in map.iter().enumerate() {
                        if idx > 0 {
                            out.push_str(",\n");
                        }
                        out.push_str(&"  ".repeat(indent + 1));
                        out.push('"');
                        out.push_str(&escape_string(key));
                        out.push_str("\": ");
                        value.write_pretty(out, indent + 1);
                    }
                    out.push('\n');
                    out.push_str(&"  ".repeat(indent));
                }
                out.push('}');
            }
        }
    }
}

fn escape_string(input: &str) -> String {
    // RFC 8259 §7: all control bytes (U+0000..U+001F), `"`, and `\` MUST be escaped.
    // Previous version silently dropped control bytes other than \n \r \t — see
    // F-01 in docs/security/serialization-boundary-audit-2026-05-06.md and
    // ADR 0010 (serialization-boundary discipline).
    use std::fmt::Write as _;
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;
    use std::borrow::Cow;
    use std::collections::HashMap;

    fn encode(s: &str) -> String {
        Value::String(s.to_string()).to_string_compact()
    }

    /// Every byte 0x00..0x20 must produce a valid JSON string that round-trips
    /// through a real JSON parser preserving the original byte.
    #[test]
    fn escape_string_handles_every_control_byte() {
        for byte in 0x00u8..0x20 {
            let original: String = std::char::from_u32(byte as u32).unwrap().to_string();
            let encoded = encode(&original);
            // Must parse back to the exact same byte (NOT silently dropped).
            let parsed: String = from_str(&encoded).unwrap_or_else(|err| {
                panic!("byte 0x{byte:02x} encoded as {encoded:?} failed to parse: {err}")
            });
            assert_eq!(
                parsed, original,
                "byte 0x{byte:02x} did not round-trip (encoded={encoded:?})"
            );
        }
    }

    #[test]
    fn escape_string_handles_standard_escapes() {
        assert_eq!(encode("\""), "\"\\\"\"");
        assert_eq!(encode("\\"), "\"\\\\\"");
        assert_eq!(encode("\n"), "\"\\n\"");
        assert_eq!(encode("\r"), "\"\\r\"");
        assert_eq!(encode("\t"), "\"\\t\"");
        assert_eq!(encode("\u{08}"), "\"\\b\"");
        assert_eq!(encode("\u{0C}"), "\"\\f\"");
    }

    #[test]
    fn escape_string_handles_mixed_payload() {
        let input = "name=\"x\"\n\\path\t\x01end";
        let encoded = encode(input);
        let parsed: String = from_str(&encoded).expect("mixed payload must parse");
        assert_eq!(parsed, input);
    }

    /// Regression test for F-01: the "self-disagreeing audit log" exploit.
    /// An attacker writes audit data containing \x01. The old encoder
    /// silently dropped \x01, so a downstream auditor that re-parses the
    /// JSONL would see a different record than what was emitted. The fix
    /// must encode \x01 as  so it survives the round trip.
    #[test]
    fn audit_log_preserves_low_control_bytes() {
        let payload = "collection\x01name\x07with\x1fbells";
        let encoded = encode(payload);

        // Encoded form must contain explicit \u escapes — NOT raw control bytes,
        // NOT silent drops.
        assert!(
            encoded.contains("\\u0001"),
            "expected \\u0001 escape in {encoded:?}"
        );
        assert!(
            encoded.contains("\\u0007"),
            "expected \\u0007 escape in {encoded:?}"
        );
        assert!(
            encoded.contains("\\u001f"),
            "expected \\u001f escape in {encoded:?}"
        );
        assert!(
            !encoded.contains('\x01'),
            "raw \\x01 must not appear in encoded output"
        );

        // Round trip through the in-house parser must reproduce the original bytes.
        let parsed: String = from_str(&encoded).expect("audit payload must parse");
        assert_eq!(parsed, payload);
    }

    #[test]
    fn value_accessors_indexing_and_pretty_printing_cover_all_shapes() {
        let mut object = Map::new();
        object.insert("null".to_string(), Value::Null);
        object.insert("bool".to_string(), Value::Bool(true));
        object.insert("number".to_string(), Value::Number(42.5));
        object.insert("string".to_string(), Value::String("reddb".to_string()));
        object.insert(
            "array".to_string(),
            Value::Array(vec![Value::Number(1.0), Value::String("two".to_string())]),
        );
        let value = Value::Object(object);

        assert_eq!(value.get("string").and_then(Value::as_str), Some("reddb"));
        assert_eq!(value.get("number").and_then(Value::as_f64), Some(42.5));
        assert_eq!(value.get("number").and_then(Value::as_i64), Some(42));
        assert_eq!(value.get("number").and_then(Value::as_u64), Some(42));
        assert_eq!(value.get("bool").and_then(Value::as_bool), Some(true));
        assert_eq!(
            value.get("array").and_then(Value::as_array).map(<[_]>::len),
            Some(2)
        );
        assert_eq!(value.as_object().map(Map::len), Some(5));
        assert!(Value::Number(-1.0).as_u64().is_none());
        assert!(Value::Null.as_str().is_none());

        assert_eq!(value["missing"], Value::Null);
        assert_eq!(value["string"], Value::String("reddb".to_string()));
        let pretty = value.to_string_pretty();
        assert!(pretty.contains('\n'));
        assert!(pretty.contains("\"array\": ["));
        assert_eq!(Value::Array(Vec::new()).to_string_pretty(), "[]");
        assert_eq!(Value::Object(Map::new()).to_string_pretty(), "{}");

        let mut created_from_index = Value::Null;
        created_from_index["created"] = Value::Bool(true);
        assert_eq!(created_from_index["created"], Value::Bool(true));
    }

    #[test]
    fn json_encode_decode_traits_cover_scalars_collections_and_errors() {
        assert_eq!(to_value(&true), Value::Bool(true));
        assert_eq!(to_value(&-7i64), Value::Number(-7.0));
        assert_eq!(to_value(&-3i32), Value::Number(-3.0));
        assert_eq!(to_value(&7u8), Value::Number(7.0));
        assert_eq!(to_value(&8u16), Value::Number(8.0));
        assert_eq!(to_value(&9u32), Value::Number(9.0));
        assert_eq!(to_value(&10u64), Value::Number(10.0));
        assert_eq!(to_value(&11usize), Value::Number(11.0));
        assert_eq!(to_value(&1.5f64), Value::Number(1.5));
        assert_eq!(to_value(&2.5f32), Value::Number(2.5));
        assert_eq!(to_value(&"borrowed"), Value::String("borrowed".to_string()));
        assert_eq!(
            to_value(&"owned".to_string()),
            Value::String("owned".to_string())
        );
        let cow: Cow<'_, str> = Cow::Borrowed("cow");
        assert_eq!(to_value(&cow), Value::String("cow".to_string()));
        assert_eq!(
            to_value(&vec![1u8, 2, 3]),
            Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0)
            ])
        );
        assert_eq!(
            to_value(&[4u8, 5, 6]),
            Value::Array(vec![
                Value::Number(4.0),
                Value::Number(5.0),
                Value::Number(6.0)
            ])
        );
        assert_eq!(to_value(&Some(12u16)), Value::Number(12.0));
        assert_eq!(to_value(&Option::<u16>::None), Value::Null);

        let mut hash = HashMap::new();
        hash.insert("a".to_string(), 1u8);
        let Value::Object(map) = to_value(&hash) else {
            panic!("hash map should encode to object");
        };
        assert_eq!(map.get("a"), Some(&Value::Number(1.0)));

        assert_eq!(
            from_value::<String>(Value::String("x".to_string())).unwrap(),
            "x"
        );
        assert!(from_value::<String>(Value::Bool(true)).is_err());
        assert_eq!(from_value::<bool>(Value::Bool(false)).unwrap(), false);
        assert!(from_value::<bool>(Value::String("no".to_string())).is_err());
        assert_eq!(from_value::<u8>(Value::Number(255.0)).unwrap(), 255);
        assert_eq!(from_value::<u16>(Value::Number(256.0)).unwrap(), 256);
        assert_eq!(from_value::<u32>(Value::Number(257.0)).unwrap(), 257);
        assert_eq!(from_value::<u64>(Value::Number(258.0)).unwrap(), 258);
        assert_eq!(from_value::<usize>(Value::Number(259.0)).unwrap(), 259);
        assert_eq!(from_value::<i64>(Value::Number(-260.0)).unwrap(), -260);
        assert_eq!(from_value::<i32>(Value::Number(-261.0)).unwrap(), -261);
        assert_eq!(from_value::<f32>(Value::Number(1.25)).unwrap(), 1.25);
        assert!(from_value::<u8>(Value::String("no".to_string())).is_err());
        assert!(from_value::<Vec<u8>>(Value::Bool(false)).is_err());
        assert_eq!(
            from_value::<Vec<u8>>(Value::Array(vec![Value::Number(1.0), Value::Number(2.0)]))
                .unwrap(),
            vec![1, 2]
        );

        let mut object = Map::new();
        object.insert("x".to_string(), Value::Number(7.0));
        let decoded: HashMap<String, u8> = from_value(Value::Object(object)).unwrap();
        assert_eq!(decoded.get("x"), Some(&7));
        assert!(from_value::<HashMap<String, u8>>(Value::Null).is_err());

        assert_eq!(from_value::<Option<u8>>(Value::Null).unwrap(), None);
        assert_eq!(
            from_value::<Option<u8>>(Value::Number(9.0)).unwrap(),
            Some(9)
        );
        assert_eq!(
            from_value::<[u8; 3]>(Value::Array(vec![
                Value::Number(1.0),
                Value::Number(2.0),
                Value::Number(3.0),
            ]))
            .unwrap(),
            [1, 2, 3]
        );
        assert!(from_value::<[u8; 3]>(Value::Array(vec![Value::Number(1.0)])).is_err());
        assert!(from_value::<[u8; 3]>(Value::Null).is_err());
    }

    #[test]
    fn string_and_byte_entry_points_round_trip_and_reject_bad_inputs() {
        let bytes = to_vec(&vec![1u8, 2, 3]).unwrap();
        assert_eq!(from_slice::<Vec<u8>>(&bytes).unwrap(), vec![1, 2, 3]);
        assert!(from_slice::<Value>(&[0xff]).is_err());

        let compact = to_string(&json!({ "b": true, "n": 2 })).unwrap();
        assert_eq!(from_str::<Value>(&compact).unwrap()["b"], Value::Bool(true));

        let pretty = to_string_pretty(&json!([1, 2])).unwrap();
        assert!(pretty.contains('\n'));
    }
}

impl From<JsonValue> for Value {
    fn from(value: JsonValue) -> Self {
        match value {
            JsonValue::Null => Value::Null,
            JsonValue::Bool(b) => Value::Bool(b),
            JsonValue::Number(n) => Value::Number(n),
            JsonValue::String(s) => Value::String(s),
            JsonValue::Array(values) => Value::Array(values.into_iter().map(Value::from).collect()),
            JsonValue::Object(entries) => {
                let mut map = Map::new();
                for (k, v) in entries {
                    map.insert(k, Value::from(v));
                }
                Value::Object(map)
            }
        }
    }
}

impl Index<&str> for Value {
    type Output = Value;

    fn index(&self, key: &str) -> &Self::Output {
        static NULL: Value = Value::Null;
        match self {
            Value::Object(map) => map.get(key).unwrap_or(&NULL),
            _ => &NULL,
        }
    }
}

impl IndexMut<&str> for Value {
    fn index_mut(&mut self, key: &str) -> &mut Self::Output {
        match self {
            Value::Object(map) => map.entry(key.to_string()).or_insert(Value::Null),
            _ => {
                *self = Value::Object(Map::new());
                match self {
                    Value::Object(map) => map.entry(key.to_string()).or_insert(Value::Null),
                    _ => unreachable!(),
                }
            }
        }
    }
}

pub trait JsonEncode {
    fn to_json_value(&self) -> Value;
}

impl<T: JsonEncode + ?Sized> JsonEncode for &T {
    fn to_json_value(&self) -> Value {
        (*self).to_json_value()
    }
}

pub trait JsonDecode: Sized {
    fn from_json_value(value: Value) -> Result<Self, String>;
}

impl JsonEncode for Value {
    fn to_json_value(&self) -> Value {
        self.clone()
    }
}

impl JsonDecode for Value {
    fn from_json_value(value: Value) -> Result<Self, String> {
        Ok(value)
    }
}

impl JsonEncode for bool {
    fn to_json_value(&self) -> Value {
        Value::Bool(*self)
    }
}

impl JsonEncode for i64 {
    fn to_json_value(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl JsonEncode for i32 {
    fn to_json_value(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl JsonEncode for u8 {
    fn to_json_value(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl JsonEncode for u16 {
    fn to_json_value(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl JsonEncode for u32 {
    fn to_json_value(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl JsonEncode for u64 {
    fn to_json_value(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl JsonEncode for usize {
    fn to_json_value(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl JsonEncode for f64 {
    fn to_json_value(&self) -> Value {
        Value::Number(*self)
    }
}

impl JsonEncode for f32 {
    fn to_json_value(&self) -> Value {
        Value::Number(*self as f64)
    }
}

impl JsonEncode for String {
    fn to_json_value(&self) -> Value {
        Value::String(self.clone())
    }
}

impl JsonEncode for &str {
    fn to_json_value(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl<'a> JsonEncode for std::borrow::Cow<'a, str> {
    fn to_json_value(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl<T: JsonEncode> JsonEncode for Vec<T> {
    fn to_json_value(&self) -> Value {
        Value::Array(self.iter().map(|v| v.to_json_value()).collect())
    }
}

impl<T: JsonEncode> JsonEncode for [T] {
    fn to_json_value(&self) -> Value {
        Value::Array(self.iter().map(|v| v.to_json_value()).collect())
    }
}

impl<T: JsonEncode> JsonEncode for Option<T> {
    fn to_json_value(&self) -> Value {
        match self {
            Some(value) => value.to_json_value(),
            None => Value::Null,
        }
    }
}

impl<const N: usize> JsonEncode for [u8; N] {
    fn to_json_value(&self) -> Value {
        Value::Array(self.iter().map(|b| Value::Number(*b as f64)).collect())
    }
}

impl<T: JsonEncode> JsonEncode for HashMap<String, T> {
    fn to_json_value(&self) -> Value {
        let mut map = Map::new();
        for (k, v) in self {
            map.insert(k.clone(), v.to_json_value());
        }
        Value::Object(map)
    }
}

impl JsonDecode for String {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::String(s) => Ok(s),
            _ => Err("expected string".to_string()),
        }
    }
}

impl JsonDecode for bool {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Bool(b) => Ok(b),
            _ => Err("expected bool".to_string()),
        }
    }
}

impl JsonDecode for u8 {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n as u8),
            _ => Err("expected number".to_string()),
        }
    }
}

impl JsonDecode for u16 {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n as u16),
            _ => Err("expected number".to_string()),
        }
    }
}

impl JsonDecode for u32 {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n as u32),
            _ => Err("expected number".to_string()),
        }
    }
}

impl JsonDecode for u64 {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n as u64),
            _ => Err("expected number".to_string()),
        }
    }
}

impl JsonDecode for usize {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n as usize),
            _ => Err("expected number".to_string()),
        }
    }
}

impl JsonDecode for i64 {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n as i64),
            _ => Err("expected number".to_string()),
        }
    }
}

impl JsonDecode for i32 {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n as i32),
            _ => Err("expected number".to_string()),
        }
    }
}

impl JsonDecode for f32 {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Number(n) => Ok(n as f32),
            _ => Err("expected number".to_string()),
        }
    }
}

impl<T: JsonDecode> JsonDecode for Vec<T> {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Array(values) => values.into_iter().map(T::from_json_value).collect(),
            _ => Err("expected array".to_string()),
        }
    }
}

impl<T: JsonDecode> JsonDecode for HashMap<String, T> {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Object(map) => map
                .into_iter()
                .map(|(k, v)| Ok((k, T::from_json_value(v)?)))
                .collect(),
            _ => Err("expected object".to_string()),
        }
    }
}

impl<T: JsonDecode> JsonDecode for Option<T> {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Null => Ok(None),
            other => Ok(Some(T::from_json_value(other)?)),
        }
    }
}

impl<const N: usize> JsonDecode for [u8; N] {
    fn from_json_value(value: Value) -> Result<Self, String> {
        match value {
            Value::Array(values) => {
                if values.len() != N {
                    return Err("invalid array length".to_string());
                }
                let mut out = [0u8; N];
                for (idx, val) in values.into_iter().enumerate() {
                    out[idx] = u8::from_json_value(val)?;
                }
                Ok(out)
            }
            _ => Err("expected array".to_string()),
        }
    }
}

pub fn to_value<T: JsonEncode + ?Sized>(value: &T) -> Value {
    value.to_json_value()
}

pub fn to_string<T: JsonEncode + ?Sized>(value: &T) -> Result<String, String> {
    Ok(to_value(value).to_string_compact())
}

pub fn to_string_pretty<T: JsonEncode + ?Sized>(value: &T) -> Result<String, String> {
    Ok(to_value(value).to_string_pretty())
}

pub fn to_vec<T: JsonEncode + ?Sized>(value: &T) -> Result<Vec<u8>, String> {
    Ok(to_string(value)?.into_bytes())
}

pub fn from_str<T: JsonDecode>(input: &str) -> Result<T, String> {
    let value = parse_json(input).map(Value::from)?;
    T::from_json_value(value)
}

pub fn from_slice<T: JsonDecode>(input: &[u8]) -> Result<T, String> {
    let s = std::str::from_utf8(input).map_err(|e| e.to_string())?;
    from_str(s)
}

pub fn from_value<T: JsonDecode>(value: Value) -> Result<T, String> {
    T::from_json_value(value)
}

#[macro_export]
macro_rules! json {
    (null) => {
        $crate::serde_json::Value::Null
    };
    ([ $( $elem:expr ),* $(,)? ]) => {
        $crate::serde_json::Value::Array(vec![ $( $crate::json!($elem) ),* ])
    };
    ({}) => {
        $crate::serde_json::Value::Object($crate::serde_json::Map::new())
    };
    ({ $( $key:literal : $value:expr ),* $(,)? }) => {{
        let mut map = $crate::serde_json::Map::new();
        $( map.insert($key.to_string(), $crate::json!($value)); )*
        $crate::serde_json::Value::Object(map)
    }};
    ($other:expr) => {
        $crate::serde_json::to_value(&$other)
    };
}

pub use crate::json;

#[cfg(test)]
mod json_macro_tests {
    use super::Value;
    use crate::json;

    #[test]
    fn object_macro_supports_empty_and_non_empty_objects() {
        assert_eq!(json!({}), Value::Object(Default::default()));

        let value = json!({ "name": "reddb", "ok": true });
        let Value::Object(map) = value else {
            panic!("non-empty object macro should produce an object");
        };
        assert_eq!(map.get("name"), Some(&Value::String("reddb".to_string())));
        assert_eq!(map.get("ok"), Some(&Value::Bool(true)));
    }
}
