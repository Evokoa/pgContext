use std::collections::BTreeSet;

use context_core::{ContextError, DistanceMetric, policy::MAX_VECTOR_DIMENSIONS};

/// Errors returned by pure inverted-file operations.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum IvfError {
    /// A configuration value violates a bounded IVF invariant.
    #[error("invalid IVFFlat parameter {parameter}: {value}")]
    InvalidConfig {
        /// Stable parameter name.
        parameter: &'static str,
        /// Rejected value.
        value: usize,
    },
    /// A vector or persisted section has the wrong dimension.
    #[error("IVFFlat dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch {
        /// Generation dimension.
        expected: usize,
        /// Observed dimension.
        actual: usize,
    },
    /// Centroid and posting sections disagree about list count.
    #[error("IVFFlat list count mismatch: centroids={centroids}, postings={postings}")]
    ListCountMismatch {
        /// Centroid count.
        centroids: usize,
        /// Posting-list count.
        postings: usize,
    },
    /// A point identifier occurs more than once in one generation.
    #[error("duplicate IVFFlat point id {point_id:?}")]
    DuplicatePointId {
        /// Duplicate stable point identifier.
        point_id: IvfPointId,
    },
    /// The caller-supplied candidate budget cannot cover selected postings.
    #[error("IVFFlat candidate budget {budget} exhausted after {visited} postings")]
    CandidateBudgetExhausted {
        /// Configured hard budget.
        budget: usize,
        /// Posting count that would exceed it.
        visited: usize,
    },
    /// A read port returned malformed generation data.
    #[error("corrupt IVFFlat generation: {reason}")]
    CorruptGeneration {
        /// Stable corruption reason.
        reason: &'static str,
    },
    /// Cooperative cancellation stopped bounded work.
    #[error("IVFFlat operation cancelled")]
    Cancelled,
    /// Canonical metric evaluation rejected the source values.
    #[error("IVFFlat metric evaluation failed: {0}")]
    Metric(#[from] context_core::Error),
}

impl IvfError {
    /// Returns the stable adapter-facing error category.
    #[must_use]
    pub fn context_error(&self) -> ContextError {
        match self {
            Self::DimensionMismatch { .. } => ContextError::DimensionMismatch,
            Self::Metric(error) => error.context_error(),
            Self::CandidateBudgetExhausted { .. } | Self::Cancelled => {
                ContextError::RecallBudgetExceeded
            }
            Self::CorruptGeneration { .. }
            | Self::ListCountMismatch { .. }
            | Self::DuplicatePointId { .. } => ContextError::IndexCorrupt,
            Self::InvalidConfig { .. } => ContextError::InvalidFilter,
        }
    }
}

/// Stable zero-based IVF list identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IvfListId(usize);

impl IvfListId {
    /// Creates a list identity.
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the zero-based list index.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Stable non-zero point identity stored in IVF postings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IvfPointId(u64);

impl IvfPointId {
    /// Creates an identity, rejecting the reserved zero value.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

macro_rules! nonzero_budget {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name(usize);

        impl $name {
            /// Creates a bounded-work value, rejecting zero.
            #[must_use]
            pub const fn new(value: usize) -> Option<Self> {
                if value == 0 { None } else { Some(Self(value)) }
            }

            /// Returns the configured budget.
            #[must_use]
            pub const fn get(self) -> usize {
                self.0
            }
        }
    };
}

nonzero_budget!(IvfProbeBudget, "Maximum IVF lists visited by one search.");
nonzero_budget!(
    IvfCandidateBudget,
    "Maximum IVF postings scored by one search."
);

/// Half-open range in centroid-distance order visited by one search round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IvfProbeWindow {
    start: usize,
    end: usize,
}

impl IvfProbeWindow {
    /// Creates a non-empty half-open probe window.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Option<Self> {
        if start < end {
            Some(Self { start, end })
        } else {
            None
        }
    }

    /// Returns the first centroid-distance rank included by the round.
    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    /// Returns the exclusive centroid-distance rank ending the round.
    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }
}

/// Ordering promise made while widening post-filtered IVF searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IvfIterativePolicy {
    /// Re-sort the complete bounded candidate set before returning rows.
    StrictOrder,
    /// Permit adapters to return later probe rounds without global reordering.
    RelaxedOrder,
}

/// Validated IVF construction and search policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IvfConfig {
    lists: usize,
    probes: usize,
    max_probes: usize,
    training_sample_size: usize,
    iteration_budget: usize,
    seed: u64,
    iterative_policy: IvfIterativePolicy,
}

impl IvfConfig {
    /// Creates a validated IVF policy.
    ///
    /// # Errors
    ///
    /// Returns [`IvfError::InvalidConfig`] when any budget is zero, the probe
    /// bounds are inconsistent, or the training sample cannot cover every
    /// list.
    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor validates one atomic IVF policy"
    )]
    pub const fn new(
        lists: usize,
        probes: usize,
        max_probes: usize,
        training_sample_size: usize,
        iteration_budget: usize,
        seed: u64,
        iterative_policy: IvfIterativePolicy,
    ) -> Result<Self, IvfError> {
        if lists == 0 {
            return Err(IvfError::InvalidConfig {
                parameter: "lists",
                value: lists,
            });
        }
        if probes == 0 || probes > max_probes {
            return Err(IvfError::InvalidConfig {
                parameter: "probes",
                value: probes,
            });
        }
        if max_probes == 0 || max_probes > lists {
            return Err(IvfError::InvalidConfig {
                parameter: "max_probes",
                value: max_probes,
            });
        }
        if training_sample_size < lists {
            return Err(IvfError::InvalidConfig {
                parameter: "training_sample_size",
                value: training_sample_size,
            });
        }
        if iteration_budget == 0 {
            return Err(IvfError::InvalidConfig {
                parameter: "iteration_budget",
                value: iteration_budget,
            });
        }
        Ok(Self {
            lists,
            probes,
            max_probes,
            training_sample_size,
            iteration_budget,
            seed,
            iterative_policy,
        })
    }

    /// Returns the generation list count.
    #[must_use]
    pub const fn lists(self) -> usize {
        self.lists
    }
    /// Returns the first search-round probe count.
    #[must_use]
    pub const fn probes(self) -> usize {
        self.probes
    }
    /// Returns the maximum widening probe count.
    #[must_use]
    pub const fn max_probes(self) -> usize {
        self.max_probes
    }
    /// Returns the deterministic training sample size.
    #[must_use]
    pub const fn training_sample_size(self) -> usize {
        self.training_sample_size
    }
    /// Returns the k-means iteration cap.
    #[must_use]
    pub const fn iteration_budget(self) -> usize {
        self.iteration_budget
    }
    /// Returns the deterministic seed.
    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }
    /// Returns the widening ordering policy.
    #[must_use]
    pub const fn iterative_policy(self) -> IvfIterativePolicy {
        self.iterative_policy
    }
}

/// One owned in-memory posting used by builders and tests.
#[derive(Debug, Clone, PartialEq)]
pub struct IvfPosting {
    point_id: IvfPointId,
    vector: Vec<f32>,
}

impl IvfPosting {
    /// Creates a posting after validating finite, non-empty coordinates.
    ///
    /// # Errors
    ///
    /// Returns [`IvfError::Metric`] when the vector is empty, non-finite, or
    /// exceeds the canonical vector-dimension policy.
    pub fn new(point_id: IvfPointId, vector: Vec<f32>) -> Result<Self, IvfError> {
        let vector = context_core::DenseVector::new(vector)?.into_values();
        Ok(Self { point_id, vector })
    }

    /// Returns the stable point identity.
    #[must_use]
    pub const fn point_id(&self) -> IvfPointId {
        self.point_id
    }
    /// Returns the full-precision derived vector.
    #[must_use]
    pub fn vector(&self) -> &[f32] {
        &self.vector
    }
}

/// Borrowed posting returned by an IVF read port.
#[derive(Debug, Clone, Copy)]
pub struct IvfPostingRef<'a> {
    point_id: IvfPointId,
    payload: IvfPostingPayload<'a>,
}

/// Representation-neutral posting payload borrowed by a prepared scorer.
#[derive(Debug, Clone, Copy)]
pub enum IvfPostingPayload<'a> {
    /// Full-precision canonical coordinates.
    Dense(&'a [f32]),
    /// A codec-bound compact posting code.
    Encoded(&'a [u8]),
}

impl<'a> IvfPostingRef<'a> {
    /// Creates a full-precision posting reference.
    #[must_use]
    pub const fn dense(point_id: IvfPointId, vector: &'a [f32]) -> Self {
        Self {
            point_id,
            payload: IvfPostingPayload::Dense(vector),
        }
    }

    /// Creates a compact codec-bound posting reference.
    #[must_use]
    pub const fn encoded(point_id: IvfPointId, code: &'a [u8]) -> Self {
        Self {
            point_id,
            payload: IvfPostingPayload::Encoded(code),
        }
    }

    /// Returns the stable point identity.
    #[must_use]
    pub const fn point_id(self) -> IvfPointId {
        self.point_id
    }
    /// Returns the borrowed full-precision values.
    #[must_use]
    pub const fn payload(self) -> IvfPostingPayload<'a> {
        self.payload
    }
}

/// Consumer-owned centroid read port.
pub trait IvfCentroidRead {
    /// Returns the canonical generation metric.
    fn metric(&self) -> DistanceMetric;
    /// Returns the vector dimension.
    fn dimensions(&self) -> usize;
    /// Returns the number of centroids.
    fn centroid_count(&self) -> usize;
    /// Returns one centroid, or `None` for a corrupt identifier.
    fn centroid(&self, list_id: IvfListId) -> Option<&[f32]>;
}

/// Consumer-owned posting-list read port.
pub trait IvfPostingRead {
    /// Returns a bounded list length.
    ///
    /// # Errors
    ///
    /// Returns [`IvfError::CorruptGeneration`] when `list_id` is not present
    /// or the backing generation cannot be decoded safely.
    fn list_len(&self, list_id: IvfListId) -> Result<usize, IvfError>;
    /// Returns one bounded posting reference.
    ///
    /// # Errors
    ///
    /// Returns [`IvfError::CorruptGeneration`] when the list or offset is not
    /// present or the backing generation cannot be decoded safely.
    fn posting(&self, list_id: IvfListId, offset: usize) -> Result<IvfPostingRef<'_>, IvfError>;
}

/// Optional source/filter eligibility port.
pub trait IvfCandidateMask {
    /// Returns whether a source point may enter the bounded result set.
    fn eligible(&self, point_id: IvfPointId) -> bool;
}

impl<F> IvfCandidateMask for F
where
    F: Fn(IvfPointId) -> bool,
{
    fn eligible(&self, point_id: IvfPointId) -> bool {
        self(point_id)
    }
}

/// Cooperative interruption port.
pub trait IvfCancellation {
    /// Returns true when the caller requests cancellation.
    fn cancelled(&self) -> bool;
}

impl<F> IvfCancellation for F
where
    F: Fn() -> bool,
{
    fn cancelled(&self) -> bool {
        self()
    }
}

/// Cancellation implementation that never interrupts work.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeverCancelIvf;

impl IvfCancellation for NeverCancelIvf {
    fn cancelled(&self) -> bool {
        false
    }
}

/// Allocation-free prepared posting scorer.
pub trait IvfScorer {
    /// Scores one posting in canonical metric order.
    ///
    /// # Errors
    ///
    /// Returns [`IvfError::Metric`] when the posting and query cannot be
    /// evaluated under the generation metric.
    fn score(&self, posting: IvfPostingRef<'_>) -> Result<f32, IvfError>;
}

struct DenseScorer<'a> {
    metric: DistanceMetric,
    query: &'a [f32],
}

impl IvfScorer for DenseScorer<'_> {
    fn score(&self, posting: IvfPostingRef<'_>) -> Result<f32, IvfError> {
        let IvfPostingPayload::Dense(vector) = posting.payload() else {
            return Err(IvfError::CorruptGeneration {
                reason: "dense scorer received an encoded posting",
            });
        };
        Ok(self.metric.distance_slices(self.query, vector)?)
    }
}

/// Validated in-memory implementation of the pure IVF read ports.
#[derive(Debug, Clone)]
pub struct InMemoryIvfIndex {
    metric: DistanceMetric,
    dimensions: usize,
    centroids: Vec<Vec<f32>>,
    postings: Vec<Vec<IvfPosting>>,
}

impl InMemoryIvfIndex {
    /// Creates a generation and validates all cross-section invariants.
    ///
    /// # Errors
    ///
    /// Rejects empty or ragged centroids, dimensions above the canonical
    /// policy, non-finite values, list-count mismatches, and duplicate point
    /// identities.
    pub fn new(
        metric: DistanceMetric,
        centroids: Vec<Vec<f32>>,
        postings: Vec<Vec<IvfPosting>>,
    ) -> Result<Self, IvfError> {
        let Some(first) = centroids.first() else {
            return Err(IvfError::InvalidConfig {
                parameter: "lists",
                value: 0,
            });
        };
        let dimensions = first.len();
        if dimensions == 0 || dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(IvfError::DimensionMismatch {
                expected: MAX_VECTOR_DIMENSIONS,
                actual: dimensions,
            });
        }
        if centroids.len() != postings.len() {
            return Err(IvfError::ListCountMismatch {
                centroids: centroids.len(),
                postings: postings.len(),
            });
        }
        for centroid in &centroids {
            validate_dimensions(dimensions, centroid)?;
            if centroid.iter().any(|value| !value.is_finite()) {
                return Err(IvfError::CorruptGeneration {
                    reason: "centroid contains a non-finite coordinate",
                });
            }
        }
        let mut point_ids = BTreeSet::new();
        for posting in postings.iter().flatten() {
            validate_dimensions(dimensions, posting.vector())?;
            if !point_ids.insert(posting.point_id()) {
                return Err(IvfError::DuplicatePointId {
                    point_id: posting.point_id(),
                });
            }
        }
        Ok(Self {
            metric,
            dimensions,
            centroids,
            postings,
        })
    }

    /// Assigns a vector to its closest centroid with stable list-id ties.
    ///
    /// # Errors
    ///
    /// Returns an error when the vector has the wrong dimensions, metric
    /// evaluation fails, or the centroid inventory is corrupt.
    pub fn assign(&self, vector: &[f32]) -> Result<IvfListId, IvfError> {
        self.probe_order(vector)?
            .into_iter()
            .next()
            .ok_or(IvfError::CorruptGeneration {
                reason: "centroid inventory is empty",
            })
    }

    /// Returns every centroid in canonical distance/list-id order.
    ///
    /// # Errors
    ///
    /// Returns an error when the query has the wrong dimensions, metric
    /// evaluation fails, or the centroid inventory is corrupt.
    pub fn probe_order(&self, query: &[f32]) -> Result<Vec<IvfListId>, IvfError> {
        ivf_probe_order(self, query)
    }
}

impl IvfCentroidRead for InMemoryIvfIndex {
    fn metric(&self) -> DistanceMetric {
        self.metric
    }
    fn dimensions(&self) -> usize {
        self.dimensions
    }
    fn centroid_count(&self) -> usize {
        self.centroids.len()
    }
    fn centroid(&self, list_id: IvfListId) -> Option<&[f32]> {
        self.centroids.get(list_id.get()).map(Vec::as_slice)
    }
}

impl IvfPostingRead for InMemoryIvfIndex {
    fn list_len(&self, list_id: IvfListId) -> Result<usize, IvfError> {
        self.postings
            .get(list_id.get())
            .map(Vec::len)
            .ok_or(IvfError::CorruptGeneration {
                reason: "posting list id is outside the directory",
            })
    }

    fn posting(&self, list_id: IvfListId, offset: usize) -> Result<IvfPostingRef<'_>, IvfError> {
        let posting = self
            .postings
            .get(list_id.get())
            .and_then(|list| list.get(offset))
            .ok_or(IvfError::CorruptGeneration {
                reason: "posting offset is outside the list",
            })?;
        Ok(IvfPostingRef::dense(posting.point_id(), posting.vector()))
    }
}

/// One bounded approximate hit returned by the pure IVF index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IvfHit {
    point_id: IvfPointId,
    score: f32,
}

impl IvfHit {
    /// Returns the source point identity.
    #[must_use]
    pub const fn point_id(self) -> IvfPointId {
        self.point_id
    }
    /// Returns the approximate score used for bounded candidate ordering.
    #[must_use]
    pub const fn score(self) -> f32 {
        self.score
    }
}

/// Terminal pure-search classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IvfSearchCompletion {
    /// The requested result count was filled or every generation list was visited.
    Complete,
    /// The maximum probe count stopped widening before filling the result count.
    ProbeBudgetExhausted,
}

impl IvfSearchCompletion {
    /// Returns whether bounded IVF work satisfied its declared search contract.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Pure IVF search results and bounded-work diagnostics.
#[derive(Debug, Clone, PartialEq)]
pub struct IvfSearchOutcome {
    hits: Vec<IvfHit>,
    visited_lists: usize,
    visited_postings: usize,
    widening_rounds: usize,
    completion: IvfSearchCompletion,
    ordering: IvfIterativePolicy,
}

impl IvfSearchOutcome {
    /// Returns candidate hits in the declared ordering contract.
    #[must_use]
    pub fn hits(&self) -> &[IvfHit] {
        &self.hits
    }
    /// Returns visited list count.
    #[must_use]
    pub const fn visited_lists(&self) -> usize {
        self.visited_lists
    }
    /// Returns scored posting count.
    #[must_use]
    pub const fn visited_postings(&self) -> usize {
        self.visited_postings
    }
    /// Returns lists visited beyond the initial probe count.
    #[must_use]
    pub const fn widening_rounds(&self) -> usize {
        self.widening_rounds
    }
    /// Returns terminal completion.
    #[must_use]
    pub const fn completion(&self) -> IvfSearchCompletion {
        self.completion
    }
    /// Returns the ordering promise.
    #[must_use]
    pub const fn ordering(&self) -> IvfIterativePolicy {
        self.ordering
    }
}

/// Searches the closest bounded IVF lists through borrowed pure ports.
///
/// Strict ordering sorts every candidate from the lists actually visited; it
/// does not turn a bounded-probe IVF search into an exhaustive search of lists
/// outside `probe_budget`.
///
/// # Errors
///
/// Returns a typed error for invalid dimensions or budgets, malformed read
/// ports, metric failures, candidate-budget exhaustion, or cancellation.
#[allow(
    clippy::too_many_arguments,
    reason = "ports and independent work budgets are explicit"
)]
pub fn search_ivf<R, M, C>(
    index: &R,
    query: &[f32],
    config: &IvfConfig,
    probe_budget: IvfProbeBudget,
    candidate_budget: IvfCandidateBudget,
    limit: usize,
    mask: &M,
    cancellation: &C,
) -> Result<IvfSearchOutcome, IvfError>
where
    R: IvfCentroidRead + IvfPostingRead,
    M: IvfCandidateMask + ?Sized,
    C: IvfCancellation + ?Sized,
{
    let scorer = DenseScorer {
        metric: index.metric(),
        query,
    };
    search_ivf_with_scorer(
        index,
        query,
        config,
        probe_budget,
        candidate_budget,
        limit,
        mask,
        cancellation,
        &scorer,
    )
}

/// Searches IVF through the canonical ports with a prepared representation-specific scorer.
///
/// This is the production composition point for page-backed full-precision and
/// codec-bound postings. Candidate selection, probing, masking, cancellation,
/// budgets, tie rules, and diagnostics remain owned by this crate.
///
/// # Errors
///
/// Returns the same typed validation, corruption, budget, cancellation, and
/// scoring errors as [`search_ivf`].
#[allow(
    clippy::too_many_arguments,
    reason = "ports and independent work budgets are explicit"
)]
pub fn search_ivf_with_scorer<R, M, C, S>(
    index: &R,
    query: &[f32],
    config: &IvfConfig,
    probe_budget: IvfProbeBudget,
    candidate_budget: IvfCandidateBudget,
    limit: usize,
    mask: &M,
    cancellation: &C,
    scorer: &S,
) -> Result<IvfSearchOutcome, IvfError>
where
    R: IvfCentroidRead + IvfPostingRead,
    M: IvfCandidateMask + ?Sized,
    C: IvfCancellation + ?Sized,
    S: IvfScorer + ?Sized,
{
    let lists_to_visit = probe_budget.get();
    if lists_to_visit < config.probes() || lists_to_visit > config.max_probes() {
        return Err(IvfError::InvalidConfig {
            parameter: "probe_budget",
            value: lists_to_visit,
        });
    }
    let window = IvfProbeWindow::new(0, lists_to_visit).ok_or(IvfError::InvalidConfig {
        parameter: "probe_budget",
        value: lists_to_visit,
    })?;
    search_ivf_probe_window_with_scorer(
        index,
        query,
        config,
        window,
        candidate_budget,
        0,
        limit,
        mask,
        cancellation,
        scorer,
    )
}

/// Searches one new half-open probe window through the canonical dense scorer.
///
/// `previously_visited` charges posting work performed by earlier rounds to the
/// same scan-global candidate budget.
///
/// # Errors
///
/// Returns the same typed errors as [`search_ivf`], and rejects windows beyond
/// the configured maximum probe count.
#[allow(
    clippy::too_many_arguments,
    reason = "window and scan-global work accounting are independent contracts"
)]
pub fn search_ivf_probe_window<R, M, C>(
    index: &R,
    query: &[f32],
    config: &IvfConfig,
    window: IvfProbeWindow,
    candidate_budget: IvfCandidateBudget,
    previously_visited: usize,
    limit: usize,
    mask: &M,
    cancellation: &C,
) -> Result<IvfSearchOutcome, IvfError>
where
    R: IvfCentroidRead + IvfPostingRead,
    M: IvfCandidateMask + ?Sized,
    C: IvfCancellation + ?Sized,
{
    let scorer = DenseScorer {
        metric: index.metric(),
        query,
    };
    search_ivf_probe_window_with_scorer(
        index,
        query,
        config,
        window,
        candidate_budget,
        previously_visited,
        limit,
        mask,
        cancellation,
        &scorer,
    )
}

/// Searches one new half-open probe window with a prepared posting scorer.
///
/// # Errors
///
/// Returns a typed validation, corruption, budget, cancellation, or scoring
/// error without revisiting lists outside `window`.
#[allow(
    clippy::too_many_arguments,
    reason = "ports, probe window, and scan-global work budget are explicit"
)]
pub fn search_ivf_probe_window_with_scorer<R, M, C, S>(
    index: &R,
    query: &[f32],
    config: &IvfConfig,
    window: IvfProbeWindow,
    candidate_budget: IvfCandidateBudget,
    previously_visited: usize,
    limit: usize,
    mask: &M,
    cancellation: &C,
    scorer: &S,
) -> Result<IvfSearchOutcome, IvfError>
where
    R: IvfCentroidRead + IvfPostingRead,
    M: IvfCandidateMask + ?Sized,
    C: IvfCancellation + ?Sized,
    S: IvfScorer + ?Sized,
{
    validate_dimensions(index.dimensions(), query)?;
    if index.centroid_count() != config.lists() {
        return Err(IvfError::ListCountMismatch {
            centroids: index.centroid_count(),
            postings: config.lists(),
        });
    }
    if window.end() > config.max_probes() || window.end() > index.centroid_count() {
        return Err(IvfError::InvalidConfig {
            parameter: "probe_window",
            value: window.end(),
        });
    }
    if limit == 0 {
        return Ok(IvfSearchOutcome {
            hits: Vec::new(),
            visited_lists: 0,
            visited_postings: 0,
            widening_rounds: 0,
            completion: IvfSearchCompletion::Complete,
            ordering: config.iterative_policy(),
        });
    }

    let order = ivf_probe_order(index, query)?;
    let mut hits =
        Vec::with_capacity(limit.min(candidate_budget.get().saturating_sub(previously_visited)));
    let mut visited_postings = 0usize;
    let mut visited_lists = 0usize;

    for list_id in order
        .into_iter()
        .skip(window.start())
        .take(window.end() - window.start())
    {
        if cancellation.cancelled() {
            return Err(IvfError::Cancelled);
        }
        let list_len = index.list_len(list_id)?;
        for offset in 0..list_len {
            if cancellation.cancelled() {
                return Err(IvfError::Cancelled);
            }
            visited_postings =
                visited_postings
                    .checked_add(1)
                    .ok_or(IvfError::CandidateBudgetExhausted {
                        budget: candidate_budget.get(),
                        visited: usize::MAX,
                    })?;
            let total_visited = previously_visited.checked_add(visited_postings).ok_or(
                IvfError::CandidateBudgetExhausted {
                    budget: candidate_budget.get(),
                    visited: usize::MAX,
                },
            )?;
            if total_visited > candidate_budget.get() {
                return Err(IvfError::CandidateBudgetExhausted {
                    budget: candidate_budget.get(),
                    visited: total_visited,
                });
            }
            let posting = index.posting(list_id, offset)?;
            if mask.eligible(posting.point_id()) {
                hits.push(IvfHit {
                    point_id: posting.point_id(),
                    score: scorer.score(posting)?,
                });
            }
        }
        visited_lists += 1;
        if hits.len() >= limit {
            break;
        }
    }

    hits.sort_unstable_by(|left, right| {
        index
            .metric()
            .score_order()
            .compare(f64::from(left.score), f64::from(right.score))
            .then_with(|| left.point_id.cmp(&right.point_id))
    });
    hits.truncate(limit);
    let completion = if hits.len() >= limit || window.end() == index.centroid_count() {
        IvfSearchCompletion::Complete
    } else {
        IvfSearchCompletion::ProbeBudgetExhausted
    };
    Ok(IvfSearchOutcome {
        hits,
        visited_lists,
        visited_postings,
        widening_rounds: window.end().saturating_sub(config.probes()),
        completion,
        ordering: config.iterative_policy(),
    })
}

/// Returns every centroid in canonical metric-score/list-id order.
///
/// # Errors
///
/// Rejects dimension mismatches, missing centroids, and metric failures.
pub fn ivf_probe_order<R: IvfCentroidRead + ?Sized>(
    index: &R,
    query: &[f32],
) -> Result<Vec<IvfListId>, IvfError> {
    validate_dimensions(index.dimensions(), query)?;
    let mut scores = Vec::with_capacity(index.centroid_count());
    for raw_list_id in 0..index.centroid_count() {
        let list_id = IvfListId::new(raw_list_id);
        let centroid = index.centroid(list_id).ok_or(IvfError::CorruptGeneration {
            reason: "centroid id is outside the inventory",
        })?;
        validate_dimensions(index.dimensions(), centroid)?;
        let score = index.metric().distance_slices(query, centroid)?;
        scores.push((list_id, score));
    }
    scores.sort_unstable_by(|(left_id, left), (right_id, right)| {
        index
            .metric()
            .score_order()
            .compare(f64::from(*left), f64::from(*right))
            .then_with(|| left_id.cmp(right_id))
    });
    Ok(scores.into_iter().map(|(list_id, _)| list_id).collect())
}

fn validate_dimensions(expected: usize, vector: &[f32]) -> Result<(), IvfError> {
    if vector.len() == expected {
        Ok(())
    } else {
        Err(IvfError::DimensionMismatch {
            expected,
            actual: vector.len(),
        })
    }
}
