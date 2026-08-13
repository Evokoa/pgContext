use std::cmp::Ordering;
use std::mem::size_of;

use context_core::{OccurrenceId, PointId};

use super::dominance::{DominanceIndex, DominanceKey, OccurrenceIndex};
use super::helpers::{
    budget_error, budget_kind, cancelled, compare_dominance, compare_expansions, compare_states,
    elapsed, projected_capacity, validate_response,
};
use super::types::BeamRuntime;
use super::{
    AuthorizationContextToken, BeamBudget, BeamBudgetKind, BeamCompletion, BeamDiagnostics,
    BeamExpansion, BeamExpansionBatch, BeamExpansionProvider, BeamNodeId, BeamOutcome, BeamParent,
    BeamProviderRequest, BeamScoreComponents, BeamSeed, BeamStateId, BeamTransition,
    PathPatternState,
};
use crate::{Cancellation, PortBudget, QueryClock, QueryError, Result};

// These frozen projections cover the inline Vec entry, promoted BTree node,
// allocator metadata, and the one-time inline-to-tree migration overlap.
const DOMINANCE_ENTRY_BYTES: usize = 160;
const OCCURRENCE_ENTRY_BYTES: usize = 96;
const FRONTIER_ENTRY_BYTES: usize = size_of::<BeamStateId>();
const PROVIDER_REQUEST_FIXED_BYTES: usize = size_of::<BeamProviderRequest>();
const PROVIDER_RESPONSE_FIXED_BYTES: usize = size_of::<BeamExpansionBatch>();
// Rust 1.96's `RawVec` uses a minimum non-zero capacity of four for element
// sizes from 2 through 1,024 bytes. Allocators may round the usable capacity
// down from this projection (Darwin currently reports two BeamParent slots),
// so request admission charges the conservative four-slot shape before growth.
const MIN_PROVIDER_PARENT_CAPACITY: usize = 4;

#[derive(Clone, Copy, Debug)]
pub(super) struct BeamState {
    pub(super) occurrence_id: OccurrenceId,
    pub(super) point_id: PointId,
    pub(super) hnsw_node_id: Option<BeamNodeId>,
    pub(super) parent: Option<BeamStateId>,
    pub(super) transition: BeamTransition,
    pub(super) path_pattern_state: PathPatternState,
    pub(super) hop: u16,
    pub(super) authorization: AuthorizationContextToken,
    pub(super) scores: BeamScoreComponents,
}

impl BeamState {
    const fn dominance_key(self) -> DominanceKey {
        DominanceKey::new(
            self.occurrence_id,
            self.path_pattern_state,
            self.authorization,
        )
    }

    const fn parent_view(self, state_id: BeamStateId) -> BeamParent {
        BeamParent {
            state_id,
            occurrence_id: self.occurrence_id,
            point_id: self.point_id,
            hnsw_node_id: self.hnsw_node_id,
            path_pattern_state: self.path_pattern_state,
            hop: self.hop,
            authorization: self.authorization,
            scores: self.scores,
        }
    }
}

#[derive(Debug)]
pub(super) struct BeamArena {
    states: Vec<BeamState>,
}

impl BeamArena {
    const fn new() -> Self {
        Self { states: Vec::new() }
    }

    pub(super) fn get(&self, state_id: BeamStateId) -> Option<BeamState> {
        self.states.get(state_id.index()).copied()
    }

    fn push(&mut self, state: BeamState, budget: BeamBudget) -> Result<BeamStateId> {
        let next_len = self
            .states
            .len()
            .checked_add(1)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_arena_length",
            })?;
        if next_len > budget.max_admitted_states {
            return Err(budget_error(
                BeamBudgetKind::AdmittedStates,
                next_len,
                budget.max_admitted_states,
            ));
        }
        if next_len > self.states.capacity() {
            let projected_capacity = projected_capacity(
                self.states.capacity(),
                next_len,
                budget.max_admitted_states,
                "beam_arena_capacity",
            )?;
            let projected_bytes = projected_capacity
                .checked_mul(size_of::<BeamState>())
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_parent_bytes",
                })?;
            if projected_bytes > budget.max_parent_bytes {
                return Err(budget_error(
                    BeamBudgetKind::ParentBytes,
                    projected_bytes,
                    budget.max_parent_bytes,
                ));
            }
            self.states
                .try_reserve_exact(projected_capacity - self.states.len())
                .map_err(|_| {
                    budget_error(
                        BeamBudgetKind::ParentBytes,
                        projected_bytes,
                        budget.max_parent_bytes,
                    )
                })?;
        }
        let state_id = BeamStateId::from_index(self.states.len())?;
        self.states.push(state);
        Ok(state_id)
    }

    fn parent_bytes(&self) -> Result<usize> {
        self.states
            .capacity()
            .checked_mul(size_of::<BeamState>())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_parent_bytes",
            })
    }

    fn projected_parent_bytes(&self, next_len: usize, budget: BeamBudget) -> Result<usize> {
        projected_capacity(
            self.states.capacity(),
            next_len,
            budget.max_admitted_states,
            "beam_arena_capacity",
        )?
        .checked_mul(size_of::<BeamState>())
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "beam_parent_bytes",
        })
    }

    fn reserve(&mut self, count: usize, budget: BeamBudget) -> Result<()> {
        let projected_bytes =
            count
                .checked_mul(size_of::<BeamState>())
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_seed_parent_bytes",
                })?;
        if projected_bytes > budget.max_parent_bytes {
            return Err(budget_error(
                BeamBudgetKind::ParentBytes,
                projected_bytes,
                budget.max_parent_bytes,
            ));
        }
        self.states.try_reserve_exact(count).map_err(|_| {
            budget_error(
                BeamBudgetKind::ParentBytes,
                projected_bytes,
                budget.max_parent_bytes,
            )
        })
    }
}

#[derive(Debug)]
pub(super) struct EngineState {
    pub(super) arena: BeamArena,
    pub(super) dominance: DominanceIndex,
    occurrences: OccurrenceIndex,
    pub(super) frontier: Vec<BeamStateId>,
    pub(super) diagnostics: BeamDiagnostics,
}

impl EngineState {
    fn new(_budget: BeamBudget) -> Self {
        Self {
            arena: BeamArena::new(),
            dominance: DominanceIndex::new(),
            occurrences: OccurrenceIndex::new(),
            frontier: Vec::new(),
            diagnostics: BeamDiagnostics::default(),
        }
    }

    fn prepare_seeds(&mut self, count: usize, budget: BeamBudget) -> Result<()> {
        if count > budget.max_admitted_states {
            return Err(budget_error(
                BeamBudgetKind::AdmittedStates,
                count,
                budget.max_admitted_states,
            ));
        }
        let parent_bytes =
            count
                .checked_mul(size_of::<BeamState>())
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_seed_parent_bytes",
                })?;
        let dominance_bytes =
            count
                .checked_mul(DOMINANCE_ENTRY_BYTES)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_seed_dominance_bytes",
                })?;
        let occurrence_bytes =
            count
                .checked_mul(OCCURRENCE_ENTRY_BYTES)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_seed_occurrence_bytes",
                })?;
        let frontier_bytes =
            count
                .checked_mul(FRONTIER_ENTRY_BYTES)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_seed_frontier_bytes",
                })?;
        let retained_bytes = parent_bytes
            .checked_add(dominance_bytes)
            .and_then(|bytes| bytes.checked_add(occurrence_bytes))
            .and_then(|bytes| bytes.checked_add(frontier_bytes))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_seed_retained_bytes",
            })?;
        if retained_bytes > budget.max_retained_bytes {
            return Err(budget_error(
                BeamBudgetKind::RetainedBytes,
                retained_bytes,
                budget.max_retained_bytes,
            ));
        }
        self.arena.reserve(count, budget)?;
        self.dominance.reserve_small(count).map_err(|_| {
            budget_error(
                BeamBudgetKind::RetainedBytes,
                retained_bytes,
                budget.max_retained_bytes,
            )
        })?;
        self.occurrences.reserve_small(count).map_err(|_| {
            budget_error(
                BeamBudgetKind::RetainedBytes,
                retained_bytes,
                budget.max_retained_bytes,
            )
        })?;
        self.frontier.try_reserve_exact(count).map_err(|_| {
            budget_error(
                BeamBudgetKind::RetainedBytes,
                retained_bytes,
                budget.max_retained_bytes,
            )
        })?;
        self.refresh_memory(budget)
    }

    pub(super) fn retained_bytes(&self) -> Result<usize> {
        self.arena
            .parent_bytes()?
            .checked_add(
                self.dominance
                    .len()
                    .checked_mul(DOMINANCE_ENTRY_BYTES)
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "beam_dominance_bytes",
                    })?,
            )
            .and_then(|bytes| {
                self.occurrences
                    .len()
                    .checked_mul(OCCURRENCE_ENTRY_BYTES)
                    .and_then(|occurrences| bytes.checked_add(occurrences))
            })
            .and_then(|bytes| {
                self.frontier
                    .capacity()
                    .checked_mul(FRONTIER_ENTRY_BYTES)
                    .and_then(|frontier| bytes.checked_add(frontier))
            })
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_retained_bytes",
            })
    }

    fn refresh_memory(&mut self, budget: BeamBudget) -> Result<()> {
        let parent_bytes = self.arena.parent_bytes()?;
        if parent_bytes > budget.max_parent_bytes {
            return Err(budget_error(
                BeamBudgetKind::ParentBytes,
                parent_bytes,
                budget.max_parent_bytes,
            ));
        }
        let retained_bytes = self.retained_bytes()?;
        if retained_bytes > budget.max_retained_bytes {
            return Err(budget_error(
                BeamBudgetKind::RetainedBytes,
                retained_bytes,
                budget.max_retained_bytes,
            ));
        }
        self.diagnostics.parent_bytes = parent_bytes;
        self.diagnostics.retained_bytes = self.diagnostics.retained_bytes.max(retained_bytes);
        Ok(())
    }

    fn update_score_range(&mut self, score: f64) {
        self.diagnostics.scores.best = Some(
            self.diagnostics
                .scores
                .best
                .map_or(score, |best| best.max(score)),
        );
        self.diagnostics.scores.worst = Some(
            self.diagnostics
                .scores
                .worst
                .map_or(score, |worst| worst.min(score)),
        );
    }

    fn ancestry_contains(&self, parent: BeamStateId, occurrence_id: OccurrenceId) -> Result<bool> {
        let mut cursor = Some(parent);
        let mut traversed = 0_usize;
        while let Some(state_id) = cursor {
            traversed = traversed
                .checked_add(1)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_ancestry",
                })?;
            if traversed > usize::from(super::MAX_BEAM_HOPS) + 1 {
                return Err(QueryError::InvariantViolation {
                    operation: "beam_parent_cycle",
                });
            }
            let state = self
                .arena
                .get(state_id)
                .ok_or(QueryError::InvariantViolation {
                    operation: "beam_parent_identity",
                })?;
            if state.occurrence_id == occurrence_id {
                return Ok(true);
            }
            cursor = state.parent;
        }
        Ok(false)
    }

    fn admit(
        &mut self,
        state: BeamState,
        budget: BeamBudget,
        transient_bytes: usize,
    ) -> Result<Option<BeamStateId>> {
        let existing_point = self.occurrences.get(state.occurrence_id);
        if existing_point.is_some_and(|point_id| point_id != state.point_id) {
            return Err(QueryError::InvariantViolation {
                operation: "beam_occurrence_point_identity",
            });
        }
        let key = state.dominance_key();
        let existing_id = self.dominance.get(&key);
        if let Some(existing_id) = existing_id {
            let existing = self
                .arena
                .get(existing_id)
                .ok_or(QueryError::InvariantViolation {
                    operation: "beam_dominance_identity",
                })?;
            if state.point_id != existing.point_id {
                return Err(QueryError::InvariantViolation {
                    operation: "beam_occurrence_point_identity",
                });
            }
            match compare_dominance(&state, &existing) {
                Ordering::Less => {}
                Ordering::Equal => {
                    self.diagnostics.pruning.duplicate =
                        self.diagnostics.pruning.duplicate.checked_add(1).ok_or(
                            QueryError::ArithmeticOverflow {
                                operation: "beam_duplicate_pruning",
                            },
                        )?;
                    return Ok(None);
                }
                Ordering::Greater => {
                    self.diagnostics.pruning.dominated =
                        self.diagnostics.pruning.dominated.checked_add(1).ok_or(
                            QueryError::ArithmeticOverflow {
                                operation: "beam_dominance_pruning",
                            },
                        )?;
                    return Ok(None);
                }
            }
        } else {
            let next =
                self.dominance
                    .len()
                    .checked_add(1)
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "beam_visited_keys",
                    })?;
            if next > budget.max_visited_keys {
                return Err(budget_error(
                    BeamBudgetKind::VisitedKeys,
                    next,
                    budget.max_visited_keys,
                ));
            }
        }

        self.preflight_admit(
            existing_id.is_none(),
            existing_point.is_none(),
            budget,
            transient_bytes,
        )?;

        let state_id = self.arena.push(state, budget)?;
        if !self
            .dominance
            .insert_known(key, state_id, existing_id.is_some())
        {
            return Err(QueryError::InvariantViolation {
                operation: "beam_dominance_identity",
            });
        }
        if existing_point.is_none() {
            self.occurrences
                .insert_new(state.occurrence_id, state.point_id);
        }
        self.frontier.push(state_id);
        self.diagnostics.admitted_states = self.arena.states.len();
        self.diagnostics.visited_keys = self.dominance.len();
        self.update_score_range(state.scores.ranking());
        self.refresh_memory_with_transient(budget, transient_bytes)?;
        Ok(Some(state_id))
    }

    fn preflight_admit(
        &mut self,
        new_key: bool,
        new_occurrence: bool,
        budget: BeamBudget,
        transient_bytes: usize,
    ) -> Result<()> {
        let next_states =
            self.arena
                .states
                .len()
                .checked_add(1)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_arena_length",
                })?;
        let parent_bytes = self.arena.projected_parent_bytes(next_states, budget)?;
        if parent_bytes > budget.max_parent_bytes {
            return Err(budget_error(
                BeamBudgetKind::ParentBytes,
                parent_bytes,
                budget.max_parent_bytes,
            ));
        }
        let dominance_len = self
            .dominance
            .len()
            .checked_add(usize::from(new_key))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_visited_keys",
            })?;
        let dominance_bytes = dominance_len.checked_mul(DOMINANCE_ENTRY_BYTES).ok_or(
            QueryError::ArithmeticOverflow {
                operation: "beam_dominance_bytes",
            },
        )?;
        let occurrence_bytes = self
            .occurrences
            .len()
            .checked_add(usize::from(new_occurrence))
            .and_then(|count| count.checked_mul(OCCURRENCE_ENTRY_BYTES))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_occurrence_bytes",
            })?;
        let frontier_len =
            self.frontier
                .len()
                .checked_add(1)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_frontier_length",
                })?;
        let frontier_capacity = projected_capacity(
            self.frontier.capacity(),
            frontier_len,
            budget.max_admitted_states,
            "beam_frontier_capacity",
        )?;
        let frontier_bytes = frontier_capacity.checked_mul(FRONTIER_ENTRY_BYTES).ok_or(
            QueryError::ArithmeticOverflow {
                operation: "beam_frontier_bytes",
            },
        )?;
        let retained_bytes = parent_bytes
            .checked_add(dominance_bytes)
            .and_then(|bytes| bytes.checked_add(occurrence_bytes))
            .and_then(|bytes| bytes.checked_add(frontier_bytes))
            .and_then(|bytes| bytes.checked_add(transient_bytes))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_retained_bytes",
            })?;
        if retained_bytes > budget.max_retained_bytes {
            return Err(budget_error(
                BeamBudgetKind::RetainedBytes,
                retained_bytes,
                budget.max_retained_bytes,
            ));
        }
        if frontier_capacity > self.frontier.capacity() {
            self.frontier
                .try_reserve_exact(frontier_capacity - self.frontier.len())
                .map_err(|_| {
                    budget_error(
                        BeamBudgetKind::RetainedBytes,
                        retained_bytes,
                        budget.max_retained_bytes,
                    )
                })?;
        }
        Ok(())
    }

    fn refresh_memory_with_transient(
        &mut self,
        budget: BeamBudget,
        transient_bytes: usize,
    ) -> Result<()> {
        self.refresh_memory(budget)?;
        let retained_bytes = self.retained_bytes()?.checked_add(transient_bytes).ok_or(
            QueryError::ArithmeticOverflow {
                operation: "beam_retained_bytes",
            },
        )?;
        if retained_bytes > budget.max_retained_bytes {
            return Err(budget_error(
                BeamBudgetKind::RetainedBytes,
                retained_bytes,
                budget.max_retained_bytes,
            ));
        }
        self.diagnostics.retained_bytes = self.diagnostics.retained_bytes.max(retained_bytes);
        Ok(())
    }

    fn trim_frontier(&mut self, budget: BeamBudget) -> Result<()> {
        let arena = &self.arena;
        let dominance = &self.dominance;
        self.frontier.retain(|state_id| {
            arena
                .get(*state_id)
                .is_some_and(|state| dominance.get(&state.dominance_key()) == Some(*state_id))
        });
        self.frontier.sort_unstable_by(|left, right| {
            let left_state = self.arena.get(*left);
            let right_state = self.arena.get(*right);
            match (left_state, right_state) {
                (Some(left_state), Some(right_state)) => {
                    compare_states(&left_state, Some(*left), &right_state, Some(*right))
                }
                _ => Ordering::Equal,
            }
        });
        if self.frontier.len() > budget.beam_width {
            let pruned = self.frontier.len() - budget.beam_width;
            self.frontier.truncate(budget.beam_width);
            self.diagnostics.pruning.beam_width = self
                .diagnostics
                .pruning
                .beam_width
                .checked_add(pruned)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_width_pruning",
                })?;
        }
        Ok(())
    }

    fn take_parents(&mut self, budget: BeamBudget) -> Result<Vec<BeamParent>> {
        self.trim_frontier(budget)?;
        let count = self.frontier.len().min(budget.expansion_batch);
        let parent_capacity = projected_capacity(
            0,
            count.max(MIN_PROVIDER_PARENT_CAPACITY),
            budget.expansion_batch,
            "beam_provider_parent_capacity",
        )?;
        let request_bytes = parent_capacity
            .checked_mul(size_of::<BeamParent>())
            .and_then(|bytes| bytes.checked_add(PROVIDER_REQUEST_FIXED_BYTES))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_provider_request_bytes",
            })?;
        let projected_peak = self.retained_bytes()?.checked_add(request_bytes).ok_or(
            QueryError::ArithmeticOverflow {
                operation: "beam_provider_peak_bytes",
            },
        )?;
        if projected_peak > budget.max_retained_bytes {
            return Err(budget_error(
                BeamBudgetKind::RetainedBytes,
                projected_peak,
                budget.max_retained_bytes,
            ));
        }
        let mut parents = Vec::new();
        parents.try_reserve_exact(parent_capacity).map_err(|_| {
            budget_error(
                BeamBudgetKind::RetainedBytes,
                projected_peak,
                budget.max_retained_bytes,
            )
        })?;
        if parents.capacity() > parent_capacity {
            return Err(QueryError::InvariantViolation {
                operation: "beam_provider_parent_capacity",
            });
        }
        let arena = &self.arena;
        for state_id in self.frontier.drain(..count) {
            let parent = arena
                .get(state_id)
                .map(|state| state.parent_view(state_id))
                .ok_or(QueryError::InvariantViolation {
                    operation: "beam_frontier_identity",
                })?;
            parents.push(parent);
        }
        self.diagnostics.retained_bytes = self.diagnostics.retained_bytes.max(projected_peak);
        Ok(parents)
    }
}

/// Provider-neutral vector-only beam engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualBeamEngine {
    budget: BeamBudget,
}

impl VirtualBeamEngine {
    /// Creates an engine from a fully validated hard budget.
    #[must_use]
    pub const fn new(budget: BeamBudget) -> Self {
        Self { budget }
    }

    /// Consumes the engine and all statement-local arena state.
    ///
    /// # Errors
    ///
    /// Returns a typed provider, contract, arithmetic, or interrupt failure.
    pub fn run<P, C, K>(
        self,
        seeds: &[BeamSeed],
        provider: &mut P,
        cancellation: &C,
        clock: &K,
    ) -> Result<BeamOutcome>
    where
        P: BeamExpansionProvider,
        C: Cancellation,
        K: QueryClock,
    {
        let runtime = BeamRuntime::new(provider, cancellation, clock);
        let started = runtime.clock.now_micros();
        let mut state = EngineState::new(self.budget);
        if cancelled(runtime.cancellation)? {
            return state.finish(BeamCompletion::Cancelled, self.budget, 0);
        }
        if let Err(error) = state.prepare_seeds(seeds.len(), self.budget) {
            if let Some(kind) = budget_kind(&error) {
                return state.finish(BeamCompletion::BudgetExhausted(kind), self.budget, 0);
            }
            return Err(error);
        }
        for seed in seeds {
            let admitted = BeamState {
                occurrence_id: seed.occurrence_id,
                point_id: seed.point_id,
                hnsw_node_id: seed.hnsw_node_id,
                parent: None,
                transition: BeamTransition::Seed,
                path_pattern_state: seed.path_pattern_state,
                hop: 0,
                authorization: seed.authorization,
                scores: seed.scores,
            };
            if let Err(error) = state.admit(admitted, self.budget, 0) {
                if let Some(kind) = budget_kind(&error) {
                    return state.finish(
                        BeamCompletion::BudgetExhausted(kind),
                        self.budget,
                        elapsed(runtime.clock, started),
                    );
                }
                return Err(error);
            }
        }

        loop {
            if cancelled(runtime.cancellation)? {
                return state.finish(
                    BeamCompletion::Cancelled,
                    self.budget,
                    elapsed(runtime.clock, started),
                );
            }
            let now = elapsed(runtime.clock, started);
            if now >= self.budget.max_elapsed_micros {
                return state.finish(
                    BeamCompletion::BudgetExhausted(BeamBudgetKind::Elapsed),
                    self.budget,
                    now,
                );
            }

            let mut parents = match state.take_parents(self.budget) {
                Ok(parents) => parents,
                Err(error) => {
                    if let Some(kind) = budget_kind(&error) {
                        return state.finish(
                            BeamCompletion::BudgetExhausted(kind),
                            self.budget,
                            now,
                        );
                    }
                    return Err(error);
                }
            };
            if parents.is_empty() {
                drop(parents);
                return state.finish(BeamCompletion::Exhausted, self.budget, now);
            }
            let before_hop_filter = parents.len();
            parents.retain(|parent| parent.hop < self.budget.max_hops);
            state.diagnostics.pruning.hop = state
                .diagnostics
                .pruning
                .hop
                .checked_add(before_hop_filter - parents.len())
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_hop_pruning",
                })?;
            if parents.is_empty() {
                continue;
            }

            let remaining_vector = self
                .budget
                .max_vector_expansions
                .checked_sub(state.diagnostics.vector_expansions)
                .ok_or(QueryError::InvariantViolation {
                    operation: "beam_vector_budget",
                })?;
            if remaining_vector == 0 {
                drop(parents);
                return state.finish(
                    BeamCompletion::BudgetExhausted(BeamBudgetKind::VectorExpansions),
                    self.budget,
                    now,
                );
            }
            let remaining_reranks = self
                .budget
                .max_exact_reranks
                .checked_sub(state.diagnostics.exact_reranks)
                .ok_or(QueryError::InvariantViolation {
                    operation: "beam_rerank_budget",
                })?;
            if remaining_reranks == 0 {
                drop(parents);
                return state.finish(
                    BeamCompletion::BudgetExhausted(BeamBudgetKind::ExactReranks),
                    self.budget,
                    now,
                );
            }
            let base_bytes = state.retained_bytes()?;
            let request_bytes = parents
                .capacity()
                .checked_mul(size_of::<BeamParent>())
                .and_then(|bytes| bytes.checked_add(PROVIDER_REQUEST_FIXED_BYTES))
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_provider_request_bytes",
                })?;
            let Some(provider_memory) = self
                .budget
                .max_retained_bytes
                .checked_sub(base_bytes)
                .and_then(|bytes| bytes.checked_sub(request_bytes))
            else {
                drop(parents);
                return state.finish(
                    BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes),
                    self.budget,
                    now,
                );
            };
            let expansion_memory = provider_memory.saturating_sub(PROVIDER_RESPONSE_FIXED_BYTES)
                / size_of::<BeamExpansion>();
            if expansion_memory == 0 {
                drop(parents);
                return state.finish(
                    BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes),
                    self.budget,
                    now,
                );
            }
            let max_expansions = self
                .budget
                .expansion_batch
                .min(remaining_vector)
                .min(expansion_memory);
            let request = BeamProviderRequest {
                parents,
                max_expansions,
                max_exact_reranks: max_expansions.min(remaining_reranks),
            };
            let admitted_response_bytes = max_expansions
                .checked_mul(size_of::<BeamExpansion>())
                .and_then(|bytes| bytes.checked_add(PROVIDER_RESPONSE_FIXED_BYTES))
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_provider_response_bytes",
                })?;
            state.diagnostics.retained_bytes = state.diagnostics.retained_bytes.max(
                base_bytes
                    .checked_add(request_bytes)
                    .and_then(|bytes| bytes.checked_add(admitted_response_bytes))
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "beam_provider_peak_bytes",
                    })?,
            );
            state.diagnostics.provider_calls =
                state.diagnostics.provider_calls.checked_add(1).ok_or(
                    QueryError::ArithmeticOverflow {
                        operation: "beam_provider_calls",
                    },
                )?;
            let response = runtime.provider.expand(
                &request,
                PortBudget::new(
                    request.max_expansions,
                    provider_memory,
                    0,
                    self.budget.max_elapsed_micros - now,
                ),
            )?;
            if cancelled(runtime.cancellation)? {
                let now = elapsed(runtime.clock, started);
                drop(response);
                drop(request);
                return state.finish(BeamCompletion::Cancelled, self.budget, now);
            }
            let after_provider = elapsed(runtime.clock, started);
            if after_provider >= self.budget.max_elapsed_micros {
                drop(response);
                drop(request);
                return state.finish(
                    BeamCompletion::BudgetExhausted(BeamBudgetKind::Elapsed),
                    self.budget,
                    after_provider,
                );
            }
            validate_response(&request, &response)?;
            let response_bytes = response
                .expansions
                .capacity()
                .checked_mul(size_of::<BeamExpansion>())
                .and_then(|bytes| bytes.checked_add(PROVIDER_RESPONSE_FIXED_BYTES))
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_provider_response_bytes",
                })?;
            if response_bytes > provider_memory {
                return Err(QueryError::PortContractViolation {
                    stage: "virtual_beam_memory",
                    requested: provider_memory,
                    returned: response_bytes,
                });
            }
            state.diagnostics.retained_bytes = state.diagnostics.retained_bytes.max(
                base_bytes
                    .checked_add(request_bytes)
                    .and_then(|bytes| bytes.checked_add(response_bytes))
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "beam_provider_peak_bytes",
                    })?,
            );
            state.diagnostics.vector_expansions = state
                .diagnostics
                .vector_expansions
                .checked_add(response.vector_expansions)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_vector_expansions",
                })?;
            state.diagnostics.exact_reranks = state
                .diagnostics
                .exact_reranks
                .checked_add(response.exact_reranks)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "beam_exact_reranks",
                })?;
            if response.expansions.is_empty()
                && state.frontier.is_empty()
                && request.parents.len() == state.dominance.len()
            {
                let transient_bytes = request_bytes.checked_add(response_bytes).ok_or(
                    QueryError::ArithmeticOverflow {
                        operation: "beam_provider_peak_bytes",
                    },
                )?;
                return state.finish_from_parents(
                    BeamCompletion::Exhausted,
                    self.budget,
                    after_provider,
                    &request.parents,
                    transient_bytes,
                );
            }
            let request_parent_count = request.parents.len();
            let mut expansions = response.expansions;
            drop(request);
            expansions
                .sort_unstable_by(|left, right| compare_expansions(&state.arena, right, left));
            while let Some(expansion) = expansions.pop() {
                let parent = state.arena.get(expansion.parent_state_id).ok_or(
                    QueryError::PortContractViolation {
                        stage: "virtual_beam_parent",
                        requested: request_parent_count,
                        returned: 1,
                    },
                )?;
                if state.ancestry_contains(expansion.parent_state_id, expansion.occurrence_id)? {
                    state.diagnostics.pruning.cycle =
                        state.diagnostics.pruning.cycle.checked_add(1).ok_or(
                            QueryError::ArithmeticOverflow {
                                operation: "beam_cycle_pruning",
                            },
                        )?;
                    continue;
                }
                let scores = BeamScoreComponents::new(
                    expansion.scores.vector(),
                    expansion.scores.transition(),
                    parent.scores.ranking(),
                    expansion.scores.exact(),
                )?;
                let child = BeamState {
                    occurrence_id: expansion.occurrence_id,
                    point_id: expansion.point_id,
                    hnsw_node_id: expansion.hnsw_node_id,
                    parent: Some(expansion.parent_state_id),
                    transition: BeamTransition::Vector,
                    path_pattern_state: expansion.path_pattern_state,
                    hop: parent
                        .hop
                        .checked_add(1)
                        .ok_or(QueryError::ArithmeticOverflow {
                            operation: "beam_hop",
                        })?,
                    authorization: parent.authorization,
                    scores,
                };
                if let Err(error) = state.admit(child, self.budget, response_bytes) {
                    if let Some(kind) = budget_kind(&error) {
                        drop(expansions);
                        return state.finish(
                            BeamCompletion::BudgetExhausted(kind),
                            self.budget,
                            elapsed(runtime.clock, started),
                        );
                    }
                    return Err(error);
                }
            }
        }
    }
}
