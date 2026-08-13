//! Provider-neutral, statement-local virtual beam execution.

mod dominance;
mod engine;
mod helpers;
mod output;
mod types;

pub use engine::VirtualBeamEngine;
pub(crate) use types::BeamPath;
pub use types::{
    AuthorizationContextToken, BeamBudget, BeamBudgetKind, BeamCompletion, BeamDiagnostics,
    BeamExpansion, BeamExpansionBatch, BeamExpansionProvider, BeamHit, BeamNodeId, BeamOutcome,
    BeamParent, BeamPathStep, BeamProviderRequest, BeamPruningDiagnostics, BeamScoreComponents,
    BeamScoreDiagnostics, BeamSeed, BeamStateId, BeamTransition, DEFAULT_BEAM_EXPANSION_BATCH,
    DEFAULT_BEAM_WIDTH, MAX_BEAM_ADMITTED_STATES, MAX_BEAM_ELAPSED_MICROS, MAX_BEAM_EXACT_RERANKS,
    MAX_BEAM_EXPANSION_BATCH, MAX_BEAM_HOPS, MAX_BEAM_PARENT_BYTES, MAX_BEAM_RETAINED_BYTES,
    MAX_BEAM_VECTOR_EXPANSIONS, MAX_BEAM_VISITED_KEYS, MAX_BEAM_WIDTH, PathPatternState,
    TopologyNodeId,
};

#[cfg(test)]
mod tests;
