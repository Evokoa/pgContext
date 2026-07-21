//! Fixed-seed benchmark dataset tests.

use context_core::{DistanceMetric, SearchLimit};
use context_hybrid::CandidateBranch;
use context_test::{
    BENCHMARK_LATENCY_REGRESSION_LIMIT, BENCHMARK_MEMORY_REGRESSION_LIMIT,
    BENCHMARK_RECALL_DROP_LIMIT, BenchmarkDatasetSize, BenchmarkDatasetSpec,
    BenchmarkDeltaDecision, BenchmarkDeltaError, BenchmarkDeltaMetric, ExactSearchBaselineWorkload,
    HYBRID_BASELINE_LIMIT, HybridBaselineWorkload, PackedPointFilter, RecallSummary,
    evaluate_benchmark_delta,
};

type DeltaTestResult = Result<(), BenchmarkDeltaError>;

#[test]
fn benchmark_dataset_specs_are_pinned() {
    let specs = [
        BenchmarkDatasetSpec::small(),
        BenchmarkDatasetSpec::medium(),
        BenchmarkDatasetSpec::large(),
    ];

    assert_eq!(
        specs.map(|spec| (
            spec.size(),
            spec.rows(),
            spec.dimensions(),
            spec.seed(),
            spec.tenant_count()
        )),
        [
            (
                BenchmarkDatasetSize::Small,
                1_000,
                32,
                0x7067_6374_5f73_6d6c,
                10
            ),
            (
                BenchmarkDatasetSize::Medium,
                100_000,
                64,
                0x7067_6374_5f6d_6564,
                100
            ),
            (
                BenchmarkDatasetSize::Large,
                1_000_000,
                128,
                0x7067_6374_5f6c_7267,
                1_000
            ),
        ]
    );
}

#[test]
fn benchmark_rows_are_deterministic_and_dimensioned() -> context_core::Result<()> {
    let spec = BenchmarkDatasetSpec::small();
    let first_run = spec.rows_iter().take(3).collect::<Result<Vec<_>, _>>()?;
    let second_run = spec.rows_iter().take(3).collect::<Result<Vec<_>, _>>()?;

    assert_eq!(first_run, second_run);
    assert_eq!(
        first_run
            .iter()
            .map(|row| (
                row.point_id,
                row.source_key.as_str(),
                row.vector.dimension(),
                row.tenant_id.as_str(),
                row.body.as_str()
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                1,
                "bench-000000000001",
                32,
                "tenant-0000",
                "small storage tenant-0000 document-000000000001"
            ),
            (
                2,
                "bench-000000000002",
                32,
                "tenant-0001",
                "small retrieval tenant-0001 document-000000000002"
            ),
            (
                3,
                "bench-000000000003",
                32,
                "tenant-0002",
                "small postgres tenant-0002 document-000000000003"
            ),
        ]
    );
    Ok(())
}

#[test]
fn benchmark_large_dataset_is_iterator_backed() -> context_core::Result<()> {
    let spec = BenchmarkDatasetSpec::large();
    let mut rows = spec.rows_iter();
    let first_two = rows.by_ref().take(2).collect::<Result<Vec<_>, _>>()?;

    assert_eq!(
        first_two
            .iter()
            .map(|row| (row.point_id, row.vector.dimension()))
            .collect::<Vec<_>>(),
        vec![(1, 128), (2, 128)]
    );
    assert_eq!(rows.len(), spec.rows() - 2);
    Ok(())
}

#[test]
fn benchmark_query_vectors_are_fixed_by_spec() -> context_core::Result<()> {
    let small = BenchmarkDatasetSpec::small().query_vector()?;
    let medium = BenchmarkDatasetSpec::medium().query_vector()?;
    let large = BenchmarkDatasetSpec::large().query_vector()?;

    assert_eq!(small.dimension(), 32);
    assert_eq!(medium.dimension(), 64);
    assert_eq!(large.dimension(), 128);
    assert_ne!(small.to_string(), medium.to_string());
    assert_ne!(medium.to_string(), large.to_string());
    Ok(())
}

#[test]
fn exact_search_baseline_workload_reports_memory_and_results() -> context_core::Result<()> {
    let workload = ExactSearchBaselineWorkload::from_spec(BenchmarkDatasetSpec::small())?;
    let results = workload.run(DistanceMetric::L2, SearchLimit::new(3)?)?;

    assert_eq!(workload.item_count(), 1_000);
    assert_eq!(workload.vector_bytes(), 1_000 * 32 * 4);
    assert_eq!(results.len(), 3);
    assert_eq!(
        results
            .iter()
            .map(context_core::ScoredPoint::point_id)
            .collect::<Vec<_>>(),
        vec![549, 876, 897]
    );
    Ok(())
}

#[test]
fn hybrid_baseline_workload_pins_release_gate_cases() -> context_core::Result<()> {
    let workload = HybridBaselineWorkload::from_spec(BenchmarkDatasetSpec::small())?;
    let cases = workload.cases();

    assert_eq!(
        cases
            .iter()
            .map(|case| (
                case.name(),
                case.batches()
                    .iter()
                    .map(context_hybrid::CandidateBatch::branch)
                    .collect::<Vec<_>>(),
                case.batches()
                    .iter()
                    .map(|batch| batch.points().len())
                    .collect::<Vec<_>>()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("dense_only", vec![CandidateBranch::DenseExact], vec![100]),
            ("text_only", vec![CandidateBranch::FullText], vec![100]),
            (
                "sparse_planned",
                vec![CandidateBranch::SparsePlanned],
                vec![0]
            ),
            (
                "fused_dense_text",
                vec![CandidateBranch::DenseExact, CandidateBranch::FullText],
                vec![100, 100]
            ),
            ("fully_empty", vec![CandidateBranch::UserProvided], vec![0]),
        ]
    );
    Ok(())
}

#[test]
fn hybrid_baseline_summaries_report_counts_and_empty_outputs() -> context_core::Result<()> {
    let workload = HybridBaselineWorkload::from_spec(BenchmarkDatasetSpec::small())?;
    let cases = workload.cases();
    let summaries = cases
        .iter()
        .map(|case| workload.run_case(case, 123))
        .collect::<Vec<_>>();

    assert_eq!(
        summaries
            .iter()
            .map(|summary| (
                summary.case_name(),
                summary.branch_count(),
                summary.non_empty_branch_count(),
                summary.input_candidate_count(),
                summary.output_count(),
                summary.elapsed_ns(),
                summary.top_point_id()
            ))
            .collect::<Vec<_>>(),
        vec![
            ("dense_only", 1, 1, 100, HYBRID_BASELINE_LIMIT, 123, Some(1)),
            (
                "text_only",
                1,
                1,
                100,
                HYBRID_BASELINE_LIMIT,
                123,
                Some(500)
            ),
            ("sparse_planned", 1, 0, 0, 0, 123, None),
            (
                "fused_dense_text",
                2,
                2,
                200,
                HYBRID_BASELINE_LIMIT,
                123,
                Some(5)
            ),
            ("fully_empty", 1, 0, 0, 0, 123, None),
        ]
    );
    assert!(summaries[2].fused().is_empty());
    assert!(summaries[4].fused().is_empty());
    Ok(())
}

#[test]
fn packed_point_filter_tracks_allowed_points_and_bitmap_bytes() -> context_core::Result<()> {
    let rows = BenchmarkDatasetSpec::small()
        .rows_iter()
        .take(130)
        .collect::<Result<Vec<_>, _>>()?;
    let filter =
        PackedPointFilter::from_rows(&rows, |row| matches!(row.tenant_id.as_str(), "tenant-0000"));

    assert_eq!(filter.allowed_count(), 13);
    assert_eq!(filter.bitmap_bytes(), 3 * 8);
    assert!(filter.contains_point_id(1));
    assert!(filter.contains_point_id(121));
    assert!(!filter.contains_point_id(2));
    assert!(!filter.contains_point_id(131));
    assert_eq!(
        filter.allowed_point_ids(),
        vec![1, 11, 21, 31, 41, 51, 61, 71, 81, 91, 101, 111, 121]
    );
    Ok(())
}

#[test]
fn packed_point_filter_handles_empty_and_no_match_inputs() -> context_core::Result<()> {
    let empty = PackedPointFilter::from_rows(&[], |_| true);
    assert_eq!(empty.allowed_count(), 0);
    assert_eq!(empty.bitmap_bytes(), 0);
    assert!(!empty.contains_point_id(1));
    assert!(empty.allowed_point_ids().is_empty());

    let rows = BenchmarkDatasetSpec::small()
        .rows_iter()
        .take(3)
        .collect::<Result<Vec<_>, _>>()?;
    let no_match = PackedPointFilter::from_rows(&rows, |_| false);
    assert_eq!(no_match.allowed_count(), 0);
    assert_eq!(no_match.bitmap_bytes(), 8);
    assert!(!no_match.contains_point_id(1));
    assert!(no_match.allowed_point_ids().is_empty());
    Ok(())
}

#[test]
fn recall_summary_deduplicates_ids_and_reports_intersection() {
    let summary = RecallSummary::from_point_ids([1, 2, 2, 3], [2, 3, 4, 4]);

    assert_eq!(summary.exact_count(), 3);
    assert_eq!(summary.candidate_count(), 3);
    assert_eq!(summary.intersection_count(), 2);
    assert!((summary.recall() - 0.666_666_666_666).abs() < 0.000_000_001);
}

#[test]
fn recall_summary_treats_empty_exact_set_as_complete() {
    let summary = RecallSummary::from_point_ids([], [1, 2, 3]);

    assert_eq!(summary.exact_count(), 0);
    assert_eq!(summary.candidate_count(), 3);
    assert_eq!(summary.intersection_count(), 0);
    assert_eq!(summary.recall(), 1.0);
}

#[test]
fn benchmark_delta_policy_accepts_improvements_and_threshold_boundary() -> DeltaTestResult {
    let improved_latency = evaluate_benchmark_delta(BenchmarkDeltaMetric::Latency, 100.0, 80.0)?;
    let boundary_memory = evaluate_benchmark_delta(
        BenchmarkDeltaMetric::Memory,
        2_000.0,
        2_000.0 * (1.0 + BENCHMARK_MEMORY_REGRESSION_LIMIT),
    )?;
    let boundary_recall = evaluate_benchmark_delta(
        BenchmarkDeltaMetric::Recall,
        0.99,
        0.99 - BENCHMARK_RECALL_DROP_LIMIT,
    )?;

    assert!(!improved_latency.requires_review());
    assert_eq!(improved_latency.summary().actual_regression(), 0.0);
    assert!(!boundary_memory.requires_review());
    assert_eq!(
        boundary_memory.summary().allowed_regression(),
        BENCHMARK_MEMORY_REGRESSION_LIMIT
    );
    assert!(!boundary_recall.requires_review());
    assert_eq!(
        boundary_recall.summary().allowed_regression(),
        BENCHMARK_RECALL_DROP_LIMIT
    );
    Ok(())
}

#[test]
fn benchmark_delta_policy_requires_review_for_slowdowns_and_recall_drops() -> DeltaTestResult {
    let latency = evaluate_benchmark_delta(
        BenchmarkDeltaMetric::Latency,
        100.0,
        100.0 * (1.0 + BENCHMARK_LATENCY_REGRESSION_LIMIT) + 0.01,
    )?;
    let memory = evaluate_benchmark_delta(
        BenchmarkDeltaMetric::Memory,
        1_000.0,
        1_000.0 * (1.0 + BENCHMARK_MEMORY_REGRESSION_LIMIT) + 1.0,
    )?;
    let recall = evaluate_benchmark_delta(BenchmarkDeltaMetric::Recall, 0.95, 0.93)?;

    assert!(matches!(latency, BenchmarkDeltaDecision::ReviewRequired(_)));
    assert!(latency.requires_review());
    assert_eq!(latency.summary().metric(), BenchmarkDeltaMetric::Latency);
    assert!(latency.summary().actual_regression() > BENCHMARK_LATENCY_REGRESSION_LIMIT);

    assert!(matches!(memory, BenchmarkDeltaDecision::ReviewRequired(_)));
    assert_eq!(memory.summary().metric(), BenchmarkDeltaMetric::Memory);
    assert!(memory.summary().actual_regression() > BENCHMARK_MEMORY_REGRESSION_LIMIT);

    assert!(matches!(recall, BenchmarkDeltaDecision::ReviewRequired(_)));
    assert_eq!(
        recall.summary().actual_regression(),
        0.019_999_999_999_999_907
    );
    Ok(())
}

#[test]
fn benchmark_delta_policy_rejects_invalid_lower_is_better_inputs() {
    assert_eq!(
        evaluate_benchmark_delta(BenchmarkDeltaMetric::Latency, 0.0, 1.0),
        Err(BenchmarkDeltaError::InvalidPositiveBaseline {
            metric: BenchmarkDeltaMetric::Latency,
            baseline: 0.0,
        })
    );
    let nan_baseline = evaluate_benchmark_delta(BenchmarkDeltaMetric::Memory, f64::NAN, 1.0);
    assert!(matches!(
        nan_baseline,
        Err(BenchmarkDeltaError::InvalidPositiveBaseline {
            metric: BenchmarkDeltaMetric::Memory,
            baseline
        }) if baseline.is_nan()
    ));
    assert_eq!(
        evaluate_benchmark_delta(BenchmarkDeltaMetric::Memory, 1.0, -1.0),
        Err(BenchmarkDeltaError::InvalidCurrent {
            metric: BenchmarkDeltaMetric::Memory,
            current: -1.0,
        })
    );
}

#[test]
fn benchmark_delta_policy_rejects_invalid_recall_inputs() {
    assert_eq!(
        evaluate_benchmark_delta(BenchmarkDeltaMetric::Recall, 1.1, 0.9),
        Err(BenchmarkDeltaError::InvalidRecall { value: 1.1 })
    );
    assert_eq!(
        evaluate_benchmark_delta(BenchmarkDeltaMetric::Recall, 0.9, -0.1),
        Err(BenchmarkDeltaError::InvalidRecall { value: -0.1 })
    );
    assert_eq!(
        evaluate_benchmark_delta(BenchmarkDeltaMetric::Recall, f64::INFINITY, 0.9),
        Err(BenchmarkDeltaError::InvalidRecall {
            value: f64::INFINITY
        })
    );
}
