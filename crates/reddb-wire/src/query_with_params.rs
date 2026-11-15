//! RedWire `QueryWithParams` payload codec.
//!
//! Payload layout v1:
//! `u32 sql_len` + UTF-8 SQL + `u32 param_count` + encoded values.

use std::fmt;

use reddb_types::{
    BiometricFormat, BiometricModality, BiometricSample, BitSignature, FeatureVector, LandmarkSet,
    MAX_BIT_SIGNATURE_BYTES, MAX_FEATURE_VECTOR_DIMENSIONS, MAX_LANDMARK_POINTS,
};

pub const FEATURE_PARAMS: u32 = 0x0000_0001;
/// Typed biometric and caller-supplied feature parameter envelopes.
pub const FEATURE_BIOMETRIC_PARAMS: u32 = 0x0000_0008;
/// Caller-supplied landmark parameter envelopes.
pub const FEATURE_LANDMARK_PARAMS: u32 = 0x0000_0010;
pub const MAX_PARAM_COUNT: usize = 65_536;

const OPTIONS_MAGIC: &[u8; 4] = b"RQP1";

const TAG_NULL: u8 = 0x00;
const TAG_BOOL: u8 = 0x01;
const TAG_INT: u8 = 0x02;
const TAG_FLOAT: u8 = 0x03;
const TAG_TEXT: u8 = 0x04;
const TAG_BYTES: u8 = 0x05;
const TAG_VECTOR: u8 = 0x06;
const TAG_JSON: u8 = 0x07;
const TAG_TIMESTAMP: u8 = 0x08;
const TAG_UUID: u8 = 0x09;
const TAG_FINGERPRINT: u8 = 0x0a;
const TAG_FACE: u8 = 0x0b;
const TAG_IRIS: u8 = 0x0c;
const TAG_FEATURE_VECTOR: u8 = 0x0d;
const TAG_BIT_SIGNATURE: u8 = 0x0e;
const TAG_LANDMARK_SET: u8 = 0x0f;

#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Vector(Vec<f32>),
    Json(Vec<u8>),
    Timestamp(i64),
    Uuid([u8; 16]),
    Fingerprint(BiometricSample),
    Face(BiometricSample),
    Iris(BiometricSample),
    FeatureVector(FeatureVector),
    BitSignature(BitSignature),
    LandmarkSet(LandmarkSet),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamCodecError {
    LengthOverflow(&'static str),
    ParamCountOverLimit(u32),
    Truncated(&'static str),
    InvalidUtf8(&'static str),
    InvalidBool(u8),
    UnknownTag(u8),
    InvalidOptionsMagic,
    TrailingBytes(usize),
    InvalidTypedValue,
}

impl fmt::Display for ParamCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LengthOverflow(field) => write!(f, "{field} is too large for RedWire v1"),
            Self::ParamCountOverLimit(count) => {
                write!(f, "param_count {count} exceeds RedWire v1 limit")
            }
            Self::Truncated(field) => write!(f, "truncated {field}"),
            Self::InvalidUtf8(field) => write!(f, "{field} must be valid UTF-8"),
            Self::InvalidBool(byte) => write!(f, "invalid bool payload byte {byte}"),
            Self::UnknownTag(tag) => write!(f, "unknown parameter value tag 0x{tag:02x}"),
            Self::InvalidOptionsMagic => write!(f, "invalid QueryWithParams options extension"),
            Self::TrailingBytes(count) => write!(f, "{count} trailing bytes after payload"),
            Self::InvalidTypedValue => write!(f, "invalid typed parameter value"),
        }
    }
}

impl std::error::Error for ParamCodecError {}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueryWithParamsOptions {
    pub commit_policy: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryWithParamsRequest {
    pub sql: String,
    pub params: Vec<ParamValue>,
    pub options: QueryWithParamsOptions,
}

pub fn encode_query_with_params(
    sql: &str,
    params: &[ParamValue],
) -> Result<Vec<u8>, ParamCodecError> {
    encode_query_with_params_request(sql, params, &QueryWithParamsOptions::default())
}

pub fn encode_query_with_params_request(
    sql: &str,
    params: &[ParamValue],
    options: &QueryWithParamsOptions,
) -> Result<Vec<u8>, ParamCodecError> {
    if sql.len() > u32::MAX as usize {
        return Err(ParamCodecError::LengthOverflow("sql"));
    }
    if params.len() > u32::MAX as usize {
        return Err(ParamCodecError::LengthOverflow("params"));
    }
    if params.len() > MAX_PARAM_COUNT {
        return Err(ParamCodecError::ParamCountOverLimit(params.len() as u32));
    }

    let mut out = Vec::new();
    out.extend_from_slice(&(sql.len() as u32).to_le_bytes());
    out.extend_from_slice(sql.as_bytes());
    out.extend_from_slice(&(params.len() as u32).to_le_bytes());
    for value in params {
        encode_value(value, &mut out)?;
    }
    if let Some(policy) = &options.commit_policy {
        if policy.len() > u32::MAX as usize {
            return Err(ParamCodecError::LengthOverflow("commit_policy"));
        }
        out.extend_from_slice(OPTIONS_MAGIC);
        write_len_prefixed(policy.as_bytes(), &mut out, "commit_policy")?;
    }
    Ok(out)
}

pub fn decode_query_with_params(
    payload: &[u8],
) -> Result<(String, Vec<ParamValue>), ParamCodecError> {
    let request = decode_query_with_params_request(payload)?;
    Ok((request.sql, request.params))
}

pub fn decode_query_with_params_request(
    payload: &[u8],
) -> Result<QueryWithParamsRequest, ParamCodecError> {
    let mut pos = 0;
    let sql_len = read_u32(payload, &mut pos, "sql_len")? as usize;
    let sql_bytes = read_bytes(payload, &mut pos, sql_len, "sql")?;
    let sql = std::str::from_utf8(sql_bytes)
        .map_err(|_| ParamCodecError::InvalidUtf8("sql"))?
        .to_string();
    let param_count = read_u32(payload, &mut pos, "param_count")?;
    if param_count as usize > MAX_PARAM_COUNT {
        return Err(ParamCodecError::ParamCountOverLimit(param_count));
    }
    let mut params = Vec::with_capacity(param_count as usize);
    for _ in 0..param_count {
        params.push(decode_value(payload, &mut pos)?);
    }
    let options = if pos == payload.len() {
        QueryWithParamsOptions::default()
    } else {
        decode_options(payload, &mut pos)?
    };
    if pos != payload.len() {
        return Err(ParamCodecError::TrailingBytes(payload.len() - pos));
    }
    Ok(QueryWithParamsRequest {
        sql,
        params,
        options,
    })
}

fn decode_options(
    payload: &[u8],
    pos: &mut usize,
) -> Result<QueryWithParamsOptions, ParamCodecError> {
    let magic = read_bytes(payload, pos, OPTIONS_MAGIC.len(), "options_magic")?;
    if magic != OPTIONS_MAGIC {
        return Err(ParamCodecError::InvalidOptionsMagic);
    }
    let len = read_u32(payload, pos, "commit_policy_len")? as usize;
    let bytes = read_bytes(payload, pos, len, "commit_policy")?;
    let commit_policy = std::str::from_utf8(bytes)
        .map_err(|_| ParamCodecError::InvalidUtf8("commit_policy"))?
        .to_string();
    Ok(QueryWithParamsOptions {
        commit_policy: Some(commit_policy),
    })
}

pub fn encode_value(value: &ParamValue, out: &mut Vec<u8>) -> Result<(), ParamCodecError> {
    match value {
        ParamValue::Null => out.push(TAG_NULL),
        ParamValue::Bool(value) => {
            out.push(TAG_BOOL);
            out.push(u8::from(*value));
        }
        ParamValue::Int(value) => {
            out.push(TAG_INT);
            out.extend_from_slice(&value.to_le_bytes());
        }
        ParamValue::Float(value) => {
            out.push(TAG_FLOAT);
            out.extend_from_slice(&value.to_le_bytes());
        }
        ParamValue::Text(value) => {
            out.push(TAG_TEXT);
            write_len_prefixed(value.as_bytes(), out, "text")?;
        }
        ParamValue::Bytes(value) => {
            out.push(TAG_BYTES);
            write_len_prefixed(value, out, "bytes")?;
        }
        ParamValue::Vector(values) => {
            out.push(TAG_VECTOR);
            if values.len() > u32::MAX as usize {
                return Err(ParamCodecError::LengthOverflow("vector"));
            }
            out.extend_from_slice(&(values.len() as u32).to_le_bytes());
            let bytes_len = values
                .len()
                .checked_mul(std::mem::size_of::<f32>())
                .ok_or(ParamCodecError::LengthOverflow("vector"))?;
            let start = out.len();
            let end = start
                .checked_add(bytes_len)
                .ok_or(ParamCodecError::LengthOverflow("vector"))?;
            out.resize(end, 0);
            for (bytes, value) in out[start..].chunks_exact_mut(4).zip(values) {
                bytes.copy_from_slice(&value.to_le_bytes());
            }
        }
        ParamValue::Json(value) => {
            out.push(TAG_JSON);
            write_len_prefixed(value, out, "json")?;
        }
        ParamValue::Timestamp(value) => {
            out.push(TAG_TIMESTAMP);
            out.extend_from_slice(&value.to_le_bytes());
        }
        ParamValue::Uuid(value) => {
            out.push(TAG_UUID);
            out.extend_from_slice(value);
        }
        ParamValue::Fingerprint(value) => {
            out.push(TAG_FINGERPRINT);
            write_biometric(value, BiometricModality::Fingerprint, out)?;
        }
        ParamValue::Face(value) => {
            out.push(TAG_FACE);
            write_biometric(value, BiometricModality::Face, out)?;
        }
        ParamValue::Iris(value) => {
            out.push(TAG_IRIS);
            write_biometric(value, BiometricModality::Iris, out)?;
        }
        ParamValue::FeatureVector(value) => {
            out.push(TAG_FEATURE_VECTOR);
            out.extend_from_slice(&value.space_id());
            out.extend_from_slice(&(value.dimensions() as u32).to_le_bytes());
            for coordinate in value.values() {
                out.extend_from_slice(&coordinate.to_le_bytes());
            }
        }
        ParamValue::BitSignature(value) => {
            out.push(TAG_BIT_SIGNATURE);
            out.extend_from_slice(&value.space_id());
            out.extend_from_slice(&(value.len_bytes() as u32).to_le_bytes());
            out.extend_from_slice(value.bits());
            out.extend_from_slice(value.mask());
        }
        ParamValue::LandmarkSet(value) => {
            out.push(TAG_LANDMARK_SET);
            out.extend_from_slice(&value.space_id());
            out.push(value.dimensions());
            out.extend_from_slice(&(value.pairs().len() as u32).to_le_bytes());
            out.extend_from_slice(&(value.midline().len() as u32).to_le_bytes());
            let dimensions = usize::from(value.dimensions());
            for (left, right) in value.pairs() {
                for coordinate in left.iter().take(dimensions) {
                    out.extend_from_slice(&coordinate.to_le_bytes());
                }
                for coordinate in right.iter().take(dimensions) {
                    out.extend_from_slice(&coordinate.to_le_bytes());
                }
            }
            for point in value.midline() {
                for coordinate in point.iter().take(dimensions) {
                    out.extend_from_slice(&coordinate.to_le_bytes());
                }
            }
        }
    }
    Ok(())
}

pub fn decode_value(payload: &[u8], pos: &mut usize) -> Result<ParamValue, ParamCodecError> {
    let tag = read_bytes(payload, pos, 1, "value tag")?[0];
    match tag {
        TAG_NULL => Ok(ParamValue::Null),
        TAG_BOOL => {
            let value = read_bytes(payload, pos, 1, "bool")?[0];
            match value {
                0 => Ok(ParamValue::Bool(false)),
                1 => Ok(ParamValue::Bool(true)),
                other => Err(ParamCodecError::InvalidBool(other)),
            }
        }
        TAG_INT => Ok(ParamValue::Int(read_i64(payload, pos, "int")?)),
        TAG_FLOAT => Ok(ParamValue::Float(f64::from_le_bytes(read_array(
            payload, pos, "float",
        )?))),
        TAG_TEXT => {
            let len = read_u32(payload, pos, "text_len")? as usize;
            let bytes = read_bytes(payload, pos, len, "text")?;
            let text = std::str::from_utf8(bytes)
                .map_err(|_| ParamCodecError::InvalidUtf8("text"))?
                .to_string();
            Ok(ParamValue::Text(text))
        }
        TAG_BYTES => {
            let len = read_u32(payload, pos, "bytes_len")? as usize;
            Ok(ParamValue::Bytes(
                read_bytes(payload, pos, len, "bytes")?.to_vec(),
            ))
        }
        TAG_VECTOR => {
            let len = read_u32(payload, pos, "vector_len")? as usize;
            let byte_len = len
                .checked_mul(4)
                .ok_or(ParamCodecError::LengthOverflow("vector"))?;
            ensure_remaining(payload, *pos, byte_len, "vector")?;
            let mut values = Vec::with_capacity(len);
            for _ in 0..len {
                values.push(f32::from_le_bytes(read_array(payload, pos, "vector")?));
            }
            Ok(ParamValue::Vector(values))
        }
        TAG_JSON => {
            let len = read_u32(payload, pos, "json_len")? as usize;
            Ok(ParamValue::Json(
                read_bytes(payload, pos, len, "json")?.to_vec(),
            ))
        }
        TAG_TIMESTAMP => Ok(ParamValue::Timestamp(read_i64(payload, pos, "timestamp")?)),
        TAG_UUID => Ok(ParamValue::Uuid(read_array(payload, pos, "uuid")?)),
        TAG_FINGERPRINT => read_biometric(payload, pos, BiometricModality::Fingerprint)
            .map(ParamValue::Fingerprint),
        TAG_FACE => read_biometric(payload, pos, BiometricModality::Face).map(ParamValue::Face),
        TAG_IRIS => read_biometric(payload, pos, BiometricModality::Iris).map(ParamValue::Iris),
        TAG_FEATURE_VECTOR => {
            let space_id = read_array(payload, pos, "feature_space_id")?;
            let dimensions = read_u32(payload, pos, "feature_dimensions")? as usize;
            if dimensions > MAX_FEATURE_VECTOR_DIMENSIONS {
                return Err(ParamCodecError::InvalidTypedValue);
            }
            let bytes_len = dimensions
                .checked_mul(4)
                .ok_or(ParamCodecError::InvalidTypedValue)?;
            ensure_remaining(payload, *pos, bytes_len, "feature_values")?;
            let mut values = Vec::with_capacity(dimensions);
            for _ in 0..dimensions {
                values.push(f32::from_le_bytes(read_array(
                    payload,
                    pos,
                    "feature_value",
                )?));
            }
            FeatureVector::new(space_id, values)
                .map(ParamValue::FeatureVector)
                .map_err(|_| ParamCodecError::InvalidTypedValue)
        }
        TAG_BIT_SIGNATURE => {
            let space_id = read_array(payload, pos, "bit_space_id")?;
            let len = read_u32(payload, pos, "bit_length")? as usize;
            if len > MAX_BIT_SIGNATURE_BYTES {
                return Err(ParamCodecError::InvalidTypedValue);
            }
            let combined = len
                .checked_mul(2)
                .ok_or(ParamCodecError::InvalidTypedValue)?;
            ensure_remaining(payload, *pos, combined, "bit_signature")?;
            let bits = read_bytes(payload, pos, len, "bit_values")?.to_vec();
            let mask = read_bytes(payload, pos, len, "bit_mask")?.to_vec();
            BitSignature::new(space_id, bits, Some(mask))
                .map(ParamValue::BitSignature)
                .map_err(|_| ParamCodecError::InvalidTypedValue)
        }
        TAG_LANDMARK_SET => {
            let space_id = read_array(payload, pos, "landmark_space_id")?;
            let dimensions = read_bytes(payload, pos, 1, "landmark_dimensions")?[0];
            if !matches!(dimensions, 2 | 3) {
                return Err(ParamCodecError::InvalidTypedValue);
            }
            let pair_count = read_u32(payload, pos, "landmark_pair_count")? as usize;
            let midline_count = read_u32(payload, pos, "landmark_midline_count")? as usize;
            let point_count = pair_count
                .checked_mul(2)
                .and_then(|count| count.checked_add(midline_count))
                .ok_or(ParamCodecError::InvalidTypedValue)?;
            if pair_count < 2 || point_count > MAX_LANDMARK_POINTS {
                return Err(ParamCodecError::InvalidTypedValue);
            }
            let byte_length = point_count
                .checked_mul(usize::from(dimensions))
                .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
                .ok_or(ParamCodecError::InvalidTypedValue)?;
            ensure_remaining(payload, *pos, byte_length, "landmark_coordinates")?;
            let read_point = |payload: &[u8], pos: &mut usize| {
                let mut point = [0.0_f32; 3];
                for coordinate in point.iter_mut().take(usize::from(dimensions)) {
                    *coordinate =
                        f32::from_le_bytes(read_array(payload, pos, "landmark_coordinate")?);
                }
                Ok::<_, ParamCodecError>(point)
            };
            let mut pairs = Vec::with_capacity(pair_count);
            for _ in 0..pair_count {
                pairs.push((read_point(payload, pos)?, read_point(payload, pos)?));
            }
            let mut midline = Vec::with_capacity(midline_count);
            for _ in 0..midline_count {
                midline.push(read_point(payload, pos)?);
            }
            LandmarkSet::new(space_id, dimensions, pairs, midline)
                .map(ParamValue::LandmarkSet)
                .map_err(|_| ParamCodecError::InvalidTypedValue)
        }
        other => Err(ParamCodecError::UnknownTag(other)),
    }
}

fn write_biometric(
    value: &BiometricSample,
    expected: BiometricModality,
    out: &mut Vec<u8>,
) -> Result<(), ParamCodecError> {
    if value.modality() != expected {
        return Err(ParamCodecError::InvalidTypedValue);
    }
    out.extend_from_slice(&(value.format() as u16).to_le_bytes());
    write_len_prefixed(value.bytes(), out, "biometric")
}

fn read_biometric(
    payload: &[u8],
    pos: &mut usize,
    expected: BiometricModality,
) -> Result<BiometricSample, ParamCodecError> {
    let format = BiometricFormat::from_code(u16::from_le_bytes(read_array(
        payload,
        pos,
        "biometric_format",
    )?))
    .ok_or(ParamCodecError::InvalidTypedValue)?;
    if format.modality() != expected {
        return Err(ParamCodecError::InvalidTypedValue);
    }
    let len = read_u32(payload, pos, "biometric_len")? as usize;
    let bytes = read_bytes(payload, pos, len, "biometric")?.to_vec();
    BiometricSample::with_format(format, bytes).map_err(|_| ParamCodecError::InvalidTypedValue)
}

fn write_len_prefixed(
    value: &[u8],
    out: &mut Vec<u8>,
    field: &'static str,
) -> Result<(), ParamCodecError> {
    if value.len() > u32::MAX as usize {
        return Err(ParamCodecError::LengthOverflow(field));
    }
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn read_u32(payload: &[u8], pos: &mut usize, field: &'static str) -> Result<u32, ParamCodecError> {
    Ok(u32::from_le_bytes(read_array(payload, pos, field)?))
}

fn read_i64(payload: &[u8], pos: &mut usize, field: &'static str) -> Result<i64, ParamCodecError> {
    Ok(i64::from_le_bytes(read_array(payload, pos, field)?))
}

fn read_array<const N: usize>(
    payload: &[u8],
    pos: &mut usize,
    field: &'static str,
) -> Result<[u8; N], ParamCodecError> {
    let bytes = read_bytes(payload, pos, N, field)?;
    let mut out = [0u8; N];
    out.copy_from_slice(bytes);
    Ok(out)
}

fn read_bytes<'a>(
    payload: &'a [u8],
    pos: &mut usize,
    len: usize,
    field: &'static str,
) -> Result<&'a [u8], ParamCodecError> {
    let end = pos
        .checked_add(len)
        .ok_or(ParamCodecError::Truncated(field))?;
    if end > payload.len() {
        return Err(ParamCodecError::Truncated(field));
    }
    let bytes = &payload[*pos..end];
    *pos = end;
    Ok(bytes)
}

fn ensure_remaining(
    payload: &[u8],
    pos: usize,
    len: usize,
    field: &'static str,
) -> Result<(), ParamCodecError> {
    let end = pos
        .checked_add(len)
        .ok_or(ParamCodecError::Truncated(field))?;
    if end > payload.len() {
        return Err(ParamCodecError::Truncated(field));
    }
    Ok(())
}

#[cfg(test)]
mod typed_value_tests {
    use super::*;

    fn iso_fmr() -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"FMR\0 20\0");
        bytes.extend_from_slice(&36_u32.to_be_bytes());
        bytes.extend_from_slice(&0_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&500_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&197_u16.to_be_bytes());
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&[1, 0, 100, 1]);
        bytes.extend_from_slice(&[0x40, 1, 0, 2, 3, 90]);
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    #[test]
    fn feature_and_bit_parameters_round_trip() {
        let space = [0x44; 16];
        let feature = FeatureVector::new(space, vec![1.0, -2.0]).expect("valid feature");
        let bits = BitSignature::new(space, vec![0xaa, 0x55], Some(vec![0xff, 0x0f]))
            .expect("valid signature");
        let values = vec![
            ParamValue::FeatureVector(feature),
            ParamValue::BitSignature(bits),
        ];
        let encoded = encode_query_with_params("SELECT $1", &values).expect("encode");
        let (sql, decoded) = decode_query_with_params(&encoded).expect("decode");
        assert_eq!(sql, "SELECT $1");
        assert_eq!(decoded, values);
    }

    #[test]
    fn landmark_parameter_tag_capability_and_round_trip_are_stable() {
        let landmarks = LandmarkSet::new(
            [0x66; 16],
            2,
            vec![
                ([-1.0, 1.0, 0.0], [1.0, 1.0, 0.0]),
                ([-2.0, -1.0, 0.0], [2.0, -1.0, 0.0]),
            ],
            vec![[0.0, 2.0, 0.0]],
        )
        .expect("valid landmarks");
        let values = vec![ParamValue::LandmarkSet(landmarks)];
        let encoded = encode_query_with_params("", &values).expect("encode");

        assert_eq!(FEATURE_LANDMARK_PARAMS, 0x0000_0010);
        assert_eq!(encoded[8], 0x0f);
        assert_eq!(
            decode_query_with_params(&encoded),
            Ok((String::new(), values))
        );

        for end in 0..encoded.len() {
            assert!(
                decode_query_with_params(&encoded[..end]).is_err(),
                "truncated RedWire landmark decoded at byte {end}"
            );
        }

        let mut excessive = encoded;
        excessive[26..30].copy_from_slice(&2_049_u32.to_le_bytes());
        assert!(decode_query_with_params(&excessive).is_err());
    }

    #[test]
    fn all_biometric_parameters_round_trip() {
        let fingerprint =
            BiometricSample::with_format(BiometricFormat::FingerprintIso19794_2, iso_fmr())
                .expect("valid fingerprint");
        let asn1 = vec![0x30, 0x03, 0x02, 0x01, 0x01];
        let face = BiometricSample::with_format(BiometricFormat::FaceIso39794_5, asn1.clone())
            .expect("valid face");
        let iris = BiometricSample::with_format(BiometricFormat::IrisIso39794_6, asn1)
            .expect("valid iris");
        let values = vec![
            ParamValue::Fingerprint(fingerprint),
            ParamValue::Face(face),
            ParamValue::Iris(iris),
        ];

        let encoded = encode_query_with_params("SELECT $1, $2, $3", &values).expect("encode");
        let (sql, decoded) = decode_query_with_params(&encoded).expect("decode");

        assert_eq!(sql, "SELECT $1, $2, $3");
        assert_eq!(decoded, values);
    }

    #[test]
    fn biometric_variant_must_match_sample_modality() {
        let face = BiometricSample::with_format(
            BiometricFormat::FaceIso39794_5,
            vec![0x30, 0x03, 0x02, 0x01, 0x01],
        )
        .expect("valid face");

        assert_eq!(
            encode_query_with_params("SELECT $1", &[ParamValue::Fingerprint(face)]),
            Err(ParamCodecError::InvalidTypedValue)
        );
    }
}
