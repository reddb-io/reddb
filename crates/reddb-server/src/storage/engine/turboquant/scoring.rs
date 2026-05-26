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
    Avx2,
    Avx512Bw,
    Neon,
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

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
static AVX2_SCORER: Avx2Scorer = Avx2Scorer;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
static AVX512BW_SCORER: Avx512BwScorer = Avx512BwScorer;

#[cfg(target_arch = "aarch64")]
static NEON_SCORER: NeonScorer = NeonScorer;

/// Pick the best available scoring kernel for this host. SIMD slices
/// register themselves here without touching the call site; the choice
/// is made once per query (no per-block branch cost).
pub fn select_scorer() -> &'static dyn PerBlockScorer {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        // AVX-512BW is the widest x86 kernel and takes precedence over
        // AVX2 when available. FMA is required throughout to match
        // `ScalarScorer`'s `f32::mul_add` bit-exactly; the SIMD paths
        // use `vfmadd` rather than separate mul+add to stay byte-
        // identical. AVX-512F implies AVX2, and FMA3 ships on every
        // relevant Intel/AMD AVX2+ core; rare hosts that drop FMA fall
        // back to scalar.
        if std::is_x86_feature_detected!("avx512bw")
            && std::is_x86_feature_detected!("avx512f")
            && std::is_x86_feature_detected!("fma")
        {
            return &AVX512BW_SCORER;
        }
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            return &AVX2_SCORER;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // NEON (Advanced SIMD) is mandatory in the AArch64 base ISA, so
        // no runtime detection is needed — every aarch64 host supports
        // the intrinsics this kernel uses. `vfmaq_f32` is the
        // single-rounding fused multiply-add that matches
        // `ScalarScorer`'s `f32::mul_add` bit-exactly.
        return &NEON_SCORER;
    }
    #[allow(unreachable_code)]
    &SCALAR_SCORER
}

/// AVX2 block scorer. Reads aligned 256-bit lanes straight from
/// [`super::storage::BlockedCodeStorage::block_codes`] with no
/// per-query repack and table-looks up nibble scores via `vpshufb`.
///
/// MIT notice: the SIMD body is adapted from RyanCodrai/turbovec
/// (commit `4a4f2cd2db233f24405911b1ceaf1823fa23b4ac`, MIT). The
/// per-vector scale and tail handling are clean-room — the trait
/// returns the unit-rotated dot product only; outer code applies
/// metric and per-vector scale, matching [`ScalarScorer`].
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub struct Avx2Scorer;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl PerBlockScorer for Avx2Scorer {
    fn kernel(&self) -> ScoreKernel {
        ScoreKernel::Avx2
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
        debug_assert!(std::is_x86_feature_detected!("avx2"));
        debug_assert!(std::is_x86_feature_detected!("fma"));
        // SAFETY: AVX2 + FMA availability is enforced by `select_scorer`
        // and re-asserted by the debug checks above.
        unsafe {
            score_block_avx2_inner(lut, block_codes, n_byte_groups, n_vectors, out);
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2,fma")]
unsafe fn score_block_avx2_inner(
    lut: &QueryLut,
    block_codes: &[u8],
    n_byte_groups: usize,
    n_vectors: usize,
    out: &mut [f32; BLOCK_LANES],
) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let mut accum = [_mm256_setzero_si256(); 4];
    let nibble_mask = _mm256_set1_epi8(0x0f);

    for g in 0..n_byte_groups {
        let codes = _mm256_loadu_si256(block_codes.as_ptr().add(g * BLOCK_LANES) as *const __m256i);
        let clo = _mm256_and_si256(codes, nibble_mask);
        let chi = _mm256_and_si256(_mm256_srli_epi16(codes, 4), nibble_mask);
        let table = _mm256_loadu_si256(lut.bytes.as_ptr().add(g * 32) as *const __m256i);
        let lo_scores = _mm256_shuffle_epi8(table, clo);
        let hi_scores = _mm256_shuffle_epi8(table, chi);

        accum[0] = _mm256_add_epi16(accum[0], lo_scores);
        accum[1] = _mm256_add_epi16(accum[1], _mm256_srli_epi16(lo_scores, 8));
        accum[2] = _mm256_add_epi16(accum[2], hi_scores);
        accum[3] = _mm256_add_epi16(accum[3], _mm256_srli_epi16(hi_scores, 8));
    }

    accum[0] = _mm256_sub_epi16(accum[0], _mm256_slli_epi16(accum[1], 8));
    accum[2] = _mm256_sub_epi16(accum[2], _mm256_slli_epi16(accum[3], 8));

    let dis0 = _mm256_add_epi16(
        _mm256_permute2x128_si256(accum[0], accum[1], 0x21),
        _mm256_blend_epi32(accum[0], accum[1], 0xf0),
    );
    let dis1 = _mm256_add_epi16(
        _mm256_permute2x128_si256(accum[2], accum[3], 0x21),
        _mm256_blend_epi32(accum[2], accum[3], 0xf0),
    );

    let sums = [
        _mm256_cvtepi32_ps(_mm256_cvtepu16_epi32(_mm256_castsi256_si128(dis0))),
        _mm256_cvtepi32_ps(_mm256_cvtepu16_epi32(_mm256_extracti128_si256(dis0, 1))),
        _mm256_cvtepi32_ps(_mm256_cvtepu16_epi32(_mm256_castsi256_si128(dis1))),
        _mm256_cvtepi32_ps(_mm256_cvtepu16_epi32(_mm256_extracti128_si256(dis1, 1))),
    ];
    let v_scale = _mm256_set1_ps(lut.scale);
    let v_bias = _mm256_set1_ps(lut.bias);

    for (chunk, sum) in sums.iter().enumerate() {
        let lane_start = chunk * 8;
        // FMA matches `f32::mul_add` used by `ScalarScorer` bit-exactly.
        let score = _mm256_fmadd_ps(v_scale, *sum, v_bias);
        _mm256_storeu_ps(out.as_mut_ptr().add(lane_start), score);
    }

    // Tail lanes match the scalar oracle: unused slots are 0.0.
    for score in out.iter_mut().take(BLOCK_LANES).skip(n_vectors) {
        *score = 0.0;
    }
}

/// AVX-512BW block scorer. Processes pairs of byte groups in a single
/// 512-bit register so that `vpshufb` resolves four 128-bit lane
/// lookups per iteration (two groups × hi/lo tables).
///
/// MIT notice: the paired-group `vpshufb` + u16-accumulator structure
/// is adapted from RyanCodrai/turbovec (commit
/// `4a4f2cd2db233f24405911b1ceaf1823fa23b4ac`, MIT). The single-block
/// trait surface, the fold from 512→256-bit lanes, and the tail
/// handling are clean-room.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub struct Avx512BwScorer;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl PerBlockScorer for Avx512BwScorer {
    fn kernel(&self) -> ScoreKernel {
        ScoreKernel::Avx512Bw
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
        debug_assert!(std::is_x86_feature_detected!("avx512bw"));
        debug_assert!(std::is_x86_feature_detected!("avx512f"));
        debug_assert!(std::is_x86_feature_detected!("fma"));
        // SAFETY: AVX-512BW/F + FMA availability is enforced by
        // `select_scorer` and re-asserted by the debug checks above.
        unsafe {
            score_block_avx512bw_inner(lut, block_codes, n_byte_groups, n_vectors, out);
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx512bw,avx512f,avx2,avx,fma")]
unsafe fn score_block_avx512bw_inner(
    lut: &QueryLut,
    block_codes: &[u8],
    n_byte_groups: usize,
    n_vectors: usize,
    out: &mut [f32; BLOCK_LANES],
) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    // 32 u16 lanes per accumulator — one position per byte across two
    // adjacent groups. The lower 256 bits hold group g, the upper 256
    // hold group g+1; they are folded together after the loop. The u16
    // headroom matches the AVX2 kernel: max sum per lane = 2 *
    // n_byte_groups * MAX_LUT, safe for n_byte_groups <= 258.
    let mut accum = [_mm512_setzero_si512(); 4];
    let nibble_mask = _mm512_set1_epi8(0x0f);

    let mut g = 0;
    while g + 2 <= n_byte_groups {
        let codes = _mm512_loadu_si512(block_codes.as_ptr().add(g * BLOCK_LANES) as *const _);
        let clo = _mm512_and_si512(codes, nibble_mask);
        let chi = _mm512_and_si512(_mm512_srli_epi16(codes, 4), nibble_mask);
        let table = _mm512_loadu_si512(lut.bytes.as_ptr().add(g * 32) as *const _);
        let lo_scores = _mm512_shuffle_epi8(table, clo);
        let hi_scores = _mm512_shuffle_epi8(table, chi);

        accum[0] = _mm512_add_epi16(accum[0], lo_scores);
        accum[1] = _mm512_add_epi16(accum[1], _mm512_srli_epi16(lo_scores, 8));
        accum[2] = _mm512_add_epi16(accum[2], hi_scores);
        accum[3] = _mm512_add_epi16(accum[3], _mm512_srli_epi16(hi_scores, 8));

        g += 2;
    }

    // Fold the 512-bit accumulators back to 256-bit by summing the two
    // halves. After this, each `accum256[i]` matches what the AVX2
    // kernel computes for the same `accum[i]`, modulo the unpaired
    // tail group which is added below.
    let mut accum256 = [
        _mm256_add_epi16(
            _mm512_castsi512_si256(accum[0]),
            _mm512_extracti64x4_epi64(accum[0], 1),
        ),
        _mm256_add_epi16(
            _mm512_castsi512_si256(accum[1]),
            _mm512_extracti64x4_epi64(accum[1], 1),
        ),
        _mm256_add_epi16(
            _mm512_castsi512_si256(accum[2]),
            _mm512_extracti64x4_epi64(accum[2], 1),
        ),
        _mm256_add_epi16(
            _mm512_castsi512_si256(accum[3]),
            _mm512_extracti64x4_epi64(accum[3], 1),
        ),
    ];

    // Tail: an odd number of byte groups leaves one group unpaired.
    // Handle it with the AVX2-shaped 256-bit kernel body so the result
    // remains bit-identical to `ScalarScorer` and `Avx2Scorer`.
    if g < n_byte_groups {
        let nibble_mask_256 = _mm256_set1_epi8(0x0f);
        let codes = _mm256_loadu_si256(block_codes.as_ptr().add(g * BLOCK_LANES) as *const __m256i);
        let clo = _mm256_and_si256(codes, nibble_mask_256);
        let chi = _mm256_and_si256(_mm256_srli_epi16(codes, 4), nibble_mask_256);
        let table = _mm256_loadu_si256(lut.bytes.as_ptr().add(g * 32) as *const __m256i);
        let lo_scores = _mm256_shuffle_epi8(table, clo);
        let hi_scores = _mm256_shuffle_epi8(table, chi);

        accum256[0] = _mm256_add_epi16(accum256[0], lo_scores);
        accum256[1] = _mm256_add_epi16(accum256[1], _mm256_srli_epi16(lo_scores, 8));
        accum256[2] = _mm256_add_epi16(accum256[2], hi_scores);
        accum256[3] = _mm256_add_epi16(accum256[3], _mm256_srli_epi16(hi_scores, 8));
    }

    // Split the (high, low) byte-position sums per the AVX2 trick:
    // accum256[0] currently holds, per 16-bit lane i,
    //   lo_scores[2i] + lo_scores[2i+1] * 256 summed across groups.
    // accum256[1] holds sum of lo_scores[2i+1] (zero-extended).
    // Subtracting (accum256[1] << 8) leaves sum of lo_scores[2i].
    accum256[0] = _mm256_sub_epi16(accum256[0], _mm256_slli_epi16(accum256[1], 8));
    accum256[2] = _mm256_sub_epi16(accum256[2], _mm256_slli_epi16(accum256[3], 8));

    // Interleave even/odd byte-position sums back into lane order.
    let dis0 = _mm256_add_epi16(
        _mm256_permute2x128_si256(accum256[0], accum256[1], 0x21),
        _mm256_blend_epi32(accum256[0], accum256[1], 0xf0),
    );
    let dis1 = _mm256_add_epi16(
        _mm256_permute2x128_si256(accum256[2], accum256[3], 0x21),
        _mm256_blend_epi32(accum256[2], accum256[3], 0xf0),
    );

    let v_scale = _mm512_set1_ps(lut.scale);
    let v_bias = _mm512_set1_ps(lut.bias);

    // 16 u16 → 16 u32 → 16 f32 per store. FMA matches `f32::mul_add`
    // bit-exactly, the contract the scalar oracle locks down.
    let sum0 = _mm512_cvtepi32_ps(_mm512_cvtepu16_epi32(dis0));
    let scores0 = _mm512_fmadd_ps(v_scale, sum0, v_bias);
    _mm512_storeu_ps(out.as_mut_ptr(), scores0);

    let sum1 = _mm512_cvtepi32_ps(_mm512_cvtepu16_epi32(dis1));
    let scores1 = _mm512_fmadd_ps(v_scale, sum1, v_bias);
    _mm512_storeu_ps(out.as_mut_ptr().add(16), scores1);

    // Tail lanes match the scalar oracle: unused slots are 0.0.
    for score in out.iter_mut().take(BLOCK_LANES).skip(n_vectors) {
        *score = 0.0;
    }
}

/// NEON block scorer. Reads aligned 128-bit lanes straight from
/// [`super::storage::BlockedCodeStorage::block_codes`] with no
/// per-query repack and table-looks up nibble scores via `vqtbl1q_u8`.
///
/// MIT notice: the SIMD body is adapted from RyanCodrai/turbovec
/// (commit `4a4f2cd2db233f24405911b1ceaf1823fa23b4ac`, MIT). The
/// single-block trait surface, the PERM0-aware lane scatter, and the
/// tail handling are clean-room — the trait returns the unit-rotated
/// dot product only; outer code applies metric and per-vector scale,
/// matching [`ScalarScorer`].
#[cfg(target_arch = "aarch64")]
pub struct NeonScorer;

#[cfg(target_arch = "aarch64")]
impl PerBlockScorer for NeonScorer {
    fn kernel(&self) -> ScoreKernel {
        ScoreKernel::Neon
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
        // SAFETY: NEON is part of the mandatory aarch64 base ISA, so
        // every `target_arch = "aarch64"` host supports these intrinsics.
        unsafe {
            score_block_neon_inner(lut, block_codes, n_byte_groups, n_vectors, out);
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn score_block_neon_inner(
    lut: &QueryLut,
    block_codes: &[u8],
    n_byte_groups: usize,
    n_vectors: usize,
    out: &mut [f32; BLOCK_LANES],
) {
    use std::arch::aarch64::*;

    // Four u16x8 accumulators cover the 32-lane block split as two
    // 16-lane halves (half-0 = lanes `PERM0[i]`, half-1 = lanes
    // `PERM0[i] + 16` for `i in 0..16`). Each half splits again into a
    // low 8-position u16x8 and a high 8-position u16x8. u16 headroom
    // matches the AVX2/AVX-512BW kernels: max sum per position = 2 *
    // n_byte_groups * MAX_LUT, safe for n_byte_groups <= 258.
    let mut acc_h0_lo = vdupq_n_u16(0);
    let mut acc_h0_hi = vdupq_n_u16(0);
    let mut acc_h1_lo = vdupq_n_u16(0);
    let mut acc_h1_hi = vdupq_n_u16(0);
    let nibble_mask = vdupq_n_u8(0x0f);

    for g in 0..n_byte_groups {
        let base = g * BLOCK_LANES;
        // 16 bytes of "hi-pairs" (perm-positions 0..16) and 16 bytes of
        // "lo-pairs" (perm-positions 16..32). Each byte packs two
        // 4-bit codes: low nibble belongs to lane `PERM0[perm_pos]`
        // (half 0), high nibble to lane `PERM0[perm_pos] + 16` (half 1).
        let hi_pair = vld1q_u8(block_codes.as_ptr().add(base));
        let lo_pair = vld1q_u8(block_codes.as_ptr().add(base + 16));
        let hi_lut = vld1q_u8(lut.bytes.as_ptr().add(g * 32));
        let lo_lut = vld1q_u8(lut.bytes.as_ptr().add(g * 32 + 16));

        // Half-0 nibble indices (low nibbles).
        let idx_lo_h0 = vandq_u8(lo_pair, nibble_mask);
        let idx_hi_h0 = vandq_u8(hi_pair, nibble_mask);
        // Half-1 nibble indices (high nibbles).
        let idx_lo_h1 = vshrq_n_u8(lo_pair, 4);
        let idx_hi_h1 = vshrq_n_u8(hi_pair, 4);

        // `vqtbl1q_u8` does a 16-entry byte table lookup; all indices
        // are masked to 0..16 so no out-of-range result is consumed.
        let s_lo_h0 = vqtbl1q_u8(lo_lut, idx_lo_h0);
        let s_hi_h0 = vqtbl1q_u8(hi_lut, idx_hi_h0);
        let s_lo_h1 = vqtbl1q_u8(lo_lut, idx_lo_h1);
        let s_hi_h1 = vqtbl1q_u8(hi_lut, idx_hi_h1);

        // Per-position score = lo-table contribution + hi-table
        // contribution. `vaddl_u8` widens u8x8 + u8x8 → u16x8 in one
        // op, accumulating without overflow into the u16 lanes.
        acc_h0_lo = vaddq_u16(
            acc_h0_lo,
            vaddl_u8(vget_low_u8(s_lo_h0), vget_low_u8(s_hi_h0)),
        );
        acc_h0_hi = vaddq_u16(
            acc_h0_hi,
            vaddl_u8(vget_high_u8(s_lo_h0), vget_high_u8(s_hi_h0)),
        );
        acc_h1_lo = vaddq_u16(
            acc_h1_lo,
            vaddl_u8(vget_low_u8(s_lo_h1), vget_low_u8(s_hi_h1)),
        );
        acc_h1_hi = vaddq_u16(
            acc_h1_hi,
            vaddl_u8(vget_high_u8(s_lo_h1), vget_high_u8(s_hi_h1)),
        );
    }

    let scale = vdupq_n_f32(lut.scale);
    let bias = vdupq_n_f32(lut.bias);

    // Widen u16x8 → u32x4 × 2 → f32x4 × 2 then FMA with scale/bias.
    // `vfmaq_f32(a, b, c) = a + b * c` is a single-rounding fused
    // multiply-add and matches `f32::mul_add` bit-exactly — the
    // contract the scalar oracle locks down.
    let conv = |acc_u16: uint16x8_t| -> [f32; 8] {
        let lo_u32 = vmovl_u16(vget_low_u16(acc_u16));
        let hi_u32 = vmovl_u16(vget_high_u16(acc_u16));
        let lo_f = vcvtq_f32_u32(lo_u32);
        let hi_f = vcvtq_f32_u32(hi_u32);
        let lo_score = vfmaq_f32(bias, scale, lo_f);
        let hi_score = vfmaq_f32(bias, scale, hi_f);
        let mut tmp = [0.0f32; 8];
        vst1q_f32(tmp.as_mut_ptr(), lo_score);
        vst1q_f32(tmp.as_mut_ptr().add(4), hi_score);
        tmp
    };

    let h0_lo = conv(acc_h0_lo);
    let h0_hi = conv(acc_h0_hi);
    let h1_lo = conv(acc_h1_lo);
    let h1_hi = conv(acc_h1_hi);

    // Scatter perm-positions back to lane order. `scores_hN[perm_pos]`
    // is the score for the lane PERM0 maps `perm_pos` to (offset by 16
    // for half 1). Done in scalar — this is 32 stores at the tail of
    // the hot loop, dwarfed by the per-group SIMD work above.
    let mut scores_h0 = [0.0f32; 16];
    let mut scores_h1 = [0.0f32; 16];
    scores_h0[..8].copy_from_slice(&h0_lo);
    scores_h0[8..].copy_from_slice(&h0_hi);
    scores_h1[..8].copy_from_slice(&h1_lo);
    scores_h1[8..].copy_from_slice(&h1_hi);

    for perm_pos in 0..16 {
        let lane = PERM0[perm_pos];
        out[lane] = scores_h0[perm_pos];
        out[lane + 16] = scores_h1[perm_pos];
    }

    // Tail lanes match the scalar oracle: unused slots are 0.0.
    for score in out.iter_mut().take(BLOCK_LANES).skip(n_vectors) {
        *score = 0.0;
    }
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
    fn select_scorer_matches_host_capability() {
        let kernel = select_scorer().kernel();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            if std::is_x86_feature_detected!("avx512bw")
                && std::is_x86_feature_detected!("avx512f")
                && std::is_x86_feature_detected!("fma")
            {
                assert_eq!(kernel, ScoreKernel::Avx512Bw);
                return;
            }
            if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
                assert_eq!(kernel, ScoreKernel::Avx2);
                return;
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            // NEON is mandatory on aarch64 — no runtime gate; the
            // dispatch must always pick it on this arch.
            assert_eq!(kernel, ScoreKernel::Neon);
            return;
        }
        #[allow(unreachable_code)]
        {
            assert_eq!(kernel, ScoreKernel::Scalar);
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx2_scorer_matches_scalar_oracle_across_dataset_sizes() {
        if !std::is_x86_feature_detected!("avx2") || !std::is_x86_feature_detected!("fma") {
            return;
        }
        let centroids = centroids_for(4);
        // Queries chosen to exercise different LUT shapes (zero, sign-mixed,
        // single-axis) and to keep n_byte_groups small enough that the
        // AVX2 kernel's u16 accumulators cannot overflow vs the scalar
        // u32 oracle (max sum per lane = 2*n_byte_groups*127).
        let queries: [Vec<f32>; 4] = [
            vec![0.0; 8],
            vec![0.7, -0.3, 0.4, -0.1, 0.2, -0.5, 0.6, -0.9],
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
        ];

        for query in &queries {
            let lut = QueryLut::build(query, &centroids);
            let n_byte_groups = lut.n_byte_groups;

            for n in [1usize, 31, 32, 33, 95, 96, 97] {
                let mut storage = BlockedCodeStorage::new(n_byte_groups);
                for i in 0..n {
                    let packed: Vec<u8> = (0..n_byte_groups)
                        .map(|g| {
                            let lo = ((i + g * 3) & 0x0f) as u8;
                            let hi = ((i * 5 + g * 7) & 0x0f) as u8;
                            lo | (hi << 4)
                        })
                        .collect();
                    storage.append(&packed, 1.0);
                }

                for b in 0..storage.n_blocks() {
                    let filled = storage.block_lanes_filled(b);
                    let mut scalar_out = [0.0f32; BLOCK_LANES];
                    let mut avx2_out = [f32::NAN; BLOCK_LANES];
                    ScalarScorer.score_block(
                        &lut,
                        storage.block_codes(b),
                        n_byte_groups,
                        filled,
                        &mut scalar_out,
                    );
                    AVX2_SCORER.score_block(
                        &lut,
                        storage.block_codes(b),
                        n_byte_groups,
                        filled,
                        &mut avx2_out,
                    );
                    for lane in 0..BLOCK_LANES {
                        assert_eq!(
                            avx2_out[lane].to_bits(),
                            scalar_out[lane].to_bits(),
                            "AVX2 diverges from scalar at N={n}, block {b}, lane {lane}",
                        );
                    }
                }
            }
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx512bw_scorer_matches_scalar_oracle_across_dataset_sizes() {
        if !std::is_x86_feature_detected!("avx512bw")
            || !std::is_x86_feature_detected!("avx512f")
            || !std::is_x86_feature_detected!("fma")
        {
            return;
        }
        let centroids = centroids_for(4);
        // Queries chosen to exercise different LUT shapes (zero,
        // sign-mixed, single-axis) plus a query whose n_byte_groups is
        // odd (5) so the kernel exercises both the paired-group main
        // loop and the unpaired-tail branch. `n_byte_groups` stays
        // small enough that the u16 accumulators cannot overflow vs
        // the scalar u32 oracle (max sum per lane = 2 *
        // n_byte_groups * 127).
        let queries: [Vec<f32>; 5] = [
            vec![0.0; 8],
            vec![0.7, -0.3, 0.4, -0.1, 0.2, -0.5, 0.6, -0.9],
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
            // 10-dim → 5 byte groups: odd count exercises the tail.
            vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8, 0.9, -1.0],
        ];

        for query in &queries {
            let lut = QueryLut::build(query, &centroids);
            let n_byte_groups = lut.n_byte_groups;

            for n in [1usize, 31, 32, 33, 95, 96, 97] {
                let mut storage = BlockedCodeStorage::new(n_byte_groups);
                for i in 0..n {
                    let packed: Vec<u8> = (0..n_byte_groups)
                        .map(|g| {
                            let lo = ((i + g * 3) & 0x0f) as u8;
                            let hi = ((i * 5 + g * 7) & 0x0f) as u8;
                            lo | (hi << 4)
                        })
                        .collect();
                    storage.append(&packed, 1.0);
                }

                for b in 0..storage.n_blocks() {
                    let filled = storage.block_lanes_filled(b);
                    let mut scalar_out = [0.0f32; BLOCK_LANES];
                    let mut avx512_out = [f32::NAN; BLOCK_LANES];
                    ScalarScorer.score_block(
                        &lut,
                        storage.block_codes(b),
                        n_byte_groups,
                        filled,
                        &mut scalar_out,
                    );
                    AVX512BW_SCORER.score_block(
                        &lut,
                        storage.block_codes(b),
                        n_byte_groups,
                        filled,
                        &mut avx512_out,
                    );
                    for lane in 0..BLOCK_LANES {
                        assert_eq!(
                            avx512_out[lane].to_bits(),
                            scalar_out[lane].to_bits(),
                            "AVX-512BW diverges from scalar at N={n}, block {b}, lane {lane}",
                        );
                    }
                }
            }
        }
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn neon_scorer_matches_scalar_oracle_across_dataset_sizes() {
        // NEON is mandatory on aarch64; no runtime gate needed. Query
        // shapes mirror the AVX-512BW test so the paired-byte structure
        // and the odd-`n_byte_groups` tail-only case are both covered.
        // `n_byte_groups` stays small enough that u16 accumulators
        // cannot overflow vs the scalar u32 oracle (max sum per
        // position = 2 * n_byte_groups * 127).
        let centroids = centroids_for(4);
        let queries: [Vec<f32>; 5] = [
            vec![0.0; 8],
            vec![0.7, -0.3, 0.4, -0.1, 0.2, -0.5, 0.6, -0.9],
            vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8],
            // 10-dim → 5 byte groups: odd count exercises the tail of
            // any paired-group kernel (NEON walks groups one at a time
            // so there is no paired/unpaired distinction, but the case
            // is kept for parity with the AVX-512BW test).
            vec![0.1, -0.2, 0.3, -0.4, 0.5, -0.6, 0.7, -0.8, 0.9, -1.0],
        ];

        for query in &queries {
            let lut = QueryLut::build(query, &centroids);
            let n_byte_groups = lut.n_byte_groups;

            // Coverage includes the tail-only case (n < BLOCK_LANES)
            // and exact-block-boundary sizes (32, 96) per acceptance.
            for n in [1usize, 31, 32, 33, 95, 96, 97] {
                let mut storage = BlockedCodeStorage::new(n_byte_groups);
                for i in 0..n {
                    let packed: Vec<u8> = (0..n_byte_groups)
                        .map(|g| {
                            let lo = ((i + g * 3) & 0x0f) as u8;
                            let hi = ((i * 5 + g * 7) & 0x0f) as u8;
                            lo | (hi << 4)
                        })
                        .collect();
                    storage.append(&packed, 1.0);
                }

                for b in 0..storage.n_blocks() {
                    let filled = storage.block_lanes_filled(b);
                    let mut scalar_out = [0.0f32; BLOCK_LANES];
                    let mut neon_out = [f32::NAN; BLOCK_LANES];
                    ScalarScorer.score_block(
                        &lut,
                        storage.block_codes(b),
                        n_byte_groups,
                        filled,
                        &mut scalar_out,
                    );
                    NEON_SCORER.score_block(
                        &lut,
                        storage.block_codes(b),
                        n_byte_groups,
                        filled,
                        &mut neon_out,
                    );
                    for lane in 0..BLOCK_LANES {
                        assert_eq!(
                            neon_out[lane].to_bits(),
                            scalar_out[lane].to_bits(),
                            "NEON diverges from scalar at N={n}, block {b}, lane {lane}",
                        );
                    }
                }
            }
        }
    }
}
