// Scan-path AM callback fragment split from hnsw_am_callbacks.rs (source-hygiene
// size target): amcostestimate, amvalidate, ambeginscan, amrescan,
// amgettuple, amgetbitmap, amendscan, and their guarded delegates.

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL supplies live planner/path inputs and distinct writable
// output pointers for the guarded amcostestimate call.
#[allow(
    clippy::too_many_arguments,
    reason = "PostgreSQL fixes the amcostestimate callback ABI"
)]
unsafe extern "C-unwind" fn pgcontext_hnsw_cost_estimate(
    root: *mut pg_sys::PlannerInfo,
    path: *mut pg_sys::IndexPath,
    loop_count: f64,
    index_startup_cost: *mut pg_sys::Cost,
    index_total_cost: *mut pg_sys::Cost,
    index_selectivity: *mut pg_sys::Selectivity,
    index_correlation: *mut f64,
    index_pages: *mut f64,
) {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: Guaranteed by the amcostestimate callback contract above.
    let root = unsafe { scope.borrow(root, "PlannerInfo") };
    // SAFETY: Guaranteed by the amcostestimate callback contract above.
    let path = unsafe { scope.borrow(path, "IndexPath") };
    // SAFETY: PostgreSQL grants distinct writable output slots for the call.
    let startup = unsafe { scope.borrow_mut(index_startup_cost, "startup cost") };
    // SAFETY: See the output-pointer contract above.
    let total = unsafe { scope.borrow_mut(index_total_cost, "total cost") };
    // SAFETY: See the output-pointer contract above.
    let selectivity = unsafe { scope.borrow_mut(index_selectivity, "selectivity") };
    // SAFETY: See the output-pointer contract above.
    let correlation = unsafe { scope.borrow_mut(index_correlation, "correlation") };
    // SAFETY: See the output-pointer contract above.
    let pages = unsafe { scope.borrow_mut(index_pages, "index pages") };
    self::hnsw_cost_estimate_safe(
        root,
        path,
        loop_count,
        startup,
        total,
        selectivity,
        correlation,
        pages,
    );
}

fn hnsw_cost_estimate_safe(
    root: PgCallbackRef<'_, pg_sys::PlannerInfo>,
    path: PgCallbackRef<'_, pg_sys::IndexPath>,
    loop_count: f64,
    mut startup: PgCallbackMut<'_, pg_sys::Cost>,
    mut total: PgCallbackMut<'_, pg_sys::Cost>,
    mut selectivity: PgCallbackMut<'_, pg_sys::Selectivity>,
    mut correlation: PgCallbackMut<'_, f64>,
    mut pages: PgCallbackMut<'_, f64>,
) {
    if path.as_ref().indexorderbys.is_null() {
        startup.write(f64::INFINITY);
        total.write(f64::INFINITY);
        selectivity.write(0.0);
        correlation.write(0.0);
        pages.write(0.0);
        return;
    }

    // SAFETY: `GenericCosts` is a C plain-data output structure whose all-zero
    // state is the required input to PostgreSQL's generic cost estimator.
    let mut costs = unsafe { std::mem::zeroed::<pg_sys::GenericCosts>() };
    // SAFETY: the guarded callback established live PlannerInfo and IndexPath
    // capabilities; PostgreSQL owns `costs` only for this immediate call.
    unsafe {
        pg_sys::genericcostestimate(root.as_ptr(), path.as_ptr(), loop_count, &mut costs);
    }
    let index_tuples = if path.as_ref().indexinfo.is_null() {
        0.0
    } else {
        // SAFETY: PostgreSQL supplied a live IndexPath whose indexinfo remains
        // valid for the duration of the planner callback.
        unsafe { (*path.as_ref().indexinfo).tuples }
    };
    let config = hnsw_config_from_gucs();
    let ratio = hnsw_scan_tuple_ratio(index_tuples, config.m(), config.ef_search());

    startup.write(costs.indexTotalCost * ratio);
    total.write(costs.indexTotalCost);
    selectivity.write(costs.indexSelectivity);
    correlation.write(costs.indexCorrelation);
    pages.write(costs.numIndexPages);
}

#[allow(
    clippy::cast_precision_loss,
    reason = "planner estimates intentionally convert bounded HNSW settings to floating-point costs"
)]
fn hnsw_scan_tuple_ratio(index_tuples: f64, m: usize, ef_search: usize) -> f64 {
    if index_tuples <= 0.0 {
        return 1.0;
    }
    let m = m as f64;
    let ef_search = ef_search as f64;
    let entry_level = index_tuples.ln() / m.ln();
    let layer_zero_max = (2.0 * m) * ef_search;
    let layer_zero_selectivity =
        0.55 * index_tuples.ln() / (m.ln() * (1.0 + ef_search.ln()));
    ((entry_level * m + layer_zero_max * layer_zero_selectivity) / index_tuples).min(1.0)
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: The opclass OID is a copied scalar supplied by PostgreSQL.
unsafe extern "C-unwind" fn pgcontext_hnsw_validate(opclass_oid: pg_sys::Oid) -> bool {
    self::hnsw_validate_safe(opclass_oid)
}

fn hnsw_validate_safe(_opclass_oid: pg_sys::Oid) -> bool {
    true
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL supplies a live index relation and nonnegative key counts
// for the guarded ambeginscan call. Returned scan ownership stays PostgreSQL's;
// Rust state is registered with the scan descriptor's memory context.
unsafe extern "C-unwind" fn pgcontext_hnsw_begin_scan(
    index_relation: pg_sys::Relation,
    nkeys: std::ffi::c_int,
    norderbys: std::ffi::c_int,
) -> pg_sys::IndexScanDesc {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: Guaranteed by the ambeginscan callback contract above.
    let index_relation = unsafe { scope.borrow(index_relation, "index relation") };
    self::hnsw_begin_scan_safe(&scope, index_relation, nkeys, norderbys)
}

fn hnsw_begin_scan_safe(
    scope: &PgCallbackScope,
    index_relation: PgCallbackRef<'_, pg_sys::RelationData>,
    nkeys: std::ffi::c_int,
    norderbys: std::ffi::c_int,
) -> pg_sys::IndexScanDesc {
    // SAFETY: PostgreSQL initializes `MyDatabaseId` before invoking an index
    // AM. Reconcile before allocating state so every HNSW scan makes bounded
    // progress, including scans that later reuse backend-local graph state.
    let database_oid = unsafe { pg_sys::MyDatabaseId.to_u32() };
    reconcile_pending_mapped_drops(database_oid);
    let _key_count = checked_scan_count(nkeys, MAX_HNSW_SCAN_KEYS, "scan keys");
    let orderby_count = checked_scan_count(norderbys, MAX_HNSW_SCAN_ORDERBYS, "scan order-bys");
    // SAFETY: PostgreSQL passes a valid index relation and scan key counts.
    let scan = unsafe { pg_sys::RelationGetIndexScan(index_relation.as_ptr(), nkeys, norderbys) };
    // SAFETY: RelationGetIndexScan returns a live descriptor in the active
    // memory context or raises a PostgreSQL error.
    let mut scan_ptr = unsafe { scope.borrow_mut(scan, "IndexScanDesc") };
    // SAFETY: RelationGetIndexScan allocated `scan` through PostgreSQL, so
    // pgrx may recover its owning memory context. Registering the slot there
    // drops Rust state on normal context deletion and on ERROR/cancel reset.
    let mut scan_context =
        unsafe { PgMemoryContexts::of(scan.cast::<c_void>()) }.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "HNSW scan descriptor has no owning PostgreSQL memory context",
            )
        });
    let state = scan_context
        .leak_and_drop_on_delete(PgMemoryContextDropSlot::new(HnswScanState::default()));
    scan_ptr.as_mut().opaque = state.cast::<c_void>();

    // SAFETY: The scan descriptor is newly allocated by PostgreSQL. The
    // order-by arrays are allocated in the same active scan context.
    unsafe {
        if norderbys > 0 {
            let datum_bytes = checked_callback_allocation_bytes::<pg_sys::Datum>(
                orderby_count,
                "scan order-by datums",
            );
            let null_bytes =
                checked_callback_allocation_bytes::<bool>(orderby_count, "scan order-by nulls");
            (*scan_ptr.as_ptr()).xs_orderbyvals =
                pg_sys::palloc0(datum_bytes).cast::<pg_sys::Datum>();
            (*scan_ptr.as_ptr()).xs_orderbynulls = pg_sys::palloc0(null_bytes).cast::<bool>();
        }
    }
    scan
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL supplies the live descriptor returned by ambeginscan and
// optional key arrays matching nonnegative counts. Arrays are copied only and
// never retained.
unsafe extern "C-unwind" fn pgcontext_hnsw_rescan(
    scan: pg_sys::IndexScanDesc,
    keys: pg_sys::ScanKey,
    nkeys: std::ffi::c_int,
    orderbys: pg_sys::ScanKey,
    norderbys: std::ffi::c_int,
) {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: Guaranteed by the amrescan callback contract above, including
    // exclusive descriptor access for the duration of this call.
    let scan = unsafe { scope.borrow_mut(scan, "IndexScanDesc") };
    let key_count = checked_scan_count(nkeys, MAX_HNSW_SCAN_KEYS, "scan keys");
    let orderby_count = checked_scan_count(norderbys, MAX_HNSW_SCAN_ORDERBYS, "scan order-bys");
    // SAFETY: PostgreSQL supplies arrays readable for their declared counts;
    // zero-length arrays may be null and are never dereferenced.
    let keys = unsafe { scope.borrow_slice(keys, key_count, "scan keys") };
    // SAFETY: The same array/count contract applies to order-by keys.
    let orderbys = unsafe { scope.borrow_slice(orderbys, orderby_count, "scan order-bys") };
    self::hnsw_rescan_safe(scan, keys, orderbys);
}

fn hnsw_rescan_safe(
    scan: PgCallbackMut<'_, pg_sys::IndexScanDescData>,
    keys: PgCallbackSlice<'_, pg_sys::ScanKeyData>,
    orderbys: PgCallbackSlice<'_, pg_sys::ScanKeyData>,
) {
    crate::pgvector_compat::nudge_pgvector_compat(scan.as_ref().indexRelation);
    let key_capacity = c_int_to_usize(scan.as_ref().numberOfKeys, "scan key capacity");
    let orderby_capacity = c_int_to_usize(scan.as_ref().numberOfOrderBys, "scan order-by capacity");
    checked_rescan_extent(keys.len(), key_capacity, "scan keys");
    checked_rescan_extent(orderbys.len(), orderby_capacity, "scan order-bys");
    // SAFETY: RelationGetIndexScan owns destination arrays for the recorded
    // capacities. Each source capability carries at most one readable value;
    // the helper stages that value on the stack before writing the destination.
    unsafe {
        copy_rescan_keys(keys, scan.as_ref().keyData, "scan keys");
        copy_rescan_keys(orderbys, scan.as_ref().orderByData, "scan order-bys");
    }
    // SAFETY: The scan descriptor's opaque pointer names a live state slot
    // registered by begin-scan in the descriptor's owning memory context.
    unsafe {
        (*hnsw_scan_state_ptr(scan.as_ptr())).reset();
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL supplies a live scan descriptor created by this AM. Its
// memory-context-owned state slot, relations, snapshot, TID destination, and
// order-by arrays remain live for the guarded call; no borrow is retained.
unsafe extern "C-unwind" fn pgcontext_hnsw_get_tuple(
    scan: pg_sys::IndexScanDesc,
    direction: pg_sys::ScanDirection::Type,
) -> bool {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: Guaranteed by the amgettuple callback contract above.
    let scan = unsafe { scope.borrow_mut(scan, "IndexScanDesc") };
    self::hnsw_get_tuple_safe(scan, direction)
}

fn hnsw_get_tuple_safe(
    scan: PgCallbackMut<'_, pg_sys::IndexScanDescData>,
    _direction: pg_sys::ScanDirection::Type,
) -> bool {
    // SAFETY: PostgreSQL passes a live scan descriptor. Candidate preparation
    // reads only committed index pages through the relation pointer in the
    // descriptor and stores Rust-owned results in AM-private scan state.
    unsafe {
        let state = hnsw_scan_state_ptr(scan.as_ptr());
        if !(*state).prepared {
            prepare_hnsw_scan(scan.as_ptr(), &mut *state);
        }
        loop {
            while let Some(candidate) = (*state).next() {
                (*state).returned_heap_tids.insert(candidate.heap_tid);
                (*state).work.rechecks = (*state).work.rechecks.saturating_add(1);
                let Some((block_number, offset_number)) =
                    hnsw_visible_heap_tid(scan.as_ptr(), candidate.heap_tid)
                else {
                    continue;
                };
                pg_sys::ItemPointerSet(
                    &mut (*scan.as_ptr()).xs_heaptid,
                    block_number,
                    offset_number,
                );
                (*scan.as_ptr()).xs_heap_continue = false;
                (*scan.as_ptr()).xs_recheck = false;
                if (*scan.as_ptr()).numberOfOrderBys > 0 {
                    let metric = hnsw_score_metric((*scan.as_ptr()).indexRelation);
                    // Bit-Jaccard navigation is intentionally `f32`, while the
                    // SQL operator is exact `f64`. PostgreSQL must re-evaluate
                    // that operator on the visible heap tuple before ordering.
                    (*scan.as_ptr()).xs_recheckorderby = metric == HnswScoreMetric::BitJaccard;
                    store_hnsw_orderby_distance(scan.as_ptr(), metric, candidate.score);
                } else {
                    (*scan.as_ptr()).xs_recheckorderby = false;
                }
                record_hnsw_scan_work((*state).work);
                return true;
            }
            if !expand_hnsw_scan(scan.as_ptr(), &mut *state) {
                break;
            }
        }
        record_hnsw_scan_work((*state).work);
    }
    false
}

unsafe fn expand_hnsw_scan(scan: pg_sys::IndexScanDesc, state: &mut HnswScanState) -> bool {
    // SAFETY: The caller passes the live descriptor owned by the active AM
    // get-tuple callback.
    if unsafe { (*scan).numberOfOrderBys } <= 0 {
        return false;
    }
    let ceiling = crate::settings::hnsw_iterative_expansion_limit_from_guc();
    if state.candidate_limit == 0 || state.candidate_limit >= ceiling {
        return false;
    }
    let next_limit = state.candidate_limit.saturating_mul(2).min(ceiling);
    // SAFETY: `scan` remains live for this callback and owns its order-by key.
    let query = unsafe { hnsw_orderby_query(scan) };
    // SAFETY: the live scan owns its index relation for the callback.
    let mut outcome = unsafe {
        hnsw_scan_candidates((*scan).indexRelation, query.as_ref(), Some(next_limit))
    };
    outcome
        .candidates
        .retain(|candidate| !state.returned_heap_tids.contains(&candidate.heap_tid));
    state.position = 0;
    state.candidate_limit = outcome.requested_limit;
    state.candidates = outcome.candidates;
    state.work.page_visits = state.work.page_visits.saturating_add(outcome.work.page_visits);
    state.work.node_reads = state.work.node_reads.saturating_add(outcome.work.node_reads);
    state.work.candidates = state.work.candidates.saturating_add(state.candidates.len());
    !state.candidates.is_empty() || state.candidate_limit < ceiling
}

unsafe fn store_hnsw_orderby_distance(
    scan: pg_sys::IndexScanDesc,
    metric: HnswScoreMetric,
    score: f32,
) {
    // SAFETY: PostgreSQL allocates these arrays in `pgcontext_hnsw_begin_scan`
    // when order-by keys are present, and this function is only called after
    // `numberOfOrderBys > 0`.
    let orderby_count = unsafe { c_int_to_usize((*scan).numberOfOrderBys, "scan order-bys") };
    match metric {
        HnswScoreMetric::L2 | HnswScoreMetric::BitJaccard => {
            let (distance, recheck) = float8_orderby_distance(metric, score);
            let mut order_by_types = vec![pg_sys::FLOAT8OID; orderby_count];
            let mut distances = vec![
                pg_sys::IndexOrderByDistance {
                    value: distance,
                    isnull: false,
                };
                orderby_count
            ];
            // SAFETY: `scan` is a live index scan descriptor and the arrays
            // contain one float8 distance entry per order-by key.
            unsafe {
                pg_sys::index_store_float8_orderby_distances(
                    scan,
                    order_by_types.as_mut_ptr(),
                    distances.as_mut_ptr(),
                    recheck,
                );
            }
        }
        HnswScoreMetric::NegativeInnerProduct | HnswScoreMetric::Cosine | HnswScoreMetric::L1 => {
            for index in 0..orderby_count {
                // SAFETY: PostgreSQL allocated one datum and null slot per
                // active order-by key; these operators return float4.
                unsafe {
                    *(*scan).xs_orderbyvals.add(index) = pg_sys::Float4GetDatum(score);
                    *(*scan).xs_orderbynulls.add(index) = false;
                }
            }
        }
        HnswScoreMetric::BitHamming => {
            let distance = bit_hamming_orderby_distance(score);
            for index in 0..orderby_count {
                // SAFETY: `xs_orderbyvals` and `xs_orderbynulls` have
                // `numberOfOrderBys` slots allocated by begin-scan.
                unsafe {
                    *(*scan).xs_orderbyvals.add(index) = pg_sys::Int32GetDatum(distance);
                    *(*scan).xs_orderbynulls.add(index) = false;
                }
            }
        }
    }
}

/// Returns a conservative lower bound for an exact `f64` Jaccard distance.
///
/// Graph navigation computes `1 - intersection / union` with two `f32`
/// operations over exactly represented counts. Each operation contributes at
/// most half an ulp in `[0, 1]`; subtracting two `f32::EPSILON`s therefore
/// keeps PostgreSQL's reorder-queue key below the exact SQL distance. The heap
/// operator recheck supplies the final `f64` value.
fn bit_jaccard_orderby_lower_bound(score: f32) -> f64 {
    (f64::from(score) - 2.0 * f64::from(f32::EPSILON)).max(0.0)
}

fn float8_orderby_distance(metric: HnswScoreMetric, score: f32) -> (f64, bool) {
    match metric {
        HnswScoreMetric::BitJaccard => (bit_jaccard_orderby_lower_bound(score), true),
        HnswScoreMetric::L2 => (f64::from(score), false),
        _ => unreachable!("only float8 HNSW metrics use float8 order-by storage"),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn bit_hamming_orderby_distance(score: f32) -> i32 {
    let score = f64::from(score);
    if !score.is_finite() || score < 0.0 || score.fract() != 0.0 || score > f64::from(i32::MAX) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("invalid HNSW bit Hamming score for int4 order-by distance: {score}"),
        );
    }
    // The checks above prove the finite integral value is in the i32 domain.
    score as i32
}

unsafe fn hnsw_visible_heap_tid(
    scan: pg_sys::IndexScanDesc,
    heap_tid: u64,
) -> Option<(pg_sys::BlockNumber, pg_sys::OffsetNumber)> {
    let (block_number, offset_number) = u64_to_item_pointer_parts(heap_tid);
    let mut tid = pg_sys::ItemPointerData::default();
    // SAFETY: `tid` is a local item pointer initialized through PostgreSQL's
    // shim before being passed to the table AM visibility checker.
    unsafe { pg_sys::ItemPointerSet(&mut tid, block_number, offset_number) };
    let mut all_dead = false;
    // SAFETY: `scan` is a live ordered index scan descriptor. The heap relation
    // and snapshot belong to the descriptor, and this helper only asks the table
    // AM whether the candidate TID has a visible row version for that snapshot.
    let visible = unsafe {
        pg_sys::table_index_fetch_tuple_check(
            (*scan).heapRelation,
            &mut tid,
            (*scan).xs_snapshot,
            &mut all_dead,
        )
    };
    visible.then_some((block_number, offset_number))
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL supplies a live scan descriptor and writable TIDBitmap.
// TIDs are copied into PostgreSQL storage and temporary Rust vectors are not
// retained after the guarded call.
unsafe extern "C-unwind" fn pgcontext_hnsw_get_bitmap(
    scan: pg_sys::IndexScanDesc,
    bitmap: *mut pg_sys::TIDBitmap,
) -> i64 {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: Guaranteed by the amgetbitmap callback contract above.
    let scan = unsafe { scope.borrow(scan, "IndexScanDesc") };
    // SAFETY: Guaranteed by the amgetbitmap callback contract above.
    let bitmap = unsafe { scope.borrow_mut(bitmap, "TIDBitmap") };
    self::hnsw_get_bitmap_safe(scan, bitmap)
}

fn hnsw_get_bitmap_safe(
    scan: PgCallbackRef<'_, pg_sys::IndexScanDescData>,
    bitmap: PgCallbackMut<'_, pg_sys::TIDBitmap>,
) -> i64 {
    // SAFETY: PostgreSQL passes a live scan descriptor and bitmap. The AM owns
    // only copied TID values, and asks the bitmap heap scan to recheck rows
    // because visibility and final predicates remain heap responsibilities.
    unsafe {
        let query = hnsw_orderby_query(scan.as_ptr());
        let outcome =
            hnsw_scan_candidates((*scan.as_ptr()).indexRelation, query.as_ref(), None);
        let mut tids = bitmap::hnsw_bitmap_tids(&outcome.candidates);
        let ntids = bitmap::hnsw_bitmap_tid_count(tids.len());
        if ntids == 0 {
            return 0;
        }
        pg_sys::tbm_add_tuples(bitmap.as_ptr(), tids.as_mut_ptr(), ntids, true);
        i64::from(ntids)
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL supplies the live descriptor created by ambeginscan.
// `opaque` is null or one memory-context-owned state slot. Normal cleanup takes
// its state once; ERROR/cancel cleanup is owned by the context reset callback.
unsafe extern "C-unwind" fn pgcontext_hnsw_end_scan(scan: pg_sys::IndexScanDesc) {
    // SAFETY: This anchor is created and dropped within the guarded callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: Guaranteed by the amendscan callback contract above.
    let scan = unsafe { scope.borrow_mut(scan, "IndexScanDesc") };
    self::hnsw_end_scan_safe(scan);
}

fn hnsw_end_scan_safe(mut scan: PgCallbackMut<'_, pg_sys::IndexScanDescData>) {
    let opaque = scan.as_ref().opaque;
    scan.as_mut().opaque = ptr::null_mut();
    // SAFETY: A non-null opaque pointer names the state slot registered by
    // begin-scan. Taking the Option drops state now; the context callback later
    // drops only the empty slot. A repeated amendscan sees null.
    unsafe {
        if let Some(slot) = opaque
            .cast::<PgMemoryContextDropSlot<HnswScanState>>()
            .as_mut()
        {
            drop(slot.take());
        }
    }
}
