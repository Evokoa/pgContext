//! Storage-agnostic HNSW graph read and write ports.

use context_core::DenseVector;

use crate::{HnswNodeId, HnswPointId, LayerIndex};

/// Result type for graph adapter operations.
pub type GraphResult<T> = Result<T, GraphError>;

/// Maximum number of layers accepted in one owned graph node DTO.
pub const MAX_GRAPH_LAYERS: usize = 64;

/// Maximum neighbor identifiers accepted for one node layer.
pub const MAX_GRAPH_NEIGHBORS_PER_LAYER: usize = context_core::policy::MAX_HNSW_M * 2;

/// Opaque graph-record token assigned and interpreted only by an adapter.
///
/// PostgreSQL page adapters assign this token to a node record and bind the
/// physical heap TID separately; artifact adapters translate their own record
/// identity. It is deliberately distinct from logical
/// [`context_core::PointId`], [`HnswNodeId`], heap TIDs, page offsets, and
/// artifact offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GraphRecordId(u64);

impl GraphRecordId {
    /// Creates a graph-record token at an explicit adapter boundary.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the adapter-owned payload value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Transport-neutral failures produced by graph adapters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum GraphError {
    /// A write addressed a node that does not exist.
    #[error("graph node {node_id:?} does not exist")]
    NodeNotFound {
        /// Missing graph-local node identifier.
        node_id: HnswNodeId,
    },

    /// A write addressed a layer that the node does not have.
    #[error("graph node {node_id:?} does not have layer {layer:?}")]
    LayerNotFound {
        /// Graph-local node identifier.
        node_id: HnswNodeId,
        /// Missing layer.
        layer: LayerIndex,
    },

    /// A node referenced itself as a neighbor.
    #[error("graph node {node_id:?} cannot reference itself")]
    SelfNeighbor {
        /// Self-referencing graph-local node identifier.
        node_id: HnswNodeId,
    },

    /// A neighbor appeared more than once on one layer.
    #[error("graph node {node_id:?} repeats neighbor {neighbor_id:?} on layer {layer:?}")]
    DuplicateNeighbor {
        /// Node whose neighbor list is invalid.
        node_id: HnswNodeId,
        /// Repeated neighbor.
        neighbor_id: HnswNodeId,
        /// Layer containing the duplicate.
        layer: LayerIndex,
    },

    /// A neighbor identifier is absent from the graph.
    #[error(
        "graph node {node_id:?} references missing neighbor {neighbor_id:?} on layer {layer:?}"
    )]
    NeighborNotFound {
        /// Node whose neighbor list is invalid.
        node_id: HnswNodeId,
        /// Missing neighbor.
        neighbor_id: HnswNodeId,
        /// Layer containing the invalid reference.
        layer: LayerIndex,
    },

    /// A neighbor does not participate in the referenced layer.
    #[error("graph node {node_id:?} references neighbor {neighbor_id:?} without layer {layer:?}")]
    NeighborLayerNotFound {
        /// Node whose neighbor list is invalid.
        node_id: HnswNodeId,
        /// Neighbor without the requested layer.
        neighbor_id: HnswNodeId,
        /// Layer missing from the neighbor.
        layer: LayerIndex,
    },

    /// A node vector differs from the graph dimension.
    #[error("graph dimension mismatch: expected {expected}, got {actual}")]
    DimensionMismatch {
        /// Established graph dimension.
        expected: usize,
        /// Supplied vector dimension.
        actual: usize,
    },

    /// Root publication referenced a node that does not exist.
    #[error("graph entry point {node_id:?} does not exist")]
    EntryPointNotFound {
        /// Missing root node.
        node_id: HnswNodeId,
    },

    /// A node omitted the required base layer.
    #[error("graph node {node_id:?} must contain a base layer")]
    MissingBaseLayer {
        /// Node without a base layer.
        node_id: HnswNodeId,
    },

    /// A bounded adapter allocation could not be reserved.
    #[error("graph adapter could not reserve capacity for {operation}")]
    CapacityExceeded {
        /// Stable operation name.
        operation: &'static str,
    },

    /// A node DTO exceeded the algorithm-local layer ceiling.
    #[error("graph node {node_id:?} has too many layers: {actual} > {maximum}")]
    TooManyLayers {
        /// Node with the oversized layer set.
        node_id: HnswNodeId,
        /// Maximum accepted layer count.
        maximum: usize,
        /// Supplied layer count.
        actual: usize,
    },

    /// A node layer exceeded the algorithm-local neighbor ceiling.
    #[error(
        "graph node {node_id:?} has too many neighbors on layer {layer:?}: {actual} > {maximum}"
    )]
    TooManyNeighbors {
        /// Node with the oversized neighbor set.
        node_id: HnswNodeId,
        /// Oversized layer.
        layer: LayerIndex,
        /// Maximum accepted neighbor count.
        maximum: usize,
        /// Supplied neighbor count.
        actual: usize,
    },

    /// A requested layer exceeds the DTO layer ceiling.
    #[error("graph layer {layer:?} exceeds maximum layer index {maximum}")]
    LayerOutOfBounds {
        /// Out-of-range requested layer.
        layer: LayerIndex,
        /// Maximum accepted zero-based layer index.
        maximum: usize,
    },

    /// Decoded graph bytes or metadata violate the pure graph contract.
    #[error("corrupt graph state: {message}")]
    CorruptGraph {
        /// Bounded adapter-supplied corruption detail.
        message: String,
    },

    /// An infrastructure adapter failed while satisfying a pure operation.
    #[error("graph adapter {operation} failed: {message}")]
    AdapterFailure {
        /// Stable pure operation name.
        operation: &'static str,
        /// Bounded adapter-supplied failure detail.
        message: String,
    },
}

/// Owned graph metadata returned by [`GraphRead`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphMetadata {
    node_count: usize,
    entry_point: Option<HnswNodeId>,
    dimensions: Option<usize>,
}

impl GraphMetadata {
    /// Returns empty graph metadata.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            node_count: 0,
            entry_point: None,
            dimensions: None,
        }
    }

    /// Creates adapter metadata after validating the published entry point.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError::EntryPointNotFound`] when `entry_point` is outside
    /// `node_count`, or [`GraphError::CorruptGraph`] for impossible dimension
    /// metadata.
    pub fn new(
        node_count: usize,
        entry_point: Option<HnswNodeId>,
        dimensions: Option<usize>,
    ) -> GraphResult<Self> {
        if let Some(node_id) = entry_point
            && node_id.get() >= node_count
        {
            return Err(GraphError::EntryPointNotFound { node_id });
        }
        if dimensions == Some(0) || (node_count > 0 && dimensions.is_none()) {
            return Err(GraphError::CorruptGraph {
                message: "graph dimensions are missing or zero".to_owned(),
            });
        }
        Ok(Self {
            node_count,
            entry_point,
            dimensions,
        })
    }

    /// Returns the number of graph nodes visible to the adapter.
    #[must_use]
    pub const fn node_count(self) -> usize {
        self.node_count
    }

    /// Returns the published traversal entry point.
    #[must_use]
    pub const fn entry_point(self) -> Option<HnswNodeId> {
        self.entry_point
    }

    /// Returns the established vector dimension, if any.
    #[must_use]
    pub const fn dimensions(self) -> Option<usize> {
        self.dimensions
    }
}

/// Fully owned, validated graph node payload returned by a read adapter.
///
/// Because the vector is owned, this record cannot retain a borrow into a
/// PostgreSQL buffer or mapped generation. Adjacency is read incrementally by
/// requested layer through [`GraphRead::read_neighbors`].
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNodeRecord {
    node_id: HnswNodeId,
    record_id: GraphRecordId,
    point_id: HnswPointId,
    vector: DenseVector,
    layer_count: usize,
}

/// Buffer-scoped node view used by traversal hot paths.
///
/// The view cannot outlive the adapter callback that produced it, allowing a
/// PostgreSQL adapter to score values directly from a pinned page.
#[derive(Debug, Clone, Copy)]
pub struct GraphNodeView<'a> {
    node_id: HnswNodeId,
    point_id: HnswPointId,
    vector: &'a [f32],
    layer_count: usize,
}

impl<'a> GraphNodeView<'a> {
    /// Creates a borrowed node view after validating graph-relative bounds.
    pub fn new(
        node_count: usize,
        node_id: HnswNodeId,
        point_id: HnswPointId,
        vector: &'a [f32],
        layer_count: usize,
    ) -> GraphResult<Self> {
        validate_layer_count(node_count, node_id, layer_count)?;
        if vector.is_empty() {
            return Err(GraphError::CorruptGraph {
                message: "graph node vector is empty".to_owned(),
            });
        }
        Ok(Self {
            node_id,
            point_id,
            vector,
            layer_count,
        })
    }

    /// Returns the graph-local identifier.
    #[must_use]
    pub const fn node_id(self) -> HnswNodeId {
        self.node_id
    }

    /// Returns the result identity bound to this node.
    #[must_use]
    pub const fn point_id(self) -> HnswPointId {
        self.point_id
    }

    /// Returns the borrowed dense values.
    #[must_use]
    pub const fn vector(self) -> &'a [f32] {
        self.vector
    }

    /// Returns the number of encoded graph layers.
    #[must_use]
    pub const fn layer_count(self) -> usize {
        self.layer_count
    }
}

impl GraphNodeRecord {
    /// Creates an owned record after validating its graph-relative identifier
    /// and bounded layer count.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] when the node is outside `node_count`, has no
    /// base layer, or exceeds the layer ceiling.
    pub fn new(
        node_count: usize,
        node_id: HnswNodeId,
        record_id: GraphRecordId,
        point_id: HnswPointId,
        vector: DenseVector,
        layer_count: usize,
    ) -> GraphResult<Self> {
        validate_layer_count(node_count, node_id, layer_count)?;
        Ok(Self {
            node_id,
            record_id,
            point_id,
            vector,
            layer_count,
        })
    }

    /// Returns the graph-local node identifier.
    #[must_use]
    pub const fn node_id(&self) -> HnswNodeId {
        self.node_id
    }

    /// Returns the opaque adapter-owned record token.
    #[must_use]
    pub const fn record_id(&self) -> GraphRecordId {
        self.record_id
    }

    /// Returns the HNSW result identity bound to this node.
    #[must_use]
    pub const fn point_id(&self) -> HnswPointId {
        self.point_id
    }

    /// Returns the owned dense vector.
    #[must_use]
    pub const fn vector(&self) -> &DenseVector {
        &self.vector
    }

    /// Returns the number of layers available for incremental adjacency reads.
    #[must_use]
    pub const fn layer_count(&self) -> usize {
        self.layer_count
    }

    /// Decomposes this owned record for an adapter or algorithm consumer.
    #[must_use]
    pub fn into_parts(self) -> (HnswNodeId, GraphRecordId, HnswPointId, DenseVector, usize) {
        (
            self.node_id,
            self.record_id,
            self.point_id,
            self.vector,
            self.layer_count,
        )
    }
}

/// Owned adjacency returned for one requested node layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNeighbors {
    node_id: HnswNodeId,
    layer: LayerIndex,
    neighbors: Vec<HnswNodeId>,
}

impl GraphNeighbors {
    /// Creates bounded validated adjacency for a known node count.
    ///
    /// # Errors
    ///
    /// Returns [`GraphError`] for a missing node, self-link, duplicate,
    /// out-of-range neighbor, or oversized neighbor list.
    pub fn new(
        node_count: usize,
        node_id: HnswNodeId,
        layer: LayerIndex,
        neighbors: Vec<HnswNodeId>,
    ) -> GraphResult<Self> {
        validate_layer_index(layer)?;
        if node_id.get() >= node_count {
            return Err(GraphError::NodeNotFound { node_id });
        }
        validate_neighbor_count(node_id, layer, neighbors.len())?;
        validate_distinct_non_self(node_id, layer, &neighbors)?;
        if let Some(&neighbor_id) = neighbors
            .iter()
            .find(|neighbor_id| neighbor_id.get() >= node_count)
        {
            return Err(GraphError::NeighborNotFound {
                node_id,
                neighbor_id,
                layer,
            });
        }
        Ok(Self {
            node_id,
            layer,
            neighbors,
        })
    }

    /// Returns the owning node identifier.
    #[must_use]
    pub const fn node_id(&self) -> HnswNodeId {
        self.node_id
    }

    /// Returns the requested graph layer.
    #[must_use]
    pub const fn layer(&self) -> LayerIndex {
        self.layer
    }

    /// Returns the owned neighbor identifiers.
    #[must_use]
    pub fn neighbors(&self) -> &[HnswNodeId] {
        &self.neighbors
    }

    /// Decomposes this owned adjacency value.
    #[must_use]
    pub fn into_parts(self) -> (HnswNodeId, LayerIndex, Vec<HnswNodeId>) {
        (self.node_id, self.layer, self.neighbors)
    }
}

/// Owned node payload accepted by [`GraphWrite::append_node`].
#[derive(Debug, Clone, PartialEq)]
pub struct NewGraphNode {
    record_id: GraphRecordId,
    point_id: HnswPointId,
    vector: DenseVector,
    layers: Vec<Vec<HnswNodeId>>,
}

impl NewGraphNode {
    /// Creates an owned node payload. The adapter validates graph-relative
    /// dimensions and neighbor references before mutating state.
    #[must_use]
    pub fn new(
        record_id: GraphRecordId,
        point_id: HnswPointId,
        vector: DenseVector,
        layers: Vec<Vec<HnswNodeId>>,
    ) -> Self {
        Self {
            record_id,
            point_id,
            vector,
            layers,
        }
    }

    /// Decomposes this payload for a write adapter.
    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        GraphRecordId,
        HnswPointId,
        DenseVector,
        Vec<Vec<HnswNodeId>>,
    ) {
        (self.record_id, self.point_id, self.vector, self.layers)
    }
}

/// Synchronous storage-agnostic graph read port.
///
/// Reads take `&mut self` so an adapter may acquire and release transient pins,
/// locks, or decoding scratch state. Returned values are always owned.
pub trait GraphRead {
    /// Reads graph-wide traversal metadata.
    fn metadata(&mut self) -> GraphResult<GraphMetadata>;

    /// Reads one owned node, or `None` when the identifier is absent.
    fn read_node(&mut self, node_id: HnswNodeId) -> GraphResult<Option<GraphNodeRecord>>;

    /// Visits one node while its adapter-owned backing storage remains valid.
    ///
    /// The default preserves compatibility with owned adapters. Page and mmap
    /// adapters should override this method to avoid decoding and copying the
    /// complete node.
    fn with_node<R>(
        &mut self,
        node_id: HnswNodeId,
        visitor: impl FnOnce(GraphNodeView<'_>) -> R,
    ) -> GraphResult<Option<R>> {
        let Some(node) = self.read_node(node_id)? else {
            return Ok(None);
        };
        let view = GraphNodeView::new(
            self.metadata()?.node_count(),
            node.node_id(),
            node.point_id(),
            node.vector().as_slice(),
            node.layer_count(),
        )?;
        Ok(Some(visitor(view)))
    }

    /// Reads one owned adjacency list.
    ///
    /// A missing node returns `Ok(None)`. An existing node without `layer`
    /// returns [`GraphError::LayerNotFound`], while a present empty layer
    /// returns an owned empty [`GraphNeighbors`].
    fn read_neighbors(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
    ) -> GraphResult<Option<GraphNeighbors>>;

    /// Decodes one adjacency list into reusable caller-owned scratch space.
    ///
    /// Returns `Ok(false)` for a missing node. The default delegates to the
    /// owned read contract; packed adapters should override it.
    fn read_neighbors_into(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        output: &mut Vec<HnswNodeId>,
    ) -> GraphResult<bool> {
        output.clear();
        let Some(neighbors) = self.read_neighbors(node_id, layer)? else {
            return Ok(false);
        };
        output.extend_from_slice(neighbors.neighbors());
        Ok(true)
    }
}

/// Synchronous storage-agnostic graph mutation port.
///
/// Each call is one semantic adapter-level mutation intent and never exposes
/// mutable bytes. Physical page grouping, WAL units, and lock order are defined
/// by later design contracts.
pub trait GraphWrite: GraphRead {
    /// Appends a validated node and returns its adapter-assigned contiguous id.
    fn append_node(&mut self, node: NewGraphNode) -> GraphResult<HnswNodeId>;

    /// Replaces one complete layer neighbor list after validating every id.
    fn replace_neighbors(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        neighbors: Vec<HnswNodeId>,
    ) -> GraphResult<()>;

    /// Publishes or clears the graph traversal entry point.
    fn publish_entry_point(&mut self, entry_point: Option<HnswNodeId>) -> GraphResult<()>;
}

/// Deterministic owned adapter used by pure graph tests and algorithms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InMemoryGraphStore {
    dimensions: Option<usize>,
    entry_point: Option<HnswNodeId>,
    nodes: Vec<StoredGraphNode>,
}

#[derive(Debug, Clone, PartialEq)]
struct StoredGraphNode {
    record_id: GraphRecordId,
    point_id: HnswPointId,
    vector: DenseVector,
    layers: Vec<Vec<HnswNodeId>>,
}

impl InMemoryGraphStore {
    /// Creates an empty in-memory graph adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dimensions: None,
            entry_point: None,
            nodes: Vec::new(),
        }
    }

    fn validate_neighbors(
        &self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        neighbors: &[HnswNodeId],
    ) -> GraphResult<()> {
        validate_neighbor_count(node_id, layer, neighbors.len())?;
        validate_distinct_non_self(node_id, layer, neighbors)?;
        for &neighbor_id in neighbors {
            let Some(neighbor) = self.nodes.get(neighbor_id.get()) else {
                return Err(GraphError::NeighborNotFound {
                    node_id,
                    neighbor_id,
                    layer,
                });
            };
            if neighbor.layers.get(layer.get()).is_none() {
                return Err(GraphError::NeighborLayerNotFound {
                    node_id,
                    neighbor_id,
                    layer,
                });
            }
        }
        Ok(())
    }
}

impl GraphRead for InMemoryGraphStore {
    fn metadata(&mut self) -> GraphResult<GraphMetadata> {
        GraphMetadata::new(self.nodes.len(), self.entry_point, self.dimensions)
    }

    fn read_node(&mut self, node_id: HnswNodeId) -> GraphResult<Option<GraphNodeRecord>> {
        Ok(self.nodes.get(node_id.get()).map(|node| GraphNodeRecord {
            node_id,
            record_id: node.record_id,
            point_id: node.point_id,
            vector: node.vector.clone(),
            layer_count: node.layers.len(),
        }))
    }

    fn read_neighbors(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
    ) -> GraphResult<Option<GraphNeighbors>> {
        validate_layer_index(layer)?;
        let Some(node) = self.nodes.get(node_id.get()) else {
            return Ok(None);
        };
        let Some(neighbors) = node.layers.get(layer.get()) else {
            return Err(GraphError::LayerNotFound { node_id, layer });
        };
        Ok(Some(GraphNeighbors {
            node_id,
            layer,
            neighbors: neighbors.clone(),
        }))
    }
}

impl GraphWrite for InMemoryGraphStore {
    fn append_node(&mut self, node: NewGraphNode) -> GraphResult<HnswNodeId> {
        let node_id = HnswNodeId::new(self.nodes.len());
        let (record_id, point_id, vector, layers) = node.into_parts();
        if layers.is_empty() {
            return Err(GraphError::MissingBaseLayer { node_id });
        }
        if layers.len() > MAX_GRAPH_LAYERS {
            return Err(GraphError::TooManyLayers {
                node_id,
                maximum: MAX_GRAPH_LAYERS,
                actual: layers.len(),
            });
        }
        if let Some(expected) = self.dimensions {
            let actual = vector.dimension();
            if actual != expected {
                return Err(GraphError::DimensionMismatch { expected, actual });
            }
        }
        for (index, neighbors) in layers.iter().enumerate() {
            self.validate_neighbors(node_id, LayerIndex::new(index), neighbors)?;
        }

        self.nodes
            .try_reserve(1)
            .map_err(|_| GraphError::CapacityExceeded {
                operation: "node append",
            })?;
        let dimensions = vector.dimension();
        self.nodes.push(StoredGraphNode {
            record_id,
            point_id,
            vector,
            layers,
        });
        self.dimensions.get_or_insert(dimensions);
        Ok(node_id)
    }

    fn replace_neighbors(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        neighbors: Vec<HnswNodeId>,
    ) -> GraphResult<()> {
        validate_layer_index(layer)?;
        let Some(node) = self.nodes.get(node_id.get()) else {
            return Err(GraphError::NodeNotFound { node_id });
        };
        if node.layers.get(layer.get()).is_none() {
            return Err(GraphError::LayerNotFound { node_id, layer });
        }
        self.validate_neighbors(node_id, layer, &neighbors)?;
        self.nodes[node_id.get()].layers[layer.get()] = neighbors;
        Ok(())
    }

    fn publish_entry_point(&mut self, entry_point: Option<HnswNodeId>) -> GraphResult<()> {
        if let Some(node_id) = entry_point
            && self.nodes.get(node_id.get()).is_none()
        {
            return Err(GraphError::EntryPointNotFound { node_id });
        }
        self.entry_point = entry_point;
        Ok(())
    }
}

fn validate_layer_count(
    node_count: usize,
    node_id: HnswNodeId,
    layer_count: usize,
) -> GraphResult<()> {
    if node_id.get() >= node_count {
        return Err(GraphError::NodeNotFound { node_id });
    }
    if layer_count == 0 {
        return Err(GraphError::MissingBaseLayer { node_id });
    }
    if layer_count > MAX_GRAPH_LAYERS {
        return Err(GraphError::TooManyLayers {
            node_id,
            maximum: MAX_GRAPH_LAYERS,
            actual: layer_count,
        });
    }
    Ok(())
}

fn validate_neighbor_count(
    node_id: HnswNodeId,
    layer: LayerIndex,
    actual: usize,
) -> GraphResult<()> {
    validate_layer_index(layer)?;
    if actual > MAX_GRAPH_NEIGHBORS_PER_LAYER {
        return Err(GraphError::TooManyNeighbors {
            node_id,
            layer,
            maximum: MAX_GRAPH_NEIGHBORS_PER_LAYER,
            actual,
        });
    }
    Ok(())
}

fn validate_layer_index(layer: LayerIndex) -> GraphResult<()> {
    if layer.get() >= MAX_GRAPH_LAYERS {
        return Err(GraphError::LayerOutOfBounds {
            layer,
            maximum: MAX_GRAPH_LAYERS - 1,
        });
    }
    Ok(())
}

fn validate_distinct_non_self(
    node_id: HnswNodeId,
    layer: LayerIndex,
    neighbors: &[HnswNodeId],
) -> GraphResult<()> {
    for (index, &neighbor_id) in neighbors.iter().enumerate() {
        if neighbor_id == node_id {
            return Err(GraphError::SelfNeighbor { node_id });
        }
        if neighbors[..index].contains(&neighbor_id) {
            return Err(GraphError::DuplicateNeighbor {
                node_id,
                neighbor_id,
                layer,
            });
        }
    }
    Ok(())
}
