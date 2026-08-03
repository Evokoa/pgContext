//! Integer source-vector boundary and exact-accumulator tests.

use context_core::{DistanceMetric, Error, Int8Vector, UInt8Vector};

#[test]
fn signed_and_unsigned_text_round_trip_at_coordinate_bounds() -> Result<(), Error> {
    let signed: Int8Vector = "[-128,-1,0,1,127]".parse()?;
    let unsigned: UInt8Vector = "[0,1,254,255]".parse()?;
    assert_eq!(signed.to_string().parse::<Int8Vector>()?, signed);
    assert_eq!(unsigned.to_string().parse::<UInt8Vector>()?, unsigned);
    assert!("[128]".parse::<Int8Vector>().is_err());
    assert!("[-1]".parse::<UInt8Vector>().is_err());
    Ok(())
}

#[test]
fn integer_accumulators_cover_the_maximum_dimension_without_overflow() -> Result<(), Error> {
    let dimensions = context_core::policy::MAX_VECTOR_DIMENSIONS;
    assert_eq!(
        dimensions, 16_000,
        "fixture must track the frozen dimension policy"
    );
    let dimensions_f64 = 16_000.0;
    let signed_low = Int8Vector::new(vec![i8::MIN; dimensions])?;
    let signed_high = Int8Vector::new(vec![i8::MAX; dimensions])?;
    let unsigned_low = UInt8Vector::new(vec![u8::MIN; dimensions])?;
    let unsigned_high = UInt8Vector::new(vec![u8::MAX; dimensions])?;

    assert_eq!(
        DistanceMetric::L2.distance_int8(&signed_low, &signed_high)?,
        (255_f64.powi(2) * dimensions_f64).sqrt()
    );
    assert_eq!(
        DistanceMetric::InnerProduct.distance_uint8(&unsigned_high, &unsigned_high)?,
        65_025.0 * dimensions_f64
    );
    assert_eq!(
        DistanceMetric::L1.distance_uint8(&unsigned_low, &unsigned_high)?,
        255.0 * dimensions_f64
    );
    Ok(())
}

#[test]
fn integer_cosine_rejects_zero_and_keeps_exact_ties_deterministic() -> Result<(), Error> {
    let zero = Int8Vector::new(vec![0, 0, 0])?;
    let axis = Int8Vector::new(vec![1, 0, 0])?;
    assert!(DistanceMetric::Cosine.distance_int8(&zero, &axis).is_err());

    let left = UInt8Vector::new(vec![3, 4])?;
    let same = UInt8Vector::new(vec![3, 4])?;
    assert_eq!(DistanceMetric::Cosine.distance_uint8(&left, &same)?, 0.0);
    assert_eq!(DistanceMetric::L2.distance_uint8(&left, &same)?, 0.0);
    Ok(())
}

#[test]
fn numeric_integer_vectors_reject_binary_metrics() -> Result<(), Error> {
    let vector = UInt8Vector::new(vec![0, 1])?;
    assert!(
        DistanceMetric::Hamming
            .distance_uint8(&vector, &vector)
            .is_err()
    );
    assert!(
        DistanceMetric::Jaccard
            .distance_uint8(&vector, &vector)
            .is_err()
    );
    Ok(())
}

#[test]
fn integer_text_rejects_over_limit_input_before_collecting_it() {
    let over_limit = format!(
        "[{}]",
        std::iter::repeat_n("0", context_core::policy::MAX_VECTOR_DIMENSIONS + 1)
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(over_limit.parse::<Int8Vector>().is_err());
    assert!(over_limit.parse::<UInt8Vector>().is_err());
}
