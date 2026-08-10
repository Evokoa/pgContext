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

/// Hard resources available to one adaptive widening schedule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdaptiveWideningBudget {
    max_candidates: usize,
    max_comparisons: usize,
    max_rechecks: usize,
    max_memory_bytes: usize,
    max_expansions: usize,
}

impl AdaptiveWideningBudget {
    /// Creates a widening budget.
    ///
    /// `max_expansions` may be zero for a one-stage exhaustive probe. Every
    /// other resource must be positive.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when a non-expansion resource is
    /// zero.
    pub fn new(
        max_candidates: usize,
        max_comparisons: usize,
        max_rechecks: usize,
        max_memory_bytes: usize,
        max_expansions: usize,
    ) -> Result<Self> {
        let resources = [
            ("max_candidates", max_candidates),
            ("max_comparisons", max_comparisons),
            ("max_rechecks", max_rechecks),
            ("max_memory_bytes", max_memory_bytes),
        ];
        if let Some((field, _)) = resources.into_iter().find(|(_, value)| *value == 0) {
            return Err(QueryError::InvalidInput {
                field,
                reason: "must be positive".to_owned(),
            });
        }
        Ok(Self {
            max_candidates,
            max_comparisons,
            max_rechecks,
            max_memory_bytes,
            max_expansions,
        })
    }
}

/// Validated inputs for a fail-closed adaptive widening schedule.
#[derive(Clone, Copy, Debug)]
pub struct AdaptiveWideningInput<'a> {
    policy: &'a MatryoshkaPolicy,
    selected_prefix: PrefixDimensions,
    limit: SearchLimit,
    visible_candidates: usize,
    bytes_per_candidate: usize,
    budget: AdaptiveWideningBudget,
}

impl<'a> AdaptiveWideningInput<'a> {
    /// Creates adaptive widening inputs.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when `selected_prefix` is not part
    /// of `policy` or the per-candidate memory projection is zero.
    pub fn new(
        policy: &'a MatryoshkaPolicy,
        selected_prefix: PrefixDimensions,
        limit: SearchLimit,
        visible_candidates: usize,
        bytes_per_candidate: usize,
        budget: AdaptiveWideningBudget,
    ) -> Result<Self> {
        if !policy.declares(selected_prefix) {
            return Err(QueryError::InvalidInput {
                field: "selected_prefix",
                reason: "must be declared by the Matryoshka policy".to_owned(),
            });
        }
        if bytes_per_candidate == 0 {
            return Err(QueryError::InvalidInput {
                field: "bytes_per_candidate",
                reason: "must be positive".to_owned(),
            });
        }
        Ok(Self {
            policy,
            selected_prefix,
            limit,
            visible_candidates,
            bytes_per_candidate,
            budget,
        })
    }
}

/// Why adaptive widening completed or selected full-vector exact fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveWideningTermination {
    /// The final prefix step admits every visible candidate.
    Exhaustive,
    /// No visible candidates require prefix work.
    EmptyCorpus,
    /// Materializing every widening step would exceed the candidate budget.
    CandidateBudget,
    /// Scoring every widening step would exceed the comparison budget.
    ComparisonBudget,
    /// The final authoritative rerank cannot admit every visible candidate.
    RecheckBudget,
    /// The exhaustive candidate page cannot fit the extension-owned memory budget.
    MemoryBudget,
    /// The required number of widening steps exceeds the expansion budget.
    ExpansionBudget,
}

impl AdaptiveWideningTermination {
    /// Returns the bounded stable diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Exhaustive => "exhaustive",
            Self::EmptyCorpus => "empty_corpus",
            Self::CandidateBudget => "candidate_budget",
            Self::ComparisonBudget => "comparison_budget",
            Self::RecheckBudget => "recheck_budget",
            Self::MemoryBudget => "memory_budget",
            Self::ExpansionBudget => "expansion_budget",
        }
    }
}

/// One monotonic prefix-width step in an exhaustive widening schedule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdaptiveWideningStep {
    dimensions: PrefixDimensions,
    candidate_limit: usize,
}

impl AdaptiveWideningStep {
    /// Returns the declared prefix dimension scored by this step.
    #[must_use]
    pub const fn dimensions(self) -> PrefixDimensions {
        self.dimensions
    }

    /// Returns the number of ranked candidates admitted by this step.
    #[must_use]
    pub const fn candidate_limit(self) -> usize {
        self.candidate_limit
    }
}

/// A preflighted widening schedule or a no-work full-vector fallback decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AdaptiveWideningPlan {
    steps: Vec<AdaptiveWideningStep>,
    termination: AdaptiveWideningTermination,
    candidate_work: usize,
    comparison_work: usize,
    recheck_work: usize,
    peak_memory_bytes: usize,
}

impl AdaptiveWideningPlan {
    fn fallback(termination: AdaptiveWideningTermination) -> Self {
        Self {
            steps: Vec::new(),
            termination,
            candidate_work: 0,
            comparison_work: 0,
            recheck_work: 0,
            peak_memory_bytes: 0,
        }
    }

    /// Returns the ordered widening steps.
    #[must_use]
    pub fn steps(&self) -> &[AdaptiveWideningStep] {
        &self.steps
    }

    /// Returns why the schedule completed or selected full-vector fallback.
    #[must_use]
    pub const fn termination(&self) -> AdaptiveWideningTermination {
        self.termination
    }

    /// Returns the total candidates materialized across all prefix steps.
    #[must_use]
    pub const fn candidate_work(&self) -> usize {
        self.candidate_work
    }

    /// Returns the total prefix comparisons projected across all steps.
    #[must_use]
    pub const fn comparison_work(&self) -> usize {
        self.comparison_work
    }

    /// Returns the final authoritative recheck cardinality.
    #[must_use]
    pub const fn recheck_work(&self) -> usize {
        self.recheck_work
    }

    /// Returns the peak extension-owned candidate memory projection.
    #[must_use]
    pub const fn peak_memory_bytes(&self) -> usize {
        self.peak_memory_bytes
    }

    /// Returns the number of steps after the initial prefix probe.
    #[must_use]
    pub fn expansion_count(&self) -> usize {
        self.steps.len().saturating_sub(1)
    }
}

/// Builds a monotonic exhaustive prefix schedule within every supplied budget.
///
/// A non-exhaustive prefix result is never returned as a complete plan. When
/// any resource cannot fund the whole schedule, the returned plan has no steps;
/// the adapter must choose full-vector exact search before performing prefix
/// work.
///
/// # Errors
///
/// Returns [`QueryError::ArithmeticOverflow`] when a work or memory projection
/// cannot be represented by `usize`.
pub fn plan_adaptive_widening(input: AdaptiveWideningInput<'_>) -> Result<AdaptiveWideningPlan> {
    if input.visible_candidates == 0 {
        return Ok(AdaptiveWideningPlan::fallback(
            AdaptiveWideningTermination::EmptyCorpus,
        ));
    }
    if input.visible_candidates > input.budget.max_rechecks {
        return Ok(AdaptiveWideningPlan::fallback(
            AdaptiveWideningTermination::RecheckBudget,
        ));
    }

    let peak_memory_bytes = input
        .visible_candidates
        .checked_mul(input.bytes_per_candidate)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "adaptive_widening_memory_projection",
        })?;
    if peak_memory_bytes > input.budget.max_memory_bytes {
        return Ok(AdaptiveWideningPlan::fallback(
            AdaptiveWideningTermination::MemoryBudget,
        ));
    }

    let first_prefix_index = input
        .policy
        .prefixes()
        .iter()
        .position(|prefix| *prefix == input.selected_prefix)
        .ok_or(QueryError::InvalidInput {
            field: "selected_prefix",
            reason: "must be declared by the Matryoshka policy".to_owned(),
        })?;
    let initial_width = oversampled_probe(input.limit, input.visible_candidates);
    let mut steps = vec![AdaptiveWideningStep {
        dimensions: input.policy.prefixes()[first_prefix_index],
        candidate_limit: initial_width,
    }];
    if initial_width < input.visible_candidates {
        let exhaustive_prefix_index = first_prefix_index
            .saturating_add(1)
            .min(input.policy.prefixes().len().saturating_sub(1));
        steps.push(AdaptiveWideningStep {
            dimensions: input.policy.prefixes()[exhaustive_prefix_index],
            candidate_limit: input.visible_candidates,
        });
    }

    let candidate_work = steps.iter().try_fold(0_usize, |total, step| {
        total
            .checked_add(step.candidate_limit)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "adaptive_widening_candidate_projection",
            })
    })?;
    if candidate_work > input.budget.max_candidates {
        return Ok(AdaptiveWideningPlan::fallback(
            AdaptiveWideningTermination::CandidateBudget,
        ));
    }

    let comparison_work = input.visible_candidates.checked_mul(steps.len()).ok_or(
        QueryError::ArithmeticOverflow {
            operation: "adaptive_widening_comparison_projection",
        },
    )?;
    if comparison_work > input.budget.max_comparisons {
        return Ok(AdaptiveWideningPlan::fallback(
            AdaptiveWideningTermination::ComparisonBudget,
        ));
    }

    if steps.len().saturating_sub(1) > input.budget.max_expansions {
        return Ok(AdaptiveWideningPlan::fallback(
            AdaptiveWideningTermination::ExpansionBudget,
        ));
    }

    Ok(AdaptiveWideningPlan {
        steps,
        termination: AdaptiveWideningTermination::Exhaustive,
        candidate_work,
        comparison_work,
        recheck_work: input.visible_candidates,
        peak_memory_bytes,
    })
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

    fn widening_budget(
        candidates: usize,
        comparisons: usize,
        rechecks: usize,
        memory_bytes: usize,
        expansions: usize,
    ) -> AdaptiveWideningBudget {
        AdaptiveWideningBudget::new(candidates, comparisons, rechecks, memory_bytes, expansions)
            .expect("widening budget")
    }

    fn widening_plan(
        visible_candidates: usize,
        bytes_per_candidate: usize,
        budget: AdaptiveWideningBudget,
    ) -> AdaptiveWideningPlan {
        let policy = policy();
        plan_adaptive_widening(
            AdaptiveWideningInput::new(
                &policy,
                PrefixDimensions::new(128).expect("prefix"),
                limit(10),
                visible_candidates,
                bytes_per_candidate,
                budget,
            )
            .expect("widening input"),
        )
        .expect("widening plan")
    }

    #[test]
    fn a_one_stage_plan_exhausts_a_corpus_within_the_initial_probe() {
        let plan = widening_plan(32, 64, widening_budget(32, 32, 32, 32 * 64, 1));

        assert_eq!(plan.termination(), AdaptiveWideningTermination::Exhaustive);
        assert_eq!(plan.expansion_count(), 0);
        assert_eq!(plan.candidate_work(), 32);
        assert_eq!(plan.comparison_work(), 32);
        assert_eq!(plan.recheck_work(), 32);
        assert_eq!(plan.peak_memory_bytes(), 32 * 64);
        assert_eq!(plan.steps().len(), 1);
        assert_eq!(plan.steps()[0].dimensions().get(), 128);
        assert_eq!(plan.steps()[0].candidate_limit(), 32);
    }

    #[test]
    fn a_multi_stage_plan_widens_monotonically_and_advances_prefixes() {
        let plan = widening_plan(
            150,
            32,
            widening_budget(40 + 150, 150 * 2, 150, 150 * 32, 1),
        );

        assert_eq!(plan.termination(), AdaptiveWideningTermination::Exhaustive);
        assert_eq!(plan.expansion_count(), 1);
        assert_eq!(
            plan.steps()
                .iter()
                .map(|step| (step.dimensions().get(), step.candidate_limit()))
                .collect::<Vec<_>>(),
            vec![(128, 40), (256, 150)]
        );
        assert!(
            plan.steps()
                .windows(2)
                .all(|pair| pair[0].candidate_limit() < pair[1].candidate_limit())
        );
    }

    #[test]
    fn insufficient_candidate_budget_selects_full_vector_before_prefix_work() {
        let plan = widening_plan(150, 32, widening_budget(189, 150 * 2, 150, 150 * 32, 1));

        assert_eq!(
            plan.termination(),
            AdaptiveWideningTermination::CandidateBudget
        );
        assert!(plan.steps().is_empty());
        assert_eq!(plan.candidate_work(), 0);
    }

    #[test]
    fn insufficient_comparison_budget_selects_full_vector_before_prefix_work() {
        let plan = widening_plan(150, 32, widening_budget(190, 299, 150, 150 * 32, 1));

        assert_eq!(
            plan.termination(),
            AdaptiveWideningTermination::ComparisonBudget
        );
        assert!(plan.steps().is_empty());
    }

    #[test]
    fn insufficient_recheck_memory_or_expansion_budget_is_fail_closed() {
        let rechecks = widening_plan(150, 32, widening_budget(190, 300, 149, 150 * 32, 1));
        assert_eq!(
            rechecks.termination(),
            AdaptiveWideningTermination::RecheckBudget
        );
        assert!(rechecks.steps().is_empty());

        let memory = widening_plan(150, 32, widening_budget(190, 300, 150, 150 * 32 - 1, 1));
        assert_eq!(
            memory.termination(),
            AdaptiveWideningTermination::MemoryBudget
        );
        assert!(memory.steps().is_empty());

        let expansions = widening_plan(150, 32, widening_budget(190, 300, 150, 150 * 32, 0));
        assert_eq!(
            expansions.termination(),
            AdaptiveWideningTermination::ExpansionBudget
        );
        assert!(expansions.steps().is_empty());
    }

    #[test]
    fn an_empty_visible_corpus_needs_no_prefix_work() {
        let plan = widening_plan(0, 64, widening_budget(1, 1, 1, 64, 1));
        assert_eq!(plan.termination(), AdaptiveWideningTermination::EmptyCorpus);
        assert!(plan.steps().is_empty());
        assert_eq!(plan.candidate_work(), 0);
        assert_eq!(plan.comparison_work(), 0);
    }
}
