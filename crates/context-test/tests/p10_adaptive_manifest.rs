//! Phase 10 adaptive-dimension promotion/no-go manifest contract.

#![allow(clippy::expect_used)]

use std::mem::size_of;

use context_core::{MatryoshkaPolicy, PrefixDimensions, SearchLimit, VectorNormalization};
use context_query::{
    AdaptiveWideningBudget, AdaptiveWideningInput, AdaptiveWideningTermination,
    plan_adaptive_widening,
};
use context_test::{
    P10_ADAPTIVE_GATES, P10_CANDIDATE_BUDGET, P10_COMPARISON_BUDGET, P10_EXPECTED_TERMINATION,
    P10_FULL_DIMENSIONS, P10_PREFIX_DIMENSIONS, P10_RECHECK_BUDGET, P10_TOP_K,
    p10_adaptive_manifest_hash,
};

#[test]
fn p10_manifest_freezes_the_1m_and_10m_no_go_decision() {
    assert_eq!(P10_FULL_DIMENSIONS, 768);
    assert_eq!(P10_PREFIX_DIMENSIONS, [128, 256, 512]);
    assert_eq!(P10_TOP_K, 10);
    assert_eq!(P10_CANDIDATE_BUDGET, 10_000);
    assert_eq!(P10_RECHECK_BUDGET, 10_000);
    assert_eq!(
        P10_CANDIDATE_BUDGET,
        context_core::policy::MAX_RECALL_CHECK_POINT_IDS
    );
    assert_eq!(P10_RECHECK_BUDGET, P10_CANDIDATE_BUDGET);
    assert_eq!(
        P10_COMPARISON_BUDGET,
        context_query::DEFAULT_QUERY_COMPARISONS
    );
    assert_eq!(P10_EXPECTED_TERMINATION, "recheck_budget");
    assert_eq!(
        P10_ADAPTIVE_GATES.map(|gate| gate.rows),
        [1_000_000, 10_000_000]
    );
    assert!(P10_ADAPTIVE_GATES.iter().all(|gate| {
        gate.decision == "no_go" && gate.serving_path == "full_vector_exact_fallback"
    }));
    assert_eq!(p10_adaptive_manifest_hash(), 0xe195_4212_821a_571e);
}

#[test]
fn frozen_scale_points_select_exact_fallback_before_prefix_work() {
    let prefixes = P10_PREFIX_DIMENSIONS
        .into_iter()
        .map(|value| PrefixDimensions::new(value).expect("valid prefix"))
        .collect();
    let policy = MatryoshkaPolicy::new(P10_FULL_DIMENSIONS, prefixes, VectorNormalization::UnitL2)
        .expect("valid policy");
    let selected = PrefixDimensions::new(P10_PREFIX_DIMENSIONS[0]).expect("selected prefix");
    let budget = AdaptiveWideningBudget::new(
        P10_CANDIDATE_BUDGET,
        P10_COMPARISON_BUDGET,
        P10_RECHECK_BUDGET,
        64 * 1024 * 1024,
        1,
    )
    .expect("valid budget");

    for gate in P10_ADAPTIVE_GATES {
        let plan = plan_adaptive_widening(
            AdaptiveWideningInput::new(
                &policy,
                selected,
                SearchLimit::new(P10_TOP_K).expect("valid top-k"),
                gate.rows,
                size_of::<context_query::Candidate>(),
                budget,
            )
            .expect("valid schedule input"),
        )
        .expect("bounded schedule projection");
        assert_eq!(
            plan.termination(),
            AdaptiveWideningTermination::RecheckBudget
        );
        assert!(plan.steps().is_empty(), "no prefix work may start at scale");
        assert_eq!(plan.candidate_work(), 0);
        assert_eq!(plan.recheck_work(), 0);
    }
}
