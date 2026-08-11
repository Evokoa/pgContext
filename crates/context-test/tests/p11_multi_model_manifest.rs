//! Phase 11 mixed-profile certification manifest contract.

#![allow(clippy::expect_used)]

use context_core::ProfileLifecycle;
use context_query::{MultiProfileBranch, MultiProfileRequest, ProfileName};
use context_test::{
    P11_CANDIDATE_BUDGET, P11_DATASET_REVISION, P11_FUSED_BRANCH_LIMIT, P11_HELD_OUT_QUERY_COUNT,
    P11_MIN_FUSED_RECALL_DELTA, P11_MULTI_MODEL_GATES, P11_PROFILE_COUNT, P11_PROFILE_WEIGHTS,
    P11_QUALITY_CURVES, P11_REPORT_MARKERS, P11_REQUIRED_PG_MAJORS, P11_RRF_K,
    P11_SINGLE_BRANCH_LIMIT, P11_TOP_K, P11_WORKLOAD_REVISION, p11_multi_model_manifest_hash,
};

fn branch(name: &str, limit: usize, weight: u32) -> MultiProfileBranch {
    MultiProfileBranch::new(
        ProfileName::new(name.to_owned()).expect("valid profile name"),
        1,
        "[1,0]".to_owned(),
        limit,
        f64::from(weight),
    )
    .expect("valid branch")
}

#[test]
fn p11_manifest_freezes_unbiased_quality_and_reporting_contracts() {
    assert_eq!(P11_PROFILE_COUNT, 2);
    assert_eq!(P11_TOP_K, 20);
    assert_eq!(P11_HELD_OUT_QUERY_COUNT, 8);
    assert_eq!(P11_RRF_K, 60);
    assert_eq!(P11_FUSED_BRANCH_LIMIT, 50);
    assert_eq!(P11_SINGLE_BRANCH_LIMIT, 101);
    assert_eq!(P11_CANDIDATE_BUDGET, 102);
    assert_eq!(P11_PROFILE_WEIGHTS, [1, 1]);
    assert_eq!(P11_MIN_FUSED_RECALL_DELTA, 0);
    assert_eq!(P11_DATASET_REVISION, "p11-mixed-model-spaces-v2");
    assert_eq!(P11_WORKLOAD_REVISION, "p11-held-out-eight-v2");
    assert_eq!(
        P11_QUALITY_CURVES,
        ["a_only", "b_only", "fused", "partial", "degraded"]
    );
    assert_eq!(
        P11_REPORT_MARKERS,
        [
            "multi_model_sample",
            "multi_model_quality",
            "multi_model_latency_cost",
            "multi_model_environment",
        ]
    );
    assert_eq!(P11_REQUIRED_PG_MAJORS, [17, 18]);
}

#[test]
fn p11_manifest_freezes_required_1m_and_scheduled_10m_commands() {
    assert_eq!(
        P11_MULTI_MODEL_GATES.map(|gate| gate.rows),
        [1_000_000, 10_000_000]
    );
    assert_eq!(P11_MULTI_MODEL_GATES[0].status, "required_pg17_pg18");
    assert_eq!(P11_MULTI_MODEL_GATES[1].status, "scheduled_release_scale");
    for (gate, rows) in P11_MULTI_MODEL_GATES
        .iter()
        .zip(["ROW_COUNT=1000000", "ROW_COUNT=10000000"])
    {
        assert!(gate.command.contains("PG_VERSION=pg{major}"));
        assert!(gate.command.contains("PG_FEATURE=pg{major}"));
        assert!(gate.command.contains("PGPORT={port}"));
        assert!(gate.command.contains(rows));
        assert!(
            gate.command
                .ends_with("./tests/heavy/multi_model_coverage.sh")
        );
        assert_eq!(gate.report_marker, "multi_model_quality");
    }
}

#[test]
fn frozen_fused_and_single_baseline_requests_fit_the_same_candidate_allowance() {
    MultiProfileRequest::new(
        vec![
            branch("legacy_v1", P11_FUSED_BRANCH_LIMIT, P11_PROFILE_WEIGHTS[0]),
            branch("modern_v2", P11_FUSED_BRANCH_LIMIT, P11_PROFILE_WEIGHTS[1]),
        ],
        P11_RRF_K,
        P11_TOP_K,
        P11_CANDIDATE_BUDGET,
        true,
    )
    .expect("two limit+1 probes exactly fit the frozen allowance");

    for profile in ["legacy_v1", "modern_v2"] {
        MultiProfileRequest::new(
            vec![branch(profile, P11_SINGLE_BRANCH_LIMIT, 1)],
            P11_RRF_K,
            P11_TOP_K,
            P11_CANDIDATE_BUDGET,
            true,
        )
        .expect("one limit+1 probe exactly fits the same allowance");
    }

    assert!(ProfileLifecycle::Active.serves_queries());
    assert!(ProfileLifecycle::Draining.serves_queries());
    assert!(!ProfileLifecycle::Shadow.serves_queries());
}

#[test]
fn p11_manifest_hash_detects_any_certification_contract_drift() {
    assert_eq!(p11_multi_model_manifest_hash(), 0x635a_6ab3_da02_173a);
}
