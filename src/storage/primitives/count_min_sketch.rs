//! Count-Min Sketch — Probabilistic Frequency Estimation
//!
//! Estimates the frequency of elements in a stream using a 2D array of counters.
//! Always overestimates (never underestimates), with configurable accuracy.
//!
//! - `width` controls accuracy (larger = more precise)
//! - `depth` controls confidence (more hash functions = lower error probability)
//!
//! Default: width=1000, depth=5 (~20KB memory

/// Count-Min Sketch
pub struct CountMinSketch {
    /// 2D array: depth rows × width columns
    counters: Vec<Vec<u64>>,
    /// Number of columns per row
    width: usize,
    /// Number of rows (hash functions)
    depth: usize,
    /// Total count of all increments
    total: u64,
}

impl CountMinSketch {
    /// Create a new sketch with given width and depth
    pub fn new(width: usize, depth: usize) -> Self {
        Self {
            counters: vec![vec![0u64; width]; depth],
            width,
            depth,
            total: 0,
        }
    }

    /// Create with default parameters (width=1000, depth=5)
    pub fn default_size() -> Self {
        Self::new(1000, 5)
    }

    /// Increment the count for an element by `count`
    pub fn add(&mut self, key: &[u8], count: u64) {
        for i in 0..self.depth {
            let idx = self.hash(key, i) % self.width;
            self.counters[i][idx] = self.counters[i][idx].saturating_add(count);
        }
        self.total = self.total.saturating_add(count);
    }

    /// Estimate the frequency of an element (minimum across all rows)
    pub fn estimate(&self, key: &[u8]) -> u64 {
        let mut min = u64::MAX;
        for i in 0..self.depth {
            let idx = self.hash(key, i) % self.width;
            min = min.min(self.counters[i][idx]);
        }
        min
    }

    /// Merge another sketch into this one (element-wise addition).
    /// Both sketches must have the same dimensions.
    pub fn merge(&mut self, other: &CountMinSketch) -> bool {
        if self.width != other.width || self.depth != other.depth {
            return false;
        }
        for i in 0..self.depth {
            for j in 0..self.width {
                self.counters[i][j] = self.counters[i][j].saturating_add(other.counters[i][j]);
            }
        }
        self.total = self.total.saturating_add(other.total);
        true
    }

    /// Total count across all increments
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Width (columns per row)
    pub fn width(&self) -> usize {
        self.width
    }

    /// Depth (number of hash function rows)
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Clear the sketch
    pub fn clear(&mut self) {
        for row in &mut self.counters {
            for cell in row.iter_mut() {
                *cell = 0;
            }
        }
        self.total = 0;
    }

    /// Memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.depth * self.width * std::mem::size_of::<u64>()
            + self.depth * std::mem::size_of::<Vec<u64>>()
    }

    /// Serialize to bytes: [width:4][depth:4][counters...]
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + self.depth * self.width * 8);
        buf.extend_from_slice(&(self.width as u32).to_le_bytes());
        buf.extend_from_slice(&(self.depth as u32).to_le_bytes());
        for row in &self.counters {
            for &val in row {
                buf.extend_from_slice(&val.to_le_bytes());
            }
        }
        buf
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 {
            return None;
        }
        let width = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        let depth = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let expected = 8 + depth * width * 8;
        if bytes.len() != expected {
            return None;
        }
        let mut counters = vec![vec![0u64; width]; depth];
        let mut offset = 8;
        let mut total = 0u64;
        for row in &mut counters {
            for cell in row.iter_mut() {
                *cell = u64::from_le_bytes(bytes[offset..offset + 8].try_into().ok()?);
                offset += 8;
            }
        }
        // Approximate total from first row
        for &val in &counters[0] {
            total = total.saturating_add(val);
        }
        Some(Self {
            counters,
            width,
            depth,
            total,
        })
    }

    /// Hash function for row `i` — seeded FNV-1a
    fn hash(&self, key: &[u8], row: usize) -> usize {
        let seed = match row {
            0 => 0xcbf29ce484222325u64,
            1 => 0x100000001b3u64,
            2 => 0x811c9dc5u64,
            3 => 0xc4ceb9fe1a85ec53u64,
            4 => 0xff51afd7ed558ccdu64,
            _ => 0xcbf29ce484222325u64.wrapping_add((row as u64).wrapping_mul(0x9e3779b97f4a7c15)),
        };
        let mut h = seed;
        for &byte in key {
            h ^= byte as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51afd7ed558ccd);
        h ^= h >> 33;
        h as usize
    }
}

impl Default for CountMinSketch {
    fn default() -> Self {
        Self::default_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cms_basic() {
        let mut cms = CountMinSketch::new(1000, 5);
        cms.add(b"apple", 3);
        cms.add(b"banana", 1);
        cms.add(b"apple", 2);

        assert!(cms.estimate(b"apple") >= 5);
        assert!(cms.estimate(b"banana") >= 1);
        assert_eq!(cms.total(), 6);
    }

    #[test]
    fn test_cms_never_underestimates() {
        let mut cms = CountMinSketch::new(500, 4);
        let n = 100;
        for i in 0..n {
            let key = format!("key_{}", i);
            cms.add(key.as_bytes(), 1);
        }
        for i in 0..n {
            let key = format!("key_{}", i);
            assert!(cms.estimate(key.as_bytes()) >= 1, "key_{i} underestimated");
        }
    }

    #[test]
    fn test_cms_accuracy() {
        let mut cms = CountMinSketch::new(2000, 7);

        // Insert known frequencies
        cms.add(b"hot", 1000);
        cms.add(b"warm", 100);
        cms.add(b"cold", 10);

        // Add noise
        for i in 0..5000 {
            cms.add(format!("noise_{}", i).as_bytes(), 1);
        }

        let hot_est = cms.estimate(b"hot");
        let warm_est = cms.estimate(b"warm");
        let cold_est = cms.estimate(b"cold");

        // Hot should be close to 1000 (may overestimate due to noise)
        assert!(hot_est >= 1000, "hot: {hot_est}");
        assert!(hot_est < 1100, "hot overestimate too high: {hot_est}");

        // Warm should be at least 100
        assert!(warm_est >= 100, "warm: {warm_est}");

        // Cold should be at least 10
        assert!(cold_est >= 10, "cold: {cold_est}");
    }

    #[test]
    fn test_cms_merge() {
        let mut cms1 = CountMinSketch::new(500, 3);
        let mut cms2 = CountMinSketch::new(500, 3);

        cms1.add(b"x", 10);
        cms2.add(b"x", 20);

        assert!(cms1.merge(&cms2));
        assert!(cms1.estimate(b"x") >= 30);
    }

    #[test]
    fn test_cms_merge_incompatible() {
        let mut cms1 = CountMinSketch::new(500, 3);
        let cms2 = CountMinSketch::new(1000, 3);
        assert!(!cms1.merge(&cms2));
    }

    #[test]
    fn test_cms_serialization() {
        let mut cms = CountMinSketch::new(100, 3);
        cms.add(b"test", 42);

        let bytes = cms.as_bytes();
        let restored = CountMinSketch::from_bytes(&bytes).unwrap();

        assert_eq!(restored.width(), 100);
        assert_eq!(restored.depth(), 3);
        assert!(restored.estimate(b"test") >= 42);
    }

    #[test]
    fn test_cms_memory() {
        let cms = CountMinSketch::new(1000, 5);
        let mem = cms.memory_bytes();
        // 1000 * 5 * 8 bytes = 40KB + overhead
        assert!(mem >= 40_000);
        assert!(mem < 50_000);
    }

    #[test]
    fn test_cms_clear() {
        let mut cms = CountMinSketch::new(100, 3);
        cms.add(b"key", 100);
        assert!(cms.estimate(b"key") >= 100);

        cms.clear();
        assert_eq!(cms.estimate(b"key"), 0);
        assert_eq!(cms.total(), 0);
    }
}
