//! SQL-facing query cohort telemetry.

use context_query::{
    Completion, ExecutionOutcome, ExecutionState, QueryError, QueryIr, QueryKind, ReadinessReason,
    StageDiagnostic, StageKind,
};
use pgrx::prelude::*;

use crate::error::raise_sql_error;
use crate::pgcontext::{QueryCohortStatus, QueryLatencyBucket, QueryLifecycleState};

const MAX_COHORT_LENGTH: usize = 128;
const AUTOMATIC_COHORT: &str = "automatic";

#[derive(Debug, Clone, Copy)]
struct QueryStatsCollection {
    collection_id: i64,
    owner_role: pg_sys::Oid,
}

#[derive(Debug, Clone, Copy)]
struct QueryStatDetail {
    result_count: i64,
    candidate_count: Option<i64>,
    rows_rechecked: i64,
    rows_pruned: i64,
    recall_threshold: Option<f64>,
    recall_achieved: Option<f64>,
    latency_ms: f64,
    latency_bucket: QueryLatencyBucket,
    lifecycle_state: QueryLifecycleState,
}

pub(crate) fn record_automatic_query_stat(
    observation: Option<crate::query_stats_async::ObservationToken>,
    diagnostics: &[StageDiagnostic],
    outcome: Result<&ExecutionOutcome, &QueryError>,
    used_fallback: bool,
) {
    let strategy = automatic_strategy_label(diagnostics, used_fallback, outcome.is_err());
    let visits = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.stage() == StageKind::Candidates)
        .fold(0usize, |total, diagnostic| {
            total.saturating_add(diagnostic.input_count())
        });
    let (result_count, filter_candidates, candidates, rechecks, stages, expansions, completion) =
        match outcome {
            Ok(outcome) => {
                let usage = outcome.usage();
                let completion = if outcome.state() == &ExecutionState::Ready {
                    completion_label(outcome.completion())
                } else if outcome.completion() == Completion::Cancelled {
                    "cancelled"
                } else {
                    "error"
                };
                (
                    outcome.points().len(),
                    usage.filter_candidates(),
                    usage.candidates(),
                    usage.rechecks(),
                    usage.stages(),
                    usage.expansions(),
                    completion,
                )
            }
            Err(error) => {
                let filter_candidates = diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.stage() == StageKind::FilterCandidates)
                    .fold(0usize, |total, diagnostic| {
                        total.saturating_add(diagnostic.output_count())
                    });
                let candidates = diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.stage() == StageKind::Candidates)
                    .fold(0usize, |total, diagnostic| {
                        total.saturating_add(diagnostic.output_count())
                    });
                let rechecks = diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.stage() == StageKind::SourceRecheck)
                    .fold(0usize, |total, diagnostic| {
                        total.saturating_add(diagnostic.input_count())
                    });
                (
                    0,
                    filter_candidates,
                    candidates,
                    rechecks,
                    diagnostics.len(),
                    0,
                    if matches!(
                        error,
                        QueryError::WorkBudgetExceeded { .. }
                            | QueryError::ArithmeticOverflow { .. }
                    ) {
                        "budget_exhausted"
                    } else {
                        "error"
                    },
                )
            }
        };
    let lifecycle = lifecycle_label(outcome, strategy, used_fallback);
    let Some(observation) = observation else {
        return;
    };
    crate::query_stats_async::finish(
        observation,
        crate::query_stats_async::AutomaticQuerySummary {
            result_count,
            visits,
            filter_candidates,
            candidates,
            rechecks,
            stages,
            expansions,
            completion,
            lifecycle,
            strategy,
        },
    );
}

pub(crate) fn begin_automatic_query_stat(
    collection_id: i64,
    query: &QueryIr,
    used_fallback: bool,
) -> Option<crate::query_stats_async::ObservationToken> {
    crate::query_stats_async::begin(collection_id, query_kind_label(query), used_fallback)
}

fn query_kind_label(query: &QueryIr) -> &'static str {
    if matches!(
        query.kind(),
        QueryKind::Prefetch { .. }
            | QueryKind::Weighted { .. }
            | QueryKind::ScoreThreshold { .. }
            | QueryKind::Formula { .. }
            | QueryKind::Rerank { .. }
    ) {
        "hybrid"
    } else if query.has_filter_in_subtree() {
        "search_filtered"
    } else {
        "search"
    }
}

pub(crate) fn completion_label(completion: Completion) -> &'static str {
    match completion {
        Completion::Complete => "complete",
        Completion::Cancelled => "cancelled",
        Completion::BudgetExhausted => "budget_exhausted",
    }
}

pub(crate) fn lifecycle_label(
    outcome: Result<&ExecutionOutcome, &QueryError>,
    strategy: &str,
    used_fallback: bool,
) -> &'static str {
    match outcome {
        Ok(outcome) => lifecycle_state_label(outcome.state(), strategy, used_fallback),
        // QueryError intentionally carries no serving-readiness category.
        // PostgreSQL ERROR paths are classified from their typed SQLSTATE by
        // the asynchronous error hook; never infer lifecycle from message text.
        Err(_) => "Unspecified",
    }
}

pub(crate) fn lifecycle_state_label(
    state: &ExecutionState,
    strategy: &str,
    used_fallback: bool,
) -> &'static str {
    match state {
        ExecutionState::Ready if used_fallback => "Fallback",
        ExecutionState::Ready if strategy.contains("hnsw") || strategy.ends_with("_ann") => {
            "Indexed"
        }
        ExecutionState::Ready => "Exact",
        ExecutionState::RebuildRequired { reason } | ExecutionState::NotReady { reason } => {
            match reason {
                ReadinessReason::GenerationMissing => "ArtifactMissing",
                ReadinessReason::ValidationFailed => "IndexCorrupt",
                _ => "IndexNotReady",
            }
        }
    }
}

fn automatic_strategy_label(
    diagnostics: &[StageDiagnostic],
    used_fallback: bool,
    executor_failed: bool,
) -> &'static str {
    if used_fallback {
        return "dense_exact_fallback";
    }
    let candidate_strategies = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.stage() == StageKind::Candidates)
        .map(StageDiagnostic::strategy)
        .collect::<Vec<_>>();
    let has_fusion = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.stage() == StageKind::Fusion);
    if has_fusion {
        if candidate_strategies
            .iter()
            .any(|strategy| strategy.contains("quantized"))
        {
            "composite_quantized_hnsw"
        } else if candidate_strategies
            .iter()
            .any(|strategy| strategy.contains("hnsw") || strategy.ends_with("_ann"))
        {
            "composite_hnsw"
        } else {
            "composite_exact"
        }
    } else {
        candidate_strategies
            .last()
            .copied()
            .unwrap_or(if executor_failed {
                "executor_error"
            } else {
                "unspecified"
            })
    }
}

/// Records one local query statistic sample.
///
/// This function is intended for local SQL-visible telemetry. It stores only
/// counts, cohort labels, query kind, and latency; callers must not put PII or
/// literal query text into `cohort`.
///
/// # Errors
///
/// Raises `undefined_object` for missing collections, `insufficient_privilege`
/// for non-owner callers, and `invalid_parameter_value` for invalid counters,
/// latency, cohort, or query kind.
#[pg_extern(name = "record_query_stat", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn record_query_stat(
    collection: String,
    cohort: String,
    query_kind: String,
    result_count: i64,
    candidate_count: Option<i64>,
    latency_ms: f64,
) -> bool {
    reject_reserved_automatic_cohort(&cohort);
    validate_query_stat_input(
        &cohort,
        &query_kind,
        result_count,
        candidate_count,
        latency_ms,
    );
    let collection_row = resolve_collection(&collection);
    require_collection_owner(collection_row, &collection);
    insert_query_stat(
        collection_row.collection_id,
        &cohort,
        &query_kind,
        QueryStatDetail {
            result_count,
            candidate_count,
            rows_rechecked: 0,
            rows_pruned: 0,
            recall_threshold: None,
            recall_achieved: None,
            latency_ms,
            latency_bucket: latency_bucket_for(latency_ms),
            lifecycle_state: QueryLifecycleState::Unspecified,
        },
    );
    true
}

/// Records one detailed local query statistic sample.
///
/// This overload keeps the original `record_query_stat` surface stable while
/// allowing monitoring clients to record explicit branchable counters for
/// candidate consideration, recheck/prune work, recall, latency, and serving
/// lifecycle state.
///
/// # Errors
///
/// Raises `undefined_object` for missing collections, `insufficient_privilege`
/// for non-owner callers, and `invalid_parameter_value` for invalid counters,
/// recall values, latency, cohort, or query kind.
#[pg_extern(name = "record_query_stat", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::too_many_arguments,
    reason = "SQL monitoring surface exposes each recorded query counter as a named argument"
)]
pub fn record_query_stat_detailed(
    collection: String,
    cohort: String,
    query_kind: String,
    result_count: i64,
    candidates_considered: Option<i64>,
    rows_rechecked: i64,
    rows_pruned: i64,
    recall_threshold: Option<f64>,
    recall_achieved: Option<f64>,
    latency_ms: f64,
    lifecycle_state: QueryLifecycleState,
) -> bool {
    reject_reserved_automatic_cohort(&cohort);
    validate_query_stat_input(
        &cohort,
        &query_kind,
        result_count,
        candidates_considered,
        latency_ms,
    );
    validate_detailed_query_stat_input(
        rows_rechecked,
        rows_pruned,
        recall_threshold,
        recall_achieved,
    );
    let collection_row = resolve_collection(&collection);
    require_collection_owner(collection_row, &collection);
    insert_query_stat(
        collection_row.collection_id,
        &cohort,
        &query_kind,
        QueryStatDetail {
            result_count,
            candidate_count: candidates_considered,
            rows_rechecked,
            rows_pruned,
            recall_threshold,
            recall_achieved,
            latency_ms,
            latency_bucket: latency_bucket_for(latency_ms),
            lifecycle_state,
        },
    );
    true
}

/// Returns grouped query statistics by collection, cohort, and query kind.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "query_cohort_stats")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_cohort_stats() -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(cohort, String),
        name!(query_kind, String),
        name!(query_count, i64),
        name!(total_results, i64),
        name!(total_candidates, Option<i64>),
        name!(total_rows_rechecked, i64),
        name!(total_rows_pruned, i64),
        name!(avg_recall_threshold, Option<f64>),
        name!(avg_recall_achieved, Option<f64>),
        name!(latency_bucket, QueryLatencyBucket),
        name!(lifecycle_state, QueryLifecycleState),
        name!(avg_latency_ms, f64),
        name!(status, QueryCohortStatus),
    ),
> {
    TableIterator::new(resolve_query_cohort_stats())
}

/// Returns cardinality-bounded rollups for automatically observed executions.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "query_execution_stats")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_execution_stats() -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(query_kind, String),
        name!(strategy, String),
        name!(query_count, i64),
        name!(total_visits, i64),
        name!(total_filter_candidates, i64),
        name!(total_candidates, i64),
        name!(total_rechecks, i64),
        name!(total_stages, i64),
        name!(total_expansions, i64),
        name!(completion, String),
        name!(latency_bucket, QueryLatencyBucket),
        name!(lifecycle_state, QueryLifecycleState),
        name!(avg_latency_ms, f64),
    ),
> {
    TableIterator::new(resolve_query_execution_stats())
}

/// Returns bounded health counters for this database's asynchronous telemetry queue.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "query_telemetry_queue_stats")]
pub fn query_telemetry_queue_stats() -> TableIterator<
    'static,
    (
        name!(transport, String),
        name!(delivery, String),
        name!(enqueued, i64),
        name!(persisted, i64),
        name!(dropped_contention, i64),
        name!(dropped_full, i64),
        name!(dropped_orphaned, i64),
        name!(database_slot_exhausted, i64),
        name!(worker_launch_failures, i64),
        name!(pending, i64),
        name!(worker_pid, Option<i32>),
    ),
> {
    let can_monitor =
        Spi::get_one::<bool>("SELECT pg_catalog.pg_has_role(SESSION_USER, 'pg_monitor', 'MEMBER')")
            .unwrap_or_default()
            .unwrap_or_default();
    if !can_monitor {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "query telemetry queue health requires membership in pg_monitor",
        );
    }
    let snapshot = crate::query_stats_async::snapshot();
    let count = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    TableIterator::once((
        "named_dsm_background_worker".to_owned(),
        "best_effort_may_duplicate".to_owned(),
        count(snapshot.enqueued),
        count(snapshot.persisted),
        count(snapshot.dropped_contention),
        count(snapshot.dropped_full),
        count(snapshot.dropped_orphaned),
        count(snapshot.database_slot_exhausted),
        count(snapshot.worker_launch_failures),
        count(snapshot.pending),
        snapshot.worker_pid,
    ))
}

fn validate_query_stat_input(
    cohort: &str,
    query_kind: &str,
    result_count: i64,
    candidate_count: Option<i64>,
    latency_ms: f64,
) {
    if cohort.is_empty() || cohort.len() > MAX_COHORT_LENGTH {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("query cohort must be 1..={MAX_COHORT_LENGTH} bytes"),
        );
    }
    if !cohort.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/')
    }) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "query cohort may contain only ASCII letters, digits, '_', '-', '.', ':', or '/'",
        );
    }
    if !matches!(
        query_kind,
        "search" | "search_filtered" | "candidate_recheck" | "hybrid"
    ) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported query kind: {query_kind}"),
        );
    }
    if result_count < 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("result_count must not be negative: {result_count}"),
        );
    }
    if let Some(candidate_count) = candidate_count
        && candidate_count < 0
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("candidate_count must not be negative: {candidate_count}"),
        );
    }
    if !latency_ms.is_finite() || latency_ms < 0.0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("latency_ms must be finite and non-negative: {latency_ms}"),
        );
    }
}

fn reject_reserved_automatic_cohort(cohort: &str) {
    if cohort == AUTOMATIC_COHORT {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "query cohort 'automatic' is reserved for executor telemetry",
        );
    }
}

fn validate_detailed_query_stat_input(
    rows_rechecked: i64,
    rows_pruned: i64,
    recall_threshold: Option<f64>,
    recall_achieved: Option<f64>,
) {
    if rows_rechecked < 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("rows_rechecked must not be negative: {rows_rechecked}"),
        );
    }
    if rows_pruned < 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("rows_pruned must not be negative: {rows_pruned}"),
        );
    }
    validate_optional_recall("recall_threshold", recall_threshold);
    validate_optional_recall("recall_achieved", recall_achieved);
}

fn validate_optional_recall(field_name: &'static str, value: Option<f64>) {
    if let Some(value) = value
        && (!value.is_finite() || !(0.0..=1.0).contains(&value))
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{field_name} must be finite and between 0 and 1 inclusive: {value}"),
        );
    }
}

fn latency_bucket_for(latency_ms: f64) -> QueryLatencyBucket {
    if latency_ms < 1.0 {
        QueryLatencyBucket::Lt1Ms
    } else if latency_ms < 10.0 {
        QueryLatencyBucket::Lt10Ms
    } else if latency_ms < 100.0 {
        QueryLatencyBucket::Lt100Ms
    } else if latency_ms < 1_000.0 {
        QueryLatencyBucket::Lt1S
    } else {
        QueryLatencyBucket::Gte1S
    }
}

fn resolve_collection(collection: &str) -> QueryStatsCollection {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collection_id, owner_role
               FROM pgcontext._collections
              WHERE collection_name = $1",
            Some(1),
            &[collection.into()],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {collection}"),
            );
        }
        let row = rows.first();
        Ok::<_, spi::Error>(QueryStatsCollection {
            collection_id: required_column(row.get::<i64>(1)?, "collection_id"),
            owner_role: required_column(row.get::<pg_sys::Oid>(2)?, "owner_role"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("query stats collection lookup failed: {error}"),
        )
    })
}

fn require_collection_owner(collection: QueryStatsCollection, collection_name: &str) {
    let is_owner = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, $1::oid, 'MEMBER')",
        &[collection.owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check collection ownership: {error}"),
        )
    })
    .unwrap_or_default();
    if !is_owner {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for collection {collection_name}"),
        );
    }
}

fn insert_query_stat(collection_id: i64, cohort: &str, query_kind: &str, detail: QueryStatDetail) {
    Spi::run_with_args(
        "INSERT INTO pgcontext._query_stats (
             collection_id,
             cohort,
             query_kind,
             result_count,
             candidate_count,
             rows_rechecked,
             rows_pruned,
             recall_threshold,
             recall_achieved,
             latency_bucket,
             lifecycle_state,
             latency_ms
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        &[
            collection_id.into(),
            cohort.into(),
            query_kind.into(),
            detail.result_count.into(),
            detail.candidate_count.into(),
            detail.rows_rechecked.into(),
            detail.rows_pruned.into(),
            detail.recall_threshold.into(),
            detail.recall_achieved.into(),
            format!("{:?}", detail.latency_bucket).into(),
            format!("{:?}", detail.lifecycle_state).into(),
            detail.latency_ms.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to record query stat: {error}"),
        )
    });
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
fn resolve_query_cohort_stats() -> Vec<(
    String,
    String,
    String,
    i64,
    i64,
    Option<i64>,
    i64,
    i64,
    Option<f64>,
    Option<f64>,
    QueryLatencyBucket,
    QueryLifecycleState,
    f64,
    QueryCohortStatus,
)> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collections.collection_name,
                    stats.cohort,
                    stats.query_kind,
                    stats.latency_bucket,
                    stats.lifecycle_state,
                    count(*)::bigint,
                    sum(stats.result_count)::bigint,
                    sum(stats.candidate_count)::bigint,
                    sum(stats.rows_rechecked)::bigint,
                    sum(stats.rows_pruned)::bigint,
                    avg(stats.recall_threshold)::double precision,
                    avg(stats.recall_achieved)::double precision,
                    avg(stats.latency_ms)::double precision
               FROM pgcontext._visible_query_stats AS stats
               JOIN pgcontext._visible_collections AS collections USING (collection_id)
              GROUP BY collections.collection_name,
                       stats.cohort,
                       stats.query_kind,
                       stats.latency_bucket,
                       stats.lifecycle_state
              ORDER BY collections.collection_name,
                       stats.cohort,
                       stats.query_kind,
                       stats.latency_bucket,
                       stats.lifecycle_state",
            None,
            &[],
        )?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                required_column(row.get::<String>(1)?, "collection_name"),
                required_column(row.get::<String>(2)?, "cohort"),
                required_column(row.get::<String>(3)?, "query_kind"),
                required_column(row.get::<i64>(6)?, "query_count"),
                required_column(row.get::<i64>(7)?, "total_results"),
                row.get::<i64>(8)?,
                required_column(row.get::<i64>(9)?, "total_rows_rechecked"),
                required_column(row.get::<i64>(10)?, "total_rows_pruned"),
                row.get::<f64>(11)?,
                row.get::<f64>(12)?,
                parse_latency_bucket(required_column(row.get::<String>(4)?, "latency_bucket")),
                parse_lifecycle_state(required_column(row.get::<String>(5)?, "lifecycle_state")),
                required_column(row.get::<f64>(13)?, "avg_latency_ms"),
                QueryCohortStatus::Observed,
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("query cohort stats lookup failed: {error}"),
        )
    })
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
fn resolve_query_execution_stats() -> Vec<(
    String,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    i64,
    String,
    QueryLatencyBucket,
    QueryLifecycleState,
    f64,
)> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collections.collection_name,
                    stats.query_kind,
                    stats.strategy,
                    count(*)::bigint,
                    sum(stats.visits)::bigint,
                    sum(stats.filter_candidates)::bigint,
                    sum(stats.candidates)::bigint,
                    sum(stats.rechecks)::bigint,
                    sum(stats.stages)::bigint,
                    sum(stats.expansions)::bigint,
                    stats.completion,
                    stats.latency_bucket,
                    stats.lifecycle_state,
                    avg(stats.latency_ms)::double precision
               FROM pgcontext._visible_query_stats AS stats
               JOIN pgcontext._visible_collections AS collections USING (collection_id)
              WHERE stats.cohort = 'automatic'
              GROUP BY collections.collection_name,
                       stats.query_kind,
                       stats.strategy,
                       stats.completion,
                       stats.latency_bucket,
                       stats.lifecycle_state
              ORDER BY collections.collection_name,
                       stats.query_kind,
                       stats.strategy,
                       stats.completion,
                       stats.latency_bucket,
                       stats.lifecycle_state",
            None,
            &[],
        )?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                required_column(row.get::<String>(1)?, "collection_name"),
                required_column(row.get::<String>(2)?, "query_kind"),
                required_column(row.get::<String>(3)?, "strategy"),
                required_column(row.get::<i64>(4)?, "query_count"),
                required_column(row.get::<i64>(5)?, "total_visits"),
                required_column(row.get::<i64>(6)?, "total_filter_candidates"),
                required_column(row.get::<i64>(7)?, "total_candidates"),
                required_column(row.get::<i64>(8)?, "total_rechecks"),
                required_column(row.get::<i64>(9)?, "total_stages"),
                required_column(row.get::<i64>(10)?, "total_expansions"),
                required_column(row.get::<String>(11)?, "completion"),
                parse_latency_bucket(required_column(row.get::<String>(12)?, "latency_bucket")),
                parse_lifecycle_state(required_column(row.get::<String>(13)?, "lifecycle_state")),
                required_column(row.get::<f64>(14)?, "avg_latency_ms"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("automatic query stats lookup failed: {error}"),
        )
    })
}

fn parse_latency_bucket(value: String) -> QueryLatencyBucket {
    match value.as_str() {
        "Lt1Ms" => QueryLatencyBucket::Lt1Ms,
        "Lt10Ms" => QueryLatencyBucket::Lt10Ms,
        "Lt100Ms" => QueryLatencyBucket::Lt100Ms,
        "Lt1S" => QueryLatencyBucket::Lt1S,
        "Gte1S" => QueryLatencyBucket::Gte1S,
        "Unspecified" => QueryLatencyBucket::Unspecified,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("query stats catalog has invalid latency bucket: {value}"),
        ),
    }
}

fn parse_lifecycle_state(value: String) -> QueryLifecycleState {
    match value.as_str() {
        "Unspecified" => QueryLifecycleState::Unspecified,
        "Exact" => QueryLifecycleState::Exact,
        "Indexed" => QueryLifecycleState::Indexed,
        "Fallback" => QueryLifecycleState::Fallback,
        "IndexNotReady" => QueryLifecycleState::IndexNotReady,
        "IndexCorrupt" => QueryLifecycleState::IndexCorrupt,
        "ArtifactMissing" => QueryLifecycleState::ArtifactMissing,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("query stats catalog has invalid lifecycle state: {value}"),
        ),
    }
}

fn required_column<T>(value: Option<T>, column_name: &'static str) -> T {
    match value {
        Some(value) => value,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("query stats catalog column was unexpectedly null: {column_name}"),
        ),
    }
}
