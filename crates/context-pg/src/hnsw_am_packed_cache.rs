// In-memory packed-generation cache fragment included by `hnsw_am.rs`:
// directory cache, packed graph arrays, shared/local store dispatch, delta
// serving generations, and the serving/build telemetry counters.


/// Scan-local authoritative node locator table.
///
/// Directory pages are append-only. A higher revision wins; records with an
/// equal revision retain append order, so the last observed locator wins.
#[derive(Default)]
struct HnswDirectoryIndex {
    nodes: BTreeMap<usize, HnswDirectoryRecord>,
}

#[derive(Clone)]
struct CachedHnswDirectory {
    epoch: u64,
    meta_lsn: pg_sys::XLogRecPtr,
    directory: Rc<HnswDirectoryIndex>,
}

#[derive(Debug, Clone, Copy)]
struct PackedHnswNode {
    point_id: HnswPointId,
    vector_start: usize,
    layers_start: usize,
    layer_count: usize,
}

#[derive(Debug, Clone, Copy)]
struct PackedHnswLayer {
    neighbors_start: usize,
    neighbor_count: usize,
}

/// Backend-local immutable graph generation built from authoritative relation
/// pages and invalidated by the metapage publication identity.
struct PackedHnswGraph {
    dimensions: usize,
    nodes: Vec<PackedHnswNode>,
    vectors: Vec<f32>,
    layers: Vec<PackedHnswLayer>,
    neighbors: Vec<HnswNodeId>,
}

impl PackedHnswGraph {
    fn from_records(
        records: Vec<HnswVectorRecord>,
        node_count: usize,
        dimensions: usize,
    ) -> context_index::GraphResult<Self> {
        if records.len() != node_count {
            return Err(context_index::GraphError::CorruptGraph {
                message: "packed HNSW generation node count is incomplete".to_owned(),
            });
        }
        let mut nodes = Vec::with_capacity(node_count);
        let mut vectors = Vec::with_capacity(node_count.saturating_mul(dimensions));
        let mut layers = Vec::new();
        let mut neighbors = Vec::new();
        for (expected_id, record) in records.into_iter().enumerate() {
            if record.node_id.get() != expected_id || record.vector.dimension() != dimensions {
                return Err(context_index::GraphError::CorruptGraph {
                    message: "packed HNSW generation has non-contiguous nodes or dimensions"
                        .to_owned(),
                });
            }
            if record.layers.is_empty() {
                return Err(context_index::GraphError::MissingBaseLayer {
                    node_id: record.node_id,
                });
            }
            let vector_start = vectors.len();
            vectors.extend_from_slice(record.vector.as_slice());
            let layers_start = layers.len();
            for layer_neighbors in &record.layers {
                let neighbors_start = neighbors.len();
                neighbors.extend_from_slice(layer_neighbors);
                layers.push(PackedHnswLayer {
                    neighbors_start,
                    neighbor_count: layer_neighbors.len(),
                });
            }
            nodes.push(PackedHnswNode {
                // Preserve the tombstone flag for traversal so dead structural
                // nodes can be excluded from the final candidate stream.
                point_id: HnswPointId::new(record.heap_tid),
                vector_start,
                layers_start,
                layer_count: record.layers.len(),
            });
        }
        Ok(Self {
            dimensions,
            nodes,
            vectors,
            layers,
            neighbors,
        })
    }

    fn node(&self, node_id: HnswNodeId) -> Option<(PackedHnswNode, &[f32])> {
        let node = *self.nodes.get(node_id.get())?;
        let vector_end = node.vector_start.checked_add(self.dimensions)?;
        Some((node, self.vectors.get(node.vector_start..vector_end)?))
    }

    /// Approximate resident bytes of the packed arrays (payload only;
    /// allocator overhead is excluded, matching `estimate_index_memory`).
    fn byte_size(&self) -> u64 {
        let nodes = self.nodes.len() * size_of::<PackedHnswNode>();
        let vectors = self.vectors.len() * size_of::<f32>();
        let layers = self.layers.len() * size_of::<PackedHnswLayer>();
        let neighbors = self.neighbors.len() * size_of::<HnswNodeId>();
        (nodes + vectors + layers + neighbors) as u64
    }

    /// Encodes this generation into the portable shared-image byte format
    /// (see `context_storage::packed_graph_image`), for publishing to the
    /// shared serving registry.
    fn encode_image(&self) -> Result<Vec<u8>, PackedGraphImageError> {
        let dimensions = u32::try_from(self.dimensions)
            .map_err(|_| PackedGraphImageError::CountOverflow)?;
        let nodes: Vec<PackedGraphImageNode> = self
            .nodes
            .iter()
            .map(|node| PackedGraphImageNode {
                point_id: node.point_id.get(),
                vector_start: node.vector_start as u64,
                layers_start: node.layers_start as u64,
                layer_count: node.layer_count as u64,
            })
            .collect();
        let layers: Vec<PackedGraphImageLayer> = self
            .layers
            .iter()
            .map(|layer| PackedGraphImageLayer {
                neighbors_start: layer.neighbors_start as u64,
                neighbor_count: layer.neighbor_count as u64,
            })
            .collect();
        let neighbors: Vec<u64> = self.neighbors.iter().map(|id| id.get() as u64).collect();
        encode_packed_graph_image(
            dimensions,
            &nodes,
            &layers,
            &neighbors,
            &self.vectors,
        )
    }

    fn neighbors(&self, node: PackedHnswNode, layer: LayerIndex) -> Option<&[HnswNodeId]> {
        if layer.get() >= node.layer_count {
            return None;
        }
        let layer = self.layers.get(node.layers_start + layer.get())?;
        let end = layer.neighbors_start.checked_add(layer.neighbor_count)?;
        self.neighbors.get(layer.neighbors_start..end)
    }
}

/// Storage seam for served packed generations.
///
/// Traversal depends on these accessors rather than on the concrete backing
/// store, so the backing can move from backend-local heap memory to a shared
/// read-mostly image (the decided hybrid serving model) without touching the
/// traversal code.
#[derive(Clone)]
enum PackedGraphStore {
    Local(Rc<PackedHnswGraph>),
    /// A read view attached from the shared serving registry:
    /// another backend published this generation, so this backend serves it
    /// without rebuilding from PostgreSQL pages.
    Shared(Rc<AttachedSharedImage>),
}

/// Node identity fields shared across both `PackedGraphStore` backings.
#[derive(Debug, Clone, Copy)]
struct PackedNodeInfo {
    point_id: HnswPointId,
    layer_count: usize,
}

impl PackedGraphStore {
    fn node(&self, node_id: HnswNodeId) -> Option<(PackedNodeInfo, &[f32])> {
        match self {
            Self::Local(graph) => {
                let (node, vector) = graph.node(node_id)?;
                Some((
                    PackedNodeInfo {
                        point_id: node.point_id,
                        layer_count: node.layer_count,
                    },
                    vector,
                ))
            }
            Self::Shared(image) => {
                let node = image.view().node(node_id.get())?;
                let vector = image.view().node_vector(node)?;
                let layer_count = usize::try_from(node.layer_count).ok()?;
                Some((
                    PackedNodeInfo {
                        point_id: HnswPointId::new(node.point_id),
                        layer_count,
                    },
                    vector,
                ))
            }
        }
    }

    /// Looks up `node_id`'s neighbors at `layer` and appends the ones whose
    /// id is below `node_count` (excludes not-yet-published rewire targets)
    /// into `output`. Returns `Ok(false)` when `node_id` is unknown to this
    /// generation, or `Err(LayerNotFound)` when the node exists but `layer`
    /// does not.
    fn neighbors_into(
        &self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        node_count: usize,
        output: &mut Vec<HnswNodeId>,
    ) -> context_index::GraphResult<bool> {
        match self {
            Self::Local(graph) => {
                let Some((node, _)) = graph.node(node_id) else {
                    output.clear();
                    return Ok(false);
                };
                let Some(neighbors) = graph.neighbors(node, layer) else {
                    return Err(context_index::GraphError::LayerNotFound { node_id, layer });
                };
                output.clear();
                output.extend(
                    neighbors
                        .iter()
                        .copied()
                        .filter(|neighbor| neighbor.get() < node_count),
                );
                Ok(true)
            }
            Self::Shared(image) => {
                let Some(node) = image.view().node(node_id.get()) else {
                    output.clear();
                    return Ok(false);
                };
                let Some(neighbors) = image.view().neighbors(node, layer.get()) else {
                    return Err(context_index::GraphError::LayerNotFound { node_id, layer });
                };
                output.clear();
                output.extend(
                    neighbors
                        .filter_map(|id| usize::try_from(id).ok().map(HnswNodeId::new))
                        .filter(|neighbor| neighbor.get() < node_count),
                );
                Ok(true)
            }
        }
    }

}

/// A packed generation ready to serve traversal: a base, either
/// backend-local or attached from the shared registry.
///
/// A generation is immutable and whole. It used to carry an optional overlay
/// of nodes that had changed since `revision_watermark`, so a stale base could
/// be patched instead of repacked; that was retired with the segmented write
/// path, which absorbs writes on disk instead. Patching was only ever correct
/// because a compaction dirties every node and so happened to shadow the
/// entire stale base -- an emergent property of the layout, not an invariant,
/// and one that a partially-dirty generation would have broken by mixing two
/// node numberings.
#[derive(Clone)]
struct HnswPackedGeneration {
    base: PackedGraphStore,
}

impl HnswPackedGeneration {
    fn node(&self, node_id: HnswNodeId) -> Option<(PackedNodeInfo, &[f32])> {
        self.base.node(node_id)
    }

    fn neighbors_into(
        &self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        node_count: usize,
        output: &mut Vec<HnswNodeId>,
    ) -> context_index::GraphResult<bool> {
        self.base.neighbors_into(node_id, layer, node_count, output)
    }
}

#[derive(Clone)]
struct CachedPackedHnswGraph {
    /// Physical relation file this pack was built from. REINDEX swaps the
    /// relfilenode while keeping the index OID, and the fresh build's
    /// directory revisions restart low enough that the stale-patch path's
    /// `dirty_since` watermark sees no drift — so without this field a
    /// cached pack of the pre-REINDEX graph is re-served as current.
    rel_file_number: u32,
    epoch: u64,
    meta_lsn: pg_sys::XLogRecPtr,
    graph: HnswPackedGeneration,
}

/// Backend-local packed-generation serving telemetry.
///
/// The packed cache is currently backend-local, so these counters describe
/// the current backend only; they exist so operators can observe pack
/// builds, reuses, and their cost without guessing from latency cliffs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HnswServingStats {
    pub(crate) pack_builds: u64,
    pub(crate) pack_reuses: u64,
    pub(crate) last_pack_bytes: u64,
    pub(crate) last_pack_millis: u64,
    pub(crate) total_pack_millis: u64,
    /// Packed generations attached from the shared registry
    /// instead of rebuilt from PostgreSQL pages.
    pub(crate) shared_attaches: u64,
    /// Backend-local builds successfully published to the shared registry.
    pub(crate) shared_publishes: u64,
    /// Backend-local builds whose publish was skipped (GUC off, over
    /// budget, or segment creation failed) — service continued locally.
    pub(crate) shared_publish_skips: u64,
    /// Queries served from unpacked directory reads because no pack was
    /// available and `pgcontext.hnsw_pack_on_first_use` was off.
    pub(crate) page_native_fallbacks: u64,
    /// Rows appended to a segmented-write delta region (P2-S3) instead of
    /// being spliced into the HNSW graph, including tombstones appended by
    /// VACUUM for delta-only rows.
    pub(crate) delta_segment_records: u64,
    /// Scans that merged an exact delta-region scan with base-graph
    /// candidates because the index's delta region held at least one record.
    pub(crate) delta_segment_scans: u64,
}

thread_local! {
    static HNSW_DIRECTORY_CACHE: RefCell<BTreeMap<u32, CachedHnswDirectory>> =
        const { RefCell::new(BTreeMap::new()) };
    static HNSW_PACKED_GRAPH_CACHE: RefCell<BTreeMap<u32, CachedPackedHnswGraph>> =
        const { RefCell::new(BTreeMap::new()) };
    static HNSW_SERVING_STATS: Cell<HnswServingStats> =
        const { Cell::new(HnswServingStats {
            pack_builds: 0,
            pack_reuses: 0,
            last_pack_bytes: 0,
            last_pack_millis: 0,
            total_pack_millis: 0,
            shared_attaches: 0,
            shared_publishes: 0,
            shared_publish_skips: 0,
            page_native_fallbacks: 0,
            delta_segment_records: 0,
            delta_segment_scans: 0,
        }) };
}

/// Phase timing of the most recent HNSW bulk build in this backend.
///
/// `graph_millis` covers the heap scan plus in-memory graph construction;
/// `write_millis` covers snapshot extraction, page writes, and Generic-WAL
/// emission. The split directs build-performance work at the dominant phase.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct HnswBuildProfile {
    pub(crate) tuples: u64,
    pub(crate) graph_millis: u64,
    pub(crate) write_millis: u64,
}

thread_local! {
    static HNSW_BUILD_PROFILE: Cell<HnswBuildProfile> =
        const { Cell::new(HnswBuildProfile {
            tuples: 0,
            graph_millis: 0,
            write_millis: 0,
        }) };
}

/// Returns the phase profile of this backend's most recent HNSW bulk build.
pub(crate) fn hnsw_build_profile_snapshot() -> HnswBuildProfile {
    HNSW_BUILD_PROFILE.with(Cell::get)
}

fn record_hnsw_build_profile(profile: HnswBuildProfile) {
    HNSW_BUILD_PROFILE.with(|slot| slot.set(profile));
}

fn saturating_elapsed_millis(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Returns this backend's packed-generation serving counters.
pub(crate) fn hnsw_serving_stats_snapshot() -> HnswServingStats {
    HNSW_SERVING_STATS.with(Cell::get)
}

fn record_hnsw_pack_reuse() {
    HNSW_SERVING_STATS.with(|stats| {
        let mut current = stats.get();
        current.pack_reuses = current.pack_reuses.saturating_add(1);
        stats.set(current);
    });
}

fn record_hnsw_pack_build(bytes: u64, millis: u64) {
    HNSW_SERVING_STATS.with(|stats| {
        let mut current = stats.get();
        current.pack_builds = current.pack_builds.saturating_add(1);
        current.last_pack_bytes = bytes;
        current.last_pack_millis = millis;
        current.total_pack_millis = current.total_pack_millis.saturating_add(millis);
        stats.set(current);
    });
}

fn record_hnsw_shared_attach() {
    HNSW_SERVING_STATS.with(|stats| {
        let mut current = stats.get();
        current.shared_attaches = current.shared_attaches.saturating_add(1);
        stats.set(current);
    });
}

fn record_hnsw_shared_publish(published: bool) {
    HNSW_SERVING_STATS.with(|stats| {
        let mut current = stats.get();
        if published {
            current.shared_publishes = current.shared_publishes.saturating_add(1);
        } else {
            current.shared_publish_skips = current.shared_publish_skips.saturating_add(1);
        }
        stats.set(current);
    });
}



fn record_hnsw_page_native_fallback() {
    HNSW_SERVING_STATS.with(|stats| {
        let mut current = stats.get();
        current.page_native_fallbacks = current.page_native_fallbacks.saturating_add(1);
        stats.set(current);
    });
}

fn record_hnsw_delta_segment_record() {
    HNSW_SERVING_STATS.with(|stats| {
        let mut current = stats.get();
        current.delta_segment_records = current.delta_segment_records.saturating_add(1);
        stats.set(current);
    });
}

fn record_hnsw_delta_segment_scan() {
    HNSW_SERVING_STATS.with(|stats| {
        let mut current = stats.get();
        current.delta_segment_scans = current.delta_segment_scans.saturating_add(1);
        stats.set(current);
    });
}

impl HnswDirectoryIndex {
    fn observe(&mut self, record: HnswDirectoryRecord) {
        if record.key_kind != GraphDirectoryKeyKind::Node || record.ordinal != 0 {
            return;
        }
        let Ok(identity) = usize::try_from(record.identity) else {
            return;
        };
        let replace = self
            .nodes
            .get(&identity)
            .is_none_or(|current| record.revision >= current.revision);
        if replace {
            self.nodes.insert(identity, record);
        }
    }

    fn node(&self, node_id: HnswNodeId) -> Option<HnswDirectoryRecord> {
        self.nodes.get(&node_id.get()).copied()
    }


}
