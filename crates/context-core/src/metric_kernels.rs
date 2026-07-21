//! Allocation-free dense distance kernels with architecture-specific dispatch.

#![allow(
    unsafe_code,
    reason = "NEON and AVX2 intrinsics are isolated here and operate only on bounds-checked slice chunks"
)]

pub(crate) fn dot(left: &[f32], right: &[f32]) -> f32 {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: AArch64 guarantees NEON; the kernel loads only complete
        // four-element chunks and handles the tail scalarly.
        unsafe { aarch64::dot(left, right) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if x86_64_impl::avx2_fma_available() {
            // SAFETY: the runtime check above proves AVX2 and FMA are present
            // on this CPU, and the kernel loads only complete chunks with a
            // scalar tail.
            unsafe { x86_64_impl::dot(left, right) }
        } else {
            scalar::dot(left, right)
        }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    scalar::dot(left, right)
}

pub(crate) fn l2(left: &[f32], right: &[f32]) -> f32 {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: both input slices passed the metric dimension check and each
        // vector load is bounded by the complete-chunk prefix.
        unsafe { aarch64::l2(left, right) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if x86_64_impl::avx2_fma_available() {
            // SAFETY: the runtime check above proves AVX2 and FMA are present
            // on this CPU, and the kernel loads only complete chunks with a
            // scalar tail.
            unsafe { x86_64_impl::l2(left, right) }
        } else {
            scalar::l2(left, right)
        }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    scalar::l2(left, right)
}

pub(crate) fn l1(left: &[f32], right: &[f32]) -> f32 {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: see [`l2`].
        unsafe { aarch64::l1(left, right) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if x86_64_impl::avx2_fma_available() {
            // SAFETY: the runtime check above proves AVX2 and FMA are present
            // on this CPU, and the kernel loads only complete chunks with a
            // scalar tail.
            unsafe { x86_64_impl::l1(left, right) }
        } else {
            scalar::l1(left, right)
        }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    scalar::l1(left, right)
}

pub(crate) fn dot_and_norms(left: &[f32], right: &[f32]) -> (f32, f32, f32) {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: see [`l2`]; all accumulators share the same bounded loads.
        unsafe { aarch64::dot_and_norms(left, right) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if x86_64_impl::avx2_fma_available() {
            // SAFETY: the runtime check above proves AVX2 and FMA are present
            // on this CPU, and the kernel loads only complete chunks with a
            // scalar tail.
            unsafe { x86_64_impl::dot_and_norms(left, right) }
        } else {
            scalar::dot_and_norms(left, right)
        }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    scalar::dot_and_norms(left, right)
}

#[cfg(target_arch = "aarch64")]
mod aarch64 {
    use core::arch::aarch64::{
        vabsq_f32, vaddq_f32, vaddvq_f32, vdupq_n_f32, vfmaq_f32, vld1q_f32, vsubq_f32,
    };

    #[allow(
        clippy::needless_range_loop,
        reason = "the lane index addresses three independent arrays (offset math, sums) that an iterator adapter cannot express as cleanly"
    )]
    pub(super) unsafe fn dot(left: &[f32], right: &[f32]) -> f32 {
        // Four independent accumulators hide FMA dependency latency on modern
        // AArch64 cores while keeping the hot loop branch-light.
        // SAFETY: AArch64 guarantees NEON register initialization.
        let mut sums = [unsafe { vdupq_n_f32(0.0) }; 4];
        let complete = left.len() / 16 * 16;
        let mut offset = 0;
        while offset < complete {
            for lane in 0..4 {
                let lane_offset = offset + lane * 4;
                // SAFETY: `offset < complete` proves every 16-wide group and
                // therefore each four-lane load lies in both slices.
                let a = unsafe { vld1q_f32(left.as_ptr().add(lane_offset)) };
                let b = unsafe { vld1q_f32(right.as_ptr().add(lane_offset)) };
                // SAFETY: AArch64 guarantees NEON FMA availability.
                sums[lane] = unsafe { vfmaq_f32(sums[lane], a, b) };
            }
            offset += 16;
        }
        // SAFETY: All values are initialized NEON accumulators.
        (unsafe { reduce_four(sums) }) + scalar_dot_tail(left, right, complete)
    }

    pub(super) unsafe fn l2(left: &[f32], right: &[f32]) -> f32 {
        // SAFETY: AArch64 guarantees the NEON register operation is available.
        let mut sum = unsafe { vdupq_n_f32(0.0) };
        let complete = left.len() / 4 * 4;
        let mut offset = 0;
        while offset < complete {
            // SAFETY: `offset < complete` proves both four-lane loads fit.
            let a = unsafe { vld1q_f32(left.as_ptr().add(offset)) };
            let b = unsafe { vld1q_f32(right.as_ptr().add(offset)) };
            // SAFETY: AArch64 guarantees the NEON register operation is available.
            let delta = unsafe { vsubq_f32(a, b) };
            // SAFETY: AArch64 guarantees the NEON register operation is available.
            sum = unsafe { vfmaq_f32(sum, delta, delta) };
            offset += 4;
        }
        let tail = left[complete..]
            .iter()
            .zip(&right[complete..])
            .map(|(a, b)| {
                let delta = a - b;
                delta * delta
            })
            .sum::<f32>();
        // SAFETY: AArch64 guarantees the NEON register operation is available.
        (unsafe { vaddvq_f32(sum) } + tail).sqrt()
    }

    pub(super) unsafe fn l1(left: &[f32], right: &[f32]) -> f32 {
        // SAFETY: AArch64 guarantees the NEON register operation is available.
        let mut sum = unsafe { vdupq_n_f32(0.0) };
        let complete = left.len() / 4 * 4;
        let mut offset = 0;
        while offset < complete {
            // SAFETY: `offset < complete` proves both four-lane loads fit.
            let a = unsafe { vld1q_f32(left.as_ptr().add(offset)) };
            let b = unsafe { vld1q_f32(right.as_ptr().add(offset)) };
            // SAFETY: AArch64 guarantees the NEON register operation is available.
            let delta = unsafe { vsubq_f32(a, b) };
            // SAFETY: AArch64 guarantees both NEON register operations are available.
            sum = unsafe { vaddq_f32(sum, vabsq_f32(delta)) };
            offset += 4;
        }
        // SAFETY: AArch64 guarantees the NEON register operation is available.
        (unsafe { vaddvq_f32(sum) })
            + left[complete..]
                .iter()
                .zip(&right[complete..])
                .map(|(a, b)| (a - b).abs())
                .sum::<f32>()
    }

    #[allow(
        clippy::needless_range_loop,
        reason = "the lane index addresses three independent accumulator arrays that an iterator adapter cannot express as cleanly"
    )]
    pub(super) unsafe fn dot_and_norms(left: &[f32], right: &[f32]) -> (f32, f32, f32) {
        // SAFETY: AArch64 guarantees NEON register initialization.
        let mut products = [unsafe { vdupq_n_f32(0.0) }; 4];
        // SAFETY: AArch64 guarantees NEON register initialization.
        let mut left_norms = [unsafe { vdupq_n_f32(0.0) }; 4];
        // SAFETY: AArch64 guarantees NEON register initialization.
        let mut right_norms = [unsafe { vdupq_n_f32(0.0) }; 4];
        let complete = left.len() / 16 * 16;
        let mut offset = 0;
        while offset < complete {
            for lane in 0..4 {
                let lane_offset = offset + lane * 4;
                // SAFETY: the complete 16-wide group is inside both slices.
                let a = unsafe { vld1q_f32(left.as_ptr().add(lane_offset)) };
                let b = unsafe { vld1q_f32(right.as_ptr().add(lane_offset)) };
                // SAFETY: AArch64 guarantees NEON FMA availability.
                products[lane] = unsafe { vfmaq_f32(products[lane], a, b) };
                // SAFETY: AArch64 guarantees NEON FMA availability.
                left_norms[lane] = unsafe { vfmaq_f32(left_norms[lane], a, a) };
                // SAFETY: AArch64 guarantees NEON FMA availability.
                right_norms[lane] = unsafe { vfmaq_f32(right_norms[lane], b, b) };
            }
            offset += 16;
        }
        left[complete..].iter().zip(&right[complete..]).fold(
            (
                // SAFETY: all accumulator lanes are initialized.
                unsafe { reduce_four(products) },
                // SAFETY: all accumulator lanes are initialized.
                unsafe { reduce_four(left_norms) },
                // SAFETY: all accumulator lanes are initialized.
                unsafe { reduce_four(right_norms) },
            ),
            |(product, left_norm, right_norm), (a, b)| {
                (product + a * b, left_norm + a * a, right_norm + b * b)
            },
        )
    }

    unsafe fn reduce_four(values: [core::arch::aarch64::float32x4_t; 4]) -> f32 {
        // SAFETY: AArch64 guarantees these NEON additions and horizontal sum.
        let first = unsafe { vaddq_f32(values[0], values[1]) };
        // SAFETY: same initialized-register contract as above.
        let second = unsafe { vaddq_f32(values[2], values[3]) };
        // SAFETY: same initialized-register contract as above.
        unsafe { vaddvq_f32(vaddq_f32(first, second)) }
    }

    fn scalar_dot_tail(left: &[f32], right: &[f32], offset: usize) -> f32 {
        left[offset..]
            .iter()
            .zip(&right[offset..])
            .map(|(a, b)| a * b)
            .sum()
    }
}

#[cfg(any(test, not(target_arch = "aarch64")))]
mod scalar {
    pub(super) fn dot(left: &[f32], right: &[f32]) -> f32 {
        left.iter().zip(right).map(|(a, b)| a * b).sum()
    }

    pub(super) fn l2(left: &[f32], right: &[f32]) -> f32 {
        left.iter()
            .zip(right)
            .map(|(a, b)| {
                let delta = a - b;
                delta * delta
            })
            .sum::<f32>()
            .sqrt()
    }

    pub(super) fn l1(left: &[f32], right: &[f32]) -> f32 {
        left.iter().zip(right).map(|(a, b)| (a - b).abs()).sum()
    }

    pub(super) fn dot_and_norms(left: &[f32], right: &[f32]) -> (f32, f32, f32) {
        left.iter().zip(right).fold(
            (0.0, 0.0, 0.0),
            |(product, left_norm, right_norm), (a, b)| {
                (product + a * b, left_norm + a * a, right_norm + b * b)
            },
        )
    }
}

#[cfg(target_arch = "x86_64")]
mod x86_64_impl {
    //! AVX2+FMA mirrors of the NEON kernels above: same chunk structure, same
    //! accumulator scheme, eight lanes per register instead of four.
    //!
    //! x86-64's baseline does not include AVX2, so unlike the NEON module the
    //! callers gate every entry on [`avx2_fma_available`] at runtime. AVX-512
    //! variants are deliberately absent: nothing on the Apple-silicon
    //! development machine can execute them (Rosetta 2 and QEMU TCG both stop
    //! at AVX2), and an unexecutable kernel cannot be honestly verified.

    use core::arch::x86_64::{
        __m256, _mm_add_ps, _mm_add_ss, _mm_cvtss_f32, _mm_movehl_ps, _mm_shuffle_ps,
        _mm256_add_ps, _mm256_andnot_ps, _mm256_castps256_ps128, _mm256_extractf128_ps,
        _mm256_fmadd_ps, _mm256_loadu_ps, _mm256_set1_ps, _mm256_setzero_ps, _mm256_sub_ps,
    };

    /// Both features are required together: every kernel below uses 256-bit
    /// FMA, and detecting them once here keeps the dispatch sites uniform.
    pub(super) fn avx2_fma_available() -> bool {
        std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
    }

    #[allow(
        clippy::needless_range_loop,
        reason = "the lane index addresses independent accumulator arrays that an iterator adapter cannot express as cleanly"
    )]
    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn dot(left: &[f32], right: &[f32]) -> f32 {
        // Four independent accumulators hide FMA dependency latency, exactly
        // as in the NEON kernel; at eight lanes each the hot loop consumes
        // thirty-two elements per iteration.
        let mut sums = [_mm256_setzero_ps(); 4];
        let complete = left.len() / 32 * 32;
        let mut offset = 0;
        while offset < complete {
            for lane in 0..4 {
                let lane_offset = offset + lane * 8;
                // SAFETY: `offset < complete` proves every 32-wide group and
                // therefore each eight-lane unaligned load lies in both slices.
                let a = unsafe { _mm256_loadu_ps(left.as_ptr().add(lane_offset)) };
                let b = unsafe { _mm256_loadu_ps(right.as_ptr().add(lane_offset)) };
                sums[lane] = _mm256_fmadd_ps(a, b, sums[lane]);
            }
            offset += 32;
        }
        reduce_four(sums)
            + left[complete..]
                .iter()
                .zip(&right[complete..])
                .map(|(a, b)| a * b)
                .sum::<f32>()
    }

    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn l2(left: &[f32], right: &[f32]) -> f32 {
        let mut sum = _mm256_setzero_ps();
        let complete = left.len() / 8 * 8;
        let mut offset = 0;
        while offset < complete {
            // SAFETY: `offset < complete` proves both eight-lane loads fit.
            let a = unsafe { _mm256_loadu_ps(left.as_ptr().add(offset)) };
            let b = unsafe { _mm256_loadu_ps(right.as_ptr().add(offset)) };
            let delta = _mm256_sub_ps(a, b);
            sum = _mm256_fmadd_ps(delta, delta, sum);
            offset += 8;
        }
        let tail = left[complete..]
            .iter()
            .zip(&right[complete..])
            .map(|(a, b)| {
                let delta = a - b;
                delta * delta
            })
            .sum::<f32>();
        (hsum256(sum) + tail).sqrt()
    }

    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn l1(left: &[f32], right: &[f32]) -> f32 {
        let mut sum = _mm256_setzero_ps();
        // Clearing the sign bit is `abs` for IEEE floats; andnot with -0.0
        // masks exactly that bit, the same trick NEON's vabsq performs.
        let sign_mask = _mm256_set1_ps(-0.0);
        let complete = left.len() / 8 * 8;
        let mut offset = 0;
        while offset < complete {
            // SAFETY: `offset < complete` proves both eight-lane loads fit.
            let a = unsafe { _mm256_loadu_ps(left.as_ptr().add(offset)) };
            let b = unsafe { _mm256_loadu_ps(right.as_ptr().add(offset)) };
            let delta = _mm256_sub_ps(a, b);
            sum = _mm256_add_ps(sum, _mm256_andnot_ps(sign_mask, delta));
            offset += 8;
        }
        hsum256(sum)
            + left[complete..]
                .iter()
                .zip(&right[complete..])
                .map(|(a, b)| (a - b).abs())
                .sum::<f32>()
    }

    #[allow(
        clippy::needless_range_loop,
        reason = "the lane index addresses three independent accumulator arrays that an iterator adapter cannot express as cleanly"
    )]
    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn dot_and_norms(left: &[f32], right: &[f32]) -> (f32, f32, f32) {
        let mut products = [_mm256_setzero_ps(); 4];
        let mut left_norms = [_mm256_setzero_ps(); 4];
        let mut right_norms = [_mm256_setzero_ps(); 4];
        let complete = left.len() / 32 * 32;
        let mut offset = 0;
        while offset < complete {
            for lane in 0..4 {
                let lane_offset = offset + lane * 8;
                // SAFETY: the complete 32-wide group is inside both slices.
                let a = unsafe { _mm256_loadu_ps(left.as_ptr().add(lane_offset)) };
                let b = unsafe { _mm256_loadu_ps(right.as_ptr().add(lane_offset)) };
                products[lane] = _mm256_fmadd_ps(a, b, products[lane]);
                left_norms[lane] = _mm256_fmadd_ps(a, a, left_norms[lane]);
                right_norms[lane] = _mm256_fmadd_ps(b, b, right_norms[lane]);
            }
            offset += 32;
        }
        left[complete..].iter().zip(&right[complete..]).fold(
            (
                reduce_four(products),
                reduce_four(left_norms),
                reduce_four(right_norms),
            ),
            |(product, left_norm, right_norm), (a, b)| {
                (product + a * b, left_norm + a * a, right_norm + b * b)
            },
        )
    }

    // Safe `#[target_feature]` functions: value-only intrinsics carry no
    // memory-safety obligation once the features are enabled, and the unsafe
    // kernels above already hold them, so these calls need no unsafe blocks.
    #[target_feature(enable = "avx2,fma")]
    fn reduce_four(values: [__m256; 4]) -> f32 {
        let first = _mm256_add_ps(values[0], values[1]);
        let second = _mm256_add_ps(values[2], values[3]);
        hsum256(_mm256_add_ps(first, second))
    }

    /// The canonical 256-bit horizontal sum: fold the high half onto the
    /// low, then the standard 128-bit movehl/shuffle reduction (shuffle
    /// mask 1 selects lane one into lane zero).
    #[target_feature(enable = "avx2,fma")]
    fn hsum256(value: __m256) -> f32 {
        let low = _mm256_castps256_ps128(value);
        let high = _mm256_extractf128_ps::<1>(value);
        let quad = _mm_add_ps(low, high);
        let pair = _mm_add_ps(quad, _mm_movehl_ps(quad, quad));
        let single = _mm_add_ss(pair, _mm_shuffle_ps::<1>(pair, pair));
        _mm_cvtss_f32(single)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    /// Reassociation tolerance: SIMD sums in a different order than scalar,
    /// so equality is asserted against the magnitude of the accumulated
    /// terms, not the (possibly cancelled-to-zero) result.
    fn close(simd: f32, scalar: f32, term_scale: f32) -> bool {
        (simd - scalar).abs() <= 1e-4 + term_scale * 1e-5
    }

    fn dot_term_scale(left: &[f32], right: &[f32]) -> f32 {
        left.iter().zip(right).map(|(a, b)| (a * b).abs()).sum()
    }

    /// Dimensions straddle every chunk boundary the kernels use: the 8-lane
    /// register width, the 32-wide four-accumulator groups, and the engine's
    /// common 384.
    fn vector_pairs() -> impl Strategy<Value = (Vec<f32>, Vec<f32>)> {
        prop::sample::select(vec![
            0_usize, 1, 2, 3, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 383, 384, 385,
        ])
        .prop_flat_map(|dimension| {
            (
                prop::collection::vec(-100.0_f32..100.0, dimension),
                prop::collection::vec(-100.0_f32..100.0, dimension),
            )
        })
    }

    proptest! {
        /// The public dispatch must agree with the scalar oracle whatever
        /// path it takes: NEON on aarch64, AVX2 or scalar on x86-64.
        #[test]
        fn dispatch_matches_the_scalar_oracle((left, right) in vector_pairs()) {
            let scale = dot_term_scale(&left, &right);
            prop_assert!(close(super::dot(&left, &right), super::scalar::dot(&left, &right), scale));
            let l2_oracle = super::scalar::l2(&left, &right);
            prop_assert!(close(super::l2(&left, &right), l2_oracle, l2_oracle));
            let l1_oracle = super::scalar::l1(&left, &right);
            prop_assert!(close(super::l1(&left, &right), l1_oracle, l1_oracle));
            let (product, left_norm, right_norm) = super::dot_and_norms(&left, &right);
            let (product_oracle, left_oracle, right_oracle) =
                super::scalar::dot_and_norms(&left, &right);
            prop_assert!(close(product, product_oracle, scale));
            prop_assert!(close(left_norm, left_oracle, left_oracle));
            prop_assert!(close(right_norm, right_oracle, right_oracle));
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[allow(
        clippy::print_stderr,
        reason = "the skip notice is how a test log records that this runner could not exercise the SIMD path"
    )]
    mod avx2_parity {
        use super::*;
        proptest! {
        /// The AVX2 kernels directly against the oracle, independent of what
        /// the dispatch chose. Skips (loudly) where the CPU lacks the
        /// features; the mutation check in the commit run proves this test
        /// exercised the SIMD path for real.
        #[test]
        fn avx2_kernels_match_the_scalar_oracle((left, right) in vector_pairs()) {
            if !super::super::x86_64_impl::avx2_fma_available() {
                eprintln!("skipped: avx2+fma not available on this CPU");
                return Ok(());
            }
            let scale = dot_term_scale(&left, &right);
            // SAFETY: availability was checked immediately above.
            let simd_dot = unsafe { super::super::x86_64_impl::dot(&left, &right) };
            prop_assert!(close(simd_dot, super::super::scalar::dot(&left, &right), scale));
            // SAFETY: availability was checked immediately above.
            let simd_l2 = unsafe { super::super::x86_64_impl::l2(&left, &right) };
            let l2_oracle = super::super::scalar::l2(&left, &right);
            prop_assert!(close(simd_l2, l2_oracle, l2_oracle));
            // SAFETY: availability was checked immediately above.
            let simd_l1 = unsafe { super::super::x86_64_impl::l1(&left, &right) };
            let l1_oracle = super::super::scalar::l1(&left, &right);
            prop_assert!(close(simd_l1, l1_oracle, l1_oracle));
            // SAFETY: availability was checked immediately above.
            let (product, left_norm, right_norm) =
                unsafe { super::super::x86_64_impl::dot_and_norms(&left, &right) };
            let (product_oracle, left_oracle, right_oracle) =
                super::super::scalar::dot_and_norms(&left, &right);
            prop_assert!(close(product, product_oracle, scale));
            prop_assert!(close(left_norm, left_oracle, left_oracle));
            prop_assert!(close(right_norm, right_oracle, right_oracle));
        }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    #[allow(
        clippy::print_stderr,
        reason = "the printed line is how a test log records which dispatch path this runner verified"
    )]
    fn report_x86_dispatch_path() {
        // Not an assertion: which path runs is machine-dependent. The line in
        // the test output is how a runner records which path it verified.
        eprintln!(
            "x86-64 dispatch path: {}",
            if super::x86_64_impl::avx2_fma_available() {
                "avx2+fma"
            } else {
                "scalar"
            }
        );
    }

    #[test]
    fn edge_values_match_the_scalar_oracle() {
        let cases: Vec<(Vec<f32>, Vec<f32>)> = vec![
            (vec![0.0; 33], vec![0.0; 33]),
            (vec![-1.5; 40], vec![-2.5; 40]),
            // Exact cancellation: the dot sum is ~0 while its terms are not,
            // which is the case a result-relative tolerance would misjudge.
            (
                (0..64)
                    .map(|i| if i % 2 == 0 { 3.0 } else { -3.0 })
                    .collect(),
                vec![3.0; 64],
            ),
            // Subnormals: products underflow identically on both paths.
            (vec![1.0e-40; 17], vec![1.0e-40; 17]),
        ];
        for (left, right) in cases {
            let scale = dot_term_scale(&left, &right);
            assert!(close(
                super::dot(&left, &right),
                super::scalar::dot(&left, &right),
                scale
            ));
            let l2_oracle = super::scalar::l2(&left, &right);
            assert!(close(super::l2(&left, &right), l2_oracle, l2_oracle));
            let l1_oracle = super::scalar::l1(&left, &right);
            assert!(close(super::l1(&left, &right), l1_oracle, l1_oracle));
        }
    }
}
