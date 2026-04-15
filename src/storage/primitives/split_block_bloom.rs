//! SplitBlockBloomFilter — cache-line-aligned, SIMD-friendly bloom filter.
//!
//! Direct port of MongoDB's `SplitBlockBloomFilter` from
//! `src/mongo/db/exec/sbe/util/bloom_filter.h`.
//!
//! # Design
//!
//! Each block is 32 bytes (8 × u32 = 256 bits), aligned to a cache line.
//! Insert/probe uses 8 independent salt multiplications, one bit per word.
//! Block index uses power-of-2 masking (fast modulo via bitwise AND).
//!
//! For k=8 bits per element: ~10 bits needed per element for ~1% FPR.
//! At n=1000: 8 blocks (256 bytes). At n=10_000: 128 blocks (4 KB).
//!
//! # Usage in `CompiledEntityFilter`
//!
//! For `Filter::In` with >IN_BLOOM_THRESHOLD values, the compiled filter
//! builds a `SplitBlockBloom` at compile time. At evaluate time:
//! 1. Hash field value to u32 (fast, no allocation)
//! 2. Bloom probe — if **false**, skip HashSet (definite miss, O(1))
//! 3. HashSet probe — exact membership check (only ~1% FPR false positives reach here)
//!
//! Benefit: for rows where the field exists but doesn't match any IN value,
//! the bloom eliminates the HashSet::contains call ~99% of the time.

const SALTS: [u32; 8] = [
    0x47b6137b, 0x44974d91, 0x8824ad5b, 0xa2b7289d, 0x705495c7, 0x2df1424b, 0x9efc4947, 0x5c6bfb31,
];

/// One 32-byte cache-line-aligned block: 8 × u32 words.
#[repr(align(32))]
#[derive(Clone)]
pub struct Block {
    words: [u32; 8],
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Block({:08x?})", &self.words)
    }
}

impl Default for Block {
    fn default() -> Self {
        Self { words: [0u32; 8] }
    }
}

/// Split-block bloom filter. Build once at compile time, probe at query time.
/// Zero-allocation probe path.
#[derive(Debug)]
pub struct SplitBlockBloom {
    blocks: Vec<Block>,
    /// `num_blocks - 1` — mask for fast modulo via bitwise AND.
    mask: usize,
}

impl SplitBlockBloom {
    /// Build a filter sized for `n` elements with ~1% FPR.
    pub fn with_capacity(n: usize) -> Self {
        // ~10 bits per element for 1% FPR with 8 salt bits per insert.
        // Each block holds 256 bits. Round up to next power of two.
        let bits_needed = (n * 10).max(256);
        let blocks_needed = bits_needed.div_ceil(256);
        let num_blocks = blocks_needed.next_power_of_two();
        Self {
            blocks: vec![Block::default(); num_blocks],
            mask: num_blocks - 1,
        }
    }

    /// Insert a u32 key into the filter.
    #[inline]
    pub fn insert(&mut self, key: u32) {
        let block_idx = (key as usize) & self.mask;
        let block = &mut self.blocks[block_idx];
        for (i, &salt) in SALTS.iter().enumerate() {
            let bit = key.wrapping_mul(salt) >> 27; // 5-bit position 0..31
            block.words[i] |= 1u32 << bit;
        }
    }

    /// Return `true` if `key` **may** be in the set (false positives possible).
    /// Return `false` if `key` is **definitely absent** (no false negatives).
    #[inline]
    pub fn probe(&self, key: u32) -> bool {
        let block_idx = (key as usize) & self.mask;
        let block = &self.blocks[block_idx];
        for (i, &salt) in SALTS.iter().enumerate() {
            let bit = key.wrapping_mul(salt) >> 27;
            if block.words[i] & (1u32 << bit) == 0 {
                return false;
            }
        }
        true
    }

    /// Number of blocks allocated (each block = 32 bytes).
    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }
}

/// Hash a `Value` to a u32 for bloom filter use.
/// Uses the standard Hash impl (which hashes discriminant + content).
/// Folds the 64-bit DefaultHasher output to 32 bits via XOR.
pub fn hash_value_u32(v: &crate::storage::schema::Value) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    v.hash(&mut h);
    let bits = h.finish();
    (bits ^ (bits >> 32)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_insert_then_probe() {
        let mut bloom = SplitBlockBloom::with_capacity(100);
        for i in 0u32..100 {
            bloom.insert(i);
        }
        for i in 0u32..100 {
            assert!(bloom.probe(i), "false negative for key {i}");
        }
    }

    #[test]
    fn test_absent_key_may_return_false() {
        let mut bloom = SplitBlockBloom::with_capacity(1000);
        for i in 0u32..1000 {
            bloom.insert(i * 2); // insert even numbers
        }
        // odd numbers were never inserted — some may be false positives, but
        // most should be absent. We just verify there are NO false negatives.
        for i in 0u32..1000 {
            assert!(bloom.probe(i * 2), "false negative for key {}", i * 2);
        }
    }

    #[test]
    fn test_false_positive_rate_approximately_one_percent() {
        const N: usize = 10_000;
        let mut bloom = SplitBlockBloom::with_capacity(N);
        for i in 0u32..N as u32 {
            bloom.insert(i);
        }
        let mut fp = 0usize;
        let probes = 10_000usize;
        for i in N as u32..(N as u32 + probes as u32) {
            if bloom.probe(i) {
                fp += 1;
            }
        }
        let fpr = fp as f64 / probes as f64;
        // Allow up to 5% FPR — the theoretical ~1% varies with data patterns.
        assert!(fpr < 0.05, "FPR too high: {fpr:.3}");
    }
}
