//! SQL-facing exact search over registered table-backed collections.

mod candidate_recheck;
mod grouped;
mod named;
pub(crate) mod recommend;
mod support;

use context_core::{CollectionName, DistanceMetric, ScrollCursor, ScrollCursorError, SearchLimit};
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use serde_json::Value;

use crate::domain_types::distance_metric_from_catalog;
use crate::error::{raise_core_error, raise_sql_error};
use crate::vector::Vector;
pub(crate) use candidate_recheck::{
    load_mmap_artifact_candidates, mmap_delta_candidates, take_last_mmap_candidate_visits,
    take_last_mmap_delta_visits,
};
pub(crate) use named::resolve_registered_vector_by_name;
use support::{FacetTarget, facet_expression, resolve_facet_target, resolve_filter_plan};
pub(crate) use support::{
    FilterField, FilterPredicatePlan, load_filter_fields, push_filter_parameter_args,
    resolve_typed_filter_plan,
};

#[derive(Debug, Clone)]
pub(crate) struct SearchCollection {
    pub(crate) collection_id: i64,
    pub(crate) owner_role: pg_sys::Oid,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchVector {
    pub(crate) schema_name: String,
    pub(crate) table_name: String,
    pub(crate) table_oid: pg_sys::Oid,
    pub(crate) vector_column_name: String,
    pub(crate) vector_attnum: i16,
    pub(crate) hnsw_index_oid: Option<pg_sys::Oid>,
    pub(crate) metric: DistanceMetric,
    pub(crate) quantization_options: Value,
}

#[pg_extern(name = "search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_collection(
    collection: String,
    vector: Vector,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let limit = search_limit_from_sql(limit);
    let query = context_query::QueryIr::nearest(
        None,
        vector.as_slice().to_vec(),
        context_query::ScoreOrder::LowerIsBetter,
        None,
        limit.get(),
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    let rows = crate::retrieval::run_query(
        &collection_name,
        query,
        crate::retrieval::CandidateAdapter::Exact,
    );
    TableIterator::new(rows)
}

#[pg_extern(name = "search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_collection_filtered(
    collection: String,
    vector: Vector,
    filter: Option<String>,
    limit: i32,
) -> TableIterator<
    'static,
    (
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

    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let fields = load_filter_fields(collection.collection_id);
    let filter_offset = usize::from(registered_vector.hnsw_index_oid.is_some()) + 3;
    let filter_plan = resolve_filter_plan(&fields, filter.as_deref(), filter_offset);
    let rows = search_registered_table_filtered(
        collection.collection_id,
        &registered_vector,
        vector,
        filter_plan,
        limit,
    );
    TableIterator::new(rows)
}

#[pg_extern(name = "scroll")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn scroll_collection(
    collection: String,
    cursor: Option<String>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(next_cursor, String),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);

    let after_point_id = decode_scroll_cursor(cursor.as_deref(), collection.collection_id);
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let rows = scroll_registered_points(collection.collection_id, after_point_id, limit);
    TableIterator::new(rows)
}

#[pg_extern(name = "count")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn count_collection(collection: String) -> i64 {
    count_collection_filtered(collection, None)
}

#[pg_extern(name = "count")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn count_collection_filtered(collection: String, filter: Option<String>) -> i64 {
    let collection_name = collection_name_from_sql(collection);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let fields = load_filter_fields(collection.collection_id);
    let filter_plan = resolve_filter_plan(&fields, filter.as_deref(), 1);
    count_registered_table(collection.collection_id, &registered_vector, filter_plan)
}

#[pg_extern(name = "facet")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn facet_collection(
    collection: String,
    field: String,
    filter: Option<String>,
    limit: i32,
) -> TableIterator<'static, (name!(value, String), name!(count, i64))> {
    let collection_name = collection_name_from_sql(collection);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let fields = load_filter_fields(collection.collection_id);
    let facet_target = resolve_facet_target(&fields, &field);
    let filter_plan = resolve_filter_plan(&fields, filter.as_deref(), 2);
    let rows = facet_registered_table(
        collection.collection_id,
        &registered_vector,
        &facet_target,
        filter_plan,
        limit,
    );
    TableIterator::new(rows)
}

pub(crate) fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn decode_scroll_cursor(cursor: Option<&str>, collection_id: i64) -> i64 {
    let Some(cursor) = cursor else {
        return 0;
    };

    match ScrollCursor::decode_for_collection(cursor, collection_id) {
        Ok(cursor) => cursor.after_point_id(),
        Err(error) => raise_scroll_cursor_error(error),
    }
}

fn raise_scroll_cursor_error(error: ScrollCursorError) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        error.to_string(),
    )
}

pub(crate) fn search_limit_from_sql(limit: i32) -> SearchLimit {
    let limit = match usize::try_from(limit) {
        Ok(limit) => limit,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid search limit: {limit}"),
        ),
    };
    match SearchLimit::new(limit) {
        Ok(limit) => limit,
        Err(error) => raise_core_error(error),
    }
}

include!("table_search/catalog_access.rs");

pub(crate) fn search_registered_table(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: Vector,
    limit: SearchLimit,
) -> Vec<(i64, String, f32)> {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = distance_function(registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
          ORDER BY score ASC, points.point_id ASC
          LIMIT $3"
    );

    Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            Some(i64::try_from(limit.get()).unwrap_or(i64::MAX)),
            &[
                query.into(),
                collection_id.into(),
                i64::try_from(limit.get()).unwrap_or(i64::MAX).into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to search registered table: {error}"),
            ),
        };

        table_search_rows_from_spi(rows, "table search")
    })
}

pub(in crate::table_search) fn search_registered_table_filtered(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: Vector,
    filter_plan: Option<FilterPredicatePlan>,
    limit: SearchLimit,
) -> Vec<(i64, String, f32)> {
    let Some(filter_plan) = filter_plan else {
        return search_registered_table(collection_id, registered_vector, query, limit);
    };
    let Some(hnsw_index_oid) = registered_vector.hnsw_index_oid else {
        return search_registered_table_filtered_exact(
            collection_id,
            registered_vector,
            query,
            &filter_plan,
            limit,
        );
    };
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = distance_function(registered_vector.metric);
    let candidate_budget = crate::settings::hnsw_candidate_budget_from_guc();
    let iterative_limit = crate::settings::hnsw_iterative_expansion_limit_from_guc();
    let filter_sql = &filter_plan.sql;
    let exact_threshold =
        adaptive_exact_candidate_threshold(candidate_budget, limit.get(), iterative_limit);
    let matching_candidates = count_filter_candidates_for_strategy(
        collection_id,
        &query,
        hnsw_index_oid,
        &table_name,
        &filter_plan,
        exact_threshold,
    );
    if matching_candidates <= exact_threshold {
        crate::hnsw_am::record_hnsw_exact_scan(matching_candidates);
        return search_registered_table_filtered_exact_with_hnsw_parameters(
            collection_id,
            registered_vector,
            query,
            &filter_plan,
            limit,
            hnsw_index_oid,
        );
    }
    let sql = format!(
        "WITH filter_candidates AS MATERIALIZED (
             SELECT source.ctid AS heap_tid
               FROM {table_name} AS source
              WHERE {filter_sql}
              ORDER BY source.ctid
              LIMIT {iterative_limit}
         ),
         candidate_mask AS MATERIALIZED (
             SELECT array_agg(heap_tid ORDER BY heap_tid) AS heap_tids
               FROM filter_candidates
         ),
         ann_candidates AS MATERIALIZED (
             SELECT ann.heap_tid,
                    ann.score
               FROM candidate_mask
              CROSS JOIN LATERAL pgcontext._hnsw_masked_candidates(
                    $4,
                    $1,
                    candidate_mask.heap_tids,
                    GREATEST({candidate_budget}::int8, $3)::int4
                ) AS ann
         ),
         candidate_points AS MATERIALIZED (
             SELECT points.point_id,
                    points.source_key
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
               JOIN ann_candidates AS ann ON source.ctid::text = ann.heap_tid
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
                AND {filter_sql}
         )
         SELECT candidates.point_id,
                candidates.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score
           FROM candidate_points AS candidates
           JOIN {table_name} AS source ON source.id::text = candidates.source_key
          ORDER BY score ASC, candidates.point_id ASC
          LIMIT $3"
    );
    let limit = i64::try_from(limit.get()).unwrap_or(i64::MAX);
    let parameter_values = filter_plan.parameters.as_slice();
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(4 + parameter_values.len());
    args.push(query.into());
    args.push(collection_id.into());
    args.push(limit.into());
    args.push(hnsw_index_oid.into());
    push_filter_parameter_args(&mut args, parameter_values);

    crate::hnsw_am::with_hnsw_candidate_helper_capability(hnsw_index_oid, || {
        Spi::connect(|client| {
            let rows = match client.select(&sql, Some(limit), &args) {
                Ok(rows) => rows,
                Err(error) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to search filtered registered table: {error}"),
                ),
            };
            table_search_rows_from_spi(rows, "filtered table search")
        })
    })
}

const ADAPTIVE_EXACT_CANDIDATE_MULTIPLIER: usize = 64;

fn adaptive_exact_candidate_threshold(
    candidate_budget: usize,
    requested_limit: usize,
    iterative_limit: usize,
) -> usize {
    candidate_budget
        .saturating_mul(ADAPTIVE_EXACT_CANDIDATE_MULTIPLIER)
        .max(requested_limit)
        .min(iterative_limit)
}

fn count_filter_candidates_for_strategy(
    collection_id: i64,
    query: &Vector,
    hnsw_index_oid: pg_sys::Oid,
    table_name: &str,
    filter_plan: &FilterPredicatePlan,
    exact_threshold: usize,
) -> usize {
    let count_limit = exact_threshold.saturating_add(1);
    let sql = format!(
        "SELECT count(*)
           FROM (
                SELECT 1
                 FROM {table_name} AS source
                 CROSS JOIN (
                       SELECT $1 AS query,
                              $2 AS collection_id,
                              $3 AS requested_limit,
                              $4 AS index_oid
                 ) AS strategy_inputs
                 WHERE {filter_sql}
                 LIMIT {count_limit}
           ) AS bounded_filter_candidates",
        filter_sql = filter_plan.sql,
    );
    let strategy_limit = i64::try_from(count_limit).unwrap_or(i64::MAX);
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(4 + filter_plan.parameters.len());
    args.push(query.clone().into());
    args.push(collection_id.into());
    args.push(strategy_limit.into());
    args.push(hnsw_index_oid.into());
    push_filter_parameter_args(&mut args, &filter_plan.parameters);

    Spi::connect(|client| {
        let rows = client.select(&sql, Some(1), &args).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to estimate filtered search strategy: {error}"),
            )
        });
        let count = rows
            .first()
            .get_one::<i64>()
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to read filtered search strategy count: {error}"),
                )
            })
            .unwrap_or_default();
        usize::try_from(count).unwrap_or(usize::MAX)
    })
}

fn search_registered_table_filtered_exact_with_hnsw_parameters(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: Vector,
    filter_plan: &FilterPredicatePlan,
    limit: SearchLimit,
    hnsw_index_oid: pg_sys::Oid,
) -> Vec<(i64, String, f32)> {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = distance_function(registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            AND $4::oid IS NOT NULL
            AND {filter_sql}
          ORDER BY score ASC, points.point_id ASC
          LIMIT $3",
        filter_sql = filter_plan.sql,
    );
    let limit = i64::try_from(limit.get()).unwrap_or(i64::MAX);
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(4 + filter_plan.parameters.len());
    args.push(query.into());
    args.push(collection_id.into());
    args.push(limit.into());
    args.push(hnsw_index_oid.into());
    push_filter_parameter_args(&mut args, &filter_plan.parameters);

    Spi::connect(|client| {
        let rows = client
            .select(&sql, Some(limit), &args)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to adaptive exact-search filtered registered table: {error}"),
                )
            });
        table_search_rows_from_spi(rows, "adaptive filtered exact table search")
    })
}

fn search_registered_table_filtered_exact(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: Vector,
    filter_plan: &FilterPredicatePlan,
    limit: SearchLimit,
) -> Vec<(i64, String, f32)> {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = distance_function(registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            AND {filter_sql}
          ORDER BY score ASC, points.point_id ASC
          LIMIT $3",
        filter_sql = filter_plan.sql,
    );
    let limit = i64::try_from(limit.get()).unwrap_or(i64::MAX);
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(3 + filter_plan.parameters.len());
    args.push(query.into());
    args.push(collection_id.into());
    args.push(limit.into());
    push_filter_parameter_args(&mut args, &filter_plan.parameters);

    Spi::connect(|client| {
        let rows = match client.select(&sql, Some(limit), &args) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to exact-search filtered registered table: {error}"),
            ),
        };
        table_search_rows_from_spi(rows, "filtered exact table search")
    })
}

pub(super) fn table_search_rows_from_spi(
    rows: spi::SpiTupleTable<'_>,
    context: &'static str,
) -> Vec<(i64, String, f32)> {
    let mut output = Vec::new();
    for row in rows {
        output.push((
            spi_iter_column::<i64>(&row, 1, context),
            spi_iter_column::<String>(&row, 2, context),
            spi_iter_column::<f32>(&row, 3, context),
        ));
    }
    output
}

fn spi_iter_column<T>(row: &spi::SpiHeapTupleData<'_>, index: usize, column_name: &'static str) -> T
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(Some(value)) => value,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("table search column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read table search column {column_name}: {error}"),
        ),
    }
}

fn scroll_registered_points(
    collection_id: i64,
    after_point_id: i64,
    limit: SearchLimit,
) -> Vec<(i64, String, String)> {
    let sql = "SELECT point_id,
                      source_key
                 FROM pgcontext._visible_collection_points
                WHERE collection_id = $1
                  AND deleted_at IS NULL
                  AND point_id > $2
                ORDER BY point_id ASC
                LIMIT $3";
    let limit = i64::try_from(limit.get()).unwrap_or(i64::MAX);

    Spi::connect(|client| {
        let rows = match client.select(
            sql,
            Some(limit),
            &[collection_id.into(), after_point_id.into(), limit.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to scroll registered points: {error}"),
            ),
        };

        let mut output = Vec::new();
        for row in rows {
            let point_id = row
                .get::<i64>(1)
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read scroll point_id: {error}"),
                    )
                })
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        "scroll point_id is null",
                    )
                });
            let source_key = row
                .get::<String>(2)
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read scroll source_key: {error}"),
                    )
                })
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        "scroll source_key is null",
                    )
                });
            output.push((
                point_id,
                source_key,
                ScrollCursor::new(collection_id, point_id).encode(),
            ));
        }
        output
    })
}

fn count_registered_table(
    collection_id: i64,
    registered_vector: &SearchVector,
    filter_plan: Option<FilterPredicatePlan>,
) -> i64 {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let filter_sql = filter_plan
        .as_ref()
        .map(|plan| format!(" AND {}", plan.sql))
        .unwrap_or_default();
    let sql = format!(
        "SELECT count(*)::bigint
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL
            {filter_sql}"
    );
    let parameter_values = filter_plan
        .as_ref()
        .map(|plan| plan.parameters.as_slice())
        .unwrap_or(&[]);
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(1 + parameter_values.len());
    args.push(collection_id.into());
    push_filter_parameter_args(&mut args, parameter_values);

    Spi::connect(|client| {
        let rows = match client.select(&sql, Some(1), &args) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to count registered table: {error}"),
            ),
        };
        let row = rows.first();
        match row.get::<i64>(1) {
            Ok(Some(count)) => count,
            Ok(None) => raise_sql_error(PgSqlErrorCode::ERRCODE_INTERNAL_ERROR, "count is null"),
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to read count: {error}"),
            ),
        }
    })
}

fn facet_registered_table(
    collection_id: i64,
    registered_vector: &SearchVector,
    facet_target: &FacetTarget,
    filter_plan: Option<FilterPredicatePlan>,
    limit: SearchLimit,
) -> Vec<(String, i64)> {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let facet_expression = facet_expression(facet_target);
    let filter_sql = filter_plan
        .as_ref()
        .map(|plan| format!(" AND {}", plan.sql))
        .unwrap_or_default();
    let sql = format!(
        "SELECT {facet_expression} AS value,
                count(*)::bigint AS count
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL
            AND {facet_expression} IS NOT NULL
            {filter_sql}
          GROUP BY value
          ORDER BY count DESC, value ASC
          LIMIT $2"
    );
    let limit = i64::try_from(limit.get()).unwrap_or(i64::MAX);
    let parameter_values = filter_plan
        .as_ref()
        .map(|plan| plan.parameters.as_slice())
        .unwrap_or(&[]);
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(2 + parameter_values.len());
    args.push(collection_id.into());
    args.push(limit.into());
    push_filter_parameter_args(&mut args, parameter_values);

    Spi::connect(|client| {
        let rows = match client.select(&sql, Some(limit), &args) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to facet registered table: {error}"),
            ),
        };

        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<String>(1)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read facet value: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "facet value is null",
                        )
                    }),
                row.get::<i64>(2)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read facet count: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "facet count is null",
                        )
                    }),
            ));
        }
        output
    })
}

pub(crate) fn distance_function(metric: DistanceMetric) -> &'static str {
    match metric {
        DistanceMetric::L2 => "l2_distance",
        DistanceMetric::InnerProduct | DistanceMetric::NegativeInnerProduct => {
            "negative_inner_product"
        }
        DistanceMetric::Cosine => "cosine_distance",
        DistanceMetric::L1 => "l1_distance",
        DistanceMetric::Hamming | DistanceMetric::Jaccard => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            "bit distance metrics cannot score vector collections",
        ),
    }
}

pub(crate) fn require_collection_owner(
    collection: &SearchCollection,
    collection_name: &CollectionName,
) {
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

pub(crate) fn require_table_select_privilege(registered_vector: &SearchVector) {
    let session_user = session_user();
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT')",
        &[
            session_user.as_str().into(),
            registered_vector.table_oid.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check source table privileges: {error}"),
        )
    })
    .unwrap_or(false);

    if !has_select {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for source table: {}.{}",
                registered_vector.schema_name, registered_vector.table_name
            ),
        );
    }
}

pub(crate) fn quote_qualified_identifier(schema_name: &str, table_name: &str) -> String {
    Spi::get_one_with_args::<String>(
        "SELECT pg_catalog.format('%I.%I', $1, $2)",
        &[schema_name.into(), table_name.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to quote table identifier: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "quoted table identifier returned null",
        )
    })
}

pub(crate) fn quote_identifier(identifier: &str) -> String {
    Spi::get_one_with_args::<String>("SELECT pg_catalog.format('%I', $1)", &[identifier.into()])
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to quote column identifier: {error}"),
            )
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "quoted column identifier returned null",
            )
        })
}

fn quote_literal(value: &str) -> String {
    Spi::get_one_with_args::<String>("SELECT pg_catalog.format('%L', $1)", &[value.into()])
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to quote literal: {error}"),
            )
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "quoted literal returned null",
            )
        })
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
            format!("table search column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read table search column {column_name}: {error}"),
        ),
    }
}

fn spi_optional_column<T>(
    row: &spi::SpiTupleTable<'_>,
    index: usize,
    column_name: &'static str,
) -> Option<T>
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(value) => value,
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read table search column {column_name}: {error}"),
        ),
    }
}
