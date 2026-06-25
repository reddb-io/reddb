//! Deterministic seeded PRNG.
//!
//! `splitmix64` matches the constants used by the C shim so workload content
//! and fault decisions are derived from the same seed family. Same seed → same
//! byte stream → exact reproduction.

/// A `splitmix64` generator: tiny, allocation-free, fully deterministic.
#[derive(Debug, Clone)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Seed the generator.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next pseudo-random `u64`.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform-ish `u64` in `[0, bound)`. `bound == 0` returns `0`.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next_u64() % bound
        }
    }

    /// Fill a byte buffer with deterministic bytes.
    pub fn fill_bytes(&mut self, out: &mut [u8]) {
        let mut i = 0;
        while i < out.len() {
            let word = self.next_u64().to_le_bytes();
            let take = (out.len() - i).min(8);
            out[i..i + take].copy_from_slice(&word[..take]);
            i += take;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SplitMix64::new(1);
        let mut b = SplitMix64::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn below_respects_bound() {
        let mut r = SplitMix64::new(7);
        for _ in 0..1000 {
            assert!(r.below(10) < 10);
        }
        assert_eq!(r.below(0), 0);
    }

    #[test]
    fn fill_bytes_is_deterministic() {
        let mut a = SplitMix64::new(99);
        let mut b = SplitMix64::new(99);
        let mut buf_a = [0u8; 13];
        let mut buf_b = [0u8; 13];
        a.fill_bytes(&mut buf_a);
        b.fill_bytes(&mut buf_b);
        assert_eq!(buf_a, buf_b);
    }
}
