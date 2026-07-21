//! Exhaustive filtered ANN application-strategy tests.

#![allow(clippy::expect_used)]

use context_core::SearchLimit;
use context_query::{
    FilteredAnnReason, FilteredAnnStrategyInput, FilteredAnnStrategyKind, QueryError,
    select_filtered_ann_strategy,
};

#[test]
fn filtered_ann_decision_table_is_stable() {
    let cases = [
        (
            input(0, Some(0), true, false, 32, 8),
            FilteredAnnStrategyKind::Exact,
            vec![FilteredAnnReason::EmptyCollection],
        ),
        (
            input(1_000, None, false, false, 32, 8),
            FilteredAnnStrategyKind::Exact,
            vec![FilteredAnnReason::NoFilter, FilteredAnnReason::NoAnnIndex],
        ),
        (
            input(1_000, None, true, false, 32, 8),
            FilteredAnnStrategyKind::AnnFirstRecheck,
            vec![FilteredAnnReason::NoFilter],
        ),
        (
            input(1_000, Some(0), true, false, 32, 8),
            FilteredAnnStrategyKind::FilterFirstExact,
            vec![FilteredAnnReason::EmptyFilter],
        ),
        (
            input(1_000, Some(100), false, false, 32, 8),
            FilteredAnnStrategyKind::FilterFirstExact,
            vec![FilteredAnnReason::NoAnnIndex],
        ),
        (
            input(1_000, Some(100), true, true, 32, 8),
            FilteredAnnStrategyKind::PartitionLocalSearch,
            vec![FilteredAnnReason::PartitionLocal],
        ),
        (
            input(1_000, Some(8), true, false, 32, 8),
            FilteredAnnStrategyKind::FilterFirstExact,
            vec![FilteredAnnReason::FilterFitsExactCutoff],
        ),
        (
            input(1_000, Some(10), true, false, 32, 4),
            FilteredAnnStrategyKind::FilterFirstExact,
            vec![FilteredAnnReason::FilterFitsExactCutoff],
        ),
        (
            input(1_000, Some(24), true, false, 32, 8),
            FilteredAnnStrategyKind::FilterFirstAnn,
            vec![FilteredAnnReason::FilterFitsCandidateBudget],
        ),
        (
            input(1_000, Some(800), true, false, 32, 8),
            FilteredAnnStrategyKind::AnnFirstRecheck,
            vec![FilteredAnnReason::BroadFilter],
        ),
        (
            input(1_000, Some(200), true, false, 32, 8),
            FilteredAnnStrategyKind::HybridCooperative,
            vec![FilteredAnnReason::IntermediateFilter],
        ),
    ];

    for (input, expected_kind, expected_reasons) in cases {
        let strategy = select_filtered_ann_strategy(input.expect("input should be valid"));
        assert_eq!(strategy.kind(), expected_kind);
        assert_eq!(strategy.reasons(), expected_reasons);
    }
}

#[test]
fn filtered_ann_input_rejects_impossible_or_zero_values() {
    assert_invalid(input(10, Some(11), true, false, 32, 8), "filter_matches");
    assert_invalid(input(10, Some(5), true, false, 0, 8), "candidate_budget");
    assert_invalid(input(10, Some(5), true, false, 32, 0), "exact_cutoff");
}

fn input(
    total_points: usize,
    filter_matches: Option<usize>,
    hnsw_available: bool,
    partition_local: bool,
    candidate_budget: usize,
    exact_cutoff: usize,
) -> context_query::Result<FilteredAnnStrategyInput> {
    FilteredAnnStrategyInput::new(
        total_points,
        filter_matches,
        hnsw_available,
        partition_local,
        candidate_budget,
        exact_cutoff,
        SearchLimit::new(10)?,
    )
}

fn assert_invalid(result: context_query::Result<FilteredAnnStrategyInput>, field: &'static str) {
    assert!(
        matches!(result, Err(QueryError::InvalidInput { field: actual, .. }) if actual == field)
    );
}
