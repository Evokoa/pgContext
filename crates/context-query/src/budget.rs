//! Bounded execution accounting.

use crate::{QueryError, Result};
use context_core::policy::{
    MAX_HNSW_CANDIDATE_MASK_POINTS, MAX_QUERY_EXPANSIONS, MAX_QUERY_STAGES,
    MAX_RECALL_CHECK_POINT_IDS, MAX_SEARCH_LIMIT,
};

/// Hard limits applied to one query execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionBudget {
    max_candidates: usize,
    max_filter_candidates: usize,
    max_rechecks: usize,
    max_stages: usize,
    max_expansions: usize,
    max_results: usize,
}

impl ExecutionBudget {
    /// Creates a policy-bounded, non-zero execution budget.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when any limit is zero or exceeds
    /// its shared policy ceiling.
    pub fn new(
        max_candidates: usize,
        max_filter_candidates: usize,
        max_rechecks: usize,
        max_stages: usize,
        max_expansions: usize,
        max_results: usize,
    ) -> Result<Self> {
        let values = [
            ("max_candidates", max_candidates),
            ("max_filter_candidates", max_filter_candidates),
            ("max_rechecks", max_rechecks),
            ("max_stages", max_stages),
            ("max_expansions", max_expansions),
            ("max_results", max_results),
        ];
        if let Some((field, _)) = values.into_iter().find(|(_, value)| *value == 0) {
            return Err(QueryError::InvalidInput {
                field,
                reason: "must be positive".to_owned(),
            });
        }
        let ceilings = [
            ("max_candidates", max_candidates, MAX_RECALL_CHECK_POINT_IDS),
            (
                "max_filter_candidates",
                max_filter_candidates,
                MAX_HNSW_CANDIDATE_MASK_POINTS,
            ),
            ("max_rechecks", max_rechecks, MAX_RECALL_CHECK_POINT_IDS),
            ("max_stages", max_stages, MAX_QUERY_STAGES),
            ("max_expansions", max_expansions, MAX_QUERY_EXPANSIONS),
            ("max_results", max_results, MAX_SEARCH_LIMIT),
        ];
        if let Some((field, value, ceiling)) = ceilings
            .into_iter()
            .find(|(_, value, ceiling)| value > ceiling)
        {
            return Err(QueryError::InvalidInput {
                field,
                reason: format!("{value} exceeds policy maximum {ceiling}"),
            });
        }
        Ok(Self {
            max_candidates,
            max_filter_candidates,
            max_rechecks,
            max_stages,
            max_expansions,
            max_results,
        })
    }

    pub(crate) const fn max_candidates(self) -> usize {
        self.max_candidates
    }

    pub(crate) const fn max_filter_candidates(self) -> usize {
        self.max_filter_candidates
    }

    pub(crate) const fn max_rechecks(self) -> usize {
        self.max_rechecks
    }

    pub(crate) const fn max_stages(self) -> usize {
        self.max_stages
    }

    /// Returns the maximum expansion count.
    #[must_use]
    pub const fn max_expansions(self) -> usize {
        self.max_expansions
    }

    pub(crate) const fn max_results(self) -> usize {
        self.max_results
    }
}

/// Work consumed by a query execution.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BudgetUsage {
    filter_candidates: usize,
    candidates: usize,
    rechecks: usize,
    stages: usize,
    expansions: usize,
}

impl BudgetUsage {
    /// Returns filter candidates materialized.
    #[must_use]
    pub const fn filter_candidates(self) -> usize {
        self.filter_candidates
    }

    /// Returns candidates materialized.
    #[must_use]
    pub const fn candidates(self) -> usize {
        self.candidates
    }

    /// Returns authoritative source rows rechecked.
    #[must_use]
    pub const fn rechecks(self) -> usize {
        self.rechecks
    }

    /// Returns completed stages.
    #[must_use]
    pub const fn stages(self) -> usize {
        self.stages
    }

    /// Returns adaptive expansion steps.
    #[must_use]
    pub const fn expansions(self) -> usize {
        self.expansions
    }

    pub(crate) fn add_filter_candidates(&mut self, count: usize) {
        self.filter_candidates = self.filter_candidates.saturating_add(count);
    }

    pub(crate) fn add_candidates(&mut self, count: usize) {
        self.candidates = self.candidates.saturating_add(count);
    }

    pub(crate) fn add_rechecks(&mut self, count: usize) {
        self.rechecks = self.rechecks.saturating_add(count);
    }

    pub(crate) fn add_stage(&mut self) {
        self.stages = self.stages.saturating_add(1);
    }
}
