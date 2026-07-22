//! Quantized candidate recall and latency benchmark runner.

#![allow(
    clippy::print_stdout,
    reason = "benchmark runners report measurements on stdout"
)]

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap},
    hint::black_box,
    mem::size_of,
    time::{Duration, Instant},
};

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};
use context_index::{
    RerankCandidate, TrainedQuantizer, rerank_by_original_vectors, train_product_quantizer,
    train_scalar_quantizer,
};
use context_storage::HnswGraphQuantizationCodebook;
use context_test::{BenchmarkDatasetSpec, BenchmarkRow, RecallSummary};

const LIMIT: usize = 10;
const TRAINING_SAMPLE: usize = 4_096;
const MIN_RECALL: f64 = 0.95;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = BenchmarkDatasetSpec::medium();
    let rows = spec.rows_iter().collect::<context_core::Result<Vec<_>>>()?;
    let query = spec.query_vector()?;
    let exact_started = Instant::now();
    let exact = exact_ids(&query, &rows)?;
    let exact_elapsed = exact_started.elapsed();
    let sample = rows
        .iter()
        .take(TRAINING_SAMPLE)
        .map(|row| row.vector.clone())
        .collect::<Vec<_>>();

    run_mode(
        spec,
        "binary",
        TrainedQuantizer::binary(spec.dimensions())?,
        &query,
        &rows,
        &exact,
        exact_elapsed,
        2_048,
    )?;
    run_mode(
        spec,
        "scalar_sq8",
        train_scalar_quantizer(&sample, 256, None)?,
        &query,
        &rows,
        &exact,
        exact_elapsed,
        256,
    )?;
    run_mode(
        spec,
        "product_quantized",
        train_product_quantizer(&sample, 8, 32, 8)?,
        &query,
        &rows,
        &exact,
        exact_elapsed,
        8_192,
    )?;
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "the benchmark keeps dataset, trained mode, oracle, and timing inputs explicit"
)]
fn run_mode(
    spec: BenchmarkDatasetSpec,
    mode: &str,
    trained: TrainedQuantizer,
    query: &DenseVector,
    rows: &[BenchmarkRow],
    exact: &[u64],
    exact_elapsed: Duration,
    candidate_budget: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let codebook = persisted_codebook(&trained);
    let codes = rows
        .iter()
        .map(|row| trained.quantize(&row.vector))
        .collect::<context_index::Result<Vec<_>>>()?;
    let prepared = codebook.prepare_query(query, DistanceMetric::L2)?;
    let original_by_id = original_vector_map(rows);

    let started = Instant::now();
    let approximate = rows.iter().zip(&codes).try_fold(
        BinaryHeap::with_capacity(candidate_budget + 1),
        |mut nearest, (row, code)| {
            let candidate = ApproximateCandidate {
                point_id: row.point_id,
                score: prepared.score(black_box(code))?,
            };
            if nearest.len() < candidate_budget
                || nearest.peek().is_some_and(|worst| candidate < *worst)
            {
                nearest.push(candidate);
                if nearest.len() > candidate_budget {
                    nearest.pop();
                }
            }
            Ok::<_, context_storage::HnswGraphPayloadError>(nearest)
        },
    )?;
    let rerank_candidates = approximate
        .into_sorted_vec()
        .into_iter()
        .map(|candidate| {
            let vector = original_by_id
                .get(&candidate.point_id)
                .cloned()
                .ok_or_else(|| {
                    context_core::Error::InvalidVector(format!(
                        "missing original vector for rerank point {}",
                        candidate.point_id
                    ))
                })?;
            Ok(RerankCandidate::with_original(candidate.point_id, vector))
        })
        .collect::<context_core::Result<Vec<_>>>()?;
    let reranked = rerank_by_original_vectors(
        query,
        &rerank_candidates,
        DistanceMetric::L2,
        SearchLimit::new(LIMIT)?,
    )?;
    let elapsed = started.elapsed();
    let recall = RecallSummary::from_point_ids(
        exact.iter().copied(),
        reranked.iter().map(|point| point.point_id()),
    );
    let codebook_bytes = codebook_bytes(&codebook);
    let encoded_bytes = codes.iter().map(Vec::len).sum::<usize>() + codebook_bytes;
    let full_precision_bytes = rows
        .len()
        .saturating_mul(spec.dimensions())
        .saturating_mul(size_of::<f32>());
    let speedup = exact_elapsed.as_secs_f64() / elapsed.as_secs_f64();
    if recall.recall() < MIN_RECALL {
        return Err(std::io::Error::other(format!(
            "{mode} recall {:.6} is below promotion threshold {MIN_RECALL:.6}",
            recall.recall()
        ))
        .into());
    }
    if elapsed >= exact_elapsed {
        return Err(std::io::Error::other(format!(
            "{mode} quantized serving {}ns did not beat full-precision scan {}ns",
            elapsed.as_nanos(),
            exact_elapsed.as_nanos()
        ))
        .into());
    }

    println!(
        "dataset={:?} mode={} rows={} dimensions={} candidate_budget={} encoded_bytes={} full_precision_bytes={} quantized_ns={} exact_scan_ns={} speedup={:.3} recall={:.6} intersection={} exact_count={} candidate_count={}",
        spec.size(),
        mode,
        rows.len(),
        spec.dimensions(),
        candidate_budget,
        encoded_bytes,
        full_precision_bytes,
        elapsed.as_nanos(),
        exact_elapsed.as_nanos(),
        speedup,
        recall.recall(),
        recall.intersection_count(),
        recall.exact_count(),
        recall.candidate_count(),
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ApproximateCandidate {
    point_id: u64,
    score: f32,
}

impl Eq for ApproximateCandidate {}

impl Ord for ApproximateCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| self.point_id.cmp(&other.point_id))
    }
}

impl PartialOrd for ApproximateCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn persisted_codebook(trained: &TrainedQuantizer) -> HnswGraphQuantizationCodebook {
    match trained {
        TrainedQuantizer::Binary { dimensions } => HnswGraphQuantizationCodebook::Binary {
            dimensions: *dimensions,
        },
        TrainedQuantizer::Scalar {
            quantizer,
            dimensions,
        } => HnswGraphQuantizationCodebook::Scalar {
            dimensions: *dimensions,
            minimum: quantizer.min(),
            maximum: quantizer.max(),
            levels: quantizer.levels(),
        },
        TrainedQuantizer::Product(quantizer) => HnswGraphQuantizationCodebook::Product {
            dimensions: trained.dimensions(),
            subvector_dimensions: quantizer.subvector_dimensions(),
            codebooks: quantizer
                .codebooks()
                .iter()
                .map(|codebook| codebook.centroids().to_vec())
                .collect(),
        },
    }
}

fn codebook_bytes(codebook: &HnswGraphQuantizationCodebook) -> usize {
    match codebook {
        HnswGraphQuantizationCodebook::Binary { .. } => 0,
        HnswGraphQuantizationCodebook::Scalar { .. } => 16,
        HnswGraphQuantizationCodebook::Product { codebooks, .. } => codebooks
            .iter()
            .flatten()
            .map(|centroid| centroid.dimension() * size_of::<f32>())
            .sum(),
    }
}

fn exact_ids(query: &DenseVector, rows: &[BenchmarkRow]) -> context_core::Result<Vec<u64>> {
    let items = rows
        .iter()
        .map(|row| ExactSearchItem::new(row.point_id, row.vector.clone()))
        .collect::<Vec<_>>();
    exact_top_k(query, &items, DistanceMetric::L2, SearchLimit::new(LIMIT)?)
        .map(|point| point.map(|point| point.point_id()))
        .collect()
}

fn original_vector_map(rows: &[BenchmarkRow]) -> BTreeMap<u64, DenseVector> {
    rows.iter()
        .map(|row| (row.point_id, row.vector.clone()))
        .collect()
}
