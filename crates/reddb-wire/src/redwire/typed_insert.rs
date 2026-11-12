//! RedWire typed single-row insert payload.
//!
//! V1 layout:
//! `[version:u8][collection_len:u16][collection][field_count:u16]`
//! followed by `field_count` entries of
//! `[name_len:u16][name][query_with_params::ParamValue]`.

use std::collections::BTreeSet;
use std::fmt;

use crate::query_with_params::{decode_value, encode_value, ParamCodecError, ParamValue};

pub const FEATURE_TYPED_MUTATIONS: u32 = 0x0000_0004;
pub const TYPED_INSERT_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct TypedInsertField {
    pub name: String,
    pub value: ParamValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypedInsertRequest {
    pub collection: String,
    pub fields: Vec<TypedInsertField>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypedInsertError {
    UnsupportedVersion(u8),
    LengthOverflow(&'static str),
    Truncated(&'static str),
    InvalidUtf8(&'static str),
    EmptyCollection,
    EmptyFieldName,
    DuplicateField(String),
    Value(ParamCodecError),
    TrailingBytes(usize),
}

impl fmt::Display for TypedInsertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported typed insert version {version}")
            }
            Self::LengthOverflow(field) => write!(f, "{field} is too large for typed insert v1"),
            Self::Truncated(field) => write!(f, "truncated {field}"),
            Self::InvalidUtf8(field) => write!(f, "{field} must be valid UTF-8"),
            Self::EmptyCollection => write!(f, "collection cannot be empty"),
            Self::EmptyFieldName => write!(f, "field name cannot be empty"),
            Self::DuplicateField(name) => write!(f, "duplicate field '{name}'"),
            Self::Value(error) => write!(f, "invalid field value: {error}"),
            Self::TrailingBytes(count) => write!(f, "{count} trailing bytes after typed insert"),
        }
    }
}

impl std::error::Error for TypedInsertError {}

pub fn encode_typed_insert(request: &TypedInsertRequest) -> Result<Vec<u8>, TypedInsertError> {
    validate_name(&request.collection, "collection")?;
    let collection_len = u16::try_from(request.collection.len())
        .map_err(|_| TypedInsertError::LengthOverflow("collection"))?;
    let field_count = u16::try_from(request.fields.len())
        .map_err(|_| TypedInsertError::LengthOverflow("fields"))?;

    let mut names = BTreeSet::new();
    let mut out = Vec::new();
    out.push(TYPED_INSERT_VERSION);
    out.extend_from_slice(&collection_len.to_le_bytes());
    out.extend_from_slice(request.collection.as_bytes());
    out.extend_from_slice(&field_count.to_le_bytes());
    for field in &request.fields {
        validate_name(&field.name, "field name")?;
        if !names.insert(field.name.as_str()) {
            return Err(TypedInsertError::DuplicateField(field.name.clone()));
        }
        let name_len = u16::try_from(field.name.len())
            .map_err(|_| TypedInsertError::LengthOverflow("field name"))?;
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(field.name.as_bytes());
        encode_value(&field.value, &mut out).map_err(TypedInsertError::Value)?;
    }
    Ok(out)
}

pub fn decode_typed_insert(payload: &[u8]) -> Result<TypedInsertRequest, TypedInsertError> {
    let mut pos = 0;
    let version = read_bytes(payload, &mut pos, 1, "version")?[0];
    if version != TYPED_INSERT_VERSION {
        return Err(TypedInsertError::UnsupportedVersion(version));
    }

    let collection_len = read_u16(payload, &mut pos, "collection_len")? as usize;
    let collection = read_string(payload, &mut pos, collection_len, "collection")?;
    validate_name(&collection, "collection")?;

    let field_count = read_u16(payload, &mut pos, "field_count")? as usize;
    let minimum_field_bytes = field_count
        .checked_mul(3)
        .ok_or(TypedInsertError::Truncated("fields"))?;
    if payload.len().saturating_sub(pos) < minimum_field_bytes {
        return Err(TypedInsertError::Truncated("fields"));
    }

    let mut names = BTreeSet::new();
    let mut fields = Vec::with_capacity(field_count);
    for _ in 0..field_count {
        let name_len = read_u16(payload, &mut pos, "field_name_len")? as usize;
        let name = read_string(payload, &mut pos, name_len, "field name")?;
        validate_name(&name, "field name")?;
        if !names.insert(name.clone()) {
            return Err(TypedInsertError::DuplicateField(name));
        }
        let value = decode_value(payload, &mut pos).map_err(TypedInsertError::Value)?;
        fields.push(TypedInsertField { name, value });
    }
    if pos != payload.len() {
        return Err(TypedInsertError::TrailingBytes(payload.len() - pos));
    }
    Ok(TypedInsertRequest { collection, fields })
}

fn validate_name(value: &str, field: &'static str) -> Result<(), TypedInsertError> {
    if value.trim().is_empty() {
        return Err(if field == "collection" {
            TypedInsertError::EmptyCollection
        } else {
            TypedInsertError::EmptyFieldName
        });
    }
    Ok(())
}

fn read_u16(payload: &[u8], pos: &mut usize, field: &'static str) -> Result<u16, TypedInsertError> {
    let bytes = read_bytes(payload, pos, 2, field)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_string(
    payload: &[u8],
    pos: &mut usize,
    len: usize,
    field: &'static str,
) -> Result<String, TypedInsertError> {
    let bytes = read_bytes(payload, pos, len, field)?;
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| TypedInsertError::InvalidUtf8(field))
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    field: &'static str,
) -> Result<&'a [u8], TypedInsertError> {
    let end = pos
        .checked_add(len)
        .ok_or(TypedInsertError::Truncated(field))?;
    if end > payload.len() {
        return Err(TypedInsertError::Truncated(field));
    }
    let bytes = &payload[*pos..end];
    *pos = end;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_values_request() -> TypedInsertRequest {
        TypedInsertRequest {
            collection: "events".to_string(),
            fields: vec![
                TypedInsertField {
                    name: "null".into(),
                    value: ParamValue::Null,
                },
                TypedInsertField {
                    name: "bool".into(),
                    value: ParamValue::Bool(true),
                },
                TypedInsertField {
                    name: "int".into(),
                    value: ParamValue::Int(-7),
                },
                TypedInsertField {
                    name: "float".into(),
                    value: ParamValue::Float(1.5),
                },
                TypedInsertField {
                    name: "text".into(),
                    value: ParamValue::Text("olá".into()),
                },
                TypedInsertField {
                    name: "bytes".into(),
                    value: ParamValue::Bytes(vec![0, 255]),
                },
                TypedInsertField {
                    name: "vector".into(),
                    value: ParamValue::Vector(vec![1.0, -2.0]),
                },
                TypedInsertField {
                    name: "json".into(),
                    value: ParamValue::Json(br#"{"ok":true}"#.to_vec()),
                },
                TypedInsertField {
                    name: "timestamp".into(),
                    value: ParamValue::Timestamp(123),
                },
                TypedInsertField {
                    name: "uuid".into(),
                    value: ParamValue::Uuid([0xAB; 16]),
                },
            ],
        }
    }

    #[test]
    fn round_trips_all_parameter_value_variants() {
        let request = all_values_request();
        let encoded = encode_typed_insert(&request).expect("encode");
        assert_eq!(decode_typed_insert(&encoded).expect("decode"), request);
    }

    #[test]
    fn simple_request_bytes_are_pinned_for_other_drivers() {
        let request = TypedInsertRequest {
            collection: "users".into(),
            fields: vec![
                TypedInsertField {
                    name: "name".into(),
                    value: ParamValue::Text("Ada".into()),
                },
                TypedInsertField {
                    name: "age".into(),
                    value: ParamValue::Int(37),
                },
            ],
        };
        assert_eq!(
            encode_typed_insert(&request).expect("encode"),
            vec![
                0x01, 0x05, 0x00, b'u', b's', b'e', b'r', b's', 0x02, 0x00, 0x04, 0x00, b'n', b'a',
                b'm', b'e', 0x04, 0x03, 0x00, 0x00, 0x00, b'A', b'd', b'a', 0x03, 0x00, b'a', b'g',
                b'e', 0x02, 0x25, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            ]
        );
    }

    #[test]
    fn preserves_empty_field_set_for_default_only_rows() {
        let request = TypedInsertRequest {
            collection: "defaults".into(),
            fields: Vec::new(),
        };
        let encoded = encode_typed_insert(&request).expect("encode");
        assert_eq!(decode_typed_insert(&encoded).expect("decode"), request);
    }

    #[test]
    fn rejects_duplicate_empty_and_trailing_fields() {
        let duplicate = TypedInsertRequest {
            collection: "events".into(),
            fields: vec![
                TypedInsertField {
                    name: "id".into(),
                    value: ParamValue::Int(1),
                },
                TypedInsertField {
                    name: "id".into(),
                    value: ParamValue::Int(2),
                },
            ],
        };
        assert!(
            matches!(encode_typed_insert(&duplicate), Err(TypedInsertError::DuplicateField(name)) if name == "id")
        );

        let empty = TypedInsertRequest {
            collection: "events".into(),
            fields: vec![TypedInsertField {
                name: " ".into(),
                value: ParamValue::Null,
            }],
        };
        assert_eq!(
            encode_typed_insert(&empty),
            Err(TypedInsertError::EmptyFieldName)
        );

        let mut encoded = encode_typed_insert(&all_values_request()).expect("encode");
        encoded.push(0);
        assert_eq!(
            decode_typed_insert(&encoded),
            Err(TypedInsertError::TrailingBytes(1))
        );
    }

    #[test]
    fn rejects_bad_version_utf8_and_truncation() {
        assert_eq!(
            decode_typed_insert(&[2]),
            Err(TypedInsertError::UnsupportedVersion(2))
        );
        assert_eq!(
            decode_typed_insert(&[1, 1, 0, 0xFF, 0, 0]),
            Err(TypedInsertError::InvalidUtf8("collection"))
        );

        let encoded = encode_typed_insert(&all_values_request()).expect("encode");
        for len in 0..encoded.len() {
            assert!(
                decode_typed_insert(&encoded[..len]).is_err(),
                "prefix {len} unexpectedly decoded"
            );
        }
    }
}
