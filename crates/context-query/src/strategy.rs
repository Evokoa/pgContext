//! Pure application strategy selection independent of index implementations.

use context_core::SearchLimit;

use crate::{QueryError, Result};

/// Selected filtered ANN execution strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilteredAnnStrategyKind {
    /// Use exact search without ANN.
    Exact,
    /// Apply the filter first and exact-score the surviving points.
    FilterFirstExact,
    /// Apply the filter first and use ANN over the filtered candidate set.
    FilterFirstAnn,
    /// Search ANN first, then recheck filtered survivors in batches.
    AnnFirstRecheck,
    /// Cooperate between filtered candidates and ANN expansion.
    HybridCooperative,
    /// Search a partition-local index or candidate set.
    PartitionLocalSearch,
}

/// Structured reason contributing to a filtered ANN strategy decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilteredAnnReason {
    /// No ANN serving path is available.
    NoAnnIndex,
    /// The query has no filter predicate.
    NoFilter,
    /// The collection or source table has no searchable rows.
    EmptyCollection,
    /// The filter matches no rows.
    EmptyFilter,
    /// Filtered candidates fit below the exact-search cutoff.
    FilterFitsExactCutoff,
    /// Filtered candidates fit inside the ANN candidate budget.
    FilterFitsCandidateBudget,
    /// Filter selectivity is broad enough that ANN-first recheck is preferred.
    BroadFilter,
    /// Filter selectivity requires cooperative expansion.
    IntermediateFilter,
    /// A partition-local path is available for the filter.
    PartitionLocal,
}

/// Validated inputs used to choose a filtered ANN execution strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilteredAnnStrategyInput {
    total_points: usize,
    filter_matches: Option<usize>,
    hnsw_available: bool,
    partition_local: bool,
    candidate_budget: usize,
    exact_cutoff: usize,
    limit: SearchLimit,
}

impl FilteredAnnStrategyInput {
    /// Creates validated strategy-selection inputs.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when filter cardinality is
    /// impossible or a required policy budget is zero.
    pub fn new(
        total_points: usize,
        filter_matches: Option<usize>,
        hnsw_available: bool,
        partition_local: bool,
        candidate_budget: usize,
        exact_cutoff: usize,
        limit: SearchLimit,
    ) -> Result<Self> {
        if let Some(matches) = filter_matches
            && matches > total_points
        {
            return Err(invalid_parameter("filter_matches", matches));
        }
        if candidate_budget == 0 {
            return Err(invalid_parameter("candidate_budget", candidate_budget));
        }
        if exact_cutoff == 0 {
            return Err(invalid_parameter("exact_cutoff", exact_cutoff));
        }
        Ok(Self {
            total_points,
            filter_matches,
            hnsw_available,
            partition_local,
            candidate_budget,
            exact_cutoff,
            limit,
        })
    }

    /// Returns the total searchable point count.
    #[must_use]
    pub const fn total_points(self) -> usize {
        self.total_points
    }

    /// Returns estimated or known filter matches.
    #[must_use]
    pub const fn filter_matches(self) -> Option<usize> {
        self.filter_matches
    }

    /// Returns whether an ANN serving path is available.
    #[must_use]
    pub const fn hnsw_available(self) -> bool {
        self.hnsw_available
    }

    /// Returns whether a partition-local path is available.
    #[must_use]
    pub const fn partition_local(self) -> bool {
        self.partition_local
    }

    /// Returns the ANN candidate budget.
    #[must_use]
    pub const fn candidate_budget(self) -> usize {
        self.candidate_budget
    }

    /// Returns the exact-search cutoff.
    #[must_use]
    pub const fn exact_cutoff(self) -> usize {
        self.exact_cutoff
    }

    /// Returns the requested result limit.
    #[must_use]
    pub const fn limit(self) -> SearchLimit {
        self.limit
    }
}

/// Filtered ANN strategy selection with machine-readable reasons.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilteredAnnStrategy {
    kind: FilteredAnnStrategyKind,
    reasons: Vec<FilteredAnnReason>,
}

impl FilteredAnnStrategy {
    fn new(kind: FilteredAnnStrategyKind, reasons: Vec<FilteredAnnReason>) -> Self {
        Self { kind, reasons }
    }

    /// Returns the selected strategy kind.
    #[must_use]
    pub const fn kind(&self) -> FilteredAnnStrategyKind {
        self.kind
    }

    /// Returns structured reasons for the selection.
    #[must_use]
    pub fn reasons(&self) -> &[FilteredAnnReason] {
        &self.reasons
    }
}

/// Selected multi-vector ANN planning outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MultiVectorAnnStrategyKind {
    /// No active points need ANN or exact work.
    ExactNoOp,
    /// Use the exact table-backed MaxSim path.
    ExactTableScan,
    /// Reject planning because projected work violates policy.
    Rejected,
    /// ANN metadata exists, but serving is not query-ready.
    PlannedNotServingReady,
    /// Use ANN for candidates before exact MaxSim rerank.
    AnnCandidateServing,
}

/// Structured reason contributing to a multi-vector strategy decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MultiVectorAnnReason {
    /// The collection has no active points.
    EmptyCollection,
    /// No ANN serving path exists for table-backed multi-vector queries.
    NoAnnServingPath,
    /// Projected MaxSim comparisons exceed the configured budget.
    ComparisonBudgetExceeded,
    /// ANN metadata exists, but approximate serving is not ready.
    AnnMetadataNotServingReady,
    /// ANN candidate serving is available.
    AnnCandidateServingReady,
}

/// Validated inputs used to choose a multi-vector ANN strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MultiVectorAnnStrategyInput {
    active_points: usize,
    candidate_vectors: usize,
    query_vectors: usize,
    ann_metadata_available: bool,
    ann_candidate_serving_available: bool,
    candidate_budget: usize,
    comparison_budget: usize,
}

impl MultiVectorAnnStrategyInput {
    /// Creates validated multi-vector strategy-selection inputs.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for impossible counts or zero
    /// budgets, and [`QueryError::ArithmeticOverflow`] when projected work
    /// cannot be represented.
    pub fn new(
        active_points: usize,
        candidate_vectors: usize,
        query_vectors: usize,
        ann_metadata_available: bool,
        ann_candidate_serving_available: bool,
        candidate_budget: usize,
        comparison_budget: usize,
    ) -> Result<Self> {
        if query_vectors == 0 {
            return Err(invalid_parameter("query_vectors", query_vectors));
        }
        if candidate_budget == 0 {
            return Err(invalid_parameter("candidate_budget", candidate_budget));
        }
        if comparison_budget == 0 {
            return Err(invalid_parameter("comparison_budget", comparison_budget));
        }
        if active_points == 0 && candidate_vectors > 0 {
            return Err(invalid_parameter("candidate_vectors", candidate_vectors));
        }
        if candidate_vectors > candidate_budget {
            return Err(invalid_parameter("candidate_vectors", candidate_vectors));
        }
        if query_vectors.checked_mul(candidate_vectors).is_none() {
            return Err(QueryError::ArithmeticOverflow {
                operation: "multi_vector_comparison_projection",
            });
        }
        Ok(Self {
            active_points,
            candidate_vectors,
            query_vectors,
            ann_metadata_available,
            ann_candidate_serving_available,
            candidate_budget,
            comparison_budget,
        })
    }

    /// Returns the active point count.
    #[must_use]
    pub const fn active_points(self) -> usize {
        self.active_points
    }

    /// Returns the active candidate-vector count.
    #[must_use]
    pub const fn candidate_vectors(self) -> usize {
        self.candidate_vectors
    }

    /// Returns the query-vector count.
    #[must_use]
    pub const fn query_vectors(self) -> usize {
        self.query_vectors
    }

    /// Returns whether ANN metadata exists.
    #[must_use]
    pub const fn ann_metadata_available(self) -> bool {
        self.ann_metadata_available
    }

    /// Returns whether ANN candidate serving is ready.
    #[must_use]
    pub const fn ann_candidate_serving_available(self) -> bool {
        self.ann_candidate_serving_available
    }

    /// Returns the candidate-vector budget.
    #[must_use]
    pub const fn candidate_budget(self) -> usize {
        self.candidate_budget
    }

    /// Returns the MaxSim comparison budget.
    #[must_use]
    pub const fn comparison_budget(self) -> usize {
        self.comparison_budget
    }
}

/// Multi-vector ANN strategy selection with machine-readable reasons.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultiVectorAnnStrategy {
    kind: MultiVectorAnnStrategyKind,
    reasons: Vec<MultiVectorAnnReason>,
    projected_comparisons: usize,
}

impl MultiVectorAnnStrategy {
    fn new(
        kind: MultiVectorAnnStrategyKind,
        reasons: Vec<MultiVectorAnnReason>,
        projected_comparisons: usize,
    ) -> Self {
        Self {
            kind,
            reasons,
            projected_comparisons,
        }
    }

    /// Returns the selected strategy kind.
    #[must_use]
    pub const fn kind(&self) -> MultiVectorAnnStrategyKind {
        self.kind
    }

    /// Returns structured reasons for the selection.
    #[must_use]
    pub fn reasons(&self) -> &[MultiVectorAnnReason] {
        &self.reasons
    }

    /// Returns projected MaxSim comparison count.
    #[must_use]
    pub const fn projected_comparisons(&self) -> usize {
        self.projected_comparisons
    }
}

/// Selects a typed filtered ANN strategy with structured reasons.
#[must_use]
pub fn select_filtered_ann_strategy(input: FilteredAnnStrategyInput) -> FilteredAnnStrategy {
    if input.total_points == 0 {
        return FilteredAnnStrategy::new(
            FilteredAnnStrategyKind::Exact,
            vec![FilteredAnnReason::EmptyCollection],
        );
    }
    let Some(filter_matches) = input.filter_matches else {
        return if input.hnsw_available {
            FilteredAnnStrategy::new(
                FilteredAnnStrategyKind::AnnFirstRecheck,
                vec![FilteredAnnReason::NoFilter],
            )
        } else {
            FilteredAnnStrategy::new(
                FilteredAnnStrategyKind::Exact,
                vec![FilteredAnnReason::NoFilter, FilteredAnnReason::NoAnnIndex],
            )
        };
    };
    if filter_matches == 0 {
        return FilteredAnnStrategy::new(
            FilteredAnnStrategyKind::FilterFirstExact,
            vec![FilteredAnnReason::EmptyFilter],
        );
    }
    if !input.hnsw_available {
        return FilteredAnnStrategy::new(
            FilteredAnnStrategyKind::FilterFirstExact,
            vec![FilteredAnnReason::NoAnnIndex],
        );
    }
    if input.partition_local {
        return FilteredAnnStrategy::new(
            FilteredAnnStrategyKind::PartitionLocalSearch,
            vec![FilteredAnnReason::PartitionLocal],
        );
    }
    if filter_matches <= input.exact_cutoff || filter_matches <= input.limit.get() {
        return FilteredAnnStrategy::new(
            FilteredAnnStrategyKind::FilterFirstExact,
            vec![FilteredAnnReason::FilterFitsExactCutoff],
        );
    }
    if filter_matches <= input.candidate_budget {
        return FilteredAnnStrategy::new(
            FilteredAnnStrategyKind::FilterFirstAnn,
            vec![FilteredAnnReason::FilterFitsCandidateBudget],
        );
    }
    if filter_matches.saturating_mul(2) >= input.total_points {
        return FilteredAnnStrategy::new(
            FilteredAnnStrategyKind::AnnFirstRecheck,
            vec![FilteredAnnReason::BroadFilter],
        );
    }
    FilteredAnnStrategy::new(
        FilteredAnnStrategyKind::HybridCooperative,
        vec![FilteredAnnReason::IntermediateFilter],
    )
}

/// Selects a typed multi-vector ANN strategy with structured reasons.
#[must_use]
pub fn select_multi_vector_ann_strategy(
    input: MultiVectorAnnStrategyInput,
) -> MultiVectorAnnStrategy {
    let projected_comparisons = input.query_vectors * input.candidate_vectors;
    if input.active_points == 0 {
        return MultiVectorAnnStrategy::new(
            MultiVectorAnnStrategyKind::ExactNoOp,
            vec![MultiVectorAnnReason::EmptyCollection],
            projected_comparisons,
        );
    }
    if projected_comparisons > input.comparison_budget {
        return MultiVectorAnnStrategy::new(
            MultiVectorAnnStrategyKind::Rejected,
            vec![MultiVectorAnnReason::ComparisonBudgetExceeded],
            projected_comparisons,
        );
    }
    if input.ann_candidate_serving_available {
        return MultiVectorAnnStrategy::new(
            MultiVectorAnnStrategyKind::AnnCandidateServing,
            vec![MultiVectorAnnReason::AnnCandidateServingReady],
            projected_comparisons,
        );
    }
    if input.ann_metadata_available {
        return MultiVectorAnnStrategy::new(
            MultiVectorAnnStrategyKind::PlannedNotServingReady,
            vec![MultiVectorAnnReason::AnnMetadataNotServingReady],
            projected_comparisons,
        );
    }
    MultiVectorAnnStrategy::new(
        MultiVectorAnnStrategyKind::ExactTableScan,
        vec![MultiVectorAnnReason::NoAnnServingPath],
        projected_comparisons,
    )
}

fn invalid_parameter(field: &'static str, value: usize) -> QueryError {
    QueryError::InvalidInput {
        field,
        reason: format!("invalid value: {value}"),
    }
}
