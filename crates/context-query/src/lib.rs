//! Pure application boundary for pgContext query planning and execution.
//!
//! This crate owns logical query contracts and ports. PostgreSQL, pgrx, index
//! storage, artifact storage, and SQLSTATE translation remain adapter concerns.

#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]
#![warn(rustdoc::broken_intra_doc_links)]

mod budget;
mod error;
mod executor;
mod ir;
mod policy;
mod ports;
mod strategy;
mod types;
mod validation;

pub use budget::{BudgetUsage, ExecutionBudget};
pub use context_core::PointId;
pub use error::{QueryError, Result};
pub use executor::QueryExecutor;
pub use ir::{QueryIr, QueryKind};
pub use policy::{
    CandidateExpansionDecision, LateInteractionWork, MAX_LATE_INTERACTION_COMPARISONS,
    candidate_expansion_decision,
};
pub use ports::{
    Cancellation, CandidateSource, FilterCandidateSource, SourceRechecker, TelemetrySink,
};
pub use strategy::{
    FilteredAnnReason, FilteredAnnStrategy, FilteredAnnStrategyInput, FilteredAnnStrategyKind,
    MultiVectorAnnReason, MultiVectorAnnStrategy, MultiVectorAnnStrategyInput,
    MultiVectorAnnStrategyKind, select_filtered_ann_strategy, select_multi_vector_ann_strategy,
};
pub use types::{
    Candidate, CandidateBranch, CandidatePage, Completion, ExecutionOutcome, ExecutionState,
    FilterCandidateBatch, HydratedCandidate, ReadinessReason, ScoreOrder, SourceReadiness,
    StageDiagnostic, StageKind,
};
pub use validation::{Formula, MAX_FORMULA_BYTES, QueryPlanValidator};

/// Returns the version of the pure query boundary.
#[must_use]
pub const fn query_contract_version() -> u16 {
    1
}

#[cfg(test)]
mod tests {
    use super::{PointId, query_contract_version};

    #[test]
    fn query_boundary_uses_logical_point_ids() {
        let point_id = PointId::new(7);
        assert_eq!(point_id.get(), 7);
        assert_eq!(query_contract_version(), 1);
    }
}
