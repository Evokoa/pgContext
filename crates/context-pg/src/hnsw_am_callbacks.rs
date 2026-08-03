// Safe callback bodies included by `hnsw_am.rs`. Raw PostgreSQL entrypoints
// validate callback-local capabilities before delegating to these functions.

fn hnsw_build_safe(
    heap_relation: PgCallbackRef<'_, pg_sys::RelationData>,
    index_relation: PgCallbackRef<'_, pg_sys::RelationData>,
    index_info: PgCallbackRef<'_, pg_sys::IndexInfo>,
) -> *mut pg_sys::IndexBuildResult {
    crate::pgvector_compat::nudge_pgvector_compat(index_relation.as_ptr());
    // SAFETY: PostgreSQL passes a valid initialized index relation.
    let score_metric = unsafe { hnsw_score_metric(index_relation.as_ptr()) };
    let config = hnsw_config_from_gucs();
    let parallel_workers = crate::settings::hnsw_build_parallel_workers_from_guc();
    let mut state = HnswBuildState::new(score_metric, config, parallel_workers);
    // SAFETY: PostgreSQL passes a valid index relation for the build callback,
    // and `rd_options` has the layout returned by this AM's options callback.
    let quantization_metadata =
        unsafe { options::hnsw_quantization_metadata(index_relation.as_ptr()) };
    ensure_hnsw_quantized_metric_supported(score_metric, quantization_metadata);
    // SAFETY: PostgreSQL passes a valid index relation for the build callback.
    unsafe { ensure_hnsw_metapage(index_relation.as_ptr()) };
    let graph_started = std::time::Instant::now();
    // SAFETY: PostgreSQL invokes AM build callbacks with valid heap/index
    // relation pointers and build metadata. The callback copies vector values
    // into Rust-owned graph state and does not retain tuple pointers.
    let heap_tuples = unsafe {
        pg_sys::table_index_build_scan(
            heap_relation.as_ptr(),
            index_relation.as_ptr(),
            index_info.as_ptr(),
            true,
            true,
            Some(pgcontext_hnsw_build_callback),
            ptr::addr_of_mut!(state).cast::<c_void>(),
            ptr::null_mut(),
        )
    };
    state.finish_parallel_build();
    let graph_millis = saturating_elapsed_millis(graph_started);
    state.enforce_maintenance_work_mem();
    if let Some(dimensions) = state.dimensions {
        ensure_hnsw_quantized_dimensions_supported(quantization_metadata, dimensions);
    }
    let write_started = std::time::Instant::now();
    // SAFETY: PostgreSQL passes a valid index relation for the build callback.
    unsafe {
        update_hnsw_metapage(index_relation.as_ptr(), |meta| {
            meta.record_index_identity(score_metric, config);
            meta.record_quantization(quantization_metadata);
            meta.record_build(
                state.dimensions,
                state.index_tuples,
                state.graph.entry_point(),
            );
        })
    };
    let source_rows = usize::try_from(state.index_tuples).unwrap_or(usize::MAX);
    let minimum_rows = source_rows.div_ceil(HNSW_MAX_SEGMENTS);
    let segment_rows = HNSW_TARGET_SEGMENT_ROWS.max(minimum_rows).max(1);
    let original_entry = state.graph.entry_point();
    let source_snapshots = state.graph.into_node_snapshots();
    let build_meta = unsafe { PgHnswGraphRead::new(index_relation.as_ptr()).meta() };
    let build_generation = build_meta.page_generation();
    let mut published_segments = Vec::with_capacity(source_rows.div_ceil(segment_rows));
    let mut rows = source_snapshots.into_iter();
    for index in 0..HNSW_MAX_SEGMENTS {
        let chunk = rows.by_ref().take(segment_rows).collect::<Vec<_>>();
        if chunk.is_empty() {
            break;
        }
        let (snapshots, entry_point) = if source_rows <= segment_rows {
            (chunk, original_entry)
        } else {
            let builder = ConcurrentHnswBuilder::new(
                score_metric.navigation_metric(),
                config,
                chunk.len(),
            );
            for row in chunk {
                let (point_id, vector) = row.into_point();
                builder
                    .insert(point_id, vector)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            format!("failed to build independent HNSW segment: {error}"),
                        )
                    });
            }
            let graph = builder.finish().unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("failed to finalize independent HNSW segment: {error}"),
                )
            });
            let entry_point = graph.entry_point();
            (graph.into_node_snapshots(), entry_point)
        };
        let generation =
            build_generation.saturating_add(u64::try_from(index).unwrap_or(u64::MAX));
        let start_block = u64::from(unsafe {
            pg_sys::RelationGetNumberOfBlocksInFork(
                index_relation.as_ptr(),
                pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        });
        if !snapshots.is_empty() {
            // SAFETY: the build owns the live relation and every independent
            // graph snapshot until the synchronous append completes.
            unsafe {
                write_hnsw_node_revisions_bulk(index_relation.as_ptr(), &snapshots, generation)
            };
        }
        let end_block = u64::from(unsafe {
            pg_sys::RelationGetNumberOfBlocksInFork(
                index_relation.as_ptr(),
                pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        });
        published_segments.push((
            generation,
            start_block,
            end_block,
            graph_node_count(&snapshots),
            entry_point,
        ));
    }
    // SAFETY: PostgreSQL passes a valid index relation for the build callback;
    // the base graph is fully written above, so the current block count marks
    // where the segmented-write delta region begins.
    let post_build_block_count = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation.as_ptr(), pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    // SAFETY: PostgreSQL passes a valid index relation for the build callback.
    unsafe {
        update_hnsw_metapage(index_relation.as_ptr(), |meta| {
            if let Some((_, start, end, nodes, entry)) = published_segments.first().copied() {
                meta.publish_single_segment(start, end, nodes, entry);
                for (generation, start, end, nodes, entry) in
                    published_segments.iter().copied().skip(1)
                {
                    meta.publish_additional_segment(
                        generation,
                        start,
                        end,
                        nodes,
                        entry,
                        u64::MAX,
                        u64::MAX,
                        0,
                    );
                }
            }
            meta.open_delta_region(post_build_block_count);
        })
    };
    let profile = HnswBuildProfile {
        tuples: state.index_tuples,
        graph_millis,
        write_millis: saturating_elapsed_millis(write_started),
    };
    pgrx::debug1!(
        "pgcontext HNSW build: {} tuples, graph {} ms, write {} ms",
        profile.tuples,
        profile.graph_millis,
        profile.write_millis,
    );
    record_hnsw_build_profile(profile);

    build_result(heap_tuples, u64_to_pg_estimate_f64(state.index_tuples))
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL owns the relation pointer passed to this callback. Empty
// index initialization creates the one-page physical base expected by later
// insert maintenance.
unsafe extern "C-unwind" fn pgcontext_hnsw_build_empty(index_relation: pg_sys::Relation) {
    // SAFETY: This scope is stack-bound to the guarded empty-build callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL supplies a live exclusively writable index relation
    // for this guarded callback and retains ownership for the call.
    let index_relation = unsafe { scope.borrow(index_relation, "index relation") };
    self::hnsw_build_empty_safe(index_relation);
}

fn hnsw_build_empty_safe(index_relation: PgCallbackRef<'_, pg_sys::RelationData>) {
    // SAFETY: PostgreSQL passes a valid index relation for the empty-build
    // callback, and `rd_options` has the layout returned by this AM's options
    // callback.
    let quantization_metadata =
        unsafe { options::hnsw_quantization_metadata(index_relation.as_ptr()) };
    // SAFETY: PostgreSQL passes a valid initialized index relation.
    let score_metric = unsafe { hnsw_score_metric(index_relation.as_ptr()) };
    ensure_hnsw_quantized_metric_supported(score_metric, quantization_metadata);
    let config = hnsw_config_from_gucs();
    // SAFETY: PostgreSQL passes a valid index relation for the empty-build
    // callback.
    unsafe { ensure_hnsw_metapage(index_relation.as_ptr()) };
    // SAFETY: PostgreSQL passes a valid index relation for the empty-build
    // callback; the base graph is empty (no node/adjacency pages), so the
    // current block count marks where the segmented-write delta region
    // begins.
    let post_build_block_count = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation.as_ptr(), pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    // SAFETY: PostgreSQL passes a valid index relation for the empty-build
    // callback and block zero is the initialized HNSW metapage.
    unsafe {
        update_hnsw_metapage(index_relation.as_ptr(), |meta| {
            meta.record_index_identity(score_metric, config);
            meta.record_quantization(quantization_metadata);
            meta.record_build(None, 0, None);
            meta.open_delta_region(post_build_block_count);
        })
    };
}

fn ensure_hnsw_quantized_metric_supported(
    metric: HnswScoreMetric,
    metadata: options::HnswQuantizationMetadata,
) {
    if metadata.mode != options::HNSW_QUANTIZATION_NONE_U16
        && matches!(metric, HnswScoreMetric::BitHamming | HnswScoreMetric::BitJaccard)
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "quantized pgcontext_hnsw indexes do not support bitvec Hamming or Jaccard opclasses",
        );
    }
}

fn ensure_hnsw_quantized_dimensions_supported(
    metadata: options::HnswQuantizationMetadata,
    dimensions: u32,
) {
    if metadata.mode == options::HNSW_QUANTIZATION_PQ_U16
        && (metadata.pq_subvector_dimensions == 0
            || !dimensions.is_multiple_of(metadata.pq_subvector_dimensions))
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "product-quantized pgcontext_hnsw dimensions {dimensions} must be divisible by pq_subvector_dimensions {}",
                metadata.pq_subvector_dimensions
            ),
        );
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL owns all pointers passed to this callback. This insert
// slice validates the indexed vector and heap TID but does not retain pointers
// after the callback returns.
#[allow(clippy::too_many_arguments)]
unsafe extern "C-unwind" fn pgcontext_hnsw_insert(
    index_relation: pg_sys::Relation,
    values: *mut pg_sys::Datum,
    is_null: *mut bool,
    heap_tid: pg_sys::ItemPointer,
    heap_relation: pg_sys::Relation,
    _check_unique: pg_sys::IndexUniqueCheck::Type,
    _index_unchanged: bool,
    index_info: *mut pg_sys::IndexInfo,
) -> bool {
    // SAFETY: This scope is stack-bound to the guarded insert callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL supplies live relation/IndexInfo pointers and
    // call-bounded datum, null, and TID pointers for guarded aminsert.
    let index_relation = unsafe { scope.borrow(index_relation, "index relation") };
    // SAFETY: See the callback contract above; this slice does not use or retain it.
    let _heap_relation = unsafe { scope.borrow(heap_relation, "heap relation") };
    // SAFETY: See the callback contract above; this slice does not retain it.
    let _index_info = unsafe { scope.borrow(index_info, "IndexInfo") };
    // SAFETY: Non-null arrays and TID are valid for the duration of this call.
    let values = unsafe { scope.borrow_optional(values) };
    // SAFETY: See the callback contract above.
    let is_null = unsafe { scope.borrow_optional(is_null) };
    // SAFETY: See the callback contract above.
    let heap_tid = unsafe { scope.borrow_optional(heap_tid) };
    self::hnsw_insert_safe(index_relation, values, is_null, heap_tid)
}

fn hnsw_insert_safe(
    index_relation: PgCallbackRef<'_, pg_sys::RelationData>,
    values: Option<PgCallbackRef<'_, pg_sys::Datum>>,
    is_null: Option<PgCallbackRef<'_, bool>>,
    heap_tid: Option<PgCallbackRef<'_, pg_sys::ItemPointerData>>,
) -> bool {
    let (Some(values), Some(is_null), Some(heap_tid)) = (values, is_null, heap_tid) else {
        return false;
    };

    // SAFETY: PostgreSQL owns the live relation pointer. The transaction-level
    // advisory lock is database-local and serializes the complete read,
    // allocation, append, and metapage-publication sequence for this index.
    unsafe { serialize_hnsw_insert(index_relation.as_ptr()) };

    // SAFETY: PostgreSQL passes a valid initialized index relation.
    let score_metric = unsafe { hnsw_score_metric(index_relation.as_ptr()) };

    // SAFETY: The callback provides value and null arrays matching the live
    // index relation descriptor; the decoder copies the vector into Rust.
    let Some(vector) = (unsafe {
        hnsw_vector_from_index_values(index_relation.as_ptr(), values.as_ptr(), is_null.as_ptr())
    }) else {
        return false;
    };
    let Some(vector) = score_metric
        .prepare_vector(vector)
        .unwrap_or_else(|error| raise_core_error(error))
    else {
        return false;
    };
    let dimensions = dimension_to_u32(vector.dimension());
    let heap_tid = item_pointer_to_u64(*heap_tid.as_ref());

    // SAFETY: The callback owns a live index relation whose metapage was
    // initialized by build or build-empty.
    let meta = unsafe { PgHnswGraphRead::new(index_relation.as_ptr()).meta() };
    let delta_limit = crate::settings::hnsw_delta_segment_limit_from_guc();
    if meta.delta_accepts_insert(delta_limit) {
        // SAFETY: The insert callback owns the live index relation for the
        // complete delta append and metapage update below.
        return unsafe {
            hnsw_insert_via_delta_safe(index_relation.as_ptr(), dimensions, heap_tid, vector)
        };
    }

    // The delta segment is full. Freeze it into an immutable graph segment
    // first; this cost is bounded by the delta limit rather than by the whole
    // index. A full segment directory falls through to compaction below.
    //
    if meta.delta_start_block != u64::MAX {
        // SAFETY: the callback owns the live relation and append lock.
        let rotated = unsafe { hnsw_rotate_delta_relation(index_relation.as_ptr(), score_metric) };
        if rotated {
            let meta = unsafe { PgHnswGraphRead::new(index_relation.as_ptr()).meta() };
            if meta.delta_accepts_insert(delta_limit) {
                return unsafe {
                    hnsw_insert_via_delta_safe(
                        index_relation.as_ptr(),
                        dimensions,
                        heap_tid,
                        vector,
                    )
                };
            }
        }
        if usize::from(meta.segment_count) >= HNSW_MAX_SEGMENTS {
            // SAFETY: the callback owns the live relation and append lock.
            let compacted = unsafe {
                hnsw_compact_smallest_pair(index_relation.as_ptr(), score_metric)
            };
            if compacted {
                // SAFETY: bounded compaction republished the directory.
                let rotated = unsafe {
                    hnsw_rotate_delta_relation(index_relation.as_ptr(), score_metric)
                };
                if rotated {
                    let meta = unsafe { PgHnswGraphRead::new(index_relation.as_ptr()).meta() };
                    if meta.delta_accepts_insert(delta_limit) {
                        return unsafe {
                            hnsw_insert_via_delta_safe(
                                index_relation.as_ptr(),
                                dimensions,
                                heap_tid,
                                vector,
                            )
                        };
                    }
                }
            }
        }
    }

    raise_sql_error_with_hint(
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
        "HNSW segmented insert could not make bounded delta capacity",
        "Retry after concurrent VACUUM/maintenance completes, enqueue bounded compaction, or raise maintenance_work_mem. The index remains unchanged.",
    )
}

/// Appends one row to the segmented-write delta instead of splicing it into
/// the HNSW graph: O(1) relative to graph size, versus the legacy path's
/// O(graph) full-relation read and reciprocal-neighbor rewiring.
///
/// # Safety
///
/// `index_relation` must be a live index relation whose metapage has an open
/// delta region (checked by the caller via [`HnswMetaPage::delta_accepts_insert`]
/// before this is called) and the caller must hold the per-index insert
/// advisory lock for the duration of this call.
unsafe fn hnsw_insert_via_delta_safe(
    index_relation: pg_sys::Relation,
    dimensions: u32,
    heap_tid: u64,
    vector: DenseVector,
) -> bool {
    // The dimension check must precede the append. Index pages are not
    // transactional: a record appended and WAL'd before an error keeps its
    // page slot when the transaction aborts, and a wrong-dimension record in
    // the delta region fails every later scan's exact distance computation —
    // one rejected INSERT would poison the index until compaction. The
    // caller holds the per-index append lock, so this read cannot race a
    // concurrent insert's dimension assignment. The message matches the
    // legacy inline path's, so the error contract is one shape regardless of
    // which insert path served the row.
    // SAFETY: the caller owns the live index relation for this read.
    let meta = unsafe { PgHnswGraphRead::new(index_relation).meta() };
    let stored_dimensions = meta.dimensions;
    if stored_dimensions != 0 && stored_dimensions != dimensions {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "failed to insert HNSW graph node: dimension mismatch: \
                 left has {stored_dimensions} dimensions, right has {dimensions}",
            ),
        );
    }
    ensure_hnsw_quantized_dimensions_supported(
        options::HnswQuantizationMetadata {
            mode: meta.quantization_mode,
            version: meta.quantization_metadata_version,
            scalar_min_bits: meta.scalar_min_bits,
            scalar_max_bits: meta.scalar_max_bits,
            scalar_levels: meta.scalar_levels,
            pq_subvector_dimensions: meta.pq_subvector_dimensions,
            codec_config_revision: meta.codec_config_revision,
        },
        dimensions,
    );
    let record = context_storage::DeltaRecord::live(heap_tid, vector.into_values())
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("failed to build HNSW delta record: {error}"),
            )
        });
    hnsw_physical_failpoint(11, "before_delta_append");
    // SAFETY: the caller holds the live index relation and owns `record` for
    // the complete append.
    let _location = unsafe { append_hnsw_delta_record(index_relation, &record, Some(dimensions)) };
    hnsw_physical_failpoint(12, "after_delta_append");
    record_hnsw_delta_segment_record();
    false
}

const fn hnsw_insert_lock_key(index_oid: u32) -> (i32, i32) {
    const PGCONTEXT_HNSW_LOCK_NAMESPACE: i32 = 0x5047_4358;
    (PGCONTEXT_HNSW_LOCK_NAMESPACE, index_oid.cast_signed())
}

/// Acquires the transaction-scoped allocator lock for one HNSW index.
///
/// # Safety
///
/// `index_relation` must point to the live PostgreSQL index relation owned by
/// the current access-method callback.
pub(crate) unsafe fn serialize_hnsw_insert(index_relation: pg_sys::Relation) {
    // SAFETY: The callback passes a live PostgreSQL index relation, so its
    // stable OID is readable for the callback duration.
    let index_oid = unsafe { (*index_relation).rd_id.to_u32() };
    let (namespace, relation_key) = hnsw_insert_lock_key(index_oid);
    // SAFETY: the built-in receives the exact two non-null int4 datums and owns
    // all lock-manager state; its void SQL result is intentionally ignored.
    unsafe {
        pgrx::direct_function_call_as_datum(
            pg_sys::pg_advisory_xact_lock_int4,
            &[
                Some(pg_sys::Datum::from(namespace)),
                Some(pg_sys::Datum::from(relation_key)),
            ],
        );
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL supplies live relation and IndexInfo pointers for the
// guarded cleanup call. No value is retained after the wrapper returns.
unsafe extern "C-unwind" fn pgcontext_hnsw_insert_cleanup(
    index_relation: pg_sys::Relation,
    index_info: *mut pg_sys::IndexInfo,
) {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: Guaranteed by the aminsertcleanup callback contract above.
    let _index_relation = unsafe { scope.borrow(index_relation, "index relation") };
    // SAFETY: Guaranteed by the aminsertcleanup callback contract above.
    let _index_info = unsafe { scope.borrow(index_info, "IndexInfo") };
    self::hnsw_insert_cleanup_safe();
}

fn hnsw_insert_cleanup_safe() {}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL owns vacuum info, optional prior stats, callback, and
// callback state for this guarded call. This design slice does not invoke or
// retain the deletion callback.
unsafe extern "C-unwind" fn pgcontext_hnsw_bulk_delete(
    info: *mut pg_sys::IndexVacuumInfo,
    stats: *mut pg_sys::IndexBulkDeleteResult,
    callback: pg_sys::IndexBulkDeleteCallback,
    callback_state: *mut c_void,
) -> *mut pg_sys::IndexBulkDeleteResult {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL guarantees a live IndexVacuumInfo for ambulkdelete.
    let info = unsafe { scope.borrow(info, "IndexVacuumInfo") };
    let stats = if stats.is_null() {
        vacuum::new_hnsw_vacuum_result()
    } else {
        stats
    };
    // SAFETY: Prior or newly allocated stats are writable for this callback.
    let stats = unsafe { scope.borrow_mut(stats, "vacuum stats") };
    self::hnsw_bulk_delete_safe(info, stats, callback, callback_state)
}

fn hnsw_bulk_delete_safe(
    info: PgCallbackRef<'_, pg_sys::IndexVacuumInfo>,
    mut stats: PgCallbackMut<'_, pg_sys::IndexBulkDeleteResult>,
    callback: pg_sys::IndexBulkDeleteCallback,
    callback_state: *mut c_void,
) -> *mut pg_sys::IndexBulkDeleteResult {
    // VACUUM already holds ShareUpdateExclusive on the parent table. Take the
    // same per-index lock as INSERT/rotation second, preserving the global
    // table -> advisory order and closing the append/publication race.
    unsafe { serialize_hnsw_insert(info.as_ref().index) };
    let meta = unsafe { PgHnswGraphRead::new(info.as_ref().index).meta() };
    let score_metric = unsafe { hnsw_score_metric(info.as_ref().index) };
    let config = meta.stored_config(score_metric, hnsw_config_from_gucs().ef_search());
    let mutation_count = meta
        .segments()
        .iter()
        .fold(meta.delta_record_count, |total, segment| {
            total.saturating_add(segment.mutation_record_count)
        });
    enforce_bounded_compaction_budget(
        meta.graph_nodes,
        usize::try_from(mutation_count).unwrap_or(usize::MAX),
        meta.dimensions,
        config,
    );
    let mut segment_records = Vec::with_capacity(meta.segments().len());
    for segment in meta.segments() {
        let base = unsafe { read_hnsw_segment_records(info.as_ref().index, *segment) };
        let mutations = if segment.mutation_start_block == u64::MAX {
            Vec::new()
        } else {
            unsafe {
                read_hnsw_delta_records_range(
                    info.as_ref().index,
                    segment.mutation_start_block,
                    segment.mutation_end_block,
                    segment.mutation_generation,
                    GraphPageKind::FrozenDelta,
                    segment.mutation_record_count,
                )
            }
        };
        segment_records.push((base, mutations));
    }
    let active = unsafe { read_hnsw_delta_records(info.as_ref().index, meta) };
    let chronological = segment_records
        .iter()
        .flat_map(|(base, mutations)| {
            base.iter()
                .map(|record| {
                    let heap_tid = hnsw_record_heap_tid(record);
                    if hnsw_record_is_tombstoned(record) {
                        DeltaScanEntry::Tombstone { heap_tid }
                    } else {
                        DeltaScanEntry::Live {
                            heap_tid,
                            vector: record.vector.as_slice(),
                        }
                    }
                })
                .chain(mutations.iter().map(|record| match record.kind {
                    DeltaRecordKind::Live => DeltaScanEntry::Live {
                        heap_tid: record.heap_tid,
                        vector: record.vector.as_slice(),
                    },
                    DeltaRecordKind::Tombstone => DeltaScanEntry::Tombstone {
                        heap_tid: record.heap_tid,
                    },
                }))
        })
        .chain(active.iter().map(|record| match record.kind {
            DeltaRecordKind::Live => DeltaScanEntry::Live {
                heap_tid: record.heap_tid,
                vector: record.vector.as_slice(),
            },
            DeltaRecordKind::Tombstone => DeltaScanEntry::Tombstone {
                heap_tid: record.heap_tid,
            },
        }));
    let live_rows = context_index::fold_compaction_live_rows(chronological);
    let mut dead_tids = Vec::new();
    if let Some(callback) = callback {
        for row in live_rows {
            let (block, offset) = u64_to_item_pointer_parts(row.heap_tid);
            let mut tid = pg_sys::ItemPointerData::default();
            item_pointer_set_all(&mut tid, block, offset);
            if unsafe { callback(&mut tid, callback_state) } {
                dead_tids.push(row.heap_tid);
            }
        }
    }
    let removed = dead_tids.len() as u64;
    if !dead_tids.is_empty() {
        let delta_limit = crate::settings::hnsw_delta_segment_limit_from_guc();
        for heap_tid in dead_tids {
            let current = unsafe { PgHnswGraphRead::new(info.as_ref().index).meta() };
            if !current.delta_accepts_insert(delta_limit) {
                if usize::from(current.segment_count) >= HNSW_MAX_SEGMENTS
                    && !unsafe {
                        hnsw_compact_smallest_pair(info.as_ref().index, score_metric)
                    }
                {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                        "VACUUM could not compact the bounded HNSW directory",
                    );
                }
                if !unsafe { hnsw_rotate_delta_relation(info.as_ref().index, score_metric) } {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                        "VACUUM could not rotate a bounded HNSW tombstone chunk",
                    );
                }
            }
            let _location = unsafe {
                append_hnsw_delta_record(
                    info.as_ref().index,
                    &context_storage::DeltaRecord::tombstone(heap_tid),
                    None,
                )
            };
            record_hnsw_delta_segment_record();
        }
        let current = unsafe { PgHnswGraphRead::new(info.as_ref().index).meta() };
        if usize::from(current.segment_count) >= HNSW_MAX_SEGMENTS
            && !unsafe { hnsw_compact_smallest_pair(info.as_ref().index, score_metric) }
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "VACUUM could not compact before final HNSW tombstone rotation",
            );
        }
        if !unsafe { hnsw_rotate_delta_relation(info.as_ref().index, score_metric) } {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "VACUUM could not publish the HNSW tombstone segment",
            );
        }
    }
    // SAFETY: The VACUUM info and optional prior stats are live for this
    // callback, and hnsw_vacuum_stats performs read-only relation inspection.
    let snapshot = unsafe {
        vacuum::hnsw_vacuum_stats(
            info.as_ref().index,
            info.as_ref().num_heap_tuples,
            stats.as_ref(),
        )
    };
    let mut snapshot = snapshot;
    // PostgreSQL exposes VACUUM tuple counters as f64, so very large integer
    // counts are necessarily approximate at this adapter boundary.
    #[allow(clippy::cast_precision_loss)]
    let removed = removed as f64;
    snapshot.tuples_removed += removed;
    if removed > 0.0 {
        // Rotation already published the directory mutation and cache epoch.
    }
    vacuum::write_hnsw_vacuum_stats(stats.as_mut(), snapshot);
    stats.as_ptr()
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL owns vacuum info and optional prior stats for this guarded
// call; the returned result remains PostgreSQL allocated.
unsafe extern "C-unwind" fn pgcontext_hnsw_vacuum_cleanup(
    info: *mut pg_sys::IndexVacuumInfo,
    stats: *mut pg_sys::IndexBulkDeleteResult,
) -> *mut pg_sys::IndexBulkDeleteResult {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL guarantees a live IndexVacuumInfo for amvacuumcleanup.
    let info = unsafe { scope.borrow(info, "IndexVacuumInfo") };
    let stats = if stats.is_null() {
        vacuum::new_hnsw_vacuum_result()
    } else {
        stats
    };
    // SAFETY: Prior or newly allocated stats are writable for this callback.
    let stats = unsafe { scope.borrow_mut(stats, "vacuum stats") };
    self::hnsw_vacuum_cleanup_safe(info, stats)
}

fn hnsw_vacuum_cleanup_safe(
    info: PgCallbackRef<'_, pg_sys::IndexVacuumInfo>,
    stats: PgCallbackMut<'_, pg_sys::IndexBulkDeleteResult>,
) -> *mut pg_sys::IndexBulkDeleteResult {
    hnsw_vacuum_safe(info, stats)
}

fn hnsw_vacuum_safe(
    info: PgCallbackRef<'_, pg_sys::IndexVacuumInfo>,
    mut stats: PgCallbackMut<'_, pg_sys::IndexBulkDeleteResult>,
) -> *mut pg_sys::IndexBulkDeleteResult {
    // SAFETY: IndexVacuumInfo supplies a live index relation for this callback.
    let snapshot = unsafe {
        vacuum::hnsw_vacuum_stats(
            info.as_ref().index,
            info.as_ref().num_heap_tuples,
            stats.as_ref(),
        )
    };
    vacuum::write_hnsw_vacuum_stats(stats.as_mut(), snapshot);
    stats.as_ptr()
}
