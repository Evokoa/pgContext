//! Allocation-free ordering, response validation, and budget helpers.

use std::cmp::Ordering;

use super::engine::{BeamArena, BeamState};
use super::{BeamBudgetKind, BeamExpansion, BeamExpansionBatch, BeamProviderRequest, BeamStateId};
use crate::{Cancellation, QueryClock, QueryError, Result};

pub(super) fn cancelled(cancellation: &impl Cancellation) -> Result<bool> {
    cancellation.check_interrupt()?;
    Ok(cancellation.is_cancelled())
}

pub(super) fn elapsed(clock: &impl QueryClock, started: u64) -> u64 {
    clock.now_micros().saturating_sub(started)
}

pub(super) fn validate_response(
    request: &BeamProviderRequest,
    response: &BeamExpansionBatch,
) -> Result<()> {
    if response.expansions.len() > request.max_expansions
        || response.vector_expansions > request.max_expansions
        || response.vector_expansions < response.expansions.len()
    {
        return Err(QueryError::PortContractViolation {
            stage: "virtual_beam_expansions",
            requested: request.max_expansions,
            returned: response.vector_expansions.max(response.expansions.len()),
        });
    }
    let exact_count = response
        .expansions
        .iter()
        .filter(|expansion| expansion.scores.exact().is_some())
        .count();
    if response.exact_reranks != exact_count || response.exact_reranks > request.max_exact_reranks {
        return Err(QueryError::PortContractViolation {
            stage: "virtual_beam_exact_reranks",
            requested: request.max_exact_reranks,
            returned: response.exact_reranks.max(exact_count),
        });
    }
    for expansion in &response.expansions {
        if expansion.topology_node_id.is_some() {
            return Err(QueryError::InvalidInput {
                field: "beam_topology_node_id",
                reason: "topology expansion is reserved for a later phase".to_owned(),
            });
        }
        if !request
            .parents
            .iter()
            .any(|parent| parent.state_id == expansion.parent_state_id)
        {
            return Err(QueryError::PortContractViolation {
                stage: "virtual_beam_parent",
                requested: request.parents.len(),
                returned: request.parents.len().saturating_add(1),
            });
        }
    }
    Ok(())
}

pub(super) fn compare_expansions(
    arena: &BeamArena,
    left: &BeamExpansion,
    right: &BeamExpansion,
) -> Ordering {
    let accumulated = |expansion: &BeamExpansion| {
        arena
            .get(expansion.parent_state_id)
            .map_or(f64::NEG_INFINITY, |parent| parent.scores.ranking())
    };
    let score = |expansion: &BeamExpansion| {
        accumulated(expansion)
            + expansion.scores.transition()
            + expansion
                .scores
                .exact()
                .unwrap_or(expansion.scores.vector())
    };
    score(right)
        .total_cmp(&score(left))
        .then_with(|| left.occurrence_id.cmp(&right.occurrence_id))
        .then_with(|| left.parent_state_id.cmp(&right.parent_state_id))
        .then_with(|| left.point_id.cmp(&right.point_id))
        .then_with(|| left.hnsw_node_id.cmp(&right.hnsw_node_id))
        .then_with(|| left.topology_node_id.cmp(&right.topology_node_id))
        .then_with(|| left.path_pattern_state.cmp(&right.path_pattern_state))
        .then_with(|| right.scores.vector().total_cmp(&left.scores.vector()))
        .then_with(|| {
            right
                .scores
                .transition()
                .total_cmp(&left.scores.transition())
        })
        .then_with(|| compare_optional_score(right.scores.exact(), left.scores.exact()))
}

fn compare_optional_score(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.total_cmp(&right),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

pub(super) fn compare_dominance(left: &BeamState, right: &BeamState) -> Ordering {
    right
        .scores
        .ranking()
        .total_cmp(&left.scores.ranking())
        .then_with(|| left.hop.cmp(&right.hop))
        .then_with(|| left.parent.cmp(&right.parent))
}

pub(super) fn compare_states(
    left: &BeamState,
    left_id: Option<BeamStateId>,
    right: &BeamState,
    right_id: Option<BeamStateId>,
) -> Ordering {
    right
        .scores
        .ranking()
        .total_cmp(&left.scores.ranking())
        .then_with(|| left.occurrence_id.cmp(&right.occurrence_id))
        .then_with(|| left_id.cmp(&right_id))
}

pub(super) fn budget_error(kind: BeamBudgetKind, actual: usize, maximum: usize) -> QueryError {
    QueryError::WorkBudgetExceeded {
        budget: kind.stable_name(),
        actual,
        maximum,
    }
}

pub(super) fn budget_kind(error: &QueryError) -> Option<BeamBudgetKind> {
    let QueryError::WorkBudgetExceeded { budget, .. } = error else {
        return None;
    };
    match *budget {
        "admitted_states" => Some(BeamBudgetKind::AdmittedStates),
        "visited_keys" => Some(BeamBudgetKind::VisitedKeys),
        "vector_expansions" => Some(BeamBudgetKind::VectorExpansions),
        "exact_reranks" => Some(BeamBudgetKind::ExactReranks),
        "parent_bytes" => Some(BeamBudgetKind::ParentBytes),
        "retained_bytes" => Some(BeamBudgetKind::RetainedBytes),
        "elapsed" => Some(BeamBudgetKind::Elapsed),
        _ => None,
    }
}

pub(super) fn projected_capacity(
    current: usize,
    required: usize,
    maximum: usize,
    operation: &'static str,
) -> Result<usize> {
    if required <= current {
        return Ok(current);
    }
    current
        .max(1)
        .checked_mul(2)
        .map(|doubled| doubled.max(required).min(maximum))
        .ok_or(QueryError::ArithmeticOverflow { operation })
}
