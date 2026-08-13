use context_core::{OccurrenceId, PointId};

use crate::{
    Cancellation, MAX_QUERY_ELAPSED_MICROS, MAX_QUERY_MEMORY_BYTES, PortBudget, QueryClock,
    QueryError, Result,
};

/// Default number of live frontier states retained by the beam.
pub const DEFAULT_BEAM_WIDTH: usize = 32;
/// Maximum number of live frontier states retained by the beam.
pub const MAX_BEAM_WIDTH: usize = 256;
/// Default maximum provider records in one expansion batch.
pub const DEFAULT_BEAM_EXPANSION_BATCH: usize = 32;
/// Maximum provider records and selected parents in one expansion batch.
pub const MAX_BEAM_EXPANSION_BATCH: usize = 256;
/// Maximum parent-linked states admitted during one execution.
pub const MAX_BEAM_ADMITTED_STATES: usize = 65_536;
/// Maximum distinct dominance keys retained during one execution.
pub const MAX_BEAM_VISITED_KEYS: usize = 65_536;
/// Maximum vector expansion work charged during one execution.
pub const MAX_BEAM_VECTOR_EXPANSIONS: usize = 10_000_000;
/// Maximum exact-rerank work charged during one execution.
pub const MAX_BEAM_EXACT_RERANKS: usize = 10_000_000;
/// Maximum parent depth in the vector-only beam.
pub const MAX_BEAM_HOPS: u16 = 64;
/// Maximum bytes retained by parent-state arena chunks.
pub const MAX_BEAM_PARENT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum total extension-owned bytes retained by the beam.
pub const MAX_BEAM_RETAINED_BYTES: usize = MAX_QUERY_MEMORY_BYTES;
/// Maximum elapsed execution allowance in microseconds.
pub const MAX_BEAM_ELAPSED_MICROS: u64 = MAX_QUERY_ELAPSED_MICROS;

/// Statement-local state identity inside one virtual beam arena.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BeamStateId(u32);

impl BeamStateId {
    pub(crate) fn from_index(index: usize) -> Result<Self> {
        let value = u32::try_from(index).map_err(|_| QueryError::ArithmeticOverflow {
            operation: "beam_state_id",
        })?;
        Ok(Self(value))
    }

    /// Returns the zero-based arena index.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    pub(crate) const fn index(self) -> usize {
        self.0 as usize
    }

    #[cfg(test)]
    pub(crate) const fn fixture(value: u32) -> Self {
        Self(value)
    }
}

/// Provider-native HNSW node identity carried without storage semantics.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BeamNodeId(u64);

impl BeamNodeId {
    /// Creates an opaque provider-native node identity.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Reserved topology identity. P16 rejects every populated value.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TopologyNodeId(u64);

impl TopologyNodeId {
    /// Creates an opaque topology identity for later mixed-beam phases.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric identity.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Bounded provider-independent path automaton state.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PathPatternState(u32);

impl PathPatternState {
    /// Creates an opaque path-pattern state.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the opaque numeric state.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Opaque authorization scope propagated across one state lineage.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AuthorizationContextToken(u64);

impl AuthorizationContextToken {
    /// Creates a non-zero opaque authorization token.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    /// Returns the opaque token for composition-root adapters.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Transition represented by an admitted P16 state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BeamTransition {
    /// Caller-authorized starting occurrence.
    Seed,
    /// Vector-provider expansion from a selected parent.
    Vector,
}

impl BeamTransition {
    /// Returns the stable content-free diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::Vector => "vector",
        }
    }
}

/// Finite score components retained separately from canonical ranking.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamScoreComponents {
    vector: f64,
    transition: f64,
    accumulated: f64,
    exact: Option<f64>,
    ranking: f64,
}

impl BeamScoreComponents {
    /// Validates score components and computes the canonical higher-is-better score.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when any component or the checked
    /// canonical sum is not finite.
    pub fn new(vector: f64, transition: f64, accumulated: f64, exact: Option<f64>) -> Result<Self> {
        if !vector.is_finite()
            || !transition.is_finite()
            || !accumulated.is_finite()
            || exact.is_some_and(|score| !score.is_finite())
        {
            return Err(QueryError::InvalidInput {
                field: "beam_score",
                reason: "every score component must be finite".to_owned(),
            });
        }
        let candidate = exact.unwrap_or(vector);
        let ranking = if accumulated == 0.0 && transition == 0.0 {
            candidate
        } else {
            accumulated + transition + candidate
        };
        if !ranking.is_finite() {
            return Err(QueryError::InvalidInput {
                field: "beam_score",
                reason: "canonical score sum must be finite".to_owned(),
            });
        }
        Ok(Self {
            vector,
            transition,
            accumulated,
            exact,
            ranking,
        })
    }

    /// Returns the vector-provider score.
    #[must_use]
    pub const fn vector(self) -> f64 {
        self.vector
    }

    /// Returns the transition contribution.
    #[must_use]
    pub const fn transition(self) -> f64 {
        self.transition
    }

    /// Returns the accumulated parent/path contribution.
    #[must_use]
    pub const fn accumulated(self) -> f64 {
        self.accumulated
    }

    /// Returns the authoritative exact score when supplied.
    #[must_use]
    pub const fn exact(self) -> Option<f64> {
        self.exact
    }

    /// Returns the canonical higher-is-better ranking score.
    #[must_use]
    pub const fn ranking(self) -> f64 {
        self.ranking
    }
}

/// Caller-authorized vector seed admitted at hop zero.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamSeed {
    pub(crate) occurrence_id: OccurrenceId,
    pub(crate) point_id: PointId,
    pub(crate) hnsw_node_id: Option<BeamNodeId>,
    pub(crate) path_pattern_state: PathPatternState,
    pub(crate) authorization: AuthorizationContextToken,
    pub(crate) scores: BeamScoreComponents,
}

impl BeamSeed {
    /// Creates one content-free authorized vector seed.
    #[must_use]
    pub const fn new(
        occurrence_id: OccurrenceId,
        point_id: PointId,
        hnsw_node_id: Option<BeamNodeId>,
        path_pattern_state: PathPatternState,
        authorization: AuthorizationContextToken,
        scores: BeamScoreComponents,
    ) -> Self {
        Self {
            occurrence_id,
            point_id,
            hnsw_node_id,
            path_pattern_state,
            authorization,
            scores,
        }
    }
}

/// One provider-produced vector expansion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamExpansion {
    pub(crate) parent_state_id: BeamStateId,
    pub(crate) occurrence_id: OccurrenceId,
    pub(crate) point_id: PointId,
    pub(crate) hnsw_node_id: Option<BeamNodeId>,
    pub(crate) topology_node_id: Option<TopologyNodeId>,
    pub(crate) path_pattern_state: PathPatternState,
    pub(crate) scores: BeamScoreComponents,
}

impl BeamExpansion {
    /// Creates one bounded provider response record.
    ///
    /// The accumulated parent contribution is derived by the engine and is
    /// deliberately not accepted at this provider boundary.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for non-finite score components.
    #[allow(
        clippy::too_many_arguments,
        reason = "the provider boundary keeps every independent identity and score component explicit"
    )]
    pub fn new(
        parent_state_id: BeamStateId,
        occurrence_id: OccurrenceId,
        point_id: PointId,
        hnsw_node_id: Option<BeamNodeId>,
        topology_node_id: Option<TopologyNodeId>,
        path_pattern_state: PathPatternState,
        vector_score: f64,
        transition_score: f64,
        exact_score: Option<f64>,
    ) -> Result<Self> {
        Ok(Self {
            parent_state_id,
            occurrence_id,
            point_id,
            hnsw_node_id,
            topology_node_id,
            path_pattern_state,
            scores: BeamScoreComponents::new(vector_score, transition_score, 0.0, exact_score)?,
        })
    }
}

/// Read-only selected parent supplied to a vector provider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamParent {
    pub(crate) state_id: BeamStateId,
    pub(crate) occurrence_id: OccurrenceId,
    pub(crate) point_id: PointId,
    pub(crate) hnsw_node_id: Option<BeamNodeId>,
    pub(crate) path_pattern_state: PathPatternState,
    pub(crate) hop: u16,
    pub(crate) authorization: AuthorizationContextToken,
    pub(crate) scores: BeamScoreComponents,
}

impl BeamParent {
    /// Returns the statement-local parent identity.
    #[must_use]
    pub const fn state_id(self) -> BeamStateId {
        self.state_id
    }

    /// Returns the occurrence identity.
    #[must_use]
    pub const fn occurrence_id(self) -> OccurrenceId {
        self.occurrence_id
    }

    /// Returns the logical point identity.
    #[must_use]
    pub const fn point_id(self) -> PointId {
        self.point_id
    }

    /// Returns the provider-native HNSW identity when present.
    #[must_use]
    pub const fn hnsw_node_id(self) -> Option<BeamNodeId> {
        self.hnsw_node_id
    }

    /// Returns the path-pattern state.
    #[must_use]
    pub const fn path_pattern_state(self) -> PathPatternState {
        self.path_pattern_state
    }

    /// Returns the parent hop.
    #[must_use]
    pub const fn hop(self) -> u16 {
        self.hop
    }

    /// Returns the opaque authorization token.
    #[must_use]
    pub const fn authorization(self) -> AuthorizationContextToken {
        self.authorization
    }

    /// Returns the separate score components.
    #[must_use]
    pub const fn scores(self) -> BeamScoreComponents {
        self.scores
    }
}

/// One complete bounded provider request.
#[derive(Clone, Debug, PartialEq)]
pub struct BeamProviderRequest {
    pub(crate) parents: Vec<BeamParent>,
    pub(crate) max_expansions: usize,
    pub(crate) max_exact_reranks: usize,
}

impl BeamProviderRequest {
    /// Returns selected parents in canonical order.
    #[must_use]
    pub fn parents(&self) -> &[BeamParent] {
        &self.parents
    }

    /// Returns the maximum response records and charged vector expansions.
    #[must_use]
    pub const fn max_expansions(&self) -> usize {
        self.max_expansions
    }

    /// Returns the maximum exact reranks chargeable by this call.
    #[must_use]
    pub const fn max_exact_reranks(&self) -> usize {
        self.max_exact_reranks
    }
}

/// One complete provider response with explicit work accounting.
#[derive(Clone, Debug, PartialEq)]
pub struct BeamExpansionBatch {
    pub(crate) expansions: Vec<BeamExpansion>,
    pub(crate) vector_expansions: usize,
    pub(crate) exact_reranks: usize,
}

impl BeamExpansionBatch {
    /// Creates a provider batch. The engine validates it against its request.
    #[must_use]
    pub const fn new(
        expansions: Vec<BeamExpansion>,
        vector_expansions: usize,
        exact_reranks: usize,
    ) -> Self {
        Self {
            expansions,
            vector_expansions,
            exact_reranks,
        }
    }
}

/// Synchronous vector expansion port consumed by the beam engine.
pub trait BeamExpansionProvider {
    /// Expands the selected parent batch within the supplied hard limits.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral provider, cancellation, or budget failure.
    fn expand(
        &mut self,
        request: &BeamProviderRequest,
        budget: PortBudget,
    ) -> Result<BeamExpansionBatch>;
}

/// One validated hard beam-execution budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BeamBudget {
    pub(crate) beam_width: usize,
    pub(crate) expansion_batch: usize,
    pub(crate) max_admitted_states: usize,
    pub(crate) max_visited_keys: usize,
    pub(crate) max_vector_expansions: usize,
    pub(crate) max_exact_reranks: usize,
    pub(crate) max_parent_bytes: usize,
    pub(crate) max_retained_bytes: usize,
    pub(crate) max_hops: u16,
    pub(crate) max_results: usize,
    pub(crate) max_elapsed_micros: u64,
}

impl BeamBudget {
    /// Creates the bounded default internal beam budget.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when `max_results` is zero or
    /// exceeds the repository-wide result ceiling.
    pub fn default_internal(max_results: usize) -> Result<Self> {
        Self::new(
            DEFAULT_BEAM_WIDTH,
            DEFAULT_BEAM_EXPANSION_BATCH,
            MAX_BEAM_ADMITTED_STATES,
            MAX_BEAM_VISITED_KEYS,
            MAX_BEAM_VECTOR_EXPANSIONS,
            MAX_BEAM_EXACT_RERANKS,
            MAX_BEAM_PARENT_BYTES,
            MAX_BEAM_RETAINED_BYTES,
            MAX_BEAM_HOPS,
            max_results,
            MAX_BEAM_ELAPSED_MICROS,
        )
    }

    /// Creates an explicit beam budget after enforcing every frozen ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when any value is zero or above its
    /// phase-owned maximum.
    #[allow(
        clippy::too_many_arguments,
        reason = "every independent hard resource is explicit"
    )]
    pub fn new(
        beam_width: usize,
        expansion_batch: usize,
        max_admitted_states: usize,
        max_visited_keys: usize,
        max_vector_expansions: usize,
        max_exact_reranks: usize,
        max_parent_bytes: usize,
        max_retained_bytes: usize,
        max_hops: u16,
        max_results: usize,
        max_elapsed_micros: u64,
    ) -> Result<Self> {
        let values = [
            ("beam_width", beam_width, MAX_BEAM_WIDTH),
            (
                "beam_expansion_batch",
                expansion_batch,
                MAX_BEAM_EXPANSION_BATCH,
            ),
            (
                "beam_admitted_states",
                max_admitted_states,
                MAX_BEAM_ADMITTED_STATES,
            ),
            ("beam_visited_keys", max_visited_keys, MAX_BEAM_VISITED_KEYS),
            (
                "beam_vector_expansions",
                max_vector_expansions,
                MAX_BEAM_VECTOR_EXPANSIONS,
            ),
            (
                "beam_exact_reranks",
                max_exact_reranks,
                MAX_BEAM_EXACT_RERANKS,
            ),
            ("beam_parent_bytes", max_parent_bytes, MAX_BEAM_PARENT_BYTES),
            (
                "beam_retained_bytes",
                max_retained_bytes,
                MAX_BEAM_RETAINED_BYTES,
            ),
            (
                "beam_results",
                max_results,
                context_core::policy::MAX_SEARCH_LIMIT,
            ),
        ];
        if let Some((field, value, maximum)) = values
            .into_iter()
            .find(|(_, value, maximum)| *value == 0 || value > maximum)
        {
            return Err(QueryError::InvalidInput {
                field,
                reason: format!("must be within 1..={maximum}; received {value}"),
            });
        }
        if max_hops == 0 || max_hops > MAX_BEAM_HOPS {
            return Err(QueryError::InvalidInput {
                field: "beam_hops",
                reason: format!("must be within 1..={MAX_BEAM_HOPS}; received {max_hops}"),
            });
        }
        if max_elapsed_micros == 0 || max_elapsed_micros > MAX_BEAM_ELAPSED_MICROS {
            return Err(QueryError::InvalidInput {
                field: "beam_elapsed_micros",
                reason: format!(
                    "must be within 1..={MAX_BEAM_ELAPSED_MICROS}; received {max_elapsed_micros}"
                ),
            });
        }
        Ok(Self {
            beam_width,
            expansion_batch,
            max_admitted_states,
            max_visited_keys,
            max_vector_expansions,
            max_exact_reranks,
            max_parent_bytes,
            max_retained_bytes,
            max_hops,
            max_results,
            max_elapsed_micros,
        })
    }

    /// Returns the live frontier width.
    #[must_use]
    pub const fn beam_width(self) -> usize {
        self.beam_width
    }

    /// Returns the provider batch limit.
    #[must_use]
    pub const fn expansion_batch(self) -> usize {
        self.expansion_batch
    }
}

/// Hard resource that terminated an incomplete beam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeamBudgetKind {
    /// Parent-linked state count.
    AdmittedStates,
    /// Distinct dominance key count.
    VisitedKeys,
    /// Provider vector expansion work.
    VectorExpansions,
    /// Provider exact-rerank work.
    ExactReranks,
    /// Parent arena bytes.
    ParentBytes,
    /// Total retained/transient bytes.
    RetainedBytes,
    /// Wall-clock allowance.
    Elapsed,
}

impl BeamBudgetKind {
    /// Returns the stable content-free diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::AdmittedStates => "admitted_states",
            Self::VisitedKeys => "visited_keys",
            Self::VectorExpansions => "vector_expansions",
            Self::ExactReranks => "exact_reranks",
            Self::ParentBytes => "parent_bytes",
            Self::RetainedBytes => "retained_bytes",
            Self::Elapsed => "elapsed",
        }
    }
}

/// Terminal state of one consumed beam execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BeamCompletion {
    /// Every admitted frontier state was expanded or reached the hop bound.
    Exhausted,
    /// Cooperative cancellation stopped before the next charged operation.
    Cancelled,
    /// One named hard resource prevented authoritative completion.
    BudgetExhausted(BeamBudgetKind),
}

impl BeamCompletion {
    /// Returns whether no additional result could have been produced.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Exhausted)
    }

    /// Returns the stable content-free diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Exhausted => "exhausted",
            Self::Cancelled => "cancelled",
            Self::BudgetExhausted(kind) => kind.stable_name(),
        }
    }
}

/// Fixed-width pruning counts with no row identities or contents.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BeamPruningDiagnostics {
    pub(crate) duplicate: usize,
    pub(crate) dominated: usize,
    pub(crate) cycle: usize,
    pub(crate) beam_width: usize,
    pub(crate) hop: usize,
}

impl BeamPruningDiagnostics {
    /// Returns duplicate-key prunes.
    #[must_use]
    pub const fn duplicate(self) -> usize {
        self.duplicate
    }
    /// Returns dominated-state prunes.
    #[must_use]
    pub const fn dominated(self) -> usize {
        self.dominated
    }
    /// Returns ancestry-cycle prunes.
    #[must_use]
    pub const fn cycle(self) -> usize {
        self.cycle
    }
    /// Returns frontier-width prunes.
    #[must_use]
    pub const fn beam_width(self) -> usize {
        self.beam_width
    }
    /// Returns hop-limit prunes.
    #[must_use]
    pub const fn hop(self) -> usize {
        self.hop
    }
}

/// Fixed-width score range diagnostics without identities or row contents.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BeamScoreDiagnostics {
    pub(crate) best: Option<f64>,
    pub(crate) worst: Option<f64>,
}

impl BeamScoreDiagnostics {
    /// Returns the best admitted canonical score.
    #[must_use]
    pub const fn best(self) -> Option<f64> {
        self.best
    }
    /// Returns the worst admitted canonical score.
    #[must_use]
    pub const fn worst(self) -> Option<f64> {
        self.worst
    }
}

/// Content-free cumulative execution diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BeamDiagnostics {
    pub(crate) provider_calls: usize,
    pub(crate) admitted_states: usize,
    pub(crate) visited_keys: usize,
    pub(crate) vector_expansions: usize,
    pub(crate) exact_reranks: usize,
    pub(crate) parent_bytes: usize,
    pub(crate) retained_bytes: usize,
    pub(crate) elapsed_micros: u64,
    pub(crate) pruning: BeamPruningDiagnostics,
    pub(crate) scores: BeamScoreDiagnostics,
}

impl BeamDiagnostics {
    /// Returns provider calls.
    #[must_use]
    pub const fn provider_calls(self) -> usize {
        self.provider_calls
    }
    /// Returns admitted parent states.
    #[must_use]
    pub const fn admitted_states(self) -> usize {
        self.admitted_states
    }
    /// Returns distinct dominance keys.
    #[must_use]
    pub const fn visited_keys(self) -> usize {
        self.visited_keys
    }
    /// Returns charged vector expansions.
    #[must_use]
    pub const fn vector_expansions(self) -> usize {
        self.vector_expansions
    }
    /// Returns charged exact reranks.
    #[must_use]
    pub const fn exact_reranks(self) -> usize {
        self.exact_reranks
    }
    /// Returns allocated parent-arena bytes.
    #[must_use]
    pub const fn parent_bytes(self) -> usize {
        self.parent_bytes
    }
    /// Returns conservative total retained bytes.
    #[must_use]
    pub const fn retained_bytes(self) -> usize {
        self.retained_bytes
    }
    /// Returns observed monotonic elapsed microseconds.
    #[must_use]
    pub const fn elapsed_micros(self) -> u64 {
        self.elapsed_micros
    }
    /// Returns pruning counts.
    #[must_use]
    pub const fn pruning(self) -> BeamPruningDiagnostics {
        self.pruning
    }
    /// Returns score range diagnostics.
    #[must_use]
    pub const fn scores(self) -> BeamScoreDiagnostics {
        self.scores
    }
}

/// One reconstructed path step returned without authorization context.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeamPathStep {
    pub(crate) occurrence_id: OccurrenceId,
    pub(crate) transition: BeamTransition,
    pub(crate) hop: u16,
}

impl BeamPathStep {
    /// Returns the occurrence identity.
    #[must_use]
    pub const fn occurrence_id(self) -> OccurrenceId {
        self.occurrence_id
    }
    /// Returns the transition used to admit the state.
    #[must_use]
    pub const fn transition(self) -> BeamTransition {
        self.transition
    }
    /// Returns the state hop.
    #[must_use]
    pub const fn hop(self) -> u16 {
        self.hop
    }
}

/// One final beam hit with a reconstructed bounded occurrence path.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum BeamPath {
    Root(BeamPathStep),
    Expanded(Vec<BeamPathStep>),
}

impl BeamPath {
    pub(crate) fn as_slice(&self) -> &[BeamPathStep] {
        match self {
            Self::Root(step) => std::slice::from_ref(step),
            Self::Expanded(steps) => steps,
        }
    }
}

/// One final beam hit with a reconstructed bounded occurrence path.
#[derive(Clone, Debug, PartialEq)]
pub struct BeamHit {
    pub(crate) occurrence_id: OccurrenceId,
    pub(crate) point_id: PointId,
    pub(crate) hnsw_node_id: Option<BeamNodeId>,
    pub(crate) scores: BeamScoreComponents,
    pub(crate) path: BeamPath,
}

impl BeamHit {
    /// Returns the occurrence identity.
    #[must_use]
    pub const fn occurrence_id(&self) -> OccurrenceId {
        self.occurrence_id
    }
    /// Returns the logical point identity.
    #[must_use]
    pub const fn point_id(&self) -> PointId {
        self.point_id
    }
    /// Returns the provider-native HNSW identity when present.
    #[must_use]
    pub const fn hnsw_node_id(&self) -> Option<BeamNodeId> {
        self.hnsw_node_id
    }
    /// Returns separate score components.
    #[must_use]
    pub const fn scores(&self) -> BeamScoreComponents {
        self.scores
    }
    /// Returns the reconstructed root-to-hit path.
    #[must_use]
    pub fn path(&self) -> &[BeamPathStep] {
        self.path.as_slice()
    }
}

/// Owned result of a consumed beam execution.
#[derive(Clone, Debug, PartialEq)]
pub struct BeamOutcome {
    pub(crate) hits: Vec<BeamHit>,
    pub(crate) completion: BeamCompletion,
    pub(crate) diagnostics: BeamDiagnostics,
}

impl BeamOutcome {
    /// Returns final hits in canonical order.
    #[must_use]
    pub fn hits(&self) -> &[BeamHit] {
        &self.hits
    }
    /// Returns the typed terminal state.
    #[must_use]
    pub const fn completion(&self) -> BeamCompletion {
        self.completion
    }
    /// Returns content-free cumulative diagnostics.
    #[must_use]
    pub const fn diagnostics(&self) -> BeamDiagnostics {
        self.diagnostics
    }
}

pub(crate) struct BeamRuntime<'a, P, C, K> {
    pub(crate) provider: &'a mut P,
    pub(crate) cancellation: &'a C,
    pub(crate) clock: &'a K,
}

impl<'a, P, C, K> BeamRuntime<'a, P, C, K>
where
    P: BeamExpansionProvider,
    C: Cancellation,
    K: QueryClock,
{
    pub(crate) const fn new(provider: &'a mut P, cancellation: &'a C, clock: &'a K) -> Self {
        Self {
            provider,
            cancellation,
            clock,
        }
    }
}
