//! SQL-visible pgContext settings.

use context_core::policy::{
    DEFAULT_HNSW_BUILD_PARALLEL_WORKERS, DEFAULT_HNSW_CANDIDATE_BUDGET,
    DEFAULT_HNSW_CANDIDATE_MASK_POINTS, DEFAULT_HNSW_EF_CONSTRUCTION, DEFAULT_HNSW_EF_SEARCH,
    DEFAULT_HNSW_ITERATIVE_EXPANSION_LIMIT, DEFAULT_HNSW_M, DEFAULT_HNSW_RECALL_THRESHOLD,
    MAX_HNSW_BUILD_PARALLEL_WORKERS, MAX_HNSW_CANDIDATE_BUDGET, MAX_HNSW_CANDIDATE_MASK_POINTS,
    MAX_HNSW_EF_CONSTRUCTION, MAX_HNSW_EF_SEARCH, MAX_HNSW_ITERATIVE_EXPANSION_LIMIT, MAX_HNSW_M,
    MIN_HNSW_M,
};
use context_index::HnswConfig;
use pgrx::guc::{GucContext, GucFlags, GucRegistry, GucSetting};
use pgrx::prelude::*;

use crate::error::raise_sql_error;

const DEFAULT_HNSW_M_I32: i32 = policy_usize_to_i32(DEFAULT_HNSW_M);
const DEFAULT_HNSW_EF_CONSTRUCTION_I32: i32 = policy_usize_to_i32(DEFAULT_HNSW_EF_CONSTRUCTION);
const DEFAULT_HNSW_EF_SEARCH_I32: i32 = policy_usize_to_i32(DEFAULT_HNSW_EF_SEARCH);
const DEFAULT_HNSW_CANDIDATE_BUDGET_I32: i32 = policy_usize_to_i32(DEFAULT_HNSW_CANDIDATE_BUDGET);
const DEFAULT_HNSW_ITERATIVE_EXPANSION_LIMIT_I32: i32 =
    policy_usize_to_i32(DEFAULT_HNSW_ITERATIVE_EXPANSION_LIMIT);
const MAX_HNSW_M_I32: i32 = policy_usize_to_i32(MAX_HNSW_M);
const MIN_HNSW_M_I32: i32 = policy_usize_to_i32(MIN_HNSW_M);
const MAX_HNSW_EF_CONSTRUCTION_I32: i32 = policy_usize_to_i32(MAX_HNSW_EF_CONSTRUCTION);
const MAX_HNSW_EF_SEARCH_I32: i32 = policy_usize_to_i32(MAX_HNSW_EF_SEARCH);
const MAX_HNSW_CANDIDATE_BUDGET_I32: i32 = policy_usize_to_i32(MAX_HNSW_CANDIDATE_BUDGET);
const MAX_HNSW_ITERATIVE_EXPANSION_LIMIT_I32: i32 =
    policy_usize_to_i32(MAX_HNSW_ITERATIVE_EXPANSION_LIMIT);
const DEFAULT_HNSW_CANDIDATE_MASK_POINTS_I32: i32 =
    policy_usize_to_i32(DEFAULT_HNSW_CANDIDATE_MASK_POINTS);
const MAX_HNSW_CANDIDATE_MASK_POINTS_I32: i32 = policy_usize_to_i32(MAX_HNSW_CANDIDATE_MASK_POINTS);
const DEFAULT_HNSW_BUILD_PARALLEL_WORKERS_I32: i32 =
    policy_usize_to_i32(DEFAULT_HNSW_BUILD_PARALLEL_WORKERS);
const MAX_HNSW_BUILD_PARALLEL_WORKERS_I32: i32 =
    policy_usize_to_i32(MAX_HNSW_BUILD_PARALLEL_WORKERS);

static HNSW_M: GucSetting<i32> = GucSetting::<i32>::new(DEFAULT_HNSW_M_I32);
static HNSW_EF_CONSTRUCTION: GucSetting<i32> =
    GucSetting::<i32>::new(DEFAULT_HNSW_EF_CONSTRUCTION_I32);
static HNSW_EF_SEARCH: GucSetting<i32> = GucSetting::<i32>::new(DEFAULT_HNSW_EF_SEARCH_I32);
static HNSW_CANDIDATE_BUDGET: GucSetting<i32> =
    GucSetting::<i32>::new(DEFAULT_HNSW_CANDIDATE_BUDGET_I32);
static HNSW_ITERATIVE_EXPANSION_LIMIT: GucSetting<i32> =
    GucSetting::<i32>::new(DEFAULT_HNSW_ITERATIVE_EXPANSION_LIMIT_I32);
static HNSW_RECALL_THRESHOLD: GucSetting<f64> =
    GucSetting::<f64>::new(DEFAULT_HNSW_RECALL_THRESHOLD);
static HNSW_SHARED_SERVING: GucSetting<bool> = GucSetting::<bool>::new(true);
const DEFAULT_HNSW_SHARED_SERVING_BUDGET_MB: i32 = 512;
static HNSW_SHARED_SERVING_BUDGET_MB: GucSetting<i32> =
    GucSetting::<i32>::new(DEFAULT_HNSW_SHARED_SERVING_BUDGET_MB);
static HNSW_PACK_ON_FIRST_USE: GucSetting<bool> = GucSetting::<bool>::new(true);
static HNSW_MASK_CANDIDATE_LIMIT: GucSetting<i32> =
    GucSetting::<i32>::new(DEFAULT_HNSW_CANDIDATE_MASK_POINTS_I32);
static HNSW_BUILD_PARALLEL_WORKERS: GucSetting<i32> =
    GucSetting::<i32>::new(DEFAULT_HNSW_BUILD_PARALLEL_WORKERS_I32);
static PGVECTOR_COMPAT_WARNINGS: GucSetting<bool> = GucSetting::<bool>::new(true);
const DEFAULT_HNSW_DELTA_SEGMENT_LIMIT: i32 = 10_000;
static HNSW_DELTA_SEGMENT_LIMIT: GucSetting<i32> =
    GucSetting::<i32>::new(DEFAULT_HNSW_DELTA_SEGMENT_LIMIT);
static HNSW_COMPACT_ON_THRESHOLD: GucSetting<bool> = GucSetting::<bool>::new(true);
/// Projected-vector-bytes ceiling for a compaction an INSERT is allowed to run
/// itself, in megabytes. 1GB admits roughly 700,000 rows at 384 dimensions,
/// deliberately high so ordinary workloads self-maintain rather than silently
/// falling back to the inline path; operators trading throughput for tail
/// latency lower it.
const DEFAULT_HNSW_COMPACT_ON_THRESHOLD_MAX_MB: i32 = 1024;
static HNSW_COMPACT_ON_THRESHOLD_MAX_MB: GucSetting<i32> =
    GucSetting::<i32>::new(DEFAULT_HNSW_COMPACT_ON_THRESHOLD_MAX_MB);

pub(crate) fn init_gucs() {
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_m",
        c"Default HNSW neighbor count.",
        c"Default maximum retained HNSW neighbors per node for pgContext HNSW indexes.",
        &HNSW_M,
        MIN_HNSW_M_I32,
        MAX_HNSW_M_I32,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_ef_construction",
        c"Default HNSW build candidate budget.",
        c"Default HNSW construction candidate budget for pgContext HNSW indexes.",
        &HNSW_EF_CONSTRUCTION,
        1,
        MAX_HNSW_EF_CONSTRUCTION_I32,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_ef_search",
        c"Default HNSW search candidate budget.",
        c"Default HNSW search candidate budget for pgContext HNSW indexes.",
        &HNSW_EF_SEARCH,
        1,
        MAX_HNSW_EF_SEARCH_I32,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_candidate_budget",
        c"Default HNSW candidate budget.",
        c"Default candidate budget for filtered or iterative pgContext HNSW search.",
        &HNSW_CANDIDATE_BUDGET,
        1,
        MAX_HNSW_CANDIDATE_BUDGET_I32,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_iterative_expansion_limit",
        c"Maximum HNSW iterative expansion budget.",
        c"Maximum candidate batch size pgContext may request during iterative HNSW recheck.",
        &HNSW_ITERATIVE_EXPANSION_LIMIT,
        1,
        MAX_HNSW_ITERATIVE_EXPANSION_LIMIT_I32,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_float_guc(
        c"pgcontext.hnsw_recall_threshold",
        c"Default HNSW recall threshold.",
        c"Default minimum recall expected before approximate HNSW serving is considered healthy.",
        &HNSW_RECALL_THRESHOLD,
        0.0,
        1.0,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"pgcontext.hnsw_shared_serving",
        c"Serve packed HNSW graph generations from shared memory.",
        c"When enabled, backends publish packed HNSW graph generations to a \
          shared registry so other backends can attach the published image \
          instead of rebuilding a private copy from PostgreSQL pages. \
          Disabling reverts to per-backend packed generations only.",
        &HNSW_SHARED_SERVING,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_shared_serving_budget_mb",
        c"Shared packed HNSW graph generation budget in megabytes.",
        c"Total bytes of published packed HNSW graph generations the shared \
          registry may hold across all indexes. A publish that would exceed \
          this budget is skipped; the publishing backend continues serving \
          from its own backend-local pack.",
        &HNSW_SHARED_SERVING_BUDGET_MB,
        0,
        i32::MAX,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"pgcontext.hnsw_pack_on_first_use",
        c"Pack an HNSW graph generation inline when none is available.",
        c"When disabled and no packed generation is available from this \
          backend's cache, a delta patch, or the shared registry, queries \
          are served from unpacked directory reads instead of paying a full \
          pack inline. Trades a large first-query cliff for a smaller, \
          sustained per-query cost until some backend packs and publishes.",
        &HNSW_PACK_ON_FIRST_USE,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_mask_candidate_limit",
        c"Maximum candidate points accepted by one masked HNSW scan.",
        c"Upper bound on distinct point IDs a filter-aware HNSW scan may \
          carry as its candidate mask, independent of the unrelated SQL \
          recall-check budget. Raise this to serve larger filtered result \
          sets through the masked scan path instead of falling back to an \
          exact scan.",
        &HNSW_MASK_CANDIDATE_LIMIT,
        0,
        MAX_HNSW_CANDIDATE_MASK_POINTS_I32,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_build_parallel_workers",
        c"HNSW bulk-build worker threads.",
        c"Number of threads used to construct the in-memory HNSW graph \
          during CREATE INDEX / REINDEX. The default of 1 builds \
          single-threaded and deterministic, matching every earlier \
          release. Raising this parallelizes graph construction across \
          threads within the building backend using per-node locking, so \
          concurrent inserts to unrelated nodes proceed without blocking \
          each other; the resulting graph is structurally valid but not \
          bit-identical to a sequential build of the same rows, since \
          concurrent insertion order is not deterministic.",
        &HNSW_BUILD_PARALLEL_WORKERS,
        1,
        MAX_HNSW_BUILD_PARALLEL_WORKERS_I32,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"pgcontext.pgvector_compat_warnings",
        c"Advise migration when serving pgvector-typed columns.",
        c"When enabled and pgContext serves an index over a column whose \
          type belongs to the pgvector extension (coexist mode), a single \
          NOTICE per backend and index recommends \
          pgcontext.migration_report() / pgcontext.adopt_pgvector(). \
          Results are always complete either way; this only controls the \
          advisory.",
        &PGVECTOR_COMPAT_WARNINGS,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_delta_segment_limit",
        c"Rows an HNSW index absorbs through the delta segment before falling back to inline insertion.",
        c"Inserts append a small fixed-format record to a bounded delta \
          segment instead of splicing the row into the HNSW graph; scans \
          merge an exact scan over the delta with the base graph results. \
          Once an index's delta segment holds this many records (live and \
          tombstone), further inserts fall back to the slower inline \
          graph-splice path. pgcontext.compact() rebuilds the base graph from \
          the index's own pages and reopens an empty delta segment, restoring \
          the fast path; REINDEX does the same from the heap and additionally \
          reclaims disk. Neither runs automatically, so a write-heavy index \
          reaches the fallback and stays there until one is run. 0 disables \
          the delta segment entirely (every insert splices inline, matching \
          pre-delta releases).",
        &HNSW_DELTA_SEGMENT_LIMIT,
        0,
        i32::MAX,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_bool_guc(
        c"pgcontext.hnsw_compact_on_threshold",
        c"Compact an HNSW index when its delta segment fills.",
        c"When enabled, the insert that fills the delta segment also compacts \
          the index: it rebuilds the base graph from the index's own pages \
          and reopens an empty segment, so following inserts stay on the fast \
          append path. That insert pays the rebuild and is correspondingly \
          slow — a predictable stall in place of an unbounded slowdown, since \
          without it every later insert falls back to inline graph splicing. \
          Disabling this leaves the fallback in place until pgcontext.compact() \
          or REINDEX is run by hand.",
        &HNSW_COMPACT_ON_THRESHOLD,
        GucContext::Userset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"pgcontext.hnsw_compact_on_threshold_max_mb",
        c"Largest index an insert may compact by itself, in megabytes of vectors.",
        c"Bounds the stall pgcontext.hnsw_compact_on_threshold can impose. The \
          insert projects the rebuild's vector footprint from the metapage; if \
          it exceeds this, the insert declines to compact and takes the inline \
          path instead, leaving the rebuild to pgcontext.compact() or REINDEX. \
          Compaction time grows with the graph, so this is a latency control: \
          on a 100,000-row 384-dimension index (about 146MB of vectors) a \
          compaction takes roughly a minute, so the 1GB default admits stalls \
          of several minutes on the largest index it accepts. The default is \
          deliberately permissive so ordinary workloads keep self-maintaining; \
          lower it when a bounded write latency matters more than sustained \
          throughput. maintenance_work_mem applies independently and is often \
          the tighter limit. 0 disables this bound entirely.",
        &HNSW_COMPACT_ON_THRESHOLD_MAX_MB,
        0,
        i32::MAX,
        GucContext::Userset,
        GucFlags::default(),
    );
}

pub(crate) fn pgvector_compat_warnings_from_guc() -> bool {
    PGVECTOR_COMPAT_WARNINGS.get()
}

pub(crate) fn hnsw_delta_segment_limit_from_guc() -> u64 {
    u64::try_from(HNSW_DELTA_SEGMENT_LIMIT.get().max(0)).unwrap_or(0)
}

pub(crate) fn hnsw_compact_on_threshold_from_guc() -> bool {
    HNSW_COMPACT_ON_THRESHOLD.get()
}

/// Ceiling in bytes on the projected rebuild an insert may run itself, or
/// `None` when the operator has removed the bound.
pub(crate) fn hnsw_compact_on_threshold_max_bytes_from_guc() -> Option<usize> {
    let megabytes = HNSW_COMPACT_ON_THRESHOLD_MAX_MB.get();
    if megabytes <= 0 {
        return None;
    }
    usize::try_from(megabytes)
        .ok()
        .and_then(|value| value.checked_mul(1024 * 1024))
}

pub(crate) fn hnsw_config_from_gucs() -> HnswConfig {
    hnsw_config_from_values(
        HNSW_M.get(),
        HNSW_EF_CONSTRUCTION.get(),
        HNSW_EF_SEARCH.get(),
    )
}

fn hnsw_config_from_values(m: i32, ef_construction: i32, ef_search: i32) -> HnswConfig {
    let m = positive_setting_to_usize("pgcontext.hnsw_m", m);
    let ef_construction =
        positive_setting_to_usize("pgcontext.hnsw_ef_construction", ef_construction);
    let ef_search = positive_setting_to_usize("pgcontext.hnsw_ef_search", ef_search);

    match HnswConfig::new(m, ef_construction, ef_search) {
        Ok(config) => config,
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid HNSW configuration from pgContext settings: {error}"),
        ),
    }
}

pub(crate) fn hnsw_candidate_budget_from_guc() -> usize {
    hnsw_candidate_budget_from_value(HNSW_CANDIDATE_BUDGET.get())
}

pub(crate) fn hnsw_iterative_expansion_limit_from_guc() -> usize {
    hnsw_iterative_expansion_limit_from_value(HNSW_ITERATIVE_EXPANSION_LIMIT.get())
}

pub(crate) fn hnsw_recall_threshold_from_guc() -> f64 {
    hnsw_recall_threshold_from_value(HNSW_RECALL_THRESHOLD.get())
}

pub(crate) fn hnsw_shared_serving_enabled_from_guc() -> bool {
    HNSW_SHARED_SERVING.get()
}

pub(crate) fn hnsw_shared_serving_budget_bytes_from_guc() -> u64 {
    let megabytes = u64::from(HNSW_SHARED_SERVING_BUDGET_MB.get().max(0).unsigned_abs());
    megabytes * 1024 * 1024
}

pub(crate) fn hnsw_pack_on_first_use_from_guc() -> bool {
    HNSW_PACK_ON_FIRST_USE.get()
}

pub(crate) fn hnsw_mask_candidate_limit_from_guc() -> usize {
    usize::try_from(HNSW_MASK_CANDIDATE_LIMIT.get().max(0)).unwrap_or(0)
}

pub(crate) fn hnsw_build_parallel_workers_from_guc() -> usize {
    positive_setting_to_usize(
        "pgcontext.hnsw_build_parallel_workers",
        HNSW_BUILD_PARALLEL_WORKERS.get(),
    )
}

fn hnsw_candidate_budget_from_value(value: i32) -> usize {
    positive_setting_to_usize("pgcontext.hnsw_candidate_budget", value)
}

fn hnsw_iterative_expansion_limit_from_value(value: i32) -> usize {
    positive_setting_to_usize("pgcontext.hnsw_iterative_expansion_limit", value)
}

fn hnsw_recall_threshold_from_value(threshold: f64) -> f64 {
    if threshold.is_finite() && (0.0..=1.0).contains(&threshold) {
        return threshold;
    }

    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        format!("pgcontext.hnsw_recall_threshold must be between 0 and 1 inclusive: {threshold}"),
    )
}

fn positive_setting_to_usize(name: &str, value: i32) -> usize {
    match usize::try_from(value) {
        Ok(value) if value > 0 => value,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{name} must be positive: {value}"),
        ),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "policy constants are compile-time checked before crossing the PostgreSQL GUC i32 API"
)]
const fn policy_usize_to_i32(value: usize) -> i32 {
    assert!(value <= i32::MAX as usize);
    value as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hnsw_config_from_values_accepts_defaults() {
        let config = hnsw_config_from_values(
            DEFAULT_HNSW_M_I32,
            DEFAULT_HNSW_EF_CONSTRUCTION_I32,
            DEFAULT_HNSW_EF_SEARCH_I32,
        );

        assert_eq!(config.m(), DEFAULT_HNSW_M);
        assert_eq!(config.ef_construction(), DEFAULT_HNSW_EF_CONSTRUCTION);
        assert_eq!(config.ef_search(), DEFAULT_HNSW_EF_SEARCH);
    }

    #[test]
    fn hnsw_budget_settings_use_policy_defaults() {
        assert_eq!(
            hnsw_candidate_budget_from_value(DEFAULT_HNSW_CANDIDATE_BUDGET_I32),
            DEFAULT_HNSW_CANDIDATE_BUDGET
        );
        assert_eq!(
            hnsw_iterative_expansion_limit_from_value(DEFAULT_HNSW_ITERATIVE_EXPANSION_LIMIT_I32),
            DEFAULT_HNSW_ITERATIVE_EXPANSION_LIMIT
        );
        assert_eq!(
            hnsw_recall_threshold_from_value(DEFAULT_HNSW_RECALL_THRESHOLD),
            DEFAULT_HNSW_RECALL_THRESHOLD
        );
    }
}
