//! Shared deterministic fixtures for pgContext tests and benchmarks.
//!
//! This crate is for development support only. Production crates should not
//! depend on it.

use std::{collections::BTreeSet, mem::size_of};

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, ScoredPoint, SearchLimit};
use context_hybrid::{
    CandidateBatch, CandidateBranch, FusedPoint, RankedPoint, RrfK, reciprocal_rank_fusion_batches,
};

mod late_interaction;

pub use late_interaction::{
    LATE_INTERACTION_BASELINE_LIMIT, LATE_INTERACTION_CANDIDATES_PER_QUERY,
    LateInteractionAnnBaselineWorkload, LateInteractionAnnSummary, LateInteractionPoint,
};

/// Maximum accepted latency regression before explicit review is required.
pub const BENCHMARK_LATENCY_REGRESSION_LIMIT: f64 = 0.10;

/// Maximum accepted memory or size regression before explicit review is required.
pub const BENCHMARK_MEMORY_REGRESSION_LIMIT: f64 = 0.05;

/// Maximum accepted absolute recall drop before explicit review is required.
pub const BENCHMARK_RECALL_DROP_LIMIT: f64 = 0.01;

/// Default top-k result count for the hybrid baseline benchmark.
pub const HYBRID_BASELINE_LIMIT: usize = 10;

/// Returns a stable small dataset size used by smoke tests.
#[must_use]
pub const fn smoke_dataset_len() -> usize {
    4
}

/// Fixed benchmark workload scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkDatasetSize {
    /// Fast local smoke benchmark dataset.
    Small,
    /// CI-sized benchmark dataset.
    Medium,
    /// Release-candidate benchmark dataset.
    Large,
}

/// Deterministic benchmark dataset specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchmarkDatasetSpec {
    size: BenchmarkDatasetSize,
    rows: usize,
    dimensions: usize,
    seed: u64,
    tenant_count: usize,
}

impl BenchmarkDatasetSpec {
    /// Returns the fixed small benchmark dataset.
    #[must_use]
    pub const fn small() -> Self {
        Self {
            size: BenchmarkDatasetSize::Small,
            rows: 1_000,
            dimensions: 32,
            seed: 0x7067_6374_5f73_6d6c,
            tenant_count: 10,
        }
    }

    /// Returns the fixed medium benchmark dataset.
    #[must_use]
    pub const fn medium() -> Self {
        Self {
            size: BenchmarkDatasetSize::Medium,
            rows: 100_000,
            dimensions: 64,
            seed: 0x7067_6374_5f6d_6564,
            tenant_count: 100,
        }
    }

    /// Returns the fixed large benchmark dataset.
    #[must_use]
    pub const fn large() -> Self {
        Self {
            size: BenchmarkDatasetSize::Large,
            rows: 1_000_000,
            dimensions: 128,
            seed: 0x7067_6374_5f6c_7267,
            tenant_count: 1_000,
        }
    }

    /// Returns the workload scale.
    #[must_use]
    pub const fn size(self) -> BenchmarkDatasetSize {
        self.size
    }

    /// Returns the number of rows in the dataset.
    #[must_use]
    pub const fn rows(self) -> usize {
        self.rows
    }

    /// Returns dense-vector dimensions for every row.
    #[must_use]
    pub const fn dimensions(self) -> usize {
        self.dimensions
    }

    /// Returns the fixed seed.
    #[must_use]
    pub const fn seed(self) -> u64 {
        self.seed
    }

    /// Returns the number of deterministic tenant buckets.
    #[must_use]
    pub const fn tenant_count(self) -> usize {
        self.tenant_count
    }

    /// Returns an iterator over deterministic benchmark rows.
    #[must_use]
    pub const fn rows_iter(self) -> BenchmarkRows {
        BenchmarkRows {
            spec: self,
            next_index: 0,
        }
    }

    /// Returns the fixed query vector for recall and latency benchmarks.
    ///
    /// # Errors
    ///
    /// Returns a vector validation error only if this fixture is changed to
    /// produce invalid dimensions or non-finite values.
    pub fn query_vector(self) -> context_core::Result<DenseVector> {
        deterministic_vector(self.seed ^ 0x7175_6572_795f_3031, self.dimensions)
    }
}

/// A deterministic benchmark row.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchmarkRow {
    /// One-based point identifier.
    pub point_id: u64,
    /// Stable source key suitable for collection-backed fixtures.
    pub source_key: String,
    /// Dense vector payload.
    pub vector: DenseVector,
    /// Deterministic tenant bucket for selectivity benchmarks.
    pub tenant_id: String,
    /// Deterministic text payload for hybrid benchmarks.
    pub body: String,
}

/// Packed point-id filter for benchmark pre-filter measurements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedPointFilter {
    words: Vec<u64>,
    max_point_id: u64,
    allowed_count: usize,
}

impl PackedPointFilter {
    /// Builds a packed filter from benchmark rows.
    #[must_use]
    pub fn from_rows(rows: &[BenchmarkRow], predicate: impl Fn(&BenchmarkRow) -> bool) -> Self {
        let max_point_id = rows
            .iter()
            .map(|row| row.point_id)
            .max()
            .unwrap_or_default();
        let word_count = point_id_to_word_count(max_point_id);
        let mut filter = Self {
            words: vec![0; word_count],
            max_point_id,
            allowed_count: 0,
        };
        for row in rows {
            if predicate(row) && filter.set(row.point_id) {
                filter.allowed_count = filter.allowed_count.saturating_add(1);
            }
        }
        filter
    }

    /// Returns true when the point identifier is allowed.
    #[must_use]
    pub fn contains_point_id(&self, point_id: u64) -> bool {
        let Some((word_index, bit_index)) = point_id_to_bit_position(point_id) else {
            return false;
        };
        self.words
            .get(word_index)
            .is_some_and(|word| word & (1_u64 << bit_index) != 0)
    }

    /// Returns the number of allowed points.
    #[must_use]
    pub const fn allowed_count(&self) -> usize {
        self.allowed_count
    }

    /// Returns the packed bitmap storage bytes.
    #[must_use]
    pub fn bitmap_bytes(&self) -> usize {
        self.words.len().saturating_mul(size_of::<u64>())
    }

    /// Returns allowed point identifiers in ascending point-id order.
    #[must_use]
    pub fn allowed_point_ids(&self) -> Vec<u64> {
        (1..=self.max_point_id)
            .filter(|point_id| self.contains_point_id(*point_id))
            .collect()
    }

    fn set(&mut self, point_id: u64) -> bool {
        let Some((word_index, bit_index)) = point_id_to_bit_position(point_id) else {
            return false;
        };
        let Some(word) = self.words.get_mut(word_index) else {
            return false;
        };
        let bit = 1_u64 << bit_index;
        let was_unset = *word & bit == 0;
        *word |= bit;
        was_unset
    }
}

/// Iterator over deterministic benchmark rows.
#[derive(Debug, Clone)]
pub struct BenchmarkRows {
    spec: BenchmarkDatasetSpec,
    next_index: usize,
}

impl Iterator for BenchmarkRows {
    type Item = context_core::Result<BenchmarkRow>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_index >= self.spec.rows {
            return None;
        }

        let row_index = self.next_index;
        self.next_index = self.next_index.saturating_add(1);
        Some(benchmark_row(self.spec, row_index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.spec.rows.saturating_sub(self.next_index);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for BenchmarkRows {}

/// Prepared exact-search baseline workload.
#[derive(Debug, Clone)]
pub struct ExactSearchBaselineWorkload {
    spec: BenchmarkDatasetSpec,
    query: DenseVector,
    items: Vec<ExactSearchItem>,
    vector_bytes: usize,
}

/// Prepared hybrid branch baseline workload.
#[derive(Debug, Clone)]
pub struct HybridBaselineWorkload {
    spec: BenchmarkDatasetSpec,
    dense: CandidateBatch,
    text: CandidateBatch,
    sparse_planned: CandidateBatch,
    empty: CandidateBatch,
}

/// One named hybrid baseline benchmark case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HybridBenchmarkCase {
    name: &'static str,
    batches: Vec<CandidateBatch>,
}

/// Deterministic hybrid benchmark case summary.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridBenchmarkSummary {
    case_name: &'static str,
    branch_count: usize,
    non_empty_branch_count: usize,
    input_candidate_count: usize,
    output_count: usize,
    elapsed_ns: u128,
    top_point_id: Option<u64>,
    fused: Vec<FusedPoint>,
}

impl ExactSearchBaselineWorkload {
    /// Builds an exact-search baseline workload from a deterministic dataset.
    ///
    /// # Errors
    ///
    /// Returns vector validation errors if the fixed dataset generator produces
    /// invalid dense vectors.
    pub fn from_spec(spec: BenchmarkDatasetSpec) -> context_core::Result<Self> {
        let query = spec.query_vector()?;
        let rows = spec.rows_iter().collect::<Result<Vec<_>, _>>()?;
        let vector_bytes = rows
            .len()
            .saturating_mul(spec.dimensions())
            .saturating_mul(size_of::<f32>());
        let items = rows
            .into_iter()
            .map(|row| ExactSearchItem::new(row.point_id, row.vector))
            .collect::<Vec<_>>();

        Ok(Self {
            spec,
            query,
            items,
            vector_bytes,
        })
    }

    /// Returns the dataset specification used by this workload.
    #[must_use]
    pub const fn spec(&self) -> BenchmarkDatasetSpec {
        self.spec
    }

    /// Returns the number of candidate vectors.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Returns the dense-vector payload bytes in the workload.
    #[must_use]
    pub const fn vector_bytes(&self) -> usize {
        self.vector_bytes
    }

    /// Runs exact top-k search against the prepared workload.
    ///
    /// # Errors
    ///
    /// Returns distance errors if the query and candidate dimensions are
    /// incompatible.
    pub fn run(
        &self,
        metric: DistanceMetric,
        limit: SearchLimit,
    ) -> context_core::Result<Vec<ScoredPoint>> {
        context_core::exact_top_k(&self.query, &self.items, metric, limit).collect()
    }
}

impl HybridBaselineWorkload {
    /// Builds a hybrid baseline workload from a deterministic dataset.
    ///
    /// # Errors
    ///
    /// Returns vector validation errors if the fixed dataset generator produces
    /// invalid dense vectors.
    pub fn from_spec(spec: BenchmarkDatasetSpec) -> context_core::Result<Self> {
        let dense = dense_hybrid_branch(spec)?;
        let text = text_hybrid_branch(spec)?;
        let sparse_planned = CandidateBatch::new(CandidateBranch::SparsePlanned, Vec::new());
        let empty = CandidateBatch::new(CandidateBranch::UserProvided, Vec::new());

        Ok(Self {
            spec,
            dense,
            text,
            sparse_planned,
            empty,
        })
    }

    /// Returns the dataset specification used by this workload.
    #[must_use]
    pub const fn spec(&self) -> BenchmarkDatasetSpec {
        self.spec
    }

    /// Returns all release-gate hybrid benchmark cases.
    #[must_use]
    pub fn cases(&self) -> Vec<HybridBenchmarkCase> {
        vec![
            HybridBenchmarkCase::new("dense_only", vec![self.dense.clone()]),
            HybridBenchmarkCase::new("text_only", vec![self.text.clone()]),
            HybridBenchmarkCase::new("sparse_planned", vec![self.sparse_planned.clone()]),
            HybridBenchmarkCase::new(
                "fused_dense_text",
                vec![self.dense.clone(), self.text.clone()],
            ),
            HybridBenchmarkCase::new("fully_empty", vec![self.empty.clone()]),
        ]
    }

    /// Runs one hybrid benchmark case and returns a structured summary.
    #[must_use]
    pub fn run_case(&self, case: &HybridBenchmarkCase, elapsed_ns: u128) -> HybridBenchmarkSummary {
        let fused =
            reciprocal_rank_fusion_batches(case.batches(), RrfK::STANDARD, HYBRID_BASELINE_LIMIT);
        let input_candidate_count = case
            .batches()
            .iter()
            .map(|batch| batch.points().len())
            .sum::<usize>();
        let non_empty_branch_count = case
            .batches()
            .iter()
            .filter(|batch| !batch.points().is_empty())
            .count();

        HybridBenchmarkSummary {
            case_name: case.name(),
            branch_count: case.batches().len(),
            non_empty_branch_count,
            input_candidate_count,
            output_count: fused.len(),
            elapsed_ns,
            top_point_id: fused.first().map(|point| point.point_id()),
            fused,
        }
    }
}

impl HybridBenchmarkCase {
    fn new(name: &'static str, batches: Vec<CandidateBatch>) -> Self {
        Self { name, batches }
    }

    /// Returns the stable case name printed by the benchmark runner.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the branch batches measured by this case.
    #[must_use]
    pub fn batches(&self) -> &[CandidateBatch] {
        &self.batches
    }
}

impl HybridBenchmarkSummary {
    /// Returns the stable case name.
    #[must_use]
    pub const fn case_name(&self) -> &'static str {
        self.case_name
    }

    /// Returns the number of branches in the case.
    #[must_use]
    pub const fn branch_count(&self) -> usize {
        self.branch_count
    }

    /// Returns the number of branches with at least one candidate.
    #[must_use]
    pub const fn non_empty_branch_count(&self) -> usize {
        self.non_empty_branch_count
    }

    /// Returns total input candidates across all branches.
    #[must_use]
    pub const fn input_candidate_count(&self) -> usize {
        self.input_candidate_count
    }

    /// Returns fused output count.
    #[must_use]
    pub const fn output_count(&self) -> usize {
        self.output_count
    }

    /// Returns measured elapsed nanoseconds supplied by the runner.
    #[must_use]
    pub const fn elapsed_ns(&self) -> u128 {
        self.elapsed_ns
    }

    /// Returns the first fused point ID, if any.
    #[must_use]
    pub const fn top_point_id(&self) -> Option<u64> {
        self.top_point_id
    }

    /// Returns the fused output points.
    #[must_use]
    pub fn fused(&self) -> &[FusedPoint] {
        &self.fused
    }

    /// Returns the same summary with a measured elapsed duration attached.
    #[must_use]
    pub const fn with_elapsed_ns(mut self, elapsed_ns: u128) -> Self {
        self.elapsed_ns = elapsed_ns;
        self
    }
}

/// Recall comparison summary for exact and approximate point IDs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RecallSummary {
    exact_count: usize,
    candidate_count: usize,
    intersection_count: usize,
    recall: f64,
}

impl RecallSummary {
    /// Computes recall from exact and candidate point identifiers.
    #[must_use]
    pub fn from_point_ids(
        exact_point_ids: impl IntoIterator<Item = u64>,
        candidate_point_ids: impl IntoIterator<Item = u64>,
    ) -> Self {
        let exact = exact_point_ids.into_iter().collect::<BTreeSet<_>>();
        let candidates = candidate_point_ids.into_iter().collect::<BTreeSet<_>>();
        let intersection_count = exact.intersection(&candidates).count();
        let recall = if exact.is_empty() {
            1.0
        } else {
            usize_to_f64(intersection_count) / usize_to_f64(exact.len())
        };

        Self {
            exact_count: exact.len(),
            candidate_count: candidates.len(),
            intersection_count,
            recall,
        }
    }

    /// Returns the unique exact point count.
    #[must_use]
    pub const fn exact_count(self) -> usize {
        self.exact_count
    }

    /// Returns the unique candidate point count.
    #[must_use]
    pub const fn candidate_count(self) -> usize {
        self.candidate_count
    }

    /// Returns the exact/candidate intersection count.
    #[must_use]
    pub const fn intersection_count(self) -> usize {
        self.intersection_count
    }

    /// Returns recall in `0..=1`.
    #[must_use]
    pub const fn recall(self) -> f64 {
        self.recall
    }
}

/// Benchmark metric category used by release delta checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchmarkDeltaMetric {
    /// Elapsed time or latency where lower values are better.
    Latency,
    /// Memory, vector payload bytes, graph bytes, index bytes, or codebook bytes.
    Memory,
    /// Recall where higher values are better.
    Recall,
}

impl BenchmarkDeltaMetric {
    const fn max_regression(self) -> f64 {
        match self {
            Self::Latency => BENCHMARK_LATENCY_REGRESSION_LIMIT,
            Self::Memory => BENCHMARK_MEMORY_REGRESSION_LIMIT,
            Self::Recall => BENCHMARK_RECALL_DROP_LIMIT,
        }
    }
}

/// Accepted or review-required benchmark delta decision.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BenchmarkDeltaDecision {
    /// The metric stayed within the release threshold.
    Accepted(BenchmarkDeltaSummary),
    /// The metric regressed beyond the release threshold and needs review.
    ReviewRequired(BenchmarkDeltaSummary),
}

impl BenchmarkDeltaDecision {
    /// Returns the underlying delta summary.
    #[must_use]
    pub const fn summary(self) -> BenchmarkDeltaSummary {
        match self {
            Self::Accepted(summary) | Self::ReviewRequired(summary) => summary,
        }
    }

    /// Returns true when the delta requires explicit review before acceptance.
    #[must_use]
    pub const fn requires_review(self) -> bool {
        matches!(self, Self::ReviewRequired(_))
    }
}

/// Normalized benchmark delta details.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BenchmarkDeltaSummary {
    metric: BenchmarkDeltaMetric,
    baseline: f64,
    current: f64,
    allowed_regression: f64,
    actual_regression: f64,
}

impl BenchmarkDeltaSummary {
    /// Returns the metric category.
    #[must_use]
    pub const fn metric(self) -> BenchmarkDeltaMetric {
        self.metric
    }

    /// Returns the baseline value.
    #[must_use]
    pub const fn baseline(self) -> f64 {
        self.baseline
    }

    /// Returns the current value.
    #[must_use]
    pub const fn current(self) -> f64 {
        self.current
    }

    /// Returns the allowed fractional or absolute regression threshold.
    #[must_use]
    pub const fn allowed_regression(self) -> f64 {
        self.allowed_regression
    }

    /// Returns the observed fractional or absolute regression.
    #[must_use]
    pub const fn actual_regression(self) -> f64 {
        self.actual_regression
    }
}

/// Invalid benchmark delta input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BenchmarkDeltaError {
    /// Lower-is-better metrics need a positive finite baseline.
    InvalidPositiveBaseline {
        /// Metric being evaluated.
        metric: BenchmarkDeltaMetric,
        /// Invalid baseline value.
        baseline: f64,
    },
    /// Metric values must be finite and non-negative.
    InvalidCurrent {
        /// Metric being evaluated.
        metric: BenchmarkDeltaMetric,
        /// Invalid current value.
        current: f64,
    },
    /// Recall values must be finite and in `0..=1`.
    InvalidRecall {
        /// Invalid recall value.
        value: f64,
    },
}

/// Evaluates a benchmark metric against the release delta threshold.
///
/// # Errors
///
/// Returns an error when the baseline or current value is non-finite, negative,
/// outside the recall domain, or unusable for a fractional regression check.
pub fn evaluate_benchmark_delta(
    metric: BenchmarkDeltaMetric,
    baseline: f64,
    current: f64,
) -> Result<BenchmarkDeltaDecision, BenchmarkDeltaError> {
    match metric {
        BenchmarkDeltaMetric::Latency | BenchmarkDeltaMetric::Memory => {
            evaluate_lower_is_better_delta(metric, baseline, current)
        }
        BenchmarkDeltaMetric::Recall => evaluate_recall_delta(baseline, current),
    }
}

fn evaluate_lower_is_better_delta(
    metric: BenchmarkDeltaMetric,
    baseline: f64,
    current: f64,
) -> Result<BenchmarkDeltaDecision, BenchmarkDeltaError> {
    if !baseline.is_finite() || baseline <= 0.0 {
        return Err(BenchmarkDeltaError::InvalidPositiveBaseline { metric, baseline });
    }
    if !current.is_finite() || current < 0.0 {
        return Err(BenchmarkDeltaError::InvalidCurrent { metric, current });
    }

    let actual_regression = ((current - baseline) / baseline).max(0.0);
    let summary = BenchmarkDeltaSummary {
        metric,
        baseline,
        current,
        allowed_regression: metric.max_regression(),
        actual_regression,
    };
    if exceeds_allowed_regression(actual_regression, summary.allowed_regression) {
        Ok(BenchmarkDeltaDecision::ReviewRequired(summary))
    } else {
        Ok(BenchmarkDeltaDecision::Accepted(summary))
    }
}

fn evaluate_recall_delta(
    baseline: f64,
    current: f64,
) -> Result<BenchmarkDeltaDecision, BenchmarkDeltaError> {
    validate_recall_delta_value(baseline)?;
    validate_recall_delta_value(current)?;

    let metric = BenchmarkDeltaMetric::Recall;
    let actual_regression = (baseline - current).max(0.0);
    let summary = BenchmarkDeltaSummary {
        metric,
        baseline,
        current,
        allowed_regression: metric.max_regression(),
        actual_regression,
    };
    if exceeds_allowed_regression(actual_regression, summary.allowed_regression) {
        Ok(BenchmarkDeltaDecision::ReviewRequired(summary))
    } else {
        Ok(BenchmarkDeltaDecision::Accepted(summary))
    }
}

fn exceeds_allowed_regression(actual_regression: f64, allowed_regression: f64) -> bool {
    actual_regression - allowed_regression > f64::EPSILON
}

fn validate_recall_delta_value(value: f64) -> Result<(), BenchmarkDeltaError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(BenchmarkDeltaError::InvalidRecall { value })
    }
}

fn benchmark_row(
    spec: BenchmarkDatasetSpec,
    row_index: usize,
) -> context_core::Result<BenchmarkRow> {
    let point_id = usize_to_u64(row_index.saturating_add(1));
    let tenant_index = row_index % spec.tenant_count;
    let vector_seed = spec.seed ^ point_id.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let vector = deterministic_vector(vector_seed, spec.dimensions)?;
    Ok(BenchmarkRow {
        point_id,
        source_key: format!("bench-{point_id:012}"),
        vector,
        tenant_id: format!("tenant-{tenant_index:04}"),
        body: benchmark_body(spec.size, point_id, tenant_index),
    })
}

fn dense_hybrid_branch(spec: BenchmarkDatasetSpec) -> context_core::Result<CandidateBatch> {
    let points = spec
        .rows_iter()
        .take(100)
        .map(|row| row.map(|row| RankedPoint::new(row.point_id)))
        .collect::<context_core::Result<Vec<_>>>()?;
    Ok(CandidateBatch::new(CandidateBranch::DenseExact, points))
}

fn text_hybrid_branch(spec: BenchmarkDatasetSpec) -> context_core::Result<CandidateBatch> {
    let mut points = spec
        .rows_iter()
        .filter_map(|row| match row {
            Ok(row) if row.body.contains("database") => Some(Ok(RankedPoint::new(row.point_id))),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .take(100)
        .collect::<context_core::Result<Vec<_>>>()?;
    points.reverse();
    Ok(CandidateBatch::new(CandidateBranch::FullText, points))
}

fn deterministic_vector(seed: u64, dimensions: usize) -> context_core::Result<DenseVector> {
    let mut state = SplitMix64::new(seed);
    let values = (0..dimensions)
        .map(|_| state.next_f32_signed_unit())
        .collect::<Vec<_>>();
    DenseVector::new(values)
}

fn benchmark_body(size: BenchmarkDatasetSize, point_id: u64, tenant_index: usize) -> String {
    let scale = match size {
        BenchmarkDatasetSize::Small => "small",
        BenchmarkDatasetSize::Medium => "medium",
        BenchmarkDatasetSize::Large => "large",
    };
    let topic = match point_id % 5 {
        0 => "database",
        1 => "storage",
        2 => "retrieval",
        3 => "postgres",
        _ => "vector",
    };
    format!("{scale} {topic} tenant-{tenant_index:04} document-{point_id:012}")
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn usize_to_f64(value: usize) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn point_id_to_word_count(max_point_id: u64) -> usize {
    let Some(zero_based) = max_point_id.checked_sub(1) else {
        return 0;
    };
    let word_count = zero_based / 64 + 1;
    usize::try_from(word_count).unwrap_or(usize::MAX)
}

fn point_id_to_bit_position(point_id: u64) -> Option<(usize, u32)> {
    let zero_based = point_id.checked_sub(1)?;
    let word_index = usize::try_from(zero_based / 64).ok()?;
    let bit_index = u32::try_from(zero_based % 64).ok()?;
    Some((word_index, bit_index))
}

#[derive(Debug, Clone, Copy)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }

    fn next_f32_signed_unit(&mut self) -> f32 {
        let mantissa = u32::try_from(self.next_u64() & 0x007f_ffff).unwrap_or(0);
        let unit = f32::from_bits(0x3f80_0000 | mantissa) - 1.0;
        unit.mul_add(2.0, -1.0)
    }
}
