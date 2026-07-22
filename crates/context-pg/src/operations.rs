//! SQL-facing operational status functions.

use std::collections::BTreeSet;
use std::mem::size_of;

use context_core::ContextError;
use context_index::HnswNodeId;
use pgrx::prelude::*;

use crate::error::{raise_context_error, raise_sql_error, sqlstate_for_context_error};
use crate::pgcontext::{
    IndexDiagnosticStatus, IndexLifecycleStatus, IndexMemoryEstimateStatus, OptimizationStatus,
    RecallCheckStatus, VacuumAdviceStatus,
};

mod advisor;

#[derive(Debug, Clone)]
struct IndexStatusRow {
    index_schema: String,
    index_name: String,
    table_schema: String,
    table_name: String,
    access_method: String,
    is_valid: bool,
    is_ready: bool,
    is_live: bool,
}

#[derive(Debug, Clone)]
struct IndexDiagnostic {
    status: IndexDiagnosticStatus,
    context_error: Option<ContextError>,
    repair_advice: &'static str,
}

#[derive(Debug, Clone)]
struct IndexMemoryCatalogRow {
    index_schema: String,
    index_name: String,
    table_schema: String,
    table_name: String,
    access_method: String,
    vector_column: Option<String>,
    estimated_rows: Option<i64>,
}

#[derive(Debug, Clone, Copy)]
struct IndexMemoryEstimate {
    estimated_rows: i64,
    dimensions: i32,
    vector_bytes: i64,
    link_bytes: i64,
    total_bytes: i64,
    status: IndexMemoryEstimateStatus,
}

#[derive(Debug, Clone)]
struct OptimizationCatalogRow {
    collection_name: String,
    table_schema: Option<String>,
    table_name: Option<String>,
    has_source_table: bool,
    source_table_exists: bool,
    registered_vectors: i64,
    active_points: i64,
    filter_fields: i64,
    hnsw_indexes: i64,
}

#[derive(Debug, Clone)]
struct VacuumAdviceRow {
    index_schema: String,
    index_name: String,
    table_schema: String,
    table_name: String,
    access_method: String,
    estimated_index_tuples: Option<i64>,
    index_pages: i64,
    dead_table_tuples: i64,
}

/// Returns structured PostgreSQL catalog status for an index.
///
/// # Errors
///
/// Raises `undefined_object` when `index_name` does not resolve to an index.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "index_status")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn index_status(
    index_name: String,
) -> TableIterator<
    'static,
    (
        name!(index_schema, String),
        name!(index_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(access_method, String),
        name!(is_valid, bool),
        name!(is_ready, bool),
        name!(is_live, bool),
        name!(status, IndexLifecycleStatus),
    ),
> {
    let row = resolve_index_status(&index_name);
    TableIterator::once((
        row.index_schema,
        row.index_name,
        row.table_schema,
        row.table_name,
        row.access_method,
        row.is_valid,
        row.is_ready,
        row.is_live,
        lifecycle_status(row.is_valid, row.is_ready, row.is_live),
    ))
}

/// Returns typed serving diagnostics and repair advice for an index.
///
/// The function reports PostgreSQL catalog readiness for `pgcontext_hnsw`
/// indexes without exposing vector contents or source-table rows. It maps
/// invalid/not-ready catalog states to stable pgContext error categories and
/// advice that operators can apply before routing approximate search traffic.
///
/// # Errors
///
/// Raises `undefined_object` when `index_name` does not resolve to an index.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "index_diagnostics")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn index_diagnostics(
    index_name: String,
) -> TableIterator<
    'static,
    (
        name!(index_schema, String),
        name!(index_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(access_method, String),
        name!(status, IndexDiagnosticStatus),
        name!(context_error, Option<String>),
        name!(sqlstate, Option<String>),
        name!(repair_advice, String),
    ),
> {
    let row = resolve_index_status(&index_name);
    let diagnostic = diagnose_index(&row, &index_name);

    TableIterator::once((
        row.index_schema,
        row.index_name,
        row.table_schema,
        row.table_name,
        row.access_method,
        diagnostic.status,
        diagnostic.context_error.map(|error| format!("{error:?}")),
        diagnostic
            .context_error
            .map(|error| sqlstate_for_context_error(error).to_owned()),
        diagnostic.repair_advice.to_owned(),
    ))
}

/// Estimates memory owned by an index's in-memory search payload.
///
/// For `pgcontext_hnsw`, the estimate projects dense `f32` vector payload bytes
/// plus retained graph-neighbor identifier bytes from PostgreSQL index row
/// estimates and an observed non-null indexed vector value. It excludes
/// allocator-dependent container overhead.
///
/// # Errors
///
/// Raises `undefined_object` when `index_name` does not resolve to an index.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "estimate_index_memory")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn estimate_index_memory(
    index_name: String,
) -> TableIterator<
    'static,
    (
        name!(index_schema, String),
        name!(index_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(access_method, String),
        name!(estimated_rows, i64),
        name!(dimensions, i32),
        name!(vector_bytes, i64),
        name!(link_bytes, i64),
        name!(total_bytes, i64),
        name!(status, IndexMemoryEstimateStatus),
    ),
> {
    let row = resolve_index_memory_catalog(&index_name);
    let estimate = estimate_index_memory_from_catalog(&row);

    TableIterator::once((
        row.index_schema,
        row.index_name,
        row.table_schema,
        row.table_name,
        row.access_method,
        estimate.estimated_rows,
        estimate.dimensions,
        estimate.vector_bytes,
        estimate.link_bytes,
        estimate.total_bytes,
        estimate.status,
    ))
}

/// Reports optimizer readiness for a pgContext collection.
///
/// The status summarizes whether the collection has the catalog artifacts
/// needed for exact table-backed retrieval and whether any registered vector has
/// a matching `pgcontext_hnsw` index. Counts are catalog-derived diagnostics for
/// operators tracking recall or latency changes.
///
/// # Errors
///
/// Raises `undefined_object` when `collection` does not resolve to a pgContext
/// collection.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "optimization_status")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn optimization_status(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(table_schema, Option<String>),
        name!(table_name, Option<String>),
        name!(has_source_table, bool),
        name!(source_table_exists, bool),
        name!(registered_vectors, i64),
        name!(active_points, i64),
        name!(filter_fields, i64),
        name!(hnsw_indexes, i64),
        name!(status, OptimizationStatus),
    ),
> {
    let row = resolve_optimization_catalog(&collection);
    let status = collection_optimization_status(&row);

    TableIterator::once((
        row.collection_name,
        row.table_schema,
        row.table_name,
        row.has_source_table,
        row.source_table_exists,
        row.registered_vectors,
        row.active_points,
        row.filter_fields,
        row.hnsw_indexes,
        status,
    ))
}

/// Reports PostgreSQL-visible vacuum guidance for an index.
///
/// For `pgcontext_hnsw`, the advice uses PostgreSQL catalog page and tuple
/// estimates plus dead heap tuple statistics for the owning table. It reflects
/// the current physical index baseline; future graph persistence can add
/// graph-specific pruning signals without changing the structured row shape.
///
/// # Errors
///
/// Raises `undefined_object` when `index_name` does not resolve to an index.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "vacuum_advice")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn vacuum_advice(
    index_name: String,
) -> TableIterator<
    'static,
    (
        name!(index_schema, String),
        name!(index_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(access_method, String),
        name!(estimated_index_tuples, i64),
        name!(index_pages, i64),
        name!(dead_table_tuples, i64),
        name!(status, VacuumAdviceStatus),
    ),
> {
    let row = resolve_vacuum_advice(&index_name);
    let (estimated_index_tuples, status) = vacuum_advice_status(&row);

    TableIterator::once((
        row.index_schema,
        row.index_name,
        row.table_schema,
        row.table_name,
        row.access_method,
        estimated_index_tuples,
        row.index_pages,
        row.dead_table_tuples,
        status,
    ))
}

/// Compares candidate point IDs against exact point IDs and reports recall.
///
/// Duplicate point IDs are counted once. Empty exact results return
/// [`RecallCheckStatus::EmptyExact`] and recall `1.0`.
///
/// # Errors
///
/// Raises `invalid_parameter_value` when `min_recall` is outside `0..=1` or
/// when either ID array contains a negative point ID. Raises
/// `program_limit_exceeded` when either input array exceeds the recall-check
/// point budget.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "recall_check")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn recall_check(
    exact_point_ids: Vec<i64>,
    candidate_point_ids: Vec<i64>,
    min_recall: f64,
) -> TableIterator<
    'static,
    (
        name!(exact_count, i64),
        name!(candidate_count, i64),
        name!(intersection_count, i64),
        name!(recall, f64),
        name!(status, RecallCheckStatus),
    ),
> {
    validate_min_recall(min_recall);
    enforce_recall_check_budget(exact_point_ids.len(), "exact_point_ids");
    enforce_recall_check_budget(candidate_point_ids.len(), "candidate_point_ids");

    let exact = point_id_set(exact_point_ids, "exact_point_ids");
    let candidates = point_id_set(candidate_point_ids, "candidate_point_ids");
    let intersection_count = exact.intersection(&candidates).count();

    let (recall, status) = if exact.is_empty() {
        (1.0, RecallCheckStatus::EmptyExact)
    } else {
        let recall = recall_ratio(intersection_count, exact.len());
        let status = if recall >= min_recall {
            RecallCheckStatus::Passing
        } else {
            RecallCheckStatus::Failing
        };
        (recall, status)
    };

    TableIterator::once((
        usize_to_i64(exact.len(), "exact_count"),
        usize_to_i64(candidates.len(), "candidate_count"),
        usize_to_i64(intersection_count, "intersection_count"),
        recall,
        status,
    ))
}

fn enforce_recall_check_budget(point_count: usize, input_name: &str) {
    let max = context_core::policy::MAX_RECALL_CHECK_POINT_IDS;
    if point_count > max {
        raise_context_error(
            ContextError::RecallBudgetExceeded,
            format!("{input_name} exceeds recall-check point budget {max}: {point_count}"),
        );
    }
}

fn lifecycle_status(is_valid: bool, is_ready: bool, is_live: bool) -> IndexLifecycleStatus {
    match (is_valid, is_ready, is_live) {
        (true, true, true) => IndexLifecycleStatus::Ready,
        (_, _, false) => IndexLifecycleStatus::Invalid,
        _ => IndexLifecycleStatus::Building,
    }
}

fn diagnose_index(row: &IndexStatusRow, index_name: &str) -> IndexDiagnostic {
    if row.access_method != "pgcontext_hnsw" {
        return IndexDiagnostic {
            status: IndexDiagnosticStatus::UnsupportedAccessMethod,
            context_error: None,
            repair_advice: "Create a pgcontext_hnsw index for vector serving diagnostics.",
        };
    }

    if !row.is_live || !row.is_valid {
        return IndexDiagnostic {
            status: IndexDiagnosticStatus::IndexCorrupt,
            context_error: Some(ContextError::IndexCorrupt),
            repair_advice: "Run REINDEX, or drop and recreate the pgcontext_hnsw index from the source table.",
        };
    }

    if !row.is_ready {
        return IndexDiagnostic {
            status: IndexDiagnosticStatus::IndexNotReady,
            context_error: Some(ContextError::IndexNotReady),
            repair_advice: "Wait for index build completion before using the pgcontext_hnsw serving path.",
        };
    }

    let memory_catalog = resolve_index_memory_catalog(index_name);
    if memory_catalog.estimated_rows.is_none() {
        return IndexDiagnostic {
            status: IndexDiagnosticStatus::IndexNotReady,
            context_error: Some(ContextError::IndexNotReady),
            repair_advice: "Run ANALYZE on the source table and retry index diagnostics.",
        };
    }

    IndexDiagnostic {
        status: IndexDiagnosticStatus::Ready,
        context_error: None,
        repair_advice: "No repair action is currently recommended.",
    }
}

fn resolve_index_status(index_name: &str) -> IndexStatusRow {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT index_namespace.nspname::text,
                    index_class.relname::text,
                    table_namespace.nspname::text,
                    table_class.relname::text,
                    access_method.amname::text,
                    index_catalog.indisvalid,
                    index_catalog.indisready,
                    index_catalog.indislive
               FROM pg_catalog.pg_class AS index_class
               JOIN pg_catalog.pg_namespace AS index_namespace
                 ON index_namespace.oid = index_class.relnamespace
               JOIN pg_catalog.pg_index AS index_catalog
                 ON index_catalog.indexrelid = index_class.oid
               JOIN pg_catalog.pg_class AS table_class
                 ON table_class.oid = index_catalog.indrelid
               JOIN pg_catalog.pg_namespace AS table_namespace
                 ON table_namespace.oid = table_class.relnamespace
               JOIN pg_catalog.pg_am AS access_method
                 ON access_method.oid = index_class.relam
              WHERE index_class.oid = pg_catalog.to_regclass($1)::oid
                AND index_class.relkind = 'i'",
            Some(1),
            &[index_name.into()],
        )?;

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("index does not exist: {index_name}"),
            );
        }

        let row = rows.first();
        Ok::<_, spi::Error>(IndexStatusRow {
            index_schema: required_column(row.get::<String>(1)?, "index_schema"),
            index_name: required_column(row.get::<String>(2)?, "index_name"),
            table_schema: required_column(row.get::<String>(3)?, "table_schema"),
            table_name: required_column(row.get::<String>(4)?, "table_name"),
            access_method: required_column(row.get::<String>(5)?, "access_method"),
            is_valid: required_column(row.get::<bool>(6)?, "is_valid"),
            is_ready: required_column(row.get::<bool>(7)?, "is_ready"),
            is_live: required_column(row.get::<bool>(8)?, "is_live"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("index status query failed: {error}"),
        )
    })
}

fn resolve_index_memory_catalog(index_name: &str) -> IndexMemoryCatalogRow {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT index_namespace.nspname::text,
                    index_class.relname::text,
                    table_namespace.nspname::text,
                    table_class.relname::text,
                    access_method.amname::text,
                    attribute.attname::text,
                    CASE
                        WHEN index_class.reltuples < 0 THEN NULL
                        ELSE index_class.reltuples::bigint
                    END
               FROM pg_catalog.pg_class AS index_class
               JOIN pg_catalog.pg_namespace AS index_namespace
                 ON index_namespace.oid = index_class.relnamespace
               JOIN pg_catalog.pg_index AS index_catalog
                 ON index_catalog.indexrelid = index_class.oid
               JOIN pg_catalog.pg_class AS table_class
                 ON table_class.oid = index_catalog.indrelid
               JOIN pg_catalog.pg_namespace AS table_namespace
                 ON table_namespace.oid = table_class.relnamespace
               JOIN pg_catalog.pg_am AS access_method
                 ON access_method.oid = index_class.relam
          LEFT JOIN LATERAL pg_catalog.unnest(index_catalog.indkey)
                    WITH ORDINALITY AS index_key(attnum, ordinal_position)
                 ON index_key.ordinal_position = 1
          LEFT JOIN pg_catalog.pg_attribute AS attribute
                 ON attribute.attrelid = index_catalog.indrelid
                AND attribute.attnum = index_key.attnum
              WHERE index_class.oid = pg_catalog.to_regclass($1)::oid
                AND index_class.relkind = 'i'",
            Some(1),
            &[index_name.into()],
        )?;

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("index does not exist: {index_name}"),
            );
        }

        let row = rows.first();
        Ok::<_, spi::Error>(IndexMemoryCatalogRow {
            index_schema: required_column(row.get::<String>(1)?, "index_schema"),
            index_name: required_column(row.get::<String>(2)?, "index_name"),
            table_schema: required_column(row.get::<String>(3)?, "table_schema"),
            table_name: required_column(row.get::<String>(4)?, "table_name"),
            access_method: required_column(row.get::<String>(5)?, "access_method"),
            vector_column: row.get::<String>(6)?,
            estimated_rows: row.get::<i64>(7)?,
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("index memory catalog query failed: {error}"),
        )
    })
}

fn estimate_index_memory_from_catalog(row: &IndexMemoryCatalogRow) -> IndexMemoryEstimate {
    if row.access_method != "pgcontext_hnsw" {
        return unavailable_index_memory(IndexMemoryEstimateStatus::UnsupportedAccessMethod);
    }

    let Some(estimated_rows) = row.estimated_rows else {
        return unavailable_index_memory(IndexMemoryEstimateStatus::UnavailableStatistics);
    };
    if estimated_rows < 0 {
        return unavailable_index_memory(IndexMemoryEstimateStatus::UnavailableStatistics);
    }
    if estimated_rows == 0 {
        return IndexMemoryEstimate {
            estimated_rows,
            dimensions: 0,
            vector_bytes: 0,
            link_bytes: 0,
            total_bytes: 0,
            status: IndexMemoryEstimateStatus::Projected,
        };
    }

    let Some(vector_column) = &row.vector_column else {
        return unavailable_index_memory(IndexMemoryEstimateStatus::UnavailableStatistics);
    };
    let Some(dimensions) = observed_vector_dimensions(row, vector_column) else {
        return unavailable_index_memory(IndexMemoryEstimateStatus::UnavailableStatistics);
    };

    projected_hnsw_index_memory(estimated_rows, dimensions)
}

fn unavailable_index_memory(status: IndexMemoryEstimateStatus) -> IndexMemoryEstimate {
    IndexMemoryEstimate {
        estimated_rows: 0,
        dimensions: 0,
        vector_bytes: 0,
        link_bytes: 0,
        total_bytes: 0,
        status,
    }
}

fn projected_hnsw_index_memory(estimated_rows: i64, dimensions: i32) -> IndexMemoryEstimate {
    let row_count = i64_to_usize(estimated_rows, "estimated_rows");
    let dimension_count = i32_to_usize(dimensions, "dimensions");
    let vector_bytes = checked_i64_product(
        &[row_count, dimension_count, size_of::<f32>()],
        "vector_bytes",
    );
    let link_count = projected_hnsw_link_count(row_count);
    let link_bytes = checked_i64_product(&[link_count, size_of::<HnswNodeId>()], "link_bytes");
    let total_bytes = vector_bytes.checked_add(link_bytes).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "estimated index memory exceeds bigint range",
        )
    });

    IndexMemoryEstimate {
        estimated_rows,
        dimensions,
        vector_bytes,
        link_bytes,
        total_bytes,
        status: IndexMemoryEstimateStatus::Projected,
    }
}

fn projected_hnsw_link_count(row_count: usize) -> usize {
    let config = hnsw_memory_estimate_config();
    row_count.saturating_mul(config.m().min(row_count.saturating_sub(1)))
}

fn hnsw_memory_estimate_config() -> context_index::HnswConfig {
    crate::settings::hnsw_config_from_gucs()
}

fn observed_vector_dimensions(row: &IndexMemoryCatalogRow, vector_column: &str) -> Option<i32> {
    let sql = format!(
        "SELECT pgcontext.vector_dims({column})
           FROM {schema}.{table_name}
          WHERE {column} IS NOT NULL
          LIMIT 1",
        schema = quote_identifier(&row.table_schema),
        table_name = quote_identifier(&row.table_name),
        column = quote_identifier(vector_column),
    );

    Spi::get_one::<i32>(&sql).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("index memory dimension query failed: {error}"),
        )
    })
}

fn resolve_optimization_catalog(collection: &str) -> OptimizationCatalogRow {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collections.collection_name,
                    collections.source_schema_name,
                    collections.source_table_name,
                    collections.source_table_oid IS NOT NULL,
                    source_class.oid IS NOT NULL,
                    (
                        SELECT count(*)::bigint
                          FROM pgcontext._collection_vectors AS vectors
                         WHERE vectors.collection_id = collections.collection_id
                    ),
                    (
                        SELECT count(*)::bigint
                          FROM pgcontext._collection_points AS points
                         WHERE points.collection_id = collections.collection_id
                           AND points.deleted_at IS NULL
                    ),
                    (
                        SELECT count(*)::bigint
                          FROM pgcontext._collection_payload_columns AS payload_columns
                         WHERE payload_columns.collection_id = collections.collection_id
                    ),
                    (
                        SELECT count(DISTINCT index_class.oid)::bigint
                          FROM pgcontext._collection_vectors AS vectors
                          JOIN pg_catalog.pg_index AS index_catalog
                            ON index_catalog.indrelid = vectors.source_table_oid
                          JOIN pg_catalog.pg_class AS index_class
                            ON index_class.oid = index_catalog.indexrelid
                          JOIN pg_catalog.pg_am AS access_method
                            ON access_method.oid = index_class.relam
                         WHERE vectors.collection_id = collections.collection_id
                           AND access_method.amname = 'pgcontext_hnsw'
                           AND EXISTS (
                               SELECT 1
                                 FROM pg_catalog.unnest(index_catalog.indkey) AS index_key(attnum)
                                WHERE index_key.attnum = vectors.vector_attnum
                           )
                    )
               FROM pgcontext._collections AS collections
          LEFT JOIN pg_catalog.pg_class AS source_class
                 ON source_class.oid = collections.source_table_oid
              WHERE collections.collection_name = $1",
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
        Ok::<_, spi::Error>(OptimizationCatalogRow {
            collection_name: required_column(row.get::<String>(1)?, "collection_name"),
            table_schema: row.get::<String>(2)?,
            table_name: row.get::<String>(3)?,
            has_source_table: required_column(row.get::<bool>(4)?, "has_source_table"),
            source_table_exists: required_column(row.get::<bool>(5)?, "source_table_exists"),
            registered_vectors: required_column(row.get::<i64>(6)?, "registered_vectors"),
            active_points: required_column(row.get::<i64>(7)?, "active_points"),
            filter_fields: required_column(row.get::<i64>(8)?, "filter_fields"),
            hnsw_indexes: required_column(row.get::<i64>(9)?, "hnsw_indexes"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("optimization status query failed: {error}"),
        )
    })
}

fn collection_optimization_status(row: &OptimizationCatalogRow) -> OptimizationStatus {
    if !row.has_source_table || row.registered_vectors == 0 {
        return OptimizationStatus::MissingArtifacts;
    }
    if !row.source_table_exists {
        return OptimizationStatus::StaleCatalog;
    }
    if row.hnsw_indexes > 0 {
        OptimizationStatus::Indexed
    } else {
        OptimizationStatus::ExactOnly
    }
}

fn resolve_vacuum_advice(index_name: &str) -> VacuumAdviceRow {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT index_namespace.nspname::text,
                    index_class.relname::text,
                    table_namespace.nspname::text,
                    table_class.relname::text,
                    access_method.amname::text,
                    CASE
                        WHEN index_class.reltuples < 0 THEN NULL
                        ELSE index_class.reltuples::bigint
                    END,
                    index_class.relpages::bigint,
                    pg_catalog.pg_stat_get_dead_tuples(table_class.oid)::bigint
               FROM pg_catalog.pg_class AS index_class
               JOIN pg_catalog.pg_namespace AS index_namespace
                 ON index_namespace.oid = index_class.relnamespace
               JOIN pg_catalog.pg_index AS index_catalog
                 ON index_catalog.indexrelid = index_class.oid
               JOIN pg_catalog.pg_class AS table_class
                 ON table_class.oid = index_catalog.indrelid
               JOIN pg_catalog.pg_namespace AS table_namespace
                 ON table_namespace.oid = table_class.relnamespace
               JOIN pg_catalog.pg_am AS access_method
                 ON access_method.oid = index_class.relam
              WHERE index_class.oid = pg_catalog.to_regclass($1)::oid
                AND index_class.relkind = 'i'",
            Some(1),
            &[index_name.into()],
        )?;

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("index does not exist: {index_name}"),
            );
        }

        let row = rows.first();
        Ok::<_, spi::Error>(VacuumAdviceRow {
            index_schema: required_column(row.get::<String>(1)?, "index_schema"),
            index_name: required_column(row.get::<String>(2)?, "index_name"),
            table_schema: required_column(row.get::<String>(3)?, "table_schema"),
            table_name: required_column(row.get::<String>(4)?, "table_name"),
            access_method: required_column(row.get::<String>(5)?, "access_method"),
            estimated_index_tuples: row.get::<i64>(6)?,
            index_pages: required_column(row.get::<i64>(7)?, "index_pages"),
            dead_table_tuples: required_column(row.get::<i64>(8)?, "dead_table_tuples"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("vacuum advice query failed: {error}"),
        )
    })
}

fn vacuum_advice_status(row: &VacuumAdviceRow) -> (i64, VacuumAdviceStatus) {
    if row.access_method != "pgcontext_hnsw" {
        return (
            row.estimated_index_tuples.unwrap_or_default(),
            VacuumAdviceStatus::UnsupportedAccessMethod,
        );
    }
    let Some(estimated_index_tuples) = row.estimated_index_tuples else {
        return (0, VacuumAdviceStatus::AnalyzeRecommended);
    };
    if row.dead_table_tuples > 0 {
        (
            estimated_index_tuples,
            VacuumAdviceStatus::VacuumRecommended,
        )
    } else {
        (estimated_index_tuples, VacuumAdviceStatus::Healthy)
    }
}

include!("operations/value_helpers.rs");

/// Returns this backend's HNSW packed-generation serving counters.
///
/// The packed cache is backend-local in the current serving model, so the
/// row describes the calling backend only: how many packed generations it
/// built, how many queries reused an existing pack, and what the most
/// recent pack cost in bytes and milliseconds. `delta_segment_records` and
/// `mapped_attaches`, `mapped_publishes`, and `mapped_publish_skips` describe
/// this backend's immutable file-generation activity. `delta_segment_records`
/// and `delta_segment_scans` describe the segmented-write delta region: rows
/// absorbed without a graph splice (including VACUUM tombstones for
/// delta-only rows) and scans that merged delta results with base-graph
/// candidates.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "hnsw_serving_stats")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn hnsw_serving_stats() -> TableIterator<
    'static,
    (
        name!(pack_builds, i64),
        name!(pack_reuses, i64),
        name!(last_pack_bytes, i64),
        name!(last_pack_millis, i64),
        name!(total_pack_millis, i64),
        name!(shared_attaches, i64),
        name!(shared_publishes, i64),
        name!(shared_publish_skips, i64),
        name!(mapped_attaches, i64),
        name!(mapped_publishes, i64),
        name!(mapped_publish_skips, i64),
        name!(page_native_fallbacks, i64),
        name!(delta_segment_records, i64),
        name!(delta_segment_scans, i64),
    ),
> {
    let stats = crate::hnsw_am::hnsw_serving_stats_snapshot();
    let saturate = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    TableIterator::once((
        saturate(stats.pack_builds),
        saturate(stats.pack_reuses),
        saturate(stats.last_pack_bytes),
        saturate(stats.last_pack_millis),
        saturate(stats.total_pack_millis),
        saturate(stats.shared_attaches),
        saturate(stats.shared_publishes),
        saturate(stats.shared_publish_skips),
        saturate(stats.mapped_attaches),
        saturate(stats.mapped_publishes),
        saturate(stats.mapped_publish_skips),
        saturate(stats.page_native_fallbacks),
        saturate(stats.delta_segment_records),
        saturate(stats.delta_segment_scans),
    ))
}

/// Returns the phase timing of this backend's most recent HNSW bulk build.
///
/// `graph_millis` covers the heap scan and in-memory graph construction;
/// `write_millis` covers snapshot extraction, index page writes, and
/// Generic-WAL emission. All zeros means this backend has not built an HNSW
/// index yet.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "hnsw_build_stats")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn hnsw_build_stats() -> TableIterator<
    'static,
    (
        name!(last_build_tuples, i64),
        name!(graph_millis, i64),
        name!(write_millis, i64),
    ),
> {
    let profile = crate::hnsw_am::hnsw_build_profile_snapshot();
    let saturate = |value: u64| i64::try_from(value).unwrap_or(i64::MAX);
    TableIterator::once((
        saturate(profile.tuples),
        saturate(profile.graph_millis),
        saturate(profile.write_millis),
    ))
}
