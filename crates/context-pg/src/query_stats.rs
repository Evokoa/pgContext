//! SQL-facing query cohort telemetry.

use pgrx::prelude::*;

use crate::error::raise_sql_error;
use crate::pgcontext::{QueryCohortStatus, QueryLatencyBucket, QueryLifecycleState};

const MAX_COHORT_LENGTH: usize = 128;

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
#[pg_extern(schema = "pgcontext", name = "record_query_stat", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn record_query_stat(
    collection: String,
    cohort: String,
    query_kind: String,
    result_count: i64,
    candidate_count: Option<i64>,
    latency_ms: f64,
) -> bool {
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
#[pg_extern(schema = "pgcontext", name = "record_query_stat", security_definer)]
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
#[pg_extern(schema = "pgcontext", name = "query_cohort_stats")]
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
               FROM pgcontext._query_stats AS stats
               JOIN pgcontext._collections AS collections USING (collection_id)
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
