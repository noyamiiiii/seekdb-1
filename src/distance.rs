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

/// Scores packed 7-bit vectors with the same affine quantization on both
/// sides. `data` is row-major and `out` receives the integer sum of squared
/// code differences; multiplying by `step * step` gives the approximate
/// squared distance. Callers use the result as a conservative prefilter.
pub fn quantized_scores(
    query: &[u8],
    data: &[u8],
    dimensions: usize,
    rows: usize,
    out: &mut [u32],
) -> bool {
    if query.len() != dimensions
        || data.len() != dimensions.saturating_mul(rows)
        || out.len() != rows
    {
        return false;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: the length checks above establish every load and store bound.
        unsafe { quantized_scores_avx2(query, data, dimensions, rows, out) };
        return true;
    }
    for (row, score) in out.iter_mut().enumerate() {
        let mut total = 0u32;
        for dimension in 0..dimensions {
            let difference =
                i32::from(query[dimension]) - i32::from(data[row * dimensions + dimension]);
            total += (difference * difference) as u32;
        }
        *score = total;
    }
    true
}

/// Scores one packed 7-bit vector. Filtered TopK visits scattered rows, so it
/// needs the same integer kernel one row at a time instead of a full sweep.
pub fn quantized_score_row(query: &[u8], row: &[u8]) -> Option<u32> {
    if query.len() != row.len() || query.is_empty() {
        return None;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        // SAFETY: the length check above covers every load below.
        return Some(unsafe { quantized_score_row_avx2(query, row) });
    }
    let mut total = 0u32;
    for (&query_code, &row_code) in query.iter().zip(row) {
        let difference = i32::from(query_code) - i32::from(row_code);
        total += (difference * difference) as u32;
    }
    Some(total)
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

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn quantized_scores_avx2(
    query: &[u8],
    data: &[u8],
    dimensions: usize,
    rows: usize,
    out: &mut [u32],
) {
    use std::arch::x86_64::*;

    let ones = _mm256_set1_epi16(1);
    // Table vectors are scanned back to back, so the query blocks stay in
    // registers for the whole sweep; 128 components is the evaluated shape and
    // needs exactly four.
    let blocks = dimensions / 32;
    let resident = blocks.min(4);
    let mut query_blocks = [_mm256_setzero_si256(); 4];
    for (block, slot) in query_blocks.iter_mut().enumerate().take(resident) {
        *slot = _mm256_loadu_si256(query.as_ptr().add(block * 32).cast());
    }
    for row in 0..rows {
        let base = data.as_ptr().add(row * dimensions);
        // Two independent chains keep the multiply-add latency off the
        // critical path of a single accumulator.
        let mut acc0 = _mm256_setzero_si256();
        let mut acc1 = _mm256_setzero_si256();
        let mut block = 0usize;
        while block + 2 <= resident {
            acc0 = _mm256_add_epi32(
                acc0,
                packed_block_squares(base.add(block * 32), query_blocks[block], ones),
            );
            acc1 = _mm256_add_epi32(
                acc1,
                packed_block_squares(base.add((block + 1) * 32), query_blocks[block + 1], ones),
            );
            block += 2;
        }
        if block < resident {
            acc0 = _mm256_add_epi32(
                acc0,
                packed_block_squares(base.add(block * 32), query_blocks[block], ones),
            );
            block += 1;
        }
        while block < blocks {
            let q = _mm256_loadu_si256(query.as_ptr().add(block * 32).cast());
            acc0 = _mm256_add_epi32(acc0, packed_block_squares(base.add(block * 32), q, ones));
            block += 1;
        }
        let mut sum = horizontal_sum(_mm256_add_epi32(acc0, acc1));
        let mut dimension = blocks * 32;
        while dimension < dimensions {
            let difference = i32::from(*query.get_unchecked(dimension))
                - i32::from(*data.get_unchecked(row * dimensions + dimension));
            sum += difference * difference;
            dimension += 1;
        }
        *out.get_unchecked_mut(row) = sum as u32;
    }
}

#[cfg(target_arch = "x86_64")]
type V256 = std::arch::x86_64::__m256i;

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn quantized_score_row_avx2(query: &[u8], row: &[u8]) -> u32 {
    use std::arch::x86_64::*;

    let dimensions = query.len();
    let ones = _mm256_set1_epi16(1);
    let blocks = dimensions / 32;
    let mut acc0 = _mm256_setzero_si256();
    let mut acc1 = _mm256_setzero_si256();
    let mut block = 0usize;
    while block + 2 <= blocks {
        let q0 = _mm256_loadu_si256(query.as_ptr().add(block * 32).cast());
        let q1 = _mm256_loadu_si256(query.as_ptr().add((block + 1) * 32).cast());
        acc0 = _mm256_add_epi32(
            acc0,
            packed_block_squares(row.as_ptr().add(block * 32), q0, ones),
        );
        acc1 = _mm256_add_epi32(
            acc1,
            packed_block_squares(row.as_ptr().add((block + 1) * 32), q1, ones),
        );
        block += 2;
    }
    if block < blocks {
        let q = _mm256_loadu_si256(query.as_ptr().add(block * 32).cast());
        acc0 = _mm256_add_epi32(acc0, packed_block_squares(row.as_ptr().add(block * 32), q, ones));
    }
    let mut sum = horizontal_sum(_mm256_add_epi32(acc0, acc1));
    let mut dimension = blocks * 32;
    while dimension < dimensions {
        let difference = i32::from(*query.get_unchecked(dimension))
            - i32::from(*row.get_unchecked(dimension));
        sum += difference * difference;
        dimension += 1;
    }
    sum as u32
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn packed_block_squares(data: *const u8, query: V256, ones: V256) -> V256 {
    use std::arch::x86_64::*;

    let x = _mm256_loadu_si256(data.cast());
    // Both operands are in 0..=127, so the unsigned/signed byte products and
    // their 16-bit pair sums stay inside their ranges.
    let difference = _mm256_or_si256(_mm256_subs_epu8(x, query), _mm256_subs_epu8(query, x));
    _mm256_madd_epi16(_mm256_maddubs_epi16(difference, difference), ones)
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
unsafe fn horizontal_sum(value: V256) -> i32 {
    use std::arch::x86_64::*;

    let pairs = _mm256_hadd_epi32(value, value);
    let quads = _mm256_hadd_epi32(pairs, pairs);
    _mm_cvtsi128_si32(_mm_add_epi32(
        _mm256_castsi256_si128(quads),
        _mm256_extracti128_si256(quads, 1),
    ))
}

/// Folds the component range of one stored little-endian `f32` vector into
/// `low`/`high`. Used once per table to choose the affine packing.
pub fn stored_range(vector: &[u8], low: &mut f32, high: &mut f32) {
    let mut row_low = f32::INFINITY;
    let mut row_high = f32::NEG_INFINITY;
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: the feature check guards the call; loads stay inside the
            // vector because the loop is bounded by its component count.
            unsafe { stored_range_avx2(vector, &mut row_low, &mut row_high) };
            *low = low.min(row_low);
            *high = high.max(row_high);
            return;
        }
    }
    for chunk in vector.chunks_exact(4) {
        let value = f32::from_le_bytes(chunk.try_into().unwrap());
        row_low = row_low.min(value);
        row_high = row_high.max(value);
    }
    *low = low.min(row_low);
    *high = high.max(row_high);
}

/// Writes one 7-bit affine code per stored component; `out` must match the
/// vector's component count.
pub fn pack_stored_vector(vector: &[u8], minimum: f32, inverse_step: f32, out: &mut [u8]) {
    debug_assert_eq!(vector.len(), out.len() * 4);
    #[cfg(target_arch = "x86_64")]
    {
        if std::is_x86_feature_detected!("avx2") {
            // SAFETY: the caller contract above bounds both slices.
            unsafe { pack_stored_vector_avx2(vector, minimum, inverse_step, out) };
            return;
        }
    }
    for (slot, chunk) in out.iter_mut().zip(vector.chunks_exact(4)) {
        let value = f32::from_le_bytes(chunk.try_into().unwrap());
        *slot = ((value - minimum) * inverse_step).round().clamp(0.0, 127.0) as u8;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn stored_range_avx2(vector: &[u8], low: &mut f32, high: &mut f32) {
    use std::arch::x86_64::*;

    let components = vector.len() / 4;
    let base = vector.as_ptr().cast::<f32>();
    let mut low_lanes = _mm256_set1_ps(f32::INFINITY);
    let mut high_lanes = _mm256_set1_ps(f32::NEG_INFINITY);
    let body = components - components % 8;
    let mut index = 0usize;
    while index < body {
        let values = _mm256_loadu_ps(base.add(index));
        low_lanes = _mm256_min_ps(low_lanes, values);
        high_lanes = _mm256_max_ps(high_lanes, values);
        index += 8;
    }
    let mut lanes = [0f32; 8];
    _mm256_storeu_ps(lanes.as_mut_ptr(), low_lanes);
    for lane in lanes {
        // Propagate NaN so a corrupt component disables the packing instead of
        // silently shrinking the range.
        if lane.is_nan() {
            *low = f32::NAN;
        } else {
            *low = low.min(lane);
        }
    }
    _mm256_storeu_ps(lanes.as_mut_ptr(), high_lanes);
    for lane in lanes {
        if lane.is_nan() {
            *high = f32::NAN;
        } else {
            *high = high.max(lane);
        }
    }
    while index < components {
        let value = *base.add(index);
        if value.is_nan() {
            *low = f32::NAN;
            *high = f32::NAN;
        } else {
            *low = low.min(value);
            *high = high.max(value);
        }
        index += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn pack_stored_vector_avx2(
    vector: &[u8],
    minimum: f32,
    inverse_step: f32,
    out: &mut [u8],
) {
    use std::arch::x86_64::*;

    let components = out.len();
    let base = vector.as_ptr().cast::<f32>();
    let low_lane = _mm256_set1_ps(minimum);
    let step_lane = _mm256_set1_ps(inverse_step);
    let zero = _mm256_setzero_ps();
    let top = _mm256_set1_ps(127.0);
    let mut index = 0usize;
    while index + 8 <= components {
        let values = _mm256_loadu_ps(base.add(index));
        let scaled = _mm256_mul_ps(_mm256_sub_ps(values, low_lane), step_lane);
        let rounded = _mm256_round_ps(scaled, _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC);
        let clamped = _mm256_min_ps(_mm256_max_ps(rounded, zero), top);
        let wide = _mm256_cvttps_epi32(clamped);
        let mut lanes = [0i32; 8];
        _mm256_storeu_si256(lanes.as_mut_ptr().cast(), wide);
        for (offset, lane) in lanes.iter().enumerate() {
            // Values are clamped to 0..=127, so the cast is exact.
            *out.get_unchecked_mut(index + offset) = *lane as u8;
        }
        index += 8;
    }
    while index < components {
        let value = *base.add(index);
        out[index] = ((value - minimum) * inverse_step).round().clamp(0.0, 127.0) as u8;
        index += 1;
    }
}

#[cfg(test)]
fn pack_reference(values: &[f32], minimum: f32, inverse_step: f32) -> Vec<u8> {
    values
        .iter()
        .map(|value| ((value - minimum) * inverse_step).round().clamp(0.0, 127.0) as u8)
        .collect()
}

#[cfg(test)]
fn encode(values: &[f32]) -> Vec<u8> {
    values.iter().flat_map(|value| value.to_le_bytes()).collect()
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

    #[test]
    fn quantized_scan_matches_a_scalar_score() {
        for dimensions in [1, 7, 31, 32, 33, 127, 128] {
            for rows in [1, 3, 17] {
                let query: Vec<u8> = (0..dimensions)
                    .map(|i| ((i * 41 + rows) % 128) as u8)
                    .collect();
                let data: Vec<u8> = (0..rows)
                    .flat_map(|row| {
                        (0..dimensions)
                            .map(move |dimension| ((row * 23 + dimension * 7) % 128) as u8)
                    })
                    .collect();
                let mut got = vec![0u32; rows];
                assert!(quantized_scores(
                    &query,
                    &data,
                    dimensions,
                    rows,
                    &mut got
                ));
                for (row, value) in got.iter().enumerate() {
                    let expected = (0..dimensions)
                        .map(|dimension| {
                            let difference = i32::from(query[dimension])
                                - i32::from(data[row * dimensions + dimension]);
                            difference * difference
                        })
                        .sum::<i32>() as u32;
                    assert_eq!(*value, expected);
                }
            }
        }
    }

    #[test]
    fn packed_rows_match_a_scalar_reference() {
        for dimensions in [1, 7, 8, 15, 31, 32, 33, 127, 128, 129] {
            let values: Vec<f32> = (0..dimensions)
                .map(|index| ((index * 37) % 53) as f32 * 0.25 - 4.0)
                .collect();
            let stored = encode(&values);
            let mut low = f32::INFINITY;
            let mut high = f32::NEG_INFINITY;
            stored_range(&stored, &mut low, &mut high);
            let expected_low = values.iter().copied().fold(f32::INFINITY, f32::min);
            let expected_high = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            assert_eq!(low, expected_low);
            assert_eq!(high, expected_high);
            let step = (high - low) / 127.0;
            let mut packed = vec![0u8; dimensions];
            let inverse_step = 1.0 / step;
            pack_stored_vector(&stored, low, inverse_step, &mut packed);
            let expected = pack_reference(&values, low, inverse_step);
            let mismatched = (0..dimensions)
                .filter(|index| packed[*index] != expected[*index])
                .count();
            assert_eq!(mismatched, 0, "{dimensions} dims: {packed:?} vs {expected:?}");
        }
    }
}
