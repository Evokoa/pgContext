//! Quantized-vs-exact recall fixtures.

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};
use context_index::{HnswError, RerankCandidate, binary_quantize, rerank_by_original_vectors};

#[test]
fn binary_quantized_candidates_rerank_to_exact_top_k() -> context_index::Result<()> {
    let metric = DistanceMetric::L2;
    let query = vector(&[1.0, 1.0])?;
    let fixtures = [
        (10, vector(&[1.0, 1.0])?),
        (20, vector(&[1.0, 0.8])?),
        (30, vector(&[-1.0, 1.0])?),
        (40, vector(&[-1.0, -1.0])?),
    ];
    let exact_items = fixtures
        .iter()
        .map(|(point_id, vector)| ExactSearchItem::new(*point_id, vector.clone()))
        .collect::<Vec<_>>();
    let exact = exact_top_k(&query, &exact_items, metric, SearchLimit::new(2)?)
        .collect::<context_core::Result<Vec<_>>>()
        .map_err(HnswError::from)?;

    let query_code = binary_quantize(&query)?;
    let mut quantized_candidates = fixtures
        .iter()
        .map(|(point_id, vector)| {
            let code = binary_quantize(vector)?;
            let distance = query_code
                .hamming_distance(&code)
                .map_err(HnswError::from)?;
            Ok((*point_id, distance, vector.clone()))
        })
        .collect::<context_index::Result<Vec<_>>>()?;
    quantized_candidates.sort_by_key(|(point_id, distance, _)| (*distance, *point_id));

    let candidates = quantized_candidates
        .into_iter()
        .take(3)
        .map(|(point_id, _, vector)| RerankCandidate::with_original(point_id, vector))
        .collect::<Vec<_>>();
    let reranked = rerank_by_original_vectors(&query, &candidates, metric, SearchLimit::new(2)?)?;

    assert_eq!(
        reranked
            .iter()
            .map(|point| point.point_id())
            .collect::<Vec<_>>(),
        exact
            .iter()
            .map(context_core::ScoredPoint::point_id)
            .collect::<Vec<_>>()
    );

    Ok(())
}

fn vector(values: &[f32]) -> context_index::Result<DenseVector> {
    DenseVector::new(values.to_vec()).map_err(HnswError::from)
}
