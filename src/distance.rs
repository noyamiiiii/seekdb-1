//! Squared L2 distance between vectors.
//!
//! The inputs are `f32` and the sum is accumulated as `f64`, so the printed
//! distance stays stable and ordering matches a reference computed in double
//! precision. On x86-64 the inner loop uses AVX2 and FMA when the CPU has them;
//! the arithmetic stays `f64`, only the order of the additions changes.

/// Squared Euclidean distance; `None` when the dimensions differ.
pub fn squared(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
            // SAFETY: the feature check above guards the call.
            return Some(unsafe { squared_avx2(a, b) });
        }
    }
    Some(squared_scalar(a, b))
}

/// Euclidean distance; `None` when the dimensions differ.
pub fn distance(a: &[f32], b: &[f32]) -> Option<f64> {
    squared(a, b).map(f64::sqrt)
}

/// Squared distance between an in-memory vector and a row's stored `f32`
/// vector, decoded straight from little-endian bytes without allocating.
pub fn squared_stored(query: &[f32], stored: &[u8]) -> Option<f64> {
    if stored.len() != query.len() * 4 {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
        // SAFETY: loads are explicitly unaligned; no aligned f32 slice is
        // fabricated from record bytes. Dimensions were validated above.
        return Some(unsafe { squared_stored_avx2(query, stored) });
    }
    let mut acc = [0.0f64; 8];
    let body = query.len() - query.len() % 8;
    let mut i = 0;
    while i < body {
        for (lane, slot) in acc.iter_mut().enumerate() {
            let raw = stored[(i + lane) * 4..(i + lane) * 4 + 4]
                .try_into()
                .unwrap();
            let d = f64::from(query[i + lane]) - f64::from(f32::from_le_bytes(raw));
            *slot += d * d;
        }
        i += 8;
    }
    let mut sum = (acc[0] + acc[4]) + (acc[1] + acc[5]) + (acc[2] + acc[6]) + (acc[3] + acc[7]);
    while i < query.len() {
        let raw = stored[i * 4..i * 4 + 4].try_into().unwrap();
        let d = f64::from(query[i]) - f64::from(f32::from_le_bytes(raw));
        sum += d * d;
        i += 1;
    }
    Some(sum)
}

/// A fast `f32` prefilter for the bounded cached scan. The caller must
/// recompute the surviving candidates in `f64`; this value is only used to
/// discard rows that cannot enter the TopK.
pub fn squared_stored_f32(query: &[f32], stored: &[u8]) -> Option<f32> {
    if stored.len() != query.len() * 4 {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") {
        // SAFETY: the feature check above guards the call and the dimensions
        // were checked against the encoded byte length.
        return Some(unsafe { squared_stored_f32_avx2(query, stored) });
    }
    let mut acc = [0.0f32; 8];
    let body = query.len() - query.len() % 8;
    let mut i = 0;
    while i < body {
        for (lane, slot) in acc.iter_mut().enumerate() {
            let raw = stored[(i + lane) * 4..(i + lane) * 4 + 4]
                .try_into()
                .unwrap();
            let d = query[i + lane] - f32::from_le_bytes(raw);
            *slot += d * d;
        }
        i += 8;
    }
    let mut sum = (acc[0] + acc[4]) + (acc[1] + acc[5]) + (acc[2] + acc[6]) + (acc[3] + acc[7]);
    while i < query.len() {
        let raw = stored[i * 4..i * 4 + 4].try_into().unwrap();
        let d = query[i] - f32::from_le_bytes(raw);
        sum += d * d;
        i += 1;
    }
    Some(sum)
}

/// The portable version: eight independent `f64` accumulators keep the
/// additions from forming one long dependency chain.
fn squared_scalar(a: &[f32], b: &[f32]) -> f64 {
    let mut acc = [0.0f64; 8];
    let body = a.len() - a.len() % 8;
    let mut i = 0;
    while i < body {
        for (lane, slot) in acc.iter_mut().enumerate() {
            let d = f64::from(a[i + lane]) - f64::from(b[i + lane]);
            *slot += d * d;
        }
        i += 8;
    }
    let mut sum = (acc[0] + acc[4]) + (acc[1] + acc[5]) + (acc[2] + acc[6]) + (acc[3] + acc[7]);
    while i < a.len() {
        let d = f64::from(a[i]) - f64::from(b[i]);
        sum += d * d;
        i += 1;
    }
    sum
}

/// Eight `f32` lanes per load, widened to `f64` in two halves, accumulated with
/// two independent `f64` vectors so the FMA latencies overlap.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn squared_avx2(a: &[f32], b: &[f32]) -> f64 {
    use std::arch::x86_64::*;

    let n = a.len();
    let pa = a.as_ptr();
    let pb = b.as_ptr();
    let mut acc0 = _mm256_setzero_pd();
    let mut acc1 = _mm256_setzero_pd();
    let body = n - n % 8;
    let mut i = 0;
    while i < body {
        let va = _mm256_loadu_ps(pa.add(i));
        let vb = _mm256_loadu_ps(pb.add(i));
        let lo_a = _mm256_cvtps_pd(_mm256_castps256_ps128(va));
        let lo_b = _mm256_cvtps_pd(_mm256_castps256_ps128(vb));
        let hi_a = _mm256_cvtps_pd(_mm256_extractf128_ps(va, 1));
        let hi_b = _mm256_cvtps_pd(_mm256_extractf128_ps(vb, 1));
        let d0 = _mm256_sub_pd(lo_a, lo_b);
        let d1 = _mm256_sub_pd(hi_a, hi_b);
        acc0 = _mm256_fmadd_pd(d0, d0, acc0);
        acc1 = _mm256_fmadd_pd(d1, d1, acc1);
        i += 8;
    }
    let sum = _mm256_add_pd(acc0, acc1);
    let lo = _mm256_castpd256_pd128(sum);
    let hi = _mm256_extractf128_pd(sum, 1);
    let pair = _mm_add_pd(lo, hi);
    let mut total = _mm_cvtsd_f64(_mm_hadd_pd(pair, pair));
    while i < n {
        let d = f64::from(*pa.add(i)) - f64::from(*pb.add(i));
        total += d * d;
        i += 1;
    }
    total
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn squared_stored_avx2(a: &[f32], b: &[u8]) -> f64 {
    use std::arch::x86_64::*;

    let n = a.len();
    let pa = a.as_ptr();
    let pb = b.as_ptr().cast::<f32>();
    let mut acc0 = _mm256_setzero_pd();
    let mut acc1 = _mm256_setzero_pd();
    let body = n - n % 8;
    let mut i = 0;
    while i < body {
        let va = _mm256_loadu_ps(pa.add(i));
        let vb = _mm256_loadu_ps(pb.add(i));
        let lo_a = _mm256_cvtps_pd(_mm256_castps256_ps128(va));
        let lo_b = _mm256_cvtps_pd(_mm256_castps256_ps128(vb));
        let hi_a = _mm256_cvtps_pd(_mm256_extractf128_ps(va, 1));
        let hi_b = _mm256_cvtps_pd(_mm256_extractf128_ps(vb, 1));
        let d0 = _mm256_sub_pd(lo_a, lo_b);
        let d1 = _mm256_sub_pd(hi_a, hi_b);
        acc0 = _mm256_fmadd_pd(d0, d0, acc0);
        acc1 = _mm256_fmadd_pd(d1, d1, acc1);
        i += 8;
    }
    let sum = _mm256_add_pd(acc0, acc1);
    let lo = _mm256_castpd256_pd128(sum);
    let hi = _mm256_extractf128_pd(sum, 1);
    let pair = _mm_add_pd(lo, hi);
    let mut total = _mm_cvtsd_f64(_mm_hadd_pd(pair, pair));
    while i < n {
        let d = f64::from(*pa.add(i)) - f64::from(f32::from_le_bytes(b[i*4..i*4+4].try_into().unwrap()));
        total += d * d;
        i += 1;
    }
    total
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn squared_stored_f32_avx2(a: &[f32], b: &[u8]) -> f32 {
    use std::arch::x86_64::*;

    let n = a.len();
    let pa = a.as_ptr();
    let pb = b.as_ptr().cast::<f32>();
    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();
    let body = n - n % 16;
    let mut i = 0;
    while i < body {
        let d0 = _mm256_sub_ps(
            _mm256_loadu_ps(pa.add(i)),
            _mm256_loadu_ps(pb.add(i)),
        );
        let d1 = _mm256_sub_ps(
            _mm256_loadu_ps(pa.add(i + 8)),
            _mm256_loadu_ps(pb.add(i + 8)),
        );
        acc0 = _mm256_fmadd_ps(d0, d0, acc0);
        acc1 = _mm256_fmadd_ps(d1, d1, acc1);
        i += 16;
    }
    while i + 8 <= n {
        let d = _mm256_sub_ps(
            _mm256_loadu_ps(pa.add(i)),
            _mm256_loadu_ps(pb.add(i)),
        );
        acc0 = _mm256_fmadd_ps(d, d, acc0);
        i += 8;
    }
    let sum = _mm256_add_ps(acc0, acc1);
    let lo = _mm256_castps256_ps128(sum);
    let hi = _mm256_extractf128_ps(sum, 1);
    let pair = _mm_add_ps(lo, hi);
    let pair = _mm_hadd_ps(pair, pair);
    let pair = _mm_hadd_ps(pair, pair);
    let mut total = _mm_cvtss_f32(pair);
    while i < n {
        let d = *pa.add(i) - *pb.add(i);
        total += d * d;
        i += 1;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_simd_matches_f64_reference_on_unaligned_bytes_and_all_tails() {
        for dim in 1..=257 {
            let a: Vec<f32> = (0..dim).map(|i| ((i * 71 + dim * 19) % 997) as f32 / 17.0 - 23.0).collect();
            let b: Vec<f32> = (0..dim).map(|i| ((i * 31 + dim * 11) % 991) as f32 / 19.0 - 29.0).collect();
            let expected = squared_scalar(&a,&b);
            for offset in 0..8 {
                let mut bytes = vec![0;offset];
                for x in &b { bytes.extend_from_slice(&x.to_le_bytes()); }
                let actual = squared_stored(&a,&bytes[offset..]).unwrap();
                assert!((actual-expected).abs() <= expected.abs().max(1.0)*1e-12);
            }
        }
        let a = [f32::MAX;13];let b = [-f32::MAX;13];
        let bytes: Vec<u8> = b.iter().flat_map(|x|x.to_le_bytes()).collect();
        let actual = squared_stored(&a,&bytes).unwrap();
        assert!(actual.is_finite());
        assert!((actual-squared_scalar(&a,&b)).abs()/actual < 1e-12);
    }

    #[test]
    fn computes_known_distances() {
        assert_eq!(squared(&[0.0, 0.0], &[3.0, 4.0]), Some(25.0));
        assert_eq!(distance(&[0.0, 0.0], &[3.0, 4.0]), Some(5.0));
        assert_eq!(distance(&[1.0], &[1.0]), Some(0.0));
    }

    #[test]
    fn dimension_mismatch_is_none() {
        assert_eq!(distance(&[1.0, 2.0], &[1.0]), None);
    }

    #[test]
    fn identity_is_symmetric() {
        let a = [0.1f32, -2.5, 7.25, 0.0];
        let b = [1.5f32, 3.0, -0.5, 9.0];
        let ab = distance(&a, &b).unwrap();
        let ba = distance(&b, &a).unwrap();
        assert!((ab - ba).abs() < 1e-12);
    }

    #[test]
    fn matches_the_scalar_sum_for_every_tail_length() {
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        for n in 0..40 {
            let mut a = Vec::with_capacity(n);
            let mut b = Vec::with_capacity(n);
            for _ in 0..n {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                a.push((rng >> 40) as f32 / (1u64 << 24) as f32 * 4.0 - 2.0);
                b.push((rng >> 40) as f32 / (1u64 << 24) as f32 * 4.0 - 2.0);
            }
            let fast = squared(&a, &b).unwrap();
            let slow = squared_scalar(&a, &b);
            assert!(
                (fast - slow).abs() <= 1e-9 * slow.abs().max(1.0),
                "n = {n}: {fast} vs {slow}"
            );
        }
    }

    #[test]
    fn stored_vectors_match_in_memory_ones() {
        let a = [0.5f32, -1.0, 2.25, 0.0, 3.5];
        let b = [-0.25f32, 0.0, 1.0, 9.0, -3.5];
        let mut stored = Vec::new();
        for x in b {
            stored.extend_from_slice(&x.to_le_bytes());
        }
        assert_eq!(
            squared_stored(&a, &stored),
            Some(squared_scalar(&a, &b))
        );
        assert_eq!(squared_stored(&a, &stored[..4]), None);
    }

    #[test]
    fn f32_prefilter_stays_close_to_the_f64_reference() {
        for dim in 1..=257 {
            let a: Vec<f32> = (0..dim)
                .map(|i| ((i * 37 + dim * 11) % 401) as f32 / 13.0 - 15.0)
                .collect();
            let b: Vec<f32> = (0..dim)
                .map(|i| ((i * 53 + dim * 17) % 397) as f32 / 17.0 - 11.0)
                .collect();
            let bytes: Vec<u8> = b.iter().flat_map(|x| x.to_le_bytes()).collect();
            let exact = squared_stored(&a, &bytes).unwrap();
            let coarse = squared_stored_f32(&a, &bytes).unwrap();
            assert!(
                (f64::from(coarse) - exact).abs() <= exact.abs().max(1.0) * 1e-4,
                "dim = {dim}: {coarse} vs {exact}"
            );
        }
    }
}
