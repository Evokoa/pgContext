//! Sparse vector canonicalization tests.

use context_core::{DistanceMetric, SparseEntry, SparseVector};

#[test]
fn sparse_vector_parse_canonicalizes_unsorted_entries() -> Result<(), Box<dyn std::error::Error>> {
    let vector: SparseVector = "{3:2,1:1.5}/5".parse()?;

    assert_eq!(vector.dimensions(), 5);
    assert_eq!(vector.non_zero_count(), 2);
    assert_eq!(vector.entries()[0].index(), 1);
    assert_eq!(vector.entries()[0].value(), 1.5);
    assert_eq!(vector.entries()[1].index(), 3);
    assert_eq!(vector.entries()[1].value(), 2.0);
    assert_eq!(vector.to_string(), "{1:1.5,3:2}/5");
    assert_eq!(vector.to_string().parse::<SparseVector>()?, vector);

    Ok(())
}

#[test]
fn sparse_vector_allows_empty_entries_for_positive_dimensions()
-> Result<(), Box<dyn std::error::Error>> {
    let vector: SparseVector = "{}/3".parse()?;

    assert_eq!(vector.dimensions(), 3);
    assert!(vector.entries().is_empty());
    assert_eq!(vector.to_string(), "{}/3");

    Ok(())
}

#[test]
fn sparse_vector_constructor_canonicalizes_entries() -> Result<(), Box<dyn std::error::Error>> {
    let vector = SparseVector::new(
        4,
        vec![SparseEntry::new(4, 7.0)?, SparseEntry::new(2, -1.0)?],
    )?;

    assert_eq!(vector.to_string(), "{2:-1,4:7}/4");

    Ok(())
}

#[test]
fn sparse_vector_rejects_duplicate_dimensions() {
    let result = "{1:2,1:3}/3".parse::<SparseVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "sparsevec duplicate index: 1"
    ));
}

#[test]
fn sparse_vector_rejects_zero_index() {
    let result = "{0:2}/3".parse::<SparseVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "sparsevec indexes are 1-based"
    ));
}

#[test]
fn sparse_vector_rejects_negative_index() {
    let result = "{-1:2}/3".parse::<SparseVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "invalid sparsevec index: -1"
    ));
}

#[test]
fn sparse_vector_rejects_overflowing_index() {
    let result = "{184467440737095516160:2}/3".parse::<SparseVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "invalid sparsevec index: 184467440737095516160"
    ));
}

#[test]
fn sparse_vector_rejects_index_above_dimensions() {
    let result = "{4:2}/3".parse::<SparseVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "sparsevec index 4 exceeds dimensions 3"
    ));
}

#[test]
fn sparse_vector_rejects_non_finite_values() {
    let result = "{1:NaN}/3".parse::<SparseVector>();

    assert!(matches!(result, Err(context_core::Error::InvalidVector(_))));
}

#[test]
fn sparse_vector_rejects_zero_dimensions() {
    let result = "{}/0".parse::<SparseVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "sparsevec dimensions must be greater than zero"
    ));
}

#[test]
fn sparse_vector_rejects_dimensions_above_policy_limit() {
    let result = SparseVector::new(context_core::policy::MAX_VECTOR_DIMENSIONS + 1, Vec::new());

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "sparsevec dimensions exceed policy limit 16000: 16001"
    ));
}

#[test]
fn sparse_vector_rejects_nonzero_count_above_policy_limit() -> Result<(), Box<dyn std::error::Error>>
{
    let mut entries = Vec::with_capacity(context_core::policy::MAX_VECTOR_DIMENSIONS + 1);
    for _ in 0..=(context_core::policy::MAX_VECTOR_DIMENSIONS) {
        entries.push(SparseEntry::new(1, 1.0)?);
    }

    let result = SparseVector::new(context_core::policy::MAX_VECTOR_DIMENSIONS, entries);

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "sparsevec nonzero entries exceed policy limit 16000: 16001"
    ));

    Ok(())
}

#[test]
fn sparse_vector_distance_metrics_return_known_answers() -> Result<(), Box<dyn std::error::Error>> {
    let left: SparseVector = "{1:1,3:2}/4".parse()?;
    let right: SparseVector = "{1:4,2:5}/4".parse()?;

    assert!(
        (DistanceMetric::L2.distance_sparse(&left, &right)? - 38.0_f32.sqrt()).abs() < 0.000_001
    );
    assert_eq!(
        DistanceMetric::InnerProduct.distance_sparse(&left, &right)?,
        4.0
    );
    assert_eq!(
        DistanceMetric::NegativeInnerProduct.distance_sparse(&left, &right)?,
        -4.0
    );
    assert_eq!(DistanceMetric::L1.distance_sparse(&left, &right)?, 10.0);

    let cosine = DistanceMetric::Cosine.distance_sparse(&left, &right)?;
    assert!((cosine - (1.0 - 4.0 / 205.0_f32.sqrt())).abs() < 0.000_001);

    Ok(())
}

#[test]
fn sparse_vector_distance_rejects_mismatch_and_zero_cosine()
-> Result<(), Box<dyn std::error::Error>> {
    let left: SparseVector = "{1:1}/2".parse()?;
    let mismatched: SparseVector = "{1:1}/3".parse()?;
    let zero: SparseVector = "{}/2".parse()?;

    assert!(matches!(
        DistanceMetric::L2.distance_sparse(&left, &mismatched),
        Err(context_core::Error::DimensionMismatch { left: 2, right: 3 })
    ));
    assert!(matches!(
        DistanceMetric::Cosine.distance_sparse(&left, &zero),
        Err(context_core::Error::InvalidVector(message))
            if message == "sparse cosine distance is undefined for zero vectors"
    ));

    Ok(())
}
