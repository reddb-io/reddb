//! HyperLogLog — Probabilistic Cardinality Estimation
//!
//! Estimates the number of distinct elements in a set using ~12KB of memory
//! with ~0.81% standard error. Based on the HyperLogLog algorithm by Flajolet
//! et al. with bias correction.
//!
//! # Example
//! ```ignore
//! let mut hll = HyperLogLog::new();
//! hll.add(b"user1");
//! hll.add(b"user2");
//! hll.add(b"user1"); // duplicate
//! assert!(hll.count() >= 2); // approximately 2
//! ```

/// Number of registers (2^14 = 16384) — standard HLL precision
const NUM_REGISTERS: usize = 16384;
/// Bits used for bucket index (14 bits → 16384 buckets)
const P: u32 = 14;
/// Alpha constant for bias correction with m=16384
const ALPHA: f64 = 0.7213 / (1.0 + 1.079 / NUM_REGISTERS as f64);

/// HyperLogLog cardinality estimator
pub struct HyperLogLog {
    /// Registers storing max leading zeros + 1
    registers: Vec<u8>,
}

impl HyperLogLog {
    /// Create a new HLL with 16384 registers (~16KB memory)
    pub fn new() -> Self {
        Self {
            registers: vec![0u8; NUM_REGISTERS],
        }
    }

    /// Add an element to the HLL
    pub fn add(&mut self, data: &[u8]) {
        let hash = Self::hash(data);
        let index = (hash >> (64 - P)) as usize;
        let remaining = (hash << P) | (1 << (P - 1)); // ensure non-zero
        let rho = remaining.leading_zeros() as u8 + 1;
        if rho > self.registers[index] {
            self.registers[index] = rho;
        }
    }

    /// Estimate the cardinality (number of distinct elements)
    pub fn count(&self) -> u64 {
        let m = NUM_REGISTERS as f64;

        // Harmonic mean of 2^(-register[i])
        let mut sum = 0.0f64;
        let mut zeros = 0u32;
        for &reg in &self.registers {
            sum += 2.0f64.powi(-(reg as i32));
            if reg == 0 {
                zeros += 1;
            }
        }

        let raw_estimate = ALPHA * m * m / sum;

        // Small range correction (linear counting)
        if raw_estimate <= 2.5 * m && zeros > 0 {
            let lc = m * (m / zeros as f64).ln();
            return lc as u64;
        }

        // Large range correction (for 64-bit hashes this is rarely needed)
        let two_pow_64 = 2.0f64.powi(64);
        if raw_estimate > two_pow_64 / 30.0 {
            let corrected = -two_pow_64 * (1.0 - raw_estimate / two_pow_64).ln();
            return corrected as u64;
        }

        raw_estimate as u64
    }

    /// Merge another HLL into this one (union)
    pub fn merge(&mut self, other: &HyperLogLog) {
        for (i, &other_reg) in other.registers.iter().enumerate() {
            if other_reg > self.registers[i] {
                self.registers[i] = other_reg;
            }
        }
    }

    /// Create a merged HLL from two HLLs without modifying either
    pub fn merged(a: &HyperLogLog, b: &HyperLogLog) -> HyperLogLog {
        let mut result = HyperLogLog::new();
        for i in 0..NUM_REGISTERS {
            result.registers[i] = a.registers[i].max(b.registers[i]);
        }
        result
    }

    /// Clear the HLL
    pub fn clear(&mut self) {
        for reg in &mut self.registers {
            *reg = 0;
        }
    }

    /// Memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.registers.len()
    }

    /// Serialize to bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.registers
    }

    /// Deserialize from bytes
    pub fn from_bytes(bytes: Vec<u8>) -> Option<Self> {
        if bytes.len() != NUM_REGISTERS {
            return None;
        }
        Some(Self { registers: bytes })
    }

    /// FNV-1a hash producing a 64-bit value
    fn hash(data: &[u8]) -> u64 {
        let mut hash = 0xcbf29ce484222325u64;
        for &byte in data {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        // Finalizer: mix bits for better distribution
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51afd7ed558ccd);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xc4ceb9fe1a85ec53);
        hash ^= hash >> 33;
        hash
    }
}

impl Default for HyperLogLog {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hll_empty() {
        let hll = HyperLogLog::new();
        assert_eq!(hll.count(), 0);
    }

    #[test]
    fn test_hll_single() {
        let mut hll = HyperLogLog::new();
        hll.add(b"hello");
        assert!(hll.count() >= 1);
        assert!(hll.count() <= 3); // some margin for probabilistic nature
    }

    #[test]
    fn test_hll_duplicates() {
        let mut hll = HyperLogLog::new();
        for _ in 0..1000 {
            hll.add(b"same_value");
        }
        // Should still be ~1
        assert!(hll.count() <= 3);
    }

    #[test]
    fn test_hll_accuracy_1k() {
        let mut hll = HyperLogLog::new();
        let n = 1000;
        for i in 0..n {
            let key = format!("element_{}", i);
            hll.add(key.as_bytes());
        }
        let estimate = hll.count();
        let error = (estimate as f64 - n as f64).abs() / n as f64;
        println!(
            "HLL 1K: actual={n}, estimate={estimate}, error={:.2}%",
            error * 100.0
        );
        assert!(error < 0.10, "Error {error:.4} exceeds 10% for 1K elements");
    }

    #[test]
    fn test_hll_accuracy_100k() {
        let mut hll = HyperLogLog::new();
        let n = 100_000;
        for i in 0..n {
            let key = format!("user_{}", i);
            hll.add(key.as_bytes());
        }
        let estimate = hll.count();
        let error = (estimate as f64 - n as f64).abs() / n as f64;
        println!(
            "HLL 100K: actual={n}, estimate={estimate}, error={:.2}%",
            error * 100.0
        );
        assert!(
            error < 0.05,
            "Error {error:.4} exceeds 5% for 100K elements"
        );
    }

    #[test]
    fn test_hll_merge() {
        let mut hll1 = HyperLogLog::new();
        let mut hll2 = HyperLogLog::new();

        for i in 0..500 {
            hll1.add(format!("a_{}", i).as_bytes());
        }
        for i in 0..500 {
            hll2.add(format!("b_{}", i).as_bytes());
        }

        let count1 = hll1.count();
        let count2 = hll2.count();

        hll1.merge(&hll2);
        let merged_count = hll1.count();

        // Merged count should be roughly count1 + count2 (no overlap)
        assert!(merged_count > count1);
        assert!(merged_count > count2);
        let error = (merged_count as f64 - 1000.0).abs() / 1000.0;
        assert!(error < 0.10);
    }

    #[test]
    fn test_hll_serialization() {
        let mut hll = HyperLogLog::new();
        hll.add(b"test1");
        hll.add(b"test2");

        let bytes = hll.as_bytes().to_vec();
        let restored = HyperLogLog::from_bytes(bytes).unwrap();
        assert_eq!(hll.count(), restored.count());
    }

    #[test]
    fn test_hll_memory() {
        let hll = HyperLogLog::new();
        let mem = hll.memory_bytes();
        // Should be around 16KB + struct overhead
        assert!(mem >= 16384);
        assert!(mem < 20000);
    }
}
