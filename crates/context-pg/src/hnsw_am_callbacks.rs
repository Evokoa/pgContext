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
    let snapshots = state.graph.node_snapshots();
    // A build's output is the published base, so it stamps the live
    // generation rather than a pending one: unlike compaction, an interrupted
    // CREATE INDEX discards the whole relation, so there is no window in which
    // these pages could be read alongside an older base.
    // SAFETY: PostgreSQL passes a valid index relation whose metapage was
    // written immediately above.
    let build_generation =
        unsafe { PgHnswGraphRead::new(index_relation.as_ptr()).meta().page_generation() };
    // SAFETY: PostgreSQL passes a valid index relation for the build callback,
    // and snapshots own finalized graph payloads copied from the heap scan.
    unsafe {
        write_hnsw_node_revisions_bulk(index_relation.as_ptr(), &snapshots, build_generation)
    };
    // SAFETY: PostgreSQL passes a valid index relation for the build callback;
    // the base graph is fully written above, so the current block count marks
    // where the segmented-write delta region begins.
    let post_build_block_count = u64::from(unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation.as_ptr(), pg_sys::ForkNumber::MAIN_FORKNUM)
    });
    // SAFETY: PostgreSQL passes a valid index relation for the build callback.
    unsafe {
        update_hnsw_metapage(index_relation.as_ptr(), |meta| {
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
            meta.open_delta_region(post_build_block_count);
        })
    };
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
    let dimensions = dimension_to_u32(vector.dimension());
    let vector = score_metric
        .prepare_vector(vector)
        .unwrap_or_else(|error| raise_core_error(error));
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

    // The delta segment is full. Compacting here drains it and reopens an
    // empty one, so this insert and the ones after it stay on the fast append
    // path; the alternative is that every later insert splices the graph
    // inline at O(graph size). This insert pays for the rebuild — a
    // predictable stall, documented on the GUC — instead of spreading an
    // unbounded cost across all its successors.
    //
    // Deliberately not attempted when the delta region was never opened
    // (`delta_start_block == u64::MAX`, an index built before the segmented
    // path) or when the limit is 0, which means the operator asked for the
    // legacy inline path: in both cases there is nothing to drain and
    // compacting would be a surprise.
    if meta.delta_start_block != u64::MAX
        && delta_limit > 0
        && crate::settings::hnsw_compact_on_threshold_from_guc()
    {
        // SAFETY: the callback owns the live index relation, and the advisory
        // lock taken at entry is the same one compaction requires.
        let compacted = unsafe { hnsw_compact_on_threshold(index_relation.as_ptr(), score_metric) };
        if compacted {
            // SAFETY: compaction republished the metapage; re-read it so the
            // reopened delta region is visible to this insert.
            let meta = unsafe { PgHnswGraphRead::new(index_relation.as_ptr()).meta() };
            if meta.delta_accepts_insert(delta_limit) {
                // SAFETY: as the delta append above.
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

    // SAFETY: The callback owns a live index relation whose metapage was
    // initialized by build or build-empty.
    let config = unsafe { hnsw_stored_config(index_relation.as_ptr(), score_metric) };
    // SAFETY: The same live metapage publishes the authoritative traversal
    // entry point needed to reconstruct the mutable graph faithfully.
    let entry_point = unsafe { hnsw_stored_entry_point(index_relation.as_ptr()) };
    // SAFETY: The AM owns the relation for this callback and the decoder returns
    // owned records before releasing every shared page buffer.
    let existing = unsafe { read_hnsw_vector_records(index_relation.as_ptr()) };
    let structural_node_count = existing.len() as u64;
    let node_id = checked_hnsw_node_id_from_graph_count(structural_node_count).unwrap_or_else(
        |count| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                format!("HNSW graph node count exceeds page-record storage: {count}"),
            )
        },
    );
    let persisted_heap_tids = existing
        .iter()
        .map(|record| record.heap_tid)
        .collect::<Vec<_>>();
    let mut graph = if existing.is_empty() {
        if entry_point.is_some() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "empty HNSW graph publishes a non-empty entry point",
            );
        }
        HnswGraph::new(score_metric.navigation_metric(), config)
    } else {
        hnsw_graph_from_records_with_config(
            existing,
            score_metric.navigation_metric(),
            config,
            entry_point,
        )
    };
    let prior_snapshots = graph.node_snapshots();
    if let Err(error) = graph.insert(HnswPointId::new(heap_tid), vector) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("failed to insert HNSW graph node: {error}"),
        );
    }
    let current_snapshots = graph.node_snapshots();
    let inserted = current_snapshots
        .iter()
        .find(|snapshot| snapshot.node_id() == node_id)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "HNSW insert did not produce its reserved node",
            )
        });
    hnsw_physical_failpoint(9, "before_rewiring");
    for snapshot in current_snapshots.iter().filter(|snapshot| {
        prior_snapshots
            .get(snapshot.node_id().get())
            .is_none_or(|prior| prior.layers() != snapshot.layers())
    }) {
        let mut record = hnsw_vector_record_from_snapshot(snapshot);
        if let Some(persisted_heap_tid) = persisted_heap_tids.get(snapshot.node_id().get()) {
            // Mutable reconstruction gives traversal-only tombstones synthetic
            // point IDs. Preserve their exact durable heap binding when a
            // neighboring insert rewires and republishes the structural node.
            record.heap_tid = *persisted_heap_tid;
        }
        // SAFETY: The insert callback owns the live index relation and record
        // payload for the duration of this append.
        let _ = unsafe { append_hnsw_node_revision(index_relation.as_ptr(), &record) };
    }
    hnsw_physical_failpoint(10, "after_rewiring");
    // Publish the metapage count only after every complete replacement record
    // has reached Generic WAL.
    let mut published_node_id = Some(node_id);
    // SAFETY: The insert callback owns the live relation and this closure only
    // mutates the locked metapage before publication. Tombstones remain
    // structural traversal nodes, so inserts always append a fresh node ID.
    unsafe {
        update_hnsw_metapage(index_relation.as_ptr(), |meta| {
            published_node_id = Some(meta.record_insert(dimensions, graph.entry_point()));
        })
    };
    if published_node_id != Some(node_id) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "HNSW metapage allocator disagrees with the staged graph node",
        );
    }
    debug_assert_eq!(inserted.node_id(), node_id);

    false
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
    let stored_dimensions = unsafe { PgHnswGraphRead::new(index_relation).meta() }.dimensions;
    if stored_dimensions != 0 && stored_dimensions != dimensions {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "failed to insert HNSW graph node: dimension mismatch: \
                 left has {stored_dimensions} dimensions, right has {dimensions}",
            ),
        );
    }
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
    let _location = unsafe { append_hnsw_delta_record(index_relation, &record) };
    hnsw_physical_failpoint(12, "after_delta_append");
    // SAFETY: the caller owns the live relation; this closure only mutates
    // the locked metapage before publication.
    unsafe {
        update_hnsw_metapage(index_relation, |meta| {
            if meta.dimensions == 0 {
                meta.dimensions = dimensions;
            }
            meta.record_delta_append();
        });
    }
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
unsafe fn serialize_hnsw_insert(index_relation: pg_sys::Relation) {
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
    // SAFETY: The VACUUM callback owns a live index relation for this call.
    let records = unsafe { read_hnsw_vector_records(info.as_ref().index) };
    let mut removed = 0_u64;
    if let Some(callback) = callback {
        for record in records
            .iter()
            .filter(|record| !hnsw_record_is_tombstoned(record))
        {
            let (block, offset) = u64_to_item_pointer_parts(hnsw_record_heap_tid(record));
            let mut tid = pg_sys::ItemPointerData::default();
            // SAFETY: The stack TID is initialized from validated block/offset
            // parts before being passed to PostgreSQL's callback.
            item_pointer_set_all(&mut tid, block, offset);
            // SAFETY: PostgreSQL supplied the callback and state for the
            // duration of this ambulkdelete invocation.
            if unsafe { callback(&mut tid, callback_state) } {
                // SAFETY: The callback confirmed this live relation record is
                // dead; append writes its bounded tombstone representation.
                unsafe {
                    append_hnsw_node_revision(
                        info.as_ref().index,
                        &hnsw_tombstone_record(record),
                    )
                };
                removed = removed.saturating_add(1);
            }
        }
    }
    // Rows absorbed by the segmented-write delta never got a base-graph
    // node record, so the walk above never sees them; tombstone dead delta
    // rows separately by folding the delta to its last-write-wins live set.
    // SAFETY: VACUUM owns the index relation for the duration of this call.
    let meta = unsafe { PgHnswGraphRead::new(info.as_ref().index).meta() };
    let mut removed_delta = 0_u64;
    if meta.delta_start_block != u64::MAX {
        // SAFETY: `delta_start_block` came from the metapage read above.
        let delta_records =
            unsafe { read_hnsw_delta_records(info.as_ref().index, meta.delta_start_block) };
        let mut live_delta_tids: BTreeSet<u64> = BTreeSet::new();
        for record in &delta_records {
            match record.kind {
                DeltaRecordKind::Live => {
                    live_delta_tids.insert(record.heap_tid);
                }
                DeltaRecordKind::Tombstone => {
                    live_delta_tids.remove(&record.heap_tid);
                }
            }
        }
        if let Some(callback) = callback {
            for heap_tid in live_delta_tids {
                let (block, offset) = u64_to_item_pointer_parts(heap_tid);
                let mut tid = pg_sys::ItemPointerData::default();
                // SAFETY: The stack TID is initialized from validated
                // block/offset parts before being passed to PostgreSQL.
                item_pointer_set_all(&mut tid, block, offset);
                // SAFETY: PostgreSQL supplied the callback and state for
                // the duration of this ambulkdelete invocation.
                if unsafe { callback(&mut tid, callback_state) } {
                    // SAFETY: The callback confirmed this delta-live row is
                    // dead; append its bounded tombstone representation.
                    unsafe {
                        append_hnsw_delta_record(
                            info.as_ref().index,
                            &context_storage::DeltaRecord::tombstone(heap_tid),
                        )
                    };
                    record_hnsw_delta_segment_record();
                    removed_delta = removed_delta.saturating_add(1);
                    removed = removed.saturating_add(1);
                }
            }
        }
        if removed_delta > 0 {
            // SAFETY: VACUUM owns the index relation and the appended
            // tombstone delta records are durable before this publish.
            unsafe {
                update_hnsw_metapage(info.as_ref().index, |meta| {
                    for _ in 0..removed_delta {
                        meta.record_delta_append();
                    }
                });
            }
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
        // SAFETY: VACUUM owns the index relation and all appended tombstone
        // locator revisions are durable before this cache-generation publish.
        unsafe {
            update_hnsw_metapage(info.as_ref().index, HnswMetaPage::record_directory_mutation)
        };
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
