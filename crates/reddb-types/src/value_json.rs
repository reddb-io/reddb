use crate::json::{from_slice as json_from_slice, Map, Value as JsonValue};
use crate::Value;

const MAX_JSON_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

impl Value {
    /// Render this value using RedDB's canonical JSON representation.
    ///
    /// Integers outside JavaScript's exactly representable range and fixed-point
    /// decimals use tagged objects so no precision is lost at a JSON boundary.
    pub fn to_json(&self) -> JsonValue {
        match self {
            Value::Null => JsonValue::Null,
            Value::Integer(value) => exact_i64_to_json(*value),
            Value::UnsignedInteger(value) => exact_u64_to_json(*value),
            Value::Float(value) => JsonValue::Number(*value),
            Value::Text(value) => JsonValue::String(value.to_string()),
            Value::Blob(value) => JsonValue::String(hex_encode(value)),
            Value::Boolean(value) => JsonValue::Bool(*value),
            Value::Timestamp(value) => exact_i64_to_json(*value),
            Value::Duration(value) => exact_i64_to_json(*value),
            Value::IpAddr(value) => JsonValue::String(value.to_string()),
            Value::MacAddr(value) => JsonValue::String(format_mac(value)),
            Value::Vector(value) => JsonValue::Array(
                value
                    .iter()
                    .map(|entry| JsonValue::Number(f64::from(*entry)))
                    .collect(),
            ),
            Value::Json(value) => json_bytes_to_json(value),
            Value::Uuid(value) => JsonValue::String(hex_encode(value)),
            Value::NodeRef(value) | Value::EdgeRef(value) => JsonValue::String(value.clone()),
            Value::VectorRef(collection, id) => JsonValue::Object(
                [
                    (
                        "collection".to_string(),
                        JsonValue::String(collection.clone()),
                    ),
                    ("id".to_string(), exact_u64_to_json(*id)),
                ]
                .into_iter()
                .collect(),
            ),
            Value::RowRef(table, row_id) => JsonValue::Object(
                [
                    ("table".to_string(), JsonValue::String(table.clone())),
                    ("row_id".to_string(), exact_u64_to_json(*row_id)),
                ]
                .into_iter()
                .collect(),
            ),
            Value::Color([r, g, b]) => {
                JsonValue::String(format!("#{r:02X}{g:02X}{b:02X}"))
            }
            Value::Email(value) | Value::Url(value) => JsonValue::String(value.clone()),
            Value::Phone(value) => exact_u64_to_json(*value),
            Value::Semver(packed) => JsonValue::String(format!(
                "{}.{}.{}",
                packed / 1_000_000,
                (packed / 1_000) % 1_000,
                packed % 1_000
            )),
            Value::Cidr(ip, prefix) => JsonValue::String(format!(
                "{}.{}.{}.{}/{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF,
                prefix
            )),
            Value::Date(days) => JsonValue::Integer(i64::from(*days)),
            Value::Time(milliseconds) => JsonValue::Integer(i64::from(*milliseconds)),
            Value::Decimal(_) => exact_decimal_to_json(self.display_string()),
            Value::DecimalText(value) => exact_decimal_to_json(value.clone()),
            Value::EnumValue(value) => JsonValue::Integer(i64::from(*value)),
            Value::Array(values) => {
                JsonValue::Array(values.iter().map(Value::to_json).collect())
            }
            Value::TimestampMs(value) => exact_i64_to_json(*value),
            Value::Ipv4(ip) => JsonValue::String(format_ipv4(*ip)),
            Value::Ipv6(bytes) => {
                JsonValue::String(std::net::Ipv6Addr::from(*bytes).to_string())
            }
            Value::Subnet(ip, mask) => {
                JsonValue::String(format!("{}/{}", format_ipv4(*ip), mask.leading_ones()))
            }
            Value::Port(value) => JsonValue::Integer(i64::from(*value)),
            Value::Latitude(microdegrees) | Value::Longitude(microdegrees) => {
                JsonValue::Number(f64::from(*microdegrees) / 1_000_000.0)
            }
            Value::GeoPoint(latitude, longitude) => JsonValue::String(format!(
                "{:.6},{:.6}",
                f64::from(*latitude) / 1_000_000.0,
                f64::from(*longitude) / 1_000_000.0
            )),
            Value::Country2(code) | Value::Lang2(code) => {
                JsonValue::String(String::from_utf8_lossy(code).to_string())
            }
            Value::Country3(code) | Value::Currency(code) => {
                JsonValue::String(String::from_utf8_lossy(code).to_string())
            }
            Value::Lang5(code) => {
                JsonValue::String(String::from_utf8_lossy(code).to_string())
            }
            Value::AssetCode(code) => JsonValue::String(code.clone()),
            Value::Money {
                asset_code,
                minor_units,
                scale,
            } => JsonValue::Object(
                [
                    (
                        "asset_code".to_string(),
                        JsonValue::String(asset_code.clone()),
                    ),
                    (
                        "minor_units".to_string(),
                        exact_i64_to_json(*minor_units),
                    ),
                    (
                        "scale".to_string(),
                        exact_i64_to_json(i64::from(*scale)),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            Value::ColorAlpha([r, g, b, a]) => {
                JsonValue::String(format!("#{r:02X}{g:02X}{b:02X}{a:02X}"))
            }
            Value::BigInt(value) => exact_i64_to_json(*value),
            Value::KeyRef(collection, key) => JsonValue::Object(
                [
                    (
                        "collection".to_string(),
                        JsonValue::String(collection.clone()),
                    ),
                    ("key".to_string(), JsonValue::String(key.clone())),
                ]
                .into_iter()
                .collect(),
            ),
            Value::DocRef(collection, id) => JsonValue::Object(
                [
                    (
                        "collection".to_string(),
                        JsonValue::String(collection.clone()),
                    ),
                    ("id".to_string(), exact_u64_to_json(*id)),
                ]
                .into_iter()
                .collect(),
            ),
            Value::TableRef(name) => JsonValue::String(name.clone()),
            Value::PageRef(page_id) => exact_u64_to_json(u64::from(*page_id)),
            Value::Secret(_) | Value::Password(_) => JsonValue::String("***".to_string()),
        }
    }
}

fn exact_i64_to_json(value: i64) -> JsonValue {
    if (-MAX_JSON_SAFE_INTEGER..=MAX_JSON_SAFE_INTEGER).contains(&value) {
        JsonValue::Integer(value)
    } else {
        single_key_object("$int", JsonValue::String(value.to_string()))
    }
}

fn exact_u64_to_json(value: u64) -> JsonValue {
    if value <= MAX_JSON_SAFE_INTEGER as u64 {
        JsonValue::Integer(value as i64)
    } else {
        single_key_object("$uint", JsonValue::String(value.to_string()))
    }
}

fn exact_decimal_to_json(value: String) -> JsonValue {
    single_key_object("$decimal", JsonValue::String(value))
}

fn single_key_object(key: &str, value: JsonValue) -> JsonValue {
    JsonValue::Object([(key.to_string(), value)].into_iter().collect())
}

fn json_bytes_to_json(bytes: &[u8]) -> JsonValue {
    if bytes.starts_with(crate::document_body_codec::MAGIC) {
        if let Ok(fields) = crate::document_body_codec::decode(bytes) {
            let mut object = Map::new();
            for (key, value) in fields {
                let json = match value {
                    Value::Integer(value) => JsonValue::Integer(value),
                    value => value.to_json(),
                };
                object.insert(key, json);
            }
            return JsonValue::Object(object);
        }
    }

    json_from_slice::<JsonValue>(bytes).unwrap_or_else(|_| {
        JsonValue::Object(
            [
                (
                    "code".to_string(),
                    JsonValue::String("INVALID_JSON".to_string()),
                ),
                ("hex".to_string(), JsonValue::String(hex_encode(bytes))),
            ]
            .into_iter()
            .collect(),
        )
    })
}

fn format_mac(bytes: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    )
}

fn format_ipv4(ip: u32) -> String {
    format!(
        "{}.{}.{}.{}",
        (ip >> 24) & 0xFF,
        (ip >> 16) & 0xFF,
        (ip >> 8) & 0xFF,
        ip & 0xFF
    )
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(*byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(*byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv6Addr};

    use crate::Value;

    #[test]
    fn every_value_variant_has_canonical_json() {
        let cases = [
            ("null", Value::Null, "null"),
            ("integer", Value::Integer(42), "42"),
            (
                "large integer",
                Value::Integer(9_007_199_254_740_992),
                r#"{"$int":"9007199254740992"}"#,
            ),
            ("unsigned integer", Value::UnsignedInteger(7), "7"),
            (
                "large unsigned integer",
                Value::UnsignedInteger(u64::MAX),
                r#"{"$uint":"18446744073709551615"}"#,
            ),
            ("float", Value::Float(1.5), "1.5"),
            ("text", Value::text("hello"), r#""hello""#),
            ("blob", Value::Blob(vec![0x00, 0xff]), r#""00ff""#),
            ("boolean", Value::Boolean(true), "true"),
            ("timestamp", Value::Timestamp(1_700_000_000), "1700000000"),
            ("duration", Value::Duration(-500), "-500"),
            (
                "IP address",
                Value::IpAddr(IpAddr::V6(Ipv6Addr::LOCALHOST)),
                r#""::1""#,
            ),
            (
                "MAC address",
                Value::MacAddr([0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]),
                r#""00:1a:2b:3c:4d:5e""#,
            ),
            (
                "vector",
                Value::Vector(vec![1.5, -2.0]),
                "[1.5,-2]",
            ),
            (
                "JSON",
                Value::Json(br#"{"nested":[1,2.5],"text":"ok"}"#.to_vec()),
                r#"{"nested":[1,2.5],"text":"ok"}"#,
            ),
            (
                "invalid JSON",
                Value::Json(vec![0xff]),
                r#"{"code":"INVALID_JSON","hex":"ff"}"#,
            ),
            (
                "UUID",
                Value::Uuid([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
                r#""000102030405060708090a0b0c0d0e0f""#,
            ),
            ("node reference", Value::NodeRef("node:1".into()), r#""node:1""#),
            ("edge reference", Value::EdgeRef("edge:1".into()), r#""edge:1""#),
            (
                "vector reference",
                Value::VectorRef("embeddings".into(), 7),
                r#"{"collection":"embeddings","id":7}"#,
            ),
            (
                "row reference",
                Value::RowRef("users".into(), 8),
                r#"{"row_id":8,"table":"users"}"#,
            ),
            ("color", Value::Color([0x12, 0xab, 0xff]), "\"#12ABFF\""),
            ("email", Value::Email("a@b.test".into()), r#""a@b.test""#),
            ("URL", Value::Url("https://example.test".into()), r#""https://example.test""#),
            ("phone", Value::Phone(55_119_999), "55119999"),
            ("semver", Value::Semver(1_002_003), r#""1.2.3""#),
            ("CIDR", Value::Cidr(0xc0a8_0100, 24), r#""192.168.1.0/24""#),
            ("date", Value::Date(20_000), "20000"),
            ("time", Value::Time(3_723_000), "3723000"),
            (
                "decimal",
                Value::Decimal(1_234_567),
                r#"{"$decimal":"123.4567"}"#,
            ),
            (
                "decimal text",
                Value::DecimalText("3.141592653589793238462643383279".into()),
                r#"{"$decimal":"3.141592653589793238462643383279"}"#,
            ),
            ("enum", Value::EnumValue(3), "3"),
            (
                "array",
                Value::Array(vec![Value::Boolean(false), Value::Array(vec![Value::Integer(2)])]),
                "[false,[2]]",
            ),
            ("millisecond timestamp", Value::TimestampMs(1_700_000_000_123), "1700000000123"),
            ("IPv4", Value::Ipv4(0x7f00_0001), r#""127.0.0.1""#),
            (
                "IPv6",
                Value::Ipv6(Ipv6Addr::LOCALHOST.octets()),
                r#""::1""#,
            ),
            (
                "subnet",
                Value::Subnet(0x0a00_0000, 0xff00_0000),
                r#""10.0.0.0/8""#,
            ),
            ("port", Value::Port(5432), "5432"),
            ("latitude", Value::Latitude(12_345_678), "12.345678"),
            ("longitude", Value::Longitude(-87_654_321), "-87.654321"),
            (
                "geo point",
                Value::GeoPoint(12_345_678, -87_654_321),
                r#""12.345678,-87.654321""#,
            ),
            ("country-2", Value::Country2(*b"BR"), r#""BR""#),
            ("country-3", Value::Country3(*b"BRA"), r#""BRA""#),
            ("language-2", Value::Lang2(*b"pt"), r#""pt""#),
            ("language-5", Value::Lang5(*b"pt-BR"), r#""pt-BR""#),
            ("currency", Value::Currency(*b"BRL"), r#""BRL""#),
            ("asset code", Value::AssetCode("BTC".into()), r#""BTC""#),
            (
                "money",
                Value::Money {
                    asset_code: "BRL".into(),
                    minor_units: 12_345,
                    scale: 2,
                },
                r#"{"asset_code":"BRL","minor_units":12345,"scale":2}"#,
            ),
            (
                "color with alpha",
                Value::ColorAlpha([0x12, 0xab, 0xff, 0x80]),
                "\"#12ABFF80\"",
            ),
            (
                "big integer",
                Value::BigInt(i64::MIN),
                r#"{"$int":"-9223372036854775808"}"#,
            ),
            (
                "key reference",
                Value::KeyRef("settings".into(), "theme".into()),
                r#"{"collection":"settings","key":"theme"}"#,
            ),
            (
                "document reference",
                Value::DocRef("profiles".into(), 9),
                r#"{"collection":"profiles","id":9}"#,
            ),
            ("table reference", Value::TableRef("users".into()), r#""users""#),
            ("page reference", Value::PageRef(17), "17"),
            ("secret", Value::Secret(vec![1, 2, 3]), r#""***""#),
            ("password", Value::Password("hash".into()), r#""***""#),
        ];

        for (name, value, expected) in cases {
            assert_eq!(value.to_json().to_string_compact(), expected, "{name}");
        }
    }
}
