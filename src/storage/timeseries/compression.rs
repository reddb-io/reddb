//! Timestamp and value compression for time-series data
//!
//! - Delta-of-delta encoding for timestamps (Facebook Gorilla paper)
//! - XOR-based compression for floating-point values

/// Delta-of-delta encode a sorted list of timestamps.
/// Each value is encoded as the difference of differences.
/// First value stored as-is, second as delta, rest as delta-of-delta.
pub fn delta_encode_timestamps(timestamps: &[u64]) -> Vec<i64> {
    if timestamps.is_empty() {
        return Vec::new();
    }
    let mut encoded = Vec::with_capacity(timestamps.len());
    encoded.push(timestamps[0] as i64); // first value as-is

    if timestamps.len() == 1 {
        return encoded;
    }

    let mut prev_delta = timestamps[1] as i64 - timestamps[0] as i64;
    encoded.push(prev_delta); // second value as delta

    for i in 2..timestamps.len() {
        let delta = timestamps[i] as i64 - timestamps[i - 1] as i64;
        let dod = delta - prev_delta;
        encoded.push(dod);
        prev_delta = delta;
    }

    encoded
}

/// Decode delta-of-delta encoded timestamps
pub fn delta_decode_timestamps(encoded: &[i64]) -> Vec<u64> {
    if encoded.is_empty() {
        return Vec::new();
    }
    let mut decoded = Vec::with_capacity(encoded.len());
    decoded.push(encoded[0] as u64); // first value

    if encoded.len() == 1 {
        return decoded;
    }

    let mut prev_delta = encoded[1];
    decoded.push((encoded[0] + prev_delta) as u64); // second value

    for val in encoded.iter().skip(2) {
        let delta = prev_delta + val;
        let value = *decoded.last().unwrap() as i64 + delta;
        decoded.push(value as u64);
        prev_delta = delta;
    }

    decoded
}

/// XOR-encode a series of f64 values (Gorilla-style).
/// Returns the XOR deltas. First value stored as-is (as u64 bits).
pub fn xor_encode_values(values: &[f64]) -> Vec<u64> {
    if values.is_empty() {
        return Vec::new();
    }
    let mut encoded = Vec::with_capacity(values.len());
    encoded.push(values[0].to_bits());

    for i in 1..values.len() {
        let xor = values[i].to_bits() ^ values[i - 1].to_bits();
        encoded.push(xor);
    }

    encoded
}

/// Decode XOR-encoded f64 values
pub fn xor_decode_values(encoded: &[u64]) -> Vec<f64> {
    if encoded.is_empty() {
        return Vec::new();
    }
    let mut decoded = Vec::with_capacity(encoded.len());
    decoded.push(f64::from_bits(encoded[0]));

    for i in 1..encoded.len() {
        let prev_bits = decoded[i - 1].to_bits();
        decoded.push(f64::from_bits(prev_bits ^ encoded[i]));
    }

    decoded
}

// =============================================================================
// T64 — bit-packing for integers drawn from a narrow range.
// =============================================================================
//
// For a sequence of `i64`s that all fit into `k` bits of unsigned
// magnitude (after subtracting the minimum), you only need `k` bits
// per value instead of 64. The on-wire layout is:
//
//   [min: i64] [max: i64] [bit_width: u8] [packed payload bits...]
//
// When every value equals `min` (zero bit_width), the payload is
// empty. Callers reconstruct via `t64_decode` which enforces the
// bit-width range (0..=64) and the declared length.

/// Encode a slice of i64s into a compact byte vector using T64
/// bit-packing. Returns `(bytes, length)` — `length` is the number
/// of values so decode knows how many to emit (bit-packed payloads
/// don't self-describe length).
pub fn t64_encode(values: &[i64]) -> (Vec<u8>, usize) {
    if values.is_empty() {
        return (Vec::new(), 0);
    }
    let min = *values.iter().min().unwrap();
    let max = *values.iter().max().unwrap();
    let range = (max as i128) - (min as i128);
    let bit_width: u8 = if range <= 0 {
        0
    } else {
        let ceil_bits = 128 - (range as u128).leading_zeros() as u8;
        ceil_bits.min(64)
    };

    let mut out: Vec<u8> = Vec::with_capacity(17 + values.len() * 8);
    out.extend_from_slice(&min.to_le_bytes());
    out.extend_from_slice(&max.to_le_bytes());
    out.push(bit_width);

    if bit_width == 0 {
        return (out, values.len());
    }

    let mut bit_buf: u128 = 0;
    let mut bits_in_buf: u32 = 0;
    for v in values {
        let offset = (*v as i128 - min as i128) as u128;
        bit_buf |= offset << bits_in_buf;
        bits_in_buf += bit_width as u32;
        while bits_in_buf >= 8 {
            out.push(bit_buf as u8);
            bit_buf >>= 8;
            bits_in_buf -= 8;
        }
    }
    if bits_in_buf > 0 {
        out.push(bit_buf as u8);
    }
    (out, values.len())
}

/// Inverse of [`t64_encode`]. `length` must match the value passed
/// at encode-time.
pub fn t64_decode(bytes: &[u8], length: usize) -> Option<Vec<i64>> {
    if length == 0 {
        return Some(Vec::new());
    }
    if bytes.len() < 17 {
        return None;
    }
    let min = i64::from_le_bytes(bytes[0..8].try_into().ok()?);
    let _max = i64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let bit_width = bytes[16];
    if bit_width == 0 {
        return Some(vec![min; length]);
    }
    if bit_width > 64 {
        return None;
    }
    let mut out = Vec::with_capacity(length);
    let payload = &bytes[17..];
    let mut bit_buf: u128 = 0;
    let mut bits_in_buf: u32 = 0;
    let mut byte_idx = 0usize;
    let mask: u128 = if bit_width == 64 {
        u64::MAX as u128
    } else {
        (1u128 << bit_width) - 1
    };
    for _ in 0..length {
        while bits_in_buf < bit_width as u32 {
            if byte_idx >= payload.len() {
                return None;
            }
            bit_buf |= (payload[byte_idx] as u128) << bits_in_buf;
            byte_idx += 1;
            bits_in_buf += 8;
        }
        let offset = bit_buf & mask;
        bit_buf >>= bit_width as u32;
        bits_in_buf -= bit_width as u32;
        let v = (min as i128).saturating_add(offset as i128) as i64;
        out.push(v);
    }
    Some(out)
}

// =============================================================================
// Chunk-wide ZSTD fallback — for payloads that compress poorly with
// the Delta / XOR / T64 codecs above, apply a final zstd pass.
// =============================================================================

/// Compress arbitrary bytes with zstd at level 3 (good-enough balance
/// between ratio and cpu). Small inputs short-circuit: we return the
/// original bytes with a `0x00` leading marker so decode knows not to
/// feed them to zstd.
pub fn zstd_compress(bytes: &[u8]) -> Vec<u8> {
    zstd_compress_at(bytes, 3)
}

/// Variant that lets the caller pick the zstd level. Level is
/// clamped to `1..=22`.
pub fn zstd_compress_at(bytes: &[u8], level: i32) -> Vec<u8> {
    if bytes.len() < 64 {
        // Smaller than a cache line — compression overhead outweighs
        // any win. Prefix `0` and emit the raw buffer.
        let mut out = Vec::with_capacity(bytes.len() + 1);
        out.push(0u8);
        out.extend_from_slice(bytes);
        return out;
    }
    let clamped = level.clamp(1, 22);
    match zstd::bulk::compress(bytes, clamped) {
        Ok(compressed) => {
            let mut out = Vec::with_capacity(compressed.len() + 1);
            out.push(1u8);
            out.extend_from_slice(&compressed);
            out
        }
        Err(_) => {
            // zstd shouldn't fail on valid slices; fall back to raw
            // so roundtrip is still correct.
            let mut out = Vec::with_capacity(bytes.len() + 1);
            out.push(0u8);
            out.extend_from_slice(bytes);
            out
        }
    }
}

/// Inverse of [`zstd_compress`]. Returns `None` for truncated or
/// malformed inputs.
pub fn zstd_decompress(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() {
        return None;
    }
    match bytes[0] {
        0 => Some(bytes[1..].to_vec()),
        1 => zstd::bulk::decompress(&bytes[1..], 1 << 28).ok(),
        _ => None,
    }
}

// =============================================================================
// Auto-selector — picks the cheapest codec for a given input shape.
// =============================================================================

/// Catalogue of codecs the time-series layer can pick between. Kept
/// in sync with the `CODEC(...)` surface exposed in the DDL sprint
/// that follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsIntCodec {
    /// Raw i64 per value — no compression. Fallback.
    Raw,
    /// Delta-of-delta (good for monotonic timestamps).
    DeltaOfDelta,
    /// T64 bit-packing (good for narrow-range integers).
    T64,
}

/// Pick a codec for an integer series based on its shape. A strictly
/// monotonic series with small deltas wins with delta-of-delta; a
/// narrow-range series (regardless of order) wins with T64; anything
/// else falls back to Raw + zstd fallback at the chunk layer.
pub fn select_int_codec(values: &[i64]) -> TsIntCodec {
    if values.len() < 4 {
        return TsIntCodec::Raw;
    }
    // Heuristic 1: monotonic non-decreasing → DeltaOfDelta.
    let monotonic = values.windows(2).all(|w| w[1] >= w[0]);
    if monotonic {
        return TsIntCodec::DeltaOfDelta;
    }
    // Heuristic 2: narrow range (< 20 bits) → T64.
    let min = *values.iter().min().unwrap();
    let max = *values.iter().max().unwrap();
    let range = (max as i128 - min as i128).max(0) as u128;
    let bits = if range == 0 {
        0
    } else {
        128 - range.leading_zeros() as u32
    };
    if bits <= 20 {
        return TsIntCodec::T64;
    }
    TsIntCodec::Raw
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delta_encode_decode() {
        let timestamps: Vec<u64> = vec![1000, 1060, 1120, 1180, 1240, 1300];
        let encoded = delta_encode_timestamps(&timestamps);
        let decoded = delta_decode_timestamps(&encoded);
        assert_eq!(timestamps, decoded);
    }

    #[test]
    fn test_delta_irregular() {
        let timestamps: Vec<u64> = vec![100, 200, 250, 400, 405, 500];
        let encoded = delta_encode_timestamps(&timestamps);
        let decoded = delta_decode_timestamps(&encoded);
        assert_eq!(timestamps, decoded);

        // Delta-of-deltas should mostly be small for regular data
        // [100, 100, -50, 100, -145, 90] — deltas
        // Then dod compresses differences of those
    }

    #[test]
    fn test_delta_single() {
        let timestamps: Vec<u64> = vec![42];
        let encoded = delta_encode_timestamps(&timestamps);
        let decoded = delta_decode_timestamps(&encoded);
        assert_eq!(timestamps, decoded);
    }

    #[test]
    fn test_delta_empty() {
        let timestamps: Vec<u64> = vec![];
        let encoded = delta_encode_timestamps(&timestamps);
        let decoded = delta_decode_timestamps(&encoded);
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_delta_compression_ratio() {
        // Regular 1-second intervals — should compress very well
        let timestamps: Vec<u64> = (0..1000).map(|i| 1_000_000 + i * 1000).collect();
        let encoded = delta_encode_timestamps(&timestamps);

        // After first two values, all delta-of-deltas should be 0
        for &dod in &encoded[2..] {
            assert_eq!(dod, 0, "Regular intervals should have zero delta-of-delta");
        }
    }

    #[test]
    fn test_xor_encode_decode() {
        let values = vec![72.5, 72.6, 72.55, 72.7, 72.65, 72.8];
        let encoded = xor_encode_values(&values);
        let decoded = xor_decode_values(&encoded);
        assert_eq!(values, decoded);
    }

    #[test]
    fn test_xor_compression_similar_values() {
        let values: Vec<f64> = (0..100).map(|i| 95.0 + (i as f64) * 0.01).collect();
        let encoded = xor_encode_values(&values);

        // XOR of similar floats should have many leading zeros
        let zero_xors = encoded[1..].iter().filter(|&&x| x == 0).count();
        // Not all will be zero since values differ, but demonstrates compression potential
        let _ = zero_xors;
    }

    #[test]
    fn test_xor_empty() {
        assert!(xor_encode_values(&[]).is_empty());
        assert!(xor_decode_values(&[]).is_empty());
    }

    // ---- T64 tests ----------------------------------------------------

    #[test]
    fn t64_round_trips_narrow_range() {
        let values: Vec<i64> = (0..1024).map(|i| 1000 + (i % 128)).collect();
        let (bytes, len) = t64_encode(&values);
        let decoded = t64_decode(&bytes, len).unwrap();
        assert_eq!(values, decoded);
        // Compression ratio sanity: 7 bits per value + 17-byte header
        // is way under 8 bytes/value.
        assert!(bytes.len() < values.len() * 8 / 4);
    }

    #[test]
    fn t64_handles_constant_sequence_with_zero_bit_width() {
        let values = vec![42i64; 100];
        let (bytes, len) = t64_encode(&values);
        assert_eq!(bytes.len(), 17); // header only
        let decoded = t64_decode(&bytes, len).unwrap();
        assert_eq!(values, decoded);
    }

    #[test]
    fn t64_empty_returns_empty() {
        let (bytes, len) = t64_encode(&[]);
        assert!(bytes.is_empty());
        assert_eq!(len, 0);
        assert_eq!(t64_decode(&[], 0).unwrap(), Vec::<i64>::new());
    }

    #[test]
    fn t64_handles_negative_values() {
        let values = vec![-1000, -500, 0, 500, 1000, -750, 250];
        let (bytes, len) = t64_encode(&values);
        let decoded = t64_decode(&bytes, len).unwrap();
        assert_eq!(values, decoded);
    }

    #[test]
    fn t64_rejects_corrupted_payload() {
        // Length claim exceeds the bytes available.
        let (bytes, _) = t64_encode(&[1i64, 2, 3, 4]);
        assert!(t64_decode(&bytes[..18], 100).is_none());
    }

    // ---- ZSTD fallback tests ------------------------------------------

    #[test]
    fn zstd_small_input_passes_through_uncompressed() {
        let data = b"short";
        let compressed = zstd_compress(data);
        // Header (1 byte) + raw data.
        assert_eq!(compressed[0], 0);
        assert_eq!(&compressed[1..], data);
        assert_eq!(zstd_decompress(&compressed).unwrap(), data.to_vec());
    }

    #[test]
    fn zstd_large_input_compresses_and_round_trips() {
        let data: Vec<u8> = (0..4096).map(|i| (i % 8) as u8).collect();
        let compressed = zstd_compress(&data);
        assert_eq!(compressed[0], 1);
        assert!(
            compressed.len() < data.len() / 2,
            "zstd should compress ≥2x on repetitive input"
        );
        let decompressed = zstd_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn zstd_decompress_rejects_unknown_marker() {
        assert!(zstd_decompress(&[0xff, 0, 1, 2]).is_none());
        assert!(zstd_decompress(&[]).is_none());
    }

    // ---- select_int_codec -------------------------------------------

    #[test]
    fn select_int_codec_picks_delta_for_monotonic_timestamps() {
        let ts: Vec<i64> = (0..1000).map(|i| 1_000_000 + i * 1000).collect();
        assert_eq!(select_int_codec(&ts), TsIntCodec::DeltaOfDelta);
    }

    #[test]
    fn select_int_codec_picks_t64_for_narrow_range() {
        // Random-looking but bounded in [0, 1024] — fits T64's 20-bit
        // threshold easily and is not monotonic.
        let vals: Vec<i64> = (0..500).map(|i| ((i * 13 + 7) % 1024) as i64).collect();
        assert_eq!(select_int_codec(&vals), TsIntCodec::T64);
    }

    #[test]
    fn select_int_codec_falls_back_to_raw_on_wide_non_monotonic() {
        let vals = vec![1_000_000_000i64, -1, 500_000_000, 42, i64::MAX / 2];
        assert_eq!(select_int_codec(&vals), TsIntCodec::Raw);
    }

    #[test]
    fn select_int_codec_returns_raw_for_tiny_inputs() {
        assert_eq!(select_int_codec(&[]), TsIntCodec::Raw);
        assert_eq!(select_int_codec(&[1, 2, 3]), TsIntCodec::Raw);
    }
}
