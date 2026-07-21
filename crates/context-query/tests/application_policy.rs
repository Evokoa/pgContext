//! Ownership tests for pure query validation and application policy.

#![allow(clippy::expect_used)]

use context_core::SearchLimit;
use context_query::{
    CandidateExpansionDecision, FilteredAnnReason, FilteredAnnStrategyInput,
    FilteredAnnStrategyKind, Formula, LateInteractionWork, MultiVectorAnnReason,
    MultiVectorAnnStrategyInput, MultiVectorAnnStrategyKind, QueryError, QueryPlanValidator,
    candidate_expansion_decision, select_filtered_ann_strategy, select_multi_vector_ann_strategy,
};

#[test]
fn filtered_ann_strategy_is_owned_by_query_policy() {
    let input = FilteredAnnStrategyInput::new(
        1_000,
        Some(24),
        true,
        false,
        32,
        8,
        SearchLimit::new(10).expect("limit should be valid"),
    )
    .expect("strategy input should be valid");

    let strategy = select_filtered_ann_strategy(input);
    assert_eq!(strategy.kind(), FilteredAnnStrategyKind::FilterFirstAnn);
    assert_eq!(
        strategy.reasons(),
        &[FilteredAnnReason::FilterFitsCandidateBudget]
    );
}

#[test]
fn multi_vector_strategy_is_owned_by_query_policy() {
    let input = MultiVectorAnnStrategyInput::new(3, 9, 2, true, true, 20, 1_000)
        .expect("strategy input should be valid");

    let strategy = select_multi_vector_ann_strategy(input);
    assert_eq!(
        strategy.kind(),
        MultiVectorAnnStrategyKind::AnnCandidateServing
    );
    assert_eq!(
        strategy.reasons(),
        &[MultiVectorAnnReason::AnnCandidateServingReady]
    );
    assert_eq!(strategy.projected_comparisons(), 18);
}

#[test]
fn candidate_expansion_policy_moves_without_phantom_executor_usage() {
    let limit = SearchLimit::new(3).expect("limit should be valid");
    let cases = [
        (3, 8, false, 100, CandidateExpansionDecision::Complete),
        (2, 8, true, 100, CandidateExpansionDecision::Complete),
        (
            1,
            12,
            false,
            100,
            CandidateExpansionDecision::Expand {
                next_batch_size: 24,
            },
        ),
        (
            1,
            2,
            false,
            100,
            CandidateExpansionDecision::Expand { next_batch_size: 6 },
        ),
        (
            1,
            60,
            false,
            64,
            CandidateExpansionDecision::Expand {
                next_batch_size: 64,
            },
        ),
        (1, 64, false, 64, CandidateExpansionDecision::Complete),
    ];
    for (survivors, seen, exhausted, expansion_limit, expected) in cases {
        assert_eq!(
            candidate_expansion_decision(limit, survivors, seen, exhausted, expansion_limit),
            expected
        );
    }
}

#[test]
fn query_constructor_validation_preserves_the_stable_json_builder_contract() {
    QueryPlanValidator::recommend_point_ids(&[1, 2], &[3])
        .expect("positive point ids should be valid");
    QueryPlanValidator::discover_point_ids(&[4]).expect("context should be valid");
    QueryPlanValidator::lookup_point_ids(&[7, 8]).expect("lookup list should be valid");
    QueryPlanValidator::prefetch_branches(1).expect("prefetch should be valid");
    QueryPlanValidator::limit(i64::from(i32::MAX)).expect("legacy builder limit should be valid");
    QueryPlanValidator::weight(0.0).expect("zero weight should be valid");
    QueryPlanValidator::score_threshold(Some(0.5), Some(0.5))
        .expect("equal score bounds should be valid");

    assert!(matches!(
        QueryPlanValidator::recommend_point_ids(&[], &[]),
        Err(QueryError::InvalidInput {
            field: "positive",
            ..
        })
    ));
    assert!(matches!(
        QueryPlanValidator::lookup_point_ids(&[0]),
        Err(QueryError::InvalidInput {
            field: "point_id",
            ..
        })
    ));
    assert!(matches!(
        QueryPlanValidator::weight(f64::INFINITY),
        Err(QueryError::InvalidInput {
            field: "weight",
            ..
        })
    ));
    assert!(matches!(
        QueryPlanValidator::score_threshold(Some(f64::NAN), None),
        Err(QueryError::InvalidInput {
            field: "min_score",
            ..
        })
    ));
    assert!(matches!(
        QueryPlanValidator::score_threshold(Some(0.9), Some(0.1)),
        Err(QueryError::InvalidInput {
            field: "score_threshold",
            ..
        })
    ));
}

#[test]
fn formula_validation_preserves_the_existing_bounded_text_contract() {
    assert_eq!(
        Formula::new("$score * 0.5")
            .expect("documented formula should be valid")
            .as_str(),
        "$score * 0.5"
    );
    assert_eq!(
        Formula::new("   ")
            .expect("whitespace formula remains public-compatible")
            .as_str(),
        "   "
    );
    assert!(Formula::new("x".repeat(512)).is_ok());
    assert!(matches!(
        Formula::new(""),
        Err(QueryError::InvalidInput {
            field: "formula",
            ..
        })
    ));
    assert!(matches!(
        Formula::new("x".repeat(513)),
        Err(QueryError::InvalidInput {
            field: "formula",
            ..
        })
    ));
}

#[test]
fn late_interaction_work_projection_is_checked_and_bounded() {
    let work = LateInteractionWork::new(2, 9, 1_000).expect("work should fit the budget");
    assert_eq!(work.projected_comparisons(), 18);
    assert!(matches!(
        LateInteractionWork::new(2, 9, 17),
        Err(QueryError::WorkBudgetExceeded {
            budget: "late_interaction_comparisons",
            actual: 18,
            maximum: 17,
        })
    ));
    assert!(matches!(
        LateInteractionWork::new(usize::MAX, 2, usize::MAX),
        Err(QueryError::ArithmeticOverflow {
            operation: "late_interaction_comparison_projection",
        })
    ));
}
