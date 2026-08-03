//! Exact top-k search over dense vectors.

use crate::{DenseVector, DistanceMetric, Error, Result, policy};

/// Non-zero maximum number of results returned by a search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchLimit(usize);

impl SearchLimit {
    /// Creates a search limit.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidSearchLimit`] when `value` is zero or greater
    /// than [`policy::MAX_SEARCH_LIMIT`].
    pub fn new(value: usize) -> Result<Self> {
        if value == 0 || value > policy::MAX_SEARCH_LIMIT {
            Err(Error::InvalidSearchLimit(value))
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the configured limit.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A vector candidate available to exact search.
#[derive(Debug, Clone, PartialEq)]
pub struct ExactSearchItem {
    point_id: u64,
    vector: DenseVector,
}

impl ExactSearchItem {
    /// Creates a candidate from a stable point identifier and vector.
    #[must_use]
    pub const fn new(point_id: u64, vector: DenseVector) -> Self {
        Self { point_id, vector }
    }
}

/// A scored point returned by exact search.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredPoint {
    point_id: u64,
    score: f32,
}

impl ScoredPoint {
    /// Returns the point identifier.
    #[must_use]
    pub const fn point_id(&self) -> u64 {
        self.point_id
    }

    /// Returns the metric score.
    #[must_use]
    pub const fn score(&self) -> f32 {
        self.score
    }
}

/// Computes exact top-k results for dense vector candidates.
///
/// Results use the metric's canonical score order, then ascending point id for
/// a deterministic tie-break.
#[must_use]
pub fn exact_top_k(
    query: &DenseVector,
    items: &[ExactSearchItem],
    metric: DistanceMetric,
    limit: SearchLimit,
) -> std::vec::IntoIter<Result<ScoredPoint>> {
    let mut scored = Vec::with_capacity(items.len());

    for item in items {
        match metric.distance(query, &item.vector) {
            Ok(score) => scored.push(ScoredPoint {
                point_id: item.point_id,
                score,
            }),
            Err(error) => return vec![Err(error)].into_iter(),
        }
    }

    let order = metric.score_order();
    scored.sort_by(|left, right| {
        order
            .compare(f64::from(left.score), f64::from(right.score))
            .then_with(|| left.point_id.cmp(&right.point_id))
    });
    scored.truncate(limit.get());
    scored.into_iter().map(Ok).collect::<Vec<_>>().into_iter()
}
