//! Bounded query work projection and adaptive expansion policy.

use context_core::SearchLimit;

use crate::{QueryError, Result};

/// Default maximum exact MaxSim comparisons accepted by SQL adapters.
pub const MAX_LATE_INTERACTION_COMPARISONS: usize = 1_000_000;

/// One bounded candidate-expansion decision after authoritative recheck.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateExpansionDecision {
    /// Stop because the result target, source, or expansion ceiling is complete.
    Complete,
    /// Request another candidate page at the specified total batch size.
    Expand {
        /// Bounded total candidate count to request next.
        next_batch_size: usize,
    },
}

/// Chooses whether a filtered candidate query needs another bounded ANN page.
#[must_use]
pub fn candidate_expansion_decision(
    limit: SearchLimit,
    survivors: usize,
    candidates_seen: usize,
    ann_exhausted: bool,
    expansion_limit: usize,
) -> CandidateExpansionDecision {
    if survivors >= limit.get() || ann_exhausted || candidates_seen >= expansion_limit {
        return CandidateExpansionDecision::Complete;
    }
    let next_batch_size = candidates_seen
        .saturating_mul(2)
        .max(limit.get().saturating_mul(2))
        .max(candidates_seen.saturating_add(1))
        .min(expansion_limit);
    if next_batch_size <= candidates_seen {
        CandidateExpansionDecision::Complete
    } else {
        CandidateExpansionDecision::Expand { next_batch_size }
    }
}

/// Checked late-interaction work projection accepted by application policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LateInteractionWork {
    projected_comparisons: usize,
}

impl LateInteractionWork {
    /// Validates a late-interaction comparison projection against a hard limit.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::ArithmeticOverflow`] when the projection cannot be
    /// represented and [`QueryError::WorkBudgetExceeded`] when it exceeds the
    /// supplied comparison budget.
    pub fn new(
        query_vector_count: usize,
        candidate_vector_count: usize,
        comparison_budget: usize,
    ) -> Result<Self> {
        let projected_comparisons = query_vector_count
            .checked_mul(candidate_vector_count)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_comparison_projection",
            })?;
        if projected_comparisons > comparison_budget {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "late_interaction_comparisons",
                actual: projected_comparisons,
                maximum: comparison_budget,
            });
        }
        Ok(Self {
            projected_comparisons,
        })
    }

    /// Returns the checked projected comparison count.
    #[must_use]
    pub const fn projected_comparisons(self) -> usize {
        self.projected_comparisons
    }
}
