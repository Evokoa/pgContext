//! Exact-search baseline benchmark runner.

#![allow(
    clippy::print_stdout,
    reason = "benchmark runners report measurements on stdout"
)]

use std::time::Instant;

use context_core::{DistanceMetric, SearchLimit};
use context_test::{BenchmarkDatasetSize, BenchmarkDatasetSpec, ExactSearchBaselineWorkload};

fn main() -> context_core::Result<()> {
    for spec in [
        BenchmarkDatasetSpec::small(),
        BenchmarkDatasetSpec::medium(),
    ] {
        run_baseline(spec)?;
    }
    Ok(())
}

fn run_baseline(spec: BenchmarkDatasetSpec) -> context_core::Result<()> {
    let build_started = Instant::now();
    let workload = ExactSearchBaselineWorkload::from_spec(spec)?;
    let build_elapsed = build_started.elapsed();

    let limit = SearchLimit::new(10)?;
    let search_started = Instant::now();
    let results = workload.run(DistanceMetric::L2, limit)?;
    let search_elapsed = search_started.elapsed();

    println!(
        "dataset={:?} rows={} dimensions={} seed={:#x} vector_bytes={} build_ms={} search_ms={} top_point_id={}",
        spec.size(),
        workload.item_count(),
        spec.dimensions(),
        spec.seed(),
        workload.vector_bytes(),
        build_elapsed.as_millis(),
        search_elapsed.as_millis(),
        results
            .first()
            .map_or(0, context_core::ScoredPoint::point_id),
    );

    if spec.size() == BenchmarkDatasetSize::Medium && results.len() != limit.get() {
        return Err(context_core::Error::InvalidSearchLimit(results.len()));
    }

    Ok(())
}
