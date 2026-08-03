//! Owned query port DTOs and execution outcomes.

use std::collections::BTreeMap;

use context_core::{
    Completion, ConfigurationRevision, GenerationId, OccurrenceId, PointId, ProfileId,
    ReadinessReason, ScoreOrder, SourceAuthority, SourceKey, SourceVersion,
};

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

impl CandidateBranch {
    /// Returns the stable numeric registration code for occurrence identities.
    #[must_use]
    pub const fn stable_code(self) -> u8 {
        match self {
            Self::DenseExact => 0,
            Self::DenseAnn => 1,
            Self::FullText => 2,
            Self::Sparse => 3,
            Self::MultiVector => 4,
            Self::UserProvided => 5,
        }
    }

    /// Returns the bounded stable diagnostic name for this branch.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::DenseExact => "dense_exact",
            Self::DenseAnn => "dense_ann",
            Self::FullText => "full_text",
            Self::Sparse => "sparse",
            Self::MultiVector => "multi_vector",
            Self::UserProvided => "user_provided",
        }
    }
}

/// Physical or logical source that produced a candidate occurrence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateSourceKind {
    /// Authoritative exact source-row scan.
    Exact,
    /// HNSW graph or delta candidate source.
    Hnsw,
    /// IVFFlat candidate source.
    IvfFlat,
    /// PostgreSQL full-text candidate source.
    FullText,
    /// Sparse exact or sparse-index candidate source.
    Sparse,
    /// Multi-vector token candidate source.
    MultiVector,
    /// Caller-provided logical identifiers.
    UserProvided,
    /// Topology expansion candidate source.
    Topology,
}

impl CandidateSourceKind {
    /// Returns the stable numeric registration code for occurrence identities.
    #[must_use]
    pub const fn stable_code(self) -> u8 {
        match self {
            Self::Exact => 0,
            Self::Hnsw => 1,
            Self::IvfFlat => 2,
            Self::FullText => 3,
            Self::Sparse => 4,
            Self::MultiVector => 5,
            Self::UserProvided => 6,
            Self::Topology => 7,
        }
    }

    /// Returns the bounded stable diagnostic name for this source kind.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Hnsw => "hnsw",
            Self::IvfFlat => "ivf_flat",
            Self::FullText => "full_text",
            Self::Sparse => "sparse",
            Self::MultiVector => "multi_vector",
            Self::UserProvided => "user_provided",
            Self::Topology => "topology",
        }
    }
}

/// Typed provenance attached to one candidate occurrence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CandidateProvenance {
    occurrence_id: OccurrenceId,
    branch: CandidateBranch,
    source: CandidateSourceKind,
    score_order: ScoreOrder,
    authority: SourceAuthority,
    generation: Option<GenerationId>,
    configuration: Option<ConfigurationRevision>,
    profile: Option<ProfileId>,
    source_version: Option<SourceVersion>,
}

impl CandidateProvenance {
    /// Creates required candidate provenance before optional revision IDs are attached.
    #[must_use]
    pub const fn new(
        occurrence_id: OccurrenceId,
        branch: CandidateBranch,
        source: CandidateSourceKind,
        score_order: ScoreOrder,
        authority: SourceAuthority,
    ) -> Self {
        Self {
            occurrence_id,
            branch,
            source,
            score_order,
            authority,
            generation: None,
            configuration: None,
            profile: None,
            source_version: None,
        }
    }

    /// Attaches the artifact generation used by the candidate source.
    #[must_use]
    pub const fn with_generation(mut self, generation: GenerationId) -> Self {
        self.generation = Some(generation);
        self
    }

    /// Attaches the immutable retrieval configuration revision.
    #[must_use]
    pub const fn with_configuration(mut self, configuration: ConfigurationRevision) -> Self {
        self.configuration = Some(configuration);
        self
    }

    /// Attaches the vector or model profile identity.
    #[must_use]
    pub const fn with_profile(mut self, profile: ProfileId) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Attaches the authoritative source version observed by the adapter.
    #[must_use]
    pub const fn with_source_version(mut self, source_version: SourceVersion) -> Self {
        self.source_version = Some(source_version);
        self
    }

    /// Returns the stable occurrence identity.
    #[must_use]
    pub const fn occurrence_id(self) -> OccurrenceId {
        self.occurrence_id
    }

    /// Returns the query-owned branch identity.
    #[must_use]
    pub const fn branch(self) -> CandidateBranch {
        self.branch
    }

    /// Returns the candidate source kind.
    #[must_use]
    pub const fn source(self) -> CandidateSourceKind {
        self.source
    }

    /// Returns the candidate score ordering.
    #[must_use]
    pub const fn score_order(self) -> ScoreOrder {
        self.score_order
    }

    /// Returns the source authority classification.
    #[must_use]
    pub const fn authority(self) -> SourceAuthority {
        self.authority
    }

    /// Returns the artifact generation, when applicable.
    #[must_use]
    pub const fn generation(self) -> Option<GenerationId> {
        self.generation
    }

    /// Returns the configuration revision, when applicable.
    #[must_use]
    pub const fn configuration(self) -> Option<ConfigurationRevision> {
        self.configuration
    }

    /// Returns the model or vector profile, when applicable.
    #[must_use]
    pub const fn profile(self) -> Option<ProfileId> {
        self.profile
    }

    /// Returns the authoritative source version, when applicable.
    #[must_use]
    pub const fn source_version(self) -> Option<SourceVersion> {
        self.source_version
    }
}

/// Fixed-width, content-free diagnostics for one candidate occurrence.
///
/// The integer fields bound telemetry cardinality and prevent adapters from
/// attaching row contents or unbounded diagnostic strings to candidates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CandidateDiagnostics {
    source_rank: u32,
    work_units: u32,
}

impl CandidateDiagnostics {
    /// Creates bounded candidate diagnostics.
    #[must_use]
    pub const fn new(source_rank: u32, work_units: u32) -> Self {
        Self {
            source_rank,
            work_units,
        }
    }

    /// Returns the candidate's zero-based rank at its source.
    #[must_use]
    pub const fn source_rank(self) -> u32 {
        self.source_rank
    }

    /// Returns source-defined bounded work units spent on the occurrence.
    #[must_use]
    pub const fn work_units(self) -> u32 {
        self.work_units
    }
}

/// Owned candidate produced by a candidate-source adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    point_id: PointId,
    approximate_score: f64,
    exact_score: Option<f64>,
    provenance: CandidateProvenance,
    diagnostics: CandidateDiagnostics,
}

impl Candidate {
    /// Creates a candidate with a finite adapter score.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for a non-finite score.
    pub fn new(
        point_id: PointId,
        approximate_score: f64,
        provenance: CandidateProvenance,
    ) -> Result<Self> {
        if !approximate_score.is_finite() {
            return Err(QueryError::InvalidInput {
                field: "candidate_score",
                reason: "must be finite".to_owned(),
            });
        }
        Ok(Self {
            point_id,
            approximate_score,
            exact_score: None,
            provenance,
            diagnostics: CandidateDiagnostics::default(),
        })
    }

    /// Attaches a finite authoritative score already computed by the source.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when `exact_score` is not finite.
    pub fn with_exact_score(mut self, exact_score: f64) -> Result<Self> {
        if !exact_score.is_finite() {
            return Err(QueryError::InvalidInput {
                field: "candidate_exact_score",
                reason: "must be finite".to_owned(),
            });
        }
        self.exact_score = Some(exact_score);
        Ok(self)
    }

    /// Attaches fixed-width, content-free source diagnostics.
    #[must_use]
    pub const fn with_diagnostics(mut self, diagnostics: CandidateDiagnostics) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    /// Returns the logical point identifier.
    #[must_use]
    pub const fn point_id(&self) -> PointId {
        self.point_id
    }

    /// Returns the adapter score.
    #[must_use]
    pub const fn approximate_score(&self) -> f64 {
        self.approximate_score
    }

    /// Returns an authoritative score already computed by the source.
    #[must_use]
    pub const fn exact_score(&self) -> Option<f64> {
        self.exact_score
    }

    /// Returns typed candidate provenance.
    #[must_use]
    pub const fn provenance(&self) -> CandidateProvenance {
        self.provenance
    }

    /// Returns fixed-width source diagnostics.
    #[must_use]
    pub const fn diagnostics(&self) -> CandidateDiagnostics {
        self.diagnostics
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

impl Default for SourceReadiness {
    fn default() -> Self {
        Self::NotReady {
            reason: ReadinessReason::Uninitialized,
        }
    }
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
        let score_order = order.compare(left.score(), right.score());
        score_order.then_with(|| left.point_id().cmp(&right.point_id()))
    });
    rows.truncate(limit);
    rows
}

fn score_is_better_or_equal(existing: f64, candidate: f64, order: ScoreOrder) -> bool {
    order.compare(existing, candidate).is_le()
}
