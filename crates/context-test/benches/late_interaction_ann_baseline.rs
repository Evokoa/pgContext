//! Late-interaction ANN candidate-serving baseline benchmark runner.

#![allow(
    clippy::print_stdout,
    reason = "benchmark runners report measurements on stdout"
)]

use std::time::Instant;

use context_core::SearchLimit;
use context_test::{
    BenchmarkDatasetSpec, LATE_INTERACTION_BASELINE_LIMIT, LATE_INTERACTION_CANDIDATES_PER_QUERY,
    LateInteractionAnnBaselineWorkload,
};

fn main() -> context_index::Result<()> {
    let workload = LateInteractionAnnBaselineWorkload::from_spec(BenchmarkDatasetSpec::small())?;
    let limit = SearchLimit::new(LATE_INTERACTION_BASELINE_LIMIT)
        .map_err(context_index::HnswError::from)?;
    let candidates_per_query = SearchLimit::new(LATE_INTERACTION_CANDIDATES_PER_QUERY)
        .map_err(context_index::HnswError::from)?;

    let exact_started = Instant::now();
    let exact = workload.exact_top_k(limit);
    let exact_elapsed = exact_started.elapsed();

    let ann_started = Instant::now();
    let candidate_ids = workload.ann_candidate_point_ids(candidates_per_query)?;
    let ann_elapsed = ann_started.elapsed();
    let measured_candidate_source_keys = candidate_ids.len();

    let rerank_started = Instant::now();
    let reranked = workload.ann_rerank(candidates_per_query, limit)?;
    let rerank_elapsed = rerank_started.elapsed();

    let summary = workload.run_summary()?;

    println!(
        "dataset={:?} points={} vectors_per_point={} token_vectors={} candidates_per_query={} candidate_source_keys={} output={} exact_ns={} ann_candidate_ns={} rerank_ns={} vector_bytes={} token_graph_bytes={} bytes_per_token_vector={} projected_comparisons={} recall={:.6} exact_top_point_id={} ann_top_point_id={}",
        workload.spec().size(),
        summary.point_count(),
        summary.vectors_per_point(),
        summary.token_vector_count(),
        summary.candidates_per_query(),
        measured_candidate_source_keys,
        summary.ann_ids().len(),
        exact_elapsed.as_nanos(),
        ann_elapsed.as_nanos(),
        rerank_elapsed.as_nanos(),
        summary.vector_bytes(),
        summary.token_graph_bytes(),
        bytes_per_token_vector(summary.token_graph_bytes(), summary.token_vector_count()),
        summary.projected_comparisons(),
        summary.recall().recall(),
        exact
            .first()
            .map(|(point_id, _)| *point_id)
            .unwrap_or_default(),
        reranked
            .first()
            .map(|(point_id, _)| *point_id)
            .unwrap_or_default(),
    );

    Ok(())
}

fn bytes_per_token_vector(bytes: usize, token_vectors: usize) -> usize {
    if token_vectors == 0 {
        return 0;
    }
    bytes / token_vectors
}
