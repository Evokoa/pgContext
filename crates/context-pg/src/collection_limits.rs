//! SQL-facing collection strict-mode limit policy.

use std::collections::BTreeSet;

use context_core::policy::{
    MAX_FILTER_NODES, MAX_HNSW_CANDIDATE_BUDGET, MAX_SEARCH_LIMIT, MAX_VECTOR_DIMENSIONS,
};
use context_core::{CollectionName, SourceKey};
use pgrx::prelude::*;

use crate::error::{raise_core_error, raise_sql_error};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CollectionLimits {
    strict_mode: bool,
    max_dimensions: Option<i32>,
    max_vectors: Option<i32>,
    max_points: Option<i64>,
    max_filter_nodes: Option<i32>,
    max_search_limit: Option<i32>,
    max_candidate_budget: Option<i32>,
    query_timeout_ms: Option<i32>,
    max_index_memory_bytes: Option<i64>,
}

type CollectionLimitsTuple = (
    bool,
    Option<i32>,
    Option<i32>,
    Option<i64>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
    Option<i64>,
);

#[derive(Debug, Copy, Clone)]
struct LimitCollection {
    id: i64,
    owner_role: pg_sys::Oid,
}

/// Configures optional strict-mode limits for one collection.
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "SQL surface intentionally exposes each limit as a named argument"
)]
pub fn configure_collection_limits(
    collection_name: String,
    strict_mode: bool,
    max_dimensions: Option<i32>,
    max_vectors: Option<i32>,
    max_points: Option<i64>,
    max_filter_nodes: Option<i32>,
    max_search_limit: Option<i32>,
    max_candidate_budget: Option<i32>,
    query_timeout_ms: Option<i32>,
    max_index_memory_bytes: Option<i64>,
) -> TableIterator<
    'static,
    (
        name!(strict_mode, bool),
        name!(max_dimensions, Option<i32>),
        name!(max_vectors, Option<i32>),
        name!(max_points, Option<i64>),
        name!(max_filter_nodes, Option<i32>),
        name!(max_search_limit, Option<i32>),
        name!(max_candidate_budget, Option<i32>),
        name!(query_timeout_ms, Option<i32>),
        name!(max_index_memory_bytes, Option<i64>),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = resolve_limit_collection(&collection_name);
    require_collection_owner(collection, &collection_name);
    validate_limits(
        &collection_name,
        &CollectionLimits {
            strict_mode,
            max_dimensions,
            max_vectors,
            max_points,
            max_filter_nodes,
            max_search_limit,
            max_candidate_budget,
            query_timeout_ms,
            max_index_memory_bytes,
        },
    );

    Spi::run_with_args(
        "UPDATE pgcontext._collections
            SET strict_mode = $1,
                max_dimensions = $2,
                max_vectors = $3,
                max_points = $4,
                max_filter_nodes = $5,
                max_search_limit = $6,
                max_candidate_budget = $7,
                query_timeout_ms = $8,
                max_index_memory_bytes = $9,
                updated_at = pg_catalog.now()
          WHERE collection_id = $10",
        &[
            strict_mode.into(),
            max_dimensions.into(),
            max_vectors.into(),
            max_points.into(),
            max_filter_nodes.into(),
            max_search_limit.into(),
            max_candidate_budget.into(),
            query_timeout_ms.into(),
            max_index_memory_bytes.into(),
            collection.id.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to configure collection limits: {error}"),
        )
    });

    TableIterator::once(limits_row(load_collection_limits(collection.id)))
}

/// Returns optional strict-mode limits for one collection.
#[pg_extern(stable, security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
pub fn collection_limits(
    collection_name: String,
) -> TableIterator<
    'static,
    (
        name!(strict_mode, bool),
        name!(max_dimensions, Option<i32>),
        name!(max_vectors, Option<i32>),
        name!(max_points, Option<i64>),
        name!(max_filter_nodes, Option<i32>),
        name!(max_search_limit, Option<i32>),
        name!(max_candidate_budget, Option<i32>),
        name!(query_timeout_ms, Option<i32>),
        name!(max_index_memory_bytes, Option<i64>),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = resolve_limit_collection(&collection_name);
    require_collection_owner(collection, &collection_name);
    TableIterator::once(limits_row(load_collection_limits(collection.id)))
}

pub(crate) fn enforce_vector_registration_limits(
    collection_id: i64,
    collection_name: &CollectionName,
    dimensions: usize,
) {
    let limits = load_collection_limits(collection_id);
    if !limits.strict_mode {
        return;
    }
    if let Some(max_dimensions) = limits.max_dimensions {
        let max_dimensions = i64::from(max_dimensions);
        if i64::try_from(dimensions).unwrap_or(i64::MAX) > max_dimensions {
            raise_limit_exceeded(
                collection_name,
                "max_dimensions",
                max_dimensions,
                dimensions,
            );
        }
    }
    if let Some(max_vectors) = limits.max_vectors {
        let current_vectors = collection_vector_count(collection_id);
        let projected_vectors = current_vectors.saturating_add(1);
        if projected_vectors > i64::from(max_vectors) {
            raise_limit_exceeded(
                collection_name,
                "max_vectors",
                i64::from(max_vectors),
                projected_vectors,
            );
        }
    }
}

pub(crate) fn enforce_point_upsert_limit(
    collection_id: i64,
    collection_name: &CollectionName,
    source_keys: &[SourceKey],
) {
    let limits = load_collection_limits(collection_id);
    let Some(max_points) = limits.max_points.filter(|_| limits.strict_mode) else {
        return;
    };
    let distinct_keys = source_keys
        .iter()
        .map(SourceKey::as_str)
        .collect::<BTreeSet<_>>();
    let active_points = active_point_count(collection_id);
    let active_requested = active_requested_point_count(collection_id, &distinct_keys);
    let newly_active = i64::try_from(distinct_keys.len())
        .unwrap_or(i64::MAX)
        .saturating_sub(active_requested);
    let projected_points = active_points.saturating_add(newly_active);
    if projected_points > max_points {
        raise_limit_exceeded(collection_name, "max_points", max_points, projected_points);
    }
}

pub(crate) fn enforce_search_limit(
    collection_id: i64,
    collection_name: &CollectionName,
    limit: usize,
) {
    let limits = load_collection_limits(collection_id);
    let Some(max_search_limit) = limits.max_search_limit.filter(|_| limits.strict_mode) else {
        return;
    };
    if i64::try_from(limit).unwrap_or(i64::MAX) > i64::from(max_search_limit) {
        raise_limit_exceeded(
            collection_name,
            "max_search_limit",
            i64::from(max_search_limit),
            limit,
        );
    }
}

pub(crate) fn enforce_candidate_budget(
    collection_id: i64,
    collection_name: &CollectionName,
    candidate_count: usize,
) {
    let limits = load_collection_limits(collection_id);
    let Some(max_candidate_budget) = limits.max_candidate_budget.filter(|_| limits.strict_mode)
    else {
        return;
    };
    if i64::try_from(candidate_count).unwrap_or(i64::MAX) > i64::from(max_candidate_budget) {
        raise_limit_exceeded(
            collection_name,
            "max_candidate_budget",
            i64::from(max_candidate_budget),
            candidate_count,
        );
    }
}

fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn validate_limits(collection_name: &CollectionName, limits: &CollectionLimits) {
    validate_i32_limit(
        collection_name,
        "max_dimensions",
        limits.max_dimensions,
        MAX_VECTOR_DIMENSIONS,
    );
    validate_i32_limit(
        collection_name,
        "max_vectors",
        limits.max_vectors,
        i32::MAX as usize,
    );
    validate_i64_limit(collection_name, "max_points", limits.max_points);
    validate_i32_limit(
        collection_name,
        "max_filter_nodes",
        limits.max_filter_nodes,
        MAX_FILTER_NODES,
    );
    validate_i32_limit(
        collection_name,
        "max_search_limit",
        limits.max_search_limit,
        MAX_SEARCH_LIMIT,
    );
    validate_i32_limit(
        collection_name,
        "max_candidate_budget",
        limits.max_candidate_budget,
        MAX_HNSW_CANDIDATE_BUDGET,
    );
    validate_i32_limit(
        collection_name,
        "query_timeout_ms",
        limits.query_timeout_ms,
        i32::MAX as usize,
    );
    validate_i64_limit(
        collection_name,
        "max_index_memory_bytes",
        limits.max_index_memory_bytes,
    );
}

fn validate_i32_limit(
    collection_name: &CollectionName,
    field: &'static str,
    value: Option<i32>,
    max_allowed: usize,
) {
    let Some(value) = value else {
        return;
    };
    if value <= 0 || usize::try_from(value).map_or(true, |value| value > max_allowed) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "invalid collection {} {field}: {value}",
                collection_name.as_str()
            ),
        );
    }
}

fn validate_i64_limit(collection_name: &CollectionName, field: &'static str, value: Option<i64>) {
    let Some(value) = value else {
        return;
    };
    if value <= 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "invalid collection {} {field}: {value}",
                collection_name.as_str()
            ),
        );
    }
}

fn resolve_limit_collection(collection_name: &CollectionName) -> LimitCollection {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT collection_id, owner_role
               FROM pgcontext._collections
              WHERE collection_name = $1",
            Some(1),
            &[collection_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query collection limits catalog: {error}"),
            ),
        };
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {}", collection_name.as_str()),
            );
        }
        let row = rows.first();
        LimitCollection {
            id: spi_required_column::<i64>(&row, 1, "collection_id"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 2, "owner_role"),
        }
    })
}

fn require_collection_owner(collection: LimitCollection, collection_name: &CollectionName) {
    let session_user = session_user();
    let is_owner = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role($1, $2, 'MEMBER')",
        &[session_user.as_str().into(), collection.owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check collection owner: {error}"),
        )
    })
    .unwrap_or(false);
    if !is_owner {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for collection {}",
                collection_name.as_str()
            ),
        );
    }
}

fn session_user() -> String {
    match Spi::get_one::<String>("SELECT SESSION_USER::text") {
        Ok(Some(user)) => user,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "SESSION_USER returned null",
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read SESSION_USER: {error}"),
        ),
    }
}

fn load_collection_limits(collection_id: i64) -> CollectionLimits {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT strict_mode,
                    max_dimensions,
                    max_vectors,
                    max_points,
                    max_filter_nodes,
                    max_search_limit,
                    max_candidate_budget,
                    query_timeout_ms,
                    max_index_memory_bytes
               FROM pgcontext._visible_collection_limits
              WHERE collection_id = $1",
            Some(1),
            &[collection_id.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query collection limits: {error}"),
            ),
        };
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist for id: {collection_id}"),
            );
        }
        let row = rows.first();
        CollectionLimits {
            strict_mode: spi_required_column::<bool>(&row, 1, "strict_mode"),
            max_dimensions: spi_optional_column::<i32>(&row, 2),
            max_vectors: spi_optional_column::<i32>(&row, 3),
            max_points: spi_optional_column::<i64>(&row, 4),
            max_filter_nodes: spi_optional_column::<i32>(&row, 5),
            max_search_limit: spi_optional_column::<i32>(&row, 6),
            max_candidate_budget: spi_optional_column::<i32>(&row, 7),
            query_timeout_ms: spi_optional_column::<i32>(&row, 8),
            max_index_memory_bytes: spi_optional_column::<i64>(&row, 9),
        }
    })
}

fn collection_vector_count(collection_id: i64) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT count(*) FROM pgcontext._collection_vectors WHERE collection_id = $1",
        &[collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to count collection vectors: {error}"),
        )
    })
    .unwrap_or(0)
}

fn active_point_count(collection_id: i64) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT count(*)
           FROM pgcontext._collection_points
          WHERE collection_id = $1
            AND deleted_at IS NULL",
        &[collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to count active collection points: {error}"),
        )
    })
    .unwrap_or(0)
}

fn active_requested_point_count(collection_id: i64, source_keys: &BTreeSet<&str>) -> i64 {
    source_keys
        .iter()
        .filter(|source_key| active_point_exists(collection_id, source_key))
        .count()
        .try_into()
        .unwrap_or(i64::MAX)
}

fn active_point_exists(collection_id: i64, source_key: &str) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pgcontext._collection_points
              WHERE collection_id = $1
                AND source_key = $2
                AND deleted_at IS NULL
         )",
        &[collection_id.into(), source_key.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check active collection point: {error}"),
        )
    })
    .unwrap_or(false)
}

fn limits_row(limits: CollectionLimits) -> CollectionLimitsTuple {
    (
        limits.strict_mode,
        limits.max_dimensions,
        limits.max_vectors,
        limits.max_points,
        limits.max_filter_nodes,
        limits.max_search_limit,
        limits.max_candidate_budget,
        limits.query_timeout_ms,
        limits.max_index_memory_bytes,
    )
}

fn raise_limit_exceeded(
    collection_name: &CollectionName,
    field: &'static str,
    max_value: i64,
    actual: impl TryInto<i64>,
) -> ! {
    let actual = actual.try_into().unwrap_or(i64::MAX);
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        format!(
            "collection {} {field} {max_value} exceeded: {actual}",
            collection_name.as_str()
        ),
    )
}

fn spi_required_column<T>(
    row: &spi::SpiTupleTable<'_>,
    index: usize,
    column_name: &'static str,
) -> T
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(Some(value)) => value,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("collection limits column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read collection limits column {column_name}: {error}"),
        ),
    }
}

fn spi_optional_column<T>(row: &spi::SpiTupleTable<'_>, index: usize) -> Option<T>
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(value) => value,
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read optional collection limits column {index}: {error}"),
        ),
    }
}
