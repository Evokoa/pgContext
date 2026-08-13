//! Exact-first readiness and advisor decision-table coverage.

use context_core::{
    ExactFirstAdvisorInput, ExactFirstAdvisorPolicy, ExactFirstAdvisorReason,
    ExactFirstApplyPolicy, ExactFirstFailure, ExactFirstIndexRecommendation,
    ExactFirstPrecisionRecommendation, ExactFirstReadinessFacts, ExactFirstReason, ExactFirstState,
    advise_exact_first, derive_exact_first_readiness,
};

const CURRENT: ExactFirstReadinessFacts = ExactFirstReadinessFacts {
    relation_current: true,
    source_key_current: true,
    source_columns_current: true,
    configuration_current: true,
    exact_path_available: true,
    build_active: false,
    optimization_ready: false,
    failure: None,
};

const POLICY: ExactFirstAdvisorPolicy = ExactFirstAdvisorPolicy {
    min_ann_rows: 10_000,
    min_ivf_rows: 1_000_000,
    high_churn_millihertz: 10_000,
    selective_filter_bps: 500,
    min_ivf_build_window_seconds: 3_600,
};

fn advisor_input() -> ExactFirstAdvisorInput {
    ExactFirstAdvisorInput {
        rows: 2_000_000,
        dimensions: 128,
        update_millihertz: 100,
        filter_selectivity_bps: 10_000,
        memory_budget_bytes: 2_000_000_000,
        build_window_seconds: 7_200,
        scalar_codec_certified: true,
        product_codec_certified: true,
        prefix_certified: true,
    }
}

#[test]
fn readiness_derivation_covers_every_frozen_state() {
    let exact = derive_exact_first_readiness(CURRENT);
    assert_eq!(exact.state, ExactFirstState::ExactOnly);
    assert_eq!(exact.reason, ExactFirstReason::CurrentExactPath);

    let building = derive_exact_first_readiness(ExactFirstReadinessFacts {
        build_active: true,
        optimization_ready: true,
        failure: Some(ExactFirstFailure::BuildFailed),
        ..CURRENT
    });
    assert_eq!(building.state, ExactFirstState::Building);
    assert_eq!(building.reason, ExactFirstReason::BuildActive);

    let indexed = derive_exact_first_readiness(ExactFirstReadinessFacts {
        optimization_ready: true,
        ..CURRENT
    });
    assert_eq!(indexed.state, ExactFirstState::Indexed);
    assert_eq!(indexed.reason, ExactFirstReason::OptimizationReady);

    let stale = derive_exact_first_readiness(ExactFirstReadinessFacts {
        source_key_current: false,
        optimization_ready: true,
        ..CURRENT
    });
    assert_eq!(stale.state, ExactFirstState::Stale);
    assert_eq!(stale.reason, ExactFirstReason::SourceKeyChanged);

    let degraded = derive_exact_first_readiness(ExactFirstReadinessFacts {
        optimization_ready: true,
        failure: Some(ExactFirstFailure::RecallRejected),
        ..CURRENT
    });
    assert_eq!(degraded.state, ExactFirstState::Degraded);
    assert_eq!(degraded.reason, ExactFirstReason::RecallRejected);
}

#[test]
fn every_state_reason_and_policy_label_round_trips() {
    for state in [
        ExactFirstState::ExactOnly,
        ExactFirstState::Building,
        ExactFirstState::Indexed,
        ExactFirstState::Stale,
        ExactFirstState::Degraded,
    ] {
        assert_eq!(
            ExactFirstState::from_catalog(state.as_catalog()),
            Some(state)
        );
    }
    for reason in [
        ExactFirstReason::CurrentExactPath,
        ExactFirstReason::BuildActive,
        ExactFirstReason::OptimizationReady,
        ExactFirstReason::SourceRelationChanged,
        ExactFirstReason::SourceKeyChanged,
        ExactFirstReason::SourceColumnChanged,
        ExactFirstReason::ConfigurationChanged,
        ExactFirstReason::ExactPathUnavailable,
        ExactFirstReason::BuildFailed,
        ExactFirstReason::BuildCancelled,
        ExactFirstReason::OptimizationCorrupt,
        ExactFirstReason::ResourceLimit,
        ExactFirstReason::RecallRejected,
    ] {
        assert_eq!(
            ExactFirstReason::from_catalog(reason.as_catalog()),
            Some(reason)
        );
    }
    for policy in [
        ExactFirstApplyPolicy::RecommendOnly,
        ExactFirstApplyPolicy::ExactOnly,
        ExactFirstApplyPolicy::Enqueue,
        ExactFirstApplyPolicy::ApplyForeground,
    ] {
        assert_eq!(
            ExactFirstApplyPolicy::from_catalog(policy.as_catalog()),
            Some(policy)
        );
    }
}

#[test]
fn advisor_prefers_exact_hnsw_and_ivf_at_frozen_boundaries() {
    let exact = advise_exact_first(
        ExactFirstAdvisorInput {
            rows: POLICY.min_ann_rows - 1,
            ..advisor_input()
        },
        POLICY,
    );
    assert_eq!(exact.index, ExactFirstIndexRecommendation::ExactOnly);
    assert_eq!(exact.reason, ExactFirstAdvisorReason::SmallCorpus);

    let high_churn = advise_exact_first(
        ExactFirstAdvisorInput {
            update_millihertz: POLICY.high_churn_millihertz,
            ..advisor_input()
        },
        POLICY,
    );
    assert_eq!(high_churn.index, ExactFirstIndexRecommendation::Hnsw);
    assert_eq!(high_churn.reason, ExactFirstAdvisorReason::HighChurn);

    let selective = advise_exact_first(
        ExactFirstAdvisorInput {
            filter_selectivity_bps: POLICY.selective_filter_bps,
            ..advisor_input()
        },
        POLICY,
    );
    assert_eq!(selective.index, ExactFirstIndexRecommendation::Hnsw);
    assert_eq!(selective.reason, ExactFirstAdvisorReason::SelectiveFilters);

    let ivf = advise_exact_first(advisor_input(), POLICY);
    assert_eq!(ivf.index, ExactFirstIndexRecommendation::IvfFlat);
    assert_eq!(ivf.reason, ExactFirstAdvisorReason::LargeLowChurnCorpus);
}

#[test]
fn advisor_never_selects_an_uncertified_memory_shortcut() {
    let no_codec = advise_exact_first(
        ExactFirstAdvisorInput {
            memory_budget_bytes: 1,
            scalar_codec_certified: false,
            product_codec_certified: false,
            prefix_certified: false,
            ..advisor_input()
        },
        POLICY,
    );
    assert_eq!(no_codec.index, ExactFirstIndexRecommendation::ExactOnly);
    assert_eq!(
        no_codec.reason,
        ExactFirstAdvisorReason::InsufficientResources
    );

    let prefix = advise_exact_first(
        ExactFirstAdvisorInput {
            memory_budget_bytes: 1,
            ..advisor_input()
        },
        POLICY,
    );
    assert_eq!(prefix.index, ExactFirstIndexRecommendation::IvfFlat);
    assert_eq!(prefix.precision, ExactFirstPrecisionRecommendation::Prefix);
}

#[test]
fn advisor_labels_are_stable() {
    assert_eq!(ExactFirstIndexRecommendation::Hnsw.as_catalog(), "hnsw");
    assert_eq!(
        ExactFirstPrecisionRecommendation::ProductQuantized.as_catalog(),
        "product_quantized"
    );
    assert_eq!(
        ExactFirstAdvisorReason::InsufficientResources.as_catalog(),
        "insufficient_resources"
    );
}
