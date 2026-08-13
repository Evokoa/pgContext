//! Pure application boundary for pgContext query planning and execution.
//!
//! This crate owns logical query contracts and ports. PostgreSQL, pgrx, index
//! storage, artifact storage, and SQLSTATE translation remain adapter concerns.

#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]
#![warn(rustdoc::broken_intra_doc_links)]

mod adaptive;
mod beam;
mod budget;
mod error;
mod executor;
mod formula;
mod ir;
mod lazy_cursor;
mod lexical;
mod multi_profile;
mod plan;
mod policy;
mod ports;
mod rerank_envelope;
mod strategy;
mod types;
mod validation;

pub use adaptive::{
    ADAPTIVE_PREFIX_OVERSAMPLE, AdaptivePrefixControl, AdaptivePrefixReason,
    AdaptivePrefixStrategy, AdaptivePrefixStrategyInput, AdaptivePrefixStrategyKind,
    AdaptiveWideningBudget, AdaptiveWideningInput, AdaptiveWideningPlan, AdaptiveWideningStep,
    AdaptiveWideningTermination, plan_adaptive_widening, select_adaptive_prefix_strategy,
};
pub use beam::{
    AuthorizationContextToken, BeamBudget, BeamBudgetKind, BeamCompletion, BeamDiagnostics,
    BeamExpansion, BeamExpansionBatch, BeamExpansionProvider, BeamHit, BeamNodeId, BeamOutcome,
    BeamParent, BeamPathStep, BeamProviderRequest, BeamPruningDiagnostics, BeamScoreComponents,
    BeamScoreDiagnostics, BeamSeed, BeamStateId, BeamTransition, DEFAULT_BEAM_EXPANSION_BATCH,
    DEFAULT_BEAM_WIDTH, MAX_BEAM_ADMITTED_STATES, MAX_BEAM_ELAPSED_MICROS, MAX_BEAM_EXACT_RERANKS,
    MAX_BEAM_EXPANSION_BATCH, MAX_BEAM_HOPS, MAX_BEAM_PARENT_BYTES, MAX_BEAM_RETAINED_BYTES,
    MAX_BEAM_VECTOR_EXPANSIONS, MAX_BEAM_VISITED_KEYS, MAX_BEAM_WIDTH, PathPatternState,
    TopologyNodeId, VirtualBeamEngine,
};
pub use budget::{
    BudgetUsage, DEFAULT_QUERY_COMPARISONS, DEFAULT_QUERY_ELAPSED_MICROS,
    DEFAULT_QUERY_HYDRATION_BYTES, DEFAULT_QUERY_MEMORY_BYTES, ExecutionBudget,
    MAX_QUERY_COMPARISONS, MAX_QUERY_ELAPSED_MICROS, MAX_QUERY_HYDRATION_BYTES,
    MAX_QUERY_MEMORY_BYTES,
};
pub use context_core::policy::MAX_PROFILE_NAME_BYTES;
pub use context_core::{
    Completion, PointId, ProfileLifecycle, ProfileName, ReadinessReason, ScoreOrder,
};
pub use error::{QueryError, Result};
pub use executor::QueryExecutor;
pub use formula::{CompiledFormula, Formula, MAX_FORMULA_BYTES, MAX_FORMULA_OPERATIONS};
pub use ir::{
    Fusion, MAX_FILTER_SCALAR_BYTES, MAX_QUERY_DEPTH, MAX_QUERY_NODES, QueryIr, QueryKind,
    build_multi_profile_query, validate_filter_json_value,
};
pub use lazy_cursor::{
    DEFAULT_LAZY_CURSOR_BATCH, LazyCursorAdvance, LazyCursorControl, LazyCursorPage,
    LazyCursorTermination, LazyCursorWork, MAX_LAZY_CURSOR_BATCH,
};
pub use lexical::{
    FuzzyMode, FuzzyQuery, FuzzySourceName, FuzzyThreshold, LexicalBooleanOperator,
    LexicalNormalization, LexicalPrefixTerm, LexicalQuery, LexicalRankWeights, LexicalRanker,
    LexicalSourceName, LexicalText, LexicalWeight, LexicalWeightSet, MAX_LEXICAL_BOOLEAN_CLAUSES,
    MAX_LEXICAL_FIELDS, MAX_LEXICAL_HEADLINE_OPTIONS_BYTES, MAX_LEXICAL_HEADLINE_OUTPUT_BYTES,
    MAX_LEXICAL_HEADLINE_POINTS, MAX_LEXICAL_HEADLINE_SOURCE_BYTES, MAX_LEXICAL_JSON_PATH_DEPTH,
    MAX_LEXICAL_NAME_BYTES, MAX_LEXICAL_NORMALIZATION, MAX_LEXICAL_PHRASE_DISTANCE,
    MAX_LEXICAL_QUERY_DEPTH, MAX_LEXICAL_QUERY_NODES, MAX_LEXICAL_TEXT_BYTES,
    RegisteredTsQueryName,
};
pub use multi_profile::{
    MAX_MULTI_PROFILE_BRANCH_LIMIT, MAX_MULTI_PROFILE_BRANCHES, MAX_MULTI_PROFILE_QUERY_BYTES,
    MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES, MissingProfile, MissingProfileReason, MultiProfileBranch,
    MultiProfileContribution, MultiProfileCoverage, MultiProfileDecision, MultiProfileFusedPoint,
    MultiProfileObserved, MultiProfileQuery, MultiProfileRankedBranch, MultiProfileRankedCandidate,
    MultiProfileRequest, fuse_multi_profile, plan_multi_profile,
};
pub use plan::parse_query_plan;
pub use policy::{
    CandidateExpansionDecision, LateInteractionWork, MAX_LATE_INTERACTION_COMPARISONS,
    MAX_LATE_INTERACTION_SCALAR_CELLS, candidate_expansion_decision,
};
pub use ports::{
    Cancellation, CandidateCursor, CandidateSource, ExternalReranker, FilterCandidateSource,
    PortBudget, QueryClock, SourceRechecker, TelemetrySink, TopologyExpander,
};
pub use rerank_envelope::{
    MAX_RERANK_CANDIDATES, MAX_RERANK_CONTRIBUTIONS, MAX_RERANK_METADATA_BYTES,
    MAX_RERANK_METADATA_ENTRIES, MAX_RERANK_MODEL_NAME_BYTES, MAX_RERANK_QUERY_BYTES,
    MAX_RERANK_REQUEST_BYTES, MAX_RERANK_TEXT_BYTES, MAX_RERANK_WIRE_BYTES,
    RERANK_CONTENT_DIGEST_BYTES, RERANK_ENVELOPE_VERSION, RerankCandidate, RerankContentDigest,
    RerankContribution, RerankFallbackPolicy, RerankMetadata, RerankModelName, RerankQuery,
    RerankRejection, RerankRequest, RerankRequestId, RerankResponse, RerankResponseCompletion,
    RerankResponsePolicy, RerankScore, ValidatedRerankResponse, validate_rerank_response,
    validate_rerank_response_with_policy,
};
pub use strategy::{
    FilteredAnnReason, FilteredAnnStrategy, FilteredAnnStrategyInput, FilteredAnnStrategyKind,
    MultiVectorAnnReason, MultiVectorAnnStrategy, MultiVectorAnnStrategyInput,
    MultiVectorAnnStrategyKind, select_filtered_ann_strategy, select_multi_vector_ann_strategy,
};
pub use types::{
    AdaptiveStageDiagnostic, BranchContribution, Candidate, CandidateBranch, CandidateDiagnostics,
    CandidatePage, CandidateProvenance, CandidateSourceKind, CandidateStageDiagnostic,
    ExecutionOutcome, ExecutionState, ExternalRerankPage, FilterCandidateBatch, HydratedCandidate,
    RecheckPage, SourceReadiness, StageDiagnostic, StageKind,
};
pub use validation::QueryPlanValidator;

/// Returns the version of the pure query boundary.
#[must_use]
pub const fn query_contract_version() -> u16 {
    3
}

#[cfg(test)]
mod tests {
    use super::{PointId, query_contract_version};

    #[test]
    fn query_boundary_uses_logical_point_ids() {
        let point_id = PointId::new(7);
        assert_eq!(point_id.get(), 7);
        assert_eq!(query_contract_version(), 3);
    }
}
