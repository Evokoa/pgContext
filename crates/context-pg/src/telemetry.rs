//! SQL-facing collection telemetry rollups.

use pgrx::prelude::*;

use crate::error::raise_sql_error;
use crate::pgcontext::TelemetryStatus;

#[derive(Debug, Clone)]
struct TelemetryRow {
    collection_name: String,
    table_schema: Option<String>,
    table_name: Option<String>,
    has_source_table: bool,
    source_table_exists: bool,
    registered_vectors: i64,
    active_points: i64,
    deleted_points: i64,
    filter_fields: i64,
    hnsw_indexes: i64,
}

/// Returns collection-level pgContext telemetry rollups.
///
/// The row shape intentionally uses counters and typed status values so
/// monitoring integrations can diff telemetry over time without parsing text.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "telemetry")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn telemetry() -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(table_schema, Option<String>),
        name!(table_name, Option<String>),
        name!(has_source_table, bool),
        name!(source_table_exists, bool),
        name!(registered_vectors, i64),
        name!(active_points, i64),
        name!(deleted_points, i64),
        name!(filter_fields, i64),
        name!(hnsw_indexes, i64),
        name!(status, TelemetryStatus),
    ),
> {
    let rows = resolve_telemetry_rows()
        .into_iter()
        .map(|row| {
            let status = telemetry_status(&row);
            (
                row.collection_name,
                row.table_schema,
                row.table_name,
                row.has_source_table,
                row.source_table_exists,
                row.registered_vectors,
                row.active_points,
                row.deleted_points,
                row.filter_fields,
                row.hnsw_indexes,
                status,
            )
        })
        .collect::<Vec<_>>();

    TableIterator::new(rows)
}

fn resolve_telemetry_rows() -> Vec<TelemetryRow> {
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
                          FROM pgcontext._collection_points AS points
                         WHERE points.collection_id = collections.collection_id
                           AND points.deleted_at IS NOT NULL
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
              ORDER BY collections.collection_name",
            None,
            &[],
        )?;

        let mut output = Vec::new();
        for row in rows {
            output.push(TelemetryRow {
                collection_name: required_column(row.get::<String>(1)?, "collection_name"),
                table_schema: row.get::<String>(2)?,
                table_name: row.get::<String>(3)?,
                has_source_table: required_column(row.get::<bool>(4)?, "has_source_table"),
                source_table_exists: required_column(row.get::<bool>(5)?, "source_table_exists"),
                registered_vectors: required_column(row.get::<i64>(6)?, "registered_vectors"),
                active_points: required_column(row.get::<i64>(7)?, "active_points"),
                deleted_points: required_column(row.get::<i64>(8)?, "deleted_points"),
                filter_fields: required_column(row.get::<i64>(9)?, "filter_fields"),
                hnsw_indexes: required_column(row.get::<i64>(10)?, "hnsw_indexes"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("telemetry query failed: {error}"),
        )
    })
}

fn telemetry_status(row: &TelemetryRow) -> TelemetryStatus {
    if !row.has_source_table || row.registered_vectors == 0 {
        return TelemetryStatus::MissingArtifacts;
    }
    if !row.source_table_exists {
        return TelemetryStatus::StaleCatalog;
    }
    if row.active_points == 0 {
        TelemetryStatus::Empty
    } else {
        TelemetryStatus::Active
    }
}

fn required_column<T>(value: Option<T>, column_name: &'static str) -> T {
    match value {
        Some(value) => value,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("telemetry catalog column was unexpectedly null: {column_name}"),
        ),
    }
}
