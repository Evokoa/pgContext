//! Shared late-interaction MaxSim scoring and budget checks.

use context_core::{DenseVector, Error as CoreError};
use context_query::LateInteractionWork;

use crate::error::{raise_core_error, raise_query_error};

pub(crate) use context_query::MAX_LATE_INTERACTION_COMPARISONS;

pub(crate) fn late_interaction_comparison_count(
    query_vector_count: usize,
    candidate_vector_count: usize,
) -> usize {
    LateInteractionWork::new(query_vector_count, candidate_vector_count, usize::MAX)
        .unwrap_or_else(|error| raise_query_error(error))
        .projected_comparisons()
}

pub(crate) fn enforce_late_interaction_budget(
    query_vector_count: usize,
    candidate_vector_count: usize,
) {
    LateInteractionWork::new(
        query_vector_count,
        candidate_vector_count,
        MAX_LATE_INTERACTION_COMPARISONS,
    )
    .unwrap_or_else(|error| raise_query_error(error));
}

pub(crate) fn late_interaction_score(
    query_vectors: &[DenseVector],
    candidate_vectors: &[DenseVector],
) -> f32 {
    query_vectors
        .iter()
        .map(|query| {
            candidate_vectors
                .iter()
                .map(|candidate| dense_inner_product(query, candidate))
                .fold(f32::NEG_INFINITY, f32::max)
        })
        .sum()
}

fn dense_inner_product(left: &DenseVector, right: &DenseVector) -> f32 {
    if left.dimension() != right.dimension() {
        raise_core_error(CoreError::InvalidVector(format!(
            "dimension mismatch: left has {} dimensions, right has {}",
            left.dimension(),
            right.dimension()
        )));
    }
    left.as_slice()
        .iter()
        .zip(right.as_slice())
        .map(|(left, right)| left * right)
        .sum()
}
