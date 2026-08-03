//! Exact integer metric accumulators with runtime architecture dispatch.

#![allow(
    unsafe_code,
    reason = "NEON and AVX2 intrinsics are isolated here and load only complete bounds-checked chunks"
)]

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IntegerAccumulators {
    pub(crate) squared_difference: i64,
    pub(crate) dot: i64,
    pub(crate) left_norm: i64,
    pub(crate) right_norm: i64,
    pub(crate) l1: i64,
}

pub(crate) fn signed(left: &[i8], right: &[i8]) -> IntegerAccumulators {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: AArch64 guarantees NEON and the implementation loads only
        // complete eight-byte chunks before a scalar tail.
        unsafe { aarch64::signed(left, right) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: runtime detection proves AVX2 and the implementation
            // loads only complete 16-byte chunks.
            unsafe { x86_64::signed(left, right) }
        } else {
            scalar_signed(left, right)
        }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    scalar_signed(left, right)
}

pub(crate) fn unsigned(left: &[u8], right: &[u8]) -> IntegerAccumulators {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: same complete-chunk contract as [`signed`].
        unsafe { aarch64::unsigned(left, right) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: runtime detection proves AVX2 and all loads are bounded.
            unsafe { x86_64::unsigned(left, right) }
        } else {
            scalar_unsigned(left, right)
        }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    scalar_unsigned(left, right)
}

#[cfg(any(test, not(target_arch = "aarch64")))]
fn scalar_signed(left: &[i8], right: &[i8]) -> IntegerAccumulators {
    scalar(
        left.iter().map(|value| i64::from(*value)),
        right.iter().map(|value| i64::from(*value)),
    )
}

#[cfg(any(test, not(target_arch = "aarch64")))]
fn scalar_unsigned(left: &[u8], right: &[u8]) -> IntegerAccumulators {
    scalar(
        left.iter().map(|value| i64::from(*value)),
        right.iter().map(|value| i64::from(*value)),
    )
}

#[cfg(any(test, not(target_arch = "aarch64")))]
fn scalar(
    left: impl Iterator<Item = i64>,
    right: impl Iterator<Item = i64>,
) -> IntegerAccumulators {
    left.zip(right)
        .fold(IntegerAccumulators::default(), |mut sums, (left, right)| {
            accumulate(&mut sums, left, right);
            sums
        })
}

fn accumulate(sums: &mut IntegerAccumulators, left: i64, right: i64) {
    let difference = left - right;
    sums.squared_difference += difference * difference;
    sums.dot += left * right;
    sums.left_norm += left * left;
    sums.right_norm += right * right;
    sums.l1 += difference.abs();
}

#[cfg(target_arch = "aarch64")]
mod aarch64 {
    use core::arch::aarch64::{
        vabsq_s16, vaddlvq_s16, vaddvq_s32, vaddvq_u32, vget_high_s16, vget_high_u16, vget_low_s16,
        vget_low_u16, vld1_s8, vld1_u8, vmovl_s8, vmovl_u8, vmull_s16, vmull_u16,
        vreinterpretq_s16_u16, vsubq_s16,
    };

    use super::{IntegerAccumulators, accumulate};

    pub(super) unsafe fn signed(left: &[i8], right: &[i8]) -> IntegerAccumulators {
        let complete = left.len() / 8 * 8;
        let mut sums = IntegerAccumulators::default();
        let mut offset = 0;
        while offset < complete {
            // SAFETY: `complete` contains only full eight-byte chunks.
            let a = unsafe { vmovl_s8(vld1_s8(left.as_ptr().add(offset))) };
            // SAFETY: right has the same validated length as left.
            let b = unsafe { vmovl_s8(vld1_s8(right.as_ptr().add(offset))) };
            // SAFETY: all operations use initialized NEON lanes.
            let difference = unsafe { vsubq_s16(a, b) };
            sums.squared_difference += i64::from(unsafe {
                vaddvq_s32(vmull_s16(
                    vget_low_s16(difference),
                    vget_low_s16(difference),
                )) + vaddvq_s32(vmull_s16(
                    vget_high_s16(difference),
                    vget_high_s16(difference),
                ))
            });
            sums.dot += i64::from(unsafe {
                vaddvq_s32(vmull_s16(vget_low_s16(a), vget_low_s16(b)))
                    + vaddvq_s32(vmull_s16(vget_high_s16(a), vget_high_s16(b)))
            });
            sums.left_norm += i64::from(unsafe {
                vaddvq_s32(vmull_s16(vget_low_s16(a), vget_low_s16(a)))
                    + vaddvq_s32(vmull_s16(vget_high_s16(a), vget_high_s16(a)))
            });
            sums.right_norm += i64::from(unsafe {
                vaddvq_s32(vmull_s16(vget_low_s16(b), vget_low_s16(b)))
                    + vaddvq_s32(vmull_s16(vget_high_s16(b), vget_high_s16(b)))
            });
            sums.l1 += i64::from(unsafe { vaddlvq_s16(vabsq_s16(difference)) });
            offset += 8;
        }
        for (&left, &right) in left[complete..].iter().zip(&right[complete..]) {
            accumulate(&mut sums, i64::from(left), i64::from(right));
        }
        sums
    }

    pub(super) unsafe fn unsigned(left: &[u8], right: &[u8]) -> IntegerAccumulators {
        let complete = left.len() / 8 * 8;
        let mut sums = IntegerAccumulators::default();
        let mut offset = 0;
        while offset < complete {
            // SAFETY: `complete` contains only full eight-byte chunks.
            let a = unsafe { vmovl_u8(vld1_u8(left.as_ptr().add(offset))) };
            // SAFETY: right has the same validated length as left.
            let b = unsafe { vmovl_u8(vld1_u8(right.as_ptr().add(offset))) };
            // SAFETY: values fit signed i16 exactly before subtraction.
            let difference =
                unsafe { vsubq_s16(vreinterpretq_s16_u16(a), vreinterpretq_s16_u16(b)) };
            sums.squared_difference += i64::from(unsafe {
                vaddvq_s32(vmull_s16(
                    vget_low_s16(difference),
                    vget_low_s16(difference),
                )) + vaddvq_s32(vmull_s16(
                    vget_high_s16(difference),
                    vget_high_s16(difference),
                ))
            });
            sums.dot += i64::from(unsafe {
                vaddvq_u32(vmull_u16(vget_low_u16(a), vget_low_u16(b)))
                    + vaddvq_u32(vmull_u16(vget_high_u16(a), vget_high_u16(b)))
            });
            sums.left_norm += i64::from(unsafe {
                vaddvq_u32(vmull_u16(vget_low_u16(a), vget_low_u16(a)))
                    + vaddvq_u32(vmull_u16(vget_high_u16(a), vget_high_u16(a)))
            });
            sums.right_norm += i64::from(unsafe {
                vaddvq_u32(vmull_u16(vget_low_u16(b), vget_low_u16(b)))
                    + vaddvq_u32(vmull_u16(vget_high_u16(b), vget_high_u16(b)))
            });
            sums.l1 += i64::from(unsafe { vaddlvq_s16(vabsq_s16(difference)) });
            offset += 8;
        }
        for (&left, &right) in left[complete..].iter().zip(&right[complete..]) {
            accumulate(&mut sums, i64::from(left), i64::from(right));
        }
        sums
    }
}

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use core::arch::x86_64::{
        __m256i, _mm_loadu_si128, _mm256_abs_epi16, _mm256_cvtepi8_epi16, _mm256_cvtepu8_epi16,
        _mm256_madd_epi16, _mm256_storeu_si256, _mm256_sub_epi16,
    };

    use super::{IntegerAccumulators, accumulate};

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn signed(left: &[i8], right: &[i8]) -> IntegerAccumulators {
        // SAFETY: the runtime caller proved AVX2 and both slices have equal lengths.
        unsafe {
            kernel(
                left.as_ptr().cast(),
                right.as_ptr().cast(),
                left.len(),
                true,
            )
        }
    }

    #[target_feature(enable = "avx2")]
    pub(super) unsafe fn unsigned(left: &[u8], right: &[u8]) -> IntegerAccumulators {
        // SAFETY: the runtime caller proved AVX2 and both slices have equal lengths.
        unsafe { kernel(left.as_ptr(), right.as_ptr(), left.len(), false) }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn kernel(
        left: *const u8,
        right: *const u8,
        len: usize,
        signed: bool,
    ) -> IntegerAccumulators {
        let complete = len / 16 * 16;
        let mut sums = IntegerAccumulators::default();
        let mut offset = 0;
        while offset < complete {
            // SAFETY: each load is inside a complete 16-byte chunk.
            let a8 = unsafe { _mm_loadu_si128(left.add(offset).cast()) };
            let b8 = unsafe { _mm_loadu_si128(right.add(offset).cast()) };
            let a = if signed {
                _mm256_cvtepi8_epi16(a8)
            } else {
                _mm256_cvtepu8_epi16(a8)
            };
            let b = if signed {
                _mm256_cvtepi8_epi16(b8)
            } else {
                _mm256_cvtepu8_epi16(b8)
            };
            let difference = _mm256_sub_epi16(a, b);
            sums.squared_difference += sum_i32(_mm256_madd_epi16(difference, difference));
            sums.dot += sum_i32(_mm256_madd_epi16(a, b));
            sums.left_norm += sum_i32(_mm256_madd_epi16(a, a));
            sums.right_norm += sum_i32(_mm256_madd_epi16(b, b));
            sums.l1 += sum_i16(_mm256_abs_epi16(difference));
            offset += 16;
        }
        for position in offset..len {
            // SAFETY: the scalar tail remains inside both equal-length slices.
            let (left, right) = unsafe { (*left.add(position), *right.add(position)) };
            let left = if signed {
                i64::from(left.cast_signed())
            } else {
                i64::from(left)
            };
            let right = if signed {
                i64::from(right.cast_signed())
            } else {
                i64::from(right)
            };
            accumulate(&mut sums, left, right);
        }
        sums
    }

    #[target_feature(enable = "avx2")]
    fn sum_i32(value: __m256i) -> i64 {
        let mut lanes = [0_i32; 8];
        // SAFETY: the destination has exactly one 256-bit register of space.
        unsafe { _mm256_storeu_si256(lanes.as_mut_ptr().cast(), value) };
        lanes.into_iter().map(i64::from).sum()
    }

    #[target_feature(enable = "avx2")]
    fn sum_i16(value: __m256i) -> i64 {
        let mut lanes = [0_i16; 16];
        // SAFETY: the destination has exactly one 256-bit register of space.
        unsafe { _mm256_storeu_si256(lanes.as_mut_ptr().cast(), value) };
        lanes.into_iter().map(i64::from).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatched_signed_and_unsigned_match_scalar_for_chunks_and_tails() {
        for dimensions in 1..=65 {
            let signed_left =
                core::iter::successors(Some(0_i8), |value| Some(value.wrapping_add(17)))
                    .take(dimensions)
                    .collect::<Vec<_>>();
            let signed_right =
                core::iter::successors(Some(0_i8), |value| Some(value.wrapping_sub(11)))
                    .take(dimensions)
                    .collect::<Vec<_>>();
            assert_eq!(
                signed(&signed_left, &signed_right),
                scalar_signed(&signed_left, &signed_right)
            );

            let unsigned_left =
                core::iter::successors(Some(0_u8), |value| Some(value.wrapping_add(23)))
                    .take(dimensions)
                    .collect::<Vec<_>>();
            let unsigned_right =
                core::iter::successors(Some(0_u8), |value| Some(value.wrapping_add(31)))
                    .take(dimensions)
                    .collect::<Vec<_>>();
            assert_eq!(
                unsigned(&unsigned_left, &unsigned_right),
                scalar_unsigned(&unsigned_left, &unsigned_right)
            );
        }
    }
}
