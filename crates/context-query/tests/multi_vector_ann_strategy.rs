//! Exhaustive multi-vector ANN application-strategy tests.

#![allow(clippy::expect_used)]

use context_query::{
    MultiVectorAnnReason, MultiVectorAnnStrategyInput, MultiVectorAnnStrategyKind, QueryError,
    select_multi_vector_ann_strategy,
};

#[test]
fn multi_vector_ann_decision_table_is_stable() {
    let cases = [
        (
            input(0, 0, 2, false, false, 10, 1_000),
            MultiVectorAnnStrategyKind::ExactNoOp,
            vec![MultiVectorAnnReason::EmptyCollection],
            0,
        ),
        (
            input(3, 9, 2, false, false, 20, 1_000),
            MultiVectorAnnStrategyKind::ExactTableScan,
            vec![MultiVectorAnnReason::NoAnnServingPath],
            18,
        ),
        (
            input(3, 9, 2, false, false, 20, 17),
            MultiVectorAnnStrategyKind::Rejected,
            vec![MultiVectorAnnReason::ComparisonBudgetExceeded],
            18,
        ),
        (
            input(3, 9, 2, true, false, 20, 1_000),
            MultiVectorAnnStrategyKind::PlannedNotServingReady,
            vec![MultiVectorAnnReason::AnnMetadataNotServingReady],
            18,
        ),
        (
            input(3, 9, 2, true, true, 20, 1_000),
            MultiVectorAnnStrategyKind::AnnCandidateServing,
            vec![MultiVectorAnnReason::AnnCandidateServingReady],
            18,
        ),
    ];

    for (input, expected_kind, expected_reasons, expected_comparisons) in cases {
        let strategy = select_multi_vector_ann_strategy(input.expect("input should be valid"));
        assert_eq!(strategy.kind(), expected_kind);
        assert_eq!(strategy.reasons(), expected_reasons);
        assert_eq!(strategy.projected_comparisons(), expected_comparisons);
    }
}

#[test]
fn multi_vector_ann_input_rejects_impossible_or_zero_values() {
    assert_invalid(input(1, 1, 0, false, false, 10, 10), "query_vectors");
    assert_invalid(input(1, 1, 1, false, false, 0, 10), "candidate_budget");
    assert_invalid(input(1, 1, 1, false, false, 10, 0), "comparison_budget");
    assert_invalid(input(0, 1, 1, false, false, 10, 10), "candidate_vectors");
    assert_invalid(input(1, 11, 1, false, false, 10, 10), "candidate_vectors");
    assert!(matches!(
        input(1, usize::MAX, 2, false, false, usize::MAX, usize::MAX),
        Err(QueryError::ArithmeticOverflow {
            operation: "multi_vector_comparison_projection",
        })
    ));
}

fn input(
    active_points: usize,
    candidate_vectors: usize,
    query_vectors: usize,
    ann_metadata_available: bool,
    ann_candidate_serving_available: bool,
    candidate_budget: usize,
    comparison_budget: usize,
) -> context_query::Result<MultiVectorAnnStrategyInput> {
    MultiVectorAnnStrategyInput::new(
        active_points,
        candidate_vectors,
        query_vectors,
        ann_metadata_available,
        ann_candidate_serving_available,
        candidate_budget,
        comparison_budget,
    )
}

fn assert_invalid(result: context_query::Result<MultiVectorAnnStrategyInput>, field: &'static str) {
    assert!(
        matches!(result, Err(QueryError::InvalidInput { field: actual, .. }) if actual == field)
    );
}
