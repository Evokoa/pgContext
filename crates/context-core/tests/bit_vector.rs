//! Bit vector distance behavior tests.

use context_core::BitVector;

#[test]
fn bit_vector_parse_format_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let vector: BitVector = "101001".parse()?;

    assert_eq!(vector.len(), 6);
    assert_eq!(vector.as_slice(), &[true, false, true, false, false, true]);
    assert_eq!(vector.to_string(), "101001");
    assert_eq!(vector.to_string().parse::<BitVector>()?, vector);

    Ok(())
}

#[test]
fn bit_vector_hamming_distance_counts_different_bits() -> Result<(), Box<dyn std::error::Error>> {
    let left: BitVector = "10110".parse()?;
    let right: BitVector = "00101".parse()?;

    assert_eq!(left.hamming_distance(&right)?, 3);

    Ok(())
}

#[test]
fn bit_vector_jaccard_distance_uses_set_bits() -> Result<(), Box<dyn std::error::Error>> {
    let left: BitVector = "11010".parse()?;
    let right: BitVector = "10110".parse()?;

    let distance = left.jaccard_distance(&right)?;

    assert!((distance - 0.5).abs() < 0.000_001);

    Ok(())
}

#[test]
fn bit_vector_jaccard_distance_is_zero_for_two_empty_sets() -> Result<(), Box<dyn std::error::Error>>
{
    let left: BitVector = "000".parse()?;
    let right: BitVector = "000".parse()?;

    assert_eq!(left.jaccard_distance(&right)?, 0.0);

    Ok(())
}

#[test]
fn bit_vector_rejects_empty_text() {
    let result = "".parse::<BitVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "bit vectors must contain at least one bit"
    ));
}

#[test]
fn bit_vector_rejects_non_bit_characters() {
    let result = "102".parse::<BitVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "invalid bit at position 2: 2"
    ));
}

#[test]
fn bit_vector_rejects_lengths_above_policy_limit() {
    let bits = vec![false; context_core::policy::MAX_VECTOR_DIMENSIONS + 1];
    let result = BitVector::new(bits);

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "bit vector bits exceed policy limit 16000: 16001"
    ));
}

#[test]
fn bit_vector_distances_reject_length_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let left: BitVector = "101".parse()?;
    let right: BitVector = "1010".parse()?;

    let result = left.hamming_distance(&right);

    assert!(matches!(
        result,
        Err(context_core::Error::DimensionMismatch { left: 3, right: 4 })
    ));
    assert!(matches!(
        left.jaccard_distance(&right),
        Err(context_core::Error::DimensionMismatch { left: 3, right: 4 })
    ));

    Ok(())
}
