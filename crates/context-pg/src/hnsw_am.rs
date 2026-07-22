//! PostgreSQL access-method registration for pgContext HNSW indexes.

#![allow(
    clippy::too_many_arguments,
    reason = "PostgreSQL fixes access-method callback signatures and their guarded delegates"
)]

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{
    CandidateMask, ConcurrentHnswBuilder, DeltaHit, DeltaScanEntry, GraphDirectoryKeyKind,
    GraphMetadata, GraphNeighbors, GraphNodeRecord, GraphNodeView, GraphPageId, GraphPageKind,
    GraphRead, GraphRecordId, HnswCancellation, HnswConfig, HnswError, HnswGraph,
    HnswGraphNodeSnapshot, HnswNodeId, HnswPointId, HnswSearchOutcome, LayerIndex, merge_topk,
    scan_delta_topk, search_graph_read, search_graph_read_with_mask_budgeted,
};
use context_storage::{
    DeltaRecordKind, PackedGraphImageError, PackedGraphImageLayer, PackedGraphImageNode,
    PackedGraphImageView, encode_packed_graph_image,
};
use pgrx::datum::{AnyArray, AnyElement};
use pgrx::itemptr::{item_pointer_to_u64, u64_to_item_pointer_parts};
use pgrx::prelude::*;
use pgrx::{AllocatedByRust, FromDatum, PgBox, PgMemoryContexts, PgRelation};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, c_void};
use std::mem::{offset_of, size_of};
use std::ptr;
use std::rc::Rc;
use std::slice;
#[cfg(any(test, feature = "pg_test"))]
use std::sync::atomic::{AtomicU8, Ordering};

use crate::Vector;
use crate::error::{raise_core_error, raise_sql_error, raise_sql_error_with_hint};
use crate::settings::hnsw_config_from_gucs;
use crate::vector_variants::{BitVec, HalfVec, SparseVec};
#[allow(unused_imports)]
use crate::vector_variants::{
    bitvec_hamming_distance, bitvec_jaccard_distance, halfvec_cosine_distance, halfvec_l1_distance,
    halfvec_l2_distance, halfvec_negative_inner_product, sparsevec_cosine_distance,
    sparsevec_l1_distance, sparsevec_l2_distance, sparsevec_negative_inner_product,
};

mod bitmap;
#[allow(
    dead_code,
    reason = "the executable callback inventory is consumed by tests and the source guard"
)]
mod callback_contract;
mod ffi_boundary;
#[allow(dead_code)]
mod mvcc_contract;
mod options;
#[allow(dead_code)]
mod page_codec;
mod storage;
mod vacuum;
#[allow(dead_code)]
mod wal_contract;

use ffi_boundary::{
    PgCallbackMut, PgCallbackRef, PgCallbackScope, PgCallbackSlice, PgMemoryContextDropSlot,
};
use page_codec::{PageHeaderV2, decode_page_header, encode_page_header};
use storage::{
    HnswAdjacencyRecord, HnswDirectoryRecord, HnswVectorRecord, decode_hnsw_adjacency_record,
    decode_hnsw_directory_record, decode_hnsw_vector_record, encode_hnsw_adjacency_record,
    encode_hnsw_directory_record, encode_hnsw_vector_record, hnsw_graph_snapshot_from_record,
    hnsw_point_id_is_tombstoned, hnsw_record_heap_tid, hnsw_record_is_tombstoned,
    hnsw_tombstone_record, hnsw_vector_record_from_snapshot, hnsw_vector_record_view,
};

const MAX_HNSW_SCAN_KEYS: usize = 1;
const MAX_HNSW_SCAN_ORDERBYS: usize = 1;

include!("hnsw_am/sql_contract.rs");
include!("hnsw_am/shared_registry.rs");

static HNSW_HANDLER_FINFO: pg_sys::Pg_finfo_record = pg_sys::Pg_finfo_record { api_version: 1 };
const HNSW_META_MAGIC: u32 = 0x4853_4e57;
// Version five makes directory locators authoritative and generation-tagged
// for backend-local cache reuse. Version seven adds the segmented-write
// delta region (delta_start_block/delta_record_count). Version eight adds
// base_start_block so compaction can publish a base written past the old
// one. Earlier experimental indexes must be rebuilt.
const HNSW_META_VERSION: u16 = 9;
const HNSW_FIRST_VECTOR_BLOCK: pg_sys::BlockNumber = 1;
const HNSW_INVALID_OFFSET: pg_sys::OffsetNumber = 0;
const HNSW_FIRST_OFFSET: pg_sys::OffsetNumber = 1;
const HNSW_FIRST_VECTOR_RECORD_OFFSET: pg_sys::OffsetNumber = HNSW_FIRST_OFFSET + 1;
const HNSW_INITIAL_PAGE_GENERATION: u64 = 1;

#[cfg(any(test, feature = "pg_test"))]
static HNSW_PHYSICAL_FAILPOINT: AtomicU8 = AtomicU8::new(0);

#[cfg(any(test, feature = "pg_test"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HnswPhysicalFailpoint {
    BeforePageInitialization = 1,
    AfterPageInitialization = 2,
    BeforeAppend = 3,
    AfterAppend = 4,
    BeforeGenericXLogFinish = 5,
    AfterGenericXLogFinish = 6,
    BeforeMetapagePublication = 7,
    AfterMetapagePublication = 8,
    BeforeRewiring = 9,
    AfterRewiring = 10,
    BeforeDeltaAppend = 11,
    AfterDeltaAppend = 12,
    BeforeCompactionWrite = 13,
    AfterCompactionWrite = 14,
    AfterCompactionPublish = 15,
}

#[cfg(any(test, feature = "pg_test"))]
fn hnsw_set_physical_failpoint(failpoint: Option<HnswPhysicalFailpoint>) {
    HNSW_PHYSICAL_FAILPOINT.store(failpoint.map_or(0, |point| point as u8), Ordering::SeqCst);
}

fn hnsw_physical_failpoint(stage: u8, label: &'static str) {
    #[cfg(any(test, feature = "pg_test"))]
    if HNSW_PHYSICAL_FAILPOINT.load(Ordering::SeqCst) == stage {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("injected HNSW physical failpoint: {label}"),
        );
    }
    #[cfg(not(any(test, feature = "pg_test")))]
    let _ = (stage, label);
}

#[cfg(feature = "pg_test")]
#[pg_extern(schema = "pgcontext")]
fn test_set_hnsw_physical_failpoint(name: Option<String>) {
    let failpoint = match name.as_deref() {
        None => None,
        Some("before_page_initialization") => Some(HnswPhysicalFailpoint::BeforePageInitialization),
        Some("after_page_initialization") => Some(HnswPhysicalFailpoint::AfterPageInitialization),
        Some("before_append") => Some(HnswPhysicalFailpoint::BeforeAppend),
        Some("after_append") => Some(HnswPhysicalFailpoint::AfterAppend),
        Some("before_rewiring") => Some(HnswPhysicalFailpoint::BeforeRewiring),
        Some("after_rewiring") => Some(HnswPhysicalFailpoint::AfterRewiring),
        Some("before_generic_xlog_finish") => Some(HnswPhysicalFailpoint::BeforeGenericXLogFinish),
        Some("after_generic_xlog_finish") => Some(HnswPhysicalFailpoint::AfterGenericXLogFinish),
        Some("before_metapage_publication") => {
            Some(HnswPhysicalFailpoint::BeforeMetapagePublication)
        }
        Some("after_metapage_publication") => Some(HnswPhysicalFailpoint::AfterMetapagePublication),
        Some("before_delta_append") => Some(HnswPhysicalFailpoint::BeforeDeltaAppend),
        Some("after_delta_append") => Some(HnswPhysicalFailpoint::AfterDeltaAppend),
        Some("before_compaction_write") => Some(HnswPhysicalFailpoint::BeforeCompactionWrite),
        Some("after_compaction_write") => Some(HnswPhysicalFailpoint::AfterCompactionWrite),
        Some("after_compaction_publish") => Some(HnswPhysicalFailpoint::AfterCompactionPublish),
        Some(value) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unknown HNSW physical failpoint: {value}"),
        ),
    };
    hnsw_set_physical_failpoint(failpoint);
}

/// Returns L2 distance as `float8` for HNSW order-by operators.
#[pg_extern(schema = "pgcontext", immutable, parallel_safe)]
pub fn hnsw_l2_distance(left: Vector, right: Vector) -> f64 {
    let left = match left.to_dense() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };
    let right = match right.to_dense() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };
    match DistanceMetric::L2.distance(&left, &right) {
        Ok(score) => f64::from(score),
        Err(error) => raise_core_error(error),
    }
}

fn hnsw_handler_safe() -> pg_sys::Datum {
    let routine = hnsw_index_am_routine();
    // SAFETY: PostgreSQL calls access-method handlers inside a valid current
    // memory context. `IndexAmRoutine` is a plain FFI struct, and every field is
    // initialized before the pointer is handed back to PostgreSQL.
    let mut boxed = unsafe { PgBox::<pg_sys::IndexAmRoutine, AllocatedByRust>::alloc0() };
    *boxed = routine;

    pg_sys::Datum::from(boxed.into_pg())
}

fn hnsw_index_am_routine() -> pg_sys::IndexAmRoutine {
    pg_sys::IndexAmRoutine {
        type_: pg_sys::NodeTag::T_IndexAmRoutine,
        amstrategies: 1,
        amsupport: 1,
        amcanorder: false,
        amcanorderbyop: true,
        amcanbackward: false,
        amcanunique: false,
        amcanmulticol: false,
        amoptionalkey: true,
        amsearcharray: false,
        amsearchnulls: false,
        amstorage: true,
        amclusterable: false,
        ampredlocks: false,
        amcanparallel: false,
        amcanbuildparallel: false,
        amcaninclude: false,
        // Candidate lists are owned, one-pass scan state. PostgreSQL must not
        // request mark/restore until this AM implements a stable mark token.
        ammarkpos: None,
        amrestrpos: None,
        amusemaintenanceworkmem: true,
        amkeytype: pg_sys::InvalidOid,
        ambuild: Some(pgcontext_hnsw_build),
        ambuildempty: Some(pgcontext_hnsw_build_empty),
        aminsert: Some(pgcontext_hnsw_insert),
        aminsertcleanup: Some(pgcontext_hnsw_insert_cleanup),
        ambulkdelete: Some(pgcontext_hnsw_bulk_delete),
        amvacuumcleanup: Some(pgcontext_hnsw_vacuum_cleanup),
        amoptions: Some(options::pgcontext_hnsw_options),
        amcostestimate: Some(pgcontext_hnsw_cost_estimate),
        amvalidate: Some(pgcontext_hnsw_validate),
        ambeginscan: Some(pgcontext_hnsw_begin_scan),
        amrescan: Some(pgcontext_hnsw_rescan),
        amgettuple: Some(pgcontext_hnsw_get_tuple),
        amgetbitmap: Some(pgcontext_hnsw_get_bitmap),
        amendscan: Some(pgcontext_hnsw_end_scan),
        ..pg_sys::IndexAmRoutine::default()
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: PostgreSQL owns all relation and index-info pointers passed to this
// callback. The callback copies SQL vector values into an in-memory HNSW graph
// for this static-table slice and does not retain PostgreSQL tuple pointers.
unsafe extern "C-unwind" fn pgcontext_hnsw_build(
    heap_relation: pg_sys::Relation,
    index_relation: pg_sys::Relation,
    index_info: *mut pg_sys::IndexInfo,
) -> *mut pg_sys::IndexBuildResult {
    // SAFETY: This scope is stack-bound to the guarded build callback.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL guarantees live relation and IndexInfo pointers for
    // the complete guarded ambuild call and retains ownership of all three.
    let heap_relation = unsafe { scope.borrow(heap_relation, "heap relation") };
    // SAFETY: See the callback contract above.
    let index_relation = unsafe { scope.borrow(index_relation, "index relation") };
    // SAFETY: See the callback contract above; this callback does not retain it.
    let index_info = unsafe { scope.borrow(index_info, "IndexInfo") };
    self::hnsw_build_safe(heap_relation, index_relation, index_info)
}

include!("hnsw_am_metapage.rs");
include!("hnsw_am_callbacks.rs");
include!("hnsw_am_scan_callbacks.rs");
include!("hnsw_am_compaction.rs");

struct HnswBuildState {
    graph: HnswGraph,
    config: HnswConfig,
    /// Rows collected instead of inserted directly when `parallel_workers >
    /// 1`; drained into `graph` by [`HnswBuildState::finish_parallel_build`]
    /// after the heap scan completes. Empty (and unused) at `workers == 1`.
    pending: Vec<(HnswPointId, DenseVector)>,
    parallel_workers: usize,
    score_metric: HnswScoreMetric,
    index_tuples: u64,
    dimensions: Option<u32>,
    maintenance_work_mem_bytes: usize,
}

impl HnswBuildState {
    fn new(score_metric: HnswScoreMetric, config: HnswConfig, parallel_workers: usize) -> Self {
        Self {
            graph: HnswGraph::new(score_metric.navigation_metric(), config),
            config,
            pending: Vec::new(),
            parallel_workers,
            score_metric,
            index_tuples: 0,
            dimensions: None,
            maintenance_work_mem_bytes: maintenance_work_mem_budget_bytes(),
        }
    }

    /// Builds `graph` from `pending` across `parallel_workers` threads.
    ///
    /// Pure in-memory work over owned `DenseVector`s — no PostgreSQL API
    /// call, including error-raising, happens on the spawned threads; each
    /// thread returns its first error (if any) instead, and this method
    /// raises on the caller's thread after every worker has joined. Calling
    /// `raise_sql_error` (PostgreSQL's longjmp-based error path) from a
    /// worker thread would corrupt backend state.
    fn finish_parallel_build(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let rows = std::mem::take(&mut self.pending);
        let builder = ConcurrentHnswBuilder::new(
            self.score_metric.navigation_metric(),
            self.config,
            rows.len(),
        );
        let workers = self.parallel_workers.min(rows.len()).max(1);
        let first_error = std::thread::scope(|scope| {
            let handles = rows
                .chunks(rows.len().div_ceil(workers))
                .map(|chunk| {
                    let builder = &builder;
                    scope.spawn(move || {
                        for (point_id, vector) in chunk {
                            builder.insert(*point_id, vector.clone())?;
                        }
                        Ok::<(), HnswError>(())
                    })
                })
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .filter_map(|handle| {
                    // A worker panic is re-raised on this (main) thread so
                    // the pg_guard boundary converts it like any other panic.
                    handle
                        .join()
                        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
                        .err()
                })
                .next()
        });
        if let Some(error) = first_error {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("failed to build HNSW graph: {error}"),
            );
        }
        match builder.finish() {
            Ok(graph) => self.graph = graph,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("parallel HNSW build produced an invalid graph: {error}"),
            ),
        }
    }

    fn enforce_maintenance_work_mem(&self) {
        let estimate = self.graph.memory_estimate();
        let estimated_bytes = estimate.total_bytes();
        if estimated_bytes > self.maintenance_work_mem_bytes {
            // The estimate covers only rows indexed so far, so a finished
            // build needs at least this much again in headroom; suggest 1.5x
            // rounded up to a whole MiB.
            let suggested_mib = estimated_bytes
                .saturating_mul(3)
                .div_ceil(2 * 1024 * 1024)
                .max(1);
            raise_sql_error_with_hint(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "HNSW build estimated memory {estimated_bytes} bytes exceeds maintenance_work_mem budget {} bytes after {} indexed vectors",
                    self.maintenance_work_mem_bytes, self.index_tuples
                ),
                format!(
                    "Raise the build budget for this session, for example \
                     SET maintenance_work_mem = '{suggested_mib}MB', then retry \
                     CREATE INDEX. The estimate covers rows indexed so far; a \
                     partially built index may need more than the suggestion."
                ),
            );
        }
    }
}

#[derive(Default)]
struct HnswScanState {
    prepared: bool,
    position: usize,
    candidate_limit: usize,
    candidates: Vec<HnswScanCandidate>,
    returned_heap_tids: BTreeSet<u64>,
    work: HnswScanWork,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HnswScanWork {
    page_visits: usize,
    node_reads: usize,
    candidates: usize,
    rechecks: usize,
    exact_strategy: bool,
}

thread_local! {
    static HNSW_LAST_SCAN_WORK: RefCell<HnswScanWork> = RefCell::new(HnswScanWork::default());
}

fn record_hnsw_scan_work(work: HnswScanWork) {
    HNSW_LAST_SCAN_WORK.with(|last| *last.borrow_mut() = work);
}

pub(crate) fn record_hnsw_exact_scan(candidates: usize) {
    record_hnsw_scan_work(HnswScanWork {
        candidates,
        rechecks: candidates,
        exact_strategy: true,
        ..HnswScanWork::default()
    });
}

#[pg_extern(schema = "pgcontext", name = "hnsw_last_scan_work")]
#[search_path(pg_catalog, pgcontext, public)]
fn hnsw_last_scan_work() -> TableIterator<
    'static,
    (
        name!(page_visits, i64),
        name!(node_reads, i64),
        name!(candidates, i64),
        name!(rechecks, i64),
        name!(exact_strategy, bool),
    ),
> {
    let work = HNSW_LAST_SCAN_WORK.with(|last| *last.borrow());
    TableIterator::once((
        scan_counter_to_sql(work.page_visits, "page_visits"),
        scan_counter_to_sql(work.node_reads, "node_reads"),
        scan_counter_to_sql(work.candidates, "candidates"),
        scan_counter_to_sql(work.rechecks, "rechecks"),
        work.exact_strategy,
    ))
}

fn scan_counter_to_sql(value: usize, label: &'static str) -> i64 {
    try_scan_counter_to_sql(value).unwrap_or_else(|value| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            format!("HNSW scan counter {label} exceeds PostgreSQL bigint range: {value}"),
        )
    })
}

fn try_scan_counter_to_sql(value: usize) -> Result<i64, usize> {
    i64::try_from(value).map_err(|_| value)
}

#[pg_extern(schema = "pgcontext", name = "_hnsw_candidates")]
#[search_path(pg_catalog, pgcontext, public)]
fn hnsw_candidates(
    index_relation: PgRelation,
    query: Vector,
    limit: i32,
) -> TableIterator<'static, (name!(heap_tid, String), name!(score, f32))> {
    let limit = search_limit_from_masked_candidates(limit);
    let query = match query.to_dense() {
        Ok(query) => query,
        Err(error) => raise_core_error(error),
    };
    let index_relation = index_relation.as_ptr();
    ensure_hnsw_candidate_relation(index_relation);
    // SAFETY: `PgRelation` holds AccessShareLock for a validated index relation;
    // the page scan owns every buffer pin and returns owned candidate values.
    let outcome = unsafe { hnsw_scan_candidates(index_relation, Some(&query), Some(limit.get())) };
    record_hnsw_scan_work(outcome.work);
    TableIterator::new(outcome.candidates.into_iter().map(|candidate| {
        let (block, offset) = u64_to_item_pointer_parts(candidate.heap_tid);
        (format!("({block},{offset})"), candidate.score)
    }))
}

#[pg_extern(schema = "pgcontext", name = "_hnsw_masked_candidates")]
#[search_path(pg_catalog, pgcontext, public)]
fn hnsw_masked_candidates(
    index_relation: PgRelation,
    query: Vector,
    allowed_heap_tids: AnyArray,
    limit: i32,
) -> TableIterator<'static, (name!(heap_tid, String), name!(score, f32))> {
    let limit = search_limit_from_masked_candidates(limit);
    let mask = candidate_mask_from_heap_tids(&allowed_heap_tids);
    let query = match query.to_dense() {
        Ok(query) => query,
        Err(error) => raise_core_error(error),
    };
    let index_relation = index_relation.as_ptr();
    let score_metric = ensure_hnsw_candidate_relation(index_relation);
    // SAFETY: The validated relation owns a live versioned metapage.
    let config = unsafe { hnsw_stored_config(index_relation, score_metric) };
    // SAFETY: `PgRelation` holds AccessShareLock and keeps the relation cache
    // entry live for this regular SQL function. The page adapter reads through
    // shared buffer pins only; no SPI or AM callback is involved.
    let outcome = unsafe {
        hnsw_page_graph_scan_candidates_with_mask(
            index_relation,
            score_metric,
            &query,
            config,
            limit,
            &mask,
        )
    };
    record_hnsw_scan_work(outcome.work);
    TableIterator::new(outcome.candidates.into_iter().map(|candidate| {
        let (block, offset) = u64_to_item_pointer_parts(candidate.heap_tid);
        (format!("({block},{offset})"), candidate.score)
    }))
}

fn ensure_hnsw_candidate_relation(index_relation: pg_sys::Relation) -> HnswScoreMetric {
    // SAFETY: `PgRelation` owns a live relation cache entry for this function.
    // Reading its class form only validates that the SQL caller passed an
    // index relation before HNSW opclass metadata is inspected below.
    let is_index = unsafe {
        !index_relation.is_null()
            && !(*index_relation).rd_rel.is_null()
            && u8::try_from((*(*index_relation).rd_rel).relkind).ok() == Some(pg_sys::RELKIND_INDEX)
    };
    if !is_index {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "HNSW candidate traversal requires an index relation",
        );
    }
    // SAFETY: the relation was checked as an index above and stays locked by
    // `PgRelation`; opclass metadata reads are therefore callback-independent
    // but otherwise identical to the regular AM scan validation.
    unsafe { hnsw_score_metric(index_relation) }
}

fn search_limit_from_masked_candidates(limit: i32) -> SearchLimit {
    let limit = match usize::try_from(limit) {
        Ok(limit) => limit,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("masked HNSW candidate limit must be positive: {limit}"),
        ),
    };
    match SearchLimit::new(limit) {
        Ok(limit) => limit,
        Err(error) => raise_core_error(error),
    }
}

fn candidate_mask_from_heap_tids(heap_tids: &AnyArray) -> CandidateMask {
    let array_oid = heap_tids.oid();
    if array_oid != pg_sys::TEXTARRAYOID && array_oid != pg_sys::TIDARRAYOID {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "masked HNSW candidates must be a tid[] or text[] of heap TIDs",
        );
    }
    let max = crate::settings::hnsw_mask_candidate_limit_from_guc();
    let mut points = Vec::new();
    for (position, value) in heap_tids.into_iter().enumerate() {
        if position >= max {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                format!("masked HNSW candidate set exceeds point budget {max}"),
            );
        }
        let Some(value) = value else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "masked HNSW candidate set cannot contain null heap TIDs",
            );
        };
        let heap_tid = if array_oid == pg_sys::TIDARRAYOID {
            // SAFETY: the array OID proves each non-null element is a
            // PostgreSQL ItemPointer datum valid for this immediate copy.
            let Some(value) = (unsafe { AnyElement::into::<pg_sys::ItemPointerData>(&value) })
            else {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "masked HNSW candidate set contains an invalid heap TID",
                );
            };
            item_pointer_to_u64(value)
        } else {
            // SAFETY: the remaining accepted array OID is text[], and each
            // datum is owned by PostgreSQL for this immediate conversion.
            let Some(value) = (unsafe { AnyElement::into::<String>(&value) }) else {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "masked HNSW candidate set contains an invalid heap TID",
                );
            };
            heap_tid_from_text(&value)
        };
        points.push(HnswPointId::new(heap_tid));
    }
    CandidateMask::only(points)
}

fn heap_tid_from_text(value: &str) -> u64 {
    let Some(value) = value
        .strip_prefix('(')
        .and_then(|value| value.strip_suffix(')'))
    else {
        raise_invalid_heap_tid(value);
    };
    let Some((block, offset)) = value.split_once(',') else {
        raise_invalid_heap_tid(value);
    };
    let block = match block.parse::<u32>() {
        Ok(block) => block,
        Err(_) => raise_invalid_heap_tid(value),
    };
    let offset = match offset.parse::<u16>() {
        Ok(offset) if offset > 0 => offset,
        _ => raise_invalid_heap_tid(value),
    };
    let mut heap_tid = pg_sys::ItemPointerData::default();
    // SAFETY: `block` and nonzero `offset` are validated scalar fields for a
    // PostgreSQL item pointer stored only long enough to encode its identity.
    unsafe { pg_sys::ItemPointerSet(&mut heap_tid, block, offset) };
    item_pointer_to_u64(heap_tid)
}

fn raise_invalid_heap_tid(value: &str) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        format!("invalid masked HNSW heap TID: {value}"),
    )
}

impl HnswScanState {
    fn reset(&mut self) {
        self.prepared = false;
        self.position = 0;
        self.candidate_limit = 0;
        self.candidates.clear();
        self.returned_heap_tids.clear();
        self.work = HnswScanWork::default();
    }

    fn next(&mut self) -> Option<HnswScanCandidate> {
        let candidate = self.candidates.get(self.position).copied();
        if candidate.is_some() {
            self.position += 1;
        }
        candidate
    }
}

#[derive(Debug, Clone, Copy)]
struct HnswScanCandidate {
    heap_tid: u64,
    score: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HnswScoreMetric {
    L2,
    NegativeInnerProduct,
    Cosine,
    L1,
    BitHamming,
    BitJaccard,
}

impl HnswScoreMetric {
    /// Returns the dense graph score preserving this metric's ordering.
    ///
    /// Bit metrics operate over the validated dense 0/1 storage form so their
    /// graph traversal score has the same ordering as the SQL operator.
    const fn navigation_metric(self) -> DistanceMetric {
        match self {
            Self::L2 => DistanceMetric::L2,
            Self::NegativeInnerProduct => DistanceMetric::NegativeInnerProduct,
            Self::Cosine => DistanceMetric::NegativeInnerProduct,
            Self::L1 => DistanceMetric::L1,
            Self::BitHamming => DistanceMetric::Hamming,
            Self::BitJaccard => DistanceMetric::Jaccard,
        }
    }

    fn prepare_vector(self, vector: DenseVector) -> Result<DenseVector, context_core::Error> {
        if self != Self::Cosine {
            return Ok(vector);
        }
        let mut values = vector.into_values();
        let norm_squared = values.iter().map(|value| value * value).sum::<f32>();
        if !norm_squared.is_finite() || norm_squared <= 0.0 {
            return Err(context_core::Error::InvalidVector(
                "cosine HNSW vectors must have a finite nonzero norm".to_owned(),
            ));
        }
        let inverse_norm = norm_squared.sqrt().recip();
        values.iter_mut().for_each(|value| *value *= inverse_norm);
        DenseVector::new(values)
    }

    const fn output_score(self, navigation_score: f32) -> f32 {
        match self {
            Self::Cosine => navigation_score + 1.0,
            _ => navigation_score,
        }
    }

    const fn storage_tag(self) -> u16 {
        match self {
            Self::L2 => 1,
            Self::NegativeInnerProduct => 2,
            Self::Cosine => 3,
            Self::L1 => 4,
            Self::BitHamming => 5,
            Self::BitJaccard => 6,
        }
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
// SAFETY: table_index_build_scan supplies callback-bounded relation, tuple
// arrays/TID, and the exclusive HnswBuildState pointer installed by ambuild.
// Datum and TID values are copied before return and no PostgreSQL pointer is
// retained.
unsafe extern "C-unwind" fn pgcontext_hnsw_build_callback(
    index_relation: pg_sys::Relation,
    tid: pg_sys::ItemPointer,
    values: *mut pg_sys::Datum,
    is_null: *mut bool,
    tuple_is_alive: bool,
    state: *mut c_void,
) {
    if !tuple_is_alive {
        return;
    }
    // SAFETY: This scope is stack-bound to the synchronous build visitor.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: Guaranteed by the synchronous table_index_build_scan contract.
    let index_relation = unsafe { scope.borrow(index_relation, "index relation") };
    // SAFETY: Any non-null TID/arrays are live for this visitor invocation.
    let tid = unsafe { scope.borrow_optional(tid) };
    // SAFETY: See the build-visitor callback contract above.
    let values = unsafe { scope.borrow_optional(values) };
    // SAFETY: See the build-visitor callback contract above.
    let is_null = unsafe { scope.borrow_optional(is_null) };
    // SAFETY: ambuild passes an exclusive HnswBuildState pointer for the
    // synchronous scan and retains the allocation on its stack.
    let state = unsafe { scope.borrow_mut(state.cast::<HnswBuildState>(), "HnswBuildState") };
    self::hnsw_build_callback_safe(index_relation, tid, values, is_null, state);
}

fn hnsw_build_callback_safe(
    index_relation: PgCallbackRef<'_, pg_sys::RelationData>,
    tid: Option<PgCallbackRef<'_, pg_sys::ItemPointerData>>,
    values: Option<PgCallbackRef<'_, pg_sys::Datum>>,
    is_null: Option<PgCallbackRef<'_, bool>>,
    mut state: PgCallbackMut<'_, HnswBuildState>,
) {
    let (Some(tid), Some(values), Some(is_null)) = (tid, values, is_null) else {
        return;
    };

    // SAFETY: The guarded build visitor established that the index relation
    // and non-null datum/null arrays remain live for this invocation.
    let Some(dense) = (unsafe {
        hnsw_vector_from_index_values(index_relation.as_ptr(), values.as_ptr(), is_null.as_ptr())
    }) else {
        return;
    };
    let dimension = dimension_to_u32(dense.dimension());
    let point_id = HnswPointId::new(item_pointer_to_u64(*tid.as_ref()));

    let state = state.as_mut();
    let dense = state
        .score_metric
        .prepare_vector(dense)
        .unwrap_or_else(|error| raise_core_error(error));
    if state.parallel_workers > 1 {
        // Collected, not inserted: `finish_parallel_build` builds the graph
        // across worker threads once the scan completes. Duplicate/dimension
        // validation still happens there (via `ConcurrentHnswBuilder`), just
        // deferred past this per-row callback.
        state.pending.push((point_id, dense));
    } else if let Err(error) = state.graph.insert(point_id, dense) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("failed to build HNSW graph: {error}"),
        );
    }
    state.dimensions = Some(dimension);
    state.index_tuples = state.index_tuples.saturating_add(1);
    // Exact graph-footprint accounting walks every node. Amortize it during
    // bulk construction; the build callback performs an unconditional final
    // check before publishing metadata or writing index records.
    if state.parallel_workers == 1 && state.index_tuples.is_multiple_of(256) {
        state.enforce_maintenance_work_mem();
    }
}

fn maintenance_work_mem_budget_bytes() -> usize {
    // SAFETY: PostgreSQL exposes `maintenance_work_mem` as a backend-local GUC
    // measured in KiB. Reading it during an index build does not mutate server
    // state.
    let budget_kib = unsafe { pg_sys::maintenance_work_mem };
    let budget_kib = usize::try_from(budget_kib).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("maintenance_work_mem must be non-negative: {budget_kib}"),
        )
    });
    budget_kib.checked_mul(1024).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "maintenance_work_mem budget exceeds addressable memory",
        )
    })
}

unsafe fn hnsw_scan_state_ptr(scan: pg_sys::IndexScanDesc) -> *mut HnswScanState {
    // SAFETY: PostgreSQL owns the scan descriptor for the active AM callback,
    // and ambeginscan initializes opaque with a memory-context-owned slot.
    let slot = unsafe {
        (*scan)
            .opaque
            .cast::<PgMemoryContextDropSlot<HnswScanState>>()
    };
    // SAFETY: A non-null opaque pointer was installed from a live registered
    // slot by ambeginscan and remains owned by the scan memory context.
    let Some(slot) = (unsafe { slot.as_mut() }) else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "HNSW scan state is not initialized",
        );
    };
    let Some(state) = slot.value_mut() else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "HNSW scan state was already released",
        );
    };
    ptr::from_mut(state)
}

unsafe fn prepare_hnsw_scan(scan: pg_sys::IndexScanDesc, state: &mut HnswScanState) {
    // SAFETY: The caller passes a live scan descriptor; `hnsw_orderby_query`
    // reads only PostgreSQL-owned scan keys for the current callback.
    let query = unsafe { hnsw_orderby_query(scan) };
    // SAFETY: The scan descriptor owns a valid index relation for the duration
    // of the AM callback.
    let outcome = unsafe { hnsw_scan_candidates((*scan).indexRelation, query.as_ref(), None) };

    state.prepared = true;
    state.position = 0;
    state.candidates = outcome.candidates;
    state.candidate_limit = outcome.requested_limit;
    state.work = outcome.work;
}

struct HnswScanCandidates {
    candidates: Vec<HnswScanCandidate>,
    work: HnswScanWork,
    requested_limit: usize,
}

unsafe fn hnsw_scan_candidates(
    index_relation: pg_sys::Relation,
    query: Option<&DenseVector>,
    requested_limit: Option<usize>,
) -> HnswScanCandidates {
    // SAFETY: The caller passes a valid index relation for the current AM
    // callback, and the first opclass input type is authoritative for this
    // single-column AM.
    let metric = unsafe { hnsw_score_metric(index_relation) };
    if let Some(query) = query {
        // SAFETY: The versioned metapage binds this opclass metric to the
        // persisted graph's construction configuration.
        let config = unsafe { hnsw_stored_config(index_relation, metric) };
        let requested_limit = requested_limit.unwrap_or(config.ef_search());
        // SAFETY: The page adapter reads only through shared pins while this
        // AM callback owns the relation. It fetches nodes/layers on demand and
        // never materializes the persisted graph.
        return unsafe {
            hnsw_page_graph_scan_candidates(index_relation, metric, query, config, requested_limit)
        };
    }
    // A non-ordered AM scan has no kNN strategy: it visits visible index
    // entries without manufacturing a score-ranked exact candidate set.
    // SAFETY: The scan owns a live index relation for the callback duration.
    let records = unsafe { read_hnsw_vector_records(index_relation) };
    let candidates = hnsw_unordered_scan_candidates(records);
    HnswScanCandidates {
        work: HnswScanWork {
            candidates: candidates.len(),
            ..HnswScanWork::default()
        },
        candidates,
        requested_limit: 0,
    }
}

fn hnsw_unordered_scan_candidates(records: Vec<HnswVectorRecord>) -> Vec<HnswScanCandidate> {
    let mut candidates = Vec::with_capacity(records.len());
    for record in records {
        if hnsw_record_is_tombstoned(&record) {
            continue;
        }
        candidates.push(HnswScanCandidate {
            heap_tid: hnsw_record_heap_tid(&record),
            score: 0.0,
        });
    }
    candidates
}

include!("hnsw_am_validation.rs");

include!("hnsw_am_page_storage.rs");
include!("hnsw_am_packed_cache.rs");
include!("hnsw_am_graph_read.rs");
include!("hnsw_am_graph_scan.rs");

fn build_result(heap_tuples: f64, index_tuples: f64) -> *mut pg_sys::IndexBuildResult {
    // SAFETY: PostgreSQL invokes AM build callbacks with a valid current memory
    // context. `IndexBuildResult` is a plain FFI result struct and every field is
    // initialized before ownership is transferred to PostgreSQL.
    let mut result = unsafe { PgBox::<pg_sys::IndexBuildResult, AllocatedByRust>::alloc0() };
    result.heap_tuples = heap_tuples;
    result.index_tuples = index_tuples;
    result.into_pg()
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_serving;
