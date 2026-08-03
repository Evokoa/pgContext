// Graph-traversal scan fragment included by `hnsw_am.rs`: the `GraphRead`
// impl over `PgHnswGraphRead` and the persisted-page HNSW scan entry
// points used by the AM callbacks and the masked SQL path.

impl GraphRead for ParallelPackedGraphRead {
    fn metadata(&mut self) -> context_index::GraphResult<GraphMetadata> {
        Ok(self.metadata)
    }

    fn prepare_query(
        &mut self,
        metric: DistanceMetric,
        query: &DenseVector,
    ) -> context_index::GraphResult<()> {
        self.prepared_query = self.graph.prepare_query(query, metric)?;
        Ok(())
    }

    fn score_node(
        &mut self,
        node_id: HnswNodeId,
        metric: DistanceMetric,
        query: &DenseVector,
    ) -> context_index::GraphResult<Option<GraphNodeScore>> {
        let Some((node, vector)) = self.graph.node(node_id) else {
            return Ok(None);
        };
        self.node_reads = self.node_reads.saturating_add(1);
        let score = if let Some(prepared) = &self.prepared_query {
            let code = self.graph.node_code(node_id).ok_or_else(|| {
                context_index::GraphError::CorruptGraph {
                    message: "quantized packed HNSW node is missing its code".to_owned(),
                }
            })?;
            prepared.score(code).map_err(|error| {
                context_index::GraphError::CorruptGraph {
                    message: format!("quantized packed HNSW score failed: {error}"),
                }
            })?
        } else {
            metric
                .distance_slices(query.as_slice(), vector)
                .map_err(|error| context_index::GraphError::CorruptGraph {
                    message: format!("packed HNSW score failed: {error}"),
                })?
        };
        Ok(Some(GraphNodeScore::new(
            score,
            node.point_id,
            node.layer_count,
        )))
    }

    fn read_node(
        &mut self,
        node_id: HnswNodeId,
    ) -> context_index::GraphResult<Option<GraphNodeRecord>> {
        let Some((node, vector)) = self.graph.node(node_id) else {
            return Ok(None);
        };
        let vector = DenseVector::new(vector.to_vec()).map_err(|error| {
            context_index::GraphError::CorruptGraph {
                message: format!("parallel packed HNSW vector is invalid: {error}"),
            }
        })?;
        GraphNodeRecord::new(
            self.metadata.node_count(),
            node_id,
            GraphRecordId::new(node_id.get() as u64),
            node.point_id,
            vector,
            node.layer_count,
        )
        .map(Some)
    }

    fn with_node<R>(
        &mut self,
        node_id: HnswNodeId,
        visitor: impl FnOnce(GraphNodeView<'_>) -> R,
    ) -> context_index::GraphResult<Option<R>> {
        let Some((node, vector)) = self.graph.node(node_id) else {
            return Ok(None);
        };
        self.node_reads = self.node_reads.saturating_add(1);
        GraphNodeView::new(
            self.metadata.node_count(),
            node_id,
            node.point_id,
            vector,
            node.layer_count,
        )
        .map(|view| Some(visitor(view)))
    }

    fn read_neighbors(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
    ) -> context_index::GraphResult<Option<GraphNeighbors>> {
        let Some((node, _)) = self.graph.node(node_id) else {
            return Ok(None);
        };
        let Some(neighbors) = self.graph.neighbors(node, layer) else {
            return Err(context_index::GraphError::LayerNotFound { node_id, layer });
        };
        GraphNeighbors::new(
            self.metadata.node_count(),
            node_id,
            layer,
            neighbors
                .iter()
                .copied()
                .filter(|neighbor| neighbor.get() < self.metadata.node_count())
                .collect(),
        )
        .map(Some)
    }

    fn read_neighbors_into(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        output: &mut Vec<HnswNodeId>,
    ) -> context_index::GraphResult<bool> {
        let Some((node, _)) = self.graph.node(node_id) else {
            output.clear();
            return Ok(false);
        };
        let Some(neighbors) = self.graph.neighbors(node, layer) else {
            return Err(context_index::GraphError::LayerNotFound { node_id, layer });
        };
        output.clear();
        output.extend(
            neighbors
                .iter()
                .copied()
                .filter(|neighbor| neighbor.get() < self.metadata.node_count()),
        );
        Ok(true)
    }
}

impl GraphRead for PgHnswGraphRead {
    fn metadata(&mut self) -> context_index::GraphResult<GraphMetadata> {
        // SAFETY: this adapter exists only for the active AM callback.
        let meta = unsafe { self.meta() };
        // SAFETY: the descriptor is copied from the validated publication.
        let segment = unsafe { self.selected_segment() };
        let graph_nodes = segment.map_or(0, |segment| segment.graph_nodes);
        let entry_node_id = segment.map_or(u64::MAX, |segment| segment.entry_node_id);
        let node_count = usize::try_from(graph_nodes).map_err(|_| context_index::GraphError::CapacityExceeded {
            operation: "HNSW metapage node count",
        })?;
        let entry = if entry_node_id == u64::MAX {
            None
        } else {
            Some(HnswNodeId::new(
                usize::try_from(entry_node_id).map_err(|_| {
                    context_index::GraphError::CapacityExceeded {
                        operation: "HNSW metapage entry node",
                    }
                })?,
            ))
        };
        GraphMetadata::new(node_count, entry, (meta.dimensions != 0).then_some(meta.dimensions as usize))
    }

    fn prepare_query(
        &mut self,
        metric: DistanceMetric,
        query: &DenseVector,
    ) -> context_index::GraphResult<()> {
        // SAFETY: this adapter exists only for the active AM callback and the
        // packed generation remains pinned by `self` for the traversal.
        self.prepared_query = unsafe { self.load_packed()? }
            .map(|packed| packed.prepare_query(query, metric))
            .transpose()?
            .flatten();
        Ok(())
    }

    fn score_node(
        &mut self,
        node_id: HnswNodeId,
        metric: DistanceMetric,
        query: &DenseVector,
    ) -> context_index::GraphResult<Option<GraphNodeScore>> {
        if self.prepared_query.is_some() {
            // SAFETY: this adapter owns the active relation and generation.
            let packed = unsafe { self.load_packed()? }.ok_or_else(|| {
                context_index::GraphError::AdapterFailure {
                    operation: "score quantized HNSW node",
                    message: "quantized traversal has no packed generation".to_owned(),
                }
            })?;
            let prepared = self.prepared_query.as_ref().ok_or_else(|| {
                context_index::GraphError::AdapterFailure {
                    operation: "score quantized HNSW node",
                    message: "quantized traversal lost its prepared query".to_owned(),
                }
            })?;
            let Some((node, _)) = packed.node(node_id) else {
                return Ok(None);
            };
            let code = packed.node_code(node_id).ok_or_else(|| {
                context_index::GraphError::CorruptGraph {
                    message: "quantized HNSW node is missing its codec row".to_owned(),
                }
            })?;
            let score = prepared.score(code).map_err(|error| {
                context_index::GraphError::CorruptGraph {
                    message: format!("quantized HNSW score failed: {error}"),
                }
            })?;
            self.node_reads = self.node_reads.saturating_add(1);
            return Ok(Some(GraphNodeScore::new(
                score,
                node.point_id,
                node.layer_count,
            )));
        }
        self.with_node(node_id, |node| {
            metric
                .distance_slices(query.as_slice(), node.vector())
                .map(|score| GraphNodeScore::new(score, node.point_id(), node.layer_count()))
        })?
        .transpose()
        .map_err(|error| context_index::GraphError::CorruptGraph {
            message: format!("page-native HNSW score failed: {error}"),
        })
    }

    fn read_node(&mut self, node_id: HnswNodeId) -> context_index::GraphResult<Option<GraphNodeRecord>> {
        let metadata = self.metadata()?;
        // SAFETY: this adapter exists only for the active AM callback.
        let Some(record) = (unsafe { self.node(node_id) }) else { return Ok(None); };
        GraphNodeRecord::new(
            metadata.node_count(),
            node_id,
            GraphRecordId::new(node_id.get() as u64),
            HnswPointId::new(hnsw_record_heap_tid(&record)),
            record.vector,
            record.layers.len(),
        ).map(Some)
    }

    fn with_node<R>(
        &mut self,
        node_id: HnswNodeId,
        visitor: impl FnOnce(GraphNodeView<'_>) -> R,
    ) -> context_index::GraphResult<Option<R>> {
        let metadata = self.metadata()?;
        // SAFETY: the packed generation owns copies loaded from the current
        // metapage publication and is invalidated by epoch/LSN changes.
        let packed = unsafe { self.load_packed()? };
        let Some(graph) = packed else {
            // Page-native fallback: no pack is available and
            // inline packing is disabled, so read this one node directly
            // from its directory-located page instead.
            // SAFETY: this adapter exists only for the active AM callback.
            let Some(record) = (unsafe { self.node(node_id) }) else {
                return Ok(None);
            };
            self.node_reads = self.node_reads.saturating_add(1);
            return GraphNodeView::new(
                metadata.node_count(),
                node_id,
                HnswPointId::new(record.heap_tid),
                record.vector.as_slice(),
                record.layers.len(),
            )
            .map(|view| Some(visitor(view)));
        };
        let Some((node, vector)) = graph.node(node_id) else {
            return Ok(None);
        };
        self.node_reads = self.node_reads.saturating_add(1);
        GraphNodeView::new(
            metadata.node_count(),
            node_id,
            node.point_id,
            vector,
            node.layer_count,
        )
        .map(|view| Some(visitor(view)))
    }

    fn read_neighbors(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
    ) -> context_index::GraphResult<Option<GraphNeighbors>> {
        let metadata = self.metadata()?;
        // SAFETY: this adapter exists only for the active AM callback.
        let Some(record) = (unsafe { self.node(node_id) }) else { return Ok(None); };
        let Some(neighbors) = record.layers.get(layer.get()) else {
            return Err(context_index::GraphError::LayerNotFound { node_id, layer });
        };
        // Rewire records may become visible before the final metapage count
        // publication. Those future-node links are not in the published graph
        // yet, so retain the previous reader state until the count/root commit.
        let published_neighbors = neighbors
            .iter()
            .copied()
            .filter(|neighbor| neighbor.get() < metadata.node_count())
            .collect();
        GraphNeighbors::new(metadata.node_count(), node_id, layer, published_neighbors).map(Some)
    }

    fn read_neighbors_into(
        &mut self,
        node_id: HnswNodeId,
        layer: LayerIndex,
        output: &mut Vec<HnswNodeId>,
    ) -> context_index::GraphResult<bool> {
        let metadata = self.metadata()?;
        // SAFETY: the packed generation is bound to this metapage publication.
        let packed = unsafe { self.load_packed()? };
        let Some(graph) = packed else {
            // Page-native fallback: mirrors `read_neighbors`, but
            // writes into the caller's reusable buffer.
            // SAFETY: this adapter exists only for the active AM callback.
            let Some(record) = (unsafe { self.node(node_id) }) else {
                output.clear();
                return Ok(false);
            };
            let Some(neighbors) = record.layers.get(layer.get()) else {
                return Err(context_index::GraphError::LayerNotFound { node_id, layer });
            };
            output.clear();
            output.extend(
                neighbors
                    .iter()
                    .copied()
                    .filter(|neighbor| neighbor.get() < metadata.node_count()),
            );
            return Ok(true);
        };
        graph.neighbors_into(node_id, layer, metadata.node_count(), output)
    }
}

struct ParallelSegmentCancellation {
    cancelled: Arc<AtomicBool>,
}

impl HnswCancellation for ParallelSegmentCancellation {
    fn check(&mut self) -> context_index::Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(HnswError::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Default)]
struct ParallelSegmentOutcome {
    hits: Vec<SegmentDeltaHit>,
    node_reads: usize,
}

#[derive(Clone, Copy)]
struct SegmentDeltaHit {
    segment_index: usize,
    hit: DeltaHit,
}

const HNSW_TREE_ENTRY_PEAK_BYTES: u64 = 128;

fn checked_projection_sum(values: impl IntoIterator<Item = u64>) -> Option<u64> {
    values
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
}

fn projected_mask_bytes(point_count: usize) -> Option<u64> {
    u64::try_from(point_count)
        .ok()?
        .checked_mul(HNSW_TREE_ENTRY_PEAK_BYTES)
}

fn projected_mutation_overlay_bytes(meta: HnswMetaPage) -> Option<u64> {
    let record_count = projected_mutation_record_count(meta)?;
    let vector_bytes = u64::from(meta.dimensions).checked_mul(size_of::<f32>() as u64)?;
    let per_record = (size_of::<context_storage::DeltaRecord>() as u64)
        .checked_add(vector_bytes)?
        .checked_mul(2)?;
    let outer_vectors = u64::try_from(meta.segments().len())
        .ok()?
        .checked_mul(size_of::<Vec<context_storage::DeltaRecord>>() as u64)?;
    record_count
        .checked_mul(per_record)?
        .checked_add(outer_vectors)
}

fn projected_mutation_record_count(meta: HnswMetaPage) -> Option<u64> {
    meta.segments().iter().try_fold(
        meta.delta_record_count,
        |total, segment| total.checked_add(segment.mutation_record_count),
    )
}

fn projected_graph_traversal_bytes(
    node_count: u64,
    search_width: usize,
    filtered: bool,
) -> Option<u64> {
    let search_width = u64::try_from(search_width).ok()?.min(node_count);
    let candidate_slot = (size_of::<(HnswNodeId, f32)>() as u64).checked_mul(2)?;
    let visited = node_count;
    let pending = node_count.checked_mul(candidate_slot)?.checked_mul(2)?;
    let nearest = search_width
        .checked_mul(candidate_slot)?
        .checked_mul(2)?;
    let results = search_width
        .checked_mul(size_of::<context_index::HnswSearchResult>() as u64)?
        .checked_mul(2)?;
    let scratch_count = if filtered { 2_u64 } else { 1_u64 };
    let scratch = u64::try_from(context_index::MAX_GRAPH_NEIGHBORS_PER_LAYER)
        .ok()?
        .checked_mul(size_of::<HnswNodeId>() as u64)?
        .checked_mul(2)?
        .checked_mul(scratch_count)?;
    checked_projection_sum([visited, pending, nearest, results, scratch])
}

fn projected_retained_hit_bytes(meta: HnswMetaPage, requested_limit: usize) -> Option<u64> {
    u64::try_from(meta.segments().len())
        .ok()?
        .checked_mul(u64::try_from(requested_limit).ok()?)?
        .checked_mul(size_of::<SegmentDeltaHit>() as u64)?
        .checked_mul(4)
}

fn projected_serial_segment_scan_bytes(
    meta: HnswMetaPage,
    requested_limit: usize,
    ef_search: usize,
    mask_point_count: usize,
    filtered: bool,
) -> Option<u64> {
    let search_width = ef_search.max(requested_limit);
    let overlay = projected_mutation_overlay_bytes(meta)?;
    let mask = projected_mask_bytes(mask_point_count)?
        .checked_mul(if filtered { 2 } else { 1 })?;
    let retained_hits = projected_retained_hit_bytes(meta, requested_limit)?;
    let mutation_count = projected_mutation_record_count(meta)?;
    let retirement = mutation_count.checked_mul(HNSW_TREE_ENTRY_PEAK_BYTES)?;
    let traversal = meta
        .segments()
        .iter()
        .map(|segment| projected_graph_traversal_bytes(segment.graph_nodes, search_width, filtered))
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max()
        .unwrap_or_default();
    let traversal_peak = checked_projection_sum([
        overlay,
        mask,
        retained_hits,
        retirement,
        traversal,
    ])?;
    let retained_point_count = u64::try_from(meta.segments().len())
        .ok()?
        .checked_mul(u64::try_from(requested_limit).ok()?)?;
    let merge_entries = retained_point_count
        .checked_add(mutation_count)?
        .checked_mul(HNSW_TREE_ENTRY_PEAK_BYTES)?;
    let merge_peak = checked_projection_sum([overlay, mask, retained_hits, merge_entries])?;
    Some(traversal_peak.max(merge_peak))
}

fn projected_parallel_segment_scan_bytes(
    meta: HnswMetaPage,
    requested_limit: usize,
    ef_search: usize,
    mask_point_count: usize,
    filtered: bool,
) -> Option<u64> {
    let search_width = ef_search.max(requested_limit);
    let graph_copies = projected_parallel_segment_bytes(meta)?;
    let traversals = meta.segments().iter().try_fold(0_u64, |total, segment| {
        total.checked_add(projected_graph_traversal_bytes(
            segment.graph_nodes,
            search_width,
            filtered,
        )?)
    })?;
    checked_projection_sum([
        graph_copies,
        traversals,
        projected_mask_bytes(mask_point_count)?.checked_mul(if filtered { 2 } else { 1 })?,
        projected_retained_hit_bytes(meta, requested_limit)?,
    ])
}

#[cfg(test)]
fn serial_segment_memory_admitted(projected_bytes: u64, max_memory_bytes: u64) -> bool {
    projected_bytes <= max_memory_bytes
}

fn require_hnsw_scan_memory(
    projected_bytes: Option<u64>,
    max_memory_bytes: usize,
    operation: &'static str,
) {
    let maximum = u64::try_from(max_memory_bytes).unwrap_or(u64::MAX);
    let actual = projected_bytes.unwrap_or(u64::MAX);
    if actual > maximum {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "HNSW {operation} requires {actual} extension-owned bytes, exceeding query memory budget {maximum}"
            ),
        );
    }
}

struct PublishedMutationOverlay {
    frozen: Vec<Vec<context_storage::DeltaRecord>>,
    active: Vec<context_storage::DeltaRecord>,
}

impl PublishedMutationOverlay {
    fn record_count(&self) -> usize {
        self.frozen
            .iter()
            .fold(self.active.len(), |total, records| {
                total.saturating_add(records.len())
            })
    }


    fn retirement_points_after_segment(&self, segment_index: usize) -> Vec<HnswPointId> {
        self.frozen[segment_index..]
            .iter()
            .flat_map(|records| records.iter())
            .chain(self.active.iter())
            .map(|record| HnswPointId::new(record.heap_tid))
            .collect()
    }
}

unsafe fn read_published_mutation_overlay(
    index_relation: pg_sys::Relation,
    meta: HnswMetaPage,
) -> Option<PublishedMutationOverlay> {
    let mut frozen = Vec::with_capacity(meta.segments().len());
    for segment in meta.segments() {
        let records = if segment.mutation_start_block == u64::MAX {
            Vec::new()
        } else {
            unsafe {
                read_hnsw_delta_records_range(
                    index_relation,
                    segment.mutation_start_block,
                    segment.mutation_end_block,
                    segment.mutation_generation,
                    GraphPageKind::FrozenDelta,
                    segment.mutation_record_count,
                )
            }
        };
        frozen.push(records);
    }
    let active = unsafe { try_read_hnsw_delta_records(index_relation, meta) }?;
    Some(PublishedMutationOverlay { frozen, active })
}

unsafe fn read_consistent_hnsw_publication(
    index_relation: pg_sys::Relation,
    max_memory_bytes: usize,
    retained_bytes: u64,
) -> (HnswMetaPage, PublishedMutationOverlay) {
    for _ in 0..16 {
        let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
        require_hnsw_scan_memory(
            projected_mutation_overlay_bytes(meta)
                .and_then(|overlay| overlay.checked_add(retained_bytes)),
            max_memory_bytes,
            "mutation-overlay decode",
        );
        if let Some(overlay) = unsafe { read_published_mutation_overlay(index_relation, meta) } {
            return (meta, overlay);
        }
        pg_sys::check_for_interrupts!();
    }
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
        "HNSW publication changed repeatedly while starting a scan; retry the statement",
    )
}

type SegmentPoolJob = Box<dyn FnOnce() + Send + 'static>;

struct SegmentSearchPool {
    sender: SyncSender<SegmentPoolJob>,
}

impl SegmentSearchPool {
    fn start() -> Option<Self> {
        let (sender, receiver) = mpsc::sync_channel::<SegmentPoolJob>(HNSW_MAX_SEGMENTS * 2);
        let receiver = Arc::new(Mutex::new(receiver));
        for worker in 0..HNSW_MAX_SEGMENTS {
            let receiver = Arc::clone(&receiver);
            if std::thread::Builder::new()
                .name(format!("pgcontext-hnsw-{worker}"))
                .spawn(move || loop {
                    let job = receiver.lock().ok().and_then(|locked| locked.recv().ok());
                    let Some(job) = job else { break };
                    job();
                })
                .is_err()
            {
                return None;
            }
        }
        Some(Self { sender })
    }

    fn try_submit(&self, job: SegmentPoolJob) -> bool {
        match self.sender.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

static SEGMENT_SEARCH_POOL: OnceLock<Option<SegmentSearchPool>> = OnceLock::new();

thread_local! {
    static PARALLEL_SEGMENT_ADMISSION_HELD: Cell<bool> = const { Cell::new(false) };
}

struct ParallelSegmentAdmission {
    keys: Vec<i32>,
}

impl ParallelSegmentAdmission {
    fn try_acquire(requested: usize) -> Option<Self> {
        const NAMESPACE: i32 = 0x5047_5053;
        let nested = PARALLEL_SEGMENT_ADMISSION_HELD.with(|held| held.replace(true));
        if nested {
            return None;
        }
        let mut keys = Vec::with_capacity(requested);
        for key in 0..i32::try_from(HNSW_MAX_SEGMENTS).unwrap_or(i32::MAX) {
            let acquired = unsafe {
                pgrx::direct_function_call::<bool>(
                    pg_sys::pg_try_advisory_lock_int4,
                    &[Some(pg_sys::Datum::from(NAMESPACE)), Some(pg_sys::Datum::from(key))],
                )
            }
            .unwrap_or(false);
            if acquired {
                keys.push(key);
                if keys.len() == requested {
                    break;
                }
            }
        }
        if keys.len() < 2 {
            let guard = Self { keys };
            drop(guard);
            return None;
        }
        Some(Self { keys })
    }
}

impl Drop for ParallelSegmentAdmission {
    fn drop(&mut self) {
        const NAMESPACE: i32 = 0x5047_5053;
        for key in self.keys.drain(..) {
            let _ = unsafe {
                pgrx::direct_function_call::<bool>(
                    pg_sys::pg_advisory_unlock_int4,
                    &[Some(pg_sys::Datum::from(NAMESPACE)), Some(pg_sys::Datum::from(key))],
                )
            };
        }
        PARALLEL_SEGMENT_ADMISSION_HELD.with(|held| held.set(false));
    }
}

fn projected_parallel_segment_bytes(meta: HnswMetaPage) -> Option<u64> {
    meta.segments().iter().try_fold(0_u64, |total, segment| {
        total.checked_add(projected_packed_segment_bytes(meta, *segment)?)
    })
}

fn projected_packed_segment_bytes(
    meta: HnswMetaPage,
    segment: HnswSegmentMeta,
) -> Option<u64> {
    let dimensions = u64::from(meta.dimensions);
    let vector_bytes = dimensions.checked_mul(size_of::<f32>() as u64)?;
    let layer_zero_links = u64::from(meta.hnsw_m)
        .checked_mul(2)?
        .checked_mul(size_of::<HnswNodeId>() as u64)?;
    let per_node_floor = vector_bytes
        .checked_add(size_of::<PackedHnswNode>() as u64)?
        .checked_add(size_of::<PackedHnswLayer>() as u64)?
        .checked_add(layer_zero_links)?;
    let packed_floor = segment.graph_nodes.checked_mul(per_node_floor)?;
    let extent_blocks = segment.end_block.checked_sub(segment.start_block)?;
    let extent_bytes = extent_blocks.checked_mul(pg_sys::BLCKSZ as u64)?;
    if meta.quantization_mode == options::HNSW_QUANTIZATION_NONE_U16 {
        return extent_bytes
            .checked_mul(2)?
            .checked_add(packed_floor.checked_mul(2)?);
    }
    let codec = projected_hnsw_codec_bytes(meta, segment.graph_nodes)?;
    let training_sample_rows = segment
        .graph_nodes
        .min(HNSW_CODEC_TRAINING_SAMPLE_ROWS as u64);
    let training_sample = training_sample_rows.checked_mul(vector_bytes)?;
    // Page-item copies, BTreeMap/BTreeSet nodes, decoded records and nested
    // adjacency vectors can coexist with the final packed graph and its
    // encoded publication image. Charge four full persisted extents for the
    // decoded/container side and three packed floors for the final arrays,
    // publication image, and image-conversion scratch. Codec rows/codebooks
    // can likewise coexist with their serialized artifact. These factors are
    // deliberately conservative; admission must bound transient peak, not
    // merely the final retained graph.
    extent_bytes
        .checked_mul(4)?
        .checked_add(packed_floor.checked_mul(3)?)?
        .checked_add(codec.checked_mul(2)?)?
        .checked_add(training_sample)
}

fn projected_hnsw_codec_bytes(meta: HnswMetaPage, node_count: u64) -> Option<u64> {
    let dimensions = u64::from(meta.dimensions);
    let (code_width, contribution_count, codebook_values) = match meta.quantization_mode {
        options::HNSW_QUANTIZATION_NONE_U16 => return Some(0),
        options::HNSW_QUANTIZATION_BINARY_U16 => {
            (dimensions.checked_add(7)?.checked_div(8)?, dimensions.checked_add(7)?.checked_div(8)?.checked_mul(256)?, 0)
        }
        options::HNSW_QUANTIZATION_SCALAR_U16 | options::HNSW_QUANTIZATION_SQ8_U16 => (
            dimensions,
            dimensions.checked_mul(u64::from(meta.scalar_levels))?,
            0,
        ),
        options::HNSW_QUANTIZATION_PQ_U16 => {
            let subvector = u64::from(meta.pq_subvector_dimensions);
            if subvector == 0 || !dimensions.is_multiple_of(subvector) {
                return None;
            }
            let subvectors = dimensions / subvector;
            (
                subvectors,
                subvectors.checked_mul(256)?,
                dimensions.checked_mul(256)?,
            )
        }
        _ => return None,
    };
    let stride = code_width
        .checked_add(15)?
        .checked_div(16)?
        .checked_mul(16)?;
    let codes = stride.checked_mul(node_count)?;
    let codebook = codebook_values.checked_mul(size_of::<f32>() as u64)?;
    let offsets = code_width
        .checked_add(1)?
        .checked_mul(size_of::<usize>() as u64)?;
    let scorer = contribution_count
        .checked_mul(size_of::<f32>() as u64 * 2)?
        .checked_add(offsets)?;
    codes.checked_add(codebook)?.checked_add(scorer)
}

fn parallel_segment_projection_admitted(
    meta: HnswMetaPage,
    projected_bytes: u64,
    shared_serving_bytes: u64,
    remaining_query_bytes: u64,
    worker_limit: usize,
) -> bool {
    if meta.segments().len() < 3 || worker_limit < 2 {
        return true;
    }
    projected_bytes <= shared_serving_bytes.min(remaining_query_bytes)
}

fn parallel_segment_memory_admitted(
    meta: HnswMetaPage,
    projected_bytes: Option<u64>,
    max_memory_bytes: usize,
) -> bool {
    let worker_limit = crate::settings::hnsw_segment_parallel_workers_from_guc();
    if meta.segments().len() < 3 || worker_limit < 2 {
        return true;
    }
    let serving_budget = crate::settings::hnsw_shared_serving_budget_bytes_from_guc();
    let query_budget = u64::try_from(max_memory_bytes).unwrap_or(u64::MAX);
    let admitted = projected_bytes.is_some_and(|projected_bytes| {
        parallel_segment_projection_admitted(
            meta,
            projected_bytes,
            serving_budget,
            query_budget,
            worker_limit,
        )
    });
    if !admitted {
        record_hnsw_parallel_admission_denial();
    }
    admitted
}

unsafe fn try_parallel_segment_search(
    index_relation: pg_sys::Relation,
    meta: HnswMetaPage,
    metric: HnswScoreMetric,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    mask: Option<&CandidateMask>,
    mask_budget: usize,
    comparison_budget: &HnswComparisonBudget,
    max_memory_bytes: usize,
) -> context_index::Result<Option<ParallelSegmentOutcome>> {
    let worker_limit = crate::settings::hnsw_segment_parallel_workers_from_guc();
    if meta.segments().len() < 3 || worker_limit < 2 {
        return Ok(None);
    }
    let serving_budget = crate::settings::hnsw_shared_serving_budget_bytes_from_guc()
        .min(u64::try_from(max_memory_bytes).unwrap_or(u64::MAX));
    let mut graphs = Vec::with_capacity(meta.segments().len());
    let mut packed_bytes = 0_u64;
    for segment in meta.segments() {
        let mut backend_read = PgHnswGraphRead::for_segment(index_relation, *segment);
        // SAFETY: all PostgreSQL-backed loading completes in this backend
        // before the returned pure owned adapter is placed in a worker task.
        let Some(graph) = (unsafe { backend_read.parallel_local_read()? }) else {
            record_hnsw_parallel_admission_denial();
            return Ok(None);
        };
        let Some(next_packed_bytes) = packed_bytes.checked_add(graph.graph.byte_size()) else {
            record_hnsw_parallel_admission_denial();
            return Ok(None);
        };
        if next_packed_bytes > serving_budget {
            record_hnsw_parallel_admission_denial();
            return Ok(None);
        }
        packed_bytes = next_packed_bytes;
        graphs.push(graph);
    }
    record_hnsw_generation_bytes(packed_bytes);

    let requested_workers = worker_limit.min(graphs.len());
    let Some(admission) = ParallelSegmentAdmission::try_acquire(requested_workers) else {
        record_hnsw_parallel_admission_denial();
        return Ok(None);
    };
    let worker_count = admission.keys.len();
    let Some(pool) = SEGMENT_SEARCH_POOL.get_or_init(SegmentSearchPool::start) else {
        record_hnsw_parallel_admission_denial();
        return Ok(None);
    };
    let mut tasks: Vec<Vec<(usize, ParallelPackedGraphRead)>> =
        (0..worker_count).map(|_| Vec::new()).collect();
    for (index, graph) in graphs.into_iter().enumerate() {
        tasks[index % worker_count].push((index, graph));
    }
    let query = Arc::new(query.clone());
    let mask = mask.cloned().map(Arc::new);
    let cancelled = Arc::new(AtomicBool::new(false));
    let comparison_budget = comparison_budget.clone();
    let (result_sender, result_receiver) = mpsc::channel();
    for task in tasks {
        let query = Arc::clone(&query);
        let mask = mask.as_ref().map(Arc::clone);
        let task_cancelled = Arc::clone(&cancelled);
        let comparison_budget = comparison_budget.clone();
        let result_sender = result_sender.clone();
        let submitted = pool.try_submit(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut combined = ParallelSegmentOutcome::default();
                    for (segment_index, mut graph) in task {
                        let mut cancellation = ParallelSegmentCancellation {
                            cancelled: Arc::clone(&task_cancelled),
                        };
                        let outcome = if let Some(mask) = mask.as_deref() {
                            search_graph_read_with_mask_and_comparison_budget(
                                &mut graph,
                                metric.navigation_metric(),
                                &query,
                                config,
                                limit,
                                mask,
                                mask_budget,
                                &comparison_budget,
                                &mut cancellation,
                            )
                        } else {
                            search_graph_read_with_comparison_budget(
                                &mut graph,
                                metric.navigation_metric(),
                                &query,
                                config,
                                limit,
                                &comparison_budget,
                                &mut cancellation,
                            )
                        };
                        let outcome = match outcome {
                            Ok(outcome) => outcome,
                            Err(error) => {
                                task_cancelled.store(true, Ordering::Relaxed);
                                return Err(error);
                            }
                        };
                        combined.hits.extend(
                            outcome
                                .results()
                                .iter()
                                .filter(|result| {
                                    !hnsw_point_id_is_tombstoned(result.point_id())
                                })
                                .map(|result| SegmentDeltaHit {
                                    segment_index,
                                    hit: DeltaHit {
                                        heap_tid: result.point_id().get(),
                                        score: result.score(),
                                    },
                                }),
                        );
                        combined.node_reads =
                            combined.node_reads.saturating_add(graph.node_reads);
                    }
                    Ok(combined)
            }))
            .unwrap_or_else(|_| {
                task_cancelled.store(true, Ordering::Relaxed);
                Err(HnswError::GraphRead(
                    context_index::GraphError::AdapterFailure {
                        operation: "parallel segment search",
                        message: "backend-local worker panicked".to_owned(),
                    },
                ))
            });
            let _ = result_sender.send(result);
        }));
        if !submitted {
            cancelled.store(true, Ordering::Relaxed);
            record_hnsw_parallel_admission_denial();
            return Ok(None);
        }
    }
    drop(result_sender);
    let mut combined = ParallelSegmentOutcome::default();
    let mut completed = 0;
    let mut first_error = None;
    let mut interrupted = false;
    while completed < worker_count {
        let result = match result_receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if unsafe { pg_sys::InterruptPending != 0 } {
                    cancelled.store(true, Ordering::Relaxed);
                    interrupted = true;
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                cancelled.store(true, Ordering::Relaxed);
                return Err(HnswError::GraphRead(
                    context_index::GraphError::AdapterFailure {
                        operation: "parallel segment search",
                        message: "backend-local worker pool disconnected".to_owned(),
                    },
                ));
            }
        };
        match result {
            Ok(result) if first_error.is_none() => {
                combined.hits.extend(result.hits);
                combined.node_reads = combined.node_reads.saturating_add(result.node_reads);
            }
            Ok(_) => {}
            Err(error) => {
                cancelled.store(true, Ordering::Relaxed);
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        completed += 1;
    }
    drop(admission);
    // PostgreSQL interrupt processing may longjmp across Rust frames. Every
    // pure worker result is therefore drained and every session-level
    // admission lock explicitly released before entering ProcessInterrupts.
    if interrupted {
        pg_sys::check_for_interrupts!();
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    record_hnsw_parallel_segment_scan();
    Ok(Some(combined))
}

unsafe fn hnsw_page_graph_scan_candidates(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
    query: &DenseVector,
    config: HnswConfig,
    requested_limit: usize,
    comparison_budget: &HnswComparisonBudget,
    max_memory_bytes: usize,
) -> HnswScanCandidates {
    // Read the retirement overlay before ANN traversal. Each mutation may
    // invalidate one returned base hit, so this conservative expansion keeps
    // enough successors for the final chronological replay.
    let (meta, overlay) = unsafe {
        read_consistent_hnsw_publication(index_relation, max_memory_bytes, 0)
    };
    let limit = SearchLimit::new(requested_limit).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("invalid persisted HNSW search policy: {error}"),
        )
    });
    let normalized_query;
    let query = if metric == HnswScoreMetric::Cosine {
        let prepared = metric
            .prepare_vector(query.clone())
            .unwrap_or_else(|error| raise_core_error(error))
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "cosine HNSW query vectors must have a finite nonzero norm",
                )
            });
        normalized_query = prepared;
        &normalized_query
    } else {
        query
    };
    // SAFETY: the metapage snapshot owns the immutable segment descriptors.
    record_hnsw_multi_segment_scan(meta.segments().len());
    let mut base_hits = Vec::new();
    let mut page_visits = 0_usize;
    let mut node_reads = 0_usize;
    // SAFETY: parallel admission copies every segment into an owned pure
    // adapter before any worker starts and returns None without publication.
    let parallel_projection = projected_parallel_segment_scan_bytes(
        meta,
        limit.get(),
        config.ef_search(),
        0,
        false,
    );
    let parallel_memory_admitted =
        parallel_segment_memory_admitted(meta, parallel_projection, max_memory_bytes);
    let parallel = if overlay.record_count() == 0 && parallel_memory_admitted {
        unsafe {
            try_parallel_segment_search(
                index_relation,
                meta,
                metric,
                query,
                config,
                limit,
                None,
                0,
                comparison_budget,
                max_memory_bytes,
            )
        }
        .unwrap_or_else(|error| raise_hnsw_scan_error(error))
    } else {
        None
    };
    if let Some(parallel) = parallel {
        base_hits = parallel.hits;
        node_reads = parallel.node_reads;
    } else {
        require_hnsw_scan_memory(
            projected_serial_segment_scan_bytes(
                meta,
                limit.get(),
                config.ef_search(),
                0,
                false,
            ),
            max_memory_bytes,
            "serial traversal",
        );
        if meta.segments().len() > 1 {
            record_hnsw_serial_segment_degradation();
        }
        let mut cancellation = PgHnswCancellation;
        for (segment_index, segment) in meta.segments().iter().enumerate() {
            let mut graph = PgHnswGraphRead::for_segment(index_relation, *segment);
            let retirement = CandidateMask::all()
                .excluding(overlay.retirement_points_after_segment(segment_index));
            let outcome = search_graph_read_with_mask_and_comparison_budget(
                &mut graph,
                metric.navigation_metric(),
                query,
                config,
                limit,
                &retirement,
                usize::MAX,
                comparison_budget,
                &mut cancellation,
            )
            .unwrap_or_else(|error| raise_hnsw_scan_error(error));
            base_hits.extend(
                outcome
                    .results()
                    .iter()
                    .filter(|result| !hnsw_point_id_is_tombstoned(result.point_id()))
                    .map(|result| SegmentDeltaHit {
                        segment_index,
                        hit: DeltaHit {
                            heap_tid: result.point_id().get(),
                            score: result.score(),
                        },
                    }),
            );
            page_visits = page_visits.saturating_add(graph.page_visits);
            node_reads = node_reads.saturating_add(graph.node_reads);
        }
    }
    // SAFETY: this adapter exists only for the active AM callback, which
    // owns a live `index_relation` for the duration of this scan.
    unsafe {
        hnsw_segment_candidates_with_delta_merge(
            index_relation,
            metric,
            query,
            base_hits,
            page_visits,
            node_reads,
            limit.get(),
            None,
            overlay,
            comparison_budget,
        )
    }
}

/// Returns every live base or delta entry for an unordered index scan.
///
/// Delta records are replayed in append order so a later tombstone retires a
/// base/live entry and a later live record replaces an earlier one.
///
/// # Safety
///
/// `index_relation` must remain a live index relation for the complete scan.
unsafe fn hnsw_unordered_scan_candidates_with_delta(
    index_relation: pg_sys::Relation,
) -> Vec<HnswScanCandidate> {
    // SAFETY: The metapage and its published delta boundary belong to the
    // same live relation held by the caller.
    let (meta, overlay) = unsafe {
        read_consistent_hnsw_publication(index_relation, usize::MAX, 0)
    };
    record_hnsw_multi_segment_scan(meta.segments().len());
    if meta.segments().len() > 1 {
        record_hnsw_serial_segment_degradation();
    }
    let mut candidates = BTreeMap::new();
    for (segment_index, segment) in meta.segments().iter().enumerate() {
        let records = unsafe { read_hnsw_segment_records(index_relation, *segment) };
        for candidate in hnsw_unordered_scan_candidates(records) {
            candidates.insert(candidate.heap_tid, candidate);
        }
        for record in &overlay.frozen[segment_index] {
            match record.kind {
                DeltaRecordKind::Live => {
                    candidates.insert(
                        record.heap_tid,
                        HnswScanCandidate {
                            heap_tid: record.heap_tid,
                            score: 0.0,
                        },
                    );
                }
                DeltaRecordKind::Tombstone => {
                    candidates.remove(&record.heap_tid);
                }
            }
        }
    }
    for record in &overlay.active {
        match record.kind {
            DeltaRecordKind::Live => {
                candidates.insert(
                    record.heap_tid,
                    HnswScanCandidate {
                        heap_tid: record.heap_tid,
                        score: 0.0,
                    },
                );
            }
            DeltaRecordKind::Tombstone => {
                candidates.remove(&record.heap_tid);
            }
        }
    }
    if overlay.record_count() > 0 {
        record_hnsw_delta_segment_scan();
    }

    candidates.into_values().collect()
}

unsafe fn hnsw_page_graph_scan_candidates_with_mask(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    mask: &CandidateMask,
    comparison_budget: &HnswComparisonBudget,
    max_memory_bytes: usize,
    mask_point_count: usize,
) -> HnswScanCandidates {
    let normalized_query;
    let query = if metric == HnswScoreMetric::Cosine {
        let prepared = metric
            .prepare_vector(query.clone())
            .unwrap_or_else(|error| raise_core_error(error))
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "cosine HNSW query vectors must have a finite nonzero norm",
                )
            });
        normalized_query = prepared;
        &normalized_query
    } else {
        query
    };
    // SAFETY: the metapage snapshot owns the immutable segment descriptors.
    let retained_mask_bytes = projected_mask_bytes(mask_point_count).unwrap_or(u64::MAX);
    let (meta, overlay) = unsafe {
        read_consistent_hnsw_publication(
            index_relation,
            max_memory_bytes,
            retained_mask_bytes,
        )
    };
    let mask_budget = crate::settings::hnsw_mask_candidate_limit_from_guc();
    mask.validate_budget_with_limit(mask_budget)
        .unwrap_or_else(|error| raise_hnsw_scan_error(error));
    record_hnsw_multi_segment_scan(meta.segments().len());
    let mut base_hits = Vec::new();
    let mut page_visits = 0_usize;
    let mut node_reads = 0_usize;
    // SAFETY: as the unmasked path; the mask is cloned into immutable owned
    // state before the scoped workers start.
    let parallel_projection = projected_parallel_segment_scan_bytes(
        meta,
        limit.get(),
        config.ef_search(),
        mask_point_count,
        true,
    );
    let parallel_memory_admitted =
        parallel_segment_memory_admitted(meta, parallel_projection, max_memory_bytes);
    let parallel = if overlay.record_count() == 0 && parallel_memory_admitted {
        unsafe {
            try_parallel_segment_search(
                index_relation,
                meta,
                metric,
                query,
                config,
                limit,
                Some(mask),
                mask_budget,
                comparison_budget,
                max_memory_bytes,
            )
        }
        .unwrap_or_else(|error| raise_hnsw_scan_error(error))
    } else {
        None
    };
    if let Some(parallel) = parallel {
        base_hits = parallel.hits;
        node_reads = parallel.node_reads;
    } else {
        require_hnsw_scan_memory(
            projected_serial_segment_scan_bytes(
                meta,
                limit.get(),
                config.ef_search(),
                mask_point_count,
                true,
            ),
            max_memory_bytes,
            "filtered serial traversal",
        );
        if meta.segments().len() > 1 {
            record_hnsw_serial_segment_degradation();
        }
        let mut cancellation = PgHnswCancellation;
        for (segment_index, segment) in meta.segments().iter().enumerate() {
            let mut graph = PgHnswGraphRead::for_segment(index_relation, *segment);
            let retirement = mask
                .clone()
                .excluding(overlay.retirement_points_after_segment(segment_index));
            let outcome = search_graph_read_with_mask_and_comparison_budget(
                &mut graph,
                metric.navigation_metric(),
                query,
                config,
                limit,
                &retirement,
                usize::MAX,
                comparison_budget,
                &mut cancellation,
            )
            .unwrap_or_else(|error| raise_hnsw_scan_error(error));
            base_hits.extend(
                outcome
                    .results()
                    .iter()
                    .filter(|result| !hnsw_point_id_is_tombstoned(result.point_id()))
                    .map(|result| SegmentDeltaHit {
                        segment_index,
                        hit: DeltaHit {
                            heap_tid: result.point_id().get(),
                            score: result.score(),
                        },
                    }),
            );
            page_visits = page_visits.saturating_add(graph.page_visits);
            node_reads = node_reads.saturating_add(graph.node_reads);
        }
    }
    // SAFETY: this adapter exists only for the active AM callback, which
    // owns a live `index_relation` for the duration of this scan.
    unsafe {
        hnsw_segment_candidates_with_delta_merge(
            index_relation,
            metric,
            query,
            base_hits,
            page_visits,
            node_reads,
            limit.get(),
            Some(mask),
            overlay,
            comparison_budget,
        )
    }
}

/// Merges base-graph scan results with an exact scan over the segmented
/// delta region, applying delta-based retirement of stale/deleted base
/// candidates. Falls back to the base-only outcome when no delta region is
/// open or the delta is empty, avoiding the decode pass entirely.
unsafe fn hnsw_segment_candidates_with_delta_merge(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
    query: &DenseVector,
    base_hits: Vec<SegmentDeltaHit>,
    page_visits: usize,
    node_reads: usize,
    requested_limit: usize,
    delta_mask: Option<&CandidateMask>,
    overlay: PublishedMutationOverlay,
    comparison_budget: &HnswComparisonBudget,
) -> HnswScanCandidates {
    // SAFETY: this adapter exists only for the active AM callback.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    if meta.segments().len() > 1 {
        record_hnsw_segment_merge(base_hits.len(), 0);
    }
    if overlay.record_count() == 0 {
        return hnsw_scan_candidates_from_segment_hits(
            base_hits.into_iter().map(|hit| hit.hit).collect(), metric, page_visits, node_reads, requested_limit,
        );
    }
    record_hnsw_delta_segment_scan();
    record_hnsw_segment_merge(
        0,
        overlay.frozen.iter().map(Vec::len).sum(),
    );
    let mut by_segment = vec![Vec::new(); meta.segments().len()];
    for candidate in base_hits {
        if let Some(segment) = by_segment.get_mut(candidate.segment_index) {
            segment.push(candidate.hit);
        }
    }
    let mut live = BTreeMap::<u64, f32>::new();
    let mut scored_delta_vectors = 0_usize;
    for (segment_index, mutations) in overlay.frozen.iter().enumerate() {
        for hit in &by_segment[segment_index] {
            live.insert(hit.heap_tid, hit.score);
        }
        for record in mutations {
            apply_hnsw_overlay_record(
                &mut live,
                &mut scored_delta_vectors,
                record,
                metric,
                query,
                delta_mask,
                comparison_budget,
            );
        }
    }
    for record in &overlay.active {
        apply_hnsw_overlay_record(
            &mut live,
            &mut scored_delta_vectors,
            record,
            metric,
            query,
            delta_mask,
            comparison_budget,
        );
    }
    let mut merged = live.into_iter().map(|(heap_tid, score)| DeltaHit { heap_tid, score }).collect::<Vec<_>>();
    merged.sort_by(|left, right| left.score.total_cmp(&right.score).then_with(|| left.heap_tid.cmp(&right.heap_tid)));
    merged.truncate(requested_limit);
    let candidates = merged
        .into_iter()
        .map(|hit| HnswScanCandidate {
            heap_tid: hit.heap_tid,
            score: metric.output_score(hit.score),
        })
        .collect::<Vec<_>>();
    HnswScanCandidates {
        work: HnswScanWork {
            page_visits,
            node_reads: node_reads.saturating_add(scored_delta_vectors),
            candidates: candidates.len(),
            rechecks: 0,
            exact_strategy: false,
        },
        candidates,
        requested_limit,
    }
}

fn apply_hnsw_overlay_record(
    live: &mut BTreeMap<u64, f32>,
    scored: &mut usize,
    record: &context_storage::DeltaRecord,
    metric: HnswScoreMetric,
    query: &DenseVector,
    mask: Option<&CandidateMask>,
    comparison_budget: &HnswComparisonBudget,
) {
    if mask.is_some_and(|mask| !mask.allows(HnswPointId::new(record.heap_tid))) {
        return;
    }
    match record.kind {
        DeltaRecordKind::Live => {
            comparison_budget
                .reserve_comparison()
                .unwrap_or_else(|error| raise_hnsw_scan_error(error));
            let score = metric
                .navigation_metric()
                .distance_slices(query.as_slice(), record.vector.as_slice())
                .unwrap_or_else(|error| raise_core_error(error));
            live.insert(record.heap_tid, score);
            *scored = scored.saturating_add(1);
        }
        DeltaRecordKind::Tombstone => {
            live.remove(&record.heap_tid);
        }
    }
}


fn raise_hnsw_scan_error(error: HnswError) -> ! {
    let code = match &error {
        HnswError::DimensionMismatch { .. } => PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        HnswError::ComparisonBudgetExceeded { .. } => {
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED
        }
        _ => PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
    };
    raise_sql_error(
        code,
        format!("failed to search persisted HNSW pages: {error}"),
    )
}

fn hnsw_scan_candidates_from_segment_hits(
    hits: Vec<DeltaHit>,
    metric: HnswScoreMetric,
    page_visits: usize,
    node_reads: usize,
    requested_limit: usize,
) -> HnswScanCandidates {
    let mut best_by_tid = BTreeMap::<u64, f32>::new();
    for hit in hits {
        best_by_tid
            .entry(hit.heap_tid)
            .and_modify(|score| {
                if hit.score.total_cmp(score).is_lt() {
                    *score = hit.score;
                }
            })
            .or_insert(hit.score);
    }
    let mut ranked = best_by_tid.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.truncate(requested_limit);
    let candidates = ranked
        .into_iter()
        .map(|(heap_tid, score)| HnswScanCandidate {
            heap_tid,
            score: metric.output_score(score),
        })
        .collect::<Vec<_>>();
    HnswScanCandidates {
        work: HnswScanWork {
            page_visits,
            node_reads,
            candidates: candidates.len(),
            rechecks: 0,
            exact_strategy: false,
        },
        candidates,
        requested_limit,
    }
}

unsafe fn hnsw_stored_config(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
) -> HnswConfig {
    let runtime = hnsw_config_from_gucs();
    // SAFETY: The caller owns a live index relation for the current callback.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    meta.stored_config(metric, runtime.ef_search())
}

#[cfg(any(test, feature = "pg_test"))]
unsafe fn hnsw_stored_entry_point(
    index_relation: pg_sys::Relation,
) -> Option<HnswNodeId> {
    // SAFETY: The caller owns a live index relation for the current callback.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    if meta.entry_node_id == u64::MAX {
        return None;
    }
    if meta.entry_node_id >= meta.graph_nodes {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW stored entry point lies outside the published graph",
        );
    }
    let entry = usize::try_from(meta.entry_node_id).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW stored entry point exceeds platform range",
        )
    });
    Some(HnswNodeId::new(entry))
}

unsafe fn initialize_hnsw_data_page(
    page: pg_sys::Page,
    page_id: u64,
    kind: GraphPageKind,
    generation: u64,
) {
    // SAFETY: The caller holds an exclusive buffer lock on a fresh or zeroed
    // vector-record page. HNSW vector pages do not reserve special space.
    unsafe { pg_sys::PageInit(page, pg_sys::BLCKSZ as pg_sys::Size, 0) };
    let header = encode_page_header(PageHeaderV2 {
        kind,
        generation,
        page_id: GraphPageId::new(page_id).get(),
    })
    .unwrap_or_else(|error| {
        raise_sql_error(PgSqlErrorCode::ERRCODE_INTERNAL_ERROR, error.to_string())
    });
    // SAFETY: The new page is exclusively locked and the fixed-size header
    // remains borrowed only for PostgreSQL's immediate item copy.
    let offset = unsafe {
        pg_sys::PageAddItemExtended(
            page,
            header.as_ptr().cast_mut().cast(),
            header.len() as pg_sys::Size,
            HNSW_FIRST_OFFSET,
            0,
        )
    };
    if offset != HNSW_FIRST_OFFSET {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to initialize typed HNSW vector page header",
        );
    }
}

unsafe fn find_last_hnsw_page(
    index_relation: pg_sys::Relation,
    kind: GraphPageKind,
) -> Option<pg_sys::BlockNumber> {
    // SAFETY: The caller supplies a live index relation for the duration of
    // this bounded page search.
    let block_count = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    // Never append into a region a compaction superseded. Compacting an index
    // whose rows were all deleted writes no node pages at all, so the search
    // below would otherwise walk back into the old base and extend a page no
    // read will ever visit again, silently dropping the record.
    // SAFETY: The caller supplies a live index relation whose metapage was
    // initialized before any typed page could be appended.
    let base_start = unsafe { PgHnswGraphRead::new(index_relation).meta() }.base_scan_start();
    let first_block = block_number_from_u64(base_start, "HNSW base start block");
    for block_number in (first_block..block_count).rev() {
        // SAFETY: The block number is within the live main-fork block count.
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                block_number,
                pg_sys::ReadBufferMode::RBM_NORMAL,
                ptr::null_mut(),
            )
        };
        // SAFETY: The buffer is pinned by ReadBufferExtended; this block owns
        // its share lock and releases it before continuing or returning.
        let header_item = unsafe {
            pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed());
            let page = pg_sys::BufferGetPage(buffer);
            let header_item = if pg_sys::PageIsNew(page)
                || pg_sys::PageGetMaxOffsetNumber(page) < HNSW_FIRST_OFFSET
            {
                None
            } else {
                match copy_hnsw_page_item(page, HNSW_FIRST_OFFSET) {
                    Ok(item) => Some(item),
                    Err(error) => {
                        pg_sys::UnlockReleaseBuffer(buffer);
                        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error);
                    }
                }
            };
            pg_sys::UnlockReleaseBuffer(buffer);
            header_item
        };
        let matches = match header_item {
            None => false,
            Some(item) => match decode_page_header(&item) {
                Ok(header) => {
                    header.kind == kind && header.page_id == u64::from(block_number)
                }
                Err(error) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    error.to_string(),
                ),
            },
        };
        if matches {
            return Some(block_number);
        }
    }
    None
}

unsafe fn update_hnsw_metapage<F>(index_relation: pg_sys::Relation, update: F)
where
    F: FnOnce(&mut HnswMetaPage),
{
    // SAFETY: The caller passes a valid index relation owned by PostgreSQL.
    unsafe { ensure_hnsw_metapage(index_relation) };
    // SAFETY: The caller passes a valid index relation and block zero is the
    // initialized HNSW metapage.
    let buffer = unsafe {
        pg_sys::ReadBufferExtended(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
            0,
            pg_sys::ReadBufferMode::RBM_NORMAL,
            ptr::null_mut(),
        )
    };
    // SAFETY: The buffer is pinned and locked exclusively. Generic WAL gives
    // this callback a private registered-page image; only that image is
    // mutated, then GenericXLogFinish atomically installs and logs it before
    // the original buffer is released.
    unsafe {
        pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_EXCLUSIVE.cast_signed());
        let state = pg_sys::GenericXLogStart(index_relation);
        let registered =
            wal_contract::critical_section::HnswWalRegisteredSinglePage::register(
            state,
            buffer,
            pg_sys::GENERIC_XLOG_FULL_IMAGE.cast_signed(),
        );
        let page = registered.page();
        hnsw_physical_failpoint(7, "before_metapage_publication");
        let mut meta = match read_hnsw_meta_page(page) {
            Ok(Some(meta)) => meta,
            Ok(None) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "HNSW metapage is missing",
            ),
            Err(error) => raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error),
        };
        update(&mut meta);
        write_hnsw_meta_page(page, meta);
        let finish_permit = registered.seal();
        finish_permit.finish();
        pg_sys::UnlockReleaseBuffer(buffer);
        hnsw_physical_failpoint(8, "after_metapage_publication");
    }
}

unsafe fn read_hnsw_meta_page(
    page: pg_sys::Page,
) -> Result<Option<HnswMetaPage>, String> {
    // SAFETY: `page` is a valid PostgreSQL page pointer from a pinned buffer.
    if unsafe { pg_sys::PageIsNew(page) } {
        return Ok(None);
    }
    // SAFETY: The page is initialized and can be inspected for line pointers.
    let max_offset = unsafe { pg_sys::PageGetMaxOffsetNumber(page) };
    if max_offset < HNSW_FIRST_OFFSET {
        return Ok(None);
    }
    // SAFETY: the page is pinned and the checked helper validates the complete
    // line pointer and item span before copying it into Rust-owned bytes.
    let item = unsafe { copy_hnsw_page_item(page, HNSW_FIRST_OFFSET)? };
    if item.len() != size_of::<HnswMetaPage>() {
        if item.len() >= 6 {
            let magic = u32::from_ne_bytes(item[0..4].try_into().unwrap_or([0; 4]));
            let version = u16::from_ne_bytes(item[4..6].try_into().unwrap_or([0; 2]));
            if magic == HNSW_META_MAGIC && version != HNSW_META_VERSION {
                return Err(format!(
                    "HNSW metapage format {version} requires REINDEX for format {}",
                    HNSW_META_VERSION
                ));
            }
        }
        return Err("HNSW metapage item has an unexpected length".to_owned());
    }
    // SAFETY: the owned item has exactly the size of `HnswMetaPage`; unaligned
    // reads avoid assuming item payload alignment.
    let meta = unsafe { ptr::read_unaligned(item.as_ptr().cast::<HnswMetaPage>()) };
    if !meta.is_valid() {
        return Err("HNSW metapage metadata is invalid".to_owned());
    }
    Ok(Some(meta))
}

unsafe fn write_hnsw_meta_page(page: pg_sys::Page, meta: HnswMetaPage) {
    // SAFETY: `page` is a valid PostgreSQL page pointer from an exclusive buffer lock.
    if unsafe { pg_sys::PageIsNew(page) } {
        // SAFETY: The caller owns the exclusive lock and may initialize a new page.
        unsafe { pg_sys::PageInit(page, pg_sys::BLCKSZ as pg_sys::Size, 0) };
    }
    // SAFETY: The page is initialized before line pointer inspection.
    let max_offset = unsafe { pg_sys::PageGetMaxOffsetNumber(page) };
    if max_offset < HNSW_FIRST_OFFSET {
        // SAFETY: The page is initialized and exclusively locked; `meta` lives
        // for the duration of the copy performed by `PageAddItemExtended`.
        let offset = unsafe {
            pg_sys::PageAddItemExtended(
                page,
                ptr::addr_of!(meta).cast_mut().cast(),
                size_of::<HnswMetaPage>() as pg_sys::Size,
                HNSW_FIRST_OFFSET,
                0,
            )
        };
        if offset == HNSW_INVALID_OFFSET {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "failed to initialize HNSW metapage item",
            );
        }
        return;
    }

    // SAFETY: the caller holds the page buffer exclusively and the checked
    // helper validates the complete line pointer and writable item span.
    let (item, item_len) = unsafe { checked_hnsw_page_item_span(page, HNSW_FIRST_OFFSET) }
        .unwrap_or_else(|error| {
            raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
        });
    if item_len != size_of::<HnswMetaPage>() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!(
                "HNSW metapage item has unexpected length: expected {} bytes, got {item_len}",
                size_of::<HnswMetaPage>()
            ),
        );
    }
    // SAFETY: The caller holds the page buffer exclusively, and the existing
    // checked metapage item has exactly the size of `HnswMetaPage`.
    unsafe {
        ptr::copy_nonoverlapping(
            ptr::addr_of!(meta).cast::<u8>(),
            item,
            size_of::<HnswMetaPage>(),
        );
    }
}
