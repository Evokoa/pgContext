//! Native PostgreSQL IVFFlat access method.

#![allow(
    clippy::too_many_arguments,
    reason = "PostgreSQL fixes access-method callback signatures"
)]

use context_codec::{CodecKind, PreparedQuantizedQuery, ReconstructionPolicy};
use context_index::{
    IvfCancellation, IvfCandidateBudget, IvfCentroidRead, IvfConfig, IvfError, IvfIterativePolicy,
    IvfListId, IvfPointId, IvfPostingPayload, IvfPostingRead, IvfPostingRef, IvfProbeWindow,
    IvfScorer, ivf_probe_order, search_ivf_probe_window_with_scorer,
};
use context_storage::{
    CodecArtifactView, DeltaRecord, DeltaRecordKind,
    IVF_PAGE_CHUNK_HEADER_BYTES as CHUNK_HEADER_BYTES, IVF_PAGE_META_BYTES as META_BYTES,
    IVF_PAGE_META_VERSION as META_VERSION, IvfPageChunkKind as ChunkKind,
    IvfPageMeta as IvfflatMeta, decode_delta_record, decode_ivf_delta_chunk_identity,
    decode_ivf_page_chunk, encode_delta_record, encode_ivf_page_chunk,
};
use pgrx::itemptr::{item_pointer_set_all, item_pointer_to_u64, u64_to_item_pointer};
use pgrx::prelude::*;
use pgrx::{AllocatedByRust, JsonB, PgBox, PgMemoryContexts, PgRelation};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;

use crate::error::raise_sql_error;
use crate::hnsw_am::ffi_boundary::{PgCallbackScope, PgMemoryContextDropSlot};
use crate::hnsw_am::{HnswOrderByContract, HnswScoreMetric};

#[allow(
    dead_code,
    reason = "the executable callback inventory is consumed by tests and the source guard"
)]
mod callback_contract;
mod external_build;
mod options;

#[cfg(feature = "pg_test")]
pub(crate) fn test_ivfflat_parallel_transcode_bound(batch_count: usize) -> (usize, usize) {
    external_build::test_parallel_transcode_bound(batch_count)
}

#[cfg(feature = "pg_test")]
#[pg_extern]
fn ivfflat_test_append_tombstone(index: PgRelation, tid: pg_sys::ItemPointerData) {
    let relation = index.as_ptr();
    let heap_tid = item_pointer_to_u64(tid);
    // SAFETY: PgRelation retains the test index and its AccessShare lock for
    // this complete synchronous call.
    let meta = unsafe { read_meta(relation) };
    // SAFETY: the same live relation owns validated opclass metadata.
    let metric = unsafe { crate::hnsw_am::hnsw_score_metric(relation) };
    let record = DeltaRecord::tombstone(heap_tid);
    let payload = encode_delta_record(&record).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("failed to encode IVFFlat test tombstone: {error}"),
        )
    });
    // SAFETY: the append lock serializes this deliberately injected test
    // record with normal DML and maintenance writers.
    unsafe {
        crate::hnsw_am::serialize_hnsw_insert(relation);
        append_delta_record(relation, metric, &payload, meta.dimensions as usize);
    }
}

const MAX_PAGE_ITEM_BYTES: usize = pg_sys::BLCKSZ as usize - 128;
const CHUNK_DATA_BYTES: usize = MAX_PAGE_ITEM_BYTES - CHUNK_HEADER_BYTES;
const DIRECTORY_ENTRY_BYTES: usize = 16;
const IVFFLAT_DELTA_COMPACTION_THRESHOLD: u64 = 10_000;
const FIRST_ITEM: pg_sys::OffsetNumber = 1;
static HANDLER_FINFO: pg_sys::Pg_finfo_record = pg_sys::Pg_finfo_record { api_version: 1 };

/// A `regclass` datum decoded without opening or locking the named relation.
///
/// `PgRelation::from_datum` takes `AccessShareLock`, which cannot be upgraded
/// safely by two concurrent manual compactors. This newtype keeps SQL name
/// resolution while deferring every relation lock to the compaction boundary.
#[derive(Debug, Clone, Copy)]
struct UnlockedRegclass(pg_sys::Oid);

impl FromDatum for UnlockedRegclass {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _typoid: pg_sys::Oid,
    ) -> Option<Self> {
        // SAFETY: PostgreSQL passes regclass by value using the OID datum
        // representation; decoding the scalar does not dereference relation
        // storage or acquire a relation lock.
        unsafe { pg_sys::Oid::from_datum(datum, is_null) }.map(Self)
    }
}

impl IntoDatum for UnlockedRegclass {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        Some(self.0.into())
    }

    fn type_oid() -> pg_sys::Oid {
        pg_sys::REGCLASSOID
    }
}

// SAFETY: `UnlockedRegclass` has the same by-value OID representation as its
// declared SQL `regclass` mapping, and `FromDatum` performs only that scalar
// conversion.
unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for UnlockedRegclass {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        let index = arg.index();
        // SAFETY: pg_extern's generated wrapper supplies the datum for the
        // declared regclass argument at this position.
        unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                format!("compact_ivfflat argument {index} must not be null"),
            )
        })
    }
}

impl_sql_translatable!(UnlockedRegclass, "regclass");

#[derive(Debug, Clone, Copy)]
struct ListExtent {
    start: u64,
    end: u64,
}

struct BuildState {
    metric: HnswScoreMetric,
    collector: external_build::ExternalBuildCollector,
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    heap_tid: u64,
    score: f32,
}

#[derive(Debug)]
enum PagePostingPayload {
    Dense(Vec<f32>),
    Encoded(Vec<u8>),
}

#[derive(Debug)]
struct PagePosting {
    point_id: IvfPointId,
    payload: PagePostingPayload,
}

#[derive(Debug)]
struct PageIvfAdapter {
    metric: context_core::DistanceMetric,
    dimensions: usize,
    centroids: Vec<Vec<f32>>,
    lists: Vec<Vec<PagePosting>>,
}

impl IvfCentroidRead for PageIvfAdapter {
    fn metric(&self) -> context_core::DistanceMetric {
        self.metric
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn centroid_count(&self) -> usize {
        self.centroids.len()
    }

    fn centroid(&self, list_id: IvfListId) -> Option<&[f32]> {
        self.centroids.get(list_id.get()).map(Vec::as_slice)
    }
}

impl IvfPostingRead for PageIvfAdapter {
    fn list_len(&self, list_id: IvfListId) -> Result<usize, IvfError> {
        self.lists
            .get(list_id.get())
            .map(Vec::len)
            .ok_or(IvfError::CorruptGeneration {
                reason: "page adapter list id is outside the directory",
            })
    }

    fn posting(&self, list_id: IvfListId, offset: usize) -> Result<IvfPostingRef<'_>, IvfError> {
        let posting = self
            .lists
            .get(list_id.get())
            .and_then(|list| list.get(offset))
            .ok_or(IvfError::CorruptGeneration {
                reason: "page adapter posting offset is outside its list",
            })?;
        Ok(match &posting.payload {
            PagePostingPayload::Dense(vector) => IvfPostingRef::dense(posting.point_id, vector),
            PagePostingPayload::Encoded(code) => IvfPostingRef::encoded(posting.point_id, code),
        })
    }
}

struct PagePreparedScorer<'a> {
    metric: context_core::DistanceMetric,
    query: &'a [f32],
    quantized: Option<&'a PreparedQuantizedQuery>,
}

struct PgInterruptCancellation;

impl IvfCancellation for PgInterruptCancellation {
    fn cancelled(&self) -> bool {
        pg_sys::check_for_interrupts!();
        false
    }
}

impl IvfScorer for PagePreparedScorer<'_> {
    fn score(&self, posting: IvfPostingRef<'_>) -> Result<f32, IvfError> {
        match (posting.payload(), self.quantized) {
            (IvfPostingPayload::Dense(vector), None) => {
                Ok(self.metric.distance_slices(self.query, vector)?)
            }
            (IvfPostingPayload::Encoded(code), Some(scorer)) => {
                scorer.score(code).map_err(|_| IvfError::CorruptGeneration {
                    reason: "quantized posting code is invalid",
                })
            }
            _ => Err(IvfError::CorruptGeneration {
                reason: "posting representation disagrees with codec binding",
            }),
        }
    }
}

#[derive(Debug)]
struct ScanState {
    prepared: bool,
    position: usize,
    contract: Option<HnswOrderByContract>,
    candidates: Vec<Candidate>,
    returned_tids: BTreeSet<u64>,
    delta_overlay: BTreeMap<u64, Option<f32>>,
    delta_loaded: bool,
    initial_probes: usize,
    current_probes: usize,
    max_probes: usize,
    probed_lists: usize,
    iterative: crate::settings::IvfflatIterativeScan,
    widening_rounds: usize,
    total_visited_lists: usize,
    total_visited_postings: usize,
    total_delta_records: usize,
}

impl Default for ScanState {
    fn default() -> Self {
        Self {
            prepared: false,
            position: 0,
            contract: None,
            candidates: Vec::new(),
            returned_tids: BTreeSet::new(),
            delta_overlay: BTreeMap::new(),
            delta_loaded: false,
            initial_probes: 0,
            current_probes: 0,
            max_probes: 0,
            probed_lists: 0,
            iterative: crate::settings::IvfflatIterativeScan::Off,
            widening_rounds: 0,
            total_visited_lists: 0,
            total_visited_postings: 0,
            total_delta_records: 0,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct IvfflatScanWork {
    requested_probes: usize,
    visited_lists: usize,
    visited_postings: usize,
    delta_records: usize,
    candidates: usize,
    exact_rerank_candidates: usize,
    widening_rounds: usize,
    codec_mode: u16,
    completion_reason: &'static str,
    generation: u64,
}

thread_local! {
    static IVFFLAT_LAST_SCAN_WORK: RefCell<IvfflatScanWork> = const {
        RefCell::new(IvfflatScanWork {
            requested_probes: 0,
            visited_lists: 0,
            visited_postings: 0,
            delta_records: 0,
            candidates: 0,
            exact_rerank_candidates: 0,
            widening_rounds: 0,
            codec_mode: 0,
            completion_reason: "not_run",
            generation: 0,
        })
    };
}

impl ScanState {
    fn reset(&mut self) {
        self.prepared = false;
        self.position = 0;
        self.contract = None;
        self.candidates.clear();
        self.returned_tids.clear();
        self.delta_overlay.clear();
        self.delta_loaded = false;
        self.initial_probes = 0;
        self.current_probes = 0;
        self.max_probes = 0;
        self.probed_lists = 0;
        self.iterative = crate::settings::IvfflatIterativeScan::Off;
        self.widening_rounds = 0;
        self.total_visited_lists = 0;
        self.total_visited_postings = 0;
        self.total_delta_records = 0;
    }

    fn widen(&mut self) -> bool {
        if self.iterative != crate::settings::IvfflatIterativeScan::RelaxedOrder
            || self.current_probes >= self.max_probes
        {
            return false;
        }
        self.current_probes = self
            .current_probes
            .saturating_add(self.initial_probes)
            .min(self.max_probes);
        self.widening_rounds = self.widening_rounds.saturating_add(1);
        self.prepared = false;
        self.position = 0;
        self.candidates.clear();
        true
    }
}

/// Returns PostgreSQL V1 metadata for [`pgcontext_ivfflat_handler`].
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_finfo_pgcontext_ivfflat_handler() -> *const pg_sys::Pg_finfo_record {
    &HANDLER_FINFO
}

/// Returns the PostgreSQL index access-method routine.
///
/// # Safety
///
/// PostgreSQL must invoke this through its V1 function manager with a live
/// call-info pointer and current memory context.
#[pg_guard]
#[allow(unused_qualifications)]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn pgcontext_ivfflat_handler(
    fcinfo: pg_sys::FunctionCallInfo,
) -> pg_sys::Datum {
    // SAFETY: the guarded V1 entrypoint supplies a call-bounded pointer.
    let scope = unsafe { PgCallbackScope::new() };
    // SAFETY: the pointer remains live for this guarded invocation.
    let _fcinfo = unsafe { scope.borrow(fcinfo, "FunctionCallInfo") };
    let mut routine = unsafe { PgBox::<pg_sys::IndexAmRoutine, AllocatedByRust>::alloc0() };
    *routine = ivfflat_routine();
    pg_sys::Datum::from(routine.into_pg())
}

fn ivfflat_routine() -> pg_sys::IndexAmRoutine {
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
        amcanbuildparallel: true,
        amcaninclude: false,
        ammarkpos: None,
        amrestrpos: None,
        amusemaintenanceworkmem: true,
        amkeytype: pg_sys::InvalidOid,
        ambuild: Some(ivfflat_build),
        ambuildempty: Some(ivfflat_build_empty),
        aminsert: Some(ivfflat_insert),
        aminsertcleanup: None,
        ambulkdelete: Some(ivfflat_bulk_delete),
        amvacuumcleanup: Some(ivfflat_vacuum_cleanup),
        amoptions: Some(options::pgcontext_ivfflat_options),
        amcostestimate: Some(ivfflat_cost_estimate),
        ambuildphasename: Some(ivfflat_build_phase_name),
        amvalidate: Some(ivfflat_validate),
        ambeginscan: Some(ivfflat_begin_scan),
        amrescan: Some(ivfflat_rescan),
        amgettuple: Some(ivfflat_get_tuple),
        amgetbitmap: None,
        amendscan: Some(ivfflat_end_scan),
        ..pg_sys::IndexAmRoutine::default()
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn ivfflat_build_phase_name(phase: i64) -> *mut std::ffi::c_char {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    match phase {
        2 => c"initializing".as_ptr().cast_mut(),
        3 => c"sampling source rows".as_ptr().cast_mut(),
        4 => c"training and assigning lists".as_ptr().cast_mut(),
        5 => c"writing centroids and list directory".as_ptr().cast_mut(),
        6 => c"writing quantization codebook".as_ptr().cast_mut(),
        7 => c"writing postings".as_ptr().cast_mut(),
        8 => c"validating and publishing".as_ptr().cast_mut(),
        _ => ptr::null_mut(),
    }
}

fn update_build_phase(phase: i64) {
    // SAFETY: PostgreSQL owns the active CREATE INDEX progress slot and
    // ignores updates when the callback is not running under that command.
    unsafe {
        pg_sys::pgstat_progress_update_param(
            pg_sys::PROGRESS_CREATEIDX_SUBPHASE.cast_signed(),
            phase,
        )
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_build(
    heap_relation: pg_sys::Relation,
    index_relation: pg_sys::Relation,
    index_info: *mut pg_sys::IndexInfo,
) -> *mut pg_sys::IndexBuildResult {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    update_build_phase(2);
    // SAFETY: PostgreSQL owns all pointers for this guarded build callback.
    let metric = unsafe { crate::hnsw_am::hnsw_score_metric(index_relation) };
    let requested_lists = unsafe { options::list_count(index_relation) };
    let codec_spec = unsafe { options::codec_spec(index_relation) };
    if codec_spec.kind() != CodecKind::Plain
        && matches!(
            metric.navigation_metric(),
            context_core::DistanceMetric::Hamming | context_core::DistanceMetric::Jaccard
        )
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "IVFFlat SQ8/PQ quantization requires a continuous dense metric",
        );
    }
    let mut state = BuildState {
        metric,
        collector: external_build::ExternalBuildCollector::new(
            metric.navigation_metric(),
            requested_lists,
            crate::settings::ivfflat_build_parallel_workers_from_guc(),
            maintenance_work_mem_budget_bytes(),
            codec_spec,
        ),
    };
    update_build_phase(3);
    // SAFETY: the synchronous heap scan retains every pointer and state for
    // the callback sequence; the visitor copies all datum/TID data.
    let heap_tuples = unsafe {
        pg_sys::table_index_build_scan(
            heap_relation,
            index_relation,
            index_info,
            true,
            true,
            Some(ivfflat_build_callback),
            ptr::addr_of_mut!(state).cast::<c_void>(),
            ptr::null_mut(),
        )
    };
    // SAFETY: the building relation is exclusively writable.
    unsafe { initialize_metapage(index_relation) };
    let tuples = state.collector.tuple_count();
    if tuples == 0 {
        // SAFETY: block zero was initialized immediately above.
        unsafe { publish_meta(index_relation, IvfflatMeta::empty()) };
        unsafe { wal_log_build_pages(index_relation) };
        return crate::hnsw_am::build_result(heap_tuples, 0.0);
    }

    update_build_phase(4);
    let concurrent = !index_info.is_null() && unsafe { (*index_info).ii_Concurrent };
    // SAFETY: PostgreSQL retains both relation locks and IndexInfo for the
    // complete synchronous parallel-worker lifecycle.
    let output = unsafe {
        state
            .collector
            .finish_parallel(heap_relation, index_relation, metric, concurrent)
    };
    let generation_start = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    // SAFETY: the build owns this relation and publishes generation one.
    unsafe { write_ivfflat_generation(index_relation, metric, output, 1, generation_start) };
    // PostgreSQL build callbacks also emit the standard new-page range record.
    unsafe { wal_log_build_pages(index_relation) };
    #[allow(
        clippy::cast_precision_loss,
        reason = "PostgreSQL stores index tuple counts as f64 planner estimates"
    )]
    let index_tuples = tuples as f64;
    crate::hnsw_am::build_result(heap_tuples, index_tuples)
}

unsafe fn write_ivfflat_generation(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
    mut output: external_build::ExternalBuildOutput,
    generation: u64,
    generation_start: u32,
) -> IvfflatMeta {
    let dimensions = u32::try_from(output.dimensions).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "IVFFlat dimensions exceed metapage range",
        )
    });
    let centroid_bytes = encode_vectors(&output.centroids);
    let directory_bytes = output.directory;
    update_build_phase(5);
    // SAFETY: the build owns the new relation and writes immutable, checksummed
    // section pages before publishing their extents through block zero.
    let centroid_start = generation_start;
    let centroid_end = unsafe {
        write_chunk_pages(
            index_relation,
            ChunkKind::Centroids,
            0,
            &centroid_bytes,
            centroid_start,
        )
    };
    let directory_start = centroid_end;
    let directory_end = unsafe {
        write_chunk_pages(
            index_relation,
            ChunkKind::Directory,
            0,
            &directory_bytes,
            directory_start,
        )
    };
    let codec_start = directory_end;
    update_build_phase(6);
    let (codec_end, codec_bytes) = if let Some(codec) = output.codec_artifact.as_deref() {
        (
            unsafe { write_chunk_pages(index_relation, ChunkKind::Codec, 0, codec, codec_start) },
            codec.len(),
        )
    } else {
        (codec_start, 0)
    };
    let posting_start = codec_end;
    update_build_phase(7);
    let posting_end = unsafe {
        write_chunk_pages_from_temp(
            index_relation,
            ChunkKind::Postings,
            0,
            &mut output.postings,
            output.posting_bytes,
            posting_start,
        )
    };
    let meta = IvfflatMeta {
        metric_tag: metric.storage_tag(),
        dimensions,
        lists: u32::try_from(output.lists).unwrap_or(u32::MAX),
        tuples: u64::try_from(output.tuples).unwrap_or(u64::MAX),
        centroid_bytes: u64::try_from(centroid_bytes.len()).unwrap_or(u64::MAX),
        directory_bytes: u64::try_from(directory_bytes.len()).unwrap_or(u64::MAX),
        codec_bytes: u64::try_from(codec_bytes).unwrap_or(u64::MAX),
        posting_bytes: u64::try_from(output.posting_bytes).unwrap_or(u64::MAX),
        centroid_start,
        centroid_end,
        directory_start,
        directory_end,
        codec_start,
        codec_end,
        posting_start,
        posting_end,
        delta_start: posting_end,
        delta_end: posting_end,
        delta_count: 0,
        generation,
        codec_revision: if codec_bytes == 0 {
            0
        } else {
            output.codec_revision
        },
        codec_code_width: u32::try_from(output.code_width).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat codec width exceeds metapage range",
            )
        }),
        codec_mode: output.codec_mode,
        build_workers: u16::try_from(output.parallel_workers).unwrap_or(u16::MAX),
    };
    update_build_phase(8);
    // SAFETY: every artifact page is WAL-logged and complete before publication.
    unsafe { publish_meta(index_relation, meta) };
    meta
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_build_callback(
    index_relation: pg_sys::Relation,
    tid: pg_sys::ItemPointer,
    values: *mut pg_sys::Datum,
    is_null: *mut bool,
    tuple_is_alive: bool,
    state: *mut c_void,
) {
    // SAFETY: PostgreSQL entered this guarded build-visitor callback.
    let _scope = unsafe { PgCallbackScope::new() };
    if !tuple_is_alive || tid.is_null() || state.is_null() {
        return;
    }
    // SAFETY: table_index_build_scan supplies one live datum/null entry for
    // this single-column AM and the callback copies the vector immediately.
    let Some(vector) =
        (unsafe { crate::hnsw_am::hnsw_vector_from_index_values(index_relation, values, is_null) })
    else {
        return;
    };
    // SAFETY: ambuild supplied this exclusive stack-owned state pointer.
    let state = unsafe { &mut *state.cast::<BuildState>() };
    let Some(vector) = state
        .metric
        .prepare_vector(vector)
        .unwrap_or_else(|error| crate::error::raise_core_error(error))
    else {
        return;
    };
    // SAFETY: the heap scan supplies a live item pointer for this invocation.
    let heap_tid = item_pointer_to_u64(unsafe { *tid });
    state.collector.push(heap_tid, vector.into_values());
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_build_empty(index_relation: pg_sys::Relation) {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL grants exclusive initialization access.
    unsafe {
        initialize_metapage(index_relation);
        publish_meta(index_relation, IvfflatMeta::empty());
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_insert(
    index_relation: pg_sys::Relation,
    values: *mut pg_sys::Datum,
    is_null: *mut bool,
    heap_tid: pg_sys::ItemPointer,
    _heap_relation: pg_sys::Relation,
    _check_unique: pg_sys::IndexUniqueCheck::Type,
    _index_unchanged: bool,
    _index_info: *mut pg_sys::IndexInfo,
) -> bool {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    if index_relation.is_null() || values.is_null() || is_null.is_null() || heap_tid.is_null() {
        return false;
    }
    // SAFETY: the callback supplies a live single-column datum/null array and
    // the decoder copies the vector before returning.
    let Some(vector) =
        (unsafe { crate::hnsw_am::hnsw_vector_from_index_values(index_relation, values, is_null) })
    else {
        return false;
    };
    // SAFETY: the live relation certifies its opclass metric.
    let metric = unsafe { crate::hnsw_am::hnsw_score_metric(index_relation) };
    let Some(vector) = metric
        .prepare_vector(vector)
        .unwrap_or_else(|error| crate::error::raise_core_error(error))
    else {
        return false;
    };
    // SAFETY: the callback's TID remains live for this immediate copy.
    let heap_tid = item_pointer_to_u64(unsafe { *heap_tid });
    let record = DeltaRecord::live(heap_tid, vector.into_values()).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid IVFFlat delta record: {error}"),
        )
    });
    let payload = encode_delta_record(&record).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("failed to encode IVFFlat delta record: {error}"),
        )
    });
    // SAFETY: this transaction-scoped relation lock serializes the append
    // cursor and metapage publication for the index.
    unsafe { crate::hnsw_am::serialize_hnsw_insert(index_relation) };
    // SAFETY: payload is fully validated and owned before Generic WAL starts.
    unsafe { append_delta_record(index_relation, metric, &payload, record.vector.len()) };
    false
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_cost_estimate(
    root: *mut pg_sys::PlannerInfo,
    path: *mut pg_sys::IndexPath,
    loop_count: f64,
    startup: *mut pg_sys::Cost,
    total: *mut pg_sys::Cost,
    selectivity: *mut pg_sys::Selectivity,
    correlation: *mut f64,
    pages: *mut f64,
) {
    // SAFETY: PostgreSQL entered this guarded planner callback.
    let _scope = unsafe { PgCallbackScope::new() };
    if path.is_null()
        || startup.is_null()
        || total.is_null()
        || selectivity.is_null()
        || correlation.is_null()
        || pages.is_null()
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "IVFFlat planner callback received a null pointer",
        );
    }
    // SAFETY: non-null planner path is live for this callback.
    if unsafe { (*path).indexorderbys.is_null() } {
        // SAFETY: output pointers are distinct writable planner slots.
        unsafe {
            *startup = f64::INFINITY;
            *total = f64::INFINITY;
            *selectivity = 0.0;
            *correlation = 0.0;
            *pages = 0.0;
            #[cfg(feature = "pg18")]
            {
                (*path).path.disabled_nodes = 2;
            }
        }
        return;
    }
    // SAFETY: PostgreSQL owns all planner inputs and this zeroed output value.
    let mut costs = unsafe { std::mem::zeroed::<pg_sys::GenericCosts>() };
    unsafe { pg_sys::genericcostestimate(root, path, loop_count, &mut costs) };
    let probes = u32::try_from(crate::settings::ivfflat_probes_from_guc())
        .map(f64::from)
        .unwrap_or(f64::from(u32::MAX));
    // SAFETY: IndexOptInfo is live for the planner callback; opening its OID
    // only borrows the AccessShareLock already held by the planner.
    let lists = unsafe {
        if (*path).indexinfo.is_null() || (*(*path).indexinfo).indexoid == pg_sys::InvalidOid {
            1.0
        } else {
            let relation = PgRelation::open((*(*path).indexinfo).indexoid);
            f64::from(read_meta(relation.as_ptr()).lists.max(1))
        }
    };
    let ratio = (probes / lists).clamp(1.0 / lists, 1.0);
    // SAFETY: output pointers are distinct and writable for this callback.
    unsafe {
        *startup = costs.indexTotalCost * ratio;
        *total = costs.indexTotalCost * ratio;
        *selectivity = costs.indexSelectivity;
        *correlation = costs.indexCorrelation;
        *pages = costs.numIndexPages;
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_validate(opclass_oid: pg_sys::Oid) -> bool {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    // SAFETY: PostgreSQL supplies a copied pg_opclass OID and the validator
    // retains no syscache pointers after returning.
    unsafe { crate::hnsw_am::validate_vector_opclass_for_method(opclass_oid, c"pgcontext_ivfflat") }
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_begin_scan(
    index_relation: pg_sys::Relation,
    nkeys: std::ffi::c_int,
    norderbys: std::ffi::c_int,
) -> pg_sys::IndexScanDesc {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    if nkeys < 0 || norderbys < 0 || nkeys > 1 || norderbys > 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "IVFFlat supports at most one scan key and one order-by key",
        );
    }
    // SAFETY: PostgreSQL supplies a live relation and validated counts.
    let scan = unsafe { pg_sys::RelationGetIndexScan(index_relation, nkeys, norderbys) };
    if scan.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "PostgreSQL did not allocate an IVFFlat scan descriptor",
        );
    }
    // SAFETY: the descriptor is PostgreSQL-allocated in one owning context.
    let mut context = unsafe { PgMemoryContexts::of(scan.cast::<c_void>()) }.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "IVFFlat scan descriptor has no memory context",
        )
    });
    let slot = context.leak_and_drop_on_delete(PgMemoryContextDropSlot::new(ScanState::default()));
    // SAFETY: descriptor fields are exclusively initialized here.
    unsafe {
        (*scan).opaque = slot.cast::<c_void>();
        if norderbys > 0 {
            (*scan).xs_orderbyvals =
                pg_sys::palloc0(std::mem::size_of::<pg_sys::Datum>()).cast::<pg_sys::Datum>();
            (*scan).xs_orderbynulls = pg_sys::palloc0(std::mem::size_of::<bool>()).cast::<bool>();
        }
    }
    scan
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_rescan(
    scan: pg_sys::IndexScanDesc,
    keys: pg_sys::ScanKey,
    nkeys: std::ffi::c_int,
    orderbys: pg_sys::ScanKey,
    norderbys: std::ffi::c_int,
) {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    if scan.is_null() || nkeys < 0 || norderbys < 0 || nkeys > 1 || norderbys > 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "invalid IVFFlat rescan extent",
        );
    }
    // SAFETY: source arrays contain at most one live scan key, and descriptor
    // destinations were allocated for their fixed capacities.
    unsafe {
        if nkeys == 1 {
            ptr::copy_nonoverlapping(keys, (*scan).keyData, 1);
        }
        if norderbys == 1 {
            ptr::copy_nonoverlapping(orderbys, (*scan).orderByData, 1);
        }
        scan_state(scan).reset();
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_get_tuple(
    scan: pg_sys::IndexScanDesc,
    _direction: pg_sys::ScanDirection::Type,
) -> bool {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    if scan.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "IVFFlat scan descriptor is null",
        );
    }
    // SAFETY: the descriptor and memory-context-owned state are live.
    let state = unsafe { scan_state(scan) };
    loop {
        if !state.prepared {
            // SAFETY: scan owns its relation and order-by datum for this callback.
            unsafe { prepare_scan(scan, state) };
        }
        while let Some(candidate) = state.candidates.get(state.position).copied() {
            state.position += 1;
            // SAFETY: visibility is checked against this descriptor's snapshot.
            let Some((block, offset)) =
                (unsafe { crate::hnsw_am::hnsw_visible_heap_tid(scan, candidate.heap_tid) })
            else {
                continue;
            };
            // SAFETY: the descriptor owns writable result and order-by slots.
            unsafe {
                item_pointer_set_all(&mut (*scan).xs_heaptid, block, offset);
                (*scan).xs_heap_continue = false;
                (*scan).xs_recheck = false;
                if (*scan).numberOfOrderBys > 0 {
                    let contract = state.contract.unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "IVFFlat order-by contract is missing",
                        )
                    });
                    (*scan).xs_recheckorderby = true;
                    crate::hnsw_am::store_hnsw_orderby_distance(
                        scan,
                        contract,
                        candidate.score,
                        true,
                        if state.iterative == crate::settings::IvfflatIterativeScan::RelaxedOrder {
                            crate::hnsw_am::OrderByRecheckKey::Approximate
                        } else {
                            crate::hnsw_am::OrderByRecheckKey::Conservative
                        },
                    );
                } else {
                    (*scan).xs_recheckorderby = false;
                }
            }
            state.returned_tids.insert(candidate.heap_tid);
            IVFFLAT_LAST_SCAN_WORK.with(|last| {
                let mut work = last.borrow_mut();
                work.exact_rerank_candidates = work.exact_rerank_candidates.saturating_add(1);
            });
            return true;
        }
        if !state.widen() {
            return false;
        }
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_end_scan(scan: pg_sys::IndexScanDesc) {
    // SAFETY: PostgreSQL entered this guarded AM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    if scan.is_null() {
        return;
    }
    // SAFETY: opaque is null or the slot installed by begin-scan.
    unsafe {
        let opaque = (*scan).opaque;
        (*scan).opaque = ptr::null_mut();
        if let Some(slot) = opaque.cast::<PgMemoryContextDropSlot<ScanState>>().as_mut() {
            drop(slot.take());
        }
    }
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_bulk_delete(
    info: *mut pg_sys::IndexVacuumInfo,
    stats: *mut pg_sys::IndexBulkDeleteResult,
    callback: pg_sys::IndexBulkDeleteCallback,
    callback_state: *mut c_void,
) -> *mut pg_sys::IndexBulkDeleteResult {
    // SAFETY: PostgreSQL entered this guarded VACUUM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    let result = if stats.is_null() {
        // SAFETY: PostgreSQL owns the current memory context.
        unsafe { PgBox::<pg_sys::IndexBulkDeleteResult, AllocatedByRust>::alloc0().into_pg() }
    } else {
        stats
    };
    let Some(callback) = callback else {
        return result;
    };
    if info.is_null() {
        return result;
    }
    // VACUUM's table lock is acquired before the per-index append lock, the
    // same global order used by the HNSW maintenance path.
    let index_relation = unsafe { (*info).index };
    unsafe { crate::hnsw_am::serialize_hnsw_insert(index_relation) };
    let meta = unsafe { read_meta(index_relation) };
    let metric = unsafe { crate::hnsw_am::hnsw_score_metric(index_relation) };
    let live = unsafe { read_all_live_tids(index_relation, meta) };
    let live_count = live.len();
    let mut removed = 0_u64;
    for heap_tid in live {
        let mut tid = pg_sys::ItemPointerData::default();
        u64_to_item_pointer(heap_tid, &mut tid);
        if unsafe { callback(ptr::addr_of_mut!(tid), callback_state) } {
            let record = DeltaRecord::tombstone(heap_tid);
            let payload = encode_delta_record(&record).unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("failed to encode IVFFlat vacuum tombstone: {error}"),
                )
            });
            unsafe {
                append_delta_record(index_relation, metric, &payload, meta.dimensions as usize)
            };
            removed = removed.saturating_add(1);
        }
    }
    // SAFETY: result is newly allocated or supplied as a writable stats row.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "PostgreSQL vacuum statistics expose tuple estimates as f64"
    )]
    unsafe {
        (*result).tuples_removed += removed as f64;
        (*result).num_index_tuples = (live_count.saturating_sub(removed as usize)) as f64;
        (*result).estimated_count = false;
    }
    result
}

#[pg_guard]
#[allow(unused_qualifications)]
unsafe extern "C-unwind" fn ivfflat_vacuum_cleanup(
    info: *mut pg_sys::IndexVacuumInfo,
    stats: *mut pg_sys::IndexBulkDeleteResult,
) -> *mut pg_sys::IndexBulkDeleteResult {
    // SAFETY: PostgreSQL entered this guarded VACUUM callback.
    let _scope = unsafe { PgCallbackScope::new() };
    let result = if stats.is_null() {
        // SAFETY: PostgreSQL owns the current memory context.
        unsafe { PgBox::<pg_sys::IndexBulkDeleteResult, AllocatedByRust>::alloc0().into_pg() }
    } else {
        stats
    };
    if !info.is_null() {
        // SAFETY: PostgreSQL supplies live vacuum info and writable stats.
        unsafe {
            (*result).num_pages = pg_sys::RelationGetNumberOfBlocksInFork(
                (*info).index,
                pg_sys::ForkNumber::MAIN_FORKNUM,
            );
            (*result).num_index_tuples = (*info).num_heap_tuples;
            (*result).estimated_count = true;
        }
    }
    result
}

unsafe fn prepare_scan(scan: pg_sys::IndexScanDesc, state: &mut ScanState) {
    // SAFETY: caller owns a live descriptor and index relation.
    let metric = unsafe { crate::hnsw_am::hnsw_score_metric((*scan).indexRelation) };
    // SAFETY: order-by metadata is valid for this scan.
    let query = unsafe { crate::hnsw_am::hnsw_orderby_query(scan) };
    let Some(query) = query else {
        state.prepared = true;
        return;
    };
    let Some(query) = metric
        .prepare_vector(query)
        .unwrap_or_else(|error| crate::error::raise_core_error(error))
    else {
        state.prepared = true;
        return;
    };
    // SAFETY: block zero is pinned and validated by the helper.
    let meta = unsafe { read_meta((*scan).indexRelation) };
    if meta.metric_tag != metric.storage_tag() || meta.dimensions as usize != query.dimension() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat metapage does not match the query metric or dimensions",
        );
    }
    if meta.tuples == 0 && meta.delta_count == 0 {
        state.prepared = true;
        return;
    }
    let budget = crate::settings::ivfflat_candidate_budget_from_guc();
    let mut visited_postings = 0usize;
    let mut visited_lists = 0usize;
    let mut requested_probes = 0usize;
    let mut delta_records = 0usize;
    let mut widening_rounds = state.widening_rounds;
    let mut completion_reason = "delta_only";
    let mut live = BTreeMap::<u64, f32>::new();
    if meta.tuples > 0 {
        let centroid_bytes = unsafe {
            read_chunk_range(
                (*scan).indexRelation,
                ChunkKind::Centroids,
                0,
                meta.centroid_start,
                meta.centroid_end,
                meta.centroid_bytes,
                0,
                meta.centroid_bytes,
            )
        };
        let directory_bytes = unsafe {
            read_chunk_range(
                (*scan).indexRelation,
                ChunkKind::Directory,
                0,
                meta.directory_start,
                meta.directory_end,
                meta.directory_bytes,
                0,
                meta.directory_bytes,
            )
        };
        let list_count = usize::try_from(meta.lists).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat list count exceeds platform range",
            )
        });
        let centroids = decode_vectors(&centroid_bytes, list_count, query.dimension());
        let directory = decode_directory(&directory_bytes, list_count, meta.posting_bytes);
        let quantized_scorer = if meta.codec_bytes == 0 {
            None
        } else {
            let codec_bytes = unsafe {
                read_chunk_range(
                    (*scan).indexRelation,
                    ChunkKind::Codec,
                    0,
                    meta.codec_start,
                    meta.codec_end,
                    meta.codec_bytes,
                    0,
                    meta.codec_bytes,
                )
            };
            let codec = CodecArtifactView::attach(&codec_bytes).unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("invalid IVFFlat codec artifact: {error}"),
                )
            });
            validate_codec_binding(meta, &codec);
            Some(
                codec
                    .codebook()
                    .prepare_query(&query, metric.navigation_metric())
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            format!("failed to prepare IVFFlat codec query: {error}"),
                        )
                    }),
            )
        };
        let initial_probes = crate::settings::ivfflat_probes_from_guc();
        let max_probes = crate::settings::ivfflat_max_probes_from_guc();
        if initial_probes > max_probes {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "pgcontext.ivfflat_probes ({initial_probes}) exceeds pgcontext.ivfflat_max_probes ({max_probes})"
                ),
            );
        }
        let iterative = crate::settings::ivfflat_iterative_scan_from_guc();
        let effective_initial = initial_probes.min(list_count).max(1);
        let effective_max = max_probes.min(list_count).max(effective_initial);
        if state.current_probes == 0 {
            state.initial_probes = effective_initial;
            state.current_probes =
                if iterative == crate::settings::IvfflatIterativeScan::StrictOrder {
                    // Without persisted metric-specific list lower bounds, strict
                    // global ordering requires materializing the complete bounded
                    // max-probe frontier before the first tuple is returned.
                    effective_max
                } else {
                    effective_initial
                };
            state.max_probes = effective_max;
            state.iterative = iterative;
        }
        let probes = state.current_probes;
        let probe_start = state.probed_lists;
        completion_reason = if probes == list_count {
            "all_lists"
        } else if iterative == crate::settings::IvfflatIterativeScan::Off {
            "probe_limit"
        } else if probes == effective_max {
            "max_probes"
        } else {
            "candidate_batch"
        };
        requested_probes = probes;
        let payload_width = if meta.codec_code_width == 0 {
            query.dimension().checked_mul(4).unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat posting stride overflow",
                )
            })
        } else {
            meta.codec_code_width as usize
        };
        let posting_stride = 8usize.checked_add(payload_width).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat posting stride overflow",
            )
        });
        let policy = if iterative == crate::settings::IvfflatIterativeScan::RelaxedOrder {
            IvfIterativePolicy::RelaxedOrder
        } else {
            IvfIterativePolicy::StrictOrder
        };
        let config = IvfConfig::new(
            list_count,
            effective_initial,
            effective_max,
            list_count,
            32,
            0x5047_4354,
            policy,
        )
        .unwrap_or_else(|error| raise_ivf_error(error));
        let mut adapter = PageIvfAdapter {
            metric: metric.navigation_metric(),
            dimensions: query.dimension(),
            centroids,
            lists: std::iter::repeat_with(Vec::new).take(list_count).collect(),
        };
        let probe_order = ivf_probe_order(&adapter, query.as_slice())
            .unwrap_or_else(|error| raise_ivf_error(error));
        for list_id in probe_order
            .into_iter()
            .skip(probe_start)
            .take(probes.saturating_sub(probe_start))
        {
            pg_sys::check_for_interrupts!();
            let list = list_id.get();
            let extent = directory[list];
            let list_bytes = unsafe {
                read_chunk_range(
                    (*scan).indexRelation,
                    ChunkKind::Postings,
                    0,
                    meta.posting_start,
                    meta.posting_end,
                    meta.posting_bytes,
                    extent.start,
                    extent.end,
                )
            };
            if !list_bytes.len().is_multiple_of(posting_stride) {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "IVFFlat posting list has a partial record",
                );
            }
            for record in list_bytes.chunks_exact(posting_stride) {
                let heap_tid = read_u64(record, 0);
                let point_id = IvfPointId::new(heap_tid).unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                        "IVFFlat posting contains a zero heap TID",
                    )
                });
                let payload = if quantized_scorer.is_some() {
                    PagePostingPayload::Encoded(record[8..].to_vec())
                } else {
                    PagePostingPayload::Dense(decode_vector(&record[8..], query.dimension()))
                };
                adapter.lists[list].push(PagePosting { point_id, payload });
            }
        }
        let scorer = PagePreparedScorer {
            metric: metric.navigation_metric(),
            query: query.as_slice(),
            quantized: quantized_scorer.as_ref(),
        };
        let probe_window = IvfProbeWindow::new(probe_start, probes).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat scan attempted to revisit an exhausted probe window",
            )
        });
        let previously_visited = state
            .total_visited_postings
            .saturating_add(state.total_delta_records);
        let outcome = search_ivf_probe_window_with_scorer(
            &adapter,
            query.as_slice(),
            &config,
            probe_window,
            IvfCandidateBudget::new(budget).unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "IVFFlat candidate budget is zero",
                )
            }),
            previously_visited,
            budget,
            &|_| true,
            &PgInterruptCancellation,
            &scorer,
        )
        .unwrap_or_else(|error| raise_ivf_error(error));
        visited_postings = outcome.visited_postings();
        visited_lists = outcome.visited_lists();
        state.probed_lists = probes;
        widening_rounds = state.widening_rounds;
        for hit in outcome.hits() {
            live.insert(hit.point_id().get(), hit.score());
        }
    }
    // Every delta is exact-scanned because it has not yet been folded into a
    // trained centroid generation. Chronological overwrite/tombstone folding
    // also prevents duplicate TID returns after updates and reuse.
    if !state.delta_loaded {
        for record in unsafe { read_delta_records((*scan).indexRelation, meta) } {
            delta_records = delta_records.saturating_add(1);
            let global_visited = state
                .total_visited_postings
                .saturating_add(state.total_delta_records)
                .saturating_add(visited_postings)
                .saturating_add(delta_records);
            enforce_candidate_budget(global_visited, budget);
            match record.kind {
                DeltaRecordKind::Live => {
                    let score = metric
                        .navigation_metric()
                        .distance_slices(query.as_slice(), &record.vector)
                        .unwrap_or_else(|error| crate::error::raise_core_error(error));
                    state.delta_overlay.insert(record.heap_tid, Some(score));
                }
                DeltaRecordKind::Tombstone => {
                    state.delta_overlay.insert(record.heap_tid, None);
                }
            }
        }
        state.delta_loaded = true;
    }
    for (heap_tid, score) in &state.delta_overlay {
        if let Some(score) = score {
            live.insert(*heap_tid, *score);
        } else {
            live.remove(heap_tid);
        }
    }
    for (heap_tid, score) in live {
        if !state.returned_tids.contains(&heap_tid) {
            state.candidates.push(Candidate { heap_tid, score });
        }
    }
    state.candidates.sort_unstable_by(|left, right| {
        metric
            .navigation_metric()
            .score_order()
            .compare(f64::from(left.score), f64::from(right.score))
            .then(left.heap_tid.cmp(&right.heap_tid))
    });
    // SAFETY: this contract is certified from the same live index relation.
    state.contract = Some(unsafe { crate::hnsw_am::hnsw_orderby_contract((*scan).indexRelation) });
    state.total_visited_lists = state.total_visited_lists.saturating_add(visited_lists);
    state.total_visited_postings = state
        .total_visited_postings
        .saturating_add(visited_postings);
    state.total_delta_records = state.total_delta_records.saturating_add(delta_records);
    IVFFLAT_LAST_SCAN_WORK.with(|last| {
        *last.borrow_mut() = IvfflatScanWork {
            requested_probes,
            visited_lists: state.total_visited_lists,
            visited_postings: state.total_visited_postings,
            delta_records: state.total_delta_records,
            candidates: state.candidates.len(),
            exact_rerank_candidates: state.returned_tids.len(),
            widening_rounds,
            codec_mode: meta.codec_mode,
            completion_reason,
            generation: meta.generation,
        };
    });
    state.prepared = true;
}

fn enforce_candidate_budget(visited: usize, budget: usize) {
    if visited > budget {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("IVFFlat candidate budget {budget} exhausted"),
        );
    }
}

fn raise_ivf_error(error: IvfError) -> ! {
    let sqlstate = match error {
        IvfError::CandidateBudgetExhausted { .. } | IvfError::Cancelled => {
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED
        }
        IvfError::InvalidConfig { .. } | IvfError::DimensionMismatch { .. } => {
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE
        }
        IvfError::Metric(_) => PgSqlErrorCode::ERRCODE_DATA_EXCEPTION,
        IvfError::ListCountMismatch { .. }
        | IvfError::DuplicatePointId { .. }
        | IvfError::CorruptGeneration { .. } => PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
    };
    raise_sql_error(sqlstate, error.to_string())
}

fn validate_codec_binding(meta: IvfflatMeta, codec: &CodecArtifactView<'_>) {
    let mode = match codec.codebook() {
        context_codec::QuantizedCodebook::Binary { .. } => 1,
        context_codec::QuantizedCodebook::Scalar { .. } => 2,
        context_codec::QuantizedCodebook::Product { .. } => 3,
    };
    if codec.dimensions() != meta.dimensions as usize
        || codec.revision().get() != meta.codec_revision
        || codec.codes().code_width() != meta.codec_code_width as usize
        || mode != meta.codec_mode
        || codec.reconstruction_policy() != ReconstructionPolicy::ExactSourceRerank
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat codec artifact does not match the published generation",
        );
    }
}

unsafe fn scan_state(scan: pg_sys::IndexScanDesc) -> &'static mut ScanState {
    // SAFETY: caller owns the descriptor and begin-scan installed this slot.
    let slot = unsafe {
        (*scan)
            .opaque
            .cast::<PgMemoryContextDropSlot<ScanState>>()
            .as_mut()
    }
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "IVFFlat scan state is not initialized",
        )
    });
    slot.value_mut().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "IVFFlat scan state was already released",
        )
    })
}

unsafe fn initialize_metapage(index_relation: pg_sys::Relation) {
    let blocks = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    if blocks != 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "IVFFlat build relation is not empty",
        );
    }
    // SAFETY: append sentinel plus zero-and-lock returns one exclusive buffer.
    let buffer = unsafe {
        pg_sys::ReadBufferExtended(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
            pg_sys::InvalidBlockNumber,
            pg_sys::ReadBufferMode::RBM_ZERO_AND_LOCK,
            ptr::null_mut(),
        )
    };
    // SAFETY: buffer is pinned and exclusively locked.
    unsafe {
        let page = pg_sys::BufferGetPage(buffer);
        pg_sys::PageInit(page, pg_sys::BLCKSZ as pg_sys::Size, 0);
        let bytes = IvfflatMeta::empty().encode();
        let offset = pg_sys::PageAddItemExtended(
            page,
            bytes.as_ptr().cast_mut().cast(),
            bytes.len() as pg_sys::Size,
            0,
            0,
        );
        if offset != FIRST_ITEM {
            pg_sys::UnlockReleaseBuffer(buffer);
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "failed to initialize IVFFlat metapage",
            );
        }
        pg_sys::MarkBufferDirty(buffer);
        pg_sys::UnlockReleaseBuffer(buffer);
    }
}

unsafe fn append_delta_record(
    index_relation: pg_sys::Relation,
    metric: HnswScoreMetric,
    payload: &[u8],
    dimensions: usize,
) {
    // Lock block zero before the append page so all writers share one order.
    let meta_buffer = unsafe {
        pg_sys::ReadBufferExtended(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
            0,
            pg_sys::ReadBufferMode::RBM_NORMAL,
            ptr::null_mut(),
        )
    };
    unsafe {
        pg_sys::LockBuffer(meta_buffer, pg_sys::BUFFER_LOCK_EXCLUSIVE.cast_signed());
    }
    let meta_bytes = unsafe {
        crate::hnsw_am::copy_hnsw_page_item(pg_sys::BufferGetPage(meta_buffer), FIRST_ITEM)
    }
    .unwrap_or_else(|error| {
        unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
    });
    let mut meta = IvfflatMeta::decode(&meta_bytes).unwrap_or_else(|error| {
        unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
    });
    let dimensions = u32::try_from(dimensions).unwrap_or_else(|_| {
        unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "IVFFlat delta dimensions exceed metapage range",
        )
    });
    if meta.dimensions != 0 && meta.dimensions != dimensions {
        unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_EXCEPTION,
            format!(
                "IVFFlat dimension mismatch: expected {}, got {dimensions}",
                meta.dimensions
            ),
        );
    }
    if meta.metric_tag != 0 && meta.metric_tag != metric.storage_tag() {
        unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_OBJECT_DEFINITION,
            "IVFFlat insert metric does not match the published generation",
        );
    }
    let relation_blocks = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    if let Err(error) = meta.validate_page_extents(CHUNK_DATA_BYTES, relation_blocks) {
        unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string());
    }
    let chunk_count = payload.len().div_ceil(CHUNK_DATA_BYTES);
    let stream_id = meta.delta_count;
    for (chunk_index, chunk) in payload.chunks(CHUNK_DATA_BYTES).enumerate() {
        let page_payload = encode_page_chunk(
            ChunkKind::Delta,
            stream_id,
            chunk_index,
            chunk_count,
            payload.len(),
            chunk,
        );
        let target_block = meta
            .delta_end
            .checked_add(u32::try_from(chunk_index).unwrap_or_else(|_| {
                unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat delta chunk index exceeds block range",
                )
            }))
            .unwrap_or_else(|| {
                unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat delta block range overflow",
                )
            });
        let requested_block = if target_block < relation_blocks {
            target_block
        } else {
            pg_sys::InvalidBlockNumber
        };
        let data_buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                requested_block,
                pg_sys::ReadBufferMode::RBM_ZERO_AND_LOCK,
                ptr::null_mut(),
            )
        };
        let actual_block = unsafe { pg_sys::BufferGetBlockNumber(data_buffer) };
        if actual_block != target_block {
            unsafe {
                pg_sys::UnlockReleaseBuffer(data_buffer);
                pg_sys::UnlockReleaseBuffer(meta_buffer);
            }
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat delta page is not at the publication cursor",
            );
        }
        // Each data page is WAL-logged before metapage publication. A crash
        // before publication leaves only overwriteable pages at delta_end.
        let wal = unsafe { pg_sys::GenericXLogStart(index_relation) };
        let data_page = unsafe {
            pg_sys::GenericXLogRegisterBuffer(
                wal,
                data_buffer,
                pg_sys::GENERIC_XLOG_FULL_IMAGE.cast_signed(),
            )
        };
        unsafe { pg_sys::PageInit(data_page, pg_sys::BLCKSZ as pg_sys::Size, 0) };
        let offset = unsafe {
            pg_sys::PageAddItemExtended(
                data_page,
                page_payload.as_ptr().cast_mut().cast(),
                page_payload.len() as pg_sys::Size,
                0,
                0,
            )
        };
        if offset != FIRST_ITEM {
            unsafe {
                pg_sys::GenericXLogAbort(wal);
                pg_sys::UnlockReleaseBuffer(data_buffer);
                pg_sys::UnlockReleaseBuffer(meta_buffer);
            }
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "failed to append IVFFlat WAL delta chunk",
            );
        }
        unsafe {
            pg_sys::GenericXLogFinish(wal);
            pg_sys::UnlockReleaseBuffer(data_buffer);
        }
    }
    meta.metric_tag = metric.storage_tag();
    meta.dimensions = dimensions;
    meta.delta_end = meta
        .delta_end
        .checked_add(u32::try_from(chunk_count).unwrap_or_else(|_| {
            unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat delta page count exceeds block range",
            )
        }))
        .unwrap_or_else(|| {
            unsafe { pg_sys::UnlockReleaseBuffer(meta_buffer) };
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat delta extent overflow",
            )
        });
    meta.delta_count = meta.delta_count.saturating_add(1);
    meta.generation = meta.generation.saturating_add(1);
    let encoded_meta = meta.encode();

    // SAFETY: the metapage is pinned and exclusively locked. Data chunks were
    // durably WAL-logged before this single publication record.
    let wal = unsafe { pg_sys::GenericXLogStart(index_relation) };
    let meta_page = unsafe {
        pg_sys::GenericXLogRegisterBuffer(
            wal,
            meta_buffer,
            pg_sys::GENERIC_XLOG_FULL_IMAGE.cast_signed(),
        )
    };
    let (meta_target, meta_len) =
        unsafe { crate::hnsw_am::checked_hnsw_page_item_span(meta_page, FIRST_ITEM) }
            .unwrap_or_else(|error| {
                unsafe {
                    pg_sys::GenericXLogAbort(wal);
                    pg_sys::UnlockReleaseBuffer(meta_buffer);
                }
                raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
            });
    if meta_len != META_BYTES {
        unsafe {
            pg_sys::GenericXLogAbort(wal);
            pg_sys::UnlockReleaseBuffer(meta_buffer);
        }
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat metapage length changed during delta append",
        );
    }
    // SAFETY: the registered metapage item has exactly the encoded size.
    unsafe {
        ptr::copy_nonoverlapping(encoded_meta.as_ptr(), meta_target, encoded_meta.len());
        pg_sys::GenericXLogFinish(wal);
        pg_sys::UnlockReleaseBuffer(meta_buffer);
    }
    if meta.delta_count >= IVFFLAT_DELTA_COMPACTION_THRESHOLD {
        let generation = i64::try_from(meta.generation).unwrap_or(i64::MAX);
        let index_oid = unsafe { (*index_relation).rd_id };
        let queued = Spi::get_one_with_args::<bool>(
            "SELECT pgcontext._enqueue_ivfflat_compaction_debt($1, $2)",
            &[index_oid.into(), generation.into()],
        )
        .ok()
        .flatten()
        .unwrap_or(false);
        if queued {
            let _worker_started = crate::build_worker::launch_for_current_database();
        }
    }
}

unsafe fn publish_meta(index_relation: pg_sys::Relation, meta: IvfflatMeta) {
    // SAFETY: block zero is the initialized metapage.
    let buffer = unsafe {
        pg_sys::ReadBufferExtended(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
            0,
            pg_sys::ReadBufferMode::RBM_NORMAL,
            ptr::null_mut(),
        )
    };
    unsafe { pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_EXCLUSIVE.cast_signed()) };
    // Publish through one full-image Generic WAL record so this helper is safe
    // during both CREATE INDEX and later immutable-generation cutover.
    let wal = unsafe { pg_sys::GenericXLogStart(index_relation) };
    let page = unsafe {
        pg_sys::GenericXLogRegisterBuffer(
            wal,
            buffer,
            pg_sys::GENERIC_XLOG_FULL_IMAGE.cast_signed(),
        )
    };
    // SAFETY: page is the registered shadow of the pinned exclusive buffer.
    let span = unsafe { crate::hnsw_am::checked_hnsw_page_item_span(page, FIRST_ITEM) };
    let (target, len) = span.unwrap_or_else(|error| {
        unsafe {
            pg_sys::GenericXLogAbort(wal);
            pg_sys::UnlockReleaseBuffer(buffer);
        }
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error)
    });
    if len != META_BYTES {
        unsafe {
            pg_sys::GenericXLogAbort(wal);
            pg_sys::UnlockReleaseBuffer(buffer);
        }
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat metapage item length changed",
        );
    }
    let bytes = meta.encode();
    // SAFETY: validated mutable span is exactly META_BYTES and exclusively held.
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        pg_sys::GenericXLogFinish(wal);
        pg_sys::UnlockReleaseBuffer(buffer);
    }
}

unsafe fn write_chunk_pages(
    index_relation: pg_sys::Relation,
    kind: ChunkKind,
    stream_id: u64,
    bytes: &[u8],
    start_block: u32,
) -> u32 {
    if bytes.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "cannot write an empty IVFFlat page stream",
        );
    }
    let chunk_count = bytes.len().div_ceil(CHUNK_DATA_BYTES);
    for (chunk_index, chunk) in bytes.chunks(CHUNK_DATA_BYTES).enumerate() {
        let payload = encode_page_chunk(
            kind,
            stream_id,
            chunk_index,
            chunk_count,
            bytes.len(),
            chunk,
        );
        let target_block = start_block
            .checked_add(u32::try_from(chunk_index).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat section page index exceeds block range",
                )
            }))
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat section block range overflow",
                )
            });
        let relation_blocks = unsafe {
            pg_sys::RelationGetNumberOfBlocksInFork(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        };
        let requested_block = if target_block < relation_blocks {
            target_block
        } else {
            pg_sys::InvalidBlockNumber
        };
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                requested_block,
                pg_sys::ReadBufferMode::RBM_ZERO_AND_LOCK,
                ptr::null_mut(),
            )
        };
        if unsafe { pg_sys::BufferGetBlockNumber(buffer) } != target_block {
            unsafe { pg_sys::UnlockReleaseBuffer(buffer) };
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat section writer lost its publication cursor",
            );
        }
        let wal = unsafe { pg_sys::GenericXLogStart(index_relation) };
        let page = unsafe {
            pg_sys::GenericXLogRegisterBuffer(
                wal,
                buffer,
                pg_sys::GENERIC_XLOG_FULL_IMAGE.cast_signed(),
            )
        };
        // SAFETY: buffer is pinned and its registered shadow is exclusive.
        unsafe {
            pg_sys::PageInit(page, pg_sys::BLCKSZ as pg_sys::Size, 0);
            let offset = pg_sys::PageAddItemExtended(
                page,
                payload.as_ptr().cast_mut().cast(),
                payload.len() as pg_sys::Size,
                0,
                0,
            );
            if offset != FIRST_ITEM {
                pg_sys::GenericXLogAbort(wal);
                pg_sys::UnlockReleaseBuffer(buffer);
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "failed to append IVFFlat section page",
                );
            }
            pg_sys::GenericXLogFinish(wal);
            pg_sys::UnlockReleaseBuffer(buffer);
        }
    }
    start_block
        .checked_add(u32::try_from(chunk_count).unwrap_or(u32::MAX))
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat section end exceeds block range",
            )
        })
}

unsafe fn write_chunk_pages_from_temp(
    index_relation: pg_sys::Relation,
    kind: ChunkKind,
    stream_id: u64,
    file: &mut external_build::PgTempFile,
    total_bytes: usize,
    start_block: u32,
) -> u32 {
    if total_bytes == 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "cannot write an empty IVFFlat temporary page stream",
        );
    }
    file.rewind();
    let chunk_count = total_bytes.div_ceil(CHUNK_DATA_BYTES);
    for chunk_index in 0..chunk_count {
        let consumed = chunk_index.saturating_mul(CHUNK_DATA_BYTES);
        let length = (total_bytes - consumed).min(CHUNK_DATA_BYTES);
        let chunk = file.read_exact_bytes(length);
        let payload = encode_page_chunk(
            kind,
            stream_id,
            chunk_index,
            chunk_count,
            total_bytes,
            &chunk,
        );
        let target_block = start_block
            .checked_add(u32::try_from(chunk_index).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat posting page index exceeds block range",
                )
            }))
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat posting block range overflow",
                )
            });
        let relation_blocks = unsafe {
            pg_sys::RelationGetNumberOfBlocksInFork(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
            )
        };
        let requested_block = if target_block < relation_blocks {
            target_block
        } else {
            pg_sys::InvalidBlockNumber
        };
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                requested_block,
                pg_sys::ReadBufferMode::RBM_ZERO_AND_LOCK,
                ptr::null_mut(),
            )
        };
        if unsafe { pg_sys::BufferGetBlockNumber(buffer) } != target_block {
            unsafe { pg_sys::UnlockReleaseBuffer(buffer) };
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat posting writer lost its publication cursor",
            );
        }
        let wal = unsafe { pg_sys::GenericXLogStart(index_relation) };
        let page = unsafe {
            pg_sys::GenericXLogRegisterBuffer(
                wal,
                buffer,
                pg_sys::GENERIC_XLOG_FULL_IMAGE.cast_signed(),
            )
        };
        unsafe {
            pg_sys::PageInit(page, pg_sys::BLCKSZ as pg_sys::Size, 0);
            let offset = pg_sys::PageAddItemExtended(
                page,
                payload.as_ptr().cast_mut().cast(),
                payload.len() as pg_sys::Size,
                0,
                0,
            );
            if offset != FIRST_ITEM {
                pg_sys::GenericXLogAbort(wal);
                pg_sys::UnlockReleaseBuffer(buffer);
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "failed to append IVFFlat external posting page",
                );
            }
            pg_sys::GenericXLogFinish(wal);
            pg_sys::UnlockReleaseBuffer(buffer);
        }
    }
    start_block
        .checked_add(u32::try_from(chunk_count).unwrap_or(u32::MAX))
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat posting end exceeds block range",
            )
        })
}

fn maintenance_work_mem_budget_bytes() -> usize {
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

unsafe fn wal_log_build_pages(index_relation: pg_sys::Relation) {
    if index_relation.is_null() {
        return;
    }
    // SAFETY: a live build relation has a live pg_class row. Temporary and
    // unlogged indexes intentionally do not enter WAL.
    let permanent = unsafe {
        !(*index_relation).rd_rel.is_null()
            && u8::try_from((*(*index_relation).rd_rel).relpersistence).ok()
                == Some(pg_sys::RELPERSISTENCE_PERMANENT)
    };
    if !permanent {
        return;
    }
    let blocks = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    if blocks > 0 {
        unsafe {
            pg_sys::log_newpage_range(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                0,
                blocks,
                true,
            )
        };
    }
}

unsafe fn read_meta(index_relation: pg_sys::Relation) -> IvfflatMeta {
    let blocks = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    if blocks == 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat metapage is missing",
        );
    }
    let buffer = unsafe {
        pg_sys::ReadBufferExtended(
            index_relation,
            pg_sys::ForkNumber::MAIN_FORKNUM,
            0,
            pg_sys::ReadBufferMode::RBM_NORMAL,
            ptr::null_mut(),
        )
    };
    unsafe { pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed()) };
    let bytes =
        unsafe { crate::hnsw_am::copy_hnsw_page_item(pg_sys::BufferGetPage(buffer), FIRST_ITEM) };
    unsafe { pg_sys::UnlockReleaseBuffer(buffer) };
    let bytes = bytes
        .unwrap_or_else(|error| raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error));
    let meta = IvfflatMeta::decode(&bytes).unwrap_or_else(|error| {
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
    });
    meta.validate_page_extents(CHUNK_DATA_BYTES, blocks)
        .unwrap_or_else(|error| {
            raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
        });
    meta
}

unsafe fn read_delta_records(
    index_relation: pg_sys::Relation,
    meta: IvfflatMeta,
) -> Vec<DeltaRecord> {
    let capacity = usize::try_from(meta.delta_count).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "IVFFlat delta count exceeds platform range",
        )
    });
    let mut records = Vec::with_capacity(capacity);
    let mut block = meta.delta_start;
    for stream_id in 0..meta.delta_count {
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                block,
                pg_sys::ReadBufferMode::RBM_NORMAL,
                ptr::null_mut(),
            )
        };
        unsafe { pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed()) };
        let payload = unsafe {
            crate::hnsw_am::copy_hnsw_page_item(pg_sys::BufferGetPage(buffer), FIRST_ITEM)
        };
        unsafe { pg_sys::UnlockReleaseBuffer(buffer) };
        let payload = payload
            .unwrap_or_else(|error| raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error));
        let (stored_stream, chunk_index, chunk_count, total_bytes) = chunk_identity(&payload);
        if stored_stream != stream_id || chunk_index != 0 || chunk_count == 0 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat delta stream sequence is invalid",
            );
        }
        let end_block = block
            .checked_add(u32::try_from(chunk_count).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat delta page count exceeds block range",
                )
            }))
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat delta extent overflow",
                )
            });
        if end_block > meta.delta_end {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat delta stream exceeds its published extent",
            );
        }
        let record_bytes = unsafe {
            read_chunk_range(
                index_relation,
                ChunkKind::Delta,
                stream_id,
                block,
                end_block,
                u64::try_from(total_bytes).unwrap_or(u64::MAX),
                0,
                u64::try_from(total_bytes).unwrap_or(u64::MAX),
            )
        };
        let record = decode_delta_record(&record_bytes).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("failed to decode IVFFlat delta record: {error}"),
            )
        });
        records.push(record);
        block = end_block;
    }
    if records.len() != capacity || block != meta.delta_end {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat delta extent is truncated",
        );
    }
    records
}

unsafe fn read_all_live_tids(index_relation: pg_sys::Relation, meta: IvfflatMeta) -> BTreeSet<u64> {
    let mut live = BTreeSet::new();
    if meta.tuples > 0 {
        let posting_bytes = unsafe {
            read_chunk_range(
                index_relation,
                ChunkKind::Postings,
                0,
                meta.posting_start,
                meta.posting_end,
                meta.posting_bytes,
                0,
                meta.posting_bytes,
            )
        };
        let codec_bytes = if meta.codec_bytes == 0 {
            None
        } else {
            Some(unsafe {
                read_chunk_range(
                    index_relation,
                    ChunkKind::Codec,
                    0,
                    meta.codec_start,
                    meta.codec_end,
                    meta.codec_bytes,
                    0,
                    meta.codec_bytes,
                )
            })
        };
        let codec = codec_bytes.as_deref().map(|bytes| {
            let codec = CodecArtifactView::attach(bytes).unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("invalid IVFFlat codec artifact: {error}"),
                )
            });
            validate_codec_binding(meta, &codec);
            codec
        });
        let payload_width = if meta.codec_code_width == 0 {
            (meta.dimensions as usize)
                .checked_mul(4)
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                        "IVFFlat vacuum posting stride overflow",
                    )
                })
        } else {
            meta.codec_code_width as usize
        };
        let stride = 8usize.checked_add(payload_width).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat vacuum posting stride overflow",
            )
        });
        if !posting_bytes.len().is_multiple_of(stride) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat vacuum encountered a partial posting record",
            );
        }
        for posting in posting_bytes.chunks_exact(stride) {
            let heap_tid = read_u64(posting, 0);
            if heap_tid == 0 || !live.insert(heap_tid) {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "IVFFlat vacuum encountered an invalid or duplicate base TID",
                );
            }
            if let Some(codec) = &codec {
                context_codec::validate_quantized_code(codec.codebook(), 0, &posting[8..])
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                            format!("IVFFlat vacuum found an invalid posting code: {error}"),
                        )
                    });
            } else {
                let _ = decode_vector(&posting[8..], meta.dimensions as usize);
            }
        }
    }
    for record in unsafe { read_delta_records(index_relation, meta) } {
        match record.kind {
            DeltaRecordKind::Live => {
                live.insert(record.heap_tid);
            }
            DeltaRecordKind::Tombstone => {
                live.remove(&record.heap_tid);
            }
        }
    }
    live
}

fn chunk_identity(payload: &[u8]) -> (u64, usize, usize, usize) {
    decode_ivf_delta_chunk_identity(payload).unwrap_or_else(|error| {
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
    })
}

unsafe fn read_chunk_range(
    index_relation: pg_sys::Relation,
    kind: ChunkKind,
    stream_id: u64,
    block_start: u32,
    block_end: u32,
    total_bytes: u64,
    byte_start: u64,
    byte_end: u64,
) -> Vec<u8> {
    if byte_end < byte_start || byte_end > total_bytes {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat requested byte range is outside its section",
        );
    }
    if byte_start == byte_end {
        return Vec::new();
    }
    let expected = usize::try_from(total_bytes).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "IVFFlat section length exceeds platform range",
        )
    });
    let expected_chunks = expected.div_ceil(CHUNK_DATA_BYTES);
    if usize::try_from(block_end.saturating_sub(block_start)).ok() != Some(expected_chunks) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat section extent disagrees with its byte length",
        );
    }
    let start = usize::try_from(byte_start).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "IVFFlat range start exceeds platform range",
        )
    });
    let end = usize::try_from(byte_end).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "IVFFlat range end exceeds platform range",
        )
    });
    let first_chunk = start / CHUNK_DATA_BYTES;
    let last_chunk = (end - 1) / CHUNK_DATA_BYTES;
    let mut bytes = Vec::with_capacity((last_chunk - first_chunk + 1) * CHUNK_DATA_BYTES);
    for chunk_index in first_chunk..=last_chunk {
        let block = block_start.saturating_add(u32::try_from(chunk_index).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat chunk index exceeds block range",
            )
        }));
        let buffer = unsafe {
            pg_sys::ReadBufferExtended(
                index_relation,
                pg_sys::ForkNumber::MAIN_FORKNUM,
                block,
                pg_sys::ReadBufferMode::RBM_NORMAL,
                ptr::null_mut(),
            )
        };
        unsafe { pg_sys::LockBuffer(buffer, pg_sys::BUFFER_LOCK_SHARE.cast_signed()) };
        let payload = unsafe {
            crate::hnsw_am::copy_hnsw_page_item(pg_sys::BufferGetPage(buffer), FIRST_ITEM)
        };
        unsafe { pg_sys::UnlockReleaseBuffer(buffer) };
        let payload = payload
            .unwrap_or_else(|error| raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error));
        let chunk = decode_page_chunk(
            &payload,
            kind,
            stream_id,
            chunk_index,
            expected_chunks,
            expected,
        );
        bytes.extend_from_slice(chunk);
    }
    let loaded_start = first_chunk * CHUNK_DATA_BYTES;
    bytes[start - loaded_start..end - loaded_start].to_vec()
}

fn encode_vectors(vectors: &[Vec<f32>]) -> Vec<u8> {
    let values = vectors.iter().map(Vec::len).sum::<usize>();
    let mut bytes = Vec::with_capacity(values.saturating_mul(4));
    for value in vectors.iter().flatten() {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_vectors(bytes: &[u8], count: usize, dimensions: usize) -> Vec<Vec<f32>> {
    let expected = count
        .checked_mul(dimensions)
        .and_then(|value| value.checked_mul(4))
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat centroid size overflow",
            )
        });
    if bytes.len() != expected {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat centroid stream length is invalid",
        )
    }
    bytes
        .chunks_exact(dimensions * 4)
        .map(|chunk| decode_vector(chunk, dimensions))
        .collect()
}

fn decode_vector(bytes: &[u8], dimensions: usize) -> Vec<f32> {
    if bytes.len() != dimensions.saturating_mul(4) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat vector record length is invalid",
        )
    }
    let vector = bytes
        .chunks_exact(4)
        .map(|value| f32::from_le_bytes(value.try_into().unwrap_or([0; 4])))
        .collect::<Vec<_>>();
    if vector.iter().any(|value| !value.is_finite()) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat vector contains a non-finite value",
        )
    }
    vector
}

fn decode_directory(bytes: &[u8], list_count: usize, posting_bytes: u64) -> Vec<ListExtent> {
    if bytes.len() != list_count.saturating_mul(DIRECTORY_ENTRY_BYTES) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat directory length is invalid",
        )
    }
    let mut prior = 0_u64;
    let mut result = Vec::with_capacity(list_count);
    for entry in bytes.chunks_exact(DIRECTORY_ENTRY_BYTES) {
        let extent = ListExtent {
            start: read_u64(entry, 0),
            end: read_u64(entry, 8),
        };
        if extent.start != prior || extent.end < extent.start || extent.end > posting_bytes {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat directory is not contiguous and monotonic",
            )
        }
        prior = extent.end;
        result.push(extent);
    }
    if prior != posting_bytes {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat directory does not cover the posting stream",
        )
    }
    result
}

fn encode_page_chunk(
    kind: ChunkKind,
    stream_id: u64,
    chunk_index: usize,
    chunk_count: usize,
    total_bytes: usize,
    chunk: &[u8],
) -> Vec<u8> {
    encode_ivf_page_chunk(
        kind,
        stream_id,
        chunk_index,
        chunk_count,
        total_bytes,
        chunk,
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            error.to_string(),
        )
    })
}

fn decode_page_chunk(
    payload: &[u8],
    kind: ChunkKind,
    stream_id: u64,
    chunk_index: usize,
    chunk_count: usize,
    total_bytes: usize,
) -> &[u8] {
    decode_ivf_page_chunk(
        payload,
        kind,
        stream_id,
        chunk_index,
        chunk_count,
        total_bytes,
        CHUNK_DATA_BYTES,
    )
    .unwrap_or_else(|error| {
        raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
    })
}

#[pg_extern(name = "ivfflat_last_scan_work", parallel_safe)]
#[allow(
    clippy::type_complexity,
    reason = "the SQL diagnostics row intentionally exposes named scalar columns"
)]
fn ivfflat_last_scan_work() -> TableIterator<
    'static,
    (
        name!(requested_probes, i64),
        name!(visited_lists, i64),
        name!(visited_postings, i64),
        name!(delta_records, i64),
        name!(candidates, i64),
        name!(exact_rerank_candidates, i64),
        name!(widening_rounds, i64),
        name!(codec, &'static str),
        name!(completion_reason, &'static str),
        name!(generation, i64),
    ),
> {
    let work = IVFFLAT_LAST_SCAN_WORK.with(|last| *last.borrow());
    TableIterator::once((
        diagnostic_i64(work.requested_probes, "requested_probes"),
        diagnostic_i64(work.visited_lists, "visited_lists"),
        diagnostic_i64(work.visited_postings, "visited_postings"),
        diagnostic_i64(work.delta_records, "delta_records"),
        diagnostic_i64(work.candidates, "candidates"),
        diagnostic_i64(work.exact_rerank_candidates, "exact_rerank_candidates"),
        diagnostic_i64(work.widening_rounds, "widening_rounds"),
        codec_name(work.codec_mode),
        work.completion_reason,
        i64::try_from(work.generation).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                "IVFFlat generation exceeds PostgreSQL bigint range",
            )
        }),
    ))
}

fn diagnostic_i64(value: usize, field: &'static str) -> i64 {
    i64::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            format!("IVFFlat diagnostic {field} exceeds PostgreSQL bigint range"),
        )
    })
}

/// Verifies one native IVFFlat index and reports its published generation.
#[pg_extern(name = "ivfflat_index_info", parallel_safe)]
fn ivfflat_index_info(index: PgRelation) -> JsonB {
    let relation = index.as_ptr();
    // SAFETY: PgRelation holds AccessShareLock for this complete verifier.
    let meta = unsafe { read_meta(relation) };
    // The live opclass must agree with the format's metric binding.
    let metric = unsafe { crate::hnsw_am::hnsw_score_metric(relation) };
    if meta.metric_tag != 0 && meta.metric_tag != metric.storage_tag() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat verifier found a metapage/opclass metric mismatch",
        );
    }
    let mut occupancies = Vec::new();
    if meta.tuples > 0 {
        let centroids = unsafe {
            read_chunk_range(
                relation,
                ChunkKind::Centroids,
                0,
                meta.centroid_start,
                meta.centroid_end,
                meta.centroid_bytes,
                0,
                meta.centroid_bytes,
            )
        };
        let directory = unsafe {
            read_chunk_range(
                relation,
                ChunkKind::Directory,
                0,
                meta.directory_start,
                meta.directory_end,
                meta.directory_bytes,
                0,
                meta.directory_bytes,
            )
        };
        let dimensions = meta.dimensions as usize;
        let lists = meta.lists as usize;
        let _ = decode_vectors(&centroids, lists, dimensions);
        let directory = decode_directory(&directory, lists, meta.posting_bytes);
        let payload_width = if meta.codec_code_width == 0 {
            dimensions.saturating_mul(4)
        } else {
            meta.codec_code_width as usize
        };
        let stride = 8usize.checked_add(payload_width).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat verifier posting stride overflow",
            )
        });
        for extent in directory {
            let bytes = usize::try_from(extent.end - extent.start).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "IVFFlat list length exceeds platform range",
                )
            });
            if !bytes.is_multiple_of(stride) {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "IVFFlat verifier found a partial posting",
                );
            }
            occupancies.push(bytes / stride);
        }
        let verified_tuples = occupancies.iter().try_fold(0_u64, |total, occupancy| {
            u64::try_from(*occupancy)
                .ok()
                .and_then(|value| total.checked_add(value))
        });
        if verified_tuples != Some(meta.tuples) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat directory tuple count disagrees with the metapage",
            );
        }
        // A full range read verifies every posting page checksum. Record-level
        // finite/TID validation is shared with the vacuum reader.
        let _ = unsafe { read_all_live_tids(relation, meta) };
    } else {
        let _ = unsafe { read_delta_records(relation, meta) };
    }
    let minimum = occupancies.iter().copied().min().unwrap_or(0);
    let maximum = occupancies.iter().copied().max().unwrap_or(0);
    let empty_lists = occupancies.iter().filter(|value| **value == 0).count();
    #[allow(
        clippy::cast_precision_loss,
        reason = "diagnostic skew is an approximate human-facing ratio"
    )]
    let skew_ratio = if occupancies.is_empty() || meta.tuples == 0 {
        0.0
    } else {
        maximum as f64 / (meta.tuples as f64 / occupancies.len() as f64)
    };
    JsonB(serde_json::json!({
        "verified": true,
        "format_version": META_VERSION,
        "generation": meta.generation,
        "metric": format!("{:?}", metric.navigation_metric()),
        "dimensions": meta.dimensions,
        "lists": meta.lists,
        "base_tuples": meta.tuples,
        "delta_records": meta.delta_count,
        "centroid_pages": meta.centroid_end - meta.centroid_start,
        "directory_pages": meta.directory_end - meta.directory_start,
        "codec": codec_name(meta.codec_mode),
        "build_workers": meta.build_workers,
        "codec_revision": meta.codec_revision,
        "codec_code_width": meta.codec_code_width,
        "codec_pages": meta.codec_end - meta.codec_start,
        "posting_pages": meta.posting_end - meta.posting_start,
        "delta_pages": meta.delta_end - meta.delta_start,
        "minimum_list_occupancy": minimum,
        "maximum_list_occupancy": maximum,
        "empty_lists": empty_lists,
        "skew_ratio": skew_ratio,
        "recommendation": if skew_ratio > 4.0 { "REINDEX with more representative training data or a different lists value" } else { "none" },
    }))
}

/// Rebuilds and atomically publishes one IVFFlat generation from live source rows.
///
/// Foreground deltas remain readable until the new checksummed generation is
/// fully WAL-logged. Superseded pages are immutable and remain safe for scans
/// that pinned the prior metapage generation.
#[pg_extern(name = "compact_ivfflat")]
fn compact_ivfflat(index: UnlockedRegclass) -> JsonB {
    // SAFETY: this boundary resolves the heap, establishes PostgreSQL's
    // heap-before-index lock order, and opens the relation under those locks.
    unsafe { compact_ivfflat_oid(index.0) }
}

pub(crate) fn ivfflat_generation(index_relation: pg_sys::Relation) -> i64 {
    // SAFETY: callers hold a PgRelation or callback relation lock.
    let generation = unsafe { read_meta(index_relation) }.generation;
    i64::try_from(generation).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            "IVFFlat generation exceeds bigint",
        )
    })
}

unsafe fn compact_ivfflat_oid(index_oid: pg_sys::Oid) -> JsonB {
    // IndexGetRelation consults catalog state without opening the index and
    // therefore cannot introduce the AccessShare-to-AccessExclusive upgrade
    // cycle this boundary exists to prevent.
    let heap_oid = unsafe { pg_sys::IndexGetRelation(index_oid, true) };
    if heap_oid == pg_sys::InvalidOid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_WRONG_OBJECT_TYPE,
            "compact_ivfflat requires an index relation",
        );
    }
    ensure_ivfflat_maintenance_privilege(index_oid, heap_oid);
    // PostgreSQL DML and DDL acquire the heap before its indexes. Matching that
    // order also lets concurrent compactors serialize directly on the index's
    // AccessExclusive lock without either holding a weaker index lock.
    unsafe {
        pg_sys::LockRelationOid(heap_oid, pg_sys::ShareLock.cast_signed());
        pg_sys::LockRelationOid(index_oid, pg_sys::AccessExclusiveLock.cast_signed());
    }
    // SAFETY: the transaction-scoped AccessExclusive lock pins the relation
    // identity and storage until transaction end; NoLock avoids re-locking.
    let index_relation = unsafe { pg_sys::index_open(index_oid, pg_sys::NoLock.cast_signed()) };
    let result = unsafe { compact_ivfflat_relation(index_relation, heap_oid) };
    unsafe { pg_sys::index_close(index_relation, pg_sys::NoLock.cast_signed()) };
    result
}

unsafe fn compact_ivfflat_relation(
    index_relation: pg_sys::Relation,
    expected_heap_oid: pg_sys::Oid,
) -> JsonB {
    if index_relation.is_null() || unsafe { (*index_relation).rd_index }.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_WRONG_OBJECT_TYPE,
            "compact_ivfflat requires an index relation",
        );
    }
    let heap_oid = unsafe { (*(*index_relation).rd_index).indrelid };
    let access_method = unsafe { pg_sys::get_index_am_oid(c"pgcontext_ivfflat".as_ptr(), false) };
    if heap_oid != expected_heap_oid
        || unsafe { (*(*index_relation).rd_rel).relam } != access_method
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_WRONG_OBJECT_TYPE,
            "compact_ivfflat requires a pgcontext_ivfflat index",
        );
    }
    // Recheck under the locked relation identity so an ownership or ACL change
    // racing the pre-lock check cannot authorize the generation rewrite.
    ensure_ivfflat_maintenance_privilege(unsafe { (*index_relation).rd_id }, heap_oid);
    // AccessExclusive is the generation pin: existing scans finish before an
    // inactive page slot can be reused or a superseded tail can be truncated.
    // ShareLock on the source table gives the build one stable DML-free source
    // snapshot. The OID boundary acquired both before opening this relation.
    unsafe { crate::hnsw_am::serialize_hnsw_insert(index_relation) };
    let metric = unsafe { crate::hnsw_am::hnsw_score_metric(index_relation) };
    let prior = unsafe { read_meta(index_relation) };
    let generation = prior.generation.checked_add(1).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "IVFFlat generation identity overflow",
        )
    });
    let mut state = BuildState {
        metric,
        collector: external_build::ExternalBuildCollector::new(
            metric.navigation_metric(),
            unsafe { options::list_count(index_relation) },
            crate::settings::ivfflat_build_parallel_workers_from_guc(),
            maintenance_work_mem_budget_bytes(),
            unsafe { options::codec_spec(index_relation) },
        ),
    };
    // SAFETY: the relation locks above retain both relations and the generated
    // IndexInfo for this synchronous source scan.
    let heap_relation = unsafe { pg_sys::table_open(heap_oid, pg_sys::NoLock.cast_signed()) };
    let index_info = unsafe { pg_sys::BuildIndexInfo(index_relation) };
    let heap_tuples = unsafe {
        pg_sys::table_index_build_scan(
            heap_relation,
            index_relation,
            index_info,
            true,
            true,
            Some(ivfflat_build_callback),
            ptr::addr_of_mut!(state).cast::<c_void>(),
            ptr::null_mut(),
        )
    };
    let source_tuples = state.collector.tuple_count();
    let blocks_before = unsafe {
        pg_sys::RelationGetNumberOfBlocksInFork(index_relation, pg_sys::ForkNumber::MAIN_FORKNUM)
    };
    let (published, reclaimed_pages) = if source_tuples == 0 {
        let meta = IvfflatMeta::empty_at(1, generation);
        unsafe { publish_meta(index_relation, meta) };
        unsafe { pg_sys::RelationTruncate(index_relation, 1) };
        (meta, blocks_before.saturating_sub(1))
    } else {
        // SAFETY: compaction holds both relations under transaction-scoped
        // locks for the complete PostgreSQL parallel-worker lifecycle.
        let output = unsafe {
            state
                .collector
                .finish_parallel(heap_relation, index_relation, metric, false)
        };
        let required_pages = ivfflat_generation_page_count(&output);
        let inactive_prefix_pages = prior.centroid_start.saturating_sub(1);
        let reuse_inactive_prefix = required_pages <= inactive_prefix_pages;
        let generation_start = if reuse_inactive_prefix {
            1
        } else {
            blocks_before
        };
        let meta = unsafe {
            write_ivfflat_generation(index_relation, metric, output, generation, generation_start)
        };
        let reclaimed = if reuse_inactive_prefix {
            let pages = blocks_before.saturating_sub(meta.posting_end);
            unsafe { pg_sys::RelationTruncate(index_relation, meta.posting_end) };
            pages
        } else {
            0
        };
        (meta, reclaimed)
    };
    unsafe { pg_sys::table_close(heap_relation, pg_sys::NoLock.cast_signed()) };
    let verified = unsafe { read_meta(index_relation) };
    if verified != published {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat generation publication failed post-write validation",
        );
    }
    JsonB(serde_json::json!({
        "generation": published.generation,
        "source_tuples": source_tuples,
        "heap_tuples_scanned": heap_tuples,
        "folded_delta_records": prior.delta_count,
        "base_tuples": published.tuples,
        "codec": codec_name(published.codec_mode),
        "reclaimed_pages": reclaimed_pages,
    }))
}

fn ensure_ivfflat_maintenance_privilege(index_oid: pg_sys::Oid, heap_oid: pg_sys::Oid) {
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_class AS index_class
              WHERE index_class.oid = $1::oid
                AND (
                    pg_catalog.pg_has_role(
                        SESSION_USER,
                        index_class.relowner,
                        'MEMBER'
                    )
                    OR pg_catalog.has_table_privilege(
                        SESSION_USER,
                        $2::oid,
                        'MAINTAIN'
                    )
                )
         )",
        &[index_oid.into(), heap_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("IVFFlat maintenance privilege check failed: {error}"),
        )
    })
    .unwrap_or(false);
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "permission denied for IVFFlat index maintenance",
        );
    }
}

fn ivfflat_generation_page_count(output: &external_build::ExternalBuildOutput) -> u32 {
    let centroid_bytes = output
        .lists
        .checked_mul(output.dimensions)
        .and_then(|values| values.checked_mul(size_of::<f32>()))
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat centroid page projection overflow",
            )
        });
    let bytes = [
        centroid_bytes,
        output.directory.len(),
        output.codec_artifact.as_ref().map_or(0, Vec::len),
        output.posting_bytes,
    ];
    bytes
        .into_iter()
        .filter(|bytes| *bytes > 0)
        .map(|bytes| bytes.div_ceil(CHUNK_DATA_BYTES))
        .try_fold(0_u32, |total, pages| {
            total.checked_add(u32::try_from(pages).ok()?)
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "IVFFlat generation page projection exceeds block range",
            )
        })
}

const fn codec_name(mode: u16) -> &'static str {
    match mode {
        0 => "none",
        1 => "binary",
        2 => "sq8",
        3 => "pq",
        _ => "invalid",
    }
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap_or([0; 8]))
}

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext.ivfflat_handler(internal)
RETURNS index_am_handler
AS 'MODULE_PATHNAME', 'pgcontext_ivfflat_handler'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE ACCESS METHOD pgcontext_ivfflat
    TYPE INDEX
    HANDLER pgcontext.ivfflat_handler;

CREATE OPERATOR CLASS pgcontext.vector_ivfflat_ops
    DEFAULT FOR TYPE pgcontext.vector USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<-> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.hnsw_l2_distance(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR CLASS pgcontext.vector_ivfflat_ip_ops
    FOR TYPE pgcontext.vector USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<#> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.negative_inner_product(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR CLASS pgcontext.vector_ivfflat_cosine_ops
    FOR TYPE pgcontext.vector USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<=> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.cosine_distance(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR CLASS pgcontext.vector_ivfflat_l1_ops
    FOR TYPE pgcontext.vector USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<+> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.l1_distance(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR CLASS pgcontext.halfvec_ivfflat_ops
    DEFAULT FOR TYPE pgcontext.halfvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<-> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.halfvec_ivfflat_ip_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<#> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_negative_inner_product(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.halfvec_ivfflat_cosine_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<=> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_cosine_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.halfvec_ivfflat_l1_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<+> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l1_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.int8vec_ivfflat_ops
    DEFAULT FOR TYPE pgcontext.int8vec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<-> (pgcontext.int8vec, pgcontext.int8vec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.int8vec_l2_distance(pgcontext.int8vec, pgcontext.int8vec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.int8vec_ivfflat_ip_ops
    FOR TYPE pgcontext.int8vec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<#> (pgcontext.int8vec, pgcontext.int8vec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.int8vec_negative_inner_product(pgcontext.int8vec, pgcontext.int8vec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.int8vec_ivfflat_cosine_ops
    FOR TYPE pgcontext.int8vec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<=> (pgcontext.int8vec, pgcontext.int8vec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.int8vec_cosine_distance(pgcontext.int8vec, pgcontext.int8vec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.int8vec_ivfflat_l1_ops
    FOR TYPE pgcontext.int8vec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<+> (pgcontext.int8vec, pgcontext.int8vec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.int8vec_l1_distance(pgcontext.int8vec, pgcontext.int8vec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.uint8vec_ivfflat_ops
    DEFAULT FOR TYPE pgcontext.uint8vec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<-> (pgcontext.uint8vec, pgcontext.uint8vec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.uint8vec_l2_distance(pgcontext.uint8vec, pgcontext.uint8vec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.uint8vec_ivfflat_ip_ops
    FOR TYPE pgcontext.uint8vec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<#> (pgcontext.uint8vec, pgcontext.uint8vec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.uint8vec_negative_inner_product(pgcontext.uint8vec, pgcontext.uint8vec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.uint8vec_ivfflat_cosine_ops
    FOR TYPE pgcontext.uint8vec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<=> (pgcontext.uint8vec, pgcontext.uint8vec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.uint8vec_cosine_distance(pgcontext.uint8vec, pgcontext.uint8vec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.uint8vec_ivfflat_l1_ops
    FOR TYPE pgcontext.uint8vec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<+> (pgcontext.uint8vec, pgcontext.uint8vec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.uint8vec_l1_distance(pgcontext.uint8vec, pgcontext.uint8vec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.bitvec_ivfflat_hamming_ops
    FOR TYPE pgcontext.bitvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<~> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(pgcontext.bitvec, pgcontext.bitvec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.bitvec_ivfflat_jaccard_ops
    FOR TYPE pgcontext.bitvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<%> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.bitvec_jaccard_distance(pgcontext.bitvec, pgcontext.bitvec),
    STORAGE pgcontext.vector;

-- Pgvector-spelled aliases are scoped by access method and schema, so the
-- same names coexist with the HNSW aliases without changing native ownership.
CREATE OPERATOR CLASS pgcontext.vector_l2_ops
    FOR TYPE pgcontext.vector USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<-> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.hnsw_l2_distance(pgcontext.vector, pgcontext.vector);
CREATE OPERATOR CLASS pgcontext.vector_ip_ops
    FOR TYPE pgcontext.vector USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<#> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.negative_inner_product(pgcontext.vector, pgcontext.vector);
CREATE OPERATOR CLASS pgcontext.vector_cosine_ops
    FOR TYPE pgcontext.vector USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<=> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.cosine_distance(pgcontext.vector, pgcontext.vector);
CREATE OPERATOR CLASS pgcontext.vector_l1_ops
    FOR TYPE pgcontext.vector USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<+> (pgcontext.vector, pgcontext.vector) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.l1_distance(pgcontext.vector, pgcontext.vector);

CREATE OPERATOR CLASS pgcontext.halfvec_l2_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<-> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.halfvec_ip_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<#> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_negative_inner_product(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.halfvec_cosine_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<=> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_cosine_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.halfvec_l1_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<+> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l1_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.bit_hamming_ops
    FOR TYPE pgcontext.bitvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<~> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(pgcontext.bitvec, pgcontext.bitvec),
    STORAGE pgcontext.vector;
CREATE OPERATOR CLASS pgcontext.bit_jaccard_ops
    FOR TYPE pgcontext.bitvec USING pgcontext_ivfflat AS
    OPERATOR 1 pgcontext.<%> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.bitvec_jaccard_distance(pgcontext.bitvec, pgcontext.bitvec),
    STORAGE pgcontext.vector;
"#,
    name = "create_ivfflat_access_method",
    requires = ["pgcontext_bootstrap", "create_hnsw_access_method"]
);
