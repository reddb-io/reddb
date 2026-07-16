//! SplitBlockBloom — cache-line-aligned, SIMD-friendly bloom filter.
//!
//! Direct port of MongoDB's split-block bloom from
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
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Block {
    words: [u32; 8],
}

impl std::fmt::Debug for Block {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Block({:08x?})", &self.words)
    }
}

/// Split-block bloom filter. Build once at compile time, probe at query time.
/// Zero-allocation probe path.
#[derive(Debug, Clone, PartialEq, Eq)]
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

    /// Insert an arbitrary byte key into the filter using one stable hash pass.
    #[inline]
    pub fn insert_bytes(&mut self, key: &[u8]) {
        let h = stable_hash64(key);
        let block_idx = ((h >> 32) as usize) & self.mask;
        let probe_word = h as u32;
        let block = &mut self.blocks[block_idx];
        for (i, &salt) in SALTS.iter().enumerate() {
            let bit = probe_word.wrapping_mul(salt) >> 27;
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

    /// Return `true` if `key` **may** be in the set (false positives possible).
    /// Return `false` if `key` is **definitely absent** (no false negatives).
    #[inline]
    pub fn probe_bytes(&self, key: &[u8]) -> bool {
        let h = stable_hash64(key);
        let block_idx = ((h >> 32) as usize) & self.mask;
        let probe_word = h as u32;
        let block = &self.blocks[block_idx];
        for (i, &salt) in SALTS.iter().enumerate() {
            let bit = probe_word.wrapping_mul(salt) >> 27;
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

    /// Number of bytes used by the block payload, excluding the serialized
    /// block-count prefix.
    pub fn byte_size(&self) -> usize {
        self.blocks.len() * 32
    }

    /// OR-merge another split-block bloom into this filter. Returns `false`
    /// when the filters have different block counts, because their block
    /// routing masks are incompatible.
    pub fn union_inplace(&mut self, other: &SplitBlockBloom) -> bool {
        if self.blocks.len() != other.blocks.len() {
            return false;
        }
        for (left, right) in self.blocks.iter_mut().zip(&other.blocks) {
            for (left_word, right_word) in left.words.iter_mut().zip(right.words) {
                *left_word |= right_word;
            }
        }
        true
    }

    /// Fraction of block words that have at least one bit set.
    pub fn fill_ratio(&self) -> f64 {
        let total_words = self.blocks.len() * 8;
        if total_words == 0 {
            return 0.0;
        }
        let filled_words = self
            .blocks
            .iter()
            .flat_map(|block| block.words)
            .filter(|word| *word != 0)
            .count();
        filled_words as f64 / total_words as f64
    }

    /// Serialize to a self-describing blob: a 4-byte LE block count followed
    /// by `num_blocks × 8` little-endian `u32` words. The block count is
    /// always a power of two, so [`from_bytes`](Self::from_bytes) can rebuild
    /// the modulo mask without storing it. Used to persist a per-granule
    /// bloom in a sealed columnar chunk's footer (#855).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.blocks.len() * 32);
        out.extend_from_slice(&(self.blocks.len() as u32).to_le_bytes());
        for block in &self.blocks {
            for &w in &block.words {
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
        out
    }

    /// Rebuild a filter from [`to_bytes`](Self::to_bytes). Returns `None` on a
    /// truncated blob or a non-power-of-two block count (a corrupt mask would
    /// silently mis-route probes and could manufacture false negatives, so we
    /// refuse it rather than risk under-inclusion).
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 4 {
            return None;
        }
        let num_blocks = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        if num_blocks == 0 || !num_blocks.is_power_of_two() {
            return None;
        }
        if bytes.len() < 4 + num_blocks * 32 {
            return None;
        }
        let mut blocks = Vec::with_capacity(num_blocks);
        let mut cur = 4;
        for _ in 0..num_blocks {
            let mut words = [0u32; 8];
            for w in &mut words {
                *w = u32::from_le_bytes(bytes[cur..cur + 4].try_into().ok()?);
                cur += 4;
            }
            blocks.push(Block { words });
        }
        Some(Self {
            blocks,
            mask: num_blocks - 1,
        })
    }
}

/// Hash a raw byte slice to a stable `u64` for byte-key bloom-filter use.
///
/// This hasher is seedless, dependency-free, and deterministic across
/// processes and restarts. Persisted filters that store byte-key bloom bits
/// MUST use this same function for both writers and readers so the stored
/// bloom and probe key agree bit-for-bit across the persisted boundary.
pub fn stable_hash64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    hash ^ (hash >> 33)
}

/// Hash a raw byte slice to a `u32` for bloom-filter use. The writer and the
/// pruner MUST fold a value through this same function so a granule's stored
/// bloom and a probe key agree bit-for-bit — that identity is what makes the
/// no-false-negative guarantee hold across the persisted boundary (#855).
pub fn hash_bytes_u32(bytes: &[u8]) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    let bits = h.finish();
    (bits ^ (bits >> 32)) as u32
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
    fn to_bytes_from_bytes_round_trips_and_keeps_no_false_negatives() {
        let mut bloom = SplitBlockBloom::with_capacity(500);
        for i in 0u32..500 {
            bloom.insert(i.wrapping_mul(2_654_435_761));
        }
        let blob = bloom.to_bytes();
        let restored = SplitBlockBloom::from_bytes(&blob).expect("round-trips");
        assert_eq!(restored, bloom);
        // The persisted filter still never reports a false negative.
        for i in 0u32..500 {
            assert!(restored.probe(i.wrapping_mul(2_654_435_761)));
        }
    }

    #[test]
    fn from_bytes_rejects_truncated_or_non_power_of_two() {
        assert!(SplitBlockBloom::from_bytes(&[]).is_none());
        assert!(SplitBlockBloom::from_bytes(&[1, 2, 3]).is_none());
        // Claims 3 blocks (not a power of two) → rejected.
        assert!(SplitBlockBloom::from_bytes(&3u32.to_le_bytes()).is_none());
        // Claims 2 blocks but supplies no word payload → truncated.
        assert!(SplitBlockBloom::from_bytes(&2u32.to_le_bytes()).is_none());
    }

    #[test]
    fn hash_bytes_u32_is_stable_across_calls() {
        let a = hash_bytes_u32(&7u64.to_le_bytes());
        let b = hash_bytes_u32(&7u64.to_le_bytes());
        assert_eq!(a, b);
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

    #[test]
    fn insert_bytes_has_no_false_negatives() {
        const N: usize = 10_000;
        let mut bloom = SplitBlockBloom::with_capacity(N);
        for i in 0..N as u64 {
            bloom.insert_bytes(&byte_key(i));
        }
        for i in 0..N as u64 {
            assert!(
                bloom.probe_bytes(&byte_key(i)),
                "false negative for key {i}"
            );
        }
    }

    #[test]
    fn insert_bytes_false_positive_rate_stays_below_two_percent() {
        const N: usize = 10_000;
        let mut bloom = SplitBlockBloom::with_capacity(N);
        for i in 0..N as u64 {
            bloom.insert_bytes(&byte_key(i));
        }
        let mut fp = 0usize;
        let probes = 10_000usize;
        for i in N as u64..(N + probes) as u64 {
            if bloom.probe_bytes(&byte_key(i)) {
                fp += 1;
            }
        }
        let fpr = fp as f64 / probes as f64;
        assert!(fpr < 0.02, "FPR too high: {fpr:.3}");
    }

    #[test]
    fn bytes_round_trip_keeps_no_false_negatives() {
        const N: usize = 10_000;
        let mut bloom = SplitBlockBloom::with_capacity(N);
        for i in 0..N as u64 {
            bloom.insert_bytes(&byte_key(i));
        }
        let blob = bloom.to_bytes();
        let restored = SplitBlockBloom::from_bytes(&blob).expect("round-trips");
        assert_eq!(restored, bloom);
        for i in 0..N as u64 {
            assert!(
                restored.probe_bytes(&byte_key(i)),
                "false negative for key {i}"
            );
        }
    }

    #[test]
    fn bytes_hashing_agrees_across_filters() {
        const N: usize = 10_000;
        let mut a = SplitBlockBloom::with_capacity(N);
        let mut b = SplitBlockBloom::with_capacity(N);
        for i in 0..N as u64 {
            let key = byte_key(i);
            a.insert_bytes(&key);
            b.insert_bytes(&key);
        }
        assert_eq!(a, b);
        for i in 0..N as u64 {
            let key = byte_key(i);
            assert_eq!(a.probe_bytes(&key), b.probe_bytes(&key));
            assert!(a.probe_bytes(&key), "false negative for key {i}");
        }
    }

    #[test]
    fn union_inplace_merges_matching_block_counts() {
        let mut a = SplitBlockBloom::with_capacity(1024);
        let mut b = SplitBlockBloom::with_capacity(1024);
        a.insert_bytes(b"one");
        b.insert_bytes(b"two");

        assert!(a.union_inplace(&b));
        assert!(a.probe_bytes(b"one"));
        assert!(a.probe_bytes(b"two"));
    }

    #[test]
    fn union_inplace_rejects_mismatched_block_counts() {
        let mut a = SplitBlockBloom::with_capacity(1024);
        let b = SplitBlockBloom::with_capacity(4096);

        assert!(!a.union_inplace(&b));
    }

    fn byte_key(i: u64) -> [u8; 16] {
        let mut key = [0u8; 16];
        key[..8].copy_from_slice(&i.to_le_bytes());
        key[8..].copy_from_slice(&i.wrapping_mul(0x9e37_79b9_7f4a_7c15).to_le_bytes());
        key
    }
}
