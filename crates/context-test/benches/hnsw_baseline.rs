//! HNSW baseline benchmark runner.

#![allow(
    clippy::print_stdout,
    reason = "benchmark runners report measurements on stdout"
)]

use std::time::Instant;

use context_core::{DistanceMetric, SearchLimit};
use context_index::{HnswConfig, HnswGraph, HnswPointId};
use context_test::{BenchmarkDatasetSpec, ExactSearchBaselineWorkload, RecallSummary};

fn main() -> context_index::Result<()> {
    let spec = BenchmarkDatasetSpec::small();
    for config in [
        HnswConfig::new(8, 32, 16)?,
        HnswConfig::new(16, 64, 32)?,
        HnswConfig::new(32, 128, 64)?,
    ] {
        run_baseline(spec, config)?;
    }
    Ok(())
}

fn run_baseline(spec: BenchmarkDatasetSpec, config: HnswConfig) -> context_index::Result<()> {
    let query = spec
        .query_vector()
        .map_err(context_index::HnswError::from)?;
    let exact =
        ExactSearchBaselineWorkload::from_spec(spec).map_err(context_index::HnswError::from)?;
    let limit = SearchLimit::new(10).map_err(context_index::HnswError::from)?;

    let build_started = Instant::now();
    let mut graph = HnswGraph::new(DistanceMetric::L2, config);
    for row in spec.rows_iter() {
        let row = row.map_err(context_index::HnswError::from)?;
        graph.insert(HnswPointId::new(row.point_id), row.vector)?;
    }
    let build_elapsed = build_started.elapsed();

    let search_started = Instant::now();
    let hnsw_results = graph.search(&query, limit)?;
    let search_elapsed = search_started.elapsed();
    let exact_results = exact
        .run(DistanceMetric::L2, limit)
        .map_err(context_index::HnswError::from)?;
    let recall = RecallSummary::from_point_ids(
        exact_results
            .iter()
            .map(context_core::ScoredPoint::point_id),
        hnsw_results.iter().map(|point| point.point_id().get()),
    );
    let memory = graph.memory_estimate();
    let memory_per_vector = memory.total_bytes() / graph.len().max(1);

    println!(
        "dataset={:?} rows={} dimensions={} m={} ef_construction={} ef_search={} build_ms={} search_ms={} vector_bytes={} graph_bytes={} bytes_per_vector={} recall={:.6} intersection={} exact_count={} candidate_count={}",
        spec.size(),
        graph.len(),
        spec.dimensions(),
        config.m(),
        config.ef_construction(),
        config.ef_search(),
        build_elapsed.as_millis(),
        search_elapsed.as_millis(),
        memory.vector_bytes(),
        memory.total_bytes(),
        memory_per_vector,
        recall.recall(),
        recall.intersection_count(),
        recall.exact_count(),
        recall.candidate_count(),
    );

    Ok(())
}
