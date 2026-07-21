use context_core::{BitVector, DistanceMetric, SparseVector};
use pgrx::prelude::PgSqlErrorCode;

use super::{BitVec, HalfVec, SparseVec};
use crate::error::{raise_core_error, raise_sql_error};

pub(super) fn halfvec_distance(left: HalfVec, right: HalfVec, metric: DistanceMetric) -> f32 {
    let left = match left.to_half() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };
    let right = match right.to_half() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };

    match metric.distance_half(&left, &right) {
        Ok(distance) => distance,
        Err(error) => raise_core_error(error),
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SparseDistanceMetric {
    L2,
    InnerProduct,
    Cosine,
    L1,
}

pub(super) fn sparsevec_distance(
    left: SparseVec,
    right: SparseVec,
    metric: SparseDistanceMetric,
) -> f32 {
    let left = sparsevec_to_core(left);
    let right = sparsevec_to_core(right);
    let metric = match metric {
        SparseDistanceMetric::L2 => DistanceMetric::L2,
        SparseDistanceMetric::InnerProduct => DistanceMetric::InnerProduct,
        SparseDistanceMetric::Cosine => DistanceMetric::Cosine,
        SparseDistanceMetric::L1 => DistanceMetric::L1,
    };
    match metric.distance_sparse(&left, &right) {
        Ok(distance) => distance,
        Err(error) => raise_core_error(error),
    }
}

pub(super) fn sparsevec_to_core(vector: SparseVec) -> SparseVector {
    match vector.to_sparse() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    }
}

pub(super) fn bitvec_to_core(vector: BitVec) -> BitVector {
    match vector.to_bit() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    }
}

pub(super) fn dimension_to_i32(value: usize, label: &str) -> i32 {
    match i32::try_from(value) {
        Ok(value) => value,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            format!("{label} exceeds PostgreSQL integer range"),
        ),
    }
}
