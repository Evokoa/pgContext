//! Pure Rust indexing algorithms for pgContext.
//!
//! Exact search, HNSW, candidate masks, recall checks, and quantization live
//! here before any PostgreSQL access method adapter is introduced.

use std::{cmp::Ordering, collections::BTreeSet, mem::size_of};

use context_core::policy::{MAX_HNSW_EF_CONSTRUCTION, MAX_HNSW_EF_SEARCH, MAX_HNSW_M, MIN_HNSW_M};
use context_core::{ContextError, DenseVector, DistanceMetric, Error as CoreError, SearchLimit};

mod delta_scan;
mod graph_mutation;
mod graph_mvcc;
mod graph_port;
mod hnsw_hierarchy;
mod page_codec;
mod quantization;
mod quantization_training;

pub use graph_mutation::{
    CURRENT_GRAPH_LAYOUT_VERSION, GRAPH_PAGE_HEADER_BYTES, GRAPH_PAGE_MAGIC,
    GRAPH_PENDING_RESERVATION_BYTES, GRAPH_PENDING_RESERVATION_REGION_BYTES, GraphAdjacencyStep,
    GraphAllocationState, GraphAppendStep, GraphAvailability, GraphDirectoryDepth,
    GraphDirectoryKeyKind, GraphFormatDisposition, GraphFormatVersion, GraphInsertEvent,
    GraphInsertEventKind, GraphInsertPhase, GraphInsertPlan, GraphLayerCount, GraphLockPlan,
    GraphLockTarget, GraphMutationDescriptor, GraphMutationDescriptorEntry,
    GraphMutationDescriptorEntryKind, GraphMutationDescriptorTransition, GraphMutationError,
    GraphMutationId, GraphMutationResult, GraphMutationStep, GraphNodePublication,
    GraphNodeReservation, GraphPageHeader, GraphPageId, GraphPageKind, GraphPublicationStep,
    GraphPublishedState, GraphReadyStep, GraphRebuildReason, GraphRecordRevision,
    GraphRepairReason, MAX_GRAPH_DIRECTORY_DEPTH, MAX_GRAPH_LOCK_TARGETS,
    MAX_PENDING_GRAPH_MUTATIONS,
};

pub use graph_mvcc::{
    GraphMvccError, GraphMvccResult, GraphNodeUse, GraphTombstonePlan,
    GraphTombstonePublicationStep, GraphTombstoneStep,
};

pub use graph_port::{
    GraphError, GraphMetadata, GraphNeighbors, GraphNodeRecord, GraphNodeView, GraphRead,
    GraphRecordId, GraphResult, GraphWrite, InMemoryGraphStore, MAX_GRAPH_LAYERS,
    MAX_GRAPH_NEIGHBORS_PER_LAYER, NewGraphNode,
};

pub use delta_scan::{
    CompactionLiveRow, DeltaHit, DeltaScanEntry, DeltaScanOutcome, fold_compaction_live_rows,
    merge_topk, scan_delta_topk,
};
pub use hnsw_hierarchy::{
    ConcurrentHnswBuilder, HnswCancellation, HnswGraphSnapshot, HnswInsertOutcome, HnswLevel,
    HnswLevelSeed, HnswSearchOutcome, HnswWork, NeverCancel, search_graph_read,
    search_graph_read_with_mask, search_graph_read_with_mask_budgeted,
};
pub use page_codec::{GraphPageCodecError, GraphPageEnvelope};

pub use quantization::{
    ProductCodebook, ProductQuantizedVector, ProductQuantizer, RerankCandidate, RerankResult,
    ScalarQuantizedVector, ScalarQuantizer, binary_quantize, rerank_by_original_vectors,
};
pub use quantization_training::{
    TrainedQuantizer, train_product_quantizer, train_scalar_quantizer,
};

/// Result type used by pure index structures.
pub type Result<T> = core::result::Result<T, HnswError>;

/// Errors produced by pure HNSW graph operations.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HnswError {
    /// A bounded graph-read adapter rejected persisted traversal data.
    #[error("HNSW graph read failed: {0}")]
    GraphRead(#[from] GraphError),

    /// HNSW configuration is outside supported policy.
    #[error("invalid HNSW parameter {parameter}: {value}")]
    InvalidParameter {
        /// Parameter name.
        parameter: &'static str,
        /// Invalid value.
        value: usize,
    },

    /// The requested core metric has no ascending-distance HNSW contract.
    #[error("unsupported HNSW metric: {metric}")]
    UnsupportedMetric {
        /// Stable metric name suitable for adapter error messages.
        metric: &'static str,
    },

    /// Two vectors have incompatible dimensions.
    #[error("dimension mismatch: left has {left} dimensions, right has {right}")]
    DimensionMismatch {
        /// Existing graph dimension.
        left: usize,
        /// Inserted vector dimension.
        right: usize,
    },

    /// Core vector validation failed.
    #[error("{0}")]
    Core(#[from] CoreError),

    /// Candidate mask exceeded the configured recall/candidate budget.
    #[error("candidate mask exceeds point budget {max}: {actual}")]
    RecallBudgetExceeded {
        /// Maximum allowed distinct mask point IDs.
        max: usize,
        /// Actual distinct mask point IDs.
        actual: usize,
    },

    /// An insertion reused a point identifier already present in the graph.
    #[error("duplicate HNSW point id {point_id:?}")]
    DuplicatePointId {
        /// Duplicate stable point identifier.
        point_id: HnswPointId,
    },

    /// A caller-owned cancellation checkpoint stopped bounded graph work.
    #[error("HNSW operation cancelled")]
    Cancelled,

    /// Serialized pure hierarchy state is truncated, unsupported, or invalid.
    #[error("invalid HNSW snapshot: {reason}")]
    InvalidSnapshot {
        /// Stable corruption reason.
        reason: &'static str,
    },
}

impl HnswError {
    /// Returns the stable error category used by SQL adapters.
    #[must_use]
    pub fn context_error(&self) -> ContextError {
        match self {
            Self::GraphRead(_) => ContextError::IndexCorrupt,
            Self::InvalidParameter { .. } => ContextError::InvalidFilter,
            Self::UnsupportedMetric { .. } => ContextError::UnsupportedMetric,
            Self::DimensionMismatch { .. } => ContextError::DimensionMismatch,
            Self::Core(error) => error.context_error(),
            Self::RecallBudgetExceeded { .. } => ContextError::RecallBudgetExceeded,
            Self::DuplicatePointId { .. } => ContextError::InvalidFilter,
            Self::Cancelled => ContextError::RecallBudgetExceeded,
            Self::InvalidSnapshot { .. } => ContextError::IndexCorrupt,
        }
    }
}

/// Stable point identifier stored in an HNSW node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HnswPointId(u64);

impl HnswPointId {
    /// Creates a point identifier wrapper.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the raw point identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Internal HNSW node identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HnswNodeId(usize);

impl HnswNodeId {
    /// Creates a node identifier wrapper.
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the raw node index.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// HNSW graph layer identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LayerIndex(usize);

impl LayerIndex {
    /// Creates a graph layer identifier.
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the base layer used by every HNSW node.
    #[must_use]
    pub const fn base() -> Self {
        Self(0)
    }

    /// Returns the raw layer index.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// HNSW construction and search parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswConfig {
    m: usize,
    ef_construction: usize,
    ef_search: usize,
}

impl HnswConfig {
    /// Creates validated HNSW parameters.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::InvalidParameter`] when `m` is below the reciprocal
    /// connectivity minimum or any value exceeds shared HNSW policy. The
    /// construction budget must also be at least `m`.
    pub const fn new(m: usize, ef_construction: usize, ef_search: usize) -> Result<Self> {
        if m < MIN_HNSW_M || m > MAX_HNSW_M {
            return Err(HnswError::InvalidParameter {
                parameter: "m",
                value: m,
            });
        }
        if ef_construction < m || ef_construction > MAX_HNSW_EF_CONSTRUCTION {
            return Err(HnswError::InvalidParameter {
                parameter: "ef_construction",
                value: ef_construction,
            });
        }
        if ef_search == 0 || ef_search > MAX_HNSW_EF_SEARCH {
            return Err(HnswError::InvalidParameter {
                parameter: "ef_search",
                value: ef_search,
            });
        }

        Ok(Self {
            m,
            ef_construction,
            ef_search,
        })
    }

    /// Returns the maximum number of graph neighbors retained per layer.
    #[must_use]
    pub const fn m(self) -> usize {
        self.m
    }

    /// Returns the construction candidate budget.
    #[must_use]
    pub const fn ef_construction(self) -> usize {
        self.ef_construction
    }

    /// Returns the search candidate budget.
    #[must_use]
    pub const fn ef_search(self) -> usize {
        self.ef_search
    }

    /// Returns the canonical degree limit for one hierarchy layer.
    ///
    /// HNSW uses `M0 = 2 * M` on the base layer and `M` above it.
    #[must_use]
    pub const fn max_connections(self, layer: LayerIndex) -> usize {
        if layer.get() == 0 { self.m * 2 } else { self.m }
    }
}

/// Pure Rust HNSW graph for dense vectors.
#[derive(Debug, Clone)]
pub struct HnswGraph {
    metric: DistanceMetric,
    config: HnswConfig,
    dimension: Option<usize>,
    entry_point: Option<HnswNodeId>,
    nodes: Vec<HnswNode>,
    point_ids: BTreeSet<HnswPointId>,
    level_seed: HnswLevelSeed,
    layer_tails: [Option<HnswNodeId>; MAX_GRAPH_LAYERS],
}

#[derive(Debug, Clone)]
struct HnswNode {
    point_id: HnswPointId,
    vector: DenseVector,
    layers: Vec<Vec<HnswNodeId>>,
    backbone_previous: Vec<Option<HnswNodeId>>,
    backbone_next: Vec<Option<HnswNodeId>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Candidate {
    node_id: HnswNodeId,
    score: f32,
}

impl Eq for Candidate {}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| self.node_id.cmp(&other.node_id))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A scored HNSW search result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HnswSearchResult {
    point_id: HnswPointId,
    score: f32,
}

impl HnswSearchResult {
    /// Returns the point identifier.
    #[must_use]
    pub const fn point_id(self) -> HnswPointId {
        self.point_id
    }

    /// Returns the metric score.
    #[must_use]
    pub const fn score(self) -> f32 {
        self.score
    }
}

/// Immutable node state used by storage adapters to persist and reload HNSW.
#[derive(Debug, Clone, PartialEq)]
pub struct HnswGraphNodeSnapshot {
    node_id: HnswNodeId,
    point_id: HnswPointId,
    vector: DenseVector,
    layers: Vec<Vec<HnswNodeId>>,
}

impl HnswGraphNodeSnapshot {
    /// Creates a base-layer node snapshot.
    #[must_use]
    pub fn new(
        node_id: HnswNodeId,
        point_id: HnswPointId,
        vector: DenseVector,
        base_neighbors: Vec<HnswNodeId>,
    ) -> Self {
        Self {
            node_id,
            point_id,
            vector,
            layers: vec![base_neighbors],
        }
    }

    /// Creates a full-hierarchy node snapshot for a storage adapter.
    ///
    /// An empty layer list is normalized to the required empty base layer.
    #[must_use]
    pub fn from_layers(
        node_id: HnswNodeId,
        point_id: HnswPointId,
        vector: DenseVector,
        layers: Vec<Vec<HnswNodeId>>,
    ) -> Self {
        Self {
            node_id,
            point_id,
            vector,
            layers: if layers.is_empty() {
                vec![Vec::new()]
            } else {
                layers
            },
        }
    }

    /// Returns the internal node identifier.
    #[must_use]
    pub const fn node_id(&self) -> HnswNodeId {
        self.node_id
    }

    /// Returns the stable point identifier.
    #[must_use]
    pub const fn point_id(&self) -> HnswPointId {
        self.point_id
    }

    /// Returns the original dense vector stored for exact scoring.
    #[must_use]
    pub const fn vector(&self) -> &DenseVector {
        &self.vector
    }

    /// Returns base-layer neighbor node identifiers.
    #[must_use]
    pub fn base_neighbors(&self) -> &[HnswNodeId] {
        self.layers.first().map(Vec::as_slice).unwrap_or_default()
    }
}

/// Deterministic memory estimate for the stored pure HNSW graph payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswMemoryEstimate {
    node_count: usize,
    vector_bytes: usize,
    link_bytes: usize,
}

impl HnswMemoryEstimate {
    /// Returns the number of graph nodes included in the estimate.
    #[must_use]
    pub const fn node_count(self) -> usize {
        self.node_count
    }

    /// Returns bytes used by stored vector values.
    #[must_use]
    pub const fn vector_bytes(self) -> usize {
        self.vector_bytes
    }

    /// Returns bytes used by stored neighbor identifiers.
    #[must_use]
    pub const fn link_bytes(self) -> usize {
        self.link_bytes
    }

    /// Returns total estimated payload bytes.
    #[must_use]
    pub const fn total_bytes(self) -> usize {
        self.vector_bytes + self.link_bytes
    }
}

/// Candidate mask used to restrict HNSW search results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateMask {
    allowed: BTreeSet<HnswPointId>,
    allow_all: bool,
}

impl CandidateMask {
    /// Creates a mask that allows every visited point.
    #[must_use]
    pub fn all() -> Self {
        Self {
            allowed: BTreeSet::new(),
            allow_all: true,
        }
    }

    /// Creates a mask that allows only the provided point identifiers.
    #[must_use]
    pub fn only(points: impl IntoIterator<Item = HnswPointId>) -> Self {
        Self {
            allowed: points.into_iter().collect(),
            allow_all: false,
        }
    }

    fn allows(&self, point_id: HnswPointId) -> bool {
        self.allow_all || self.allowed.contains(&point_id)
    }

    fn is_sparse_for(&self, node_count: usize) -> bool {
        !self.allow_all && self.allowed.len().saturating_mul(4) < node_count
    }

    fn validate_budget(&self) -> Result<()> {
        self.validate_budget_with_limit(context_core::policy::DEFAULT_HNSW_CANDIDATE_MASK_POINTS)
    }

    /// Validates this mask against an explicit, caller-supplied point budget.
    ///
    /// Callers that can raise the default candidate-mask ceiling (for
    /// example the AM masked-scan path, backed by
    /// `pgcontext.hnsw_mask_candidate_limit`) use this instead of the
    /// fixed-default [`Self::validate_budget`].
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::RecallBudgetExceeded`] when the mask has more
    /// than `max` allowed points.
    pub fn validate_budget_with_limit(&self, max: usize) -> Result<()> {
        if self.allow_all {
            return Ok(());
        }

        let actual = self.allowed.len();
        if actual > max {
            return Err(HnswError::RecallBudgetExceeded { max, actual });
        }
        Ok(())
    }
}

/// Storage strategy selected for a candidate pre-filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidatePrefilterKind {
    /// Every point is allowed.
    All,
    /// Sparse candidate IDs are stored as a sorted deduplicated list.
    Sorted,
    /// Dense candidate IDs are stored in a packed bitmap over a point-ID range.
    PackedBitmap,
}

/// Adaptive candidate pre-filter consumed directly by HNSW search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidatePrefilter {
    /// Allow every point.
    All,
    /// Sparse candidate set.
    Sorted(Vec<HnswPointId>),
    /// Dense candidate set packed by point-ID offset from `base`.
    PackedBitmap {
        /// First point ID represented by bit offset zero.
        base: u64,
        /// Packed bit words; a set bit means the point at `base + offset` is allowed.
        words: Vec<u64>,
        /// Number of distinct allowed points represented by the bitmap.
        count: usize,
    },
}

impl CandidatePrefilter {
    /// Creates a pre-filter that allows every point.
    #[must_use]
    pub const fn all() -> Self {
        Self::All
    }

    /// Creates an adaptive pre-filter from candidate point IDs.
    #[must_use]
    pub fn from_points(points: impl IntoIterator<Item = HnswPointId>) -> Self {
        let mut points = points.into_iter().collect::<Vec<_>>();
        points.sort_unstable_by_key(|point| point.get());
        points.dedup();

        let Some(first) = points.first().copied() else {
            return Self::Sorted(Vec::new());
        };
        let last = points.last().copied().unwrap_or(first);
        let Some(range) = last
            .get()
            .checked_sub(first.get())
            .and_then(|span| span.checked_add(1))
        else {
            return Self::Sorted(points);
        };
        let Ok(range_len) = usize::try_from(range) else {
            return Self::Sorted(points);
        };

        if range_len > points.len().saturating_mul(8).max(64) {
            return Self::Sorted(points);
        }

        let mut words = vec![0_u64; range_len.div_ceil(64)];
        for point in &points {
            let offset = usize::try_from(point.get() - first.get()).unwrap_or(usize::MAX);
            if let Some(word) = words.get_mut(offset / 64) {
                *word |= 1_u64 << (offset % 64);
            }
        }

        Self::PackedBitmap {
            base: first.get(),
            words,
            count: points.len(),
        }
    }

    /// Returns the selected storage strategy.
    #[must_use]
    pub const fn kind(&self) -> CandidatePrefilterKind {
        match self {
            Self::All => CandidatePrefilterKind::All,
            Self::Sorted(_) => CandidatePrefilterKind::Sorted,
            Self::PackedBitmap { .. } => CandidatePrefilterKind::PackedBitmap,
        }
    }

    fn allows(&self, point_id: HnswPointId) -> bool {
        match self {
            Self::All => true,
            Self::Sorted(points) => points
                .binary_search_by_key(&point_id.get(), |point| point.get())
                .is_ok(),
            Self::PackedBitmap { base, words, .. } => {
                let Some(offset) = point_id.get().checked_sub(*base) else {
                    return false;
                };
                let Ok(offset) = usize::try_from(offset) else {
                    return false;
                };
                words
                    .get(offset / 64)
                    .map(|word| (*word & (1_u64 << (offset % 64))) != 0)
                    .unwrap_or_default()
            }
        }
    }

    fn validate_budget(&self) -> Result<()> {
        self.validate_budget_with_limit(context_core::policy::DEFAULT_HNSW_CANDIDATE_MASK_POINTS)
    }

    /// Validates this pre-filter against an explicit, caller-supplied point budget.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::RecallBudgetExceeded`] when the pre-filter has
    /// more than `max` allowed points.
    pub fn validate_budget_with_limit(&self, max: usize) -> Result<()> {
        let actual = match self {
            Self::All => return Ok(()),
            Self::Sorted(points) => points.len(),
            Self::PackedBitmap { count, .. } => *count,
        };
        if actual > max {
            return Err(HnswError::RecallBudgetExceeded { max, actual });
        }
        Ok(())
    }
}

impl HnswGraph {
    /// Creates an empty HNSW graph.
    #[must_use]
    pub const fn new(metric: DistanceMetric, config: HnswConfig) -> Self {
        Self {
            metric,
            config,
            dimension: None,
            entry_point: None,
            nodes: Vec::new(),
            point_ids: BTreeSet::new(),
            level_seed: HnswLevelSeed::DEFAULT,
            layer_tails: [None; MAX_GRAPH_LAYERS],
        }
    }

    /// Returns the number of inserted nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` when the graph has no inserted nodes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns the current graph entry point.
    #[must_use]
    pub const fn entry_point(&self) -> Option<HnswNodeId> {
        self.entry_point
    }

    /// Returns the point identifier for a node.
    #[must_use]
    pub fn point_id(&self, node_id: HnswNodeId) -> Option<HnswPointId> {
        self.nodes.get(node_id.get()).map(|node| node.point_id)
    }

    /// Returns the number of layers stored for a node.
    #[must_use]
    pub fn layer_count(&self, node_id: HnswNodeId) -> Option<usize> {
        self.nodes.get(node_id.get()).map(|node| node.layers.len())
    }

    /// Returns node neighbors on one layer.
    #[must_use]
    pub fn neighbors(&self, node_id: HnswNodeId, layer: LayerIndex) -> Option<&[HnswNodeId]> {
        self.nodes
            .get(node_id.get())
            .and_then(|node| node.layers.get(layer.get()))
            .map(Vec::as_slice)
    }

    /// Returns a deterministic estimate of stored vector and graph-link bytes.
    ///
    /// The estimate intentionally covers payload owned by the graph: dense
    /// `f32` values and retained neighbor identifiers. Container allocation
    /// overhead is allocator-dependent and is not included.
    #[must_use]
    pub fn memory_estimate(&self) -> HnswMemoryEstimate {
        let vector_bytes = self
            .nodes
            .iter()
            .map(|node| node.vector.dimension() * size_of::<f32>())
            .sum();
        let link_bytes = self
            .nodes
            .iter()
            .flat_map(|node| node.layers.iter())
            .map(|neighbors| neighbors.len() * size_of::<HnswNodeId>())
            .sum();

        HnswMemoryEstimate {
            node_count: self.nodes.len(),
            vector_bytes,
            link_bytes,
        }
    }

    /// Returns immutable full-hierarchy node snapshots suitable for storage adapters.
    #[must_use]
    pub fn node_snapshots(&self) -> Vec<HnswGraphNodeSnapshot> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| HnswGraphNodeSnapshot {
                node_id: HnswNodeId::new(index),
                point_id: node.point_id,
                vector: node.vector.clone(),
                layers: node.layers.clone(),
            })
            .collect()
    }

    /// Rebuilds an HNSW graph from persisted base-layer node snapshots.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::InvalidParameter`] when node ids are not contiguous
    /// in snapshot order or a neighbor points outside the snapshot set. Returns
    /// [`HnswError::DimensionMismatch`] when stored vectors have inconsistent
    /// dimensions.
    pub fn from_base_layer_snapshots(
        metric: DistanceMetric,
        config: HnswConfig,
        snapshots: Vec<HnswGraphNodeSnapshot>,
    ) -> Result<Self> {
        let mut graph = Self::new(metric, config);
        let node_count = snapshots.len();
        for (expected_id, snapshot) in snapshots.into_iter().enumerate() {
            if snapshot.node_id.get() != expected_id {
                return Err(HnswError::InvalidParameter {
                    parameter: "node_id",
                    value: snapshot.node_id.get(),
                });
            }
            if !graph.point_ids.insert(snapshot.point_id) {
                return Err(HnswError::DuplicatePointId {
                    point_id: snapshot.point_id,
                });
            }
            graph.ensure_dimension(&snapshot.vector)?;
            for neighbor in snapshot.base_neighbors() {
                if neighbor.get() >= node_count || neighbor.get() == expected_id {
                    return Err(HnswError::InvalidParameter {
                        parameter: "neighbor_id",
                        value: neighbor.get(),
                    });
                }
            }
            graph.nodes.push(HnswNode {
                point_id: snapshot.point_id,
                vector: snapshot.vector,
                layers: snapshot.layers,
                backbone_previous: Vec::new(),
                backbone_next: Vec::new(),
            });
        }
        graph.rebuild_hierarchy_backbone();
        if !graph.nodes.is_empty() {
            graph.entry_point = Some(HnswNodeId::new(0));
        }
        Ok(graph)
    }

    /// Rebuilds an HNSW graph from persisted node snapshots and its published
    /// entry point.
    ///
    /// Unlike [`Self::from_base_layer_snapshots`], this constructor preserves
    /// the storage adapter's authoritative entry point instead of assuming
    /// node zero. This matters after an upper-layer node has replaced the
    /// original entry point.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::from_base_layer_snapshots`], plus
    /// [`HnswError::InvalidParameter`] when the entry point is missing for a
    /// non-empty graph or lies outside the persisted node span.
    pub fn from_persisted_snapshots(
        metric: DistanceMetric,
        config: HnswConfig,
        entry_point: Option<HnswNodeId>,
        snapshots: Vec<HnswGraphNodeSnapshot>,
    ) -> Result<Self> {
        let mut graph = Self::from_base_layer_snapshots(metric, config, snapshots)?;
        match (graph.nodes.is_empty(), entry_point) {
            (true, None) => graph.entry_point = None,
            (false, Some(entry_point)) if entry_point.get() < graph.nodes.len() => {
                graph.entry_point = Some(entry_point);
            }
            (_, Some(entry_point)) => {
                return Err(HnswError::InvalidParameter {
                    parameter: "entry_point",
                    value: entry_point.get(),
                });
            }
            (false, None) => {
                return Err(HnswError::InvalidParameter {
                    parameter: "entry_point",
                    value: usize::MAX,
                });
            }
        }
        Ok(graph)
    }

    /// Inserts a vector and returns its internal node identifier.
    ///
    /// The node receives a deterministic bounded level from the graph seed and
    /// insertion ordinal. Construction descends existing upper layers and uses
    /// `ef_construction` to select and prune reciprocal neighbors.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::DuplicatePointId`] when `point_id` already exists,
    /// [`HnswError::DimensionMismatch`] when the vector dimension differs, or
    /// [`HnswError::Core`] when metric distance computation fails.
    pub fn insert(&mut self, point_id: HnswPointId, vector: DenseVector) -> Result<HnswNodeId> {
        let level = self.assigned_level(HnswNodeId::new(self.nodes.len()));
        self.insert_at_level(point_id, vector, level)
            .map(HnswInsertOutcome::node_id)
    }

    /// Searches the hierarchy with the configured `ef_search` budget.
    ///
    /// Results are ordered by ascending metric score, then ascending point id.
    /// Empty graphs return an empty result.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::DimensionMismatch`] when `query` does not match the
    /// graph dimension. Returns [`HnswError::Core`] when metric distance
    /// computation fails.
    pub fn search(&self, query: &DenseVector, limit: SearchLimit) -> Result<Vec<HnswSearchResult>> {
        self.search_with_mask(query, limit, &CandidateMask::all())
    }

    /// Searches the hierarchy while restricting returned point IDs.
    ///
    /// Masked-out nodes may still be visited as traversal connectors, but they
    /// are not returned.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::DimensionMismatch`] when `query` does not match the
    /// graph dimension. Returns [`HnswError::Core`] when metric distance
    /// computation fails.
    pub fn search_with_mask(
        &self,
        query: &DenseVector,
        limit: SearchLimit,
        mask: &CandidateMask,
    ) -> Result<Vec<HnswSearchResult>> {
        self.search_filtered(
            query,
            limit,
            || mask.validate_budget(),
            |point_id| mask.allows(point_id),
        )
    }

    /// Searches the base-layer graph while restricting returned point IDs with
    /// an adaptive sorted or packed-bitmap pre-filter.
    ///
    /// # Errors
    ///
    /// Returns [`HnswError::RecallBudgetExceeded`] when the pre-filter exceeds
    /// the configured candidate-mask budget.
    pub fn search_with_prefilter(
        &self,
        query: &DenseVector,
        limit: SearchLimit,
        prefilter: &CandidatePrefilter,
    ) -> Result<Vec<HnswSearchResult>> {
        self.search_filtered(
            query,
            limit,
            || prefilter.validate_budget(),
            |point_id| prefilter.allows(point_id),
        )
    }

    fn search_filtered(
        &self,
        query: &DenseVector,
        limit: SearchLimit,
        validate_filter: impl FnOnce() -> Result<()>,
        allows: impl Fn(HnswPointId) -> bool,
    ) -> Result<Vec<HnswSearchResult>> {
        validate_filter()?;
        self.search_hierarchical(query, limit, allows, &mut NeverCancel)
            .map(|(results, _)| results)
    }

    fn ensure_dimension(&mut self, vector: &DenseVector) -> Result<()> {
        match self.dimension {
            Some(dimension) if dimension != vector.dimension() => {
                Err(HnswError::DimensionMismatch {
                    left: dimension,
                    right: vector.dimension(),
                })
            }
            Some(_) => Ok(()),
            None => {
                self.dimension = Some(vector.dimension());
                Ok(())
            }
        }
    }

    fn ensure_query_dimension(&self, query: &DenseVector) -> Result<()> {
        match self.dimension {
            Some(dimension) if dimension != query.dimension() => {
                Err(HnswError::DimensionMismatch {
                    left: dimension,
                    right: query.dimension(),
                })
            }
            Some(_) | None => Ok(()),
        }
    }

    fn distance_to_node(&self, query: &DenseVector, node_id: HnswNodeId) -> Result<f32> {
        let Some(node) = self.nodes.get(node_id.get()) else {
            return Err(HnswError::InvalidParameter {
                parameter: "node_id",
                value: node_id.get(),
            });
        };
        hnsw_hierarchy::hnsw_distance(self.metric, query, &node.vector)
    }
}

fn sort_candidates(candidates: &mut [Candidate]) {
    candidates.sort_by(|left, right| {
        left.score
            .total_cmp(&right.score)
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
}

fn sort_search_results(results: &mut [HnswSearchResult]) {
    results.sort_by(|left, right| {
        left.score
            .total_cmp(&right.score)
            .then_with(|| left.point_id.cmp(&right.point_id))
    });
}

/// Returns the package version compiled into this crate.
#[must_use]
pub const fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
