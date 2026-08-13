//! Bounded final projection and parent-path reconstruction.

use std::cmp::Ordering;
use std::mem::size_of;

use super::engine::EngineState;
use super::helpers::{budget_error, compare_states, projected_capacity};
use super::{
    BeamBudget, BeamBudgetKind, BeamCompletion, BeamHit, BeamOutcome, BeamParent, BeamPath,
    BeamPathStep, BeamStateId,
};
use crate::{QueryError, Result};

impl EngineState {
    fn output_bytes(
        &self,
        state_ids_capacity: usize,
        hit_capacity: usize,
        hit_count: usize,
        budget: BeamBudget,
        transient_bytes: usize,
    ) -> Result<usize> {
        let per_path = usize::from(budget.max_hops)
            .checked_add(1)
            .and_then(|count| count.checked_mul(size_of::<BeamPathStep>()))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_output_path_bytes",
            })?;
        let state_ids_bytes = state_ids_capacity
            .checked_mul(size_of::<BeamStateId>())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_output_state_ids_bytes",
            })?;
        let hit_bytes = hit_capacity
            .checked_mul(size_of::<BeamHit>())
            .and_then(|bytes| hit_count.checked_mul(per_path)?.checked_add(bytes))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_output_hit_bytes",
            })?;
        self.retained_bytes()?
            .checked_add(state_ids_bytes)
            .and_then(|bytes| bytes.checked_add(hit_bytes))
            .and_then(|bytes| bytes.checked_add(transient_bytes))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "beam_output_bytes",
            })
    }

    pub(super) fn finish(
        mut self,
        completion: BeamCompletion,
        budget: BeamBudget,
        elapsed: u64,
    ) -> Result<BeamOutcome> {
        self.diagnostics.elapsed_micros = elapsed;
        let mut state_ids = std::mem::take(&mut self.frontier);
        state_ids.clear();
        let state_ids_capacity = projected_capacity(
            state_ids.capacity(),
            self.dominance.len(),
            budget.max_visited_keys.min(budget.max_admitted_states),
            "beam_output_state_ids_capacity",
        )?;
        let state_id_peak = self.output_bytes(state_ids_capacity, 0, 0, budget, 0)?;
        if state_id_peak > budget.max_retained_bytes {
            self.diagnostics.retained_bytes = self.diagnostics.retained_bytes.max(state_id_peak);
            return Ok(BeamOutcome {
                hits: Vec::new(),
                completion: BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes),
                diagnostics: self.diagnostics,
            });
        }
        if state_ids.capacity() < state_ids_capacity {
            state_ids
                .try_reserve_exact(state_ids_capacity)
                .map_err(|_| {
                    budget_error(
                        BeamBudgetKind::RetainedBytes,
                        state_id_peak,
                        budget.max_retained_bytes,
                    )
                })?;
        }
        self.dominance.extend_values(&mut state_ids);
        state_ids.sort_unstable_by(|left, right| {
            let left_state = self.arena.get(*left);
            let right_state = self.arena.get(*right);
            match (left_state, right_state) {
                (Some(left_state), Some(right_state)) => {
                    compare_states(&left_state, Some(*left), &right_state, Some(*right))
                }
                _ => Ordering::Equal,
            }
        });
        state_ids.truncate(budget.max_results);
        let hit_capacity = projected_capacity(
            0,
            state_ids.len(),
            budget.max_results,
            "beam_output_hit_capacity",
        )?;
        let output_bytes = self.output_bytes(
            state_ids.capacity(),
            hit_capacity,
            state_ids.len(),
            budget,
            0,
        )?;
        if output_bytes > budget.max_retained_bytes {
            self.diagnostics.retained_bytes = self.diagnostics.retained_bytes.max(output_bytes);
            return Ok(BeamOutcome {
                hits: Vec::new(),
                completion: BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes),
                diagnostics: self.diagnostics,
            });
        }
        let mut hits = Vec::new();
        hits.try_reserve_exact(hit_capacity).map_err(|_| {
            budget_error(
                BeamBudgetKind::RetainedBytes,
                output_bytes,
                budget.max_retained_bytes,
            )
        })?;
        for state_id in state_ids {
            hits.push(self.reconstruct_hit(state_id)?);
        }
        self.diagnostics.retained_bytes = self.diagnostics.retained_bytes.max(output_bytes);
        Ok(BeamOutcome {
            hits,
            completion,
            diagnostics: self.diagnostics,
        })
    }

    pub(super) fn finish_from_parents(
        mut self,
        completion: BeamCompletion,
        budget: BeamBudget,
        elapsed: u64,
        parents: &[BeamParent],
        transient_bytes: usize,
    ) -> Result<BeamOutcome> {
        self.diagnostics.elapsed_micros = elapsed;
        let hit_count = parents.len().min(budget.max_results);
        let hit_capacity =
            projected_capacity(0, hit_count, budget.max_results, "beam_output_hit_capacity")?;
        let output_bytes =
            self.output_bytes(0, hit_capacity, hit_count, budget, transient_bytes)?;
        if output_bytes > budget.max_retained_bytes {
            self.diagnostics.retained_bytes = self.diagnostics.retained_bytes.max(output_bytes);
            return Ok(BeamOutcome {
                hits: Vec::new(),
                completion: BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes),
                diagnostics: self.diagnostics,
            });
        }
        let mut hits = Vec::new();
        hits.try_reserve_exact(hit_capacity).map_err(|_| {
            budget_error(
                BeamBudgetKind::RetainedBytes,
                output_bytes,
                budget.max_retained_bytes,
            )
        })?;
        for parent in parents.iter().take(hit_count) {
            hits.push(self.reconstruct_hit(parent.state_id())?);
        }
        self.diagnostics.retained_bytes = self.diagnostics.retained_bytes.max(output_bytes);
        Ok(BeamOutcome {
            hits,
            completion,
            diagnostics: self.diagnostics,
        })
    }

    fn reconstruct_hit(&self, state_id: BeamStateId) -> Result<BeamHit> {
        let state = self
            .arena
            .get(state_id)
            .ok_or(QueryError::InvariantViolation {
                operation: "beam_output_identity",
            })?;
        if state.parent.is_none() {
            return Ok(BeamHit {
                occurrence_id: state.occurrence_id,
                point_id: state.point_id,
                hnsw_node_id: state.hnsw_node_id,
                scores: state.scores,
                path: BeamPath::Root(BeamPathStep {
                    occurrence_id: state.occurrence_id,
                    transition: state.transition,
                    hop: state.hop,
                }),
            });
        }
        let mut path = Vec::with_capacity(usize::from(state.hop) + 1);
        let mut cursor = Some(state_id);
        while let Some(path_id) = cursor {
            let path_state = self
                .arena
                .get(path_id)
                .ok_or(QueryError::InvariantViolation {
                    operation: "beam_path_identity",
                })?;
            path.push(BeamPathStep {
                occurrence_id: path_state.occurrence_id,
                transition: path_state.transition,
                hop: path_state.hop,
            });
            cursor = path_state.parent;
        }
        path.reverse();
        Ok(BeamHit {
            occurrence_id: state.occurrence_id,
            point_id: state.point_id,
            hnsw_node_id: state.hnsw_node_id,
            scores: state.scores,
            path: BeamPath::Expanded(path),
        })
    }
}
