//! Statement-local resumable traversal for graph-read HNSW adapters.

use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    mem::{size_of, take},
};

use context_core::{DenseVector, DistanceMetric, LazyCursorTermination, SearchLimit};

use crate::{
    Candidate, CandidateMask, GraphRead, HnswConfig, HnswError, HnswNodeId, HnswPointId,
    HnswSearchResult, LayerIndex, MAX_GRAPH_NEIGHBORS_PER_LAYER, Result, sort_search_results,
};

use super::{
    HnswCancellation, HnswComparisonBudget, HnswSearchOutcome, HnswWork, ensure_hnsw_metric,
};

/// Maximum frontier pops admitted by one cursor advance.
pub const MAX_HNSW_CURSOR_ADVANCE: usize = context_core::MAX_LAZY_CURSOR_BATCH;

/// Hard non-comparison resources for one statement-local HNSW cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HnswCursorBudget {
    max_node_expansions: usize,
    max_edges: usize,
    max_memory_bytes: usize,
}

impl HnswCursorBudget {
    /// Creates explicit cumulative cursor limits.
    #[must_use]
    pub const fn new(
        max_node_expansions: usize,
        max_edges: usize,
        max_memory_bytes: usize,
    ) -> Self {
        Self {
            max_node_expansions,
            max_edges,
            max_memory_bytes,
        }
    }

    /// Creates an effectively unbounded cursor policy for eager compatibility.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self::new(usize::MAX, usize::MAX, usize::MAX)
    }

    /// Returns the cumulative node-pop ceiling.
    #[must_use]
    pub const fn max_node_expansions(self) -> usize {
        self.max_node_expansions
    }

    /// Returns the cumulative adjacency-entry ceiling.
    #[must_use]
    pub const fn max_edges(self) -> usize {
        self.max_edges
    }

    /// Returns the extension-owned allocation ceiling.
    #[must_use]
    pub const fn max_memory_bytes(self) -> usize {
        self.max_memory_bytes
    }
}

/// Index-facing name for the shared lazy-cursor terminal vocabulary.
pub type HnswCursorTermination = LazyCursorTermination;

/// One deterministic frontier item popped by traversal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HnswCursorFrontier {
    node_id: HnswNodeId,
    score: f32,
}

impl HnswCursorFrontier {
    /// Returns the internal node identity.
    #[must_use]
    pub const fn node_id(self) -> HnswNodeId {
        self.node_id
    }

    /// Returns the ascending-distance traversal score.
    #[must_use]
    pub const fn score(self) -> f32 {
        self.score
    }
}

impl From<Candidate> for HnswCursorFrontier {
    fn from(candidate: Candidate) -> Self {
        Self {
            node_id: candidate.node_id,
            score: candidate.score,
        }
    }
}

/// Result of one bounded cursor advance.
#[derive(Debug, PartialEq)]
pub struct HnswCursorAdvanceOutcome {
    frontiers: Vec<HnswCursorFrontier>,
    work: HnswWork,
    termination: Option<HnswCursorTermination>,
}

impl HnswCursorAdvanceOutcome {
    /// Returns frontier items popped by this call in deterministic order.
    #[must_use]
    pub fn frontiers(&self) -> &[HnswCursorFrontier] {
        &self.frontiers
    }

    /// Returns cumulative work after this advance.
    #[must_use]
    pub const fn work(&self) -> HnswWork {
        self.work
    }

    /// Returns the terminal reason, if traversal stopped.
    #[must_use]
    pub const fn termination(&self) -> Option<HnswCursorTermination> {
        self.termination
    }
}

struct LayerCursor {
    layer: LayerIndex,
    ef: usize,
    pending: BinaryHeap<Reverse<Candidate>>,
    nearest: BinaryHeap<Candidate>,
    visited: Vec<bool>,
    neighbor_scratch: Vec<HnswNodeId>,
    second_neighbor_scratch: Vec<HnswNodeId>,
    acorn: bool,
    exhausted: bool,
}

impl LayerCursor {
    #[allow(
        clippy::too_many_arguments,
        reason = "cursor seeding keeps graph, topology, filter, work, and scoring budgets explicit"
    )]
    fn seed(
        graph: &mut impl GraphRead,
        scorer: &HnswScorer<'_>,
        entry: HnswNodeId,
        ef: usize,
        layer: LayerIndex,
        mask: Option<&CandidateMask>,
        work: &mut HnswWork,
        comparison_budget: &HnswComparisonBudget,
        node_count: usize,
    ) -> Result<Self> {
        work.record_distance_budgeted(comparison_budget)?;
        let (entry_score, entry_point_id, _) = graph_read_node_score(graph, scorer, entry)?;
        let entry_candidate = Candidate {
            node_id: entry,
            score: entry_score,
        };
        let mut nearest = BinaryHeap::new();
        if mask.is_none_or(|mask| mask.allows(entry_point_id)) {
            nearest.push(entry_candidate);
        }
        let mut visited = vec![false; node_count];
        let Some(entry_visited) = visited.get_mut(entry.get()) else {
            return Err(HnswError::InvalidSnapshot {
                reason: "entry point exceeds graph node count",
            });
        };
        *entry_visited = true;
        Ok(Self {
            layer,
            ef,
            pending: BinaryHeap::from([Reverse(entry_candidate)]),
            nearest,
            visited,
            neighbor_scratch: Vec::new(),
            second_neighbor_scratch: Vec::new(),
            acorn: mask.is_some_and(|mask| mask.is_sparse_for(node_count)),
            exhausted: false,
        })
    }

    fn peek(&self) -> Option<Candidate> {
        self.pending.peek().map(|candidate| candidate.0)
    }

    fn complete(&self) -> bool {
        self.exhausted || self.pending.is_empty()
    }

    fn best_node(&self) -> Option<HnswNodeId> {
        self.nearest.iter().min().map(|candidate| candidate.node_id)
    }

    fn into_candidates(self) -> Vec<Candidate> {
        self.nearest.into_sorted_vec()
    }
}

#[derive(Default)]
enum CursorPhase {
    #[default]
    Empty,
    Layer(LayerCursor),
    Done,
}

struct HnswScorer<'a> {
    metric: DistanceMetric,
    query: &'a DenseVector,
}

/// Statement-local lazy traversal over one graph-read adapter.
///
/// The cursor borrows its graph, query, budgets, and cancellation hook. It is
/// intentionally neither cloneable nor serializable, so snapshots cannot be
/// replayed after source or index state changes.
pub struct HnswLazyCursor<'a, G: GraphRead, C: HnswCancellation> {
    graph: &'a mut G,
    scorer: HnswScorer<'a>,
    mask: Option<&'a CandidateMask>,
    comparison_budget: &'a HnswComparisonBudget,
    cancellation: &'a mut C,
    budget: HnswCursorBudget,
    search_width: usize,
    limit: SearchLimit,
    node_count: usize,
    phase: CursorPhase,
    completed_candidates: Option<Vec<Candidate>>,
    work: HnswWork,
    retained_bytes: usize,
    termination: Option<HnswCursorTermination>,
}

impl<G: GraphRead, C: HnswCancellation> HnswLazyCursor<'_, G, C> {
    /// Returns the next frontier item without consuming it.
    #[must_use]
    pub fn peek(&self) -> Option<HnswCursorFrontier> {
        match &self.phase {
            CursorPhase::Layer(layer) => layer.peek().map(Into::into),
            CursorPhase::Empty | CursorPhase::Done => None,
        }
    }

    /// Pops and expands at most one frontier item.
    ///
    /// # Errors
    ///
    /// Returns typed validation or graph-adapter failures. Cancellation and
    /// comparison exhaustion are returned as their existing typed errors and
    /// also recorded in [`Self::termination`].
    pub fn pop(&mut self) -> Result<Option<HnswCursorFrontier>> {
        self.advance(1)
            .map(|outcome| outcome.frontiers.first().copied())
    }

    /// Advances by at most `batch_size` frontier expansions.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::InvalidParameter`] outside `1..=256`, plus typed
    /// graph, cancellation, and comparison-budget errors.
    pub fn advance(&mut self, batch_size: usize) -> Result<HnswCursorAdvanceOutcome> {
        if !(1..=MAX_HNSW_CURSOR_ADVANCE).contains(&batch_size) {
            return Err(HnswError::InvalidParameter {
                parameter: "cursor_batch",
                value: batch_size,
            });
        }
        if self.termination.is_some() {
            return Ok(self.advance_outcome(Vec::new()));
        }
        self.admit_advance_output(batch_size)?;
        let mut frontiers = Vec::with_capacity(batch_size);
        while frontiers.len() < batch_size && self.termination.is_none() {
            self.transition_or_terminate()?;
            let CursorPhase::Layer(layer) = &mut self.phase else {
                break;
            };
            match advance_layer_once(
                layer,
                self.graph,
                &self.scorer,
                self.mask.filter(|_| layer.layer == LayerIndex::base()),
                &mut self.work,
                self.comparison_budget,
                self.cancellation,
                self.budget,
            ) {
                Ok(Some(candidate)) => frontiers.push(candidate.into()),
                Ok(None) => {}
                Err(HnswError::CursorIncomplete { reason }) => {
                    self.termination = Some(reason);
                    break;
                }
                Err(error) => {
                    self.termination = Some(termination_for_error(&error));
                    return Err(error);
                }
            }
        }
        if self.termination.is_none() {
            self.transition_or_terminate()?;
        }
        Ok(self.advance_outcome(frontiers))
    }

    /// Reports complete graph exhaustion, distinct from budget termination.
    #[must_use]
    pub const fn exhausted(&self) -> bool {
        matches!(self.termination, Some(HnswCursorTermination::Exhausted))
    }

    /// Returns cumulative traversal work.
    #[must_use]
    pub const fn work(&self) -> HnswWork {
        self.work
    }

    /// Returns the terminal reason once traversal cannot continue.
    #[must_use]
    pub const fn termination(&self) -> Option<HnswCursorTermination> {
        self.termination
    }

    /// Returns conservatively projected cursor-owned retained bytes.
    #[must_use]
    pub const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Drains traversal and materializes the canonical eager result.
    ///
    /// # Errors
    ///
    /// Returns the first graph, cancellation, comparison, or cursor resource
    /// failure. Partial candidates are never returned as complete results.
    pub fn finish(mut self) -> Result<HnswSearchOutcome> {
        while self.termination.is_none() {
            self.advance(MAX_HNSW_CURSOR_ADVANCE)?;
        }
        if self.termination != Some(HnswCursorTermination::Exhausted) {
            return Err(HnswError::CursorIncomplete {
                reason: self
                    .termination
                    .unwrap_or(HnswCursorTermination::AdapterError),
            });
        }
        let candidates = self.completed_candidates.take().unwrap_or_default();
        let mut results = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            self.comparison_budget.reserve_comparison()?;
            let point_id = graph_read_point_id(self.graph, candidate.node_id)?;
            if self.mask.is_some_and(|mask| !mask.allows(point_id)) {
                continue;
            }
            results.push(HnswSearchResult {
                point_id,
                score: candidate.score,
            });
        }
        sort_search_results(&mut results);
        results.truncate(self.limit.get());
        Ok(HnswSearchOutcome {
            results,
            work: self.work,
        })
    }

    fn transition_completed_layer(&mut self) -> Result<()> {
        loop {
            let complete = matches!(&self.phase, CursorPhase::Layer(layer) if layer.complete());
            if !complete {
                return Ok(());
            }
            let phase = take(&mut self.phase);
            let CursorPhase::Layer(layer) = phase else {
                return Ok(());
            };
            if layer.layer == LayerIndex::base() {
                self.completed_candidates = Some(layer.into_candidates());
                self.phase = CursorPhase::Done;
                self.termination = Some(HnswCursorTermination::Exhausted);
                return Ok(());
            }
            let current = layer.best_node().ok_or(HnswError::InvalidSnapshot {
                reason: "upper-layer traversal produced no candidate",
            })?;
            let next_layer = LayerIndex::new(layer.layer.get() - 1);
            let ef = if next_layer == LayerIndex::base() {
                self.search_width
            } else {
                1
            };
            let mask = self.mask.filter(|_| next_layer == LayerIndex::base());
            drop(layer);
            self.phase = CursorPhase::Layer(LayerCursor::seed(
                self.graph,
                &self.scorer,
                current,
                ef,
                next_layer,
                mask,
                &mut self.work,
                self.comparison_budget,
                self.node_count,
            )?);
        }
    }

    fn transition_or_terminate(&mut self) -> Result<()> {
        match self.transition_completed_layer() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.termination = Some(termination_for_error(&error));
                Err(error)
            }
        }
    }

    fn admit_advance_output(&mut self, batch_size: usize) -> Result<()> {
        let output_bytes = exact_allocation_bytes::<HnswCursorFrontier>(batch_size)?;
        let required = self.retained_bytes.checked_add(output_bytes).ok_or(
            HnswError::CursorMemoryBudgetExceeded {
                maximum: self.budget.max_memory_bytes,
                required: usize::MAX,
            },
        )?;
        if required > self.budget.max_memory_bytes {
            self.termination = Some(HnswCursorTermination::MemoryBudget);
            return Err(HnswError::CursorMemoryBudgetExceeded {
                maximum: self.budget.max_memory_bytes,
                required,
            });
        }
        Ok(())
    }

    fn advance_outcome(&self, frontiers: Vec<HnswCursorFrontier>) -> HnswCursorAdvanceOutcome {
        HnswCursorAdvanceOutcome {
            frontiers,
            work: self.work,
            termination: self.termination,
        }
    }
}

/// Seeds an unmasked statement-local graph-read cursor.
///
/// # Errors
///
/// Returns typed metric, dimension, graph, cancellation, comparison, or
/// cursor-memory failures before returning an invalid cursor.
#[allow(clippy::too_many_arguments)]
pub fn seed_graph_read_cursor<'a, G: GraphRead, C: HnswCancellation>(
    graph: &'a mut G,
    metric: DistanceMetric,
    query: &'a DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    comparison_budget: &'a HnswComparisonBudget,
    cursor_budget: HnswCursorBudget,
    cancellation: &'a mut C,
) -> Result<HnswLazyCursor<'a, G, C>> {
    seed_graph_read_cursor_impl(
        graph,
        metric,
        query,
        config,
        limit,
        None,
        comparison_budget,
        cursor_budget,
        cancellation,
    )
}

/// Seeds a masked statement-local graph-read cursor.
///
/// Masked nodes remain traversal connectors but cannot enter the result set.
///
/// # Errors
///
/// Returns [`HnswError::RecallBudgetExceeded`] when the mask is oversized,
/// plus the failures from [`seed_graph_read_cursor`].
#[allow(clippy::too_many_arguments)]
pub fn seed_graph_read_cursor_with_mask<'a, G: GraphRead, C: HnswCancellation>(
    graph: &'a mut G,
    metric: DistanceMetric,
    query: &'a DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    mask: &'a CandidateMask,
    max_mask_points: usize,
    comparison_budget: &'a HnswComparisonBudget,
    cursor_budget: HnswCursorBudget,
    cancellation: &'a mut C,
) -> Result<HnswLazyCursor<'a, G, C>> {
    mask.validate_budget_with_limit(max_mask_points)?;
    seed_graph_read_cursor_impl(
        graph,
        metric,
        query,
        config,
        limit,
        Some(mask),
        comparison_budget,
        cursor_budget,
        cancellation,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn seed_graph_read_cursor_impl<'a, G: GraphRead, C: HnswCancellation>(
    graph: &'a mut G,
    metric: DistanceMetric,
    query: &'a DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    mask: Option<&'a CandidateMask>,
    comparison_budget: &'a HnswComparisonBudget,
    cursor_budget: HnswCursorBudget,
    cancellation: &'a mut C,
) -> Result<HnswLazyCursor<'a, G, C>> {
    ensure_hnsw_metric(metric)?;
    let mut work = HnswWork::default();
    cancellation.check()?;
    let metadata = graph.metadata()?;
    let scorer = HnswScorer { metric, query };
    let search_width = config.ef_search().max(limit.get());
    let Some(entry) = metadata.entry_point() else {
        return Ok(HnswLazyCursor {
            graph,
            scorer,
            mask,
            comparison_budget,
            cancellation,
            budget: cursor_budget,
            search_width,
            limit,
            node_count: 0,
            phase: CursorPhase::Done,
            completed_candidates: Some(Vec::new()),
            work: HnswWork::default(),
            retained_bytes: 0,
            termination: Some(HnswCursorTermination::Exhausted),
        });
    };
    let retained_bytes =
        projected_hnsw_cursor_retained_bytes(metadata.node_count(), search_width, mask.is_some())?;
    if retained_bytes > cursor_budget.max_memory_bytes {
        return Err(HnswError::CursorMemoryBudgetExceeded {
            maximum: cursor_budget.max_memory_bytes,
            required: retained_bytes,
        });
    }
    let dimensions = metadata.dimensions().ok_or(HnswError::InvalidSnapshot {
        reason: "nonempty graph is missing dimensions",
    })?;
    if query.dimension() != dimensions {
        return Err(HnswError::DimensionMismatch {
            left: dimensions,
            right: query.dimension(),
        });
    }
    graph.prepare_query(metric, query)?;
    work.check_cancellation(cancellation)?;
    work.record_distance_budgeted(comparison_budget)?;
    let entry_layer_count = graph_read_node_score(graph, &scorer, entry)?.2;
    if entry_layer_count == 0 {
        return Err(HnswError::InvalidSnapshot {
            reason: "entry point has no base layer",
        });
    }
    let mut current = entry;
    for layer_index in (1..entry_layer_count).rev() {
        let mut upper = LayerCursor::seed(
            graph,
            &scorer,
            current,
            1,
            LayerIndex::new(layer_index),
            None,
            &mut work,
            comparison_budget,
            metadata.node_count(),
        )?;
        while !upper.complete() {
            advance_layer_once(
                &mut upper,
                graph,
                &scorer,
                None,
                &mut work,
                comparison_budget,
                cancellation,
                cursor_budget,
            )?;
        }
        current = upper.best_node().ok_or(HnswError::InvalidSnapshot {
            reason: "upper-layer traversal produced no candidate",
        })?;
    }
    let phase = CursorPhase::Layer(LayerCursor::seed(
        graph,
        &scorer,
        current,
        search_width,
        LayerIndex::base(),
        mask,
        &mut work,
        comparison_budget,
        metadata.node_count(),
    )?);
    Ok(HnswLazyCursor {
        graph,
        scorer,
        mask,
        comparison_budget,
        cancellation,
        budget: cursor_budget,
        search_width,
        limit,
        node_count: metadata.node_count(),
        phase,
        completed_candidates: None,
        work,
        retained_bytes,
        termination: None,
    })
}

#[allow(clippy::too_many_arguments)]
fn advance_layer_once(
    layer: &mut LayerCursor,
    graph: &mut impl GraphRead,
    scorer: &HnswScorer<'_>,
    mask: Option<&CandidateMask>,
    work: &mut HnswWork,
    comparison_budget: &HnswComparisonBudget,
    cancellation: &mut impl HnswCancellation,
    budget: HnswCursorBudget,
) -> Result<Option<Candidate>> {
    if work.node_expansions() >= budget.max_node_expansions {
        return Err(HnswError::CursorIncomplete {
            reason: HnswCursorTermination::ExpansionBudget,
        });
    }
    work.check_cancellation(cancellation)?;
    let Some(Reverse(candidate)) = layer.pending.pop() else {
        layer.exhausted = true;
        return Ok(None);
    };
    work.record_expansion()?;
    let worst = layer
        .nearest
        .peek()
        .map_or(f32::INFINITY, |item| item.score);
    if layer.nearest.len() >= layer.ef && candidate.score > worst {
        layer.exhausted = true;
        return Ok(Some(candidate));
    }
    if !graph.read_neighbors_into(candidate.node_id, layer.layer, &mut layer.neighbor_scratch)? {
        return Err(HnswError::InvalidSnapshot {
            reason: "traversal adjacency is missing",
        });
    }
    for neighbor_index in 0..layer.neighbor_scratch.len() {
        let neighbor = layer.neighbor_scratch[neighbor_index];
        admit_edge(work, budget)?;
        let Some(neighbor_visited) = layer.visited.get_mut(neighbor.get()) else {
            return Err(HnswError::InvalidSnapshot {
                reason: "neighbor exceeds graph node count",
            });
        };
        if *neighbor_visited {
            continue;
        }
        *neighbor_visited = true;
        work.record_distance_budgeted(comparison_budget)?;
        let (score, point_id, _) = graph_read_node_score(graph, scorer, neighbor)?;
        let scored = Candidate {
            node_id: neighbor,
            score,
        };
        if mask.is_none() {
            let should_add = layer.nearest.len() < layer.ef
                || layer
                    .nearest
                    .peek()
                    .is_some_and(|current_worst| scored < *current_worst);
            if should_add {
                layer.pending.push(Reverse(scored));
                if layer.nearest.len() == layer.ef {
                    layer.nearest.pop();
                }
                layer.nearest.push(scored);
            }
            continue;
        }
        let allowed = mask.is_some_and(|mask| mask.allows(point_id));
        let should_add = allowed
            && (layer.nearest.len() < layer.ef
                || layer
                    .nearest
                    .peek()
                    .is_some_and(|current_worst| scored < *current_worst));
        if should_add {
            if layer.nearest.len() == layer.ef {
                layer.nearest.pop();
            }
            layer.nearest.push(scored);
        }
        let current_worst = layer
            .nearest
            .peek()
            .map_or(f32::INFINITY, |item| item.score);
        if layer.nearest.len() < layer.ef || scored.score <= current_worst {
            layer.pending.push(Reverse(scored));
        } else if layer.acorn && !allowed {
            expand_second_hop(
                layer,
                graph,
                scorer,
                mask,
                work,
                comparison_budget,
                budget,
                neighbor,
            )?;
        }
    }
    Ok(Some(candidate))
}

#[allow(clippy::too_many_arguments)]
fn expand_second_hop(
    layer: &mut LayerCursor,
    graph: &mut impl GraphRead,
    scorer: &HnswScorer<'_>,
    mask: Option<&CandidateMask>,
    work: &mut HnswWork,
    comparison_budget: &HnswComparisonBudget,
    budget: HnswCursorBudget,
    connector: HnswNodeId,
) -> Result<()> {
    if !graph.read_neighbors_into(connector, layer.layer, &mut layer.second_neighbor_scratch)? {
        return Err(HnswError::InvalidSnapshot {
            reason: "ACORN connector adjacency is missing",
        });
    }
    for second_neighbor in layer.second_neighbor_scratch.iter().copied() {
        admit_edge(work, budget)?;
        let Some(second_visited) = layer.visited.get_mut(second_neighbor.get()) else {
            return Err(HnswError::InvalidSnapshot {
                reason: "ACORN neighbor exceeds graph node count",
            });
        };
        if *second_visited {
            continue;
        }
        *second_visited = true;
        work.record_distance_budgeted(comparison_budget)?;
        let (score, point_id, _) = graph_read_node_score(graph, scorer, second_neighbor)?;
        let scored = Candidate {
            node_id: second_neighbor,
            score,
        };
        let allowed = mask.is_none_or(|mask| mask.allows(point_id));
        let should_add = allowed
            && (layer.nearest.len() < layer.ef
                || layer
                    .nearest
                    .peek()
                    .is_some_and(|current_worst| scored < *current_worst));
        if should_add {
            if layer.nearest.len() == layer.ef {
                layer.nearest.pop();
            }
            layer.nearest.push(scored);
        }
        let worst = layer
            .nearest
            .peek()
            .map_or(f32::INFINITY, |item| item.score);
        if layer.nearest.len() < layer.ef || scored.score <= worst {
            layer.pending.push(Reverse(scored));
        }
    }
    Ok(())
}

fn admit_edge(work: &mut HnswWork, budget: HnswCursorBudget) -> Result<()> {
    if work.edges_examined() >= budget.max_edges {
        return Err(HnswError::CursorIncomplete {
            reason: HnswCursorTermination::EdgeBudget,
        });
    }
    work.record_edge()
}

fn graph_read_node_score(
    graph: &mut impl GraphRead,
    scorer: &HnswScorer<'_>,
    node_id: HnswNodeId,
) -> Result<(f32, HnswPointId, usize)> {
    let scored = graph.score_node(node_id, scorer.metric, scorer.query)?;
    scored
        .map(|scored| (scored.score(), scored.point_id(), scored.layer_count()))
        .ok_or(HnswError::InvalidSnapshot {
            reason: "traversal node is missing",
        })
}

fn graph_read_point_id(graph: &mut impl GraphRead, node_id: HnswNodeId) -> Result<HnswPointId> {
    graph
        .with_node(node_id, |node| node.point_id())?
        .ok_or(HnswError::InvalidSnapshot {
            reason: "candidate node is missing",
        })
}

/// Conservatively projects state retained by one lazy graph-read cursor.
///
/// # Errors
///
/// Returns [`HnswError::CursorMemoryBudgetExceeded`] when platform-sized
/// allocation arithmetic overflows.
pub fn projected_hnsw_cursor_retained_bytes(
    node_count: usize,
    ef: usize,
    filtered: bool,
) -> Result<usize> {
    let visited = exact_allocation_bytes::<bool>(node_count)?;
    let pending = growing_allocation_bytes::<Reverse<Candidate>>(node_count)?;
    let nearest = growing_allocation_bytes::<Candidate>(ef)?;
    let results = exact_allocation_bytes::<HnswSearchResult>(ef)?;
    let neighbors = growing_allocation_bytes::<HnswNodeId>(MAX_GRAPH_NEIGHBORS_PER_LAYER)?;
    let second_neighbors = if filtered { neighbors } else { 0 };
    [
        size_of::<LayerCursor>(),
        visited,
        pending,
        nearest,
        results,
        neighbors,
        second_neighbors,
    ]
    .into_iter()
    .try_fold(0_usize, |total, bytes| total.checked_add(bytes))
    .ok_or(HnswError::CursorMemoryBudgetExceeded {
        maximum: usize::MAX,
        required: usize::MAX,
    })
}

/// Projects the equivalent pre-cursor eager traversal allocations.
///
/// This is retained for the frozen P15 overhead comparison only; eager search
/// itself drains [`HnswLazyCursor`] and owns no duplicate traversal loop.
///
/// # Errors
///
/// Returns [`HnswError::CursorMemoryBudgetExceeded`] when platform-sized
/// allocation arithmetic overflows.
pub fn projected_legacy_eager_hnsw_bytes(
    node_count: usize,
    ef: usize,
    filtered: bool,
) -> Result<usize> {
    let visited = exact_allocation_bytes::<bool>(node_count)?;
    let pending = growing_allocation_bytes::<Reverse<Candidate>>(node_count)?;
    let legacy_nearest_items = ef.saturating_add(1);
    let nearest = growing_allocation_bytes::<Candidate>(legacy_nearest_items)?;
    let results = exact_allocation_bytes::<HnswSearchResult>(ef)?;
    let neighbors = growing_allocation_bytes::<HnswNodeId>(MAX_GRAPH_NEIGHBORS_PER_LAYER)?;
    let second_neighbors = if filtered { neighbors } else { 0 };
    [
        visited,
        pending,
        nearest,
        results,
        neighbors,
        second_neighbors,
    ]
    .into_iter()
    .try_fold(0_usize, |total, bytes| total.checked_add(bytes))
    .ok_or(HnswError::CursorMemoryBudgetExceeded {
        maximum: usize::MAX,
        required: usize::MAX,
    })
}

fn exact_allocation_bytes<T>(items: usize) -> Result<usize> {
    items
        .checked_mul(size_of::<T>())
        .ok_or(HnswError::CursorMemoryBudgetExceeded {
            maximum: usize::MAX,
            required: usize::MAX,
        })
}

fn growing_allocation_bytes<T>(items: usize) -> Result<usize> {
    let capacity = if items == 0 {
        0
    } else {
        items
            .checked_next_power_of_two()
            .unwrap_or(usize::MAX)
            .max(4)
    };
    exact_allocation_bytes::<T>(capacity)
}

fn termination_for_error(error: &HnswError) -> HnswCursorTermination {
    match error {
        HnswError::Cancelled => HnswCursorTermination::Cancelled,
        HnswError::ComparisonBudgetExceeded { .. } => HnswCursorTermination::ComparisonBudget,
        HnswError::CursorMemoryBudgetExceeded { .. } => HnswCursorTermination::MemoryBudget,
        HnswError::CursorIncomplete { reason } => *reason,
        HnswError::GraphRead(_)
        | HnswError::InvalidSnapshot { .. }
        | HnswError::Core(_)
        | HnswError::DimensionMismatch { .. }
        | HnswError::DuplicatePointId { .. }
        | HnswError::InvalidParameter { .. }
        | HnswError::RecallBudgetExceeded { .. }
        | HnswError::UnsupportedMetric { .. } => HnswCursorTermination::AdapterError,
    }
}
