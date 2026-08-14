//! Memory-bounded IVFFlat collection, training, assignment, and merge.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::ffi::{CString, c_void};
use std::mem::size_of;
use std::ptr;
use std::slice;

use context_build::deterministic_metric_clusters_with_workers;
use context_codec::{CodecKind, CodecSpec, TrainedCodecArtifact};
use context_core::{DenseVector, DistanceMetric};
use context_storage::{CodecArtifact, encode_codec_artifact};
use pgrx::prelude::*;

use crate::error::raise_sql_error;

const TRAINING_SEED: u64 = 0x5047_4354;
const MAX_TRAINING_SAMPLE: usize = 100_000;

pub(super) struct ExternalBuildCollector {
    metric: DistanceMetric,
    requested_lists: usize,
    workers: usize,
    budget: usize,
    codec_spec: CodecSpec,
    dimensions: Option<usize>,
    tuples: usize,
    sample_limit: Option<usize>,
    sample: BinaryHeap<SampleRow>,
    source: PgTempFile,
}

impl ExternalBuildCollector {
    pub(super) fn new(
        metric: DistanceMetric,
        requested_lists: usize,
        workers: usize,
        budget: usize,
        codec_spec: CodecSpec,
    ) -> Self {
        if requested_lists == 0 || workers == 0 || budget == 0 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "IVFFlat external build requires positive lists, workers, and memory budget",
            );
        }
        Self {
            metric,
            requested_lists,
            workers,
            budget,
            codec_spec,
            dimensions: None,
            tuples: 0,
            sample_limit: None,
            sample: BinaryHeap::new(),
            source: PgTempFile::new(),
        }
    }

    pub(super) fn push(&mut self, heap_tid: u64, vector: Vec<f32>) {
        let dimensions = *self.dimensions.get_or_insert(vector.len());
        if heap_tid == 0
            || vector.len() != dimensions
            || vector.iter().any(|value| !value.is_finite())
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat external build received an invalid source row",
            );
        }
        let sample_limit = *self.sample_limit.get_or_insert_with(|| {
            let row_bytes = dimensions
                .checked_mul(4)
                .and_then(|bytes| bytes.checked_add(48))
                .unwrap_or_else(|| build_limit("IVFFlat training row size overflow"));
            self.budget
                .saturating_div(8)
                .saturating_div(row_bytes)
                .clamp(1, MAX_TRAINING_SAMPLE)
        });
        self.source.write_u64(heap_tid);
        self.source.write_vector(&vector);
        let sample = SampleRow {
            rank: splitmix64(TRAINING_SEED ^ heap_tid),
            heap_tid,
            vector,
        };
        if self.sample.len() < sample_limit {
            self.sample.push(sample);
        } else if self.sample.peek().is_some_and(|largest| sample < *largest)
            && let Some(mut largest) = self.sample.peek_mut()
        {
            *largest = sample;
        }
        self.tuples = self.tuples.saturating_add(1);
    }

    pub(super) const fn tuple_count(&self) -> usize {
        self.tuples
    }

    /// Finishes assignment through PostgreSQL parallel workers when the
    /// planner and configured worker cap admit at least one worker.
    ///
    /// # Safety
    ///
    /// Both relations must remain live and locked for the complete build.
    pub(super) unsafe fn finish_parallel(
        self,
        heap_relation: pg_sys::Relation,
        index_relation: pg_sys::Relation,
        metric: crate::hnsw_am::HnswScoreMetric,
        concurrent: bool,
    ) -> ExternalBuildOutput {
        self.finish_inner(Some(ParallelAssignment {
            heap_relation,
            index_relation,
            metric,
            concurrent,
        }))
    }

    fn finish_inner(mut self, parallel: Option<ParallelAssignment>) -> ExternalBuildOutput {
        let dimensions = self.dimensions.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "cannot finish an empty IVFFlat external build",
            )
        });
        let lists = self.requested_lists.min(self.tuples);
        if self.sample.len() < lists {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "maintenance_work_mem admits {} IVFFlat training rows but {lists} lists require at least {lists}",
                    self.sample.len()
                ),
            );
        }
        let mut sample = std::mem::take(&mut self.sample).into_vec();
        sample.sort_unstable_by_key(|row| (row.rank, row.heap_tid));
        let vectors = sample.into_iter().map(|row| row.vector).collect::<Vec<_>>();
        preflight_training_memory(
            self.codec_spec,
            dimensions,
            lists,
            vectors.len(),
            self.budget,
        );
        pg_sys::check_for_interrupts!();
        let trained = deterministic_metric_clusters_with_workers(
            &vectors,
            lists,
            32,
            TRAINING_SEED,
            self.metric,
            self.workers,
        )
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("failed to train external IVFFlat centroids: {error}"),
            )
        });
        let centroids = trained.centroids().to_vec();
        let dense_sample = vectors
            .into_iter()
            .map(DenseVector::new)
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("invalid IVFFlat codec training sample: {error}"),
                )
            });
        let codec =
            TrainedCodecArtifact::train(self.codec_spec, &dense_sample).unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("failed to train IVFFlat codec: {error}"),
                )
            });
        pg_sys::check_for_interrupts!();
        let code_width = codec.codebook().map_or(0, |codebook| codebook.code_len());
        let codec_artifact = encode_codec_state(&codec, &dense_sample[0]);
        let payload_width = if code_width == 0 {
            dimensions.saturating_mul(4)
        } else {
            code_width
        };
        let budget = self.budget;
        let (runs, parallel_workers) = parallel
            .filter(|_| self.workers > 1)
            .and_then(|assignment| {
                // SAFETY: finish_parallel's relation-lifetime contract covers
                // the synchronous PostgreSQL worker lifecycle.
                unsafe {
                    postgres_parallel_assignment_runs(
                        assignment,
                        &centroids,
                        self.metric,
                        dimensions,
                        self.workers,
                        self.budget,
                    )
                }
            })
            .map_or_else(
                || {
                    (
                        self.assignment_runs(&centroids, dimensions, &codec, payload_width),
                        0,
                    )
                },
                |(dense_runs, workers)| {
                    (
                        transcode_parallel_runs(dense_runs, dimensions, &codec, budget),
                        workers,
                    )
                },
            );
        let (directory, postings, posting_bytes) =
            merge_runs(runs, lists, payload_width, self.tuples);
        ExternalBuildOutput {
            centroids,
            directory,
            postings,
            posting_bytes,
            codec_artifact,
            codec_revision: codec.revision().get(),
            codec_mode: codec_mode(codec.spec().kind()),
            code_width,
            dimensions,
            lists,
            tuples: self.tuples,
            parallel_workers,
        }
    }

    fn assignment_runs(
        &mut self,
        centroids: &[Vec<f32>],
        dimensions: usize,
        codec: &TrainedCodecArtifact,
        payload_width: usize,
    ) -> Vec<PgTempFile> {
        self.source.rewind();
        let row_memory = dimensions
            .checked_mul(4)
            .and_then(|bytes| bytes.checked_add(48))
            .unwrap_or_else(|| build_limit("IVFFlat assignment row size overflow"));
        let rows_per_run = self
            .budget
            .saturating_div(4)
            .saturating_div(row_memory)
            .max(1);
        let mut runs = RunAccumulator::new(payload_width);
        let mut rows = Vec::with_capacity(rows_per_run);
        while let Some((heap_tid, vector)) = self.source.read_source_row(dimensions) {
            pg_sys::check_for_interrupts!();
            rows.push((heap_tid, vector));
            if rows.len() == rows_per_run {
                runs.push(write_run(
                    assign_rows(
                        std::mem::take(&mut rows),
                        centroids,
                        self.metric,
                        self.workers,
                    ),
                    codec,
                ));
                rows = Vec::with_capacity(rows_per_run);
            }
        }
        if !rows.is_empty() {
            runs.push(write_run(
                assign_rows(rows, centroids, self.metric, self.workers),
                codec,
            ));
        }
        runs.finish()
    }
}

pub(super) struct ExternalBuildOutput {
    pub(super) centroids: Vec<Vec<f32>>,
    pub(super) directory: Vec<u8>,
    pub(super) postings: PgTempFile,
    pub(super) posting_bytes: usize,
    pub(super) codec_artifact: Option<Vec<u8>>,
    pub(super) codec_revision: u64,
    pub(super) codec_mode: u16,
    pub(super) code_width: usize,
    pub(super) dimensions: usize,
    pub(super) lists: usize,
    pub(super) tuples: usize,
    pub(super) parallel_workers: usize,
}

#[derive(Clone, Copy)]
struct ParallelAssignment {
    heap_relation: pg_sys::Relation,
    index_relation: pg_sys::Relation,
    metric: crate::hnsw_am::HnswScoreMetric,
    concurrent: bool,
}

#[derive(Debug)]
struct SampleRow {
    rank: u64,
    heap_tid: u64,
    vector: Vec<f32>,
}

impl PartialEq for SampleRow {
    fn eq(&self, other: &Self) -> bool {
        (self.rank, self.heap_tid) == (other.rank, other.heap_tid)
    }
}

impl Eq for SampleRow {}

impl PartialOrd for SampleRow {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SampleRow {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.rank, self.heap_tid).cmp(&(other.rank, other.heap_tid))
    }
}

#[derive(Debug)]
struct AssignedRow {
    list: usize,
    heap_tid: u64,
    vector: Vec<f32>,
}

struct AssignedPayload {
    list: usize,
    heap_tid: u64,
    payload: Vec<u8>,
}

const PARALLEL_KEY_SHARED: u64 = 0xA100_0000_0000_0001;
const PARALLEL_KEY_SCAN: u64 = 0xA100_0000_0000_0002;
const PARALLEL_KEY_CENTROIDS: u64 = 0xA100_0000_0000_0003;
const PARALLEL_KEY_FILE_SET: u64 = 0xA100_0000_0000_0004;

#[repr(C)]
#[derive(Clone, Copy)]
struct ParallelBuildShared {
    heap_oid: pg_sys::Oid,
    index_oid: pg_sys::Oid,
    dimensions: u32,
    lists: u32,
    metric_tag: u16,
    concurrent: u8,
    _reserved: u8,
    budget: u64,
    workers: u32,
}

struct ParallelWorkerCollector<'a> {
    metric: crate::hnsw_am::HnswScoreMetric,
    centroids: &'a [Vec<f32>],
    dimensions: usize,
    rows_per_run: usize,
    rows: Vec<AssignedRow>,
    runs: RunAccumulator,
}

impl<'a> ParallelWorkerCollector<'a> {
    fn new(
        metric: crate::hnsw_am::HnswScoreMetric,
        centroids: &'a [Vec<f32>],
        dimensions: usize,
        budget: usize,
    ) -> Self {
        let row_bytes = dimensions.saturating_mul(4).saturating_add(48);
        let rows_per_run = budget.saturating_div(row_bytes).max(1);
        Self {
            metric,
            centroids,
            dimensions,
            rows_per_run,
            rows: Vec::with_capacity(rows_per_run),
            runs: RunAccumulator::new(dimensions.saturating_mul(4)),
        }
    }

    fn push(&mut self, heap_tid: u64, vector: Vec<f32>) {
        let list = nearest_centroid(&vector, self.centroids, self.metric.navigation_metric())
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("failed to assign IVFFlat parallel row: {error}"),
                )
            });
        self.rows.push(AssignedRow {
            list,
            heap_tid,
            vector,
        });
        if self.rows.len() == self.rows_per_run {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        self.runs
            .push(write_dense_run(std::mem::take(&mut self.rows)));
        self.rows = Vec::with_capacity(self.rows_per_run);
    }

    fn finish(mut self) -> PgTempFile {
        self.flush();
        let runs = self.runs.finish();
        match runs.len() {
            0 => PgTempFile::new(),
            1 => runs.into_iter().next().unwrap_or_else(PgTempFile::new),
            _ => merge_assignment_runs(runs, self.dimensions.saturating_mul(4)),
        }
    }
}

fn estimate_parallel_chunk(estimator: &mut pg_sys::shm_toc_estimator, bytes: usize) {
    // PostgreSQL's BUFFERALIGN is 32 bytes on the supported PG17/PG18 builds.
    const BUFFER_ALIGNMENT: usize = 32;
    let aligned = bytes
        .checked_add(BUFFER_ALIGNMENT - 1)
        .map(|value| value & !(BUFFER_ALIGNMENT - 1))
        .unwrap_or_else(|| build_limit("IVFFlat parallel DSM chunk alignment overflow"));
    estimator.space_for_chunks = estimator
        .space_for_chunks
        .checked_add(aligned)
        .unwrap_or_else(|| build_limit("IVFFlat parallel DSM estimate overflow"));
    estimator.number_of_keys = estimator
        .number_of_keys
        .checked_add(1)
        .unwrap_or_else(|| build_limit("IVFFlat parallel DSM key estimate overflow"));
}

unsafe fn postgres_parallel_assignment_runs(
    assignment: ParallelAssignment,
    centroids: &[Vec<f32>],
    metric: DistanceMetric,
    dimensions: usize,
    worker_cap: usize,
    budget: usize,
) -> Option<(Vec<PgTempFile>, usize)> {
    // PostgreSQL chooses a safe upper bound from table size and server GUCs;
    // the extension GUC can only reduce it.
    // SAFETY: the caller retains both live relations for the complete parallel
    // lifecycle and their rd_id fields are immutable while locked.
    let planned = unsafe {
        pg_sys::plan_create_index_workers(
            (*assignment.heap_relation).rd_id,
            (*assignment.index_relation).rd_id,
        )
    };
    let requested = usize::try_from(planned.max(0)).unwrap_or(0).min(worker_cap);
    if requested == 0 {
        return None;
    }
    let lists = centroids.len();
    let centroid_values = lists
        .checked_mul(dimensions)
        .unwrap_or_else(|| build_limit("IVFFlat centroid DSM value count overflow"));
    let centroid_bytes = centroid_values
        .checked_mul(size_of::<f32>())
        .unwrap_or_else(|| build_limit("IVFFlat centroid DSM byte count overflow"));
    let flattened = centroids
        .iter()
        .flat_map(|centroid| centroid.iter().copied())
        .collect::<Vec<_>>();
    if flattened.len() != centroid_values {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat trained centroids are ragged",
        );
    }
    if assignment.metric.navigation_metric() != metric {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat parallel metric contract drifted after training",
        );
    }

    // SAFETY: this backend is not already in parallel mode and every exit path
    // below calls ExitParallelMode exactly once after successful entry.
    unsafe { pg_sys::EnterParallelMode() };
    // SAFETY: PostgreSQL is initialized and both static C strings are
    // NUL-terminated symbols compiled into this extension.
    let pcxt = unsafe {
        pg_sys::CreateParallelContext(
            c"pgcontext".as_ptr(),
            c"pgcontext_ivfflat_parallel_build_main".as_ptr(),
            i32::try_from(requested).unwrap_or(i32::MAX),
        )
    };
    if pcxt.is_null() {
        // SAFETY: parallel mode was entered above and no context was created.
        unsafe { pg_sys::ExitParallelMode() };
        return None;
    }

    let snapshot = if assignment.concurrent {
        // SAFETY: the build runs in a transaction; the returned snapshot is
        // registered until every parallel worker finishes or setup aborts.
        unsafe { pg_sys::RegisterSnapshot(pg_sys::GetTransactionSnapshot()) }
    } else {
        ptr::addr_of_mut!(pg_sys::SnapshotAnyData)
    };
    // SAFETY: the heap relation and registered/static snapshot remain live;
    // PostgreSQL only estimates the descriptor size here.
    let scan_bytes =
        unsafe { pg_sys::table_parallelscan_estimate(assignment.heap_relation, snapshot) };
    // SAFETY: CreateParallelContext returned one exclusive leader estimator.
    let estimator = unsafe { &mut (*pcxt).estimator };
    estimate_parallel_chunk(estimator, size_of::<ParallelBuildShared>());
    estimate_parallel_chunk(estimator, scan_bytes);
    estimate_parallel_chunk(estimator, centroid_bytes);
    estimate_parallel_chunk(estimator, size_of::<pg_sys::SharedFileSet>());
    // SAFETY: the exclusive leader context estimator contains all required
    // chunk and key reservations before DSM initialization.
    unsafe { pg_sys::InitializeParallelDSM(pcxt) };
    // SAFETY: `pcxt` is the live leader-owned context returned above.
    if unsafe { (*pcxt).seg.is_null() } {
        if assignment.concurrent {
            // SAFETY: this branch owns the registered snapshot and no workers
            // were launched from the failed DSM context.
            unsafe { pg_sys::UnregisterSnapshot(snapshot) };
        }
        // SAFETY: this branch owns the context and parallel-mode entry.
        unsafe {
            pg_sys::DestroyParallelContext(pcxt);
            pg_sys::ExitParallelMode();
        }
        return None;
    }

    // SAFETY: DSM initialization succeeded and the estimator reserved exactly
    // this aligned chunk in the leader-owned TOC.
    let shared = unsafe {
        pg_sys::shm_toc_allocate((*pcxt).toc, size_of::<ParallelBuildShared>())
            .cast::<ParallelBuildShared>()
    };
    // SAFETY: DSM initialization succeeded and the estimator reserved exactly
    // `scan_bytes` for the parallel scan descriptor.
    let parallel_scan = unsafe {
        pg_sys::shm_toc_allocate((*pcxt).toc, scan_bytes)
            .cast::<pg_sys::ParallelTableScanDescData>()
    };
    // SAFETY: the estimator reserved exactly `centroid_bytes`, aligned for f32.
    let shared_centroids =
        unsafe { pg_sys::shm_toc_allocate((*pcxt).toc, centroid_bytes).cast::<f32>() };
    // SAFETY: the estimator reserved one aligned SharedFileSet chunk.
    let file_set = unsafe {
        pg_sys::shm_toc_allocate((*pcxt).toc, size_of::<pg_sys::SharedFileSet>())
            .cast::<pg_sys::SharedFileSet>()
    };
    if shared.is_null()
        || parallel_scan.is_null()
        || shared_centroids.is_null()
        || file_set.is_null()
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OUT_OF_MEMORY,
            "PostgreSQL could not allocate IVFFlat parallel build state",
        );
    }
    // SAFETY: all four allocations are non-null, properly sized and exclusively
    // leader-owned; source slices and relations remain live through launch.
    unsafe {
        ptr::write(
            shared,
            ParallelBuildShared {
                heap_oid: (*assignment.heap_relation).rd_id,
                index_oid: (*assignment.index_relation).rd_id,
                dimensions: u32::try_from(dimensions).unwrap_or(u32::MAX),
                lists: u32::try_from(lists).unwrap_or(u32::MAX),
                metric_tag: assignment.metric.storage_tag(),
                concurrent: u8::from(assignment.concurrent),
                _reserved: 0,
                budget: u64::try_from(budget).unwrap_or(u64::MAX),
                workers: u32::try_from(requested).unwrap_or(u32::MAX),
            },
        );
        ptr::copy_nonoverlapping(flattened.as_ptr(), shared_centroids, flattened.len());
        pg_sys::table_parallelscan_initialize(assignment.heap_relation, parallel_scan, snapshot);
        pg_sys::SharedFileSetInit(file_set, (*pcxt).seg);
        pg_sys::shm_toc_insert((*pcxt).toc, PARALLEL_KEY_SHARED, shared.cast());
        pg_sys::shm_toc_insert((*pcxt).toc, PARALLEL_KEY_SCAN, parallel_scan.cast());
        pg_sys::shm_toc_insert((*pcxt).toc, PARALLEL_KEY_CENTROIDS, shared_centroids.cast());
        pg_sys::shm_toc_insert((*pcxt).toc, PARALLEL_KEY_FILE_SET, file_set.cast());
        pg_sys::LaunchParallelWorkers(pcxt);
    }
    // SAFETY: `pcxt` is live and LaunchParallelWorkers initialized this field.
    let launched = usize::try_from(unsafe { (*pcxt).nworkers_launched.max(0) }).unwrap_or(0);
    if launched == 0 {
        // SAFETY: no workers launched; this branch owns the file set, context,
        // optional snapshot registration and parallel-mode entry.
        unsafe {
            pg_sys::SharedFileSetDeleteAll(file_set);
            pg_sys::DestroyParallelContext(pcxt);
            if assignment.concurrent {
                pg_sys::UnregisterSnapshot(snapshot);
            }
            pg_sys::ExitParallelMode();
        }
        return None;
    }
    // SAFETY: `pcxt` remains leader-owned and launched workers are joined before
    // any DSM-backed state is read or destroyed.
    unsafe {
        pg_sys::WaitForParallelWorkersToAttach(pcxt);
        pg_sys::WaitForParallelWorkersToFinish(pcxt);
    }
    let mut runs = Vec::with_capacity(launched);
    for worker in 0..launched {
        let name = parallel_worker_file_name(worker);
        // SAFETY: workers finished, `file_set` remains attached, and `name` is a
        // NUL-terminated unique file-set key.
        let file = unsafe {
            pg_sys::BufFileOpenFileSet(ptr::addr_of_mut!((*file_set).fs), name.as_ptr(), 0, false)
        };
        if file.is_null() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_IO_ERROR,
                format!("IVFFlat parallel worker {worker} did not publish its spill run"),
            );
        }
        runs.push(PgTempFile(file));
    }
    // SAFETY: all worker files are opened, workers are joined, and this branch
    // owns the file set, context, optional snapshot and parallel-mode entry.
    unsafe {
        pg_sys::SharedFileSetDeleteAll(file_set);
        pg_sys::DestroyParallelContext(pcxt);
        if assignment.concurrent {
            pg_sys::UnregisterSnapshot(snapshot);
        }
        pg_sys::ExitParallelMode();
    }
    Some((runs, launched))
}

fn parallel_worker_file_name(worker: usize) -> CString {
    CString::new(format!("pgcontext-ivfflat-{worker}"))
        .unwrap_or_else(|_| unreachable!("fixed worker filename has no NUL"))
}

fn transcode_parallel_runs(
    runs: Vec<PgTempFile>,
    dimensions: usize,
    codec: &TrainedCodecArtifact,
    budget: usize,
) -> Vec<PgTempFile> {
    let transcoded = transcode_parallel_runs_measured(runs, dimensions, codec, budget);
    let _peak_live_runs = transcoded.peak_live_runs;
    transcoded.runs
}

struct TranscodedRuns {
    runs: Vec<PgTempFile>,
    peak_live_runs: usize,
}

fn transcode_parallel_runs_measured(
    runs: Vec<PgTempFile>,
    dimensions: usize,
    codec: &TrainedCodecArtifact,
    budget: usize,
) -> TranscodedRuns {
    if codec.codebook().is_none() {
        let peak_live_runs = runs.len();
        return TranscodedRuns {
            runs,
            peak_live_runs,
        };
    }
    let dense_width = dimensions.saturating_mul(4);
    let row_bytes = dense_width.saturating_add(48);
    let rows_per_batch = budget.saturating_div(8).saturating_div(row_bytes).max(1);
    let payload_width = codec
        .codebook()
        .map(|codebook| codebook.code_len())
        .unwrap_or(dense_width);
    let mut encoded_runs = RunAccumulator::new(payload_width);
    for mut run in runs {
        let mut rows = Vec::with_capacity(rows_per_batch);
        while let Some(row) = run.read_assigned_row(dense_width) {
            let vector = row
                .payload
                .chunks_exact(4)
                .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap_or([0; 4])))
                .collect::<Vec<_>>();
            rows.push(AssignedRow {
                list: row.list,
                heap_tid: row.heap_tid,
                vector,
            });
            if rows.len() == rows_per_batch {
                encoded_runs.push(write_run(std::mem::take(&mut rows), codec));
                rows = Vec::with_capacity(rows_per_batch);
            }
        }
        if !rows.is_empty() {
            encoded_runs.push(write_run(rows, codec));
        }
    }
    let peak_live_runs = encoded_runs.peak_live_runs();
    TranscodedRuns {
        runs: encoded_runs.finish(),
        peak_live_runs,
    }
}

fn write_dense_run(mut rows: Vec<AssignedRow>) -> PgTempFile {
    rows.sort_unstable_by_key(|row| (row.list, row.heap_tid));
    let mut run = PgTempFile::new();
    for row in rows {
        run.write_u32(u32::try_from(row.list).unwrap_or_else(|_| {
            build_limit("IVFFlat list identifier exceeds parallel run format")
        }));
        run.write_u64(row.heap_tid);
        run.write_vector(&row.vector);
    }
    run.rewind();
    run
}

/// PostgreSQL dynamic parallel-worker entrypoint for IVFFlat assignment.
///
/// # Safety
///
/// PostgreSQL invokes this symbol with the DSM segment and TOC created by the
/// leader in `postgres_parallel_assignment_runs`.
#[pg_guard]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn pgcontext_ivfflat_parallel_build_main(
    segment: *mut pg_sys::dsm_segment,
    toc: *mut pg_sys::shm_toc,
) {
    // SAFETY: PostgreSQL entered this guarded dynamic-worker callback.
    let _scope = unsafe { crate::hnsw_am::ffi_boundary::PgCallbackScope::new() };
    // SAFETY: PostgreSQL passes the leader-created TOC and the key maps to an
    // aligned ParallelBuildShared allocation reserved during estimation.
    let shared = unsafe {
        pg_sys::shm_toc_lookup(toc, PARALLEL_KEY_SHARED, false).cast::<ParallelBuildShared>()
    };
    // SAFETY: the leader inserted this key with a fully initialized parallel
    // scan descriptor allocation.
    let parallel_scan = unsafe {
        pg_sys::shm_toc_lookup(toc, PARALLEL_KEY_SCAN, false)
            .cast::<pg_sys::ParallelTableScanDescData>()
    };
    // SAFETY: the leader inserted this key with `dimensions * lists`
    // initialized f32 values.
    let centroid_data =
        unsafe { pg_sys::shm_toc_lookup(toc, PARALLEL_KEY_CENTROIDS, false).cast::<f32>() };
    // SAFETY: the leader inserted this key with an initialized SharedFileSet.
    let file_set = unsafe {
        pg_sys::shm_toc_lookup(toc, PARALLEL_KEY_FILE_SET, false).cast::<pg_sys::SharedFileSet>()
    };
    if shared.is_null() || parallel_scan.is_null() || centroid_data.is_null() || file_set.is_null()
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat parallel worker DSM state is incomplete",
        );
    }
    // SAFETY: the null checks above establish a live aligned shared header for
    // the lifetime of this worker callback.
    let shared = unsafe { &*shared };
    let dimensions = usize::try_from(shared.dimensions).unwrap_or(0);
    let lists = usize::try_from(shared.lists).unwrap_or(0);
    let values = dimensions
        .checked_mul(lists)
        .unwrap_or_else(|| build_limit("IVFFlat parallel centroid extent overflow"));
    // SAFETY: the leader allocated and initialized exactly `values` f32 entries
    // and DSM remains attached for this callback.
    let centroid_values = unsafe { slice::from_raw_parts(centroid_data, values) };
    let centroids = centroid_values
        .chunks_exact(dimensions)
        .map(<[f32]>::to_vec)
        .collect::<Vec<_>>();
    if dimensions == 0 || centroids.len() != lists {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat parallel centroid DSM state is invalid",
        );
    }
    let metric = crate::hnsw_am::HnswScoreMetric::from_storage_tag(shared.metric_tag)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat parallel worker metric tag is invalid",
            )
        });
    let per_worker_budget = usize::try_from(shared.budget)
        .unwrap_or(usize::MAX)
        .saturating_div(usize::try_from(shared.workers).unwrap_or(1).max(1));
    let mut collector =
        ParallelWorkerCollector::new(metric, &centroids, dimensions, per_worker_budget);
    // SAFETY: both pointers were null-checked, belong to this DSM segment, and
    // remain live until the worker callback returns.
    unsafe { pg_sys::SharedFileSetAttach(file_set, segment) };
    let heap_lock = if shared.concurrent != 0 {
        pg_sys::ShareUpdateExclusiveLock
    } else {
        pg_sys::ShareLock
    };
    let index_lock = if shared.concurrent != 0 {
        pg_sys::RowExclusiveLock
    } else {
        pg_sys::AccessExclusiveLock
    };
    // SAFETY: the shared OIDs were copied from leader-locked relations and the
    // selected lock levels match PostgreSQL's parallel build protocol.
    let heap = unsafe { pg_sys::table_open(shared.heap_oid, heap_lock.cast_signed()) };
    // SAFETY: the heap was opened first and the shared index OID came from the
    // same leader-owned build assignment.
    let index = unsafe { pg_sys::index_open(shared.index_oid, index_lock.cast_signed()) };
    // SAFETY: `index` is live and locked for the complete synchronous scan.
    let index_info = unsafe { pg_sys::BuildIndexInfo(index) };
    // SAFETY: IndexInfo is worker-owned and writable until scan completion.
    unsafe { (*index_info).ii_Concurrent = shared.concurrent != 0 };
    // SAFETY: `heap` is live and the shared scan descriptor was initialized by
    // the leader for this exact relation and snapshot.
    let scan = unsafe { pg_sys::table_beginscan_parallel(heap, parallel_scan) };
    // SAFETY: both relations, IndexInfo, scan descriptor and collector state
    // remain live throughout this synchronous callback-driven scan.
    unsafe {
        pg_sys::table_index_build_scan(
            heap,
            index,
            index_info,
            true,
            false,
            Some(ivfflat_parallel_build_callback),
            ptr::addr_of_mut!(collector).cast(),
            scan,
        );
    }
    let mut run = collector.finish();
    // SAFETY: PostgreSQL initializes the nonnegative worker number before
    // invoking a dynamic worker entrypoint.
    let worker = usize::try_from(unsafe { pg_sys::ParallelWorkerNumber.max(0) }).unwrap_or(0);
    let name = parallel_worker_file_name(worker);
    // SAFETY: the attached non-null SharedFileSet remains live and `name` is a
    // NUL-terminated unique worker filename.
    let output =
        unsafe { pg_sys::BufFileCreateFileSet(ptr::addr_of_mut!((*file_set).fs), name.as_ptr()) };
    if output.is_null() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_IO_ERROR,
            "PostgreSQL could not create an IVFFlat shared worker run",
        );
    }
    run.rewind();
    let mut copy_buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = run.read_some(&mut copy_buffer);
        if read == 0 {
            break;
        }
        // SAFETY: `output` is a live BufFile and the initialized prefix of
        // `copy_buffer` remains valid for the synchronous write.
        unsafe { pg_sys::BufFileWrite(output, copy_buffer.as_ptr().cast(), read) };
        pg_sys::check_for_interrupts!();
    }
    // SAFETY: the output file and both opened relations are worker-owned and
    // closed/exported exactly once after the scan and copy complete.
    unsafe {
        pg_sys::BufFileExportFileSet(output);
        pg_sys::BufFileClose(output);
        pg_sys::index_close(index, index_lock.cast_signed());
        pg_sys::table_close(heap, heap_lock.cast_signed());
    }
}

#[pg_guard]
unsafe extern "C-unwind" fn ivfflat_parallel_build_callback(
    index_relation: pg_sys::Relation,
    tid: pg_sys::ItemPointer,
    values: *mut pg_sys::Datum,
    is_null: *mut bool,
    tuple_is_alive: bool,
    state: *mut c_void,
) {
    // SAFETY: PostgreSQL entered this guarded build-visitor callback.
    let _scope = unsafe { crate::hnsw_am::ffi_boundary::PgCallbackScope::new() };
    if !tuple_is_alive || tid.is_null() || state.is_null() {
        return;
    }
    // SAFETY: PostgreSQL supplies values/null flags matching the live index
    // tuple descriptor for this synchronous callback.
    let Some(vector) =
        (unsafe { crate::hnsw_am::hnsw_vector_from_index_values(index_relation, values, is_null) })
    else {
        return;
    };
    // SAFETY: the callback rejected null state and PostgreSQL keeps the
    // worker-owned collector alive for the synchronous scan.
    let state = unsafe { &mut *state.cast::<ParallelWorkerCollector<'_>>() };
    let Some(vector) = state
        .metric
        .prepare_vector(vector)
        .unwrap_or_else(|error| crate::error::raise_core_error(error))
    else {
        return;
    };
    // SAFETY: the callback rejected a null TID and PostgreSQL keeps it readable
    // for the duration of this invocation.
    let heap_tid = pgrx::itemptr::item_pointer_to_u64(unsafe { *tid });
    state.push(heap_tid, vector.into_values());
}

fn assign_rows(
    rows: Vec<(u64, Vec<f32>)>,
    centroids: &[Vec<f32>],
    metric: DistanceMetric,
    workers: usize,
) -> Vec<AssignedRow> {
    pg_sys::check_for_interrupts!();
    let workers = workers.min(rows.len()).max(1);
    let chunk_size = rows.len().div_ceil(workers);
    let assignments = std::thread::scope(|scope| {
        let handles = rows
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|(_, vector)| nearest_centroid(vector, centroids, metric))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();
        let mut assignments = Vec::with_capacity(rows.len());
        let mut first_error = None;
        let mut worker_panicked = false;
        for handle in handles {
            match handle.join() {
                Ok(results) => {
                    for result in results {
                        match result {
                            Ok(list) if first_error.is_none() => assignments.push(list),
                            Ok(_) => {}
                            Err(error) => {
                                first_error.get_or_insert(error);
                            }
                        }
                    }
                }
                Err(_) => worker_panicked = true,
            }
        }
        if worker_panicked {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "an IVFFlat assignment worker panicked",
            );
        }
        if let Some(error) = first_error {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("failed to assign IVFFlat external row: {error}"),
            );
        }
        assignments
    });
    pg_sys::check_for_interrupts!();
    rows.into_iter()
        .zip(assignments)
        .map(|((heap_tid, vector), list)| AssignedRow {
            list,
            heap_tid,
            vector,
        })
        .collect()
}

fn write_run(mut rows: Vec<AssignedRow>, codec: &TrainedCodecArtifact) -> PgTempFile {
    rows.sort_unstable_by_key(|row| (row.list, row.heap_tid));
    let mut run = PgTempFile::new();
    let codes = codec.codebook().map(|_| {
        let vectors = rows
            .iter_mut()
            .map(|row| DenseVector::new(std::mem::take(&mut row.vector)))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("invalid IVFFlat assignment vector: {error}"),
                )
            });
        codec
            .encode(&vectors)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("failed to encode IVFFlat assignment run: {error}"),
                )
            })
            .unwrap_or_else(|| unreachable!("quantized codec must produce assignment codes"))
    });
    for (row_index, row) in rows.drain(..).enumerate() {
        run.write_u32(u32::try_from(row.list).unwrap_or_else(|_| {
            build_limit("IVFFlat list identifier exceeds external run format")
        }));
        run.write_u64(row.heap_tid);
        if let Some(codes) = &codes {
            run.write(codes.code(row_index).unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "IVFFlat assignment code row is missing",
                )
            }));
        } else {
            run.write_vector(&row.vector);
        }
    }
    run.rewind();
    run
}

fn merge_runs(
    mut runs: Vec<PgTempFile>,
    lists: usize,
    payload_width: usize,
    expected_tuples: usize,
) -> (Vec<u8>, PgTempFile, usize) {
    let mut current = runs
        .iter_mut()
        .map(|run| run.read_assigned_row(payload_width))
        .collect::<Vec<_>>();
    let mut heap = BinaryHeap::new();
    for (run, row) in current.iter().enumerate() {
        if let Some(row) = row {
            heap.push(std::cmp::Reverse((row.list, row.heap_tid, run)));
        }
    }
    let mut output = PgTempFile::new();
    let mut counts = vec![0_usize; lists];
    let mut merged = 0_usize;
    while let Some(std::cmp::Reverse((list, heap_tid, run))) = heap.pop() {
        pg_sys::check_for_interrupts!();
        let row = current[run].take().unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat external merge lost its run head",
            )
        });
        if row.list != list || row.heap_tid != heap_tid || list >= lists {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat external merge order is corrupt",
            );
        }
        output.write_u64(row.heap_tid);
        output.write(&row.payload);
        counts[list] = counts[list].saturating_add(1);
        merged = merged.saturating_add(1);
        current[run] = runs[run].read_assigned_row(payload_width);
        if let Some(next) = &current[run] {
            heap.push(std::cmp::Reverse((next.list, next.heap_tid, run)));
        }
    }
    if merged != expected_tuples {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat external merge tuple count drifted",
        );
    }
    let posting_stride = 8usize
        .checked_add(payload_width)
        .unwrap_or_else(|| build_limit("IVFFlat posting stride overflow"));
    let posting_bytes = merged
        .checked_mul(posting_stride)
        .unwrap_or_else(|| build_limit("IVFFlat posting stream length overflow"));
    let mut directory = Vec::with_capacity(lists.saturating_mul(16));
    let mut cursor = 0_usize;
    for count in counts {
        let start = u64::try_from(cursor)
            .unwrap_or_else(|_| build_limit("IVFFlat posting directory exceeds u64"));
        cursor = cursor
            .checked_add(count.saturating_mul(posting_stride))
            .unwrap_or_else(|| build_limit("IVFFlat posting directory overflow"));
        let end = u64::try_from(cursor)
            .unwrap_or_else(|_| build_limit("IVFFlat posting directory exceeds u64"));
        directory.extend_from_slice(&start.to_le_bytes());
        directory.extend_from_slice(&end.to_le_bytes());
    }
    if cursor != posting_bytes {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "IVFFlat external directory does not cover postings",
        );
    }
    output.rewind();
    (directory, output, posting_bytes)
}

/// Binary-carry spill compaction bounds simultaneously open assignment runs
/// to the machine word width, independent of source cardinality.
struct RunAccumulator {
    payload_width: usize,
    levels: Vec<Option<PgTempFile>>,
    peak_live_runs: usize,
}

impl RunAccumulator {
    fn new(payload_width: usize) -> Self {
        Self {
            payload_width,
            levels: Vec::new(),
            peak_live_runs: 0,
        }
    }

    fn push(&mut self, mut run: PgTempFile) {
        self.peak_live_runs = self.peak_live_runs.max(self.live_runs().saturating_add(1));
        let mut level = 0usize;
        loop {
            if level == self.levels.len() {
                self.levels.push(Some(run));
                return;
            }
            if let Some(existing) = self.levels[level].take() {
                // Other retained levels plus two inputs and the merge output
                // bound the number of simultaneously live files.
                self.peak_live_runs = self.peak_live_runs.max(self.live_runs().saturating_add(3));
                run = merge_assignment_runs(vec![existing, run], self.payload_width);
                level = level.saturating_add(1);
            } else {
                self.levels[level] = Some(run);
                return;
            }
        }
    }

    fn finish(self) -> Vec<PgTempFile> {
        self.levels.into_iter().flatten().collect()
    }

    fn live_runs(&self) -> usize {
        self.levels.iter().filter(|run| run.is_some()).count()
    }

    fn peak_live_runs(&self) -> usize {
        self.peak_live_runs
    }
}

#[cfg(feature = "pg_test")]
pub(super) fn test_parallel_transcode_bound(batch_count: usize) -> (usize, usize) {
    let sample = [
        DenseVector::new(vec![0.0]).expect("zero scalar sample should be valid"),
        DenseVector::new(vec![1.0]).expect("unit scalar sample should be valid"),
    ];
    let spec = CodecSpec::scalar(256, None).expect("SQ8 test specification should be valid");
    let codec = TrainedCodecArtifact::train(spec, &sample)
        .expect("SQ8 production transcode test should train");
    let mut dense_run = PgTempFile::new();
    for row in 0..batch_count {
        dense_run.write_u32(0);
        dense_run.write_u64(u64::try_from(row).unwrap_or(u64::MAX).saturating_add(1));
        dense_run.write_vector(&[if row.is_multiple_of(2) { 0.0 } else { 1.0 }]);
    }
    dense_run.rewind();
    // A one-byte budget forces one row per batch through the same production
    // transcode loop used after PostgreSQL parallel assignment workers finish.
    let transcoded = transcode_parallel_runs_measured(vec![dense_run], 1, &codec, 1);
    (transcoded.peak_live_runs, transcoded.runs.len())
}

fn merge_assignment_runs(mut runs: Vec<PgTempFile>, payload_width: usize) -> PgTempFile {
    let mut current = runs
        .iter_mut()
        .map(|run| run.read_assigned_row(payload_width))
        .collect::<Vec<_>>();
    let mut heap = BinaryHeap::new();
    for (run, row) in current.iter().enumerate() {
        if let Some(row) = row {
            heap.push(std::cmp::Reverse((row.list, row.heap_tid, run)));
        }
    }
    let mut output = PgTempFile::new();
    while let Some(std::cmp::Reverse((list, heap_tid, run))) = heap.pop() {
        pg_sys::check_for_interrupts!();
        let row = current[run].take().unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat bounded merge lost its run head",
            )
        });
        if row.list != list || row.heap_tid != heap_tid {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "IVFFlat bounded merge order is corrupt",
            );
        }
        output.write_u32(u32::try_from(row.list).unwrap_or_else(|_| {
            build_limit("IVFFlat list identifier exceeds external run format")
        }));
        output.write_u64(row.heap_tid);
        output.write(&row.payload);
        current[run] = runs[run].read_assigned_row(payload_width);
        if let Some(next) = &current[run] {
            heap.push(std::cmp::Reverse((next.list, next.heap_tid, run)));
        }
    }
    output.rewind();
    output
}

fn encode_codec_state(codec: &TrainedCodecArtifact, sample: &DenseVector) -> Option<Vec<u8>> {
    let codebook = codec.codebook()?.clone();
    let codes = codec
        .encode(core::slice::from_ref(sample))
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("failed to encode IVFFlat codec validation row: {error}"),
            )
        })
        .unwrap_or_else(|| unreachable!("trained quantized codec must produce codes"));
    let artifact = CodecArtifact::new(
        codec.revision(),
        codec.reconstruction_policy(),
        codebook,
        codes,
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("failed to assemble IVFFlat codec artifact: {error}"),
        )
    });
    Some(encode_codec_artifact(&artifact).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("failed to persist IVFFlat codec artifact: {error}"),
        )
    }))
}

fn preflight_codec_memory(spec: CodecSpec, dimensions: usize, sample_rows: usize, budget: usize) {
    let Some((_, centroids, _)) = spec.product_parameters() else {
        return;
    };
    // Product training retains the quantizer and equivalent serving codebook,
    // then temporarily serializes a validated publication copy. Charge four
    // complete f32 codebook images before entering codec training.
    let projected = dimensions
        .checked_mul(centroids.min(sample_rows))
        .and_then(|values| values.checked_mul(size_of::<f32>()))
        .and_then(|bytes| bytes.checked_mul(4))
        .unwrap_or_else(|| build_limit("IVFFlat PQ memory projection overflow"));
    if projected > budget.saturating_div(2) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "IVFFlat PQ codec projects {projected} bytes before assignment, exceeding half of maintenance_work_mem ({budget})"
            ),
        );
    }
}

fn preflight_training_memory(
    spec: CodecSpec,
    dimensions: usize,
    lists: usize,
    sample_rows: usize,
    budget: usize,
) {
    let vector_bytes = dimensions
        .checked_mul(size_of::<f32>())
        .unwrap_or_else(|| build_limit("IVFFlat training row projection overflow"));
    let sample_bytes = sample_rows
        .checked_mul(vector_bytes.saturating_add(48))
        .unwrap_or_else(|| build_limit("IVFFlat sample projection overflow"));
    let centroid_bytes = lists
        .checked_mul(vector_bytes)
        .unwrap_or_else(|| build_limit("IVFFlat centroid projection overflow"));
    let accumulator_bytes = centroid_bytes
        .checked_mul(2)
        .unwrap_or_else(|| build_limit("IVFFlat accumulator projection overflow"));
    let projected = sample_bytes
        .checked_add(centroid_bytes)
        .and_then(|value| value.checked_add(accumulator_bytes))
        .unwrap_or_else(|| build_limit("IVFFlat training projection overflow"));
    if projected > budget {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "IVFFlat training projects {projected} bytes, exceeding maintenance_work_mem ({budget})"
            ),
        );
    }
    preflight_codec_memory(spec, dimensions, sample_rows, budget);
}

const fn codec_mode(kind: CodecKind) -> u16 {
    match kind {
        CodecKind::Plain => 0,
        CodecKind::Binary => 1,
        CodecKind::Scalar => 2,
        CodecKind::Product => 3,
    }
}

fn nearest_centroid(
    vector: &[f32],
    centroids: &[Vec<f32>],
    metric: DistanceMetric,
) -> Result<usize, context_core::Error> {
    centroids
        .iter()
        .enumerate()
        .map(|(list, centroid)| {
            metric
                .distance_slices(vector, centroid)
                .map(|score| (list, score))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min_by(|left, right| {
            metric
                .score_order()
                .compare(f64::from(left.1), f64::from(right.1))
                .then(left.0.cmp(&right.0))
        })
        .map_or(Ok(0), |(list, _)| Ok(list))
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

pub(super) struct PgTempFile(*mut pg_sys::BufFile);

impl PgTempFile {
    fn new() -> Self {
        // SAFETY: PostgreSQL registers this transaction-scoped temp file with
        // the current ResourceOwner and cleans it after errors or disconnects.
        let file = unsafe { pg_sys::BufFileCreateTemp(false) };
        if file.is_null() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_IO_ERROR,
                "PostgreSQL could not create an IVFFlat temporary build file",
            );
        }
        Self(file)
    }

    fn write_u32(&mut self, value: u32) {
        self.write(&value.to_le_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.write(&value.to_le_bytes());
    }

    fn write_vector(&mut self, vector: &[f32]) {
        for value in vector {
            self.write(&value.to_le_bytes());
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        // SAFETY: BufFile remains ResourceOwner-registered and bytes remain
        // live for this synchronous write.
        unsafe { pg_sys::BufFileWrite(self.0, bytes.as_ptr().cast::<c_void>(), bytes.len()) };
    }

    pub(super) fn rewind(&mut self) {
        // SAFETY: file zero and offset zero identify the start of this BufFile.
        let result = unsafe { pg_sys::BufFileSeek(self.0, 0, 0, 0) };
        if result != 0 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_IO_ERROR,
                "failed to rewind IVFFlat temporary build file",
            );
        }
    }

    pub(super) fn read_exact_bytes(&mut self, length: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; length];
        // SAFETY: the owned buffer has exactly length writable bytes.
        unsafe { pg_sys::BufFileReadExact(self.0, bytes.as_mut_ptr().cast::<c_void>(), length) };
        bytes
    }

    fn read_some(&mut self, buffer: &mut [u8]) -> usize {
        // SAFETY: the owned buffer exposes its complete writable extent for
        // this synchronous PostgreSQL buffered-file read.
        unsafe { pg_sys::BufFileRead(self.0, buffer.as_mut_ptr().cast::<c_void>(), buffer.len()) }
    }

    fn read_source_row(&mut self, dimensions: usize) -> Option<(u64, Vec<f32>)> {
        let heap_tid = self.read_optional_u64()?;
        Some((heap_tid, self.read_vector(dimensions)))
    }

    fn read_assigned_row(&mut self, payload_width: usize) -> Option<AssignedPayload> {
        let list = self.read_optional_u32()? as usize;
        let heap_tid = self.read_u64();
        Some(AssignedPayload {
            list,
            heap_tid,
            payload: self.read_exact_bytes(payload_width),
        })
    }

    fn read_optional_u32(&mut self) -> Option<u32> {
        let mut bytes = [0_u8; 4];
        // SAFETY: eof is accepted only before the next complete fixed record.
        let read = unsafe {
            pg_sys::BufFileReadMaybeEOF(
                self.0,
                bytes.as_mut_ptr().cast::<c_void>(),
                bytes.len(),
                true,
            )
        };
        (read != 0).then(|| u32::from_le_bytes(bytes))
    }

    fn read_optional_u64(&mut self) -> Option<u64> {
        let mut bytes = [0_u8; 8];
        // SAFETY: eof is accepted only before the next complete fixed record.
        let read = unsafe {
            pg_sys::BufFileReadMaybeEOF(
                self.0,
                bytes.as_mut_ptr().cast::<c_void>(),
                bytes.len(),
                true,
            )
        };
        (read != 0).then(|| u64::from_le_bytes(bytes))
    }

    fn read_u64(&mut self) -> u64 {
        let mut bytes = [0_u8; 8];
        // SAFETY: assigned run framing requires a complete u64 here.
        unsafe {
            pg_sys::BufFileReadExact(self.0, bytes.as_mut_ptr().cast::<c_void>(), bytes.len())
        };
        u64::from_le_bytes(bytes)
    }

    fn read_vector(&mut self, dimensions: usize) -> Vec<f32> {
        let bytes = self.read_exact_bytes(dimensions.saturating_mul(4));
        bytes
            .chunks_exact(4)
            .map(|value| f32::from_le_bytes(value.try_into().unwrap_or([0; 4])))
            .collect()
    }
}

impl Drop for PgTempFile {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this wrapper owns the ResourceOwner-registered BufFile
            // and closes it at most once.
            unsafe { pg_sys::BufFileClose(self.0) };
            self.0 = ptr::null_mut();
        }
    }
}

fn build_limit(message: &'static str) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED, message)
}
