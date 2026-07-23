//! Typed hierarchical state and bounded-operation seams for pure HNSW.

use std::{
    cmp::Reverse,
    collections::{BTreeSet, BinaryHeap, VecDeque},
    mem::size_of,
};

use context_core::{DenseVector, DistanceMetric, SearchLimit, policy::MAX_VECTOR_DIMENSIONS};

use crate::{
    Candidate, CandidateMask, GraphRead, HnswConfig, HnswError, HnswGraph, HnswGraphNodeSnapshot,
    HnswNode, HnswNodeId, HnswPointId, HnswSearchResult, LayerIndex, MAX_GRAPH_LAYERS,
    MAX_GRAPH_NEIGHBORS_PER_LAYER, Result, sort_candidates, sort_search_results,
};

/// Searches an owned graph adapter without materializing the full graph.
///
/// Each node vector and adjacency layer is fetched only when traversal reaches
/// it. `metric` must use an ascending-distance HNSW score: L2, negative inner
/// product, cosine distance, or L1. Adapter failures remain typed
/// [`HnswError::GraphRead`] failures.
///
/// # Errors
///
/// Returns [`HnswError::UnsupportedMetric`] before reading graph metadata when
/// passed raw inner product. Other failures cover graph data, dimensions,
/// distance evaluation, bounded work, and cancellation.
pub fn search_graph_read(
    graph: &mut impl GraphRead,
    metric: DistanceMetric,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    cancellation: &mut impl HnswCancellation,
) -> Result<HnswSearchOutcome> {
    search_graph_read_impl(graph, metric, query, config, limit, None, cancellation)
}

/// Searches a graph adapter while allowing only results named by `mask`.
///
/// Masked-out nodes remain available as traversal connectors. The adapter
/// reads only nodes reached by bounded HNSW traversal; it does not materialize
/// the persisted graph or scan all mask entries.
///
/// # Errors
///
/// Returns [`HnswError::RecallBudgetExceeded`] when the mask exceeds its shared
/// bound, [`HnswError::UnsupportedMetric`] before traversal for raw inner
/// product, plus the same graph, dimension, and cancellation errors as
/// [`search_graph_read`].
pub fn search_graph_read_with_mask(
    graph: &mut impl GraphRead,
    metric: DistanceMetric,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    mask: &CandidateMask,
    cancellation: &mut impl HnswCancellation,
) -> Result<HnswSearchOutcome> {
    search_graph_read_with_mask_budgeted(
        graph,
        metric,
        query,
        config,
        limit,
        mask,
        context_core::policy::DEFAULT_HNSW_CANDIDATE_MASK_POINTS,
        cancellation,
    )
}

/// Same as [`search_graph_read_with_mask`], but validates `mask` against an
/// explicit, caller-supplied point budget instead of the fixed default.
///
/// The AM masked-scan path uses this to honor
/// `pgcontext.hnsw_mask_candidate_limit`, which can be raised well above the
/// library default without touching this crate's compiled-in policy.
///
/// # Errors
///
/// Same as [`search_graph_read_with_mask`], with `max_mask_points` in place
/// of the default budget.
#[allow(clippy::too_many_arguments)]
pub fn search_graph_read_with_mask_budgeted(
    graph: &mut impl GraphRead,
    metric: DistanceMetric,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    mask: &CandidateMask,
    max_mask_points: usize,
    cancellation: &mut impl HnswCancellation,
) -> Result<HnswSearchOutcome> {
    mask.validate_budget_with_limit(max_mask_points)?;
    search_graph_read_impl(
        graph,
        metric,
        query,
        config,
        limit,
        Some(mask),
        cancellation,
    )
}

fn search_graph_read_impl(
    graph: &mut impl GraphRead,
    metric: DistanceMetric,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    mask: Option<&CandidateMask>,
    cancellation: &mut impl HnswCancellation,
) -> Result<HnswSearchOutcome> {
    ensure_hnsw_metric(metric)?;
    let scorer = HnswScorer { metric, query };
    let metadata = graph.metadata()?;
    let Some(mut current) = metadata.entry_point() else {
        return Ok(HnswSearchOutcome {
            results: Vec::new(),
            work: HnswWork::default(),
        });
    };
    let dimensions = metadata.dimensions().ok_or(HnswError::InvalidSnapshot {
        reason: "nonempty graph is missing dimensions",
    })?;
    if query.dimension() != dimensions {
        return Err(HnswError::DimensionMismatch {
            left: dimensions,
            right: query.dimension(),
        });
    }
    let mut work = HnswWork::default();
    work.check_cancellation(cancellation)?;
    let entry_layer_count = graph_read_node_score(graph, &scorer, current)?.2;
    for layer_index in (1..entry_layer_count).rev() {
        let candidates = search_graph_read_layer(
            graph,
            &scorer,
            current,
            1,
            LayerIndex::new(layer_index),
            &mut work,
            cancellation,
        )?;
        if let Some(best) = candidates.first() {
            current = best.node_id;
        }
    }
    let search_width = config.ef_search().max(limit.get());
    let candidates = if let Some(mask) = mask {
        search_graph_read_layer_filtered(
            graph,
            &scorer,
            current,
            search_width,
            LayerIndex::base(),
            mask,
            &mut work,
            cancellation,
        )?
    } else {
        search_graph_read_layer(
            graph,
            &scorer,
            current,
            search_width,
            LayerIndex::base(),
            &mut work,
            cancellation,
        )?
    };
    let mut results = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let point_id = graph_read_point_id(graph, candidate.node_id)?;
        if mask.is_some_and(|mask| !mask.allows(point_id)) {
            continue;
        }
        results.push(HnswSearchResult {
            point_id,
            score: candidate.score,
        });
    }
    sort_search_results(&mut results);
    results.truncate(limit.get());
    Ok(HnswSearchOutcome { results, work })
}

#[allow(clippy::too_many_arguments)]
fn search_graph_read_layer_filtered(
    graph: &mut impl GraphRead,
    scorer: &HnswScorer<'_>,
    entry: HnswNodeId,
    ef: usize,
    layer: LayerIndex,
    mask: &CandidateMask,
    work: &mut HnswWork,
    cancellation: &mut impl HnswCancellation,
) -> Result<Vec<Candidate>> {
    work.record_distance()?;
    let (entry_score, entry_point_id, _) = graph_read_node_score(graph, scorer, entry)?;
    let entry_candidate = Candidate {
        node_id: entry,
        score: entry_score,
    };
    let entry_allowed = mask.allows(entry_point_id);
    let mut pending = BinaryHeap::from([Reverse(entry_candidate)]);
    let mut nearest = BinaryHeap::new();
    if entry_allowed {
        nearest.push(entry_candidate);
    }
    let metadata = graph.metadata()?;
    let acorn = mask.is_sparse_for(metadata.node_count());
    let mut visited = vec![false; metadata.node_count()];
    let Some(entry_visited) = visited.get_mut(entry.get()) else {
        return Err(HnswError::InvalidSnapshot {
            reason: "entry point exceeds graph node count",
        });
    };
    *entry_visited = true;

    let mut neighbor_scratch = Vec::new();
    let mut second_neighbor_scratch = Vec::new();
    while let Some(Reverse(candidate)) = pending.pop() {
        work.check_cancellation(cancellation)?;
        work.record_expansion()?;
        let worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
        if nearest.len() >= ef && candidate.score > worst {
            break;
        }
        if !graph.read_neighbors_into(candidate.node_id, layer, &mut neighbor_scratch)? {
            return Err(HnswError::InvalidSnapshot {
                reason: "traversal adjacency is missing",
            });
        }
        for neighbor in neighbor_scratch.iter().copied() {
            work.record_edge()?;
            let Some(neighbor_visited) = visited.get_mut(neighbor.get()) else {
                return Err(HnswError::InvalidSnapshot {
                    reason: "neighbor exceeds graph node count",
                });
            };
            if *neighbor_visited {
                continue;
            }
            *neighbor_visited = true;
            work.record_distance()?;
            let (score, point_id, _) = graph_read_node_score(graph, scorer, neighbor)?;
            let scored = Candidate {
                node_id: neighbor,
                score,
            };
            let allowed = mask.allows(point_id);
            if allowed {
                nearest.push(scored);
                if nearest.len() > ef {
                    nearest.pop();
                }
            }
            let current_worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
            if nearest.len() < ef || scored.score <= current_worst {
                pending.push(Reverse(scored));
            } else if acorn && !allowed {
                let second_hop =
                    graph.read_neighbors_into(neighbor, layer, &mut second_neighbor_scratch)?;
                if !second_hop {
                    return Err(HnswError::InvalidSnapshot {
                        reason: "ACORN connector adjacency is missing",
                    });
                }
                for second_neighbor in second_neighbor_scratch.iter().copied() {
                    work.record_edge()?;
                    let Some(second_visited) = visited.get_mut(second_neighbor.get()) else {
                        return Err(HnswError::InvalidSnapshot {
                            reason: "ACORN neighbor exceeds graph node count",
                        });
                    };
                    if *second_visited {
                        continue;
                    }
                    *second_visited = true;
                    work.record_distance()?;
                    let (score, point_id, _) =
                        graph_read_node_score(graph, scorer, second_neighbor)?;
                    let second_scored = Candidate {
                        node_id: second_neighbor,
                        score,
                    };
                    let second_allowed = mask.allows(point_id);
                    if second_allowed {
                        nearest.push(second_scored);
                        if nearest.len() > ef {
                            nearest.pop();
                        }
                    }
                    let second_worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
                    if nearest.len() < ef || second_scored.score <= second_worst {
                        pending.push(Reverse(second_scored));
                    }
                }
            }
        }
    }
    Ok(nearest.into_sorted_vec())
}

fn search_graph_read_layer(
    graph: &mut impl GraphRead,
    scorer: &HnswScorer<'_>,
    entry: HnswNodeId,
    ef: usize,
    layer: LayerIndex,
    work: &mut HnswWork,
    cancellation: &mut impl HnswCancellation,
) -> Result<Vec<Candidate>> {
    work.record_distance()?;
    let entry_candidate = Candidate {
        node_id: entry,
        score: graph_read_node_score(graph, scorer, entry)?.0,
    };
    let mut pending = BinaryHeap::from([Reverse(entry_candidate)]);
    let mut nearest = BinaryHeap::from([entry_candidate]);
    let metadata = graph.metadata()?;
    let mut visited = vec![false; metadata.node_count()];
    let Some(entry_visited) = visited.get_mut(entry.get()) else {
        return Err(HnswError::InvalidSnapshot {
            reason: "entry point exceeds graph node count",
        });
    };
    *entry_visited = true;
    let mut neighbor_scratch = Vec::new();
    while let Some(Reverse(candidate)) = pending.pop() {
        work.check_cancellation(cancellation)?;
        work.record_expansion()?;
        let worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
        if nearest.len() >= ef && candidate.score > worst {
            break;
        }
        if !graph.read_neighbors_into(candidate.node_id, layer, &mut neighbor_scratch)? {
            return Err(HnswError::InvalidSnapshot {
                reason: "traversal adjacency is missing",
            });
        }
        for neighbor in neighbor_scratch.iter().copied() {
            work.record_edge()?;
            let Some(neighbor_visited) = visited.get_mut(neighbor.get()) else {
                return Err(HnswError::InvalidSnapshot {
                    reason: "neighbor exceeds graph node count",
                });
            };
            if *neighbor_visited {
                continue;
            }
            *neighbor_visited = true;
            work.record_distance()?;
            let scored = Candidate {
                node_id: neighbor,
                score: graph_read_node_score(graph, scorer, neighbor)?.0,
            };
            let should_add = nearest.len() < ef
                || nearest
                    .peek()
                    .is_some_and(|current_worst| scored < *current_worst);
            if should_add {
                pending.push(Reverse(scored));
                nearest.push(scored);
                if nearest.len() > ef {
                    nearest.pop();
                }
            }
        }
    }
    Ok(nearest.into_sorted_vec())
}

fn graph_read_node_score(
    graph: &mut impl GraphRead,
    scorer: &HnswScorer<'_>,
    node_id: HnswNodeId,
) -> Result<(f32, HnswPointId, usize)> {
    let scored = graph.with_node(node_id, |node| {
        scorer
            .distance(node.vector())
            .map(|score| (score, node.point_id(), node.layer_count()))
    })?;
    scored.ok_or(HnswError::InvalidSnapshot {
        reason: "traversal node is missing",
    })?
}

fn graph_read_point_id(graph: &mut impl GraphRead, node_id: HnswNodeId) -> Result<HnswPointId> {
    graph
        .with_node(node_id, |node| node.point_id())?
        .ok_or(HnswError::InvalidSnapshot {
            reason: "candidate node is missing",
        })
}

struct HnswScorer<'a> {
    metric: DistanceMetric,
    query: &'a DenseVector,
}

impl HnswScorer<'_> {
    fn distance(&self, vector: &[f32]) -> Result<f32> {
        Ok(self.metric.distance_slices(self.query.as_slice(), vector)?)
    }
}

fn ensure_hnsw_metric(metric: DistanceMetric) -> Result<()> {
    match metric {
        DistanceMetric::L2
        | DistanceMetric::NegativeInnerProduct
        | DistanceMetric::Cosine
        | DistanceMetric::L1
        | DistanceMetric::Hamming
        | DistanceMetric::Jaccard => Ok(()),
        DistanceMetric::InnerProduct => Err(HnswError::UnsupportedMetric {
            metric: "inner_product",
        }),
    }
}

pub(crate) fn hnsw_distance(
    metric: DistanceMetric,
    left: &DenseVector,
    right: &DenseVector,
) -> Result<f32> {
    ensure_hnsw_metric(metric)?;
    Ok(metric.distance(left, right)?)
}

const HNSW_SNAPSHOT_MAGIC: [u8; 4] = *b"HSG1";
const HNSW_SNAPSHOT_VERSION: u16 = 1;

/// Validated maximum layer owned by one HNSW node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HnswLevel(usize);

impl HnswLevel {
    /// Creates a bounded level in `0..MAX_GRAPH_LAYERS`.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::InvalidParameter`] when `value` is outside the
    /// shared layer policy.
    pub const fn new(value: usize) -> Result<Self> {
        if value >= MAX_GRAPH_LAYERS {
            return Err(HnswError::InvalidParameter {
                parameter: "level",
                value,
            });
        }
        Ok(Self(value))
    }

    /// Returns the base level.
    #[must_use]
    pub const fn base() -> Self {
        Self(0)
    }

    /// Returns the maximum layer index.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }

    /// Returns the number of owned layers, including layer zero.
    #[must_use]
    pub const fn layer_count(self) -> usize {
        self.0 + 1
    }
}

/// Seed for platform-independent deterministic geometric level assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HnswLevelSeed(u64);

impl HnswLevelSeed {
    /// Stable default used by [`HnswGraph::new`].
    pub const DEFAULT: Self = Self(0x7067_636f_6e74_6578);

    /// Creates an explicit deterministic seed.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    const fn get(self) -> u64 {
        self.0
    }
}

/// Bounded work counters returned by insertion and search.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HnswWork {
    distance_evaluations: usize,
    node_expansions: usize,
    edges_examined: usize,
    cancellation_checks: usize,
}

impl HnswWork {
    /// Returns distance computations performed.
    #[must_use]
    pub const fn distance_evaluations(self) -> usize {
        self.distance_evaluations
    }

    /// Returns expanded graph nodes.
    #[must_use]
    pub const fn node_expansions(self) -> usize {
        self.node_expansions
    }

    /// Returns examined adjacency entries.
    #[must_use]
    pub const fn edges_examined(self) -> usize {
        self.edges_examined
    }

    /// Returns cancellation checkpoints reached.
    #[must_use]
    pub const fn cancellation_checks(self) -> usize {
        self.cancellation_checks
    }

    fn record_distance(&mut self) -> Result<()> {
        self.distance_evaluations = checked_work_increment(self.distance_evaluations)?;
        Ok(())
    }

    fn record_expansion(&mut self) -> Result<()> {
        self.node_expansions = checked_work_increment(self.node_expansions)?;
        Ok(())
    }

    fn record_edge(&mut self) -> Result<()> {
        self.edges_examined = checked_work_increment(self.edges_examined)?;
        Ok(())
    }

    fn check_cancellation(&mut self, cancellation: &mut impl HnswCancellation) -> Result<()> {
        self.cancellation_checks = checked_work_increment(self.cancellation_checks)?;
        cancellation.check()
    }
}

/// Read-only insertion plan produced by [`HnswGraph::plan_insertion`] and
/// consumed by [`HnswGraph::commit_insertion`].
///
/// Splitting search (`&self`, safe to run concurrently) from mutation
/// (`&mut self`, exclusive) is what lets [`crate::ConcurrentHnswBuilder`]
/// parallelize the expensive candidate-search portion of HNSW insertion
/// while keeping every graph mutation serialized.
#[derive(Debug, Clone)]
pub(crate) enum InsertionPlan {
    /// The graph had no entry point when this was planned.
    FirstNode,
    /// Per-layer neighbor selections computed against the graph's state at
    /// plan time.
    Candidates { layers: Vec<Vec<HnswNodeId>> },
}

/// Result of one explicit or assigned-level insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswInsertOutcome {
    node_id: HnswNodeId,
    work: HnswWork,
}

impl HnswInsertOutcome {
    /// Returns the inserted internal node id.
    #[must_use]
    pub const fn node_id(self) -> HnswNodeId {
        self.node_id
    }

    /// Returns bounded construction work.
    #[must_use]
    pub const fn work(self) -> HnswWork {
        self.work
    }
}

/// Result and work evidence from a controlled search.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswSearchOutcome {
    results: Vec<HnswSearchResult>,
    work: HnswWork,
}

impl HnswSearchOutcome {
    /// Returns ordered search results.
    #[must_use]
    pub fn results(&self) -> &[HnswSearchResult] {
        &self.results
    }

    /// Returns bounded search work.
    #[must_use]
    pub const fn work(&self) -> HnswWork {
        self.work
    }
}

/// Caller-owned cancellation checkpoints for pure graph operations.
pub trait HnswCancellation {
    /// Returns an error when graph work should stop.
    fn check(&mut self) -> Result<()>;
}

/// Cancellation policy that always permits work.
#[derive(Debug, Default, Clone, Copy)]
pub struct NeverCancel;

impl HnswCancellation for NeverCancel {
    fn check(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Full hierarchy snapshot; the PostgreSQL base-only codec remains separate.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswGraphSnapshot {
    level_seed: HnswLevelSeed,
    entry_point: Option<HnswNodeId>,
    nodes: Vec<HnswGraphNodeSnapshot>,
}

impl HnswGraphSnapshot {
    /// Encodes the pure hierarchy DTO independently from PostgreSQL page layout.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::InvalidSnapshot`] when a platform-sized count
    /// cannot be represented by the portable format.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut output = Vec::new();
        output.extend_from_slice(&HNSW_SNAPSHOT_MAGIC);
        output.extend_from_slice(&HNSW_SNAPSHOT_VERSION.to_le_bytes());
        output.extend_from_slice(&0_u16.to_le_bytes());
        output.extend_from_slice(&self.level_seed.get().to_le_bytes());
        let entry = match self.entry_point {
            Some(node) => usize_to_u64(node.get())?,
            None => u64::MAX,
        };
        output.extend_from_slice(&entry.to_le_bytes());
        output.extend_from_slice(&usize_to_u64(self.nodes.len())?.to_le_bytes());
        for node in &self.nodes {
            output.extend_from_slice(&usize_to_u64(node.node_id.get())?.to_le_bytes());
            output.extend_from_slice(&node.point_id.get().to_le_bytes());
            output.extend_from_slice(&usize_to_u32(node.vector.dimension())?.to_le_bytes());
            output.extend_from_slice(&usize_to_u16(node.layers.len())?.to_le_bytes());
            output.extend_from_slice(&0_u16.to_le_bytes());
            for value in node.vector.as_slice() {
                output.extend_from_slice(&value.to_bits().to_le_bytes());
            }
            for neighbors in &node.layers {
                output.extend_from_slice(&usize_to_u16(neighbors.len())?.to_le_bytes());
                for neighbor in neighbors {
                    output.extend_from_slice(&usize_to_u64(neighbor.get())?.to_le_bytes());
                }
            }
        }
        Ok(output)
    }

    /// Decodes bounded pure hierarchy state; graph-relative checks run on restore.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::InvalidSnapshot`] for truncated, unsupported,
    /// over-policy, non-finite, or trailing input.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut cursor = SnapshotCursor::new(bytes);
        if cursor.read_array::<4>()? != HNSW_SNAPSHOT_MAGIC {
            return Err(snapshot_error("magic mismatch"));
        }
        if cursor.read_u16()? != HNSW_SNAPSHOT_VERSION {
            return Err(snapshot_error("unsupported version"));
        }
        if cursor.read_u16()? != 0 {
            return Err(snapshot_error("reserved header bits are nonzero"));
        }
        let level_seed = HnswLevelSeed::new(cursor.read_u64()?);
        let entry_raw = cursor.read_u64()?;
        let node_count = u64_to_usize(cursor.read_u64()?)?;
        if node_count > cursor.remaining() / 24 {
            return Err(snapshot_error("node count exceeds payload"));
        }
        let entry_point = if entry_raw == u64::MAX {
            None
        } else {
            Some(HnswNodeId::new(u64_to_usize(entry_raw)?))
        };
        let mut nodes = Vec::with_capacity(node_count);
        for _ in 0..node_count {
            let node_id = HnswNodeId::new(u64_to_usize(cursor.read_u64()?)?);
            let point_id = HnswPointId::new(cursor.read_u64()?);
            let dimensions = usize::try_from(cursor.read_u32()?)
                .map_err(|_| snapshot_error("dimension count exceeds usize"))?;
            if dimensions == 0 || dimensions > MAX_VECTOR_DIMENSIONS {
                return Err(snapshot_error("dimension count is outside policy"));
            }
            let layer_count = usize::from(cursor.read_u16()?);
            if cursor.read_u16()? != 0 {
                return Err(snapshot_error("reserved node bits are nonzero"));
            }
            if layer_count == 0 || layer_count > MAX_GRAPH_LAYERS {
                return Err(snapshot_error("layer count is outside bounds"));
            }
            let vector_bytes = dimensions
                .checked_mul(size_of::<f32>())
                .ok_or_else(|| snapshot_error("vector byte count overflow"))?;
            if vector_bytes > cursor.remaining() {
                return Err(snapshot_error("vector payload is truncated"));
            }
            let mut values = Vec::with_capacity(dimensions);
            for _ in 0..dimensions {
                values.push(f32::from_bits(cursor.read_u32()?));
            }
            let vector = DenseVector::new(values)
                .map_err(|_| snapshot_error("vector payload is invalid"))?;
            let mut layers = Vec::with_capacity(layer_count);
            for _ in 0..layer_count {
                let neighbor_count = usize::from(cursor.read_u16()?);
                if neighbor_count > MAX_GRAPH_NEIGHBORS_PER_LAYER
                    || neighbor_count > cursor.remaining() / size_of::<u64>()
                {
                    return Err(snapshot_error("neighbor count exceeds payload or policy"));
                }
                let mut neighbors = Vec::with_capacity(neighbor_count);
                for _ in 0..neighbor_count {
                    neighbors.push(HnswNodeId::new(u64_to_usize(cursor.read_u64()?)?));
                }
                layers.push(neighbors);
            }
            nodes.push(HnswGraphNodeSnapshot {
                node_id,
                point_id,
                vector,
                layers,
            });
        }
        if cursor.remaining() != 0 {
            return Err(snapshot_error("trailing bytes"));
        }
        Ok(Self {
            level_seed,
            entry_point,
            nodes,
        })
    }
}

include!("hnsw_hierarchy/graph_impl.rs");
include!("hnsw_hierarchy/concurrent_build.rs");
struct SnapshotCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> SnapshotCursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn read_array<const SIZE: usize>(&mut self) -> Result<[u8; SIZE]> {
        let end = self
            .offset
            .checked_add(SIZE)
            .ok_or_else(|| snapshot_error("cursor overflow"))?;
        let Some(source) = self.bytes.get(self.offset..end) else {
            return Err(snapshot_error("payload is truncated"));
        };
        let mut output = [0_u8; SIZE];
        output.copy_from_slice(source);
        self.offset = end;
        Ok(output)
    }

    fn read_u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }
}

const fn snapshot_error(reason: &'static str) -> HnswError {
    HnswError::InvalidSnapshot { reason }
}

fn usize_to_u16(value: usize) -> Result<u16> {
    u16::try_from(value).map_err(|_| snapshot_error("value exceeds u16"))
}

fn usize_to_u32(value: usize) -> Result<u32> {
    u32::try_from(value).map_err(|_| snapshot_error("value exceeds u32"))
}

fn usize_to_u64(value: usize) -> Result<u64> {
    u64::try_from(value).map_err(|_| snapshot_error("value exceeds u64"))
}

fn u64_to_usize(value: u64) -> Result<usize> {
    usize::try_from(value).map_err(|_| snapshot_error("value exceeds usize"))
}

fn checked_work_increment(value: usize) -> Result<usize> {
    value.checked_add(1).ok_or(HnswError::InvalidParameter {
        parameter: "work_counter",
        value,
    })
}

const fn deterministic_level(seed: HnswLevelSeed, node_id: HnswNodeId, m: usize) -> HnswLevel {
    let mut value = splitmix64(seed.get() ^ node_id.get() as u64);
    let divisor = if m < 2 { 2 } else { m as u64 };
    let mut level = 0;
    while level + 1 < MAX_GRAPH_LAYERS && value.is_multiple_of(divisor) {
        level += 1;
        value /= divisor;
    }
    HnswLevel(level)
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

impl HnswGraphNodeSnapshot {
    /// Returns the maximum level present in this full snapshot node.
    #[must_use]
    pub fn level(&self) -> HnswLevel {
        HnswLevel(self.layers.len().saturating_sub(1))
    }

    /// Returns all stored hierarchy layers.
    #[must_use]
    pub fn layers(&self) -> &[Vec<HnswNodeId>] {
        &self.layers
    }

    /// Returns neighbors on one stored layer.
    #[must_use]
    pub fn neighbors(&self, layer: LayerIndex) -> Option<&[HnswNodeId]> {
        self.layers.get(layer.get()).map(Vec::as_slice)
    }
}
