// Graph-traversal scan fragment included by `hnsw_am.rs`: the `GraphRead`
// impl over `PgHnswGraphRead` and the persisted-page HNSW scan entry
// points used by the AM callbacks and the masked SQL path.

impl GraphRead for PgHnswGraphRead {
    fn metadata(&mut self) -> context_index::GraphResult<GraphMetadata> {
        // SAFETY: this adapter exists only for the active AM callback.
        let meta = unsafe { self.meta() };
        let node_count = usize::try_from(meta.graph_nodes).map_err(|_| context_index::GraphError::CapacityExceeded {
            operation: "HNSW metapage node count",
        })?;
        let entry = if meta.entry_node_id == u64::MAX {
            None
        } else {
            Some(HnswNodeId::new(
                usize::try_from(meta.entry_node_id).map_err(|_| {
                    context_index::GraphError::CapacityExceeded {
                        operation: "HNSW metapage entry node",
                    }
                })?,
            ))
        };
        GraphMetadata::new(node_count, entry, (meta.dimensions != 0).then_some(meta.dimensions as usize))
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

unsafe fn hnsw_page_graph_scan_candidates(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
    query: &DenseVector,
    config: HnswConfig,
    requested_limit: usize,
) -> HnswScanCandidates {
    let limit = SearchLimit::new(requested_limit).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("invalid persisted HNSW ef_search policy: {error}"),
        )
    });
    let normalized_query;
    let query = if metric == HnswScoreMetric::Cosine {
        normalized_query = metric
            .prepare_vector(query.clone())
            .unwrap_or_else(|error| raise_core_error(error));
        &normalized_query
    } else {
        query
    };
    let mut graph = PgHnswGraphRead::new(index_relation);
    let mut cancellation = PgHnswCancellation;
    let outcome = search_graph_read(
        &mut graph,
        metric.navigation_metric(),
        query,
        config,
        limit,
        &mut cancellation,
    )
    .unwrap_or_else(|error| raise_hnsw_scan_error(error));
    // SAFETY: this adapter exists only for the active AM callback, which
    // owns a live `index_relation` for the duration of this scan.
    unsafe {
        hnsw_scan_candidates_with_delta_merge(
            index_relation,
            metric,
            query,
            outcome,
            graph.page_visits,
            graph.node_reads,
            limit.get(),
        )
    }
}

unsafe fn hnsw_page_graph_scan_candidates_with_mask(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    mask: &CandidateMask,
) -> HnswScanCandidates {
    let normalized_query;
    let query = if metric == HnswScoreMetric::Cosine {
        normalized_query = metric
            .prepare_vector(query.clone())
            .unwrap_or_else(|error| raise_core_error(error));
        &normalized_query
    } else {
        query
    };
    let mut graph = PgHnswGraphRead::new(index_relation);
    let mut cancellation = PgHnswCancellation;
    let mask_budget = crate::settings::hnsw_mask_candidate_limit_from_guc();
    let outcome = search_graph_read_with_mask_budgeted(
        &mut graph,
        metric.navigation_metric(),
        query,
        config,
        limit,
        mask,
        mask_budget,
        &mut cancellation,
    )
    .unwrap_or_else(|error| raise_hnsw_scan_error(error));
    // SAFETY: this adapter exists only for the active AM callback, which
    // owns a live `index_relation` for the duration of this scan.
    unsafe {
        hnsw_scan_candidates_with_delta_merge(
            index_relation,
            metric,
            query,
            outcome,
            graph.page_visits,
            graph.node_reads,
            limit.get(),
        )
    }
}

/// Merges base-graph scan results with an exact scan over the segmented
/// delta region, applying delta-based retirement of stale/deleted base
/// candidates. Falls back to the base-only outcome when no delta region is
/// open or the delta is empty, avoiding the decode pass entirely.
unsafe fn hnsw_scan_candidates_with_delta_merge(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
    query: &DenseVector,
    outcome: HnswSearchOutcome,
    page_visits: usize,
    node_reads: usize,
    requested_limit: usize,
) -> HnswScanCandidates {
    // SAFETY: this adapter exists only for the active AM callback.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    if meta.delta_start_block == u64::MAX {
        return hnsw_scan_candidates_from_outcome(
            outcome,
            metric,
            page_visits,
            node_reads,
            requested_limit,
        );
    }
    // SAFETY: `delta_start_block` came from the metapage read above and the
    // caller holds whatever lock the active AM callback requires.
    let delta_records = unsafe { read_hnsw_delta_records(index_relation, meta.delta_start_block) };
    if delta_records.is_empty() {
        return hnsw_scan_candidates_from_outcome(
            outcome,
            metric,
            page_visits,
            node_reads,
            requested_limit,
        );
    }
    record_hnsw_delta_segment_scan();
    let entries = delta_records.iter().map(|record| match record.kind {
        DeltaRecordKind::Live => DeltaScanEntry::Live {
            heap_tid: record.heap_tid,
            vector: record.vector.as_slice(),
        },
        DeltaRecordKind::Tombstone => DeltaScanEntry::Tombstone {
            heap_tid: record.heap_tid,
        },
    });
    let delta_outcome = scan_delta_topk(
        entries,
        metric.navigation_metric(),
        query.as_slice(),
        requested_limit,
    )
    .unwrap_or_else(|error| raise_hnsw_scan_error(error));
    let base_hits = outcome
        .results()
        .iter()
        .filter(|result| !hnsw_point_id_is_tombstoned(result.point_id()))
        .map(|result| DeltaHit {
            heap_tid: result.point_id().get(),
            score: result.score(),
        });
    let merged = merge_topk(base_hits, &delta_outcome, requested_limit);
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
            node_reads,
            candidates: candidates.len(),
            rechecks: 0,
            exact_strategy: false,
        },
        candidates,
        requested_limit,
    }
}

fn raise_hnsw_scan_error(error: HnswError) -> ! {
    let code = match error {
        HnswError::DimensionMismatch { .. } => PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        _ => PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
    };
    raise_sql_error(
        code,
        format!("failed to search persisted HNSW pages: {error}"),
    )
}

fn hnsw_scan_candidates_from_outcome(
    outcome: HnswSearchOutcome,
    metric: HnswScoreMetric,
    page_visits: usize,
    node_reads: usize,
    requested_limit: usize,
) -> HnswScanCandidates {
    let candidates = outcome
        .results()
        .iter()
        .filter(|result| !hnsw_point_id_is_tombstoned(result.point_id()))
        .map(|result| HnswScanCandidate {
            heap_tid: result.point_id().get(),
            score: metric.output_score(result.score()),
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
) -> Result<Option<HnswMetaPage>, &'static str> {
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
        return Err("HNSW metapage item has an unexpected length");
    }
    // SAFETY: the owned item has exactly the size of `HnswMetaPage`; unaligned
    // reads avoid assuming item payload alignment.
    let meta = unsafe { ptr::read_unaligned(item.as_ptr().cast::<HnswMetaPage>()) };
    if !meta.is_valid() {
        return Err("HNSW metapage metadata is invalid");
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
