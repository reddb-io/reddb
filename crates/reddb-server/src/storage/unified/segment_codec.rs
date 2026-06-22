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
//! | `Xor`          | floating-point gauges             | reuses Gorilla `xor_*_values` |
//!
//! Codecs chain: callers compose a `Vec<ColumnCodec>` and apply them
//! outer → inner on encode, inner → outer on decode. The pipeline
//! header records every codec used so downstream readers don't need
//! the schema declaration — just the byte buffer.

use crate::storage::timeseries::compression::{
    delta_decode_timestamps, delta_encode_timestamps, t64_decode, t64_encode, xor_decode_values,
    xor_encode_values, zstd_compress, zstd_decompress,
};

/// Codec identifiers. Stored as a single byte in the header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnCodec {
    None,
    Lz4,
    Zstd {
        level: i32,
    },
    Delta,
    DoubleDelta,
    Dict,
    /// Gorilla XOR for floating-point gauges. Reuses the existing
    /// time-series `xor_encode_values`/`xor_decode_values` — #853 only
    /// *wires* the codec into the pipeline, it adds no new algorithm.
    Xor,
}

impl ColumnCodec {
    /// Single-byte codec discriminant recorded in stream + column-block
    /// directory headers. Public so the columnar chunk layout
    /// (`column_block`) can store the codec each column was written with.
    pub fn tag(&self) -> u8 {
        match self {
            ColumnCodec::None => 0,
            ColumnCodec::Lz4 => 1,
            ColumnCodec::Zstd { .. } => 2,
            ColumnCodec::Delta => 3,
            ColumnCodec::DoubleDelta => 4,
            ColumnCodec::Dict => 5,
            ColumnCodec::Xor => 6,
        }
    }

    /// Inverse of [`ColumnCodec::tag`]. `Zstd` decodes to the default
    /// level (3); the real level is carried inline by the stream header,
    /// so a tag round-trip is only used for directory bookkeeping.
    pub fn from_tag(tag: u8) -> Option<ColumnCodec> {
        match tag {
            0 => Some(ColumnCodec::None),
            1 => Some(ColumnCodec::Lz4),
            2 => Some(ColumnCodec::Zstd { level: 3 }),
            3 => Some(ColumnCodec::Delta),
            4 => Some(ColumnCodec::DoubleDelta),
            5 => Some(ColumnCodec::Dict),
            6 => Some(ColumnCodec::Xor),
            _ => None,
        }
    }
}

/// Column semantics — the *role* a column plays, independent of its raw
/// logical type. This is the signal [`select_codecs`] keys off: a numeric
/// column can be a monotonic timestamp, an oscillating gauge, or a
/// monotonic counter, and each wants a different codec even though all
/// three are stored as 8-byte little-endian values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColumnSemantics {
    /// Monotonic, regularly-spaced timestamps → delta-of-delta + ZSTD.
    Timestamp,
    /// Floating-point gauge (oscillating reading) → Gorilla/XOR + ZSTD.
    Gauge,
    /// Monotonic-ish integer counter → delta + ZSTD.
    Counter,
    /// Low-cardinality string / enum label → dictionary + ZSTD.
    LowCardinality,
    /// No semantic hint — fall back to a generic ZSTD codec.
    Generic,
}

/// Pick the codec pipeline for a column from its semantics (and, for the
/// `Generic` case, its logical type). This is the whole of #853: it wires
/// *selection* over the existing codecs — delta-of-delta for timestamps,
/// Gorilla/XOR for gauges, delta for counters, dictionary for
/// low-cardinality strings, ZSTD otherwise. Every semantic codec is
/// chained with `Zstd(3)` so the residual stream (which the leading codec
/// only *re-shapes* into mostly-zero bytes) is actually shrunk on disk —
/// the ClickHouse `CODEC(DoubleDelta, ZSTD)` parity posture.
///
/// The leading codec is the *semantic* one; [`crate::storage::unified::column_block`]
/// records its tag in the chunk directory as the column's chosen codec.
/// The full chain is always self-described by the stream header, so the
/// reader never consults this function.
pub fn select_codecs(logical_type: u8, semantics: ColumnSemantics) -> Vec<ColumnCodec> {
    // LZ4 as the outer compression layer: decompresses at ~4–6 GB/s vs ~2 GB/s
    // for Zstd(3), removing the dominant bottleneck on the read path (#962).
    // The semantic codecs (DoubleDelta/XOR/Delta/Dict) reshape column bytes into
    // near-zero residuals before LZ4 sees them, so the compression ratio stays
    // competitive with Zstd for these structured columns.
    // Old data written with Zstd is still decodeable — the stream is self-describing.
    let outer = ColumnCodec::Lz4;
    match semantics {
        ColumnSemantics::Timestamp => vec![ColumnCodec::DoubleDelta, outer],
        ColumnSemantics::Gauge => vec![ColumnCodec::Xor, outer],
        ColumnSemantics::Counter => vec![ColumnCodec::Delta, outer],
        ColumnSemantics::LowCardinality => vec![ColumnCodec::Dict, outer],
        // `Generic` ignores the logical type today — there is no
        // type-only heuristic that beats LZ4 without a semantic hint.
        // The parameter is kept so a future slice can refine the fallback
        // without changing this signature.
        ColumnSemantics::Generic => {
            let _ = logical_type;
            vec![outer]
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
    // Start decoding from the raw bytes without a copy (#962): pass the
    // buffer slice directly to the first (outermost) codec, which is
    // typically LZ4/Zstd.  Subsequent codecs work on owned Vecs as before.
    let start = &buf[cursor..];
    let mut rev = codecs.iter().rev();
    match rev.next() {
        None => Ok(start.to_vec()),
        Some(first) => {
            let mut payload = apply_decode(first, start)?;
            for codec in rev {
                payload = apply_decode(codec, &payload)?;
            }
            Ok(payload)
        }
    }
}

/// Decode a column stream whose innermost codec is a fixed-width 8-byte
/// numeric codec (`Delta` / `DoubleDelta` / `Xor`) straight into an
/// 8-byte-aligned `Vec<u64>`, skipping the `Vec<u8>` → typed-`Vec` copy the
/// columnar batch reader would otherwise pay (#962). The returned `u64` words
/// carry the same little-endian bit pattern [`decode_bytes`] would produce, so
/// the caller reinterprets them as `i64`/`f64` for free.
///
/// Returns `Ok(None)` when the stream's innermost codec is not one of those
/// (e.g. a `Generic` LZ4-only or `Dict` column); the caller then falls back to
/// [`decode_bytes`] plus a copy. Header/codec parsing matches [`decode_bytes`]
/// byte-for-byte, and the result is bit-identical to running `decode_bytes` and
/// reinterpreting its bytes as `u64` words.
pub fn decode_bytes_to_u64(buf: &[u8]) -> CodecResult<Option<Vec<u64>>> {
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
    // The innermost codec (encoded first, decoded last) is `codecs[0]`; only the
    // fixed-width numeric codecs can emit u64 words without an extra copy.
    match codecs.first() {
        Some(ColumnCodec::Delta | ColumnCodec::DoubleDelta | ColumnCodec::Xor) => {}
        _ => return Ok(None),
    }
    let inner = codecs[0].clone();
    let start = &buf[cursor..];
    // Decode every outer codec (`codecs[len-1..=1]`, in decode order) to raw
    // bytes, leaving the innermost numeric codec to run typed.
    let decoded_outer;
    let mid: &[u8] = if codecs.len() == 1 {
        start
    } else {
        let mut payload = apply_decode(&codecs[codecs.len() - 1], start)?;
        for codec in codecs[1..codecs.len() - 1].iter().rev() {
            payload = apply_decode(codec, &payload)?;
        }
        decoded_outer = payload;
        &decoded_outer
    };
    Ok(Some(decode_numeric_to_u64(&inner, mid)?))
}

/// Decode a `(count:u32) + 8-byte words` numeric segment — the inner
/// `Delta`/`DoubleDelta`/`Xor` codec output — into `Vec<u64>`. Mirrors the
/// `Vec<u8>` arms of [`apply_decode`] exactly, emitting the same little-endian
/// bit pattern as `u64` words instead of bytes (#962 typed fast path).
fn decode_numeric_to_u64(codec: &ColumnCodec, data: &[u8]) -> CodecResult<Vec<u64>> {
    if data.len() < 4 {
        return Err(CodecError::Truncated);
    }
    let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let payload = &data[4..];
    if payload.len() < count * 8 {
        return Err(CodecError::Truncated);
    }
    let mut out = Vec::with_capacity(count);
    match codec {
        ColumnCodec::Delta | ColumnCodec::DoubleDelta => {
            if count > 0 {
                let mut chunks = payload.chunks_exact(8);
                let v0 = u64::from_le_bytes(chunks.next().unwrap().try_into().unwrap());
                out.push(v0);
                if count >= 2 {
                    let mut prev_delta =
                        i64::from_le_bytes(chunks.next().unwrap().try_into().unwrap());
                    let v1 = (v0 as i64 + prev_delta) as u64;
                    out.push(v1);
                    let mut prev_val = v1 as i64;
                    for chunk in chunks.take(count - 2) {
                        prev_delta += i64::from_le_bytes(chunk.try_into().unwrap());
                        prev_val += prev_delta;
                        out.push(prev_val as u64);
                    }
                }
            }
        }
        ColumnCodec::Xor => {
            if count > 0 {
                let mut chunks = payload.chunks_exact(8);
                let mut prev = u64::from_le_bytes(chunks.next().unwrap().try_into().unwrap());
                out.push(prev);
                for chunk in chunks.take(count - 1) {
                    prev ^= u64::from_le_bytes(chunk.try_into().unwrap());
                    out.push(prev);
                }
            }
        }
        _ => return Err(CodecError::InvalidPayload("non-numeric inner codec")),
    }
    Ok(out)
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
            if !data.len().is_multiple_of(8) {
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
        ColumnCodec::Xor => {
            // f64 stream required.
            if !data.len().is_multiple_of(8) {
                return Err(CodecError::InvalidPayload("xor expects f64 stream"));
            }
            let values: Vec<f64> = data
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
                .collect();
            let encoded = xor_encode_values(&values);
            // Pack as (count:u32) + u64 XOR words.
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
            // Inline delta_decode_timestamps to output directly to Vec<u8>,
            // eliminating the intermediate Vec<i64> + Vec<u64> round-trip (#962).
            // Use chunks_exact(8) so the compiler can eliminate per-iteration bounds checks.
            let mut out = Vec::with_capacity(count * 8);
            if count > 0 {
                let mut chunks = payload.chunks_exact(8);
                let v0 = u64::from_le_bytes(chunks.next().unwrap().try_into().unwrap());
                out.extend_from_slice(&v0.to_le_bytes());
                if count >= 2 {
                    let d1_bytes = chunks.next().unwrap();
                    let mut prev_delta = i64::from_le_bytes(d1_bytes.try_into().unwrap());
                    let v1 = (v0 as i64 + prev_delta) as u64;
                    out.extend_from_slice(&v1.to_le_bytes());
                    let mut prev_val = v1 as i64;
                    for chunk in chunks.take(count - 2) {
                        prev_delta += i64::from_le_bytes(chunk.try_into().unwrap());
                        prev_val += prev_delta;
                        out.extend_from_slice(&(prev_val as u64).to_le_bytes());
                    }
                }
            }
            Ok(out)
        }
        ColumnCodec::Xor => {
            if data.len() < 4 {
                return Err(CodecError::Truncated);
            }
            let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
            let payload = &data[4..];
            if payload.len() < count * 8 {
                return Err(CodecError::Truncated);
            }
            // Inline xor_decode_values to output directly to Vec<u8>,
            // eliminating the intermediate Vec<u64> + Vec<f64> round-trip (#962).
            // Use chunks_exact(8) so the compiler can eliminate per-iteration bounds checks.
            // The XOR accumulator is kept as u64 bits; f64::from_bits(bits).to_le_bytes()
            // == bits.to_le_bytes() on all LE targets so we skip the from_bits call.
            let mut out = Vec::with_capacity(count * 8);
            if count > 0 {
                let mut chunks = payload.chunks_exact(8);
                let mut prev = u64::from_le_bytes(chunks.next().unwrap().try_into().unwrap());
                out.extend_from_slice(&prev.to_le_bytes());
                for chunk in chunks.take(count - 1) {
                    prev ^= u64::from_le_bytes(chunk.try_into().unwrap());
                    out.extend_from_slice(&prev.to_le_bytes());
                }
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

    fn f64_stream(values: &[f64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn u64_stream(values: &[u64]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn xor_codec_round_trips_gauge_floats() {
        // Slowly-varying gauge — the case Gorilla/XOR targets.
        let values: Vec<f64> = (0..2000).map(|i| 95.0 + (i % 13) as f64 * 0.125).collect();
        let raw = f64_stream(&values);
        let encoded = encode_bytes(&[ColumnCodec::Xor], &raw).unwrap();
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn xor_codec_is_lossless_for_special_floats() {
        // NaN / inf / signed zero must survive bit-for-bit.
        let values = vec![
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            -0.0,
            1.5,
            -1.5,
        ];
        let raw = f64_stream(&values);
        let encoded = encode_bytes(&[ColumnCodec::Xor], &raw).unwrap();
        let decoded = decode_bytes(&encoded).unwrap();
        assert_eq!(decoded, raw);
    }

    #[test]
    fn xor_codec_rejects_non_multiple_of_eight() {
        assert!(encode_bytes(&[ColumnCodec::Xor], &[1u8, 2, 3]).is_err());
    }

    #[test]
    fn xor_tag_round_trips() {
        assert_eq!(
            ColumnCodec::from_tag(ColumnCodec::Xor.tag()),
            Some(ColumnCodec::Xor)
        );
    }

    #[test]
    fn select_codecs_maps_semantics_to_expected_chains() {
        // Outer codec is LZ4 since #962 (was Zstd(3)).
        let outer = ColumnCodec::Lz4;
        assert_eq!(
            select_codecs(0, ColumnSemantics::Timestamp),
            vec![ColumnCodec::DoubleDelta, outer.clone()]
        );
        assert_eq!(
            select_codecs(0, ColumnSemantics::Gauge),
            vec![ColumnCodec::Xor, outer.clone()]
        );
        assert_eq!(
            select_codecs(0, ColumnSemantics::Counter),
            vec![ColumnCodec::Delta, outer.clone()]
        );
        assert_eq!(
            select_codecs(0, ColumnSemantics::LowCardinality),
            vec![ColumnCodec::Dict, outer.clone()]
        );
        assert_eq!(select_codecs(0, ColumnSemantics::Generic), vec![outer]);
    }

    /// Criterion 2: round-trip stays lossless for every codec/type
    /// combination the selector can produce.
    #[test]
    fn selected_codecs_round_trip_losslessly() {
        let ts = u64_stream(
            &(0..1000)
                .map(|i| 1_700_000_000_000 + i * 1_000_000)
                .collect::<Vec<_>>(),
        );
        let gauge = f64_stream(
            &(0..1000)
                .map(|i| 50.0 + (i % 9) as f64 * 0.5)
                .collect::<Vec<_>>(),
        );
        let counter = u64_stream(&(0..1000).map(|i| (i * 7) as u64).collect::<Vec<_>>());
        let strings = str_stream(&["a", "a", "b", "c", "a", "b", "b", "a", "c", "a"]);
        let cases = [
            (ColumnSemantics::Timestamp, 2u8, &ts),
            (ColumnSemantics::Gauge, 3u8, &gauge),
            (ColumnSemantics::Counter, 2u8, &counter),
            (ColumnSemantics::LowCardinality, 4u8, &strings),
            (ColumnSemantics::Generic, 4u8, &strings),
        ];
        for (sem, ty, raw) in cases {
            let codecs = select_codecs(ty, sem);
            let encoded = encode_bytes(&codecs, raw).unwrap();
            let decoded = decode_bytes(&encoded).unwrap();
            assert_eq!(decoded, *raw, "lossless round-trip failed for {sem:?}");
        }
    }

    /// #962: the typed `decode_bytes_to_u64` fast path is bit-identical to
    /// `decode_bytes` reinterpreted as u64 words for numeric-inner-codec
    /// columns, and declines (`None`) for non-numeric inner codecs so the
    /// caller falls back to the byte path.
    #[test]
    fn decode_bytes_to_u64_matches_decode_bytes() {
        let ts = u64_stream(
            &(0..1000)
                .map(|i| 1_700_000_000_000 + i * 1_000_000)
                .collect::<Vec<_>>(),
        );
        let gauge = f64_stream(
            &(0..1000)
                .map(|i| 95.0 + (i % 7) as f64 * 0.25)
                .collect::<Vec<_>>(),
        );
        let counter = u64_stream(&(0..1000).map(|i| (i * 7) as u64).collect::<Vec<_>>());
        let strings = str_stream(&["a", "a", "b", "c", "a", "b", "b", "a", "c", "a"]);

        // Numeric inner codec (DoubleDelta / Xor / Delta): typed path matches.
        for (sem, ty, raw) in [
            (ColumnSemantics::Timestamp, 2u8, &ts),
            (ColumnSemantics::Gauge, 3u8, &gauge),
            (ColumnSemantics::Counter, 2u8, &counter),
        ] {
            let encoded = encode_bytes(&select_codecs(ty, sem), raw).unwrap();
            let bytes = decode_bytes(&encoded).unwrap();
            let words = decode_bytes_to_u64(&encoded)
                .unwrap()
                .expect("numeric inner codec must take the typed path");
            let from_words: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            assert_eq!(from_words, bytes, "typed/byte mismatch for {sem:?}");
            assert_eq!(words.len() * 8, bytes.len());
        }

        // Non-numeric inner codec (Dict / Generic LZ4-only): declines.
        for (sem, ty, raw) in [
            (ColumnSemantics::LowCardinality, 4u8, &strings),
            (ColumnSemantics::Generic, 4u8, &strings),
        ] {
            let encoded = encode_bytes(&select_codecs(ty, sem), raw).unwrap();
            assert!(
                decode_bytes_to_u64(&encoded).unwrap().is_none(),
                "non-numeric inner codec must decline the typed path for {sem:?}"
            );
        }
    }

    /// Criterion 3: loose compression-ratio sanity bounds per codec so a
    /// regression that bloats storage is caught. Bands are deliberately
    /// generous — they assert "this codec actually shrinks its target
    /// shape", not a precise ratio.
    #[test]
    fn selected_codecs_meet_loose_ratio_bounds() {
        // Regular timestamps: delta-of-delta collapses to ~0 residuals,
        // ZSTD then crushes them. Expect a large win.
        let ts = u64_stream(
            &(0..4000)
                .map(|i| 1_700_000_000_000 + i * 1_000_000)
                .collect::<Vec<_>>(),
        );
        let enc = encode_bytes(&select_codecs(2, ColumnSemantics::Timestamp), &ts).unwrap();
        assert!(
            enc.len() < ts.len() / 4,
            "timestamp codec ratio too weak: {} -> {}",
            ts.len(),
            enc.len()
        );

        // Slowly-varying gauge: XOR yields long zero runs, ZSTD shrinks.
        let gauge = f64_stream(
            &(0..4000)
                .map(|i| 95.0 + (i % 5) as f64 * 0.1)
                .collect::<Vec<_>>(),
        );
        let enc = encode_bytes(&select_codecs(3, ColumnSemantics::Gauge), &gauge).unwrap();
        assert!(
            enc.len() < gauge.len() / 2,
            "gauge codec ratio too weak: {} -> {}",
            gauge.len(),
            enc.len()
        );

        // Monotonic counter: constant delta → ~0 residuals.
        let counter = u64_stream(&(0..4000).map(|i| (i * 3) as u64).collect::<Vec<_>>());
        let enc = encode_bytes(&select_codecs(2, ColumnSemantics::Counter), &counter).unwrap();
        assert!(
            enc.len() < counter.len() / 4,
            "counter codec ratio too weak: {} -> {}",
            counter.len(),
            enc.len()
        );

        // Low-cardinality strings: dictionary folds repeats.
        let labels: Vec<&str> = (0..4000)
            .map(|i| ["us-east-1", "eu-west-1", "apac-south-1"][i % 3])
            .collect();
        let strings = str_stream(&labels);
        let enc =
            encode_bytes(&select_codecs(4, ColumnSemantics::LowCardinality), &strings).unwrap();
        assert!(
            enc.len() < strings.len() / 2,
            "low-cardinality codec ratio too weak: {} -> {}",
            strings.len(),
            enc.len()
        );
    }
}
