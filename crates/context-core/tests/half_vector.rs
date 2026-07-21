//! Half vector parsing and metric behavior tests.

use context_core::{DistanceMetric, HalfVector};

#[test]
fn half_vector_parse_format_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let vector: HalfVector = "[1, -2.5, 3.25]".parse()?;

    assert_eq!(vector.dimension(), 3);
    assert_eq!(vector.as_slice(), &[1.0, -2.5, 3.25]);
    assert_eq!(vector.to_string(), "[1,-2.5,3.25]");
    assert_eq!(vector.to_string().parse::<HalfVector>()?, vector);

    Ok(())
}

#[test]
fn half_vector_accepts_max_finite_half_values() -> Result<(), Box<dyn std::error::Error>> {
    let vector: HalfVector = "[65504, -65504]".parse()?;

    assert_eq!(vector.as_slice(), &[65_504.0, -65_504.0]);

    Ok(())
}

#[test]
fn half_vector_rejects_overflowing_values() {
    let result = "[65520]".parse::<HalfVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "halfvec value at dimension 0 exceeds finite half precision range: 65520"
    ));
}

#[test]
fn half_vector_rejects_non_finite_values() {
    let result = "[1, Infinity]".parse::<HalfVector>();

    assert!(matches!(result, Err(context_core::Error::InvalidVector(_))));
}

#[test]
fn half_vector_rejects_dimensions_above_policy_limit() {
    let values = vec![0.0; context_core::policy::MAX_VECTOR_DIMENSIONS + 1];
    let result = HalfVector::new(values);

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "halfvec dimensions exceed policy limit 16000: 16001"
    ));
}

#[test]
fn half_vector_distance_metrics_return_known_answers() -> Result<(), Box<dyn std::error::Error>> {
    let left: HalfVector = "[1, 2, 3]".parse()?;
    let right: HalfVector = "[4, 6, 3]".parse()?;

    assert_eq!(DistanceMetric::L2.distance_half(&left, &right)?, 5.0);
    assert_eq!(
        DistanceMetric::InnerProduct.distance_half(&left, &right)?,
        25.0
    );
    assert_eq!(
        DistanceMetric::NegativeInnerProduct.distance_half(&left, &right)?,
        -25.0
    );
    assert_eq!(DistanceMetric::L1.distance_half(&left, &right)?, 7.0);

    let cosine = DistanceMetric::Cosine.distance_half(&left, &right)?;
    assert!((cosine - 0.144_517_61).abs() < 0.000_001);

    Ok(())
}

#[test]
fn half_vector_distance_rejects_dimension_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let left: HalfVector = "[1, 2, 3]".parse()?;
    let right: HalfVector = "[1, 2]".parse()?;

    let result = DistanceMetric::L2.distance_half(&left, &right);

    assert!(matches!(
        result,
        Err(context_core::Error::DimensionMismatch { left: 3, right: 2 })
    ));

    Ok(())
}

#[test]
fn half_vector_cosine_distance_rejects_zero_vectors() -> Result<(), Box<dyn std::error::Error>> {
    let zero: HalfVector = "[0, 0]".parse()?;
    let unit: HalfVector = "[1, 0]".parse()?;

    assert!(matches!(
        DistanceMetric::Cosine.distance_half(&zero, &unit),
        Err(context_core::Error::InvalidVector(message))
            if message == "cosine distance is undefined for zero vectors"
    ));

    Ok(())
}

#[test]
#[allow(
    clippy::excessive_precision,
    reason = "the pinned literals intentionally spell the full decimal expansion of exact binary16 values"
)]
fn half_bit_conversion_pins_known_ieee_binary16_patterns() {
    use context_core::{f32_to_half_bits, half_bits_to_f32};
    // (f32 input, expected binary16 bits, exact widened value)
    let pins: &[(f32, u16, f32)] = &[
        (0.0, 0x0000, 0.0),
        (-0.0, 0x8000, -0.0),
        (1.0, 0x3C00, 1.0),
        (-2.5, 0xC100, -2.5),
        (65504.0, 0x7BFF, 65504.0),
        (-65504.0, 0xFBFF, -65504.0),
        (0.1, 0x2E66, 0.099_975_586),
        (5.960_464_5e-8, 0x0001, 5.960_464_5e-8), // smallest subnormal
        (6.097_555_2e-5, 0x03FF, 6.097_555_2e-5), // largest subnormal
        (6.103_515_6e-5, 0x0400, 6.103_515_6e-5), // smallest normal
    ];
    for (input, bits, widened) in pins {
        assert_eq!(f32_to_half_bits(*input), *bits, "encode {input}");
        assert_eq!(half_bits_to_f32(*bits), *widened, "decode {bits:#06x}");
    }
}

#[test]
fn half_bit_conversion_round_trips_every_finite_bit_pattern() {
    use context_core::{f32_to_half_bits, half_bits_to_f32};
    // Exhaustive over both sign halves of the finite binary16 space: the
    // widened f32 is exactly representable, so re-encoding must be identity.
    for sign in [0x0000_u16, 0x8000] {
        for magnitude in 0..0x7C00_u16 {
            let bits = sign | magnitude;
            let widened = half_bits_to_f32(bits);
            assert_eq!(
                f32_to_half_bits(widened),
                bits,
                "round trip failed for {bits:#06x} (widened {widened})"
            );
        }
    }
}

#[test]
#[allow(
    clippy::excessive_precision,
    reason = "tie-rounding inputs intentionally spell exact halfway decimal expansions"
)]
fn half_bit_conversion_rounds_to_nearest_even_and_saturates() {
    use context_core::{f32_to_half_bits, half_bits_to_f32};
    // Halfway between 0x3C00 (1.0) and 0x3C01 (1.0009765625) is 1.00048828125:
    // ties go to the even mantissa (0x3C00).
    assert_eq!(f32_to_half_bits(1.000_488_281_25), 0x3C00);
    // Halfway between 0x3C01 and 0x3C02 rounds up to even 0x3C02.
    assert_eq!(f32_to_half_bits(1.001_464_843_75), 0x3C02);
    // Just above halfway rounds up.
    assert_eq!(f32_to_half_bits(1.000_49), 0x3C01);
    // Overflow saturates to the infinity pattern; NaN stays NaN.
    assert_eq!(f32_to_half_bits(1.0e6), 0x7C00);
    assert_eq!(f32_to_half_bits(-1.0e6), 0xFC00);
    assert!(half_bits_to_f32(f32_to_half_bits(f32::NAN)).is_nan());
    // Below the smallest subnormal rounds to signed zero.
    assert_eq!(f32_to_half_bits(1.0e-9), 0x0000);
    assert_eq!(f32_to_half_bits(-1.0e-9), 0x8000);
}
