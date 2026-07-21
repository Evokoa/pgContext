//! Hybrid branch baseline benchmark runner.

#![allow(
    clippy::print_stdout,
    reason = "benchmark runners report measurements on stdout"
)]

use std::time::Instant;

use context_test::{BenchmarkDatasetSpec, HybridBaselineWorkload, HybridBenchmarkCase};

fn main() -> context_core::Result<()> {
    let workload = HybridBaselineWorkload::from_spec(BenchmarkDatasetSpec::small())?;
    for case in workload.cases() {
        run_case(&workload, &case);
    }

    Ok(())
}

fn run_case(workload: &HybridBaselineWorkload, case: &HybridBenchmarkCase) {
    let started = Instant::now();
    let summary = workload.run_case(case, 0);
    let summary = summary.with_elapsed_ns(started.elapsed().as_nanos());

    println!(
        "dataset={:?} case={} branches={} non_empty_branches={} input_candidates={} output={} elapsed_ns={} top_point_id={}",
        workload.spec().size(),
        summary.case_name(),
        summary.branch_count(),
        summary.non_empty_branch_count(),
        summary.input_candidate_count(),
        summary.output_count(),
        summary.elapsed_ns(),
        summary.top_point_id().unwrap_or_default(),
    );
}
