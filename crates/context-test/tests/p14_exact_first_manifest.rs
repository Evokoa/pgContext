//! Phase 14 exact-first certification manifest contract.

use context_core::{ExactFirstAdvisorPolicy, ExactFirstApplyPolicy, ExactFirstState};
use context_test::*;

#[test]
fn p14_manifest_freezes_states_policies_and_supported_families() {
    assert_eq!(
        P14_READINESS_STATES,
        ["exact_only", "building", "indexed", "stale", "degraded"]
    );
    assert_eq!(
        P14_APPLY_POLICIES,
        [
            "recommend_only",
            "exact_only",
            "enqueue",
            "apply_foreground"
        ]
    );
    for state in P14_READINESS_STATES {
        assert!(ExactFirstState::from_catalog(state).is_some());
    }
    for policy in P14_APPLY_POLICIES {
        assert!(ExactFirstApplyPolicy::from_catalog(policy).is_some());
    }
    assert_eq!(P14_SOURCE_FAMILIES.len(), 13);
    assert_eq!(P14_OPTIMIZATION_FAMILIES, ["exact", "hnsw", "ivfflat"]);
}

#[test]
fn p14_manifest_freezes_every_public_and_resource_bound() {
    assert_eq!(P14_MAX_COLUMNS, 256);
    assert_eq!(P14_MAX_INDEXES, 256);
    assert_eq!(P14_MAX_SPEC_BYTES, 1024 * 1024);
    assert_eq!(P14_MAX_JSON_NODES, 16 * 1024);
    assert_eq!(P14_MAX_JSON_DEPTH, 64);
    assert_eq!(P14_MAX_ERROR_CODE_BYTES, 64);
    assert_eq!(P14_MAX_BATCH_ROWS, 4096);
    assert_eq!(P14_MAX_BATCH_BYTES, 32 * 1024 * 1024);
    assert_eq!(P14_MAX_RSS_BYTES, 512 * 1024 * 1024);
    assert_eq!(P14_MAX_LEASE_MILLIS, 60_000);
    assert_eq!(P14_MAX_ATTEMPTS, 3);
    assert_eq!(P14_INDEXED_CANDIDATE_BUDGET, 10_000_000);
    assert_eq!(P14_REQUIRED_DATASET_ROWS, 10_000_000);
    assert_eq!(P14_REQUIRED_PG_MAJORS, [17, 18]);
    assert_eq!(P14_EXACT_FIRST_GATES.len(), 2);
    assert_eq!(P14_REPORT_MARKERS.len(), 10);
}

#[test]
fn p14_manifest_thresholds_construct_the_pure_advisor_policy() {
    let policy = ExactFirstAdvisorPolicy {
        min_ann_rows: P14_MIN_ANN_ROWS,
        min_ivf_rows: P14_MIN_IVF_ROWS,
        high_churn_millihertz: P14_HIGH_CHURN_MILLIHERTZ,
        selective_filter_bps: P14_SELECTIVE_FILTER_BPS,
        min_ivf_build_window_seconds: P14_MIN_IVF_BUILD_WINDOW_SECONDS,
    };
    assert!(policy.min_ann_rows < policy.min_ivf_rows);
    assert!(policy.selective_filter_bps <= 10_000);
    assert_eq!(P14_MIN_RECALL_BPS, 9_500);
}

#[test]
fn p14_manifest_freezes_dataset_workload_and_hash() {
    assert_eq!(P14_DATASET_GENERATOR_SHA256.len(), 64);
    assert_eq!(P14_WORKLOAD_SHA256.len(), 64);
    assert_eq!(p14_exact_first_manifest_hash(), 0xca96_1d8f_bd5d_b1d6);
}
