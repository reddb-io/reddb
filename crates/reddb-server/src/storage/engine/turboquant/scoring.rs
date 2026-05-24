//! TurboQuant lookup-table scoring kernels.
//!
//! MIT notice: NEON scoring structure is derived from the turbovec MIT upstream
//! at RyanCodrai/turbovec commit 4a4f2cd2db233f24405911b1ceaf1823fa23b4ac.

use super::codec::EncodedVector;

pub const BLOCK_LANES: usize = 32;
pub const QUERY_BATCH: usize = 4;
const MAX_LUT: f32 = 127.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScoreKernel {
    Scalar,
    Neon,
}

#[derive(Debug, Clone)]
pub struct QueryLut {
    uint8_luts: Vec<u8>,
    n_byte_groups: usize,
    scale: f32,
    bias: f32,
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
        let mut uint8_luts = vec![0u8; n_byte_groups * 32];

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
                uint8_luts[group_base + code] = ((float_luts[group_base + code] - hi_min)
                    * inv_scale)
                    .round()
                    .clamp(0.0, MAX_LUT) as u8;
                uint8_luts[group_base + 16 + code] = ((float_luts[group_base + 16 + code] - lo_min)
                    * inv_scale)
                    .round()
                    .clamp(0.0, MAX_LUT) as u8;
            }
        }

        Self {
            uint8_luts,
            n_byte_groups,
            scale,
            bias,
        }
    }

    pub fn score_scalar(&self, packed: &[u8]) -> f32 {
        let mut sum = 0u32;
        for group in 0..self.n_byte_groups {
            let codes = packed.get(group).copied().unwrap_or(0);
            let lo = (codes & 0x0f) as usize;
            let hi = (codes >> 4) as usize;
            let group_base = group * 32;
            sum += self.uint8_luts[group_base + hi] as u32;
            sum += self.uint8_luts[group_base + 16 + lo] as u32;
        }
        self.scale.mul_add(sum as f32, self.bias)
    }
}

#[inline]
pub fn detect_score_kernel() -> ScoreKernel {
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return ScoreKernel::Neon;
        }
    }
    ScoreKernel::Scalar
}

pub fn score_units(lut: &QueryLut, encoded: &[EncodedVector]) -> Vec<f32> {
    match detect_score_kernel() {
        #[cfg(target_arch = "aarch64")]
        ScoreKernel::Neon => unsafe { score_units_neon(lut, encoded) },
        _ => score_units_scalar(lut, encoded),
    }
}

pub fn score_query_batch(
    luts: [&QueryLut; QUERY_BATCH],
    encoded: &[EncodedVector],
) -> [Vec<f32>; QUERY_BATCH] {
    std::array::from_fn(|query| score_units(luts[query], encoded))
}

pub fn score_units_scalar(lut: &QueryLut, encoded: &[EncodedVector]) -> Vec<f32> {
    encoded
        .iter()
        .map(|vector| lut.score_scalar(&vector.packed))
        .collect()
}

#[inline]
pub fn block_has_allowed(_block_index: usize) -> bool {
    true
}

#[cfg(target_arch = "aarch64")]
unsafe fn score_units_neon(lut: &QueryLut, encoded: &[EncodedVector]) -> Vec<f32> {
    let mut scores = vec![0.0; encoded.len()];
    for (block_idx, block) in encoded.chunks(BLOCK_LANES).enumerate() {
        if !block_has_allowed(block_idx) {
            continue;
        }
        score_block_neon(
            lut,
            block,
            &mut scores[block_idx * BLOCK_LANES..block_idx * BLOCK_LANES + block.len()],
        );
    }
    scores
}

#[cfg(target_arch = "aarch64")]
unsafe fn score_block_neon(lut: &QueryLut, encoded: &[EncodedVector], out: &mut [f32]) {
    use std::arch::aarch64::*;

    debug_assert!(encoded.len() <= BLOCK_LANES);
    let mut acc = [0u32; BLOCK_LANES];
    let mask = vdupq_n_u8(0x0f);

    for group in 0..lut.n_byte_groups {
        let mut codes = [0u8; BLOCK_LANES];
        for (lane, vector) in encoded.iter().enumerate() {
            codes[lane] = vector.packed.get(group).copied().unwrap_or(0);
        }

        let group_base = group * 32;
        let lut_hi = vld1q_u8(lut.uint8_luts.as_ptr().add(group_base));
        let lut_lo = vld1q_u8(lut.uint8_luts.as_ptr().add(group_base + 16));

        for half in 0..2 {
            let lane_base = half * 16;
            let c = vld1q_u8(codes.as_ptr().add(lane_base));
            let lo = vandq_u8(c, mask);
            let hi = vshrq_n_u8(c, 4);
            let sum = vaddq_u8(vqtbl1q_u8(lut_lo, lo), vqtbl1q_u8(lut_hi, hi));
            let widened_lo = vmovl_u8(vget_low_u8(sum));
            let widened_hi = vmovl_u8(vget_high_u8(sum));
            let acc0 = vmovl_u16(vget_low_u16(widened_lo));
            let acc1 = vmovl_u16(vget_high_u16(widened_lo));
            let acc2 = vmovl_u16(vget_low_u16(widened_hi));
            let acc3 = vmovl_u16(vget_high_u16(widened_hi));
            vst1q_u32(
                acc.as_mut_ptr().add(lane_base),
                vaddq_u32(vld1q_u32(acc.as_ptr().add(lane_base)), acc0),
            );
            vst1q_u32(
                acc.as_mut_ptr().add(lane_base + 4),
                vaddq_u32(vld1q_u32(acc.as_ptr().add(lane_base + 4)), acc1),
            );
            vst1q_u32(
                acc.as_mut_ptr().add(lane_base + 8),
                vaddq_u32(vld1q_u32(acc.as_ptr().add(lane_base + 8)), acc2),
            );
            vst1q_u32(
                acc.as_mut_ptr().add(lane_base + 12),
                vaddq_u32(vld1q_u32(acc.as_ptr().add(lane_base + 12)), acc3),
            );
        }
    }

    let v_bias = vdupq_n_f32(lut.bias);
    let v_scale = vdupq_n_f32(lut.scale);
    let mut scored = [0.0f32; BLOCK_LANES];
    for chunk in 0..8 {
        let f = vcvtq_f32_u32(vld1q_u32(acc.as_ptr().add(chunk * 4)));
        vst1q_f32(
            scored.as_mut_ptr().add(chunk * 4),
            vfmaq_f32(v_bias, v_scale, f),
        );
    }

    out.copy_from_slice(&scored[..encoded.len()]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::Config;

    fn encoded_strategy() -> impl Strategy<Value = EncodedVector> {
        prop::collection::vec(any::<u8>(), 1..=33)
            .prop_map(|packed| EncodedVector { packed, scale: 1.0 })
    }

    fn query_strategy() -> impl Strategy<Value = Vec<f32>> {
        prop::collection::vec(-1.0f32..1.0, 1..=65)
    }

    fn centroids() -> Vec<f64> {
        (0..16)
            .map(|code| -1.0 + (code as f64 + 0.5) * 0.125)
            .collect()
    }

    proptest! {
        #![proptest_config(Config { cases: 10_000, ..Config::default() })]

        #[test]
        fn scalar_lut_scoring_is_finite_for_random_codes(query in query_strategy(), encoded in encoded_strategy()) {
            let lut = QueryLut::build(&query, &centroids());
            prop_assert!(lut.score_scalar(&encoded.packed).is_finite());
        }
    }

    #[test]
    fn scalar_lut_scoring_covers_required_edge_blocks() {
        let centroids = centroids();
        let cases = [
            (vec![0.0; 4], vec![0x00; 2]),
            (vec![1.0, -1.0, 0.5, -0.5], vec![0xff; 2]),
            (vec![-0.25, 0.5, -0.75], vec![0xf0; 2]),
        ];

        for (query, packed) in cases {
            let lut = QueryLut::build(&query, &centroids);
            assert_eq!(
                lut.score_scalar(&packed),
                score_units_scalar(&lut, &[EncodedVector { packed, scale: 1.0 }])[0]
            );
        }
    }

    #[test]
    fn four_query_batch_matches_individual_dispatch() {
        let centroids = centroids();
        let queries = [
            vec![0.0; 4],
            vec![1.0, -1.0, 0.5, -0.5],
            vec![-0.25, 0.5, -0.75, 1.0],
            vec![0.125, 0.25, 0.5, 1.0],
        ];
        let luts = queries
            .iter()
            .map(|query| QueryLut::build(query, &centroids))
            .collect::<Vec<_>>();
        let encoded = (0..BLOCK_LANES)
            .map(|lane| EncodedVector {
                packed: vec![lane as u8, 0xff],
                scale: 1.0,
            })
            .collect::<Vec<_>>();

        let batch = score_query_batch([&luts[0], &luts[1], &luts[2], &luts[3]], &encoded);
        for query in 0..QUERY_BATCH {
            assert_eq!(batch[query], score_units(&luts[query], &encoded));
        }
    }

    #[cfg(target_arch = "aarch64")]
    proptest! {
        #![proptest_config(Config { cases: 10_000, ..Config::default() })]

        #[test]
        fn neon_matches_scalar_for_random_blocks(query in query_strategy(), block in prop::collection::vec(encoded_strategy(), 1..=BLOCK_LANES)) {
            let lut = QueryLut::build(&query, &centroids());
            let scalar = score_units_scalar(&lut, &block);
            let neon = unsafe { score_units_neon(&lut, &block) };
            prop_assert_eq!(scalar, neon);
        }
    }
}
