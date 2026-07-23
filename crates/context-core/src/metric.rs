//! Exact distance metrics for framework-free vector representations.

use crate::{DenseVector, Error, HalfVector, Result, SparseVector, metric_kernels};

/// Distance or similarity family used for vector comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistanceMetric {
    /// Euclidean distance `sqrt(sum((left - right)^2))`, ordered ascending.
    L2,
    /// Raw inner product `sum(left * right)`, ordered descending for similarity.
    InnerProduct,
    /// Negative inner product `-sum(left * right)`, ordered ascending.
    NegativeInnerProduct,
    /// Cosine distance `1 - dot(left, right) / (norm(left) * norm(right))`,
    /// ordered ascending and undefined for a zero-magnitude operand.
    Cosine,
    /// Manhattan distance `sum(abs(left - right))`, ordered ascending.
    L1,
    /// Number of unequal binary coordinates, ordered ascending.
    ///
    /// Every coordinate must be exactly `0.0` or `1.0`.
    Hamming,
    /// Set distance `1 - intersection / union` over binary coordinates,
    /// ordered ascending. Two empty sets have distance zero.
    ///
    /// Every coordinate must be exactly `0.0` or `1.0`.
    Jaccard,
}

impl DistanceMetric {
    /// Computes this metric between two dense vectors.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DimensionMismatch`] when the vectors have different
    /// dimensions. Returns [`Error::InvalidVector`] for cosine distance when
    /// either vector has zero magnitude, or for a binary metric when a
    /// coordinate is not exactly zero or one.
    pub fn distance(self, left: &DenseVector, right: &DenseVector) -> Result<f32> {
        self.distance_slices(left.as_slice(), right.as_slice())
    }

    /// Computes this metric directly over borrowed dense values.
    ///
    /// This is the allocation-free boundary used by database datum adapters
    /// and packed index records.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DimensionMismatch`] when the slices have different
    /// lengths. Returns [`Error::InvalidVector`] for cosine distance when
    /// either slice has zero magnitude.
    pub fn distance_slices(self, left: &[f32], right: &[f32]) -> Result<f32> {
        ensure_same_dimension(left.len(), right.len())?;

        match self {
            Self::L2 => Ok(l2(left, right)),
            Self::InnerProduct => Ok(dot(left, right)),
            Self::NegativeInnerProduct => Ok(-dot(left, right)),
            Self::Cosine => cosine(left, right),
            Self::L1 => Ok(l1(left, right)),
            Self::Hamming => binary_distance(left, right, BinaryMetric::Hamming),
            Self::Jaccard => binary_distance(left, right, BinaryMetric::Jaccard),
        }
    }

    /// Computes this metric between two half vectors after widening to f32.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DimensionMismatch`] when the vectors have different
    /// dimensions. Returns [`Error::InvalidVector`] for cosine distance when
    /// either vector has zero magnitude, or for a binary metric when a
    /// coordinate is not exactly zero or one.
    pub fn distance_half(self, left: &HalfVector, right: &HalfVector) -> Result<f32> {
        ensure_same_dimension(left.dimension(), right.dimension())?;

        match self {
            Self::L2 => Ok(l2(left.as_slice(), right.as_slice())),
            Self::InnerProduct => Ok(dot(left.as_slice(), right.as_slice())),
            Self::NegativeInnerProduct => Ok(-dot(left.as_slice(), right.as_slice())),
            Self::Cosine => cosine(left.as_slice(), right.as_slice()),
            Self::L1 => Ok(l1(left.as_slice(), right.as_slice())),
            Self::Hamming => {
                binary_distance(left.as_slice(), right.as_slice(), BinaryMetric::Hamming)
            }
            Self::Jaccard => {
                binary_distance(left.as_slice(), right.as_slice(), BinaryMetric::Jaccard)
            }
        }
    }

    /// Computes this metric between canonical sparse vectors.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DimensionMismatch`] for unequal declared dimensions and
    /// [`Error::InvalidVector`] for cosine distance involving a zero vector or
    /// for a binary metric whose stored values are not exactly one.
    pub fn distance_sparse(self, left: &SparseVector, right: &SparseVector) -> Result<f32> {
        ensure_same_dimension(left.dimensions(), right.dimensions())?;
        let dot = sparse_dot(left, right);
        match self {
            Self::L2 => Ok(sparse_merge(left, right, |a, b| (a - b) * (a - b)).sqrt()),
            Self::InnerProduct => Ok(dot),
            Self::NegativeInnerProduct => Ok(-dot),
            Self::L1 => Ok(sparse_merge(left, right, |a, b| (a - b).abs())),
            Self::Cosine => {
                let left_norm = sparse_dot(left, left).sqrt();
                let right_norm = sparse_dot(right, right).sqrt();
                if left_norm == 0.0 || right_norm == 0.0 {
                    return Err(Error::InvalidVector(
                        "sparse cosine distance is undefined for zero vectors".to_owned(),
                    ));
                }
                Ok(1.0 - (dot / (left_norm * right_norm)))
            }
            Self::Hamming => sparse_binary_distance(left, right, BinaryMetric::Hamming),
            Self::Jaccard => sparse_binary_distance(left, right, BinaryMetric::Jaccard),
        }
    }
}

#[derive(Clone, Copy)]
enum BinaryMetric {
    Hamming,
    Jaccard,
}

fn binary_distance(left: &[f32], right: &[f32], metric: BinaryMetric) -> Result<f32> {
    let (mut intersection, mut union, mut unequal) = (0_u16, 0_u16, 0_u16);
    for (&left, &right) in left.iter().zip(right) {
        let left = binary_coordinate(left)?;
        let right = binary_coordinate(right)?;
        intersection += u16::from(left && right);
        union += u16::from(left || right);
        unequal += u16::from(left != right);
    }
    Ok(binary_score(intersection, union, unequal, metric))
}

fn sparse_binary_distance(
    left: &SparseVector,
    right: &SparseVector,
    metric: BinaryMetric,
) -> Result<f32> {
    for entry in left.entries().iter().chain(right.entries()) {
        if entry.value() != 1.0 {
            return Err(Error::InvalidVector(
                "binary distance coordinates must be exactly zero or one".to_owned(),
            ));
        }
    }

    let (mut i, mut j, mut intersection, mut union) = (0, 0, 0_u16, 0_u16);
    while i < left.entries().len() || j < right.entries().len() {
        match (left.entries().get(i), right.entries().get(j)) {
            (Some(a), Some(b)) if a.index() == b.index() => {
                intersection += 1;
                union += 1;
                i += 1;
                j += 1;
            }
            (Some(a), Some(b)) if a.index() < b.index() => {
                union += 1;
                i += 1;
            }
            (Some(_), Some(_)) => {
                union += 1;
                j += 1;
            }
            (Some(_), None) => {
                union += 1;
                i += 1;
            }
            (None, Some(_)) => {
                union += 1;
                j += 1;
            }
            (None, None) => break,
        }
    }
    let unequal = union - intersection;
    Ok(binary_score(intersection, union, unequal, metric))
}

fn binary_coordinate(value: f32) -> Result<bool> {
    if value == 0.0 {
        Ok(false)
    } else if value == 1.0 {
        Ok(true)
    } else {
        Err(Error::InvalidVector(
            "binary distance coordinates must be exactly zero or one".to_owned(),
        ))
    }
}

fn binary_score(intersection: u16, union: u16, unequal: u16, metric: BinaryMetric) -> f32 {
    match metric {
        BinaryMetric::Hamming => f32::from(unequal),
        BinaryMetric::Jaccard if union == 0 => 0.0,
        BinaryMetric::Jaccard => 1.0 - (f32::from(intersection) / f32::from(union)),
    }
}

fn sparse_dot(left: &SparseVector, right: &SparseVector) -> f32 {
    sparse_merge(left, right, |a, b| a * b)
}

fn sparse_merge(
    left: &SparseVector,
    right: &SparseVector,
    mut f: impl FnMut(f32, f32) -> f32,
) -> f32 {
    let (mut i, mut j, mut sum) = (0, 0, 0.0);
    while i < left.entries().len() || j < right.entries().len() {
        match (left.entries().get(i), right.entries().get(j)) {
            (Some(a), Some(b)) if a.index() == b.index() => {
                sum += f(a.value(), b.value());
                i += 1;
                j += 1;
            }
            (Some(a), Some(b)) if a.index() < b.index() => {
                sum += f(a.value(), 0.0);
                i += 1;
            }
            (Some(_), Some(b)) => {
                sum += f(0.0, b.value());
                j += 1;
            }
            (Some(a), None) => {
                sum += f(a.value(), 0.0);
                i += 1;
            }
            (None, Some(b)) => {
                sum += f(0.0, b.value());
                j += 1;
            }
            (None, None) => break,
        }
    }
    sum
}

fn ensure_same_dimension(left: usize, right: usize) -> Result<()> {
    if left == right {
        Ok(())
    } else {
        Err(Error::DimensionMismatch { left, right })
    }
}

fn l2(left: &[f32], right: &[f32]) -> f32 {
    metric_kernels::l2(left, right)
}

fn l1(left: &[f32], right: &[f32]) -> f32 {
    metric_kernels::l1(left, right)
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    metric_kernels::dot(left, right)
}

fn cosine(left: &[f32], right: &[f32]) -> Result<f32> {
    let (product, left_norm_squared, right_norm_squared) =
        metric_kernels::dot_and_norms(left, right);

    if left_norm_squared == 0.0 || right_norm_squared == 0.0 {
        return Err(Error::InvalidVector(
            "cosine distance is undefined for zero vectors".to_owned(),
        ));
    }

    Ok(1.0 - (product / (left_norm_squared * right_norm_squared).sqrt()))
}
