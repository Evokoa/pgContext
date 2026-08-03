//! Exact recommendation search over registered table-backed collections.

use std::mem::size_of;

use context_core::DenseVector;
use pgrx::prelude::*;

use crate::error::{raise_core_error, raise_sql_error};
use crate::vector::Vector;

use super::{
    SearchVector, collection_name_from_sql, distance_function, quote_identifier,
    quote_qualified_identifier, require_collection_owner, require_table_select_privilege,
    resolve_collection, resolve_registered_vector, search_limit_from_sql, validate_search_drift,
};

#[pg_extern(name = "recommend")]
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
    TableIterator::new(
        recommend_collection_from_points_scored(
            collection,
            positive_point_ids,
            negative_point_ids,
            limit,
        )
        .rows,
    )
}

pub(crate) fn recommend_collection_from_points_scored(
    collection: String,
    positive_point_ids: Vec<i64>,
    negative_point_ids: Vec<i64>,
    limit: i32,
) -> ScoredRecommendationRows {
    let (context, query, excluded_point_ids) =
        prepare_point_recommendation(collection, positive_point_ids, negative_point_ids, limit);

    search_recommendation_table(&context, query, excluded_point_ids)
}

/// Runs a recommendation scan only when the statement-local visible candidate
/// set fits within `maximum_comparisons`.
///
/// Unlike a separate count followed by a scoring query, admission and scoring
/// use the same materialized visible set. This matters when row-security policy
/// expressions are volatile: the rows admitted are exactly the rows scored.
pub(crate) fn recommend_collection_from_points_scored_within_budget(
    collection: String,
    positive_point_ids: Vec<i64>,
    negative_point_ids: Vec<i64>,
    limit: i32,
    maximum_comparisons: usize,
    maximum_memory_bytes: usize,
) -> Result<ScoredRecommendationRows, RecommendationBudgetExceeded> {
    let example_count = positive_point_ids
        .len()
        .checked_add(negative_point_ids.len())
        .ok_or(RecommendationBudgetExceeded {
            budget: "recommendation_preparation_memory",
            actual: usize::MAX,
            maximum: maximum_memory_bytes,
        })?;
    require_recommendation_preparation_memory(example_count, maximum_memory_bytes)?;
    let (context, query, excluded_point_ids) =
        prepare_point_recommendation(collection, positive_point_ids, negative_point_ids, limit);

    search_recommendation_table_within_budget(
        &context,
        query,
        excluded_point_ids,
        maximum_comparisons,
    )
}

fn prepare_point_recommendation(
    collection: String,
    positive_point_ids: Vec<i64>,
    negative_point_ids: Vec<i64>,
    limit: i32,
) -> (RecommendContext, Vector, Vec<i64>) {
    let context = recommend_context(collection, limit);
    let positive_point_ids = recommendation_point_ids(positive_point_ids);
    let negative_point_ids = recommendation_point_ids(negative_point_ids);
    let positive_vectors = load_example_vectors(&context, &positive_point_ids);
    let negative_vectors = load_example_vectors(&context, &negative_point_ids);
    let query = recommendation_query_vector(&positive_vectors, &negative_vectors);
    let mut excluded_point_ids = positive_point_ids;
    excluded_point_ids.reserve_exact(negative_point_ids.len());
    excluded_point_ids.extend(negative_point_ids);
    excluded_point_ids.sort_unstable();
    excluded_point_ids.dedup();
    (context, query, excluded_point_ids)
}

#[pg_extern(name = "recommend")]
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

    TableIterator::new(search_recommendation_table(&context, query, Vec::new()).rows)
}

#[pg_extern(name = "discover")]
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
    TableIterator::new(
        discover_or_explore_collection_scored(collection, context_point_ids, limit).rows,
    )
}

#[pg_extern(name = "explore")]
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
    TableIterator::new(
        discover_or_explore_collection_scored(collection, context_point_ids, limit).rows,
    )
}

#[derive(Debug, Clone)]
struct RecommendContext {
    collection_id: i64,
    registered_vector: SearchVector,
    limit: i64,
}

pub(crate) struct ScoredRecommendationRows {
    pub(crate) rows: Vec<(i64, String, f32)>,
    pub(crate) scored_count: usize,
}

/// A bounded recommendation/discovery operation exceeded one hard resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecommendationBudgetExceeded {
    pub(crate) budget: &'static str,
    pub(crate) actual: usize,
    pub(crate) maximum: usize,
}

fn recommendation_preparation_memory_bytes(example_count: usize) -> Option<usize> {
    let point_id_bytes = example_count
        .checked_mul(size_of::<i64>())?
        // The input IDs, an SPI-array copy, and the combined exclusion list
        // can overlap. Sorting and deduplication otherwise happen in place.
        .checked_mul(3)?;
    let vector_headers = example_count.checked_mul(size_of::<DenseVector>())?;
    let vector_values = example_count
        .checked_mul(context_core::policy::MAX_VECTOR_DIMENSIONS)?
        .checked_mul(size_of::<f32>())?;
    let centroid_work = context_core::policy::MAX_VECTOR_DIMENSIONS
        .checked_mul(size_of::<f32>())?
        // Positive centroid, negative/zero centroid, and result vector can
        // coexist while the subtraction iterator is being collected.
        .checked_mul(3)?;

    point_id_bytes
        .checked_add(vector_headers)?
        .checked_add(vector_values)?
        .checked_add(centroid_work)
}

pub(crate) fn require_recommendation_preparation_memory(
    example_count: usize,
    maximum_memory_bytes: usize,
) -> Result<(), RecommendationBudgetExceeded> {
    let actual = recommendation_preparation_memory_bytes(example_count).unwrap_or(usize::MAX);
    if actual > maximum_memory_bytes {
        return Err(RecommendationBudgetExceeded {
            budget: "recommendation_preparation_memory",
            actual,
            maximum: maximum_memory_bytes,
        });
    }
    Ok(())
}

pub(crate) fn discover_or_explore_collection_scored(
    collection: String,
    context_point_ids: Vec<i64>,
    limit: i32,
) -> ScoredRecommendationRows {
    let (context, query, excluded_point_ids) =
        prepare_discovery(collection, context_point_ids, limit);
    search_discovery_table(&context, query, excluded_point_ids)
}

/// Runs a discovery scan with statement-local visible-set admission.
pub(crate) fn discover_or_explore_collection_scored_within_budget(
    collection: String,
    context_point_ids: Vec<i64>,
    limit: i32,
    maximum_comparisons: usize,
    maximum_memory_bytes: usize,
) -> Result<ScoredRecommendationRows, RecommendationBudgetExceeded> {
    require_recommendation_preparation_memory(context_point_ids.len(), maximum_memory_bytes)?;
    let (context, query, excluded_point_ids) =
        prepare_discovery(collection, context_point_ids, limit);
    search_discovery_table_within_budget(&context, query, excluded_point_ids, maximum_comparisons)
}

fn prepare_discovery(
    collection: String,
    context_point_ids: Vec<i64>,
    limit: i32,
) -> (RecommendContext, Vector, Vec<i64>) {
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
    let excluded_point_ids = context_point_ids;
    (context, query, excluded_point_ids)
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
    for point_id in &point_ids {
        if *point_id <= 0 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("recommendation point id must be positive: {point_id}"),
            );
        }
    }
    let mut point_ids = point_ids;
    point_ids.sort_unstable();
    point_ids.dedup();
    point_ids
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
        let mut expected_point_ids = point_ids.iter().copied();
        let mut vectors = Vec::with_capacity(point_ids.len());
        for row in rows {
            let point_id = recommend_iter_column::<i64>(&row, 1, "point_id");
            let Some(expected_point_id) = expected_point_ids.next() else {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "recommendation example query returned an unexpected extra row",
                );
            };
            if point_id != expected_point_id {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!(
                        "recommendation example point is not active or visible: {expected_point_id}"
                    ),
                );
            }
            let values = recommend_iter_column::<Vec<f32>>(&row, 2, "vector_values");
            vectors.push(match DenseVector::new(values) {
                Ok(vector) => vector,
                Err(error) => raise_core_error(error),
            });
        }
        if let Some(point_id) = expected_point_ids.next() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("recommendation example point is not active or visible: {point_id}"),
            );
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
    let mut values = Vec::with_capacity(dimensions);
    values.extend(
        positive
            .into_iter()
            .zip(negative)
            .map(|(positive, negative)| positive - negative),
    );
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
    excluded_point_ids: Vec<i64>,
) -> ScoredRecommendationRows {
    let table_name = quote_qualified_identifier(
        &context.registered_vector.schema_name,
        &context.registered_vector.table_name,
    );
    let vector_column = quote_identifier(&context.registered_vector.vector_column_name);
    let distance_function = distance_function(context.registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score,
                count(*) OVER ()::bigint AS scored_count
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            AND NOT (points.point_id = ANY($3))
          ORDER BY score ASC, points.point_id ASC
          LIMIT $4"
    );
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
        scored_recommendation_rows(rows, "recommendation search")
    })
}

fn search_discovery_table(
    context: &RecommendContext,
    query: Vector,
    excluded_point_ids: Vec<i64>,
) -> ScoredRecommendationRows {
    let table_name = quote_qualified_identifier(
        &context.registered_vector.schema_name,
        &context.registered_vector.table_name,
    );
    let vector_column = quote_identifier(&context.registered_vector.vector_column_name);
    let distance_function = distance_function(context.registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score,
                count(*) OVER ()::bigint AS scored_count
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            AND NOT (points.point_id = ANY($3))
          ORDER BY score DESC, points.point_id ASC
          LIMIT $4"
    );
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
        scored_recommendation_rows(rows, "discovery search")
    })
}

fn search_recommendation_table_within_budget(
    context: &RecommendContext,
    query: Vector,
    excluded_point_ids: Vec<i64>,
    maximum_comparisons: usize,
) -> Result<ScoredRecommendationRows, RecommendationBudgetExceeded> {
    search_table_within_budget(
        context,
        query,
        excluded_point_ids,
        maximum_comparisons,
        ScoreOrder::Ascending,
        "recommendation search",
    )
}

fn search_discovery_table_within_budget(
    context: &RecommendContext,
    query: Vector,
    excluded_point_ids: Vec<i64>,
    maximum_comparisons: usize,
) -> Result<ScoredRecommendationRows, RecommendationBudgetExceeded> {
    search_table_within_budget(
        context,
        query,
        excluded_point_ids,
        maximum_comparisons,
        ScoreOrder::Descending,
        "discovery search",
    )
}

#[derive(Debug, Clone, Copy)]
enum ScoreOrder {
    Ascending,
    Descending,
}

impl ScoreOrder {
    const fn sql(self) -> &'static str {
        match self {
            Self::Ascending => "ASC",
            Self::Descending => "DESC",
        }
    }
}

fn search_table_within_budget(
    context: &RecommendContext,
    query: Vector,
    excluded_point_ids: Vec<i64>,
    maximum_comparisons: usize,
    score_order: ScoreOrder,
    operation: &'static str,
) -> Result<ScoredRecommendationRows, RecommendationBudgetExceeded> {
    let table_name = quote_qualified_identifier(
        &context.registered_vector.schema_name,
        &context.registered_vector.table_name,
    );
    let vector_column = quote_identifier(&context.registered_vector.vector_column_name);
    let distance_function = distance_function(context.registered_vector.metric);
    let (comparison_limit, visible_limit) = bounded_comparison_sql_limits(maximum_comparisons);
    let score_order = score_order.sql();
    let max_source_key_bytes = context_core::policy::MAX_SOURCE_KEY_BYTES;

    // `visible` is both the admission set and the scoring set. The CASE in
    // `scored` is intentional: even if PostgreSQL chooses to materialize that
    // CTE before applying the lateral one-time filter, an over-budget statement
    // cannot invoke the distance function.
    let sql = format!(
        "WITH visible AS MATERIALIZED (
             SELECT points.point_id,
                    points.source_key,
                    source.{vector_column} AS vector_value
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
                AND NOT (points.point_id = ANY($3))
              LIMIT $5
         ),
         admission AS MATERIALIZED (
             SELECT count(*)::bigint AS scored_count
               FROM visible
         ),
         scored AS MATERIALIZED (
             SELECT visible.point_id,
                    CASE WHEN octet_length(visible.source_key) <= {max_source_key_bytes}
                         THEN visible.source_key
                         ELSE NULL
                    END AS source_key,
                    CASE WHEN admission.scored_count <= $4
                         THEN pgcontext.{distance_function}(visible.vector_value, $1)
                         ELSE NULL
                    END AS score
               FROM visible
              CROSS JOIN admission
         )
         SELECT ranked.point_id,
                ranked.source_key,
                ranked.score,
                admission.scored_count
           FROM admission
           LEFT JOIN LATERAL (
                SELECT scored.point_id, scored.source_key, scored.score
                  FROM scored
                 WHERE admission.scored_count <= $4
                 ORDER BY scored.score {score_order}, scored.point_id ASC
                 LIMIT $6
           ) AS ranked ON true"
    );
    Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            Some(context.limit),
            &[
                query.into(),
                context.collection_id.into(),
                excluded_point_ids.into(),
                comparison_limit.into(),
                visible_limit.into(),
                context.limit.into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to {operation} registered table: {error}"),
            ),
        };
        bounded_scored_recommendation_rows(rows, operation, maximum_comparisons)
    })
}

fn bounded_comparison_sql_limits(maximum_comparisons: usize) -> (i64, i64) {
    let comparison_limit = i64::try_from(maximum_comparisons).unwrap_or(i64::MAX);
    (comparison_limit, comparison_limit.saturating_add(1))
}

fn bounded_scored_recommendation_rows(
    rows: spi::SpiTupleTable<'_>,
    context: &'static str,
    maximum_comparisons: usize,
) -> Result<ScoredRecommendationRows, RecommendationBudgetExceeded> {
    let mut output = Vec::new();
    let mut scored_count = 0;
    for row in rows {
        let row_count = recommend_iter_column::<i64>(&row, 4, "scored_count");
        scored_count = usize::try_from(row_count).unwrap_or(usize::MAX);
        if scored_count > maximum_comparisons {
            return Err(RecommendationBudgetExceeded {
                budget: "candidate_comparisons",
                actual: scored_count,
                maximum: maximum_comparisons,
            });
        }
        let Some(point_id) = recommend_iter_optional_column::<i64>(&row, 1, "point_id") else {
            continue;
        };
        output.push((
            point_id,
            recommend_iter_column::<String>(&row, 2, "source_key"),
            recommend_iter_column::<f32>(&row, 3, context),
        ));
    }
    Ok(ScoredRecommendationRows {
        rows: output,
        scored_count,
    })
}

fn scored_recommendation_rows(
    rows: spi::SpiTupleTable<'_>,
    context: &'static str,
) -> ScoredRecommendationRows {
    let mut output = Vec::new();
    let mut scored_count = 0;
    for row in rows {
        let row_count = recommend_iter_column::<i64>(&row, 4, "scored_count");
        scored_count = usize::try_from(row_count).unwrap_or(usize::MAX);
        output.push((
            recommend_iter_column::<i64>(&row, 1, "point_id"),
            recommend_iter_column::<String>(&row, 2, "source_key"),
            recommend_iter_column::<f32>(&row, 3, context),
        ));
    }
    ScoredRecommendationRows {
        rows: output,
        scored_count,
    }
}

fn recommend_iter_optional_column<T>(
    row: &spi::SpiHeapTupleData<'_>,
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
            format!("failed to read recommendation column {column_name}: {error}"),
        ),
    }
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

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::{
        RecommendationBudgetExceeded, ScoreOrder, bounded_comparison_sql_limits,
        recommendation_preparation_memory_bytes, require_recommendation_preparation_memory,
    };

    #[test]
    fn bounded_scan_probes_one_row_past_the_exact_budget() {
        assert_eq!(bounded_comparison_sql_limits(0), (0, 1));
        assert_eq!(bounded_comparison_sql_limits(10), (10, 11));
    }

    #[test]
    fn bounded_scan_limits_saturate_for_unrepresentable_usize_budgets() {
        assert_eq!(
            bounded_comparison_sql_limits(usize::MAX),
            (i64::MAX, i64::MAX)
        );
    }

    #[test]
    fn recommendation_and_discovery_keep_opposite_rank_order() {
        assert_eq!(ScoreOrder::Ascending.sql(), "ASC");
        assert_eq!(ScoreOrder::Descending.sql(), "DESC");
    }

    #[test]
    fn recommendation_preparation_rejects_tiny_memory_before_materialization() {
        let projected = recommendation_preparation_memory_bytes(1)
            .expect("one maximum-dimension example has a representable projection");

        assert_eq!(
            require_recommendation_preparation_memory(1, projected - 1),
            Err(RecommendationBudgetExceeded {
                budget: "recommendation_preparation_memory",
                actual: projected,
                maximum: projected - 1,
            })
        );
        assert_eq!(
            require_recommendation_preparation_memory(1, projected),
            Ok(())
        );
    }

    #[test]
    fn maximum_recommendation_shape_has_a_checked_memory_projection() {
        let maximum_examples = context_core::policy::MAX_RECALL_CHECK_POINT_IDS;
        let projected = recommendation_preparation_memory_bytes(maximum_examples)
            .expect("policy maximum recommendation shape must not overflow usize");
        let coordinate_bytes = maximum_examples
            .checked_mul(context_core::policy::MAX_VECTOR_DIMENSIONS)
            .and_then(|cells| cells.checked_mul(size_of::<f32>()))
            .expect("policy maximum coordinate storage must be representable");

        assert!(projected >= coordinate_bytes);
    }

    #[test]
    fn unrepresentable_recommendation_shape_fails_closed() {
        assert_eq!(recommendation_preparation_memory_bytes(usize::MAX), None);
        assert_eq!(
            require_recommendation_preparation_memory(usize::MAX, usize::MAX - 1),
            Err(RecommendationBudgetExceeded {
                budget: "recommendation_preparation_memory",
                actual: usize::MAX,
                maximum: usize::MAX - 1,
            })
        );
    }
}
