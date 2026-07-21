//! Reciprocal rank fusion known-answer tests.

use context_hybrid::{
    BranchCandidate, CandidateBatch, CandidateBranch, RankedPoint, RrfK, reciprocal_rank_fusion,
    reciprocal_rank_fusion_batches,
};

fn point_ids(points: &[context_hybrid::FusedPoint]) -> Vec<u64> {
    points
        .iter()
        .map(|point| point.point_id())
        .collect::<Vec<_>>()
}

#[test]
fn reciprocal_rank_fusion_matches_known_answer_ordering() {
    let dense = [
        RankedPoint::new(10),
        RankedPoint::new(20),
        RankedPoint::new(30),
    ];
    let full_text = [
        RankedPoint::new(30),
        RankedPoint::new(10),
        RankedPoint::new(40),
    ];

    let fused = reciprocal_rank_fusion(&[&dense, &full_text], RrfK::STANDARD, 10);

    assert_eq!(point_ids(&fused), vec![10, 30, 20, 40]);
    assert_eq!(
        fused.iter().map(|point| point.score()).collect::<Vec<_>>(),
        vec![
            1.0 / 61.0 + 1.0 / 62.0,
            1.0 / 63.0 + 1.0 / 61.0,
            1.0 / 62.0,
            1.0 / 63.0,
        ]
    );
}

#[test]
fn reciprocal_rank_fusion_limits_results_after_sorting() {
    let dense = [
        RankedPoint::new(10),
        RankedPoint::new(20),
        RankedPoint::new(30),
    ];
    let full_text = [
        RankedPoint::new(30),
        RankedPoint::new(10),
        RankedPoint::new(40),
    ];

    let fused = reciprocal_rank_fusion(&[&dense, &full_text], RrfK::STANDARD, 2);

    assert_eq!(point_ids(&fused), vec![10, 30]);
}

#[test]
fn reciprocal_rank_fusion_breaks_score_ties_by_point_id() {
    let dense = [RankedPoint::new(1), RankedPoint::new(2)];
    let full_text = [RankedPoint::new(2), RankedPoint::new(1)];

    let fused = reciprocal_rank_fusion(&[&dense, &full_text], RrfK::STANDARD, 10);

    assert_eq!(point_ids(&fused), vec![1, 2]);
    assert_eq!(
        fused.iter().map(|point| point.score()).collect::<Vec<_>>(),
        vec![1.0 / 61.0 + 1.0 / 62.0, 1.0 / 61.0 + 1.0 / 62.0]
    );
}

#[test]
fn reciprocal_rank_fusion_counts_first_branch_occurrence_once() {
    let dense = [
        RankedPoint::new(7),
        RankedPoint::new(7),
        RankedPoint::new(9),
    ];

    let fused = reciprocal_rank_fusion(&[&dense], RrfK::STANDARD, 10);

    assert_eq!(point_ids(&fused), vec![7, 9]);
    assert_eq!(
        fused.iter().map(|point| point.score()).collect::<Vec<_>>(),
        vec![1.0 / 61.0, 1.0 / 63.0]
    );
}

#[test]
fn reciprocal_rank_fusion_zero_limit_returns_empty_result() {
    let dense = [RankedPoint::new(1)];

    let fused = reciprocal_rank_fusion(&[&dense], RrfK::STANDARD, 0);

    assert_eq!(fused, Vec::new());
}

#[test]
fn reciprocal_rank_fusion_rejects_zero_k() {
    assert_eq!(RrfK::new(0), None);
}

#[test]
fn candidate_batch_carries_branch_identity_to_fusion() {
    let dense = CandidateBatch::new(
        CandidateBranch::DenseExact,
        vec![RankedPoint::new(10), RankedPoint::new(20)],
    );
    let full_text = CandidateBatch::new(
        CandidateBranch::FullText,
        vec![RankedPoint::new(20), RankedPoint::new(30)],
    );

    assert_eq!(dense.branch(), CandidateBranch::DenseExact);
    assert_eq!(full_text.branch(), CandidateBranch::FullText);

    let fused = reciprocal_rank_fusion_batches(&[dense, full_text], RrfK::STANDARD, 10);

    assert_eq!(point_ids(&fused), vec![20, 10, 30]);
}

#[test]
fn branch_candidate_preserves_adapter_score_before_fusion() {
    let candidate = BranchCandidate::with_score(42, 0.125);

    assert_eq!(candidate.point_id(), 42);
    assert_eq!(candidate.branch_score(), Some(0.125));
}

#[test]
fn candidate_batch_accepts_hydrated_branch_candidates() {
    let batch = CandidateBatch::from_candidates(
        CandidateBranch::DenseAnn,
        vec![BranchCandidate::with_score(3, 0.1), BranchCandidate::new(5)],
    );

    assert_eq!(batch.branch(), CandidateBranch::DenseAnn);
    assert_eq!(batch.points(), &[RankedPoint::new(3), RankedPoint::new(5)]);
}
