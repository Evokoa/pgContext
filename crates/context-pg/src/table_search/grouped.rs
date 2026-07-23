//! Grouped exact search over registered table-backed collections.

use context_core::SearchLimit;
use pgrx::prelude::*;

use crate::error::raise_sql_error;
use crate::vector::Vector;

use super::named::{resolve_registered_vector_by_name, vector_name_from_sql};
use super::support::{facet_expression, load_filter_fields, resolve_facet_target};
use super::{
    SearchVector, collection_name_from_sql, distance_function, quote_identifier,
    quote_qualified_identifier, require_collection_owner, require_table_select_privilege,
    resolve_collection, resolve_registered_vector, search_limit_from_sql, validate_search_drift,
};

#[pg_extern(name = "grouped_search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn grouped_search_collection(
    collection: String,
    vector: Vector,
    group_by: String,
    group_limit: i32,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(group_value, String),
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let group_limit = search_limit_from_sql(group_limit);
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        group_limit.get(),
    );
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );

    let fields = load_filter_fields(collection.collection_id);
    let group_target = resolve_facet_target(&fields, &group_by);
    let rows = grouped_search_registered_table(
        collection.collection_id,
        &registered_vector,
        vector,
        facet_expression(&group_target),
        group_limit,
        limit,
    );
    TableIterator::new(rows)
}

#[pg_extern(name = "grouped_search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn grouped_search_collection_named_vector(
    collection: String,
    vector_name: String,
    vector: Vector,
    group_by: String,
    group_limit: i32,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(group_value, String),
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let vector_name = vector_name_from_sql(vector_name);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector_by_name(&collection_name, collection.collection_id, &vector_name);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let group_limit = search_limit_from_sql(group_limit);
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        group_limit.get(),
    );
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );

    let fields = load_filter_fields(collection.collection_id);
    let group_target = resolve_facet_target(&fields, &group_by);
    let rows = grouped_search_registered_table(
        collection.collection_id,
        &registered_vector,
        vector,
        facet_expression(&group_target),
        group_limit,
        limit,
    );
    TableIterator::new(rows)
}

fn grouped_search_registered_table(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: Vector,
    group_expression: String,
    group_limit: SearchLimit,
    limit: SearchLimit,
) -> Vec<(String, i64, String, f32)> {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = distance_function(registered_vector.metric);
    let sql = format!(
        "WITH scored AS MATERIALIZED (
             SELECT {group_expression} AS group_value,
                    points.point_id,
                    points.source_key,
                    pgcontext.{distance_function}(source.{vector_column}, $1) AS score
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
                AND {group_expression} IS NOT NULL
         ),
         ranked AS (
             SELECT group_value,
                    point_id,
                    source_key,
                    score,
                    row_number() OVER (
                        PARTITION BY group_value
                        ORDER BY score ASC, point_id ASC
                    ) AS group_rank
               FROM scored
         )
         SELECT group_value,
                point_id,
                source_key,
                score
           FROM ranked
          WHERE group_rank <= $3
          ORDER BY score ASC, point_id ASC
          LIMIT $4"
    );
    let group_limit = i64::try_from(group_limit.get()).unwrap_or(i64::MAX);
    let limit = i64::try_from(limit.get()).unwrap_or(i64::MAX);
    let args = [
        query.into(),
        collection_id.into(),
        group_limit.into(),
        limit.into(),
    ];

    Spi::connect(|client| {
        let rows = match client.select(&sql, Some(limit), &args) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to grouped-search registered table: {error}"),
            ),
        };
        grouped_search_rows_from_spi(rows)
    })
}

fn grouped_search_rows_from_spi(rows: spi::SpiTupleTable<'_>) -> Vec<(String, i64, String, f32)> {
    let mut output = Vec::new();
    for row in rows {
        output.push((
            grouped_search_column::<String>(&row, 1, "group_value"),
            grouped_search_column::<i64>(&row, 2, "point_id"),
            grouped_search_column::<String>(&row, 3, "source_key"),
            grouped_search_column::<f32>(&row, 4, "score"),
        ));
    }
    output
}

fn grouped_search_column<T>(
    row: &spi::SpiHeapTupleData<'_>,
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
            format!("grouped search column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read grouped search column {column_name}: {error}"),
        ),
    }
}
