// Bloom Filter Implementation (From Scratch)
// Zero external dependencies - Rust std only

/// Bloom filter for fast negative lookups
/// False positive rate: ~1% with 3 hash functions
pub struct BloomFilter {
    bits: Vec<u8>,  // Bit array (size / 8 bytes)
    num_hashes: u8, // Number of hash functions (3 optimal)
    size: u32,      // Total bits
}

impl BloomFilter {
    /// Create new bloom filter with given size in bits
    pub fn new(size: u32, num_hashes: u8) -> Self {
        let byte_size = ((size + 7) / 8) as usize;
        Self {
            bits: vec![0u8; byte_size],
            num_hashes,
            size,
        }
    }

    /// Create bloom filter optimized for expected number of elements
    /// Formula: size = -n * ln(p) / (ln(2)^2)
    /// Where n = elements, p = false positive rate (0.01 = 1%)
    pub fn with_capacity(elements: usize, false_positive_rate: f64) -> Self {
        let size = Self::optimal_size(elements, false_positive_rate);
        let num_hashes = Self::optimal_hashes(elements, size);
        Self::new(size, num_hashes)
    }

    fn optimal_size(elements: usize, fp_rate: f64) -> u32 {
        let n = elements as f64;
        let p = fp_rate;
        let size = -(n * p.ln()) / (2.0_f64.ln().powi(2));
        size.ceil() as u32
    }

    fn optimal_hashes(elements: usize, size: u32) -> u8 {
        let n = elements as f64;
        let m = size as f64;
        let k = (m / n) * 2.0_f64.ln();
        k.ceil().min(10.0).max(1.0) as u8
    }

    /// Insert key into bloom filter
    pub fn insert(&mut self, key: &[u8]) {
        for i in 0..self.num_hashes {
            let hash = self.hash(key, i);
            self.set_bit(hash);
        }
    }

    /// Check if key might exist (false positive possible, no false negative)
    pub fn contains(&self, key: &[u8]) -> bool {
        for i in 0..self.num_hashes {
            let hash = self.hash(key, i);
            if !self.get_bit(hash) {
                return false; // Definitely not present
            }
        }
        true // Might be present
    }

    /// Get hash value for key using hash function index
    fn hash(&self, key: &[u8], index: u8) -> u32 {
        match index {
            0 => self.hash_fnv1a(key),
            1 => self.hash_murmur3(key),
            2 => self.hash_djb2(key),
            3 => self.hash_sdbm(key),
            4 => self.hash_lose(key),
            _ => self.hash_fnv1a(key),
        }
    }

    /// FNV-1a hash (fast, good distribution)
    fn hash_fnv1a(&self, key: &[u8]) -> u32 {
        let mut hash = 2166136261u32;
        for &byte in key {
            hash ^= byte as u32;
            hash = hash.wrapping_mul(16777619);
        }
        hash % self.size
    }

    /// MurmurHash3-inspired (bit mixing for better distribution)
    fn hash_murmur3(&self, key: &[u8]) -> u32 {
        let mut hash = 0u32;
        for &byte in key {
            hash = hash.wrapping_add(byte as u32);
            hash = hash.wrapping_add(hash << 10);
            hash ^= hash >> 6;
        }
        hash = hash.wrapping_add(hash << 3);
        hash ^= hash >> 11;
        hash = hash.wrapping_add(hash << 15);
        hash % self.size
    }

    /// DJB2 hash (simple and fast)
    fn hash_djb2(&self, key: &[u8]) -> u32 {
        let mut hash = 5381u32;
        for &byte in key {
            hash = hash.wrapping_mul(33).wrapping_add(byte as u32);
        }
        hash % self.size
    }

    /// SDBM hash (used in many hash tables)
    fn hash_sdbm(&self, key: &[u8]) -> u32 {
        let mut hash = 0u32;
        for &byte in key {
            hash = (byte as u32)
                .wrapping_add(hash << 6)
                .wrapping_add(hash << 16)
                .wrapping_sub(hash);
        }
        hash % self.size
    }

    /// Lose Lose hash (simple XOR folding)
    fn hash_lose(&self, key: &[u8]) -> u32 {
        let mut hash = 0u32;
        for chunk in key.chunks(4) {
            let mut val = 0u32;
            for (i, &byte) in chunk.iter().enumerate() {
                val |= (byte as u32) << (i * 8);
            }
            hash ^= val;
        }
        hash % self.size
    }

    /// Set bit at position
    fn set_bit(&mut self, pos: u32) {
        let byte_index = (pos / 8) as usize;
        let bit_offset = (pos % 8) as u8;
        if byte_index < self.bits.len() {
            self.bits[byte_index] |= 1 << bit_offset;
        }
    }

    /// Get bit at position
    fn get_bit(&self, pos: u32) -> bool {
        let byte_index = (pos / 8) as usize;
        let bit_offset = (pos % 8) as u8;
        if byte_index < self.bits.len() {
            (self.bits[byte_index] & (1 << bit_offset)) != 0
        } else {
            false
        }
    }

    /// Get raw bytes for serialization
    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    /// Load from raw bytes
    pub fn from_bytes(bytes: Vec<u8>, num_hashes: u8) -> Self {
        let size = (bytes.len() * 8) as u32;
        Self {
            bits: bytes,
            num_hashes,
            size,
        }
    }

    /// Get size in bytes
    pub fn byte_size(&self) -> usize {
        self.bits.len()
    }

    /// Get number of bits
    pub fn bit_size(&self) -> u32 {
        self.size
    }

    /// Clear all bits
    pub fn clear(&mut self) {
        for byte in &mut self.bits {
            *byte = 0;
        }
    }

    /// Estimate false positive rate based on current fill
    pub fn estimate_fp_rate(&self, inserted_count: usize) -> f64 {
        let m = self.size as f64;
        let n = inserted_count as f64;
        let k = self.num_hashes as f64;

        // Formula: (1 - e^(-kn/m))^k
        let exp_term = (-k * n / m).exp();
        (1.0 - exp_term).powf(k)
    }

    /// Get number of set bits
    pub fn count_set_bits(&self) -> u32 {
        let mut count = 0;
        for &byte in &self.bits {
            count += byte.count_ones();
        }
        count
    }

    /// Calculate fill ratio (0.0 to 1.0)
    pub fn fill_ratio(&self) -> f64 {
        self.count_set_bits() as f64 / self.size as f64
    }
}

/// Builder for creating bloom filters
pub struct BloomFilterBuilder {
    expected_elements: Option<usize>,
    false_positive_rate: f64,
    size: Option<u32>,
    num_hashes: u8,
}

impl BloomFilterBuilder {
    pub fn new() -> Self {
        Self {
            expected_elements: None,
            false_positive_rate: 0.01, // 1% default
            size: None,
            num_hashes: 3,
        }
    }

    pub fn expected_elements(mut self, n: usize) -> Self {
        self.expected_elements = Some(n);
        self
    }

    pub fn false_positive_rate(mut self, rate: f64) -> Self {
        self.false_positive_rate = rate;
        self
    }

    pub fn size(mut self, size: u32) -> Self {
        self.size = Some(size);
        self
    }

    pub fn num_hashes(mut self, n: u8) -> Self {
        self.num_hashes = n;
        self
    }

    pub fn build(self) -> BloomFilter {
        if let Some(size) = self.size {
            BloomFilter::new(size, self.num_hashes)
        } else if let Some(elements) = self.expected_elements {
            BloomFilter::with_capacity(elements, self.false_positive_rate)
        } else {
            // Default: 100K elements, 1% FP rate
            BloomFilter::with_capacity(100_000, self.false_positive_rate)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bloom_basic() {
        let mut bloom = BloomFilter::new(1000, 3);

        bloom.insert(b"hello");
        bloom.insert(b"world");

        assert!(bloom.contains(b"hello"));
        assert!(bloom.contains(b"world"));
        assert!(!bloom.contains(b"foo"));
    }

    #[test]
    fn test_bloom_with_capacity() {
        let mut bloom = BloomFilter::with_capacity(1000, 0.01);

        for i in 0..1000 {
            let key = format!("key{}", i);
            bloom.insert(key.as_bytes());
        }

        // Check all inserted keys are found
        for i in 0..1000 {
            let key = format!("key{}", i);
            assert!(bloom.contains(key.as_bytes()));
        }

        // Check false positive rate
        let mut false_positives = 0;
        for i in 1000..2000 {
            let key = format!("key{}", i);
            if bloom.contains(key.as_bytes()) {
                false_positives += 1;
            }
        }

        let fp_rate = false_positives as f64 / 1000.0;
        println!("False positive rate: {:.2}%", fp_rate * 100.0);
        assert!(fp_rate < 0.05); // Should be under 5%
    }

    #[test]
    fn test_bloom_serialization() {
        let mut bloom1 = BloomFilter::new(1000, 3);
        bloom1.insert(b"test1");
        bloom1.insert(b"test2");

        let bytes = bloom1.as_bytes().to_vec();
        let bloom2 = BloomFilter::from_bytes(bytes, 3);

        assert!(bloom2.contains(b"test1"));
        assert!(bloom2.contains(b"test2"));
        assert!(!bloom2.contains(b"test3"));
    }

    #[test]
    fn test_bloom_clear() {
        let mut bloom = BloomFilter::new(1000, 3);
        bloom.insert(b"hello");
        assert!(bloom.contains(b"hello"));

        bloom.clear();
        assert!(!bloom.contains(b"hello"));
    }

    #[test]
    fn test_bloom_fill_ratio() {
        let mut bloom = BloomFilter::new(1000, 3);
        assert_eq!(bloom.fill_ratio(), 0.0);

        for i in 0..100 {
            bloom.insert(format!("key{}", i).as_bytes());
        }

        let ratio = bloom.fill_ratio();
        println!("Fill ratio: {:.2}%", ratio * 100.0);
        assert!(ratio > 0.0);
        assert!(ratio < 1.0);
    }

    #[test]
    fn test_bloom_builder() {
        let bloom = BloomFilterBuilder::new()
            .expected_elements(10000)
            .false_positive_rate(0.01)
            .build();

        assert!(bloom.bit_size() > 0);
        assert!(bloom.byte_size() > 0);
    }

    #[test]
    fn test_hash_functions() {
        let bloom = BloomFilter::new(1000, 3);
        let key = b"test";

        let h1 = bloom.hash_fnv1a(key);
        let h2 = bloom.hash_murmur3(key);
        let h3 = bloom.hash_djb2(key);

        // Hashes should be different
        assert_ne!(h1, h2);
        assert_ne!(h2, h3);
        assert_ne!(h1, h3);

        // Hashes should be within range
        assert!(h1 < 1000);
        assert!(h2 < 1000);
        assert!(h3 < 1000);
    }

    #[test]
    fn test_optimal_sizing() {
        let bloom = BloomFilter::with_capacity(1_000_000, 0.01);

        println!("Bloom filter stats:");
        println!("  Elements: 1,000,000");
        println!("  FP rate: 1%");
        println!("  Bits: {}", bloom.bit_size());
        println!("  Bytes: {}", bloom.byte_size());
        println!("  Hashes: {}", bloom.num_hashes);

        // Should be around 9.6 bits per element for 1% FP rate
        let bits_per_element = bloom.bit_size() as f64 / 1_000_000.0;
        println!("  Bits/element: {:.2}", bits_per_element);

        assert!(bits_per_element > 8.0);
        assert!(bits_per_element < 12.0);
    }
}
