//! Owned query port DTOs and execution outcomes.

use std::collections::BTreeMap;

use context_core::{PointId, SourceKey};

use crate::{BudgetUsage, QueryError, Result};

/// Candidate branch selected by application strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateBranch {
    /// Exact dense scoring.
    DenseExact,
    /// Approximate dense candidate generation.
    DenseAnn,
    /// PostgreSQL full-text candidate generation.
    FullText,
    /// Sparse candidate generation.
    Sparse,
    /// Multi-vector token candidate generation.
    MultiVector,
    /// Caller-provided candidate IDs.
    UserProvided,
}

/// Owned candidate produced by a candidate-source adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    point_id: PointId,
    score: f64,
    branch: CandidateBranch,
}

impl Candidate {
    /// Creates a candidate with a finite adapter score.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for a non-finite score.
    pub fn new(point_id: PointId, score: f64, branch: CandidateBranch) -> Result<Self> {
        if !score.is_finite() {
            return Err(QueryError::InvalidInput {
                field: "candidate_score",
                reason: "must be finite".to_owned(),
            });
        }
        Ok(Self {
            point_id,
            score,
            branch,
        })
    }

    /// Returns the logical point identifier.
    #[must_use]
    pub const fn point_id(&self) -> PointId {
        self.point_id
    }

    /// Returns the adapter score.
    #[must_use]
    pub const fn score(&self) -> f64 {
        self.score
    }

    /// Returns the producing branch.
    #[must_use]
    pub const fn branch(&self) -> CandidateBranch {
        self.branch
    }
}

/// One owned page from a candidate source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CandidatePage {
    candidates: Vec<Candidate>,
    scored_count: usize,
    expansion_count: usize,
    exhausted: bool,
    strategy: &'static str,
}

impl CandidatePage {
    /// Creates a candidate page.
    #[must_use]
    pub const fn new(candidates: Vec<Candidate>, exhausted: bool) -> Self {
        let scored_count = candidates.len();
        Self {
            candidates,
            scored_count,
            expansion_count: 0,
            exhausted,
            strategy: "candidate_source",
        }
    }

    /// Creates a candidate page with explicit bounded scoring work.
    #[must_use]
    pub const fn with_scored_count(
        candidates: Vec<Candidate>,
        scored_count: usize,
        exhausted: bool,
    ) -> Self {
        Self {
            candidates,
            scored_count,
            expansion_count: 0,
            exhausted,
            strategy: "candidate_source",
        }
    }

    /// Attaches a cardinality-bounded static serving strategy label.
    #[must_use]
    pub const fn with_strategy(mut self, strategy: &'static str) -> Self {
        self.strategy = strategy;
        self
    }

    /// Attaches the number of adaptive candidate expansions performed.
    #[must_use]
    pub const fn with_expansion_count(mut self, expansion_count: usize) -> Self {
        self.expansion_count = expansion_count;
        self
    }

    /// Returns owned candidates in source order.
    #[must_use]
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// Consumes the page and returns its candidates.
    #[must_use]
    pub fn into_candidates(self) -> Vec<Candidate> {
        self.candidates
    }

    /// Returns how many source candidates the adapter scored to produce this page.
    #[must_use]
    pub const fn scored_count(&self) -> usize {
        self.scored_count
    }

    /// Returns the number of adaptive candidate expansions performed.
    #[must_use]
    pub const fn expansion_count(&self) -> usize {
        self.expansion_count
    }

    /// Reports whether the source has no additional candidates.
    #[must_use]
    pub const fn exhausted(&self) -> bool {
        self.exhausted
    }

    /// Returns the adapter's static serving strategy label.
    #[must_use]
    pub const fn strategy(&self) -> &'static str {
        self.strategy
    }
}

/// Filter-derived logical candidate IDs.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FilterCandidateBatch {
    point_ids: Vec<PointId>,
    exhausted: bool,
}

impl FilterCandidateBatch {
    /// Creates a filter-candidate batch.
    #[must_use]
    pub const fn new(point_ids: Vec<PointId>, exhausted: bool) -> Self {
        Self {
            point_ids,
            exhausted,
        }
    }

    /// Returns filter-derived logical IDs.
    #[must_use]
    pub fn point_ids(&self) -> &[PointId] {
        &self.point_ids
    }

    /// Reports whether no additional filter candidates exist.
    #[must_use]
    pub const fn exhausted(&self) -> bool {
        self.exhausted
    }
}

/// Candidate rehydrated and rechecked against an authoritative source row.
#[derive(Clone, Debug, PartialEq)]
pub struct HydratedCandidate {
    point_id: PointId,
    source_key: SourceKey,
    score: f64,
}

impl HydratedCandidate {
    /// Creates a rechecked candidate with a finite final score.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for a non-finite score.
    pub fn new(point_id: PointId, source_key: SourceKey, score: f64) -> Result<Self> {
        if !score.is_finite() {
            return Err(QueryError::InvalidInput {
                field: "rechecked_score",
                reason: "must be finite".to_owned(),
            });
        }
        Ok(Self {
            point_id,
            source_key,
            score,
        })
    }

    /// Returns the logical point identifier.
    #[must_use]
    pub const fn point_id(&self) -> PointId {
        self.point_id
    }

    /// Returns the authoritative source key.
    #[must_use]
    pub const fn source_key(&self) -> &SourceKey {
        &self.source_key
    }

    /// Returns the final rechecked score.
    #[must_use]
    pub const fn score(&self) -> f64 {
        self.score
    }
}

/// Readiness reported before a candidate source performs work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceReadiness {
    /// Source is ready to serve the current query.
    Ready,
    /// Source will serve the query through its authoritative exact fallback.
    Exact,
    /// Source exists but its active generation is stale.
    RebuildRequired {
        /// Bounded diagnostic reason.
        reason: ReadinessReason,
    },
    /// Source cannot serve the query yet.
    NotReady {
        /// Bounded diagnostic reason.
        reason: ReadinessReason,
    },
}

/// Bounded source-readiness reason safe for telemetry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadinessReason {
    /// Adapter has not established readiness yet.
    Uninitialized,
    /// No active generation or index exists.
    GenerationMissing,
    /// Configuration changed after the active generation was built.
    ConfigurationChanged,
    /// Source metadata or artifact generation is stale.
    StaleGeneration,
    /// Selected source kind cannot serve this query shape.
    UnsupportedQuery,
    /// Source failed validation and requires repair/rebuild.
    ValidationFailed,
}

impl Default for SourceReadiness {
    fn default() -> Self {
        Self::NotReady {
            reason: ReadinessReason::Uninitialized,
        }
    }
}

/// Ordering direction for final rechecked scores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScoreOrder {
    /// Smaller distance values rank first.
    LowerIsBetter,
    /// Larger similarity or fusion scores rank first.
    HigherIsBetter,
}

/// Overall execution readiness state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionState {
    /// All selected sources were ready.
    Ready,
    /// A selected source requires rebuild before serving.
    RebuildRequired {
        /// Bounded diagnostic reason.
        reason: ReadinessReason,
    },
    /// A selected source was not ready.
    NotReady {
        /// Bounded diagnostic reason.
        reason: ReadinessReason,
    },
}

/// Terminal completion classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    /// Execution completed normally.
    Complete,
    /// Cooperative cancellation stopped execution at a port boundary.
    Cancelled,
    /// A result, candidate, recheck, stage, or expansion limit prevented a
    /// complete authoritative result.
    BudgetExhausted,
}

/// Logical execution stage for diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StageKind {
    /// Source readiness preflight.
    Readiness,
    /// Filter-candidate derivation.
    FilterCandidates,
    /// Candidate generation.
    Candidates,
    /// Authoritative source hydration and recheck.
    SourceRecheck,
    /// Multi-branch reciprocal-rank or weighted fusion.
    Fusion,
    /// Score threshold, weight, or formula transformation.
    ScoreTransform,
    /// Final deterministic score ordering and result limiting.
    Rerank,
}

/// Bounded diagnostic emitted after one stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StageDiagnostic {
    stage: StageKind,
    strategy: &'static str,
    input_count: usize,
    output_count: usize,
    reason: Option<ReadinessReason>,
}

impl StageDiagnostic {
    pub(crate) const fn new(
        stage: StageKind,
        strategy: &'static str,
        input_count: usize,
        output_count: usize,
        reason: Option<ReadinessReason>,
    ) -> Self {
        Self {
            stage,
            strategy,
            input_count,
            output_count,
            reason,
        }
    }

    /// Returns the logical stage.
    #[must_use]
    pub const fn stage(&self) -> StageKind {
        self.stage
    }

    /// Returns the bounded strategy label.
    #[must_use]
    pub const fn strategy(&self) -> &'static str {
        self.strategy
    }

    /// Returns the stage input count.
    #[must_use]
    pub const fn input_count(&self) -> usize {
        self.input_count
    }

    /// Returns the stage output count.
    #[must_use]
    pub const fn output_count(&self) -> usize {
        self.output_count
    }

    /// Returns an optional bounded reason.
    #[must_use]
    pub const fn reason(&self) -> Option<ReadinessReason> {
        self.reason
    }
}

/// Deterministic outcome returned by pure orchestration.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionOutcome {
    state: ExecutionState,
    completion: Completion,
    points: Vec<HydratedCandidate>,
    diagnostics: Vec<StageDiagnostic>,
    usage: BudgetUsage,
}

impl ExecutionOutcome {
    pub(crate) const fn new(
        state: ExecutionState,
        completion: Completion,
        points: Vec<HydratedCandidate>,
        diagnostics: Vec<StageDiagnostic>,
        usage: BudgetUsage,
    ) -> Self {
        Self {
            state,
            completion,
            points,
            diagnostics,
            usage,
        }
    }

    /// Returns readiness state.
    #[must_use]
    pub const fn state(&self) -> &ExecutionState {
        &self.state
    }

    /// Returns terminal completion.
    #[must_use]
    pub const fn completion(&self) -> Completion {
        self.completion
    }

    /// Returns deterministic final points.
    #[must_use]
    pub fn points(&self) -> &[HydratedCandidate] {
        &self.points
    }

    /// Returns stage diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &[StageDiagnostic] {
        &self.diagnostics
    }

    /// Returns bounded work usage.
    #[must_use]
    pub const fn usage(&self) -> BudgetUsage {
        self.usage
    }
}

pub(crate) fn deterministic_points(
    rows: Vec<HydratedCandidate>,
    limit: usize,
    order: ScoreOrder,
) -> Vec<HydratedCandidate> {
    let mut best = BTreeMap::<PointId, HydratedCandidate>::new();
    for row in rows {
        match best.get(&row.point_id()) {
            Some(existing) if score_is_better_or_equal(existing.score(), row.score(), order) => {}
            _ => {
                best.insert(row.point_id(), row);
            }
        }
    }
    let mut rows = best.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        let score_order = match order {
            ScoreOrder::LowerIsBetter => left.score().total_cmp(&right.score()),
            ScoreOrder::HigherIsBetter => right.score().total_cmp(&left.score()),
        };
        score_order.then_with(|| left.point_id().cmp(&right.point_id()))
    });
    rows.truncate(limit);
    rows
}

fn score_is_better_or_equal(existing: f64, candidate: f64, order: ScoreOrder) -> bool {
    match order {
        ScoreOrder::LowerIsBetter => existing.total_cmp(&candidate).is_le(),
        ScoreOrder::HigherIsBetter => existing.total_cmp(&candidate).is_ge(),
    }
}
