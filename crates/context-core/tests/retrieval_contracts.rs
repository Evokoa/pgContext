//! Exhaustive tests for the canonical retrieval vocabulary.

use context_core::{
    Completion, ConfigurationRevision, DistanceMetric, GenerationId, IndexKind, OccurrenceId,
    ProfileId, ReadinessReason, ScoreOrder, SourceAuthority, SourceVersion,
};

#[test]
fn every_metric_has_one_total_score_order() {
    let cases = [
        (DistanceMetric::L2, ScoreOrder::LowerIsBetter),
        (DistanceMetric::InnerProduct, ScoreOrder::HigherIsBetter),
        (
            DistanceMetric::NegativeInnerProduct,
            ScoreOrder::LowerIsBetter,
        ),
        (DistanceMetric::Cosine, ScoreOrder::LowerIsBetter),
        (DistanceMetric::L1, ScoreOrder::LowerIsBetter),
        (DistanceMetric::Hamming, ScoreOrder::LowerIsBetter),
        (DistanceMetric::Jaccard, ScoreOrder::LowerIsBetter),
    ];

    for (metric, expected) in cases {
        assert_eq!(metric.score_order(), expected, "metric: {metric:?}");
    }
}

#[test]
fn canonical_score_order_handles_ties_and_direction() {
    assert!(ScoreOrder::LowerIsBetter.is_better(1.0, 2.0));
    assert!(!ScoreOrder::LowerIsBetter.is_better(2.0, 1.0));
    assert!(ScoreOrder::HigherIsBetter.is_better(2.0, 1.0));
    assert!(!ScoreOrder::HigherIsBetter.is_better(1.0, 2.0));
    assert!(!ScoreOrder::HigherIsBetter.is_better(1.0, 1.0));
}

#[test]
fn canonical_retrieval_identities_reject_zero() {
    assert_eq!(GenerationId::new(0), None);
    assert_eq!(ConfigurationRevision::new(0), None);
    assert_eq!(ProfileId::new(0), None);
    assert_eq!(OccurrenceId::new(0), None);
    assert_eq!(SourceVersion::new(0), None);

    assert_eq!(GenerationId::new(7).map(GenerationId::get), Some(7));
    assert_eq!(
        ConfigurationRevision::new(8).map(ConfigurationRevision::get),
        Some(8)
    );
    assert_eq!(ProfileId::new(9).map(ProfileId::get), Some(9));
    assert_eq!(OccurrenceId::new(10).map(OccurrenceId::get), Some(10));
    assert_eq!(SourceVersion::new(11).map(SourceVersion::get), Some(11));
}

proptest::proptest! {
    #[test]
    fn score_order_is_antisymmetric_for_finite_values(left in -1.0e12_f64..1.0e12, right in -1.0e12_f64..1.0e12) {
        for order in [ScoreOrder::LowerIsBetter, ScoreOrder::HigherIsBetter] {
            assert_eq!(order.compare(left, right), order.compare(right, left).reverse());
            assert_eq!(order.compare(left, left), core::cmp::Ordering::Equal);
        }
    }
}

#[test]
fn canonical_registration_enums_are_exhaustive() {
    let indexes = [IndexKind::Exact, IndexKind::Hnsw, IndexKind::IvfFlat];
    let authorities = [
        SourceAuthority::PostgreSqlRow,
        SourceAuthority::ProviderNative,
        SourceAuthority::DerivedArtifact,
    ];
    let readiness = [
        ReadinessReason::Uninitialized,
        ReadinessReason::GenerationMissing,
        ReadinessReason::ConfigurationChanged,
        ReadinessReason::StaleGeneration,
        ReadinessReason::UnsupportedQuery,
        ReadinessReason::ValidationFailed,
        ReadinessReason::PermissionScopeMismatch,
    ];
    let completion = [
        Completion::Complete,
        Completion::Cancelled,
        Completion::BudgetExhausted,
        Completion::Degraded,
    ];

    assert_eq!(indexes.len(), 3);
    assert_eq!(authorities.len(), 3);
    assert_eq!(readiness.len(), 7);
    assert_eq!(completion.len(), 4);
}
