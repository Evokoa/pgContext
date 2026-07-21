//! Recall gates for approximate candidate paths.

use std::collections::BTreeMap;
use std::io::Write;

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};
use context_index::{
    HnswConfig, HnswGraph, HnswPointId, RerankCandidate, ScalarQuantizer, binary_quantize,
    rerank_by_original_vectors,
};
use context_test::{
    BenchmarkDatasetSpec, BenchmarkRow, LATE_INTERACTION_BASELINE_LIMIT,
    LateInteractionAnnBaselineWorkload, RecallSummary,
};

const LIMIT: usize = 10;
const QUANTIZED_CANDIDATE_BUDGET: usize = 64;

#[test]
fn hnsw_recall_gate_matches_exact_top_k_fixture() -> context_index::Result<()> {
    let fixture = RecallFixture::small()?;
    let mut graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(32, 128, 64)?);
    for row in &fixture.rows {
        graph.insert(HnswPointId::new(row.point_id), row.vector.clone())?;
    }

    let hnsw = graph.search(&fixture.query, SearchLimit::new(LIMIT)?)?;
    let summary = RecallSummary::from_point_ids(
        fixture.exact_ids.iter().copied(),
        hnsw.iter().map(|point| point.point_id().get()),
    );

    assert_recall_at_least("hnsw_m32_ef64", summary, 0.95)
}

#[test]
fn scalar_quantized_rerank_recall_gate_matches_exact_top_k_fixture() -> context_index::Result<()> {
    let fixture = RecallFixture::small()?;
    let quantizer = ScalarQuantizer::new(-1.0, 1.0, 256)?;
    let query_reconstructed = quantizer.reconstruct(&quantizer.quantize(&fixture.query)?)?;
    let original_by_id = original_vector_map(&fixture.rows);
    let reconstructed = fixture
        .rows
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
        SearchLimit::new(QUANTIZED_CANDIDATE_BUDGET)?,
    )
    .collect::<context_core::Result<Vec<_>>>()
    .map_err(context_index::HnswError::from)?;
    let reranked = rerank_by_original_vectors(
        &fixture.query,
        &rerank_candidates(&approximate, &original_by_id)?,
        DistanceMetric::L2,
        SearchLimit::new(LIMIT)?,
    )?;
    let summary = RecallSummary::from_point_ids(
        fixture.exact_ids.iter().copied(),
        reranked.iter().map(|point| point.point_id()),
    );

    assert_recall_at_least("scalar_sq8_rerank", summary, 0.95)
}

#[test]
fn binary_quantized_rerank_recall_gate_matches_exact_top_k_fixture() -> context_index::Result<()> {
    let fixture = RecallFixture::small()?;
    let query_code = binary_quantize(&fixture.query)?;
    let mut candidates = fixture
        .rows
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
    let candidates = candidates
        .into_iter()
        .take(QUANTIZED_CANDIDATE_BUDGET)
        .map(|(point_id, _distance, vector)| RerankCandidate::with_original(point_id, vector))
        .collect::<Vec<_>>();
    let reranked = rerank_by_original_vectors(
        &fixture.query,
        &candidates,
        DistanceMetric::L2,
        SearchLimit::new(LIMIT)?,
    )?;
    let summary = RecallSummary::from_point_ids(
        fixture.exact_ids.iter().copied(),
        reranked.iter().map(|point| point.point_id()),
    );

    assert_recall_at_least("binary_rerank", summary, 0.75)
}

#[test]
fn late_interaction_ann_rerank_recall_gate_matches_exact_maxsim_fixture()
-> context_index::Result<()> {
    let workload = LateInteractionAnnBaselineWorkload::from_spec(BenchmarkDatasetSpec::small())?;
    let summary = workload.run_summary()?;

    assert_eq!(summary.exact_ids().len(), LATE_INTERACTION_BASELINE_LIMIT);
    assert_eq!(summary.ann_ids().len(), LATE_INTERACTION_BASELINE_LIMIT);
    assert!(summary.token_graph_bytes() > 0);
    assert!(summary.projected_comparisons() > 0);
    assert_recall_at_least("late_interaction_ann_rerank", summary.recall(), 0.95)
}

#[test]
fn recall_gate_rejects_summaries_below_threshold() {
    let summary = RecallSummary::from_point_ids([1, 2, 3, 4], [1, 2]);
    let result = recall_gate("bad_fixture", summary, 0.75);

    assert_eq!(
        result,
        Err(RecallGateError {
            name: "bad_fixture",
            min_recall: 0.75,
            actual_recall: 0.5,
        })
    );
}

struct RecallFixture {
    rows: Vec<BenchmarkRow>,
    query: DenseVector,
    exact_ids: Vec<u64>,
}

impl RecallFixture {
    fn small() -> context_index::Result<Self> {
        let spec = BenchmarkDatasetSpec::small();
        let rows = spec
            .rows_iter()
            .collect::<context_core::Result<Vec<_>>>()
            .map_err(context_index::HnswError::from)?;
        let query = spec
            .query_vector()
            .map_err(context_index::HnswError::from)?;
        let exact_items = rows
            .iter()
            .map(|row| ExactSearchItem::new(row.point_id, row.vector.clone()))
            .collect::<Vec<_>>();
        let exact = exact_top_k(
            &query,
            &exact_items,
            DistanceMetric::L2,
            SearchLimit::new(LIMIT)?,
        )
        .collect::<context_core::Result<Vec<_>>>()
        .map_err(context_index::HnswError::from)?;
        let exact_ids = exact
            .iter()
            .map(context_core::ScoredPoint::point_id)
            .collect();
        Ok(Self {
            rows,
            query,
            exact_ids,
        })
    }
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

fn assert_recall_at_least(
    name: &'static str,
    summary: RecallSummary,
    min_recall: f64,
) -> context_index::Result<()> {
    writeln!(
        std::io::stdout(),
        "recall_gate name={} recall={:.6} min={:.6} intersection={} exact_count={} candidate_count={}",
        name,
        summary.recall(),
        min_recall,
        summary.intersection_count(),
        summary.exact_count(),
        summary.candidate_count()
    )
    .map_err(|error| {
        context_index::HnswError::Core(context_core::Error::InvalidVector(format!(
            "failed to write recall gate report row: {error}"
        )))
    })?;
    recall_gate(name, summary, min_recall).map_err(|error| {
        context_index::HnswError::Core(context_core::Error::InvalidVector(format!(
            "recall gate {} failed: actual {} below minimum {}",
            error.name, error.actual_recall, error.min_recall
        )))
    })
}

fn recall_gate(
    name: &'static str,
    summary: RecallSummary,
    min_recall: f64,
) -> Result<(), RecallGateError> {
    if summary.recall() >= min_recall {
        Ok(())
    } else {
        Err(RecallGateError {
            name,
            min_recall,
            actual_recall: summary.recall(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RecallGateError {
    name: &'static str,
    min_recall: f64,
    actual_recall: f64,
}
