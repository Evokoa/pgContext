//! Dense vector parsing, metric, and exact-search behavior tests.

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};

#[test]
fn dense_vector_parse_format_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let vector: DenseVector = "[1, -2.5, 3.25]".parse()?;

    assert_eq!(vector.dimension(), 3);
    assert_eq!(vector.as_slice(), &[1.0, -2.5, 3.25]);
    assert_eq!(vector.to_string(), "[1,-2.5,3.25]");
    assert_eq!(vector.to_string().parse::<DenseVector>()?, vector);

    Ok(())
}

#[test]
fn dense_vector_rejects_non_finite_values() {
    let result = "[1, NaN, 2]".parse::<DenseVector>();

    assert!(matches!(result, Err(context_core::Error::InvalidVector(_))));
}

#[test]
fn dense_vector_rejects_empty_text_vectors() {
    let result = "[]".parse::<DenseVector>();

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "dense vectors must contain at least one value"
    ));
}

#[test]
fn dense_vector_rejects_dimensions_above_policy_limit() {
    let values = vec![0.0; context_core::policy::MAX_VECTOR_DIMENSIONS + 1];
    let result = DenseVector::new(values);

    assert!(matches!(
        result,
        Err(context_core::Error::InvalidVector(message))
            if message == "dense vector dimensions exceed policy limit 16000: 16001"
    ));
}

#[test]
fn distance_metrics_return_known_answers() -> Result<(), Box<dyn std::error::Error>> {
    let left: DenseVector = "[1, 2, 3]".parse()?;
    let right: DenseVector = "[4, 6, 3]".parse()?;

    assert_eq!(DistanceMetric::L2.distance(&left, &right)?, 5.0);
    assert_eq!(DistanceMetric::InnerProduct.distance(&left, &right)?, 25.0);
    assert_eq!(
        DistanceMetric::NegativeInnerProduct.distance(&left, &right)?,
        -25.0
    );
    assert_eq!(DistanceMetric::L1.distance(&left, &right)?, 7.0);

    let cosine = DistanceMetric::Cosine.distance(&left, &right)?;
    assert!((cosine - 0.144_517_61).abs() < 0.000_001);

    Ok(())
}

#[test]
fn distance_metrics_accept_borrowed_slices_without_vector_allocation()
-> Result<(), Box<dyn std::error::Error>> {
    let left = [1.0, 2.0, 3.0];
    let right = [4.0, 6.0, 3.0];

    assert_eq!(DistanceMetric::L2.distance_slices(&left, &right)?, 5.0);
    assert_eq!(
        DistanceMetric::InnerProduct.distance_slices(&left, &right)?,
        25.0
    );
    let cosine = DistanceMetric::Cosine.distance_slices(&left, &right)?;
    assert!((cosine - 0.144_517_61).abs() < 0.000_001);
    assert!(matches!(
        DistanceMetric::L1.distance_slices(&left, &right[..2]),
        Err(context_core::Error::DimensionMismatch { left: 3, right: 2 })
    ));

    Ok(())
}

#[test]
fn dense_distance_kernels_cover_vector_chunks_and_scalar_tails()
-> Result<(), Box<dyn std::error::Error>> {
    let left = [1.0_f32, -2.0, 3.0, -4.0, 5.0, 6.0, -7.0];
    let right = [-1.0_f32, 2.0, 1.0, -2.0, 3.0, -6.0, 7.0];
    let expected_dot = left.iter().zip(right).map(|(a, b)| a * b).sum::<f32>();
    let expected_l1 = left
        .iter()
        .zip(right)
        .map(|(a, b)| (a - b).abs())
        .sum::<f32>();
    let expected_l2 = left
        .iter()
        .zip(right)
        .map(|(a, b)| {
            let delta = a - b;
            delta * delta
        })
        .sum::<f32>()
        .sqrt();

    assert!((DistanceMetric::L2.distance_slices(&left, &right)? - expected_l2).abs() < 1e-5);
    assert!((DistanceMetric::L1.distance_slices(&left, &right)? - expected_l1).abs() < 1e-5);
    assert!(
        (DistanceMetric::InnerProduct.distance_slices(&left, &right)? - expected_dot).abs() < 1e-5
    );
    Ok(())
}

#[test]
fn distance_metrics_reject_dimension_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let left: DenseVector = "[1, 2, 3]".parse()?;
    let right: DenseVector = "[1, 2]".parse()?;

    let result = DistanceMetric::L2.distance(&left, &right);

    assert!(matches!(
        result,
        Err(context_core::Error::DimensionMismatch { left: 3, right: 2 })
    ));

    Ok(())
}

#[test]
fn cosine_distance_rejects_zero_vectors() -> Result<(), Box<dyn std::error::Error>> {
    let zero: DenseVector = "[0, 0]".parse()?;
    let unit: DenseVector = "[1, 0]".parse()?;

    assert!(matches!(
        DistanceMetric::Cosine.distance(&zero, &unit),
        Err(context_core::Error::InvalidVector(message))
            if message == "cosine distance is undefined for zero vectors"
    ));

    Ok(())
}

#[test]
fn exact_top_k_orders_by_distance_then_point_id() -> Result<(), Box<dyn std::error::Error>> {
    let query: DenseVector = "[0, 0]".parse()?;
    let items = [
        ExactSearchItem::new(30, "[2, 0]".parse()?),
        ExactSearchItem::new(10, "[1, 0]".parse()?),
        ExactSearchItem::new(20, "[0, 1]".parse()?),
    ];

    let results = exact_top_k(&query, &items, DistanceMetric::L2, SearchLimit::new(2)?)
        .collect::<Result<Vec<_>, _>>()?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].point_id(), 10);
    assert_eq!(results[1].point_id(), 20);
    assert_eq!(results[0].score(), 1.0);
    assert_eq!(results[1].score(), 1.0);

    Ok(())
}

#[test]
fn search_limit_rejects_zero_and_values_above_policy_max() {
    assert!(matches!(
        SearchLimit::new(0),
        Err(context_core::Error::InvalidSearchLimit(0))
    ));

    let too_large = context_core::policy::MAX_SEARCH_LIMIT + 1;
    assert!(matches!(
        SearchLimit::new(too_large),
        Err(context_core::Error::InvalidSearchLimit(value)) if value == too_large
    ));
}
