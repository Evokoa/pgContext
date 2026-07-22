//! Retrieval composition for dense, sparse, and full-text candidates.
//!
//! This crate combines already-hydrated candidate scores and returns stable
//! ranked outputs. It has no PostgreSQL dependency.

use std::collections::{BTreeMap, BTreeSet};

/// Tunable constant used by reciprocal rank fusion.
///
/// Larger values flatten the contribution of each branch, while smaller values
/// favor candidates near the top of an individual branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RrfK(u32);

impl RrfK {
    /// Conventional RRF constant used by many hybrid retrieval systems.
    pub const STANDARD: Self = Self(60);

    /// Creates a reciprocal rank fusion constant.
    ///
    /// Returns `None` for zero because the RRF denominator is `k + rank`.
    #[must_use]
    pub const fn new(value: u32) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the raw fusion constant value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A point from a ranked retrieval branch.
///
/// Branch scores are intentionally omitted because RRF uses only the order of
/// each branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RankedPoint {
    point_id: u64,
}

impl RankedPoint {
    /// Creates a ranked point reference.
    #[must_use]
    pub const fn new(point_id: u64) -> Self {
        Self { point_id }
    }

    /// Returns the stable point identifier.
    #[must_use]
    pub const fn point_id(self) -> u64 {
        self.point_id
    }
}

/// Hydrated candidate crossing from an adapter into hybrid fusion.
///
/// The point identifier is the stable fusion key. `branch_score` carries the
/// original branch's score for diagnostics or later score-normalization work;
/// reciprocal rank fusion still uses only candidate order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BranchCandidate {
    point_id: u64,
    branch_score: Option<f64>,
}

impl BranchCandidate {
    /// Creates a candidate without an adapter-specific score.
    #[must_use]
    pub const fn new(point_id: u64) -> Self {
        Self {
            point_id,
            branch_score: None,
        }
    }

    /// Creates a candidate with an adapter-specific score.
    #[must_use]
    pub const fn with_score(point_id: u64, branch_score: f64) -> Self {
        Self {
            point_id,
            branch_score: Some(branch_score),
        }
    }

    /// Returns the stable point identifier.
    #[must_use]
    pub const fn point_id(self) -> u64 {
        self.point_id
    }

    /// Returns the optional adapter-specific score.
    #[must_use]
    pub const fn branch_score(self) -> Option<f64> {
        self.branch_score
    }

    const fn ranked_point(self) -> RankedPoint {
        RankedPoint::new(self.point_id)
    }
}

/// Retrieval branch that produced a candidate batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateBranch {
    /// Dense exact table-backed retrieval.
    DenseExact,
    /// Dense approximate retrieval.
    DenseAnn,
    /// PostgreSQL full-text retrieval.
    FullText,
    /// Sparse-vector retrieval that is planned but not released yet.
    SparsePlanned,
    /// Caller-provided candidate point batch.
    UserProvided,
}

/// Ordered candidates from one retrieval branch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CandidateBatch {
    branch: CandidateBranch,
    points: Vec<RankedPoint>,
}

impl CandidateBatch {
    /// Creates an ordered branch candidate batch.
    #[must_use]
    pub fn new(branch: CandidateBranch, points: Vec<RankedPoint>) -> Self {
        Self { branch, points }
    }

    /// Creates an ordered branch candidate batch from hydrated adapter output.
    #[must_use]
    pub fn from_candidates(branch: CandidateBranch, candidates: Vec<BranchCandidate>) -> Self {
        let points = candidates
            .into_iter()
            .map(BranchCandidate::ranked_point)
            .collect();
        Self { branch, points }
    }

    /// Returns the branch kind that produced this batch.
    #[must_use]
    pub const fn branch(&self) -> CandidateBranch {
        self.branch
    }

    /// Returns the ordered points in this batch.
    #[must_use]
    pub fn points(&self) -> &[RankedPoint] {
        &self.points
    }
}

/// A point produced by reciprocal rank fusion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FusedPoint {
    point_id: u64,
    score: f64,
}

/// Ordering semantics for one weighted score branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScoreDirection {
    /// Larger adapter scores are better.
    HigherIsBetter,
    /// Smaller adapter scores are better, as with distances.
    LowerIsBetter,
}

/// One scored branch participating in normalized weighted fusion.
#[derive(Clone, Copy, Debug)]
pub struct WeightedBranch<'a> {
    candidates: &'a [BranchCandidate],
    weight: f64,
    direction: ScoreDirection,
}

impl<'a> WeightedBranch<'a> {
    /// Creates a weighted branch.
    #[must_use]
    pub const fn new(
        candidates: &'a [BranchCandidate],
        weight: f64,
        direction: ScoreDirection,
    ) -> Self {
        Self {
            candidates,
            weight,
            direction,
        }
    }
}

/// Invalid input to normalized weighted fusion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WeightedFusionError {
    /// A branch weight is negative or non-finite.
    InvalidWeight,
    /// Every supplied branch has zero weight.
    ZeroTotalWeight,
    /// A candidate does not carry the score required for weighted fusion.
    MissingScore,
    /// A candidate carries a non-finite adapter score.
    InvalidScore,
}

impl FusedPoint {
    /// Creates a fused point with its accumulated RRF score.
    #[must_use]
    pub const fn new(point_id: u64, score: f64) -> Self {
        Self { point_id, score }
    }

    /// Returns the stable point identifier.
    #[must_use]
    pub const fn point_id(self) -> u64 {
        self.point_id
    }

    /// Returns the accumulated reciprocal rank fusion score.
    #[must_use]
    pub const fn score(self) -> f64 {
        self.score
    }
}

/// Fuses ordered retrieval branches using reciprocal rank fusion.
///
/// Ranks are one-based inside each branch. A point contributes at most once per
/// branch; if a branch repeats the same point identifier, only the first
/// occurrence is counted. Results are ordered by descending fused score, then by
/// ascending point identifier to keep ties deterministic. A zero `limit`
/// returns an empty result.
#[must_use]
pub fn reciprocal_rank_fusion(
    branches: &[&[RankedPoint]],
    k: RrfK,
    limit: usize,
) -> Vec<FusedPoint> {
    if limit == 0 {
        return Vec::new();
    }

    let mut scores = BTreeMap::<u64, f64>::new();

    for branch in branches {
        let mut seen_in_branch = BTreeSet::new();
        for (index, point) in branch.iter().enumerate() {
            if !seen_in_branch.insert(point.point_id()) {
                continue;
            }

            let Ok(rank) = u32::try_from(index + 1) else {
                continue;
            };
            let contribution = 1.0 / (f64::from(k.get()) + f64::from(rank));
            *scores.entry(point.point_id()).or_default() += contribution;
        }
    }

    let mut fused = scores
        .into_iter()
        .map(|(point_id, score)| FusedPoint::new(point_id, score))
        .collect::<Vec<_>>();
    fused.sort_by(|left, right| {
        right
            .score()
            .total_cmp(&left.score())
            .then_with(|| left.point_id().cmp(&right.point_id()))
    });
    fused.truncate(limit);
    fused
}

/// Fuses typed candidate batches using reciprocal rank fusion.
#[must_use]
pub fn reciprocal_rank_fusion_batches(
    batches: &[CandidateBatch],
    k: RrfK,
    limit: usize,
) -> Vec<FusedPoint> {
    let branches = batches
        .iter()
        .map(CandidateBatch::points)
        .collect::<Vec<_>>();
    reciprocal_rank_fusion(&branches, k, limit)
}

/// Fuses scored branches using a normalized weighted linear combination.
///
/// Scores are min-max normalized independently inside each branch. Distance
/// branches are inverted so `1.0` always represents that branch's best score.
/// A constant-score branch contributes `1.0` for every distinct candidate,
/// preserving its weight instead of discarding the branch. Duplicate point IDs
/// contribute at most once per branch, using their best normalized score.
/// Branch weights are normalized by the sum of positive weights. Results sort
/// by descending fused score and then ascending point ID.
///
/// # Errors
///
/// Returns [`WeightedFusionError`] for invalid weights, a zero total weight, or
/// missing/non-finite candidate scores.
pub fn weighted_fusion(
    branches: &[WeightedBranch<'_>],
    limit: usize,
) -> Result<Vec<FusedPoint>, WeightedFusionError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    if branches
        .iter()
        .any(|branch| !branch.weight.is_finite() || branch.weight < 0.0)
    {
        return Err(WeightedFusionError::InvalidWeight);
    }
    let total_weight = branches.iter().map(|branch| branch.weight).sum::<f64>();
    if !total_weight.is_finite() || total_weight <= 0.0 {
        return Err(WeightedFusionError::ZeroTotalWeight);
    }

    let mut fused_scores = BTreeMap::<u64, f64>::new();
    for branch in branches.iter().filter(|branch| branch.weight > 0.0) {
        let mut minimum = f64::INFINITY;
        let mut maximum = f64::NEG_INFINITY;
        for candidate in branch.candidates {
            let score = candidate
                .branch_score()
                .ok_or(WeightedFusionError::MissingScore)?;
            if !score.is_finite() {
                return Err(WeightedFusionError::InvalidScore);
            }
            minimum = minimum.min(score);
            maximum = maximum.max(score);
        }
        if branch.candidates.is_empty() {
            continue;
        }

        let branch_weight = branch.weight / total_weight;
        let score_range = maximum - minimum;
        let mut best_in_branch = BTreeMap::<u64, f64>::new();
        for candidate in branch.candidates {
            let score = candidate
                .branch_score()
                .ok_or(WeightedFusionError::MissingScore)?;
            let normalized = if score_range == 0.0 {
                1.0
            } else {
                match branch.direction {
                    ScoreDirection::HigherIsBetter => (score - minimum) / score_range,
                    ScoreDirection::LowerIsBetter => (maximum - score) / score_range,
                }
            };
            best_in_branch
                .entry(candidate.point_id())
                .and_modify(|existing| *existing = existing.max(normalized))
                .or_insert(normalized);
        }
        for (point_id, normalized) in best_in_branch {
            *fused_scores.entry(point_id).or_default() += normalized * branch_weight;
        }
    }

    let mut fused = fused_scores
        .into_iter()
        .map(|(point_id, score)| FusedPoint::new(point_id, score))
        .collect::<Vec<_>>();
    fused.sort_by(|left, right| {
        right
            .score()
            .total_cmp(&left.score())
            .then_with(|| left.point_id().cmp(&right.point_id()))
    });
    fused.truncate(limit);
    Ok(fused)
}

/// Returns the package version compiled into this crate.
#[must_use]
pub const fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
