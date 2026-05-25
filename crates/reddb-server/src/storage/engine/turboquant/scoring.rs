//! TurboQuant per-block scoring kernels.
//!
//! This slice (S1 of PRD #688 / ADR 0024) ships the reference scalar
//! kernel only. SIMD kernels (NEON, AVX2, AVX-512BW) are added in
//! later slices and join [`select_scorer`] without changing the
//! dispatch surface or paying a per-query branch cost.
//!
//! MIT notice: LUT construction shape and the PERM0-aware decode loop
//! are derived from RyanCodrai/turbovec (commit
//! `4a4f2cd2db233f24405911b1ceaf1823fa23b4ac`, MIT); the RedDB
//! `PerBlockScorer` trait, dispatch, and scalar oracle are
//! clean-room.

use super::storage::{BLOCK_LANES, PERM0};

const MAX_LUT: f32 = 127.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreKernel {
    Scalar,
}

/// Per-query lookup table. Built once per `score_many` call and shared
/// across every block of a collection.
#[derive(Debug, Clone)]
pub struct QueryLut {
    pub bytes: Vec<u8>,
    pub n_byte_groups: usize,
    pub scale: f32,
    pub bias: f32,
}

impl QueryLut {
    pub fn build(query_terms: &[f32], centroids: &[f64]) -> Self {
        let n_byte_groups = query_terms.len().div_ceil(2);
        let mut float_luts = vec![0.0f32; n_byte_groups * 32];
        let mut max_span = 0.0f32;
        let mut bias = 0.0f32;

        for group in 0..n_byte_groups {
            let lo_term = query_terms[group * 2];
            let hi_term = query_terms.get(group * 2 + 1).copied().unwrap_or(0.0);
            let group_base = group * 32;

            for code in 0..16 {
                float_luts[group_base + code] = hi_term * centroids[code] as f32;
                float_luts[group_base + 16 + code] = lo_term * centroids[code] as f32;
            }

            let hi = &float_luts[group_base..group_base + 16];
            let lo = &float_luts[group_base + 16..group_base + 32];
            let hi_min = hi.iter().copied().fold(f32::INFINITY, f32::min);
            let hi_max = hi.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let lo_min = lo.iter().copied().fold(f32::INFINITY, f32::min);
            let lo_max = lo.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            bias += hi_min + lo_min;
            max_span = max_span.max((hi_max - hi_min).max(lo_max - lo_min));
        }

        let scale = if max_span > 1e-10 {
            max_span / MAX_LUT
        } else {
            1.0
        };
        let inv_scale = 1.0 / scale;
        let mut bytes = vec![0u8; n_byte_groups * 32];

        for group in 0..n_byte_groups {
            let group_base = group * 32;
            let hi_min = float_luts[group_base..group_base + 16]
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min);
            let lo_min = float_luts[group_base + 16..group_base + 32]
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min);
            for code in 0..16 {
                bytes[group_base + code] = ((float_luts[group_base + code] - hi_min) * inv_scale)
                    .round()
                    .clamp(0.0, MAX_LUT) as u8;
                bytes[group_base + 16 + code] = ((float_luts[group_base + 16 + code] - lo_min)
                    * inv_scale)
                    .round()
                    .clamp(0.0, MAX_LUT) as u8;
            }
        }

        Self {
            bytes,
            n_byte_groups,
            scale,
            bias,
        }
    }
}

/// Single block scoring trait. Each implementation consumes one
/// `block_codes` slice (PERM0-interleaved, 64-byte aligned by
/// [`super::storage::BlockedCodeStorage`]) and writes one score per
/// lane into `out`.
///
/// SIMD slices add impls (NEON, AVX2, AVX-512BW) and join
/// [`select_scorer`] without changing this trait.
pub trait PerBlockScorer: Sync + Send {
    fn kernel(&self) -> ScoreKernel;

    /// Compute `lut.scale * sum_g(lut[g][hi] + lut[g][lo]) + lut.bias`
    /// for each lane in `0..n_vectors`. Lanes `>= n_vectors` are filled
    /// with `0.0`. The output is the unit-rotated-query dot product —
    /// outer code applies metric-specific transforms and per-vector
    /// scales.
    fn score_block(
        &self,
        lut: &QueryLut,
        block_codes: &[u8],
        n_byte_groups: usize,
        n_vectors: usize,
        out: &mut [f32; BLOCK_LANES],
    );
}

/// Reference scalar implementation. Acts as the oracle the
/// equivalence-test harness keys off — every SIMD slice must match
/// this kernel bit-exactly.
pub struct ScalarScorer;

impl PerBlockScorer for ScalarScorer {
    fn kernel(&self) -> ScoreKernel {
        ScoreKernel::Scalar
    }

    fn score_block(
        &self,
        lut: &QueryLut,
        block_codes: &[u8],
        n_byte_groups: usize,
        n_vectors: usize,
        out: &mut [f32; BLOCK_LANES],
    ) {
        debug_assert_eq!(n_byte_groups, lut.n_byte_groups);
        debug_assert!(block_codes.len() >= n_byte_groups * BLOCK_LANES);
        debug_assert!(n_vectors <= BLOCK_LANES);

        for (lane, slot) in out.iter_mut().enumerate() {
            if lane >= n_vectors {
                *slot = 0.0;
                continue;
            }
            let mut acc = 0u32;
            for g in 0..n_byte_groups {
                let (hi, lo) = decode_perm0_byte(block_codes, g, lane);
                acc = acc.wrapping_add(lut.bytes[g * 32 + hi as usize] as u32);
                acc = acc.wrapping_add(lut.bytes[g * 32 + 16 + lo as usize] as u32);
            }
            *slot = lut.scale.mul_add(acc as f32, lut.bias);
        }
    }
}

static SCALAR_SCORER: ScalarScorer = ScalarScorer;

/// Pick the best available scoring kernel for this host. Today always
/// returns the scalar scorer; SIMD slices add themselves here without
/// touching the call site.
pub fn select_scorer() -> &'static dyn PerBlockScorer {
    &SCALAR_SCORER
}

fn decode_perm0_byte(block_codes: &[u8], group: usize, lane: usize) -> (u8, u8) {
    debug_assert!(lane < BLOCK_LANES);
    let half = lane / 16;
    let within_half = lane % 16;
    let perm_pos = PERM0
        .iter()
        .position(|&v| v == within_half)
        .expect("lane in perm0");
    let group_base = group * BLOCK_LANES;
    let hi_pair = block_codes[group_base + perm_pos];
    let lo_pair = block_codes[group_base + 16 + perm_pos];
    if half == 0 {
        (hi_pair & 0x0f, lo_pair & 0x0f)
    } else {
        (hi_pair >> 4, lo_pair >> 4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::engine::turboquant::storage::BlockedCodeStorage;

    fn centroids_for(bits: u8) -> Vec<f64> {
        let levels = 1usize << bits;
        let step = 2.0 / levels as f64;
        (0..levels)
            .map(|i| -1.0 + (i as f64 + 0.5) * step)
            .collect()
    }

    #[test]
    fn scalar_block_score_is_zero_when_query_is_zero() {
        let lut = QueryLut::build(&[0.0f32; 4], &centroids_for(4));
        let mut storage = BlockedCodeStorage::new(2);
        storage.append(&[0x12, 0x34], 1.0);
        let mut out = [0.0f32; BLOCK_LANES];
        ScalarScorer.score_block(&lut, storage.block_codes(0), 2, 1, &mut out);
        // Bias = 0 for zero query, scale arbitrary, acc clamps to LUT min
        // (all zero entries) → 0 + 0 == 0.
        assert_eq!(out[0], 0.0);
    }

    #[test]
    fn scalar_block_score_matches_per_vector_scalar_lut() {
        // Cross-check: reconstruct the per-vector scalar sum
        // (sum of LUT[hi] + LUT[16+lo] over groups) by direct
        // arithmetic on the per-vector packed bytes and confirm
        // ScalarScorer agrees on the equivalent lane.
        let centroids = centroids_for(4);
        let query = vec![0.2f32, -0.3, 0.4, -0.5];
        let lut = QueryLut::build(&query, &centroids);

        let n_byte_groups = 2;
        let mut storage = BlockedCodeStorage::new(n_byte_groups);
        let packed_a = vec![0xa3u8, 0x5c];
        let packed_b = vec![0x71u8, 0xfe];
        storage.append(&packed_a, 1.0);
        storage.append(&packed_b, 1.0);

        let mut out = [0.0f32; BLOCK_LANES];
        ScalarScorer.score_block(&lut, storage.block_codes(0), n_byte_groups, 2, &mut out);

        for (lane, packed) in [&packed_a, &packed_b].iter().enumerate() {
            let mut expected = 0u32;
            for (g, byte) in packed.iter().enumerate() {
                let lo = (byte & 0x0f) as usize;
                let hi = (byte >> 4) as usize;
                expected += lut.bytes[g * 32 + hi] as u32;
                expected += lut.bytes[g * 32 + 16 + lo] as u32;
            }
            let expected_f = lut.scale.mul_add(expected as f32, lut.bias);
            assert_eq!(
                out[lane], expected_f,
                "lane {lane} matches per-vector LUT scoring",
            );
        }

        for lane in 2..BLOCK_LANES {
            assert_eq!(out[lane], 0.0, "unused lane {lane} stays 0");
        }
    }

    #[test]
    fn select_scorer_returns_scalar_in_this_slice() {
        assert_eq!(select_scorer().kernel(), ScoreKernel::Scalar);
    }
}
