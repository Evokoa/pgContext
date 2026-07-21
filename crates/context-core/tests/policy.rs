//! Shared policy constants that gate SQL-visible resource use.

#[test]
fn policy_recall_check_point_budget_is_large_enough_for_release_gates() {
    const {
        assert!(
            context_core::policy::MAX_RECALL_CHECK_POINT_IDS >= 10_000,
            "recall-check budget should cover production release recall fixtures"
        );
    }
}

#[test]
fn policy_hnsw_candidate_mask_default_is_within_the_configurable_ceiling() {
    const {
        assert!(
            context_core::policy::DEFAULT_HNSW_CANDIDATE_MASK_POINTS
                <= context_core::policy::MAX_HNSW_CANDIDATE_MASK_POINTS
        );
    }
}

#[test]
fn policy_hnsw_guc_defaults_stay_within_release_bounds() {
    const {
        assert!(
            context_core::policy::DEFAULT_HNSW_CANDIDATE_BUDGET
                <= context_core::policy::MAX_HNSW_CANDIDATE_BUDGET
        );
        assert!(
            context_core::policy::DEFAULT_HNSW_ITERATIVE_EXPANSION_LIMIT
                <= context_core::policy::MAX_HNSW_ITERATIVE_EXPANSION_LIMIT
        );
    }
    assert!((0.0..=1.0).contains(&context_core::policy::DEFAULT_HNSW_RECALL_THRESHOLD));
}
