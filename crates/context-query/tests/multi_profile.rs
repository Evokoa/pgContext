//! Mixed-profile request, selection, and rank-only fusion contract tests.

#![allow(clippy::expect_used, clippy::panic)]

use context_core::{PointId, ProfileLifecycle};
use context_query::{
    MAX_MULTI_PROFILE_BRANCH_LIMIT, MAX_MULTI_PROFILE_BRANCHES, MAX_MULTI_PROFILE_QUERY_BYTES,
    MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES, MissingProfileReason, MultiProfileBranch,
    MultiProfileCoverage, MultiProfileDecision, MultiProfileObserved, MultiProfileQuery,
    MultiProfileRankedBranch, MultiProfileRankedCandidate, MultiProfileRequest, ProfileName,
    QueryError, QueryKind, ScoreOrder, build_multi_profile_query, fuse_multi_profile,
    plan_multi_profile,
};

fn branch(name: &str, hash: u64, limit: usize, weight: f64) -> MultiProfileBranch {
    MultiProfileBranch::new(
        ProfileName::new(name.to_owned()).expect("profile name"),
        hash,
        "[1,0,0,0]".to_owned(),
        limit,
        weight,
    )
    .expect("branch")
}

fn request(branches: Vec<MultiProfileBranch>, require_all_profiles: bool) -> MultiProfileRequest {
    MultiProfileRequest::new(branches, 10, 100, 200, require_all_profiles).expect("request")
}

#[test]
fn opaque_query_is_bounded_and_serde_transparent() {
    let query = MultiProfileQuery::new("provider-native").expect("bounded query");
    assert_eq!(query.as_str(), "provider-native");
    assert_eq!(
        serde_json::to_value(&query).expect("serialize"),
        serde_json::json!("provider-native")
    );
    assert_eq!(
        serde_json::from_value::<MultiProfileQuery>(serde_json::json!("provider-native"))
            .expect("deserialize"),
        query
    );
    assert!(MultiProfileQuery::new("\n").is_err());
    assert!(serde_json::from_value::<MultiProfileQuery>(serde_json::json!("\n")).is_err());
    assert!(MultiProfileQuery::new("x".repeat(MAX_MULTI_PROFILE_QUERY_BYTES + 1)).is_err());
}

#[test]
fn canonical_builder_creates_weighted_profile_leaves() {
    let query = build_multi_profile_query(
        vec![branch("legacy", 1, 5, 2.0), branch("modern", 2, 7, 1.0)],
        Some(serde_json::json!({
            "must": [{"key": "tenant", "match": {"value": "acme"}}]
        })),
        60,
        4,
    )
    .expect("canonical tree");

    assert_eq!(query.score_order(), ScoreOrder::HigherIsBetter);
    assert_eq!(query.limit(), 4);
    let QueryKind::Prefetch { branches, fusion } = query.kind() else {
        panic!("expected prefetch root");
    };
    assert!(matches!(
        fusion,
        context_query::Fusion::WeightedRrf { rank_constant: 60 }
    ));
    assert_eq!(branches.len(), 2);
    for (branch, expected_limit) in branches.iter().zip([5, 7]) {
        assert_eq!(branch.limit(), expected_limit);
        assert_eq!(branch.score_order(), ScoreOrder::LowerIsBetter);
        let QueryKind::Weighted { query: leaf, .. } = branch.kind() else {
            panic!("expected weighted branch");
        };
        assert!(leaf.filter().is_some());
        assert_eq!(leaf.score_order(), ScoreOrder::LowerIsBetter);
        assert!(matches!(leaf.kind(), QueryKind::ProfileNearest { .. }));
    }
}

#[test]
fn canonical_builder_rejects_duplicate_profiles_and_zero_rrf_k() {
    assert!(
        build_multi_profile_query(
            vec![branch("same", 1, 2, 1.0), branch("same", 2, 2, 1.0)],
            None,
            60,
            2,
        )
        .is_err()
    );
    assert!(build_multi_profile_query(vec![branch("one", 1, 2, 1.0)], None, 0, 2).is_err());
}

#[test]
fn canonical_builder_accepts_exact_node_boundary_and_rejects_one_more_branch() {
    let branches = |count: usize| {
        (0..count)
            .map(|index| branch(&format!("profile-{index}"), index as u64 + 1, 1, 1.0))
            .collect::<Vec<_>>()
    };
    let query = build_multi_profile_query(branches(MAX_MULTI_PROFILE_BRANCHES), None, 60, 1)
        .expect("exact root plus weighted leaves node boundary");
    assert!(matches!(query.kind(), QueryKind::Prefetch { .. }));
    assert!(
        build_multi_profile_query(branches(MAX_MULTI_PROFILE_BRANCHES + 1), None, 60, 1,).is_err()
    );
}

#[test]
fn canonical_builder_rejects_aggregate_query_bytes_over_default_memory_budget() {
    let payload = "x".repeat(MAX_MULTI_PROFILE_QUERY_BYTES);
    let branch_count = MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES / MAX_MULTI_PROFILE_QUERY_BYTES;
    let branches = |count: usize| {
        (0..count)
            .map(|index| {
                MultiProfileBranch::new(
                    ProfileName::new(format!("profile-{index}")).expect("profile name"),
                    index as u64 + 1,
                    payload.clone(),
                    1,
                    1.0,
                )
                .expect("bounded individual query")
            })
            .collect::<Vec<_>>()
    };
    build_multi_profile_query(branches(branch_count), None, 60, 1)
        .expect("exact aggregate query byte boundary");
    assert!(matches!(
        build_multi_profile_query(branches(branch_count + 1), None, 60, 1),
        Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_query_bytes",
            ..
        })
    ));
}

fn observed(name: &str, hash: u64, lifecycle: ProfileLifecycle) -> MultiProfileObserved {
    MultiProfileObserved::new(
        ProfileName::new(name.to_owned()).expect("profile name"),
        hash,
        lifecycle,
        true,
        true,
    )
}

#[test]
fn request_rejects_empty_duplicate_and_unbounded_branches() {
    assert!(matches!(
        MultiProfileRequest::new(Vec::new(), 10, 10, 10, true),
        Err(QueryError::InvalidInput {
            field: "branches",
            ..
        })
    ));

    let duplicate = vec![branch("legacy", 1, 5, 1.0), branch("legacy", 1, 5, 2.0)];
    assert!(matches!(
        MultiProfileRequest::new(duplicate, 10, 10, 10, true),
        Err(QueryError::InvalidInput {
            field: "branches",
            ..
        })
    ));

    let over_budget = vec![branch("legacy", 1, 6, 1.0), branch("modern", 2, 5, 1.0)];
    assert!(matches!(
        MultiProfileRequest::new(over_budget, 10, 10, 10, true),
        Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_candidates",
            ..
        })
    ));
}

#[test]
fn public_planning_and_fusion_bound_adapter_owned_branch_vectors() {
    let request = request(vec![branch("legacy", 1, 10, 1.0)], true);
    let excessive_observed = (0..=MAX_MULTI_PROFILE_BRANCHES)
        .map(|index| {
            MultiProfileObserved::new(
                ProfileName::new(format!("profile_{index}")).expect("profile name"),
                1,
                ProfileLifecycle::Active,
                true,
                true,
            )
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        plan_multi_profile(&request, &excessive_observed),
        Err(QueryError::InvalidInput {
            field: "observed_profiles",
            ..
        })
    ));

    let name = ProfileName::new("legacy").expect("profile name");
    let empty = MultiProfileRankedBranch::new(&name, 1.0, &[]).expect("ranked branch");
    assert!(matches!(
        fuse_multi_profile(&vec![empty; MAX_MULTI_PROFILE_BRANCHES + 1], 60, 1, 1),
        Err(QueryError::InvalidInput {
            field: "branches",
            ..
        })
    ));
}

#[test]
fn branch_and_request_reject_nonfinite_or_nonpositive_controls() {
    for weight in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(matches!(
            MultiProfileBranch::new(
                ProfileName::new("legacy".to_owned()).expect("profile name"),
                1,
                "[1]".to_owned(),
                5,
                weight,
            ),
            Err(QueryError::InvalidInput {
                field: "weight",
                ..
            })
        ));
    }
    assert!(MultiProfileRequest::new(vec![branch("legacy", 1, 5, 1.0)], 0, 5, 5, true).is_err());
    assert!(MultiProfileRequest::new(vec![branch("legacy", 1, 5, 1.0)], 10, 0, 5, true).is_err());
    assert!(
        MultiProfileBranch::new(
            ProfileName::new("max-probe").expect("profile name"),
            1,
            "[1]".to_owned(),
            MAX_MULTI_PROFILE_BRANCH_LIMIT,
            1.0,
        )
        .is_ok()
    );
    assert!(
        MultiProfileBranch::new(
            ProfileName::new("over-max-probe").expect("profile name"),
            1,
            "[1]".to_owned(),
            MAX_MULTI_PROFILE_BRANCH_LIMIT + 1,
            1.0,
        )
        .is_err()
    );
}

#[test]
fn require_all_fails_closed_before_serving_when_one_profile_is_missing() {
    let request = request(
        vec![branch("legacy", 1, 10, 1.0), branch("modern", 2, 10, 1.0)],
        true,
    );
    let decision = plan_multi_profile(&request, &[observed("legacy", 1, ProfileLifecycle::Active)])
        .expect("decision");

    let MultiProfileDecision::FailClosed { missing } = decision else {
        panic!("missing required profile must fail closed")
    };
    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].profile().as_str(), "modern");
    assert_eq!(missing[0].reason(), MissingProfileReason::NotRegistered);
}

#[test]
fn explicit_degraded_mode_records_exact_missing_reasons() {
    let request = request(
        vec![
            branch("legacy", 1, 10, 1.0),
            branch("modern", 2, 10, 1.0),
            branch("future", 3, 10, 1.0),
        ],
        false,
    );
    let decision = plan_multi_profile(
        &request,
        &[
            observed("legacy", 1, ProfileLifecycle::Draining),
            observed("modern", 999, ProfileLifecycle::Active),
            MultiProfileObserved::new(
                ProfileName::new("future".to_owned()).expect("profile name"),
                3,
                ProfileLifecycle::Shadow,
                false,
                true,
            ),
        ],
    )
    .expect("decision");

    let MultiProfileDecision::Serve { branches, coverage } = decision else {
        panic!("explicit degradation must retain the ready branch")
    };
    assert_eq!(branches.len(), 1);
    assert_eq!(branches[0].profile().as_str(), "legacy");
    let MultiProfileCoverage::Partial { missing } = coverage else {
        panic!("skipped branches must be explicit")
    };
    assert_eq!(
        missing
            .iter()
            .map(|entry| (entry.profile().as_str(), entry.reason()))
            .collect::<Vec<_>>(),
        vec![
            ("modern", MissingProfileReason::ConfigurationChanged),
            ("future", MissingProfileReason::LifecycleNotServing),
        ]
    );
}

#[test]
fn degraded_mode_still_fails_closed_when_no_profile_can_serve() {
    let request = request(vec![branch("legacy", 1, 10, 1.0)], false);
    let decision = plan_multi_profile(&request, &[]).expect("decision");
    assert!(matches!(decision, MultiProfileDecision::FailClosed { .. }));
}

#[test]
fn weighted_rrf_has_known_scores_and_contributions() {
    let legacy_name = ProfileName::new("legacy".to_owned()).expect("profile name");
    let modern_name = ProfileName::new("modern".to_owned()).expect("profile name");
    let legacy = [
        MultiProfileRankedCandidate::new(PointId::new(1), 0.01).expect("candidate"),
        MultiProfileRankedCandidate::new(PointId::new(2), 0.02).expect("candidate"),
    ];
    let modern = [
        MultiProfileRankedCandidate::new(PointId::new(2), 900.0).expect("candidate"),
        MultiProfileRankedCandidate::new(PointId::new(3), 800.0).expect("candidate"),
    ];
    let fused = fuse_multi_profile(
        &[
            MultiProfileRankedBranch::new(&legacy_name, 1.0, &legacy).expect("branch"),
            MultiProfileRankedBranch::new(&modern_name, 3.0, &modern).expect("branch"),
        ],
        10,
        3,
        4,
    )
    .expect("fusion");

    assert_eq!(
        fused
            .iter()
            .map(|row| row.point_id().get())
            .collect::<Vec<_>>(),
        vec![2, 3, 1]
    );
    let point_two = &fused[0];
    assert_eq!(point_two.contributions().len(), 2);
    let expected = 0.75 / 11.0 + 0.25 / 12.0;
    assert!((point_two.score() - expected).abs() < 1.0e-12);
}

#[test]
fn raw_scores_never_change_cross_profile_fusion_order() {
    let legacy_name = ProfileName::new("legacy".to_owned()).expect("profile name");
    let modern_name = ProfileName::new("modern".to_owned()).expect("profile name");
    let run = |legacy_score: f64, modern_score: f64| {
        let legacy = [
            MultiProfileRankedCandidate::new(PointId::new(1), legacy_score).expect("candidate"),
            MultiProfileRankedCandidate::new(PointId::new(2), -legacy_score).expect("candidate"),
        ];
        let modern = [
            MultiProfileRankedCandidate::new(PointId::new(2), modern_score).expect("candidate"),
            MultiProfileRankedCandidate::new(PointId::new(1), -modern_score).expect("candidate"),
        ];
        fuse_multi_profile(
            &[
                MultiProfileRankedBranch::new(&legacy_name, 1.0, &legacy).expect("branch"),
                MultiProfileRankedBranch::new(&modern_name, 1.0, &modern).expect("branch"),
            ],
            60,
            2,
            4,
        )
        .expect("fusion")
        .into_iter()
        .map(|row| (row.point_id(), row.score()))
        .collect::<Vec<_>>()
    };

    assert_eq!(run(0.000_001, 1.0e12), run(1.0e12, 0.000_001));
}

#[test]
fn fusion_suppresses_duplicate_occurrences_and_breaks_ties_by_point_id() {
    let name = ProfileName::new("legacy".to_owned()).expect("profile name");
    let rows = [
        MultiProfileRankedCandidate::new(PointId::new(2), 0.1).expect("candidate"),
        MultiProfileRankedCandidate::new(PointId::new(2), 0.2).expect("candidate"),
        MultiProfileRankedCandidate::new(PointId::new(1), 0.3).expect("candidate"),
    ];
    let fused = fuse_multi_profile(
        &[MultiProfileRankedBranch::new(&name, 1.0, &rows).expect("branch")],
        10,
        3,
        3,
    )
    .expect("fusion");

    assert_eq!(fused.len(), 2);
    assert_eq!(fused[0].point_id(), PointId::new(2));
    assert!(fused.iter().all(|point| !point.contributions().is_empty()));
    assert_eq!(fused[0].contributions().len(), 1);

    let equal_rank = [
        MultiProfileRankedCandidate::new(PointId::new(2), 100.0).expect("candidate"),
        MultiProfileRankedCandidate::new(PointId::new(1), -100.0).expect("candidate"),
    ];
    let other_name = ProfileName::new("modern".to_owned()).expect("profile name");
    let tied = fuse_multi_profile(
        &[
            MultiProfileRankedBranch::new(&name, 1.0, &equal_rank[..1]).expect("branch"),
            MultiProfileRankedBranch::new(&other_name, 1.0, &equal_rank[1..]).expect("branch"),
        ],
        10,
        2,
        2,
    )
    .expect("fusion");
    assert_eq!(
        tied.iter()
            .map(|row| row.point_id().get())
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}
