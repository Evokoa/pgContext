//! SQL-facing index advisor diagnostics.

use pgrx::prelude::*;

use crate::error::raise_sql_error;
use crate::pgcontext::IndexAdvisorRecommendation;

#[derive(Debug, Clone)]
struct AdvisorCollection {
    collection_name: String,
    source_schema_name: Option<String>,
    source_table_name: Option<String>,
    source_table_oid: Option<pg_sys::Oid>,
    estimated_rows: Option<i64>,
    hnsw_indexes: i64,
}

#[derive(Debug, Clone)]
struct AdvisorFilter {
    filter_key: String,
    column_name: String,
    column_attnum: i16,
    jsonb_path: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct AdvisorRow {
    filter_key: Option<String>,
    column_name: Option<String>,
    recommendation: IndexAdvisorRecommendation,
    detail: String,
    suggested_sql: Option<String>,
}

/// Reports missing-index and planner-advice diagnostics for a collection.
///
/// The advisor inspects registered filter fields and PostgreSQL catalog state.
/// It never creates indexes; it returns typed recommendations and suggested SQL
/// for operators to review.
///
/// # Errors
///
/// Raises `undefined_object` when `collection` does not resolve to a pgContext
/// collection.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "index_advisor")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn index_advisor(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(filter_key, Option<String>),
        name!(column_name, Option<String>),
        name!(recommendation, IndexAdvisorRecommendation),
        name!(detail, String),
        name!(suggested_sql, Option<String>),
    ),
> {
    let collection = resolve_advisor_collection(&collection);
    let rows = advisor_rows(&collection);

    TableIterator::new(
        rows.into_iter()
            .map(move |row| {
                (
                    collection.collection_name.clone(),
                    row.filter_key,
                    row.column_name,
                    row.recommendation,
                    row.detail,
                    row.suggested_sql,
                )
            })
            .collect::<Vec<_>>(),
    )
}

fn advisor_rows(collection: &AdvisorCollection) -> Vec<AdvisorRow> {
    let Some(source_table_oid) = collection.source_table_oid else {
        return vec![AdvisorRow {
            filter_key: None,
            column_name: None,
            recommendation: IndexAdvisorRecommendation::NoAction,
            detail: "collection has no source table; no filter indexes can be advised".to_owned(),
            suggested_sql: None,
        }];
    };
    let Some(schema_name) = &collection.source_schema_name else {
        return Vec::new();
    };
    let Some(table_name) = &collection.source_table_name else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    if collection.estimated_rows.is_none() {
        rows.push(AdvisorRow {
            filter_key: None,
            column_name: None,
            recommendation: IndexAdvisorRecommendation::AnalyzeTable,
            detail: "source-table statistics are unavailable".to_owned(),
            suggested_sql: Some(format!(
                "ANALYZE {}.{}",
                quote_identifier(schema_name),
                quote_identifier(table_name)
            )),
        });
    }

    for filter in resolve_advisor_filters(&collection.collection_name) {
        rows.push(filter_advisor_row(
            source_table_oid,
            schema_name,
            table_name,
            &filter,
        ));
    }

    if collection.hnsw_indexes == 0 {
        rows.push(AdvisorRow {
            filter_key: None,
            column_name: None,
            recommendation: IndexAdvisorRecommendation::TuneHnswSettings,
            detail:
                "collection has no pgcontext_hnsw index; filtered ANN will remain exact or fallback"
                    .to_owned(),
            suggested_sql: None,
        });
    }

    if rows.is_empty() {
        rows.push(AdvisorRow {
            filter_key: None,
            column_name: None,
            recommendation: IndexAdvisorRecommendation::NoAction,
            detail: "no missing filter indexes or stale statistics were detected".to_owned(),
            suggested_sql: None,
        });
    }

    rows
}

fn filter_advisor_row(
    source_table_oid: pg_sys::Oid,
    schema_name: &str,
    table_name: &str,
    filter: &AdvisorFilter,
) -> AdvisorRow {
    let (access_method, recommendation) = if filter.jsonb_path.is_some() {
        ("gin", IndexAdvisorRecommendation::CreateGinIndex)
    } else {
        ("btree", IndexAdvisorRecommendation::CreateBtreeIndex)
    };

    if has_index_on_column(source_table_oid, filter.column_attnum, access_method) {
        return AdvisorRow {
            filter_key: Some(filter.filter_key.clone()),
            column_name: Some(filter.column_name.clone()),
            recommendation: IndexAdvisorRecommendation::NoAction,
            detail: format!("{access_method} index already covers registered filter"),
            suggested_sql: None,
        };
    }

    let index_name = format!(
        "{}_{}_{}_idx",
        table_name, filter.column_name, access_method
    );
    AdvisorRow {
        filter_key: Some(filter.filter_key.clone()),
        column_name: Some(filter.column_name.clone()),
        recommendation,
        detail: format!("registered filter lacks a {access_method} index"),
        suggested_sql: Some(format!(
            "CREATE INDEX {} ON {}.{} USING {} ({})",
            quote_identifier(&index_name),
            quote_identifier(schema_name),
            quote_identifier(table_name),
            access_method,
            quote_identifier(&filter.column_name),
        )),
    }
}

fn resolve_advisor_collection(collection: &str) -> AdvisorCollection {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collections.collection_name,
                    collections.source_schema_name,
                    collections.source_table_name,
                    collections.source_table_oid,
                    CASE
                        WHEN source_class.reltuples < 0 THEN NULL
                        ELSE source_class.reltuples::bigint
                    END,
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
        Ok::<_, spi::Error>(AdvisorCollection {
            collection_name: required_column(row.get::<String>(1)?, "collection_name"),
            source_schema_name: row.get::<String>(2)?,
            source_table_name: row.get::<String>(3)?,
            source_table_oid: row.get::<pg_sys::Oid>(4)?,
            estimated_rows: row.get::<i64>(5)?,
            hnsw_indexes: required_column(row.get::<i64>(6)?, "hnsw_indexes"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("index advisor collection query failed: {error}"),
        )
    })
}

fn resolve_advisor_filters(collection: &str) -> Vec<AdvisorFilter> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT payload.filter_key,
                    payload.column_name,
                    payload.column_attnum,
                    payload.jsonb_path
               FROM pgcontext._collection_payload_columns AS payload
               JOIN pgcontext._collections AS collections USING (collection_id)
              WHERE collections.collection_name = $1
              ORDER BY payload.filter_key",
            None,
            &[collection.into()],
        )?;

        let mut filters = Vec::new();
        for row in rows {
            filters.push(AdvisorFilter {
                filter_key: required_column(row.get::<String>(1)?, "filter_key"),
                column_name: required_column(row.get::<String>(2)?, "column_name"),
                column_attnum: required_column(row.get::<i16>(3)?, "column_attnum"),
                jsonb_path: row.get::<Vec<String>>(4)?,
            });
        }
        Ok::<_, spi::Error>(filters)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("index advisor filter query failed: {error}"),
        )
    })
}

fn has_index_on_column(
    source_table_oid: pg_sys::Oid,
    column_attnum: i16,
    access_method: &str,
) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_index AS index_catalog
               JOIN pg_catalog.pg_class AS index_class
                 ON index_class.oid = index_catalog.indexrelid
               JOIN pg_catalog.pg_am AS am
                 ON am.oid = index_class.relam
              WHERE index_catalog.indrelid = $1
                AND am.amname = $2
                AND EXISTS (
                    SELECT 1
                      FROM pg_catalog.unnest(index_catalog.indkey) AS key_attnum
                     WHERE key_attnum = $3
                )
         )",
        &[
            source_table_oid.into(),
            access_method.into(),
            column_attnum.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("index advisor index lookup failed: {error}"),
        )
    })
    .unwrap_or(false)
}

fn required_column<T>(value: Option<T>, column_name: &str) -> T {
    value.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("index advisor column is null: {column_name}"),
        )
    })
}

fn quote_identifier(identifier: &str) -> String {
    Spi::get_one_with_args::<String>("SELECT pg_catalog.format('%I', $1)", &[identifier.into()])
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to quote SQL identifier: {error}"),
            )
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "failed to quote SQL identifier",
            )
        })
}
