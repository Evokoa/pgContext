//! Deterministic, memory-budgeted external-build primitives.

use std::error::Error;
use std::fmt::{Display, Formatter};

use context_core::DistanceMetric;

const CHECKSUM_BYTES: usize = size_of::<u64>();
const LENGTH_BYTES: usize = size_of::<u32>();
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Positive byte budget for one in-memory build run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudget(usize);

impl MemoryBudget {
    /// Creates a positive memory budget.
    #[must_use]
    pub const fn new(bytes: usize) -> Option<Self> {
        if bytes == 0 { None } else { Some(Self(bytes)) }
    }

    /// Returns allowed encoded bytes per run, including framing and checksum.
    #[must_use]
    pub const fn bytes(self) -> usize {
        self.0
    }
}

/// One checksummed external run. Adapters may persist `encoded` verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpillRun {
    encoded: Vec<u8>,
}

impl SpillRun {
    /// Partitions records into deterministic runs bounded by their complete
    /// encoded representation. Empty records still consume their length frame,
    /// so input cardinality cannot bypass the memory budget.
    ///
    /// # Errors
    ///
    /// Returns [`SpillError::RecordExceedsBudget`] for an indivisible record,
    /// or [`SpillError::LengthOverflow`] when its persisted length cannot fit.
    pub fn partition(records: &[&[u8]], budget: MemoryBudget) -> Result<Vec<Self>, SpillError> {
        let mut runs = Vec::new();
        let mut current = Vec::<u8>::new();
        for record in records {
            let length = u32::try_from(record.len()).map_err(|_| SpillError::LengthOverflow)?;
            let record_bytes = LENGTH_BYTES
                .checked_add(record.len())
                .ok_or(SpillError::LengthOverflow)?;
            let required = record_bytes
                .checked_add(CHECKSUM_BYTES)
                .ok_or(SpillError::LengthOverflow)?;
            if required > budget.bytes() {
                return Err(SpillError::RecordExceedsBudget);
            }
            let next = current
                .len()
                .checked_add(required)
                .ok_or(SpillError::LengthOverflow)?;
            if !current.is_empty() && next > budget.bytes() {
                runs.push(Self::finish_encoded(current));
                current = Vec::new();
            }
            current.extend_from_slice(&length.to_le_bytes());
            current.extend_from_slice(record);
        }
        if !current.is_empty() {
            runs.push(Self::finish_encoded(current));
        }
        Ok(runs)
    }

    fn finish_encoded(mut encoded: Vec<u8>) -> Self {
        let checksum = checksum(&encoded);
        encoded.extend_from_slice(&checksum.to_le_bytes());
        Self { encoded }
    }

    /// Validates and attaches an encoded run read by a storage adapter.
    ///
    /// # Errors
    ///
    /// Returns corruption errors before any record is exposed.
    pub fn from_encoded(encoded: Vec<u8>) -> Result<Self, SpillError> {
        let run = Self { encoded };
        run.validate()?;
        Ok(run)
    }

    /// Returns the portable encoded representation, including its checksum.
    #[must_use]
    pub fn encoded(&self) -> &[u8] {
        &self.encoded
    }

    /// Decodes all records after checking the entire run.
    ///
    /// # Errors
    ///
    /// Returns corruption errors without returning a partial record list.
    pub fn records(&self) -> Result<Vec<Vec<u8>>, SpillError> {
        let payload = self.validate()?;
        let mut cursor = 0_usize;
        let mut records = Vec::new();
        while cursor < payload.len() {
            let end = cursor
                .checked_add(LENGTH_BYTES)
                .ok_or(SpillError::Truncated)?;
            let length_bytes: [u8; LENGTH_BYTES] = payload
                .get(cursor..end)
                .ok_or(SpillError::Truncated)?
                .try_into()
                .map_err(|_| SpillError::Truncated)?;
            cursor = end;
            let length = usize::try_from(u32::from_le_bytes(length_bytes))
                .map_err(|_| SpillError::LengthOverflow)?;
            let end = cursor
                .checked_add(length)
                .ok_or(SpillError::LengthOverflow)?;
            records.push(
                payload
                    .get(cursor..end)
                    .ok_or(SpillError::Truncated)?
                    .to_vec(),
            );
            cursor = end;
        }
        Ok(records)
    }

    fn validate(&self) -> Result<&[u8], SpillError> {
        let split = self
            .encoded
            .len()
            .checked_sub(CHECKSUM_BYTES)
            .ok_or(SpillError::Truncated)?;
        let (payload, checksum_bytes) = self.encoded.split_at(split);
        let expected = u64::from_le_bytes(
            checksum_bytes
                .try_into()
                .map_err(|_| SpillError::Truncated)?,
        );
        if checksum(payload) != expected {
            return Err(SpillError::ChecksumMismatch);
        }
        let mut cursor = 0_usize;
        while cursor < payload.len() {
            let end = cursor
                .checked_add(LENGTH_BYTES)
                .ok_or(SpillError::Truncated)?;
            let length_bytes: [u8; LENGTH_BYTES] = payload
                .get(cursor..end)
                .ok_or(SpillError::Truncated)?
                .try_into()
                .map_err(|_| SpillError::Truncated)?;
            let length = usize::try_from(u32::from_le_bytes(length_bytes))
                .map_err(|_| SpillError::LengthOverflow)?;
            cursor = end.checked_add(length).ok_or(SpillError::LengthOverflow)?;
            if cursor > payload.len() {
                return Err(SpillError::Truncated);
            }
        }
        Ok(payload)
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

/// Rejected spill-run operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpillError {
    /// One record cannot fit into an otherwise empty run.
    RecordExceedsBudget,
    /// A record or offset cannot be represented safely.
    LengthOverflow,
    /// The encoded run ends before a complete record or checksum.
    Truncated,
    /// The recorded checksum differs from the complete encoded payload.
    ChecksumMismatch,
}

impl Display for SpillError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RecordExceedsBudget => "spill record exceeds the memory budget",
            Self::LengthOverflow => "spill record length overflow",
            Self::Truncated => "spill run is truncated",
            Self::ChecksumMismatch => "spill run checksum mismatch",
        })
    }
}

impl Error for SpillError {}

/// Deterministically chooses distinct source indices without replacement.
#[must_use]
pub fn deterministic_sample(population: usize, requested: usize, seed: u64) -> Vec<usize> {
    let mut ranked = (0..population)
        .map(|index| {
            let stable_index = u64::try_from(index).unwrap_or(u64::MAX);
            (splitmix64(seed ^ stable_index), index)
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable();
    ranked.truncate(requested.min(population));
    ranked.into_iter().map(|(_, index)| index).collect()
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Deterministic scalar k-means output used as the exact training oracle.
#[derive(Debug, Clone, PartialEq)]
pub struct KmeansResult {
    centroids: Vec<Vec<f32>>,
    assignments: Vec<usize>,
}

impl KmeansResult {
    /// Returns centroids in stable cluster order.
    #[must_use]
    pub fn centroids(&self) -> &[Vec<f32>] {
        &self.centroids
    }

    /// Returns one stable cluster assignment per input vector.
    #[must_use]
    pub fn assignments(&self) -> &[usize] {
        &self.assignments
    }
}

/// Trains a deterministic scalar L2 k-means oracle.
///
/// Empty clusters retain their prior centroid. Equal distances choose the
/// lower cluster index.
///
/// # Errors
///
/// Rejects empty/ragged/non-finite input, invalid cluster counts, and zero
/// iteration budgets.
pub fn deterministic_kmeans(
    vectors: &[Vec<f32>],
    clusters: usize,
    max_iterations: usize,
    seed: u64,
) -> Result<KmeansResult, TrainingError> {
    let dimensions = vectors.first().ok_or(TrainingError::EmptyInput)?.len();
    if dimensions == 0 {
        return Err(TrainingError::ZeroDimensions);
    }
    if clusters == 0 || clusters > vectors.len() {
        return Err(TrainingError::InvalidClusterCount);
    }
    if max_iterations == 0 {
        return Err(TrainingError::ZeroIterations);
    }
    if vectors
        .iter()
        .any(|vector| vector.len() != dimensions || vector.iter().any(|value| !value.is_finite()))
    {
        return Err(TrainingError::InvalidVector);
    }

    let mut centroids = deterministic_sample(vectors.len(), clusters, seed)
        .into_iter()
        .map(|index| vectors[index].clone())
        .collect::<Vec<_>>();
    let mut assignments = vec![usize::MAX; vectors.len()];
    for _ in 0..max_iterations {
        let next = vectors
            .iter()
            .map(|vector| nearest_centroid(vector, &centroids))
            .collect::<Vec<_>>();
        if next == assignments {
            break;
        }
        assignments = next;
        let mut sums = vec![vec![0.0_f64; dimensions]; clusters];
        let mut counts = vec![0_usize; clusters];
        for (vector, cluster) in vectors.iter().zip(assignments.iter().copied()) {
            counts[cluster] += 1;
            for (sum, value) in sums[cluster].iter_mut().zip(vector) {
                *sum += f64::from(*value);
            }
        }
        for cluster in 0..clusters {
            if counts[cluster] == 0 {
                continue;
            }
            for dimension in 0..dimensions {
                let count = u32::try_from(counts[cluster])
                    .map_err(|_| TrainingError::PopulationTooLarge)?;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "the f64 accumulation deliberately rounds once into the declared f32 centroid representation"
                )]
                let centroid = (sums[cluster][dimension] / f64::from(count)) as f32;
                centroids[cluster][dimension] = centroid;
            }
        }
    }
    assignments = vectors
        .iter()
        .map(|vector| nearest_centroid(vector, &centroids))
        .collect();
    Ok(KmeansResult {
        centroids,
        assignments,
    })
}

/// Trains deterministic centroids using the routing metric's value domain.
///
/// Continuous metrics use arithmetic means after metric-aware assignment.
/// Hamming uses per-coordinate binary majorities, retaining the prior bit on
/// an exact tie. Jaccard uses a deterministic cluster medoid so every centroid
/// remains a valid binary vector.
///
/// # Errors
///
/// Rejects empty/ragged/non-finite input, invalid cluster counts, zero
/// iteration budgets, and values rejected by the selected metric.
pub fn deterministic_metric_clusters(
    vectors: &[Vec<f32>],
    clusters: usize,
    max_iterations: usize,
    seed: u64,
    metric: DistanceMetric,
) -> Result<KmeansResult, TrainingError> {
    deterministic_metric_clusters_with_workers(vectors, clusters, max_iterations, seed, metric, 1)
}

/// Trains deterministic metric-aware centroids with parallel assignment.
///
/// Assignment chunks are joined in source order and centroid updates remain
/// scalar, so changing `workers` cannot change the logical result.
///
/// # Errors
///
/// Returns the same validation errors as [`deterministic_metric_clusters`],
/// plus [`TrainingError::InvalidWorkerCount`] when `workers` is zero.
pub fn deterministic_metric_clusters_with_workers(
    vectors: &[Vec<f32>],
    clusters: usize,
    max_iterations: usize,
    seed: u64,
    metric: DistanceMetric,
    workers: usize,
) -> Result<KmeansResult, TrainingError> {
    let dimensions = validate_training_input(vectors, clusters, max_iterations)?;
    if workers == 0 {
        return Err(TrainingError::InvalidWorkerCount);
    }
    let mut centroids = deterministic_sample(vectors.len(), clusters, seed)
        .into_iter()
        .map(|index| vectors[index].clone())
        .collect::<Vec<_>>();
    let mut assignments = vec![usize::MAX; vectors.len()];
    for _ in 0..max_iterations {
        let next = metric_assignments(vectors, &centroids, metric, workers)?;
        if next == assignments {
            break;
        }
        assignments = next;
        match metric {
            DistanceMetric::Hamming => update_binary_majorities(
                vectors,
                &assignments,
                &mut centroids,
                clusters,
                dimensions,
            ),
            DistanceMetric::Jaccard => {
                update_jaccard_medoids(vectors, &assignments, &mut centroids, clusters, metric)?
            }
            _ => update_arithmetic_means(
                vectors,
                &assignments,
                &mut centroids,
                clusters,
                dimensions,
            )?,
        }
    }
    assignments = metric_assignments(vectors, &centroids, metric, workers)?;
    Ok(KmeansResult {
        centroids,
        assignments,
    })
}

fn metric_assignments(
    vectors: &[Vec<f32>],
    centroids: &[Vec<f32>],
    metric: DistanceMetric,
    workers: usize,
) -> Result<Vec<usize>, TrainingError> {
    let workers = workers.min(vectors.len()).max(1);
    if workers == 1 {
        return vectors
            .iter()
            .map(|vector| nearest_metric_centroid(vector, centroids, metric))
            .collect();
    }
    let chunk_size = vectors.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let handles = vectors
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|vector| nearest_metric_centroid(vector, centroids, metric))
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .collect::<Vec<_>>();
        let mut assignments = Vec::with_capacity(vectors.len());
        for handle in handles {
            let chunk = handle
                .join()
                .unwrap_or_else(|payload| std::panic::resume_unwind(payload))?;
            assignments.extend(chunk);
        }
        Ok(assignments)
    })
}

fn validate_training_input(
    vectors: &[Vec<f32>],
    clusters: usize,
    max_iterations: usize,
) -> Result<usize, TrainingError> {
    let dimensions = vectors.first().ok_or(TrainingError::EmptyInput)?.len();
    if dimensions == 0 {
        return Err(TrainingError::ZeroDimensions);
    }
    if clusters == 0 || clusters > vectors.len() {
        return Err(TrainingError::InvalidClusterCount);
    }
    if max_iterations == 0 {
        return Err(TrainingError::ZeroIterations);
    }
    if vectors
        .iter()
        .any(|vector| vector.len() != dimensions || vector.iter().any(|value| !value.is_finite()))
    {
        return Err(TrainingError::InvalidVector);
    }
    Ok(dimensions)
}

fn nearest_metric_centroid(
    vector: &[f32],
    centroids: &[Vec<f32>],
    metric: DistanceMetric,
) -> Result<usize, TrainingError> {
    let mut best = None::<(usize, f32)>;
    for (index, centroid) in centroids.iter().enumerate() {
        let score = metric
            .distance_slices(vector, centroid)
            .map_err(|_| TrainingError::InvalidVector)?;
        let replace = best.is_none_or(|(best_index, best_score)| {
            metric
                .score_order()
                .compare(f64::from(score), f64::from(best_score))
                .then(index.cmp(&best_index))
                .is_lt()
        });
        if replace {
            best = Some((index, score));
        }
    }
    best.map(|(index, _)| index)
        .ok_or(TrainingError::InvalidClusterCount)
}

fn update_arithmetic_means(
    vectors: &[Vec<f32>],
    assignments: &[usize],
    centroids: &mut [Vec<f32>],
    clusters: usize,
    dimensions: usize,
) -> Result<(), TrainingError> {
    let mut sums = vec![vec![0.0_f64; dimensions]; clusters];
    let mut counts = vec![0_usize; clusters];
    for (vector, cluster) in vectors.iter().zip(assignments.iter().copied()) {
        counts[cluster] += 1;
        for (sum, value) in sums[cluster].iter_mut().zip(vector) {
            *sum += f64::from(*value);
        }
    }
    for cluster in 0..clusters {
        if counts[cluster] == 0 {
            continue;
        }
        let count =
            u32::try_from(counts[cluster]).map_err(|_| TrainingError::PopulationTooLarge)?;
        for dimension in 0..dimensions {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "the f64 accumulator deliberately rounds once into the f32 centroid domain"
            )]
            let value = (sums[cluster][dimension] / f64::from(count)) as f32;
            centroids[cluster][dimension] = value;
        }
    }
    Ok(())
}

fn update_binary_majorities(
    vectors: &[Vec<f32>],
    assignments: &[usize],
    centroids: &mut [Vec<f32>],
    clusters: usize,
    dimensions: usize,
) {
    let mut ones = vec![vec![0_usize; dimensions]; clusters];
    let mut counts = vec![0_usize; clusters];
    for (vector, cluster) in vectors.iter().zip(assignments.iter().copied()) {
        counts[cluster] += 1;
        for (count, value) in ones[cluster].iter_mut().zip(vector) {
            *count += usize::from(*value == 1.0);
        }
    }
    for cluster in 0..clusters {
        if counts[cluster] == 0 {
            continue;
        }
        for dimension in 0..dimensions {
            let doubled = ones[cluster][dimension].saturating_mul(2);
            centroids[cluster][dimension] = match doubled.cmp(&counts[cluster]) {
                std::cmp::Ordering::Greater => 1.0,
                std::cmp::Ordering::Less => 0.0,
                std::cmp::Ordering::Equal => centroids[cluster][dimension],
            };
        }
    }
}

fn update_jaccard_medoids(
    vectors: &[Vec<f32>],
    assignments: &[usize],
    centroids: &mut [Vec<f32>],
    clusters: usize,
    metric: DistanceMetric,
) -> Result<(), TrainingError> {
    // Exact medoids are quadratic in a cluster's population. Preserve exact
    // behavior for small fixtures, while deterministically sampling both the
    // candidate and evaluation sets for production-sized clusters. The two
    // caps bound one cluster update to 65,536 distance evaluations.
    const MAX_MEDOID_CANDIDATES: usize = 64;
    const MAX_MEDOID_EVALUATORS: usize = 1_024;

    for (cluster, centroid) in centroids.iter_mut().enumerate().take(clusters) {
        let members = assignments
            .iter()
            .enumerate()
            .filter_map(|(index, assigned)| (*assigned == cluster).then_some(index))
            .collect::<Vec<_>>();
        let Some(&first) = members.first() else {
            continue;
        };
        let candidates = evenly_spaced_members(&members, MAX_MEDOID_CANDIDATES);
        let evaluators = evenly_spaced_members(&members, MAX_MEDOID_EVALUATORS);
        let mut best = (first, f64::INFINITY);
        for candidate in candidates {
            let total = evaluators.iter().try_fold(0.0_f64, |sum, member| {
                metric
                    .distance_slices(&vectors[candidate], &vectors[*member])
                    .map(|score| sum + f64::from(score))
                    .map_err(|_| TrainingError::InvalidVector)
            })?;
            if total
                .total_cmp(&best.1)
                .then(candidate.cmp(&best.0))
                .is_lt()
            {
                best = (candidate, total);
            }
        }
        *centroid = vectors[best.0].clone();
    }
    Ok(())
}

fn evenly_spaced_members(members: &[usize], limit: usize) -> Vec<usize> {
    if members.len() <= limit {
        return members.to_vec();
    }
    (0..limit)
        .map(|sample| members[sample.saturating_mul(members.len()) / limit])
        .collect()
}

fn nearest_centroid(vector: &[f32], centroids: &[Vec<f32>]) -> usize {
    centroids
        .iter()
        .enumerate()
        .map(|(index, centroid)| {
            let distance = vector
                .iter()
                .zip(centroid)
                .fold(0.0_f64, |sum, (left, right)| {
                    let difference = f64::from(*left) - f64::from(*right);
                    sum + difference * difference
                });
            (index, distance)
        })
        .min_by(|left, right| left.1.total_cmp(&right.1).then(left.0.cmp(&right.0)))
        .map_or(0, |(index, _)| index)
}

/// Rejected deterministic training input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrainingError {
    /// No vectors were provided.
    EmptyInput,
    /// Vector dimensionality is zero.
    ZeroDimensions,
    /// Cluster count is zero or exceeds vector count.
    InvalidClusterCount,
    /// Iteration budget is zero.
    ZeroIterations,
    /// Vectors are ragged or contain non-finite coordinates.
    InvalidVector,
    /// Training population exceeds the deterministic accumulator bound.
    PopulationTooLarge,
    /// Parallel assignment worker count is zero.
    InvalidWorkerCount,
}

impl Display for TrainingError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::EmptyInput => "k-means input is empty",
            Self::ZeroDimensions => "k-means dimensions must be positive",
            Self::InvalidClusterCount => "k-means cluster count is invalid",
            Self::ZeroIterations => "k-means iteration budget must be positive",
            Self::InvalidVector => "k-means vectors are ragged or non-finite",
            Self::PopulationTooLarge => "k-means population exceeds the supported bound",
            Self::InvalidWorkerCount => "k-means worker count must be positive",
        })
    }
}

impl Error for TrainingError {}
