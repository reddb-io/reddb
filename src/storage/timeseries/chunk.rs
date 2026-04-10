//! Time-Series Chunk — grouped storage of metric data points
//!
//! Points are grouped by (metric, tags) into chunks. Each chunk stores
//! compressed timestamps and values for efficient range queries.

use std::collections::HashMap;

use super::compression::{
    delta_decode_timestamps, delta_encode_timestamps, xor_decode_values, xor_encode_values,
};

/// A single time-series data point
#[derive(Debug, Clone, PartialEq)]
pub struct TimeSeriesPoint {
    pub timestamp_ns: u64,
    pub value: f64,
}

/// A chunk of time-series data for a single metric + tag combination.
///
/// Points are stored in timestamp order. The chunk can be in either
/// "open" (uncompressed, accepting writes) or "sealed" (compressed) state.
pub struct TimeSeriesChunk {
    /// Metric name (e.g., "cpu.idle")
    pub metric: String,
    /// Dimensional tags (e.g., {"host": "srv1", "region": "us-east"})
    pub tags: HashMap<String, String>,
    /// Raw timestamps (nanoseconds since epoch)
    timestamps: Vec<u64>,
    /// Raw values
    values: Vec<f64>,
    /// Maximum points before auto-seal
    max_points: usize,
    /// Whether the chunk is sealed (compressed, immutable)
    sealed: bool,
    /// Compressed timestamp data (populated on seal)
    compressed_timestamps: Option<Vec<i64>>,
    /// Compressed value data (populated on seal)
    compressed_values: Option<Vec<u64>>,
}

impl TimeSeriesChunk {
    /// Create a new open chunk
    pub fn new(metric: impl Into<String>, tags: HashMap<String, String>) -> Self {
        Self {
            metric: metric.into(),
            tags,
            timestamps: Vec::new(),
            values: Vec::new(),
            max_points: 1024,
            sealed: false,
            compressed_timestamps: None,
            compressed_values: None,
        }
    }

    /// Create with custom max points
    pub fn with_max_points(
        metric: impl Into<String>,
        tags: HashMap<String, String>,
        max_points: usize,
    ) -> Self {
        let mut chunk = Self::new(metric, tags);
        chunk.max_points = max_points;
        chunk
    }

    /// Append a data point. Returns false if the chunk is sealed or full.
    pub fn append(&mut self, timestamp_ns: u64, value: f64) -> bool {
        if self.sealed || self.timestamps.len() >= self.max_points {
            return false;
        }
        self.timestamps.push(timestamp_ns);
        self.values.push(value);
        true
    }

    /// Number of data points
    pub fn len(&self) -> usize {
        self.timestamps.len()
    }

    /// Whether the chunk is empty
    pub fn is_empty(&self) -> bool {
        self.timestamps.is_empty()
    }

    /// Whether the chunk is full
    pub fn is_full(&self) -> bool {
        self.timestamps.len() >= self.max_points
    }

    /// Whether the chunk is sealed
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Minimum timestamp in the chunk
    pub fn min_timestamp(&self) -> Option<u64> {
        self.timestamps.first().copied()
    }

    /// Maximum timestamp in the chunk
    pub fn max_timestamp(&self) -> Option<u64> {
        self.timestamps.last().copied()
    }

    /// Seal the chunk: compress data and prevent further writes
    pub fn seal(&mut self) {
        if self.sealed {
            return;
        }
        // Sort by timestamp if not already sorted
        if !self.timestamps.windows(2).all(|w| w[0] <= w[1]) {
            let mut indices: Vec<usize> = (0..self.timestamps.len()).collect();
            indices.sort_by_key(|&i| self.timestamps[i]);
            let sorted_ts: Vec<u64> = indices.iter().map(|&i| self.timestamps[i]).collect();
            let sorted_vals: Vec<f64> = indices.iter().map(|&i| self.values[i]).collect();
            self.timestamps = sorted_ts;
            self.values = sorted_vals;
        }
        self.compressed_timestamps = Some(delta_encode_timestamps(&self.timestamps));
        self.compressed_values = Some(xor_encode_values(&self.values));
        self.sealed = true;
    }

    /// Query points within a time range [start_ns, end_ns] inclusive
    pub fn query_range(&self, start_ns: u64, end_ns: u64) -> Vec<TimeSeriesPoint> {
        self.timestamps
            .iter()
            .zip(self.values.iter())
            .filter(|(&ts, _)| ts >= start_ns && ts <= end_ns)
            .map(|(&ts, &val)| TimeSeriesPoint {
                timestamp_ns: ts,
                value: val,
            })
            .collect()
    }

    /// Get all points
    pub fn points(&self) -> Vec<TimeSeriesPoint> {
        self.timestamps
            .iter()
            .zip(self.values.iter())
            .map(|(&ts, &val)| TimeSeriesPoint {
                timestamp_ns: ts,
                value: val,
            })
            .collect()
    }

    /// Approximate memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();
        size += self.timestamps.len() * 8;
        size += self.values.len() * 8;
        if let Some(ref ct) = self.compressed_timestamps {
            size += ct.len() * 8;
        }
        if let Some(ref cv) = self.compressed_values {
            size += cv.len() * 8;
        }
        for (k, v) in &self.tags {
            size += k.len() + v.len();
        }
        size += self.metric.len();
        size
    }

    /// Compression ratio (sealed only): compressed_size / raw_size
    pub fn compression_ratio(&self) -> Option<f64> {
        if !self.sealed {
            return None;
        }
        let raw = (self.timestamps.len() * 8 + self.values.len() * 8) as f64;
        let compressed = self
            .compressed_timestamps
            .as_ref()
            .map_or(0, |v| v.len() * 8) as f64
            + self.compressed_values.as_ref().map_or(0, |v| v.len() * 8) as f64;
        if raw > 0.0 {
            Some(compressed / raw)
        } else {
            None
        }
    }

    /// Decompress and verify integrity (sealed chunks only)
    pub fn verify(&self) -> bool {
        if !self.sealed {
            return true;
        }
        if let (Some(ct), Some(cv)) = (&self.compressed_timestamps, &self.compressed_values) {
            let decoded_ts = delta_decode_timestamps(ct);
            let decoded_vals = xor_decode_values(cv);
            decoded_ts == self.timestamps && decoded_vals == self.values
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tags(host: &str) -> HashMap<String, String> {
        let mut tags = HashMap::new();
        tags.insert("host".to_string(), host.to_string());
        tags
    }

    #[test]
    fn test_chunk_basic() {
        let mut chunk = TimeSeriesChunk::new("cpu.idle", make_tags("srv1"));
        assert!(chunk.append(1000, 95.2));
        assert!(chunk.append(2000, 94.8));
        assert!(chunk.append(3000, 96.1));

        assert_eq!(chunk.len(), 3);
        assert_eq!(chunk.min_timestamp(), Some(1000));
        assert_eq!(chunk.max_timestamp(), Some(3000));
    }

    #[test]
    fn test_chunk_range_query() {
        let mut chunk = TimeSeriesChunk::new("cpu.idle", make_tags("srv1"));
        for i in 0..10 {
            chunk.append(i * 1000, 90.0 + i as f64);
        }

        let results = chunk.query_range(3000, 6000);
        assert_eq!(results.len(), 4); // 3000, 4000, 5000, 6000
        assert_eq!(results[0].timestamp_ns, 3000);
        assert_eq!(results[3].timestamp_ns, 6000);
    }

    #[test]
    fn test_chunk_seal_and_verify() {
        let mut chunk = TimeSeriesChunk::new("mem.used", make_tags("srv1"));
        for i in 0..100 {
            chunk.append(1_000_000 + i * 60_000, 72.5 + (i as f64) * 0.1);
        }

        assert!(!chunk.is_sealed());
        chunk.seal();
        assert!(chunk.is_sealed());
        assert!(chunk.verify());

        // Cannot append after seal
        assert!(!chunk.append(9_999_999, 99.0));
    }

    #[test]
    fn test_chunk_max_points() {
        let mut chunk = TimeSeriesChunk::with_max_points("test", HashMap::new(), 5);
        for i in 0..5 {
            assert!(chunk.append(i, i as f64));
        }
        assert!(chunk.is_full());
        assert!(!chunk.append(5, 5.0));
    }

    #[test]
    fn test_chunk_compression_ratio() {
        let mut chunk = TimeSeriesChunk::new("regular", HashMap::new());
        // Regular 1-second intervals with similar values → compresses well
        for i in 0..100 {
            chunk.append(
                1_000_000_000 + i * 1_000_000_000,
                95.0 + (i % 3) as f64 * 0.1,
            );
        }
        chunk.seal();

        let ratio = chunk.compression_ratio().unwrap();
        // Compressed data stored alongside raw, so ratio is of compressed vs raw
        assert!(ratio > 0.0);
        assert!(ratio <= 1.0);
    }
}
