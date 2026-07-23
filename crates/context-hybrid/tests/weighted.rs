//! Normalized weighted-fusion known-answer and property tests.

#![allow(clippy::expect_used)]

use context_hybrid::{
    BranchCandidate, RankedPoint, RrfK, ScoreDirection, WeightedBranch, WeightedFusionError,
    reciprocal_rank_fusion, weighted_fusion,
};
use proptest::prelude::*;

fn scored(point_id: u64, score: f64) -> BranchCandidate {
    BranchCandidate::with_score(point_id, score)
}

#[test]
fn weighted_fusion_normalizes_direction_and_deduplicates_per_branch() {
    let dense = [scored(1, 0.1), scored(2, 0.9), scored(2, 0.8)];
    let sparse = [scored(1, 1.0), scored(3, 0.0)];
    let fused = weighted_fusion(
        &[
            WeightedBranch::new(&dense, 3.0, ScoreDirection::HigherIsBetter),
            WeightedBranch::new(&sparse, 1.0, ScoreDirection::LowerIsBetter),
        ],
        10,
    )
    .expect("weighted fusion should accept finite scored branches");

    assert_eq!(
        fused
            .iter()
            .map(|point| point.point_id())
            .collect::<Vec<_>>(),
        vec![2, 3, 1]
    );
    assert_eq!(fused[0].score(), 0.75);
    assert_eq!(fused[1].score(), 0.25);
}

#[test]
fn weighted_fusion_rejects_invalid_weights_and_missing_scores() {
    let unscored = [BranchCandidate::new(1)];
    assert_eq!(
        weighted_fusion(
            &[WeightedBranch::new(
                &unscored,
                1.0,
                ScoreDirection::HigherIsBetter,
            )],
            1,
        ),
        Err(WeightedFusionError::MissingScore)
    );
    assert_eq!(
        weighted_fusion(
            &[WeightedBranch::new(
                &[],
                0.0,
                ScoreDirection::HigherIsBetter,
            )],
            1,
        ),
        Err(WeightedFusionError::ZeroTotalWeight)
    );
}

#[test]
fn weighted_fusion_normalizes_extreme_finite_scores_without_nan() {
    let points = [scored(1, -1.0e308), scored(2, 0.0), scored(3, 1.0e308)];
    let fused = weighted_fusion(
        &[WeightedBranch::new(
            &points,
            1.0,
            ScoreDirection::HigherIsBetter,
        )],
        3,
    )
    .expect("all finite scores should be normalizable");

    assert_eq!(
        fused
            .iter()
            .map(|point| point.point_id())
            .collect::<Vec<_>>(),
        vec![3, 2, 1]
    );
    assert!(fused.iter().all(|point| point.score().is_finite()));
}

proptest! {
    #[test]
    fn one_branch_weighted_and_rrf_preserve_the_same_strict_order(
        scores in prop::collection::vec(-10_000i32..10_000, 1..64)
    ) {
        let mut scores = scores;
        scores.sort_unstable();
        scores.dedup();
        let weighted_points = scores
            .iter()
            .enumerate()
            .map(|(index, score)| scored(index as u64 + 1, f64::from(*score)))
            .collect::<Vec<_>>();
        let ranked_points = weighted_points
            .iter()
            .map(|candidate| RankedPoint::new(candidate.point_id()))
            .collect::<Vec<_>>();
        let weighted = weighted_fusion(
            &[WeightedBranch::new(
                &weighted_points,
                1.0,
                ScoreDirection::LowerIsBetter,
            )],
            usize::MAX,
        ).expect("strict finite scores should fuse");
        let rrf = reciprocal_rank_fusion(&[&ranked_points], RrfK::STANDARD, usize::MAX);

        prop_assert_eq!(
            weighted.iter().map(|point| point.point_id()).collect::<Vec<_>>(),
            rrf.iter().map(|point| point.point_id()).collect::<Vec<_>>(),
        );
    }
}
