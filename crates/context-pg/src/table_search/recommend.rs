//! Exact recommendation search over registered table-backed collections.

use std::collections::BTreeSet;

use context_core::DenseVector;
use pgrx::prelude::*;

use crate::error::{raise_core_error, raise_sql_error};
use crate::vector::Vector;

use super::{
    SearchVector, collection_name_from_sql, distance_function, quote_identifier,
    quote_qualified_identifier, require_collection_owner, require_table_select_privilege,
    resolve_collection, resolve_registered_vector, search_limit_from_sql,
    table_search_rows_from_spi, validate_search_drift,
};

#[pg_extern(schema = "pgcontext", name = "recommend")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn recommend_collection_from_points(
    collection: String,
    positive_point_ids: Vec<i64>,
    negative_point_ids: Vec<i64>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let context = recommend_context(collection, limit);
    let positive_point_ids = recommendation_point_ids(positive_point_ids);
    let negative_point_ids = recommendation_point_ids(negative_point_ids);
    let positive_vectors = load_example_vectors(&context, &positive_point_ids);
    let negative_vectors = load_example_vectors(&context, &negative_point_ids);
    let query = recommendation_query_vector(&positive_vectors, &negative_vectors);
    let excluded_point_ids = positive_point_ids
        .iter()
        .chain(negative_point_ids.iter())
        .copied()
        .collect::<BTreeSet<_>>();

    TableIterator::new(search_recommendation_table(
        &context,
        query,
        excluded_point_ids,
    ))
}

#[pg_extern(schema = "pgcontext", name = "recommend")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn recommend_collection_from_vectors(
    collection: String,
    positive_vectors: Vec<Vector>,
    negative_vectors: Vec<Vector>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let context = recommend_context(collection, limit);
    let positive_vectors = dense_vectors_from_sql(positive_vectors);
    let negative_vectors = dense_vectors_from_sql(negative_vectors);
    let query = recommendation_query_vector(&positive_vectors, &negative_vectors);

    TableIterator::new(search_recommendation_table(
        &context,
        query,
        BTreeSet::new(),
    ))
}

#[pg_extern(schema = "pgcontext", name = "discover")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn discover_collection(
    collection: String,
    context_point_ids: Vec<i64>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    discover_or_explore_collection(collection, context_point_ids, limit)
}

#[pg_extern(schema = "pgcontext", name = "explore")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn explore_collection(
    collection: String,
    context_point_ids: Vec<i64>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    discover_or_explore_collection(collection, context_point_ids, limit)
}

#[derive(Debug, Clone)]
struct RecommendContext {
    collection_id: i64,
    registered_vector: SearchVector,
    limit: i64,
}

fn discover_or_explore_collection(
    collection: String,
    context_point_ids: Vec<i64>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let context = recommend_context(collection, limit);
    let context_point_ids = recommendation_point_ids(context_point_ids);
    if context_point_ids.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "discovery search requires at least one context point id",
        );
    }
    let context_vectors = load_example_vectors(&context, &context_point_ids);
    let query = recommendation_query_vector(&context_vectors, &[]);
    let excluded_point_ids = context_point_ids.into_iter().collect::<BTreeSet<_>>();
    TableIterator::new(search_discovery_table(&context, query, excluded_point_ids))
}

fn recommend_context(collection: String, limit: i32) -> RecommendContext {
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
    RecommendContext {
        collection_id: collection.collection_id,
        registered_vector,
        limit: i64::try_from(limit.get()).unwrap_or(i64::MAX),
    }
}

fn recommendation_point_ids(point_ids: Vec<i64>) -> Vec<i64> {
    let mut unique = BTreeSet::new();
    for point_id in point_ids {
        if point_id <= 0 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("recommendation point id must be positive: {point_id}"),
            );
        }
        unique.insert(point_id);
    }
    unique.into_iter().collect()
}

fn dense_vectors_from_sql(vectors: Vec<Vector>) -> Vec<DenseVector> {
    vectors
        .into_iter()
        .map(|vector| match vector.to_dense() {
            Ok(vector) => vector,
            Err(error) => raise_core_error(error),
        })
        .collect()
}

fn load_example_vectors(context: &RecommendContext, point_ids: &[i64]) -> Vec<DenseVector> {
    if point_ids.is_empty() {
        return Vec::new();
    }

    let table_name = quote_qualified_identifier(
        &context.registered_vector.schema_name,
        &context.registered_vector.table_name,
    );
    let vector_column = quote_identifier(&context.registered_vector.vector_column_name);
    let sql = format!(
        "SELECT points.point_id,
                pgcontext.vector_to_real_array(source.{vector_column}) AS vector_values
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL
            AND points.point_id = ANY($2)
          ORDER BY points.point_id"
    );

    Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            Some(i64::try_from(point_ids.len()).unwrap_or(i64::MAX)),
            &[context.collection_id.into(), point_ids.to_vec().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load recommendation example vectors: {error}"),
            ),
        };
        let mut found_point_ids = BTreeSet::new();
        let mut vectors = Vec::new();
        for row in rows {
            let point_id = recommend_iter_column::<i64>(&row, 1, "point_id");
            let values = recommend_iter_column::<Vec<f32>>(&row, 2, "vector_values");
            found_point_ids.insert(point_id);
            vectors.push(match DenseVector::new(values) {
                Ok(vector) => vector,
                Err(error) => raise_core_error(error),
            });
        }
        for point_id in point_ids {
            if !found_point_ids.contains(point_id) {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("recommendation example point is not active or visible: {point_id}"),
                );
            }
        }
        vectors
    })
}

fn recommendation_query_vector(
    positive_vectors: &[DenseVector],
    negative_vectors: &[DenseVector],
) -> Vector {
    if positive_vectors.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "recommendation requires at least one positive example",
        );
    }
    let dimensions = positive_vectors[0].dimension();
    let positive = centroid(positive_vectors, dimensions);
    let negative = if negative_vectors.is_empty() {
        vec![0.0; dimensions]
    } else {
        centroid(negative_vectors, dimensions)
    };
    let values = positive
        .into_iter()
        .zip(negative)
        .map(|(positive, negative)| positive - negative)
        .collect::<Vec<_>>();
    match DenseVector::new(values) {
        Ok(vector) => Vector::from_dense(vector),
        Err(error) => raise_core_error(error),
    }
}

fn centroid(vectors: &[DenseVector], dimensions: usize) -> Vec<f32> {
    let mut sums = vec![0.0; dimensions];
    for vector in vectors {
        if vector.dimension() != dimensions {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "recommendation vector dimensions do not match",
            );
        }
        for (sum, value) in sums.iter_mut().zip(vector.as_slice()) {
            *sum += value;
        }
    }
    let count = recommendation_vector_count(vectors.len());
    sums.into_iter().map(|sum| sum / count).collect()
}

#[allow(
    clippy::cast_precision_loss,
    reason = "recommendation centroids are f32 vectors; counts above exact f32 range are rejected first"
)]
fn recommendation_vector_count(count: usize) -> f32 {
    if count > (1 << f32::MANTISSA_DIGITS) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("too many recommendation vectors to average exactly: {count}"),
        );
    }
    count as f32
}

fn search_recommendation_table(
    context: &RecommendContext,
    query: Vector,
    excluded_point_ids: BTreeSet<i64>,
) -> Vec<(i64, String, f32)> {
    let table_name = quote_qualified_identifier(
        &context.registered_vector.schema_name,
        &context.registered_vector.table_name,
    );
    let vector_column = quote_identifier(&context.registered_vector.vector_column_name);
    let distance_function = distance_function(context.registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            AND NOT (points.point_id = ANY($3))
          ORDER BY score ASC, points.point_id ASC
          LIMIT $4"
    );
    let excluded_point_ids = excluded_point_ids.into_iter().collect::<Vec<_>>();
    Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            Some(context.limit),
            &[
                query.into(),
                context.collection_id.into(),
                excluded_point_ids.into(),
                context.limit.into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to recommendation-search registered table: {error}"),
            ),
        };
        table_search_rows_from_spi(rows, "recommendation search")
    })
}

fn search_discovery_table(
    context: &RecommendContext,
    query: Vector,
    excluded_point_ids: BTreeSet<i64>,
) -> Vec<(i64, String, f32)> {
    let table_name = quote_qualified_identifier(
        &context.registered_vector.schema_name,
        &context.registered_vector.table_name,
    );
    let vector_column = quote_identifier(&context.registered_vector.vector_column_name);
    let distance_function = distance_function(context.registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            AND NOT (points.point_id = ANY($3))
          ORDER BY score DESC, points.point_id ASC
          LIMIT $4"
    );
    let excluded_point_ids = excluded_point_ids.into_iter().collect::<Vec<_>>();
    Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            Some(context.limit),
            &[
                query.into(),
                context.collection_id.into(),
                excluded_point_ids.into(),
                context.limit.into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to discovery-search registered table: {error}"),
            ),
        };
        table_search_rows_from_spi(rows, "discovery search")
    })
}

fn recommend_iter_column<T>(
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
            format!("recommendation column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read recommendation column {column_name}: {error}"),
        ),
    }
}
