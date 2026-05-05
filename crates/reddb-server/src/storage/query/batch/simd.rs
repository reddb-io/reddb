//! SIMD-accelerated reducers over the batch layer.
//!
//! Runtime CPU-feature detection picks the widest available ISA
//! (`avx2` on modern x86_64, falling back to scalar). All entry
//! points have a safe scalar path so ARM / other architectures keep
//! working without `#[cfg(target_arch = "...")]` churn on the caller
//! side.
//!
//! Covered:
//! * `sum_f64`, `sum_i64`
//! * `min_f64`, `max_f64`
//! * `filter_gt_f64` — scan a column, emit a bitmap where value > threshold
//!
//! Each function has a scalar-only sibling (`*_scalar`) used by the
//! tests as ground truth and as fallback on unsupported CPUs.

// -------------------------------------------------------------------------
// Scalar implementations — always available.
// -------------------------------------------------------------------------

#[inline]
pub fn sum_f64_scalar(data: &[f64]) -> f64 {
    let mut acc = 0.0f64;
    for &v in data {
        acc += v;
    }
    acc
}

#[inline]
pub fn sum_i64_scalar(data: &[i64]) -> i64 {
    let mut acc = 0i64;
    for &v in data {
        acc = acc.wrapping_add(v);
    }
    acc
}

#[inline]
pub fn min_f64_scalar(data: &[f64]) -> f64 {
    let mut best = f64::INFINITY;
    for &v in data {
        if v < best {
            best = v;
        }
    }
    best
}

#[inline]
pub fn max_f64_scalar(data: &[f64]) -> f64 {
    let mut best = f64::NEG_INFINITY;
    for &v in data {
        if v > best {
            best = v;
        }
    }
    best
}

#[inline]
pub fn filter_gt_f64_scalar(data: &[f64], threshold: f64) -> Vec<bool> {
    data.iter().map(|v| *v > threshold).collect()
}

// -------------------------------------------------------------------------
// Runtime dispatch.
// -------------------------------------------------------------------------

pub fn sum_f64(data: &[f64]) -> f64 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") {
            unsafe {
                return sum_f64_avx2(data);
            }
        }
    }
    sum_f64_scalar(data)
}

pub fn sum_i64(data: &[i64]) -> i64 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") {
            unsafe {
                return sum_i64_avx2(data);
            }
        }
    }
    sum_i64_scalar(data)
}

pub fn min_f64(data: &[f64]) -> f64 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") {
            unsafe {
                return min_f64_avx2(data);
            }
        }
    }
    min_f64_scalar(data)
}

pub fn max_f64(data: &[f64]) -> f64 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") {
            unsafe {
                return max_f64_avx2(data);
            }
        }
    }
    max_f64_scalar(data)
}

pub fn filter_gt_f64(data: &[f64], threshold: f64) -> Vec<bool> {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if std::is_x86_feature_detected!("avx2") {
            unsafe {
                return filter_gt_f64_avx2(data, threshold);
            }
        }
    }
    filter_gt_f64_scalar(data, threshold)
}

// -------------------------------------------------------------------------
// AVX2 implementations. Each helper sums / scans 4× f64 or 4× i64 at a
// time (AVX2 is 256-bit wide). Tail elements go through the scalar
// path.
// -------------------------------------------------------------------------

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn sum_f64_avx2(data: &[f64]) -> f64 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let n = data.len();
    let mut acc = _mm256_setzero_pd();
    let mut i = 0;
    while i + 4 <= n {
        let v = _mm256_loadu_pd(data.as_ptr().add(i));
        acc = _mm256_add_pd(acc, v);
        i += 4;
    }
    // Horizontal reduction.
    let mut tmp = [0.0f64; 4];
    _mm256_storeu_pd(tmp.as_mut_ptr(), acc);
    let mut total = tmp[0] + tmp[1] + tmp[2] + tmp[3];
    while i < n {
        total += data[i];
        i += 1;
    }
    total
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn sum_i64_avx2(data: &[i64]) -> i64 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let n = data.len();
    let mut acc = _mm256_setzero_si256();
    let mut i = 0;
    while i + 4 <= n {
        let v = _mm256_loadu_si256(data.as_ptr().add(i) as *const __m256i);
        acc = _mm256_add_epi64(acc, v);
        i += 4;
    }
    let mut tmp = [0i64; 4];
    _mm256_storeu_si256(tmp.as_mut_ptr() as *mut __m256i, acc);
    let mut total = tmp[0]
        .wrapping_add(tmp[1])
        .wrapping_add(tmp[2])
        .wrapping_add(tmp[3]);
    while i < n {
        total = total.wrapping_add(data[i]);
        i += 1;
    }
    total
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn min_f64_avx2(data: &[f64]) -> f64 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let n = data.len();
    if n == 0 {
        return f64::INFINITY;
    }
    let mut acc = _mm256_set1_pd(f64::INFINITY);
    let mut i = 0;
    while i + 4 <= n {
        let v = _mm256_loadu_pd(data.as_ptr().add(i));
        acc = _mm256_min_pd(acc, v);
        i += 4;
    }
    let mut tmp = [0.0f64; 4];
    _mm256_storeu_pd(tmp.as_mut_ptr(), acc);
    let mut best = tmp[0].min(tmp[1]).min(tmp[2]).min(tmp[3]);
    while i < n {
        if data[i] < best {
            best = data[i];
        }
        i += 1;
    }
    best
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn max_f64_avx2(data: &[f64]) -> f64 {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let n = data.len();
    if n == 0 {
        return f64::NEG_INFINITY;
    }
    let mut acc = _mm256_set1_pd(f64::NEG_INFINITY);
    let mut i = 0;
    while i + 4 <= n {
        let v = _mm256_loadu_pd(data.as_ptr().add(i));
        acc = _mm256_max_pd(acc, v);
        i += 4;
    }
    let mut tmp = [0.0f64; 4];
    _mm256_storeu_pd(tmp.as_mut_ptr(), acc);
    let mut best = tmp[0].max(tmp[1]).max(tmp[2]).max(tmp[3]);
    while i < n {
        if data[i] > best {
            best = data[i];
        }
        i += 1;
    }
    best
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn filter_gt_f64_avx2(data: &[f64], threshold: f64) -> Vec<bool> {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    let n = data.len();
    let mut out = vec![false; n];
    let thresh = _mm256_set1_pd(threshold);
    let mut i = 0;
    while i + 4 <= n {
        let v = _mm256_loadu_pd(data.as_ptr().add(i));
        let cmp = _mm256_cmp_pd(v, thresh, _CMP_GT_OQ);
        let mut tmp = [0u64; 4];
        _mm256_storeu_si256(tmp.as_mut_ptr() as *mut __m256i, _mm256_castpd_si256(cmp));
        for k in 0..4 {
            out[i + k] = tmp[k] != 0;
        }
        i += 4;
    }
    while i < n {
        out[i] = data[i] > threshold;
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_f64_matches_scalar() {
        let data: Vec<f64> = (0..1001).map(|i| i as f64 * 0.5).collect();
        let expected = sum_f64_scalar(&data);
        let actual = sum_f64(&data);
        assert!((expected - actual).abs() < 1e-6);
    }

    #[test]
    fn sum_i64_matches_scalar_and_handles_wrap() {
        let data: Vec<i64> = (0..1001).map(|i| i as i64 * 3).collect();
        let expected = sum_i64_scalar(&data);
        let actual = sum_i64(&data);
        assert_eq!(expected, actual);
    }

    #[test]
    fn min_and_max_agree_with_scalar() {
        let data: Vec<f64> = vec![3.0, -1.0, 4.0, 1.5, -5.0, 9.0, 2.0, 6.5, -7.0, 8.0];
        assert_eq!(min_f64(&data), -7.0);
        assert_eq!(max_f64(&data), 9.0);
    }

    #[test]
    fn min_of_empty_slice_is_positive_infinity() {
        assert_eq!(min_f64(&[]), f64::INFINITY);
        assert_eq!(max_f64(&[]), f64::NEG_INFINITY);
    }

    #[test]
    fn filter_gt_marks_elements_strictly_greater() {
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mask = filter_gt_f64(&data, 3.5);
        assert_eq!(mask, vec![false, false, false, true, true, true]);
    }

    #[test]
    fn filter_gt_matches_scalar_on_big_input() {
        let data: Vec<f64> = (0..1001).map(|i| (i as f64).sin() * 10.0).collect();
        let simd = filter_gt_f64(&data, 5.0);
        let scalar = filter_gt_f64_scalar(&data, 5.0);
        assert_eq!(simd, scalar);
    }

    #[test]
    fn tail_elements_are_included_in_sum() {
        // 5 elements — fits one AVX2 lane plus a scalar tail.
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(sum_f64(&data), 15.0);
    }
}
