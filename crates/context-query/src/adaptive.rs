//! Adaptive-dimension candidate selection and bounded widening.
//!
//! A Matryoshka prefix makes candidate generation cheaper, never more
//! permissive: the executor still rechecks and reranks candidates against the
//! full authoritative dimensions. These decisions therefore only choose how
//! wide the *candidate* stage reads, and every one of them is bounded and
//! carries a stable reason.

use context_core::{MatryoshkaPolicy, PrefixDimensions, SearchLimit};

use crate::{QueryError, Result};

/// Multiplier applied to the requested limit when sizing a prefix probe.
///
/// A prefix distance is an approximation of the full-dimension distance, so the
/// probe must admit more candidates than the caller asked for and let the
/// authoritative rerank choose among them.
pub const ADAPTIVE_PREFIX_OVERSAMPLE: usize = 4;

/// Caller-supplied control over adaptive-dimension candidate generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptivePrefixControl {
    /// Choose a prefix automatically from the declared policy.
    Automatic,
    /// Read the full authoritative dimensions for candidate generation.
    Disabled,
    /// Use exactly this declared prefix, or fail closed if it is undeclared.
    Pinned(PrefixDimensions),
}

/// Stable reason recorded alongside an adaptive-dimension decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptivePrefixReason {
    /// The caller disabled adaptive-dimension candidate generation.
    Disabled,
    /// The profile certifies no Matryoshka prefix.
    NoPolicy,
    /// The caller pinned a declared prefix.
    Pinned,
    /// The caller pinned a prefix the profile does not declare.
    PinRejected,
    /// No declared prefix fits the remaining candidate budget.
    BudgetTooSmall,
    /// A declared prefix was selected automatically.
    Selected,
}

impl AdaptivePrefixReason {
    /// Returns the bounded stable diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NoPolicy => "no_policy",
            Self::Pinned => "pinned",
            Self::PinRejected => "pin_rejected",
            Self::BudgetTooSmall => "budget_too_small",
            Self::Selected => "selected",
        }
    }
}

/// Candidate-stage read width selected for one execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptivePrefixStrategyKind {
    /// Read the full authoritative dimensions.
    FullVector {
        /// Candidates admitted from the full-dimension probe.
        candidates: usize,
    },
    /// Read a declared prefix.
    Prefix {
        /// Declared prefix dimension read by the candidate stage.
        dimensions: PrefixDimensions,
        /// Candidates admitted from the prefix probe.
        candidates: usize,
    },
}

/// Selected adaptive-dimension strategy and its reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdaptivePrefixStrategy {
    kind: AdaptivePrefixStrategyKind,
    reason: AdaptivePrefixReason,
}

impl AdaptivePrefixStrategy {
    /// Returns the selected candidate-stage read width.
    #[must_use]
    pub const fn kind(self) -> AdaptivePrefixStrategyKind {
        self.kind
    }

    /// Returns the stable selection reason.
    #[must_use]
    pub const fn reason(self) -> AdaptivePrefixReason {
        self.reason
    }

    /// Returns the prefix dimension when one was selected.
    #[must_use]
    pub const fn prefix(self) -> Option<PrefixDimensions> {
        match self.kind {
            AdaptivePrefixStrategyKind::Prefix { dimensions, .. } => Some(dimensions),
            AdaptivePrefixStrategyKind::FullVector { .. } => None,
        }
    }

    /// Returns the candidate count admitted by the selected probe.
    #[must_use]
    pub const fn candidates(self) -> usize {
        match self.kind {
            AdaptivePrefixStrategyKind::Prefix { candidates, .. }
            | AdaptivePrefixStrategyKind::FullVector { candidates } => candidates,
        }
    }
}

/// Validated inputs for adaptive-dimension strategy selection.
#[derive(Clone, Copy, Debug)]
pub struct AdaptivePrefixStrategyInput<'a> {
    policy: Option<&'a MatryoshkaPolicy>,
    control: AdaptivePrefixControl,
    limit: SearchLimit,
    candidate_budget: usize,
}

impl<'a> AdaptivePrefixStrategyInput<'a> {
    /// Creates validated adaptive-dimension selection inputs.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the candidate budget is zero.
    pub fn new(
        policy: Option<&'a MatryoshkaPolicy>,
        control: AdaptivePrefixControl,
        limit: SearchLimit,
        candidate_budget: usize,
    ) -> Result<Self> {
        if candidate_budget == 0 {
            return Err(QueryError::InvalidInput {
                field: "candidate_budget",
                reason: "must be positive".to_owned(),
            });
        }
        Ok(Self {
            policy,
            control,
            limit,
            candidate_budget,
        })
    }
}

/// Selects the candidate-stage read width for one adaptive-dimension execution.
///
/// Selection never widens the answer: the executor reranks the admitted
/// candidates against the full authoritative dimensions either way. A prefix is
/// chosen only when the oversampled probe still fits the remaining candidate
/// budget, so a prefix can never cost more candidate work than the full-vector
/// path it replaces.
#[must_use]
pub fn select_adaptive_prefix_strategy(
    input: AdaptivePrefixStrategyInput<'_>,
) -> AdaptivePrefixStrategy {
    let full = |reason| AdaptivePrefixStrategy {
        kind: AdaptivePrefixStrategyKind::FullVector {
            candidates: input.limit.get().min(input.candidate_budget),
        },
        reason,
    };
    let Some(policy) = input.policy else {
        return full(AdaptivePrefixReason::NoPolicy);
    };
    let probe = oversampled_probe(input.limit, input.candidate_budget);
    match input.control {
        AdaptivePrefixControl::Disabled => full(AdaptivePrefixReason::Disabled),
        AdaptivePrefixControl::Pinned(prefix) => {
            if policy.declares(prefix) {
                AdaptivePrefixStrategy {
                    kind: AdaptivePrefixStrategyKind::Prefix {
                        dimensions: prefix,
                        candidates: probe,
                    },
                    reason: AdaptivePrefixReason::Pinned,
                }
            } else {
                full(AdaptivePrefixReason::PinRejected)
            }
        }
        AdaptivePrefixControl::Automatic => {
            if probe <= input.limit.get() {
                // The budget leaves no room to oversample, so a prefix probe
                // could not be rechecked into the same answer.
                return full(AdaptivePrefixReason::BudgetTooSmall);
            }
            match policy.prefixes().first().copied() {
                Some(dimensions) => AdaptivePrefixStrategy {
                    kind: AdaptivePrefixStrategyKind::Prefix {
                        dimensions,
                        candidates: probe,
                    },
                    reason: AdaptivePrefixReason::Selected,
                },
                None => full(AdaptivePrefixReason::NoPolicy),
            }
        }
    }
}

fn oversampled_probe(limit: SearchLimit, candidate_budget: usize) -> usize {
    limit
        .get()
        .saturating_mul(ADAPTIVE_PREFIX_OVERSAMPLE)
        .min(candidate_budget)
        .max(1)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use context_core::{MatryoshkaPolicy, PrefixDimensions, VectorNormalization};

    fn policy() -> MatryoshkaPolicy {
        MatryoshkaPolicy::new(
            768,
            [128, 256, 512]
                .into_iter()
                .map(|value| PrefixDimensions::new(value).expect("prefix"))
                .collect(),
            VectorNormalization::None,
        )
        .expect("policy")
    }

    fn limit(value: usize) -> SearchLimit {
        SearchLimit::new(value).expect("limit")
    }

    fn select(
        policy: Option<&MatryoshkaPolicy>,
        control: AdaptivePrefixControl,
        limit_value: usize,
        budget: usize,
    ) -> AdaptivePrefixStrategy {
        select_adaptive_prefix_strategy(
            AdaptivePrefixStrategyInput::new(policy, control, limit(limit_value), budget)
                .expect("inputs"),
        )
    }

    #[test]
    fn a_zero_candidate_budget_is_rejected() {
        assert!(
            AdaptivePrefixStrategyInput::new(None, AdaptivePrefixControl::Automatic, limit(10), 0)
                .is_err()
        );
    }

    #[test]
    fn profiles_without_a_policy_read_the_full_dimensions() {
        let strategy = select(None, AdaptivePrefixControl::Automatic, 10, 1000);
        assert_eq!(strategy.reason(), AdaptivePrefixReason::NoPolicy);
        assert_eq!(strategy.prefix(), None);
    }

    #[test]
    fn disabling_adaptive_dimensions_reads_the_full_dimensions() {
        let policy = policy();
        let strategy = select(Some(&policy), AdaptivePrefixControl::Disabled, 10, 1000);
        assert_eq!(strategy.reason(), AdaptivePrefixReason::Disabled);
        assert_eq!(strategy.prefix(), None);
    }

    #[test]
    fn automatic_selection_takes_the_narrowest_declared_prefix() {
        let policy = policy();
        let strategy = select(Some(&policy), AdaptivePrefixControl::Automatic, 10, 1000);
        assert_eq!(strategy.reason(), AdaptivePrefixReason::Selected);
        assert_eq!(strategy.prefix().map(PrefixDimensions::get), Some(128));
        assert_eq!(strategy.candidates(), 40);
    }

    #[test]
    fn pinning_uses_a_declared_prefix_and_rejects_an_undeclared_one() {
        let policy = policy();
        let pinned = select(
            Some(&policy),
            AdaptivePrefixControl::Pinned(PrefixDimensions::new(512).expect("prefix")),
            10,
            1000,
        );
        assert_eq!(pinned.reason(), AdaptivePrefixReason::Pinned);
        assert_eq!(pinned.prefix().map(PrefixDimensions::get), Some(512));

        let rejected = select(
            Some(&policy),
            AdaptivePrefixControl::Pinned(PrefixDimensions::new(300).expect("prefix")),
            10,
            1000,
        );
        assert_eq!(rejected.reason(), AdaptivePrefixReason::PinRejected);
        assert_eq!(rejected.prefix(), None);
    }

    #[test]
    fn a_budget_without_oversampling_room_reads_the_full_dimensions() {
        let policy = policy();
        let strategy = select(Some(&policy), AdaptivePrefixControl::Automatic, 10, 10);
        assert_eq!(strategy.reason(), AdaptivePrefixReason::BudgetTooSmall);
        assert_eq!(strategy.prefix(), None);
    }

    #[test]
    fn a_prefix_probe_never_exceeds_the_candidate_budget() {
        let policy = policy();
        let strategy = select(Some(&policy), AdaptivePrefixControl::Automatic, 100, 150);
        assert!(strategy.candidates() <= 150);
    }
}
