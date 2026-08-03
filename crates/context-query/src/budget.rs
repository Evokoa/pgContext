//! Bounded execution accounting.

use crate::{QueryError, Result};
use context_core::policy::{
    MAX_HNSW_CANDIDATE_MASK_POINTS, MAX_QUERY_EXPANSIONS, MAX_QUERY_STAGES,
    MAX_RECALL_CHECK_POINT_IDS, MAX_SEARCH_LIMIT,
};

/// Maximum comparisons accepted by one typed query execution.
pub const MAX_QUERY_COMPARISONS: usize = 10_000_000;
/// Maximum accounted extension-owned transient memory accepted by one query execution.
pub const MAX_QUERY_MEMORY_BYTES: usize = 256 * 1024 * 1024;
/// Maximum hydrated source-key bytes accepted by one query execution.
pub const MAX_QUERY_HYDRATION_BYTES: usize = 64 * 1024 * 1024;
/// Maximum elapsed execution allowance in microseconds.
pub const MAX_QUERY_ELAPSED_MICROS: u64 = 60_000_000;
/// Default comparison allowance used by production executors.
pub const DEFAULT_QUERY_COMPARISONS: usize = 1_000_000;
/// Default extension-owned transient-memory allowance.
pub const DEFAULT_QUERY_MEMORY_BYTES: usize = 16 * 1024 * 1024;
/// Default hydrated source-key byte allowance.
pub const DEFAULT_QUERY_HYDRATION_BYTES: usize = 8 * 1024 * 1024;
/// Default elapsed execution allowance in microseconds.
pub const DEFAULT_QUERY_ELAPSED_MICROS: u64 = 500_000;

/// Hard limits applied to one query execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionBudget {
    max_candidates: usize,
    max_filter_candidates: usize,
    max_rechecks: usize,
    max_stages: usize,
    max_expansions: usize,
    max_results: usize,
    max_comparisons: usize,
    max_memory_bytes: usize,
    max_hydration_bytes: usize,
    max_elapsed_micros: u64,
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
            max_comparisons: DEFAULT_QUERY_COMPARISONS,
            max_memory_bytes: DEFAULT_QUERY_MEMORY_BYTES,
            max_hydration_bytes: DEFAULT_QUERY_HYDRATION_BYTES,
            max_elapsed_micros: DEFAULT_QUERY_ELAPSED_MICROS,
        })
    }

    /// Replaces extended resource limits after applying global policy ceilings.
    pub fn with_resource_limits(
        mut self,
        max_comparisons: usize,
        max_memory_bytes: usize,
        max_hydration_bytes: usize,
        max_elapsed_micros: u64,
    ) -> Result<Self> {
        let limits = [
            ("max_comparisons", max_comparisons, MAX_QUERY_COMPARISONS),
            ("max_memory_bytes", max_memory_bytes, MAX_QUERY_MEMORY_BYTES),
            (
                "max_hydration_bytes",
                max_hydration_bytes,
                MAX_QUERY_HYDRATION_BYTES,
            ),
        ];
        if let Some((field, value, maximum)) = limits
            .into_iter()
            .find(|(_, value, maximum)| *value == 0 || value > maximum)
        {
            return Err(QueryError::InvalidInput {
                field,
                reason: format!("must be within 1..={maximum}; received {value}"),
            });
        }
        if max_elapsed_micros == 0 || max_elapsed_micros > MAX_QUERY_ELAPSED_MICROS {
            return Err(QueryError::InvalidInput {
                field: "max_elapsed_micros",
                reason: format!(
                    "must be within 1..={MAX_QUERY_ELAPSED_MICROS}; received {max_elapsed_micros}"
                ),
            });
        }
        self.max_comparisons = max_comparisons;
        self.max_memory_bytes = max_memory_bytes;
        self.max_hydration_bytes = max_hydration_bytes;
        self.max_elapsed_micros = max_elapsed_micros;
        Ok(self)
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

    /// Returns the maximum final result count.
    #[must_use]
    pub const fn max_results(self) -> usize {
        self.max_results
    }

    /// Returns the maximum comparison count.
    #[must_use]
    pub const fn max_comparisons(self) -> usize {
        self.max_comparisons
    }

    /// Returns the maximum accounted transient bytes.
    #[must_use]
    pub const fn max_memory_bytes(self) -> usize {
        self.max_memory_bytes
    }

    /// Returns the maximum hydrated source-key bytes.
    #[must_use]
    pub const fn max_hydration_bytes(self) -> usize {
        self.max_hydration_bytes
    }

    /// Returns the maximum wall-clock allowance in microseconds.
    #[must_use]
    pub const fn max_elapsed_micros(self) -> u64 {
        self.max_elapsed_micros
    }

    pub(crate) const fn exhausted(self, usage: BudgetUsage) -> bool {
        usage.comparisons > self.max_comparisons
            || usage.memory_bytes > self.max_memory_bytes
            || usage.hydration_bytes > self.max_hydration_bytes
            || usage.elapsed_micros >= self.max_elapsed_micros
    }

    pub(crate) const fn resources_depleted(self, usage: BudgetUsage) -> bool {
        usage.comparisons >= self.max_comparisons
            || usage.memory_bytes >= self.max_memory_bytes
            || usage.hydration_bytes >= self.max_hydration_bytes
            || usage.elapsed_micros >= self.max_elapsed_micros
    }

    pub(crate) fn remaining(self, usage: BudgetUsage, query_has_filter: bool) -> Option<Self> {
        let candidates = self.max_candidates.checked_sub(usage.candidates)?;
        let filters = self
            .max_filter_candidates
            .checked_sub(usage.filter_candidates)?;
        let rechecks = self.max_rechecks.checked_sub(usage.rechecks)?;
        let stages = self.max_stages.checked_sub(usage.stages)?;
        let expansions = self.max_expansions.checked_sub(usage.expansions)?;
        let comparisons = self.max_comparisons.checked_sub(usage.comparisons)?;
        let memory_bytes = self.max_memory_bytes.checked_sub(usage.memory_bytes)?;
        let hydration_bytes = self
            .max_hydration_bytes
            .checked_sub(usage.hydration_bytes)?;
        let elapsed_micros = self.max_elapsed_micros;
        if candidates == 0
            || rechecks == 0
            || stages == 0
            || expansions == 0
            || comparisons == 0
            || memory_bytes == 0
            || hydration_bytes == 0
            || (query_has_filter && filters == 0)
        {
            return None;
        }
        Some(Self {
            max_candidates: candidates,
            max_filter_candidates: filters.max(1),
            max_rechecks: rechecks,
            max_stages: stages,
            max_expansions: expansions,
            max_results: self.max_results,
            max_comparisons: comparisons,
            max_memory_bytes: memory_bytes,
            max_hydration_bytes: hydration_bytes,
            max_elapsed_micros: elapsed_micros,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::{BudgetUsage, ExecutionBudget};

    #[test]
    fn inclusive_resource_maxima_are_not_exhausted() {
        let budget = ExecutionBudget::new(8, 8, 8, 8, 4, 4)
            .expect("base budget")
            .with_resource_limits(7, 11, 13, 17)
            .expect("resource limits");
        let usage = BudgetUsage {
            comparisons: 7,
            memory_bytes: 11,
            hydration_bytes: 13,
            elapsed_micros: 16,
            ..BudgetUsage::default()
        };

        assert!(!budget.exhausted(usage));
        assert!(budget.resources_depleted(usage));
        assert!(budget.exhausted(BudgetUsage {
            comparisons: 8,
            ..usage
        }));
        assert!(budget.exhausted(BudgetUsage {
            memory_bytes: 12,
            ..usage
        }));
        assert!(budget.exhausted(BudgetUsage {
            hydration_bytes: 14,
            ..usage
        }));
        assert!(budget.exhausted(BudgetUsage {
            elapsed_micros: 17,
            ..usage
        }));
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
    comparisons: usize,
    memory_bytes: usize,
    hydration_bytes: usize,
    elapsed_micros: u64,
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

    /// Returns score/filter/formula comparisons performed.
    #[must_use]
    pub const fn comparisons(self) -> usize {
        self.comparisons
    }

    /// Returns accounted transient allocation bytes.
    #[must_use]
    pub const fn memory_bytes(self) -> usize {
        self.memory_bytes
    }

    /// Returns hydrated source-key bytes.
    #[must_use]
    pub const fn hydration_bytes(self) -> usize {
        self.hydration_bytes
    }

    /// Returns elapsed microseconds reported by the query clock.
    #[must_use]
    pub const fn elapsed_micros(self) -> u64 {
        self.elapsed_micros
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

    pub(crate) fn add_expansions(&mut self, count: usize) {
        self.expansions = self.expansions.saturating_add(count);
    }

    pub(crate) fn add_comparisons(&mut self, count: usize) {
        self.comparisons = self.comparisons.saturating_add(count);
    }

    pub(crate) fn add_memory_bytes(&mut self, count: usize) {
        self.memory_bytes = self.memory_bytes.saturating_add(count);
    }

    pub(crate) fn add_hydration_bytes(&mut self, count: usize) {
        self.hydration_bytes = self.hydration_bytes.saturating_add(count);
    }

    pub(crate) fn set_elapsed_micros(&mut self, elapsed_micros: u64) {
        self.elapsed_micros = elapsed_micros;
    }

    pub(crate) fn merge(&mut self, other: Self) {
        self.filter_candidates = self
            .filter_candidates
            .saturating_add(other.filter_candidates);
        self.candidates = self.candidates.saturating_add(other.candidates);
        self.rechecks = self.rechecks.saturating_add(other.rechecks);
        self.stages = self.stages.saturating_add(other.stages);
        self.expansions = self.expansions.saturating_add(other.expansions);
        self.comparisons = self.comparisons.saturating_add(other.comparisons);
        self.memory_bytes = self.memory_bytes.saturating_add(other.memory_bytes);
        self.hydration_bytes = self.hydration_bytes.saturating_add(other.hydration_bytes);
        self.elapsed_micros = self.elapsed_micros.max(other.elapsed_micros);
    }
}
