//! Per-column compression codecs — ClickHouse-parity layer.
//!
//! Rows still land in the unified page / btree path; this module adds
//! a typed codec pipeline so a column segment can be written with
//! `CODEC(Delta, ZSTD(3))` syntax and read back by chaining the
//! decoders in reverse. Callers bring their own segment layout — the
//! module is intentionally format-agnostic and operates on `&[u8]`
//! buffers plus the column's logical element type.
//!
//! # Supported codecs
//!
//! | Codec          | Best on                           | Notes |
//! |----------------|-----------------------------------|-------|
//! | `None`         | small / already-compressed data   | |
//! | `Lz4`          | generic fast compression          | requires `lz4_flex` |
//! | `Zstd { lvl }` | generic, higher ratio than LZ4    | reuses existing `zstd` crate |
//! | `Delta`        | monotonic or near-monotonic ints  | reuses `t64_encode` for residuals |
//! | `DoubleDelta`  | regular time-series timestamps    | reuses existing DoD path |
//! | `Dict`         | low-cardinality strings / ints    | builds inline dictionary |
//!
//! Codecs chain: callers compose a `Vec<ColumnCodec>` and apply them
//! outer → inner on encode, inner → outer on decode. The pipeline
//! header records every codec used so downstream readers don't need
//! the schema declaration — just the byte buffer.

use crate::storage::timeseries::compression::{
    delta_decode_timestamps, delta_encode_timestamps, t64_decode, t64_encode, zstd_compress,
    zstd_decompress,
};

/// Codec identifiers. Stored as a single byte in the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnCodec {
    None,
    Lz4,
    Zstd { level: i32 },
    Delta,
    DoubleDelta,
    Dict,
}

impl ColumnCodec {
    fn tag(&self) -> u8 {
        match self {
            ColumnCodec::None => 0,
            ColumnCodec::Lz4 => 1,
            ColumnCodec::Zstd { .. } => 2,
            ColumnCodec::Delta => 3,
            ColumnCodec::DoubleDelta => 4,
            ColumnCodec::Dict => 5,
        }
    }

    fn from_tag(tag: u8) -> Option<ColumnCodec> {
        match tag {
            0 => Some(ColumnCodec::None),
            1 => Some(ColumnCodec::Lz4),
            2 => Some(ColumnCodec::Zstd { level: 3 }),
            3 => Some(ColumnCodec::Delta),
            4 => Some(ColumnCodec::DoubleDelta),
            5 => Some(ColumnCodec::Dict),
            _ => None,
        }
    }
}

/// Errors surfaced by the codec pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// Buffer ended before the declared length / header.
    Truncated,
    /// Header references a codec tag we don't know.
    UnknownCodec(u8),
    /// Payload shape didn't match the codec (e.g. odd bytes for `i64`
    /// stream, dictionary size mismatch).
    InvalidPayload(&'static str),
    /// Upstream crate (lz4_flex / zstd) reported a failure.
    Backend(String),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Truncated => write!(f, "codec buffer truncated"),
            CodecError::UnknownCodec(t) => write!(f, "unknown column codec tag: {t}"),
            CodecError::InvalidPayload(why) => write!(f, "invalid codec payload: {why}"),
            CodecError::Backend(msg) => write!(f, "codec backend error: {msg}"),
        }
    }
}

impl std::error::Error for CodecError {}

pub type CodecResult<T> = Result<T, CodecError>;

// -------------------------------------------------------------------------
// Generic byte-stream codecs — apply to any serialised column.
// -------------------------------------------------------------------------

/// Apply every codec in order (first codec sees raw bytes, last codec
/// sees output of the previous one). Header format:
///
/// ```text
/// u16 codec_count  u8 codec_tag_1 [i32 param_1] …  payload_bytes
/// ```
pub fn encode_bytes(codecs: &[ColumnCodec], raw: &[u8]) -> CodecResult<Vec<u8>> {
    let mut buf = raw.to_vec();
    for codec in codecs {
        buf = apply_encode(codec, &buf)?;
    }
    // Prepend header.
    let mut out = Vec::with_capacity(buf.len() + codecs.len() * 2 + 2);
    out.extend_from_slice(&(codecs.len() as u16).to_le_bytes());
    for codec in codecs {
        out.push(codec.tag());
        if let ColumnCodec::Zstd { level } = codec {
            out.extend_from_slice(&level.to_le_bytes());
        }
    }
    out.extend_from_slice(&buf);
    Ok(out)
}

/// Inverse of [`encode_bytes`].
pub fn decode_bytes(buf: &[u8]) -> CodecResult<Vec<u8>> {
    if buf.len() < 2 {
        return Err(CodecError::Truncated);
    }
    let count = u16::from_le_bytes([buf[0], buf[1]]) as usize;
    let mut cursor = 2;
    let mut codecs: Vec<ColumnCodec> = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor >= buf.len() {
            return Err(CodecError::Truncated);
        }
        let tag = buf[cursor];
        cursor += 1;
        let codec = match tag {
            2 => {
                // Zstd level parameter follows.
                if cursor + 4 > buf.len() {
                    return Err(CodecError::Truncated);
                }
                let level = i32::from_le_bytes(buf[cursor..cursor + 4].try_into().unwrap());
                cursor += 4;
                ColumnCodec::Zstd { level }
            }
            other => ColumnCodec::from_tag(other).ok_or(CodecError::UnknownCodec(other))?,
        };
        codecs.push(codec);
    }
    let mut payload = buf[cursor..].to_vec();
    // Decode in reverse order.
    for codec in codecs.iter().rev() {
        payload = apply_decode(codec, &payload)?;
    }
    Ok(payload)
}

fn apply_encode(codec: &ColumnCodec, data: &[u8]) -> CodecResult<Vec<u8>> {
    match codec {
        ColumnCodec::None => Ok(data.to_vec()),
        ColumnCodec::Lz4 => {
            let mut out = (data.len() as u32).to_le_bytes().to_vec();
            out.extend(lz4_flex::compress(data));
            Ok(out)
        }
        ColumnCodec::Zstd { level } => Ok(zstd_compress_at_inner(data, *level)),
        ColumnCodec::Delta | ColumnCodec::DoubleDelta => {
            // i64 stream required.
            if data.len() % 8 != 0 {
                return Err(CodecError::InvalidPayload("delta expects i64 stream"));
            }
            let values: Vec<u64> = data
                .chunks_exact(8)
                .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
                .collect();
            let encoded = delta_encode_timestamps(&values);
            // Pack as (count:u32) + i64s.
            let mut out = Vec::with_capacity(4 + encoded.len() * 8);
            out.extend_from_slice(&(encoded.len() as u32).to_le_bytes());
            for v in encoded {
                out.extend_from_slice(&v.to_le_bytes());
            }
            Ok(out)
        }
        ColumnCodec::Dict => encode_dict(data),
    }
}

fn apply_decode(codec: &ColumnCodec, data: &[u8]) -> CodecResult<Vec<u8>> {
    match codec {
        ColumnCodec::None => Ok(data.to_vec()),
        ColumnCodec::Lz4 => {
            if data.len() < 4 {
                return Err(CodecError::Truncated);
            }
            let raw_len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
            lz4_flex::decompress(&data[4..], raw_len)
                .map_err(|e| CodecError::Backend(e.to_string()))
        }
        ColumnCodec::Zstd { .. } => {
            zstd_decompress(data).ok_or(CodecError::InvalidPayload("zstd payload malformed"))
        }
        ColumnCodec::Delta | ColumnCodec::DoubleDelta => {
            if data.len() < 4 {
                return Err(CodecError::Truncated);
            }
            let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
            let payload = &data[4..];
            if payload.len() < count * 8 {
                return Err(CodecError::Truncated);
            }
            let encoded: Vec<i64> = payload
                .chunks_exact(8)
                .take(count)
                .map(|c| i64::from_le_bytes(c.try_into().unwrap()))
                .collect();
            let decoded = delta_decode_timestamps(&encoded);
            let mut out = Vec::with_capacity(decoded.len() * 8);
            for v in decoded {
                out.extend_from_slice(&v.to_le_bytes());
            }
            Ok(out)
        }
        ColumnCodec::Dict => decode_dict(data),
    }
}

// Wrapper so we can pass the explicit level without changing the
// timeseries public surface.
fn zstd_compress_at_inner(data: &[u8], level: i32) -> Vec<u8> {
    use crate::storage::timeseries::compression::zstd_compress_at;
    zstd_compress_at(data, level)
}

// -------------------------------------------------------------------------
// Dictionary codec — builds an inline dictionary of u32 indexes.
// Assumes the input is a length-prefixed string stream:
// `[u32 count] ( [u16 len] [bytes] )*` (matches how callers already
// serialise TEXT columns in segment payloads).
// -------------------------------------------------------------------------

fn encode_dict(data: &[u8]) -> CodecResult<Vec<u8>> {
    if data.len() < 4 {
        return Err(CodecError::Truncated);
    }
    let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let mut cursor = 4usize;
    let mut dict: Vec<Vec<u8>> = Vec::new();
    let mut indexes: Vec<u32> = Vec::with_capacity(count);
    for _ in 0..count {
        if cursor + 2 > data.len() {
            return Err(CodecError::Truncated);
        }
        let len = u16::from_le_bytes(data[cursor..cursor + 2].try_into().unwrap()) as usize;
        cursor += 2;
        if cursor + len > data.len() {
            return Err(CodecError::Truncated);
        }
        let slice = &data[cursor..cursor + len];
        cursor += len;
        let idx = match dict.iter().position(|v| v == slice) {
            Some(p) => p as u32,
            None => {
                dict.push(slice.to_vec());
                (dict.len() - 1) as u32
            }
        };
        indexes.push(idx);
    }
    // Layout: [u32 dict_count] ( [u16 len] [bytes] )* [u32 idx_count] [idx: u32]*
    let mut out = Vec::new();
    out.extend_from_slice(&(dict.len() as u32).to_le_bytes());
    for entry in &dict {
        out.extend_from_slice(&(entry.len() as u16).to_le_bytes());
        out.extend_from_slice(entry);
    }
    out.extend_from_slice(&(indexes.len() as u32).to_le_bytes());
    for idx in &indexes {
        out.extend_from_slice(&idx.to_le_bytes());
    }
    Ok(out)
}

fn decode_dict(data: &[u8]) -> CodecResult<Vec<u8>> {
    if data.len() < 4 {
        return Err(CodecError::Truncated);
    }
    let dict_count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let mut cursor = 4usize;
    let mut dict: Vec<Vec<u8>> = Vec::with_capacity(dict_count);
    for _ in 0..dict_count {
        if cursor + 2 > data.len() {
            return Err(CodecError::Truncated);
        }
        let len = u16::from_le_bytes(data[cursor..cursor + 2].try_into().unwrap()) as usize;
        cursor += 2;
        if cursor + len > data.len() {
            return Err(CodecError::Truncated);
        }
        dict.push(data[cursor..cursor + len].to_vec());
        cursor += len;
    }
    if cursor + 4 > data.len() {
        return Err(CodecError::Truncated);
    }
    let idx_count = u32::from_le_bytes(data[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4;
    if cursor + idx_count * 4 > data.len() {
        return Err(CodecError::Truncated);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(idx_count as u32).to_le_bytes());
    for i in 0..idx_count {
        let idx = u32::from_le_bytes(data[cursor + i * 4..cursor + i * 4 + 4].try_into().unwrap())
            as usize;
        if idx >= dict.len() {
            return Err(CodecError::InvalidPayload("dict index out of range"));
        }
        let entry = &dict[idx];
        out.extend_from_slice(&(entry.len() as u16).to_le_bytes());
        out.extend_from_slice(entry);
    }
    Ok(out)
}

// -------------------------------------------------------------------------
// Typed i64 helpers — convenient for columns the caller already holds
// as `Vec<i64>`.
// -------------------------------------------------------------------------

/// Delta-encode + T64 bit-pack an i64 column, then zstd-compress the
/// residual. Returns the tuple `(bytes, value_count)` so callers can
/// decode without scanning the payload.
pub fn encode_delta_t64_zstd(values: &[i64]) -> (Vec<u8>, usize) {
    let (t64_bytes, len) = t64_encode(values);
    let compressed = zstd_compress(&t64_bytes);
    (compressed, len)
}

pub fn decode_delta_t64_zstd(bytes: &[u8], len: usize) -> CodecResult<Vec<i64>> {
    let raw = zstd_decompress(bytes).ok_or(CodecError::InvalidPayload(
        "delta+t64 zstd envelope malformed",
    ))?;
    t64_decode(&raw, len).ok_or(CodecError::InvalidPayload("t64 body malformed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn str_stream(items: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(items.len() as u32).to_le_bytes());
        for s in items {
            let bytes = s.as_bytes();
            out.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        out
    }

    #[test]
    fn none_codec_is_pure_passthrough() {
        let raw = b"hello world".to_vec();
        let encoded = encode_bytes(&[ColumnCodec::None], &raw).unwrap();
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn lz4_round_trips() {
        let raw: Vec<u8> = (0..4096).map(|i| (i % 19) as u8).collect();
        let encoded = encode_bytes(&[ColumnCodec::Lz4], &raw).unwrap();
        assert!(encoded.len() < raw.len());
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn zstd_round_trips_with_explicit_level() {
        let raw: Vec<u8> = (0..4096).map(|i| (i % 7) as u8).collect();
        let encoded = encode_bytes(&[ColumnCodec::Zstd { level: 6 }], &raw).unwrap();
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn lz4_then_zstd_chains_both_codecs() {
        let raw: Vec<u8> = (0..4096).map(|i| (i as u8).wrapping_mul(17)).collect();
        let encoded =
            encode_bytes(&[ColumnCodec::Lz4, ColumnCodec::Zstd { level: 3 }], &raw).unwrap();
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn delta_codec_round_trips_timestamps_as_bytes() {
        let values: Vec<u64> = (0..1000).map(|i| 1_700_000_000 + i * 1000).collect();
        let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let encoded = encode_bytes(&[ColumnCodec::Delta], &raw).unwrap();
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn dict_codec_compresses_repeated_strings() {
        let raw = str_stream(&[
            "us-east-1",
            "us-east-1",
            "eu-west-1",
            "us-east-1",
            "apac-south-1",
            "eu-west-1",
        ]);
        let encoded = encode_bytes(&[ColumnCodec::Dict], &raw).unwrap();
        // Dict overhead + indexes should beat the raw stream.
        assert!(encoded.len() < raw.len());
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn decode_rejects_unknown_codec_tag() {
        let mut buf = 1u16.to_le_bytes().to_vec();
        buf.push(99); // invalid
        buf.push(0); // payload byte
        let err = decode_bytes(&buf).unwrap_err();
        assert!(matches!(err, CodecError::UnknownCodec(99)));
    }

    #[test]
    fn decode_rejects_truncated_header() {
        assert!(decode_bytes(&[]).is_err());
        assert!(decode_bytes(&[0u8]).is_err());
    }

    #[test]
    fn typed_delta_t64_zstd_round_trips() {
        let values: Vec<i64> = (0..10_000).map(|i| 42 + i).collect();
        let (encoded, len) = encode_delta_t64_zstd(&values);
        assert_eq!(len, values.len());
        assert!(encoded.len() < values.len() * 8 / 2);
        let decoded = decode_delta_t64_zstd(&encoded, len).unwrap();
        assert_eq!(decoded, values);
    }

    #[test]
    fn delta_codec_rejects_non_multiple_of_eight() {
        let encoded = encode_bytes(&[ColumnCodec::Delta], &[1u8, 2, 3]);
        assert!(encoded.is_err());
    }
}
