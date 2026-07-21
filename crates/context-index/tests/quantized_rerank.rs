//! Exact rerank tests for quantized candidate flows.

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{RerankCandidate, rerank_by_original_vectors};

#[test]
fn rerank_orders_quantized_candidates_by_original_vectors() -> Result<(), Box<dyn std::error::Error>>
{
    let query: DenseVector = "[0,0]".parse()?;
    let candidates = [
        RerankCandidate::with_original(30, "[2,0]".parse()?),
        RerankCandidate::with_original(10, "[1,0]".parse()?),
        RerankCandidate::with_original(20, "[0,1]".parse()?),
    ];

    let results = rerank_by_original_vectors(
        &query,
        &candidates,
        DistanceMetric::L2,
        SearchLimit::new(2)?,
    )?;

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].point_id(), 10);
    assert_eq!(results[1].point_id(), 20);
    assert_eq!(results[0].score(), 1.0);
    assert_eq!(results[1].score(), 1.0);

    Ok(())
}

#[test]
fn rerank_uses_requested_metric() -> Result<(), Box<dyn std::error::Error>> {
    let query: DenseVector = "[1,0]".parse()?;
    let candidates = [
        RerankCandidate::with_original(10, "[3,0]".parse()?),
        RerankCandidate::with_original(20, "[1,0]".parse()?),
    ];

    let results = rerank_by_original_vectors(
        &query,
        &candidates,
        DistanceMetric::NegativeInnerProduct,
        SearchLimit::new(2)?,
    )?;

    assert_eq!(results[0].point_id(), 10);
    assert_eq!(results[0].score(), -3.0);
    assert_eq!(results[1].point_id(), 20);
    assert_eq!(results[1].score(), -1.0);

    Ok(())
}

#[test]
fn rerank_rejects_missing_original_vectors() -> Result<(), Box<dyn std::error::Error>> {
    let query: DenseVector = "[0,0]".parse()?;
    let candidates = [RerankCandidate::missing_original(42)];

    let result = rerank_by_original_vectors(
        &query,
        &candidates,
        DistanceMetric::L2,
        SearchLimit::new(1)?,
    );

    assert!(matches!(
        result,
        Err(context_index::HnswError::Core(context_core::Error::InvalidVector(message)))
            if message == "missing original vector for rerank point 42"
    ));

    Ok(())
}

#[test]
fn rerank_rejects_dimension_mismatch() -> Result<(), Box<dyn std::error::Error>> {
    let query: DenseVector = "[0,0]".parse()?;
    let candidates = [RerankCandidate::with_original(10, "[1,0,0]".parse()?)];

    let result = rerank_by_original_vectors(
        &query,
        &candidates,
        DistanceMetric::L2,
        SearchLimit::new(1)?,
    );

    assert!(matches!(
        result,
        Err(context_index::HnswError::Core(
            context_core::Error::DimensionMismatch { left: 2, right: 3 }
        ))
    ));

    Ok(())
}
