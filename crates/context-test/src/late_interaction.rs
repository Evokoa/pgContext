//! Deterministic late-interaction ANN benchmark fixtures.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
};

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{HnswConfig, HnswGraph, HnswPointId};

use crate::{
    BenchmarkDatasetSpec, BenchmarkRow, RecallSummary, deterministic_vector, usize_to_u64,
};

/// Default result count for the late-interaction ANN baseline benchmark.
pub const LATE_INTERACTION_BASELINE_LIMIT: usize = 10;

/// Number of token vectors generated for every deterministic benchmark point.
pub const LATE_INTERACTION_VECTORS_PER_POINT: usize = 2;

/// Number of HNSW token candidates collected for each query vector.
pub const LATE_INTERACTION_CANDIDATES_PER_QUERY: usize = 64;

/// Prepared late-interaction ANN candidate-serving workload.
#[derive(Debug, Clone)]
pub struct LateInteractionAnnBaselineWorkload {
    spec: BenchmarkDatasetSpec,
    query_vectors: Vec<DenseVector>,
    points: Vec<LateInteractionPoint>,
    token_graph: HnswGraph,
    token_to_point_id: BTreeMap<u64, u64>,
    token_memory_bytes: usize,
    vector_bytes: usize,
}

/// One late-interaction point with multiple vectors.
#[derive(Debug, Clone, PartialEq)]
pub struct LateInteractionPoint {
    point_id: u64,
    vectors: Vec<DenseVector>,
}

/// Deterministic late-interaction ANN benchmark summary.
#[derive(Debug, Clone, PartialEq)]
pub struct LateInteractionAnnSummary {
    point_count: usize,
    vectors_per_point: usize,
    token_vector_count: usize,
    candidates_per_query: usize,
    candidate_source_keys: usize,
    projected_comparisons: usize,
    vector_bytes: usize,
    token_graph_bytes: usize,
    exact_ids: Vec<u64>,
    ann_ids: Vec<u64>,
    recall: RecallSummary,
}

impl LateInteractionAnnBaselineWorkload {
    /// Builds a late-interaction ANN workload from a deterministic dataset.
    ///
    /// # Errors
    ///
    /// Returns vector or HNSW validation errors if fixed fixture generation
    /// produces invalid vectors or graph parameters.
    pub fn from_spec(spec: BenchmarkDatasetSpec) -> context_index::Result<Self> {
        let query_vectors = late_interaction_query_vectors(spec)?;
        let points = spec
            .rows_iter()
            .map(|row| late_interaction_point(spec, row?))
            .collect::<context_core::Result<Vec<_>>>()
            .map_err(context_index::HnswError::from)?;
        let mut token_graph = HnswGraph::new(
            DistanceMetric::NegativeInnerProduct,
            HnswConfig::new(16, 64, 64)?,
        );
        let mut token_to_point_id = BTreeMap::new();
        for point in &points {
            for (index, vector) in point.vectors.iter().enumerate() {
                let token_id = late_interaction_token_id(point.point_id, index);
                token_graph.insert(HnswPointId::new(token_id), vector.clone())?;
                token_to_point_id.insert(token_id, point.point_id);
            }
        }
        let memory = token_graph.memory_estimate();
        let vector_bytes = points
            .len()
            .saturating_mul(LATE_INTERACTION_VECTORS_PER_POINT)
            .saturating_mul(spec.dimensions())
            .saturating_mul(size_of::<f32>());

        Ok(Self {
            spec,
            query_vectors,
            points,
            token_graph,
            token_to_point_id,
            token_memory_bytes: memory.total_bytes(),
            vector_bytes,
        })
    }

    /// Returns the dataset specification used by this workload.
    #[must_use]
    pub const fn spec(&self) -> BenchmarkDatasetSpec {
        self.spec
    }

    /// Returns the number of benchmark points.
    #[must_use]
    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    /// Returns the number of generated token vectors.
    #[must_use]
    pub fn token_vector_count(&self) -> usize {
        self.token_to_point_id.len()
    }

    /// Returns the exact multi-vector payload bytes.
    #[must_use]
    pub const fn vector_bytes(&self) -> usize {
        self.vector_bytes
    }

    /// Returns the estimated token HNSW payload bytes.
    #[must_use]
    pub const fn token_graph_bytes(&self) -> usize {
        self.token_memory_bytes
    }

    /// Runs exact MaxSim over every benchmark point.
    #[must_use]
    pub fn exact_top_k(&self, limit: SearchLimit) -> Vec<(u64, f64)> {
        let mut scored = self
            .points
            .iter()
            .map(|point| {
                (
                    point.point_id,
                    maxsim_score(&self.query_vectors, &point.vectors),
                )
            })
            .collect::<Vec<_>>();
        sort_late_interaction_scores(&mut scored);
        scored.truncate(limit.get());
        scored
    }

    /// Collects deduped point IDs from token HNSW candidates.
    ///
    /// # Errors
    ///
    /// Returns HNSW errors if the token graph search fails.
    pub fn ann_candidate_point_ids(
        &self,
        candidates_per_query: SearchLimit,
    ) -> context_index::Result<Vec<u64>> {
        let mut point_ids = Vec::new();
        for query in &self.query_vectors {
            for candidate in self.token_graph.search(query, candidates_per_query)? {
                let token_id = candidate.point_id().get();
                if let Some(point_id) = self.token_to_point_id.get(&token_id)
                    && !point_ids.contains(point_id)
                {
                    point_ids.push(*point_id);
                }
            }
        }
        Ok(point_ids)
    }

    /// Exact-reranks token-HNSW candidate points with MaxSim.
    ///
    /// # Errors
    ///
    /// Returns HNSW errors if token candidate collection fails.
    pub fn ann_rerank(
        &self,
        candidates_per_query: SearchLimit,
        limit: SearchLimit,
    ) -> context_index::Result<Vec<(u64, f64)>> {
        let candidate_ids = self.ann_candidate_point_ids(candidates_per_query)?;
        let candidate_set = candidate_ids.into_iter().collect::<BTreeSet<_>>();
        let mut scored = self
            .points
            .iter()
            .filter(|point| candidate_set.contains(&point.point_id))
            .map(|point| {
                (
                    point.point_id,
                    maxsim_score(&self.query_vectors, &point.vectors),
                )
            })
            .collect::<Vec<_>>();
        sort_late_interaction_scores(&mut scored);
        scored.truncate(limit.get());
        Ok(scored)
    }

    /// Runs the full exact-vs-ANN recall summary.
    ///
    /// # Errors
    ///
    /// Returns validation or HNSW errors if search limits or graph search fail.
    pub fn run_summary(&self) -> context_index::Result<LateInteractionAnnSummary> {
        let limit = SearchLimit::new(LATE_INTERACTION_BASELINE_LIMIT)
            .map_err(context_index::HnswError::from)?;
        let candidates_per_query = SearchLimit::new(LATE_INTERACTION_CANDIDATES_PER_QUERY)
            .map_err(context_index::HnswError::from)?;
        let exact = self.exact_top_k(limit);
        let candidate_source_keys = self.ann_candidate_point_ids(candidates_per_query)?;
        let ann = self.ann_rerank(candidates_per_query, limit)?;
        let exact_ids = exact
            .iter()
            .map(|(point_id, _score)| *point_id)
            .collect::<Vec<_>>();
        let ann_ids = ann
            .iter()
            .map(|(point_id, _score)| *point_id)
            .collect::<Vec<_>>();
        let recall =
            RecallSummary::from_point_ids(exact_ids.iter().copied(), ann_ids.iter().copied());
        Ok(LateInteractionAnnSummary {
            point_count: self.point_count(),
            vectors_per_point: LATE_INTERACTION_VECTORS_PER_POINT,
            token_vector_count: self.token_vector_count(),
            candidates_per_query: LATE_INTERACTION_CANDIDATES_PER_QUERY,
            candidate_source_keys: candidate_source_keys.len(),
            projected_comparisons: self
                .query_vectors
                .len()
                .saturating_mul(candidate_source_keys.len())
                .saturating_mul(LATE_INTERACTION_VECTORS_PER_POINT),
            vector_bytes: self.vector_bytes(),
            token_graph_bytes: self.token_graph_bytes(),
            exact_ids,
            ann_ids,
            recall,
        })
    }
}

impl LateInteractionPoint {
    /// Returns the stable point identifier.
    #[must_use]
    pub const fn point_id(&self) -> u64 {
        self.point_id
    }

    /// Returns the original dense vectors stored for exact MaxSim scoring.
    #[must_use]
    pub fn vectors(&self) -> &[DenseVector] {
        &self.vectors
    }
}

impl LateInteractionAnnSummary {
    /// Returns the number of points in the workload.
    #[must_use]
    pub const fn point_count(&self) -> usize {
        self.point_count
    }

    /// Returns the number of vectors per point.
    #[must_use]
    pub const fn vectors_per_point(&self) -> usize {
        self.vectors_per_point
    }

    /// Returns total token vectors inserted into HNSW.
    #[must_use]
    pub const fn token_vector_count(&self) -> usize {
        self.token_vector_count
    }

    /// Returns the HNSW token candidate budget per query vector.
    #[must_use]
    pub const fn candidates_per_query(&self) -> usize {
        self.candidates_per_query
    }

    /// Returns the deduped candidate source-key count.
    #[must_use]
    pub const fn candidate_source_keys(&self) -> usize {
        self.candidate_source_keys
    }

    /// Returns projected exact MaxSim comparisons for the candidate set.
    #[must_use]
    pub const fn projected_comparisons(&self) -> usize {
        self.projected_comparisons
    }

    /// Returns exact multi-vector payload bytes.
    #[must_use]
    pub const fn vector_bytes(&self) -> usize {
        self.vector_bytes
    }

    /// Returns estimated token HNSW graph bytes.
    #[must_use]
    pub const fn token_graph_bytes(&self) -> usize {
        self.token_graph_bytes
    }

    /// Returns exact MaxSim top-k IDs.
    #[must_use]
    pub fn exact_ids(&self) -> &[u64] {
        &self.exact_ids
    }

    /// Returns ANN candidate-serving reranked IDs.
    #[must_use]
    pub fn ann_ids(&self) -> &[u64] {
        &self.ann_ids
    }

    /// Returns the recall summary against exact MaxSim.
    #[must_use]
    pub const fn recall(&self) -> RecallSummary {
        self.recall
    }
}

fn late_interaction_query_vectors(
    spec: BenchmarkDatasetSpec,
) -> context_core::Result<Vec<DenseVector>> {
    Ok(vec![
        spec.query_vector()?,
        deterministic_vector(spec.seed() ^ 0x6c61_7465_5f71_3032, spec.dimensions())?,
    ])
}

fn late_interaction_point(
    spec: BenchmarkDatasetSpec,
    row: BenchmarkRow,
) -> context_core::Result<LateInteractionPoint> {
    let secondary_seed = spec.seed() ^ row.point_id.wrapping_mul(0xd1b5_4a32_d192_ed03);
    Ok(LateInteractionPoint {
        point_id: row.point_id,
        vectors: vec![
            row.vector,
            deterministic_vector(secondary_seed, spec.dimensions())?,
        ],
    })
}

fn late_interaction_token_id(point_id: u64, vector_index: usize) -> u64 {
    let vector_index = usize_to_u64(vector_index);
    point_id
        .saturating_mul(usize_to_u64(LATE_INTERACTION_VECTORS_PER_POINT))
        .saturating_add(vector_index)
}

fn maxsim_score(query_vectors: &[DenseVector], candidate_vectors: &[DenseVector]) -> f64 {
    query_vectors
        .iter()
        .map(|query| {
            candidate_vectors
                .iter()
                .map(|candidate| dot(query.as_slice(), candidate.as_slice()))
                .fold(f64::NEG_INFINITY, f64::max)
        })
        .sum()
}

fn sort_late_interaction_scores(scored: &mut [(u64, f64)]) {
    scored.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
}

fn dot(left: &[f32], right: &[f32]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(left, right)| f64::from(*left) * f64::from(*right))
        .sum()
}
