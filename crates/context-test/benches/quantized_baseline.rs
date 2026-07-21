//! Quantized candidate baseline benchmark runner.

#![allow(
    clippy::print_stdout,
    reason = "benchmark runners report measurements on stdout"
)]

use std::{collections::BTreeMap, mem::size_of, time::Instant};

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};
use context_index::{
    ProductCodebook, ProductQuantizer, RerankCandidate, ScalarQuantizer, binary_quantize,
    rerank_by_original_vectors,
};
use context_test::{BenchmarkDatasetSpec, BenchmarkRow, RecallSummary};

const CANDIDATE_BUDGET: usize = 64;
const LIMIT: usize = 10;

fn main() -> context_index::Result<()> {
    let spec = BenchmarkDatasetSpec::small();
    let rows = spec
        .rows_iter()
        .collect::<context_core::Result<Vec<_>>>()
        .map_err(context_index::HnswError::from)?;
    let query = spec
        .query_vector()
        .map_err(context_index::HnswError::from)?;
    let exact = exact_ids(&query, &rows)?;

    run_binary(spec, &query, &rows, &exact)?;
    run_scalar(spec, &query, &rows, &exact)?;
    run_product(spec, &query, &rows, &exact)?;
    Ok(())
}

fn run_binary(
    spec: BenchmarkDatasetSpec,
    query: &DenseVector,
    rows: &[BenchmarkRow],
    exact: &[u64],
) -> context_index::Result<()> {
    let started = Instant::now();
    let query_code = binary_quantize(query)?;
    let mut candidates = rows
        .iter()
        .map(|row| {
            let code = binary_quantize(&row.vector)?;
            let distance = query_code
                .hamming_distance(&code)
                .map_err(context_index::HnswError::from)?;
            Ok((row.point_id, distance, row.vector.clone()))
        })
        .collect::<context_index::Result<Vec<_>>>()?;
    candidates.sort_by_key(|(point_id, distance, _)| (*distance, *point_id));
    let selected = candidates
        .into_iter()
        .take(CANDIDATE_BUDGET)
        .map(|(point_id, _distance, vector)| RerankCandidate::with_original(point_id, vector))
        .collect::<Vec<_>>();
    let reranked = rerank_by_original_vectors(
        query,
        &selected,
        DistanceMetric::L2,
        SearchLimit::new(LIMIT).map_err(context_index::HnswError::from)?,
    )?;
    let elapsed = started.elapsed();
    let recall = RecallSummary::from_point_ids(exact.iter().copied(), reranked_ids(&reranked));

    print_report(spec, "binary", 0, rows.len(), elapsed.as_millis(), recall);
    Ok(())
}

fn run_scalar(
    spec: BenchmarkDatasetSpec,
    query: &DenseVector,
    rows: &[BenchmarkRow],
    exact: &[u64],
) -> context_index::Result<()> {
    let started = Instant::now();
    let quantizer = ScalarQuantizer::new(-1.0, 1.0, 256)?;
    let original_by_id = original_vector_map(rows);
    let query_reconstructed = quantizer.reconstruct(&quantizer.quantize(query)?)?;
    let reconstructed = rows
        .iter()
        .map(|row| {
            let reconstructed = quantizer.reconstruct(&quantizer.quantize(&row.vector)?)?;
            Ok(ExactSearchItem::new(row.point_id, reconstructed))
        })
        .collect::<context_index::Result<Vec<_>>>()?;
    let approximate = exact_top_k(
        &query_reconstructed,
        &reconstructed,
        DistanceMetric::L2,
        SearchLimit::new(CANDIDATE_BUDGET).map_err(context_index::HnswError::from)?,
    )
    .collect::<context_core::Result<Vec<_>>>()
    .map_err(context_index::HnswError::from)?;
    let rerank_candidates = rerank_candidates(&approximate, &original_by_id)?;
    let reranked = rerank_by_original_vectors(
        query,
        &rerank_candidates,
        DistanceMetric::L2,
        SearchLimit::new(LIMIT).map_err(context_index::HnswError::from)?,
    )?;
    let elapsed = started.elapsed();
    let recall = RecallSummary::from_point_ids(exact.iter().copied(), reranked_ids(&reranked));
    let codebook_bytes = usize::from(quantizer.levels()).saturating_mul(size_of::<f32>());

    print_report(
        spec,
        "scalar_sq8",
        codebook_bytes,
        rows.len(),
        elapsed.as_millis(),
        recall,
    );
    Ok(())
}

fn run_product(
    spec: BenchmarkDatasetSpec,
    query: &DenseVector,
    rows: &[BenchmarkRow],
    exact: &[u64],
) -> context_index::Result<()> {
    let started = Instant::now();
    let quantizer = product_quantizer(spec.dimensions())?;
    let original_by_id = original_vector_map(rows);
    let query_reconstructed = quantizer.reconstruct(&quantizer.quantize(query)?)?;
    let reconstructed = rows
        .iter()
        .map(|row| {
            let reconstructed = quantizer.reconstruct(&quantizer.quantize(&row.vector)?)?;
            Ok(ExactSearchItem::new(row.point_id, reconstructed))
        })
        .collect::<context_index::Result<Vec<_>>>()?;
    let approximate = exact_top_k(
        &query_reconstructed,
        &reconstructed,
        DistanceMetric::L2,
        SearchLimit::new(CANDIDATE_BUDGET).map_err(context_index::HnswError::from)?,
    )
    .collect::<context_core::Result<Vec<_>>>()
    .map_err(context_index::HnswError::from)?;
    let rerank_candidates = rerank_candidates(&approximate, &original_by_id)?;
    let reranked = rerank_by_original_vectors(
        query,
        &rerank_candidates,
        DistanceMetric::L2,
        SearchLimit::new(LIMIT).map_err(context_index::HnswError::from)?,
    )?;
    let elapsed = started.elapsed();
    let recall = RecallSummary::from_point_ids(exact.iter().copied(), reranked_ids(&reranked));
    let codebook_bytes = quantizer
        .codebooks()
        .iter()
        .flat_map(ProductCodebook::centroids)
        .map(|centroid| centroid.dimension().saturating_mul(size_of::<f32>()))
        .sum();

    print_report(
        spec,
        "product_quantized",
        codebook_bytes,
        rows.len(),
        elapsed.as_millis(),
        recall,
    );
    Ok(())
}

fn exact_ids(query: &DenseVector, rows: &[BenchmarkRow]) -> context_index::Result<Vec<u64>> {
    let items = rows
        .iter()
        .map(|row| ExactSearchItem::new(row.point_id, row.vector.clone()))
        .collect::<Vec<_>>();
    let exact = exact_top_k(
        query,
        &items,
        DistanceMetric::L2,
        SearchLimit::new(LIMIT).map_err(context_index::HnswError::from)?,
    )
    .collect::<context_core::Result<Vec<_>>>()
    .map_err(context_index::HnswError::from)?;
    Ok(exact
        .iter()
        .map(context_core::ScoredPoint::point_id)
        .collect())
}

fn original_vector_map(rows: &[BenchmarkRow]) -> BTreeMap<u64, DenseVector> {
    rows.iter()
        .map(|row| (row.point_id, row.vector.clone()))
        .collect()
}

fn rerank_candidates(
    approximate: &[context_core::ScoredPoint],
    original_by_id: &BTreeMap<u64, DenseVector>,
) -> context_index::Result<Vec<RerankCandidate>> {
    approximate
        .iter()
        .map(|point| {
            let point_id = point.point_id();
            let vector = original_by_id.get(&point_id).cloned().ok_or_else(|| {
                context_index::HnswError::Core(context_core::Error::InvalidVector(format!(
                    "missing original vector for rerank point {point_id}"
                )))
            })?;
            Ok(RerankCandidate::with_original(point_id, vector))
        })
        .collect()
}

fn reranked_ids(reranked: &[context_index::RerankResult]) -> impl Iterator<Item = u64> + '_ {
    reranked.iter().map(|point| point.point_id())
}

fn product_quantizer(dimensions: usize) -> context_index::Result<ProductQuantizer> {
    let subvector_dimensions = 4;
    let codebook_count = dimensions / subvector_dimensions;
    let codebooks = (0..codebook_count)
        .map(|_| {
            ProductCodebook::new(vec![
                DenseVector::new(vec![-0.75; subvector_dimensions])
                    .map_err(context_index::HnswError::from)?,
                DenseVector::new(vec![-0.25; subvector_dimensions])
                    .map_err(context_index::HnswError::from)?,
                DenseVector::new(vec![0.25; subvector_dimensions])
                    .map_err(context_index::HnswError::from)?,
                DenseVector::new(vec![0.75; subvector_dimensions])
                    .map_err(context_index::HnswError::from)?,
            ])
        })
        .collect::<context_index::Result<Vec<_>>>()?;
    ProductQuantizer::new(subvector_dimensions, codebooks)
}

fn print_report(
    spec: BenchmarkDatasetSpec,
    mode: &str,
    codebook_bytes: usize,
    rows: usize,
    elapsed_ms: u128,
    recall: RecallSummary,
) {
    println!(
        "dataset={:?} mode={} rows={} dimensions={} candidate_budget={} codebook_bytes={} elapsed_ms={} recall={:.6} intersection={} exact_count={} candidate_count={}",
        spec.size(),
        mode,
        rows,
        spec.dimensions(),
        CANDIDATE_BUDGET,
        codebook_bytes,
        elapsed_ms,
        recall.recall(),
        recall.intersection_count(),
        recall.exact_count(),
        recall.candidate_count(),
    );
}
