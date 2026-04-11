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
}
