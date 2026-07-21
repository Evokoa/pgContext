//! Experimental ANN candidate generation for late-interaction SQL search.

use core::cmp::Ordering;

use context_core::{
    DenseVector, Error as CoreError, QualifiedTableName, SearchLimit, SqlIdentifier,
};
use context_query::MultiVectorAnnStrategyKind;
use pgrx::{pg_sys, prelude::*};

use crate::error::{raise_core_error, raise_sql_error};
use crate::vector::Vector;

use super::late_interaction::{
    late_interaction_ann_candidate_strategy, late_interaction_ann_detail,
    late_interaction_ann_status, late_interaction_ann_strategy_name,
    late_interaction_candidate_stats, late_interaction_rows_from_spi,
    require_late_interaction_collection_owner, require_late_interaction_table_select_privilege,
    resolve_late_interaction_collection, validate_late_interaction_drift,
};
use super::{
    QueryExplainStatus, collection_name_from_sql, policy_to_i64, quote_identifier,
    quote_qualified_identifier, search_limit_from_sql, session_user, spi_iter_required_column,
    spi_optional_column, spi_required_column,
};

#[derive(Debug, Clone)]
pub(super) struct LateInteractionAnnSource {
    table_oid: pg_sys::Oid,
    schema_name: String,
    table_name: String,
    source_key_column: String,
    vector_column: String,
    vector_dimensions: usize,
}

/// Experimental ANN candidate generation for table-backed late interaction.
///
/// The token table supplies approximate candidate source keys using a
/// `pgcontext_hnsw` index over one token vector per row. Final ordering still
/// hydrates the authoritative collection source table and applies exact MaxSim.
#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::too_many_arguments,
    reason = "SQL surface keeps the source and token table contract explicit"
)]
pub fn search_late_interaction_ann(
    collection: String,
    query_vectors: Vec<Vector>,
    vector_column: String,
    token_table: String,
    token_source_key_column: String,
    token_vector_column: String,
    candidates_per_query: i32,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f64),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let mut collection = resolve_late_interaction_collection(&collection_name);
    require_late_interaction_collection_owner(&collection, &collection_name);
    validate_late_interaction_drift(&mut collection, &vector_column);
    require_late_interaction_table_select_privilege(&collection);

    let ann_source = resolve_late_interaction_ann_source(
        &token_table,
        &token_source_key_column,
        &token_vector_column,
    );
    require_late_interaction_ann_table_select_privilege(&ann_source);
    let candidates_per_query = search_limit_from_sql(candidates_per_query);
    let query_vectors = super::late_interaction::dense_vectors_from_sql(
        "late interaction query_vectors",
        query_vectors,
    );
    validate_late_interaction_ann_token_dimensions(&ann_source, &query_vectors);
    let limit = search_limit_from_sql(limit);
    let projected_candidate_count =
        late_interaction_projected_candidate_count(query_vectors.len(), candidates_per_query.get());
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        projected_candidate_count,
    );

    let candidate_stats = late_interaction_candidate_stats(&collection, &vector_column);
    let ann_strategy = late_interaction_ann_candidate_strategy(
        &query_vectors,
        candidate_stats,
        candidates_per_query,
    );
    match ann_strategy.kind() {
        MultiVectorAnnStrategyKind::AnnCandidateServing => {}
        MultiVectorAnnStrategyKind::ExactNoOp => return TableIterator::new(Vec::new()),
        MultiVectorAnnStrategyKind::Rejected => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "late interaction comparison budget exceeded: {} > {}",
                ann_strategy.projected_comparisons(),
                crate::late_interaction::MAX_LATE_INTERACTION_COMPARISONS
            ),
        ),
        MultiVectorAnnStrategyKind::ExactTableScan
        | MultiVectorAnnStrategyKind::PlannedNotServingReady => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            late_interaction_ann_detail(&ann_strategy),
        ),
    }

    let source_keys =
        late_interaction_ann_source_keys(&ann_source, &query_vectors, candidates_per_query);
    let rows = search_late_interaction_candidate_keys(
        &collection,
        &query_vectors,
        &vector_column,
        &source_keys,
        limit,
    );
    TableIterator::new(rows)
}

/// Explains experimental ANN candidate generation for late interaction.
#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
pub fn explain_late_interaction_ann(
    collection: String,
    query_vectors: Vec<Vector>,
    vector_column: String,
    token_table: String,
    token_source_key_column: String,
    token_vector_column: String,
    candidates_per_query: i32,
) -> TableIterator<
    'static,
    (
        name!(stage, String),
        name!(detail, String),
        name!(branch, Option<String>),
        name!(strategy, String),
        name!(status, QueryExplainStatus),
        name!(estimated_candidates, Option<i64>),
        name!(candidate_budget, Option<i64>),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let mut collection = resolve_late_interaction_collection(&collection_name);
    require_late_interaction_collection_owner(&collection, &collection_name);
    validate_late_interaction_drift(&mut collection, &vector_column);
    require_late_interaction_table_select_privilege(&collection);

    let ann_source = resolve_late_interaction_ann_source(
        &token_table,
        &token_source_key_column,
        &token_vector_column,
    );
    require_late_interaction_ann_table_select_privilege(&ann_source);
    let candidates_per_query = search_limit_from_sql(candidates_per_query);
    let query_vectors = super::late_interaction::dense_vectors_from_sql(
        "late interaction query_vectors",
        query_vectors,
    );
    validate_late_interaction_ann_token_dimensions(&ann_source, &query_vectors);
    let projected_candidate_count =
        late_interaction_projected_candidate_count(query_vectors.len(), candidates_per_query.get());
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        projected_candidate_count,
    );
    let candidate_stats = late_interaction_candidate_stats(&collection, &vector_column);
    let ann_strategy = late_interaction_ann_candidate_strategy(
        &query_vectors,
        candidate_stats,
        candidates_per_query,
    );

    TableIterator::new(vec![
        (
            "ann_source".to_owned(),
            format!(
                "token_table={}.{} source_key_column={} token_vector_column={}",
                ann_source.schema_name,
                ann_source.table_name,
                ann_source.source_key_column,
                ann_source.vector_column
            ),
            Some("multi_vector".to_owned()),
            "hnsw_token_candidates".to_owned(),
            QueryExplainStatus::Ready,
            Some(policy_to_i64(
                candidate_stats.vector_count,
                "late_interaction_candidate_vectors",
            )),
            Some(policy_to_i64(
                candidates_per_query.get(),
                "late_interaction_candidates_per_query",
            )),
        ),
        (
            "ann_planner".to_owned(),
            late_interaction_ann_detail(&ann_strategy),
            Some("multi_vector".to_owned()),
            late_interaction_ann_strategy_name(ann_strategy.kind()).to_owned(),
            late_interaction_ann_status(ann_strategy.kind()),
            Some(policy_to_i64(
                ann_strategy.projected_comparisons(),
                "late_interaction_projected_comparisons",
            )),
            Some(policy_to_i64(
                crate::late_interaction::MAX_LATE_INTERACTION_COMPARISONS,
                "max_late_interaction_comparisons",
            )),
        ),
    ])
}

fn late_interaction_projected_candidate_count(
    query_vector_count: usize,
    candidates_per_query: usize,
) -> usize {
    query_vector_count
        .checked_mul(candidates_per_query)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "late interaction candidate budget overflow",
            )
        })
}

fn validate_late_interaction_ann_token_dimensions(
    ann_source: &LateInteractionAnnSource,
    query_vectors: &[DenseVector],
) {
    let Some(first_query_vector) = query_vectors.first() else {
        raise_core_error(CoreError::InvalidVector(
            "late-interaction query vectors must not be empty".to_owned(),
        ));
    };
    let expected_dimensions = first_query_vector.dimension();
    for query_vector in &query_vectors[1..] {
        if query_vector.dimension() != expected_dimensions {
            raise_core_error(CoreError::DimensionMismatch {
                left: expected_dimensions,
                right: query_vector.dimension(),
            });
        }
    }

    if expected_dimensions != ann_source.vector_dimensions {
        raise_core_error(CoreError::DimensionMismatch {
            left: expected_dimensions,
            right: ann_source.vector_dimensions,
        });
    }
}

fn search_late_interaction_candidate_keys(
    collection: &super::late_interaction::LateInteractionCollection,
    query_vectors: &[DenseVector],
    vector_column: &str,
    source_keys: &[String],
    limit: SearchLimit,
) -> Vec<(i64, String, f64)> {
    if source_keys.is_empty() {
        return Vec::new();
    }

    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let vector_column = quote_identifier(vector_column);
    let sql = format!(
        "WITH candidate_keys AS MATERIALIZED (
             SELECT DISTINCT key::text AS source_key
               FROM unnest($2::text[]) AS key
         )
         SELECT points.point_id,
                points.source_key,
                source.{vector_column}
           FROM candidate_keys AS candidates
           JOIN pgcontext._visible_collection_points AS points
             ON points.source_key = candidates.source_key
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL"
    );

    let mut scored_rows = Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            None,
            &[collection.collection_id.into(), source_keys.to_vec().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load late-interaction ANN candidates: {error}"),
            ),
        };
        late_interaction_rows_from_spi(rows, query_vectors)
    });

    scored_rows.sort_by(|left, right| {
        right
            .2
            .partial_cmp(&left.2)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored_rows.truncate(limit.get());
    scored_rows
}

fn late_interaction_ann_source_keys(
    ann_source: &LateInteractionAnnSource,
    query_vectors: &[DenseVector],
    candidates_per_query: SearchLimit,
) -> Vec<String> {
    let table_name = quote_qualified_identifier(&ann_source.schema_name, &ann_source.table_name);
    let source_key_column = quote_identifier(&ann_source.source_key_column);
    let vector_column = quote_identifier(&ann_source.vector_column);
    let sql = format!(
        "SELECT token.{source_key_column}::text
           FROM {table_name} AS token
          ORDER BY token.{vector_column} OPERATOR(pgcontext.<#>) $1
          LIMIT $2"
    );

    let mut source_keys = Vec::<String>::new();
    let candidate_limit = policy_to_i64(
        candidates_per_query.get(),
        "late_interaction_candidates_per_query",
    );
    for query_vector in query_vectors {
        let sql_vector = Vector::from_dense(query_vector.clone());
        Spi::connect(|client| {
            let rows = match client.select(&sql, None, &[sql_vector.into(), candidate_limit.into()])
            {
                Ok(rows) => rows,
                Err(error) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to collect late-interaction ANN candidates: {error}"),
                ),
            };
            for row in rows {
                let source_key =
                    spi_iter_required_column::<String>(&row, 1, "ann_candidate_source_key");
                if !source_keys.contains(&source_key) {
                    source_keys.push(source_key);
                }
            }
            Ok::<_, spi::Error>(())
        })
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to collect late-interaction ANN candidates: {error}"),
            )
        });
    }
    source_keys
}

fn resolve_late_interaction_ann_source(
    token_table: &str,
    source_key_column: &str,
    vector_column: &str,
) -> LateInteractionAnnSource {
    let table_name = qualified_table_name_from_sql(token_table);
    let source_key_column = sql_identifier_from_sql(source_key_column);
    let vector_column = sql_identifier_from_sql(vector_column);
    let schema_name = table_name.schema().as_str().to_owned();
    let relation_name = table_name.table().as_str().to_owned();
    let source_key_column_name = source_key_column.as_str().to_owned();
    let vector_column_name = vector_column.as_str().to_owned();

    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT class.oid,
                    source_attribute.attname IS NOT NULL AS source_key_exists,
                    source_attribute.attnotnull AS source_key_not_null,
                    vector_attribute.attname IS NOT NULL AS vector_exists,
                    vector_attribute.atttypid = 'public.vector'::regtype AS vector_is_valid,
                    vector_attribute.atttypmod AS vector_typmod,
                    vector_attribute.attnotnull AS vector_not_null,
                    EXISTS (
                        SELECT 1
                          FROM pg_catalog.pg_index AS idx
                          JOIN pg_catalog.pg_class AS index_class ON index_class.oid = idx.indexrelid
                          JOIN pg_catalog.pg_am AS am ON am.oid = index_class.relam
                         WHERE idx.indrelid = class.oid
                           AND am.amname = 'pgcontext_hnsw'
                           AND idx.indisvalid
                           AND idx.indisready
                           AND idx.indpred IS NULL
                           AND vector_attribute.attnum = ANY(idx.indkey::int2[])
                    ) AS has_hnsw_index
               FROM pg_catalog.pg_class AS class
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
               LEFT JOIN pg_catalog.pg_attribute AS source_attribute
                 ON source_attribute.attrelid = class.oid
                AND source_attribute.attname = $3
                AND source_attribute.attnum > 0
                AND NOT source_attribute.attisdropped
               LEFT JOIN pg_catalog.pg_attribute AS vector_attribute
                 ON vector_attribute.attrelid = class.oid
                AND vector_attribute.attname = $4
                AND vector_attribute.attnum > 0
                AND NOT vector_attribute.attisdropped
              WHERE namespace.nspname = $1
                AND class.relname = $2
                AND class.relkind IN ('r', 'p')",
            Some(1),
            &[
                schema_name.as_str().into(),
                relation_name.as_str().into(),
                source_key_column_name.as_str().into(),
                vector_column_name.as_str().into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to validate late-interaction ANN token table: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                format!("late-interaction ANN token table does not exist: {token_table}"),
            );
        }

        let row = rows.first();
        if !spi_required_column::<bool>(&row, 2, "ann_source_key_exists") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction ANN source key column does not exist: {token_table}.{source_key_column_name}"
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 3, "ann_source_key_not_null") != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction ANN source key column must be NOT NULL: {token_table}.{source_key_column_name}"
                ),
            );
        }
        if !spi_required_column::<bool>(&row, 4, "ann_vector_exists") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction ANN vector column does not exist: {token_table}.{vector_column_name}"
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 5, "ann_vector_is_valid") != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                format!(
                    "late-interaction ANN vector column must have type vector: {token_table}.{vector_column_name}"
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 7, "ann_vector_not_null") != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction ANN vector column must be NOT NULL: {token_table}.{vector_column_name}"
                ),
            );
        }
        let vector_typmod = spi_optional_column::<i32>(&row, 6, "ann_vector_typmod");
        let vector_dimensions = match vector_typmod.and_then(|value| usize::try_from(value).ok()) {
            Some(dimensions) if dimensions > 0 => dimensions,
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction ANN vector column must declare dimensions with vector(n): {token_table}.{vector_column_name}"
                ),
            ),
        };
        if !spi_required_column::<bool>(&row, 8, "ann_has_hnsw_index") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction ANN token table requires a pgcontext_hnsw index on {token_table}.{vector_column_name}"
                ),
            );
        }

        LateInteractionAnnSource {
            table_oid: spi_required_column::<pg_sys::Oid>(&row, 1, "ann_table_oid"),
            schema_name,
            table_name: relation_name,
            source_key_column: source_key_column_name,
            vector_column: vector_column_name,
            vector_dimensions,
        }
    })
}

fn require_late_interaction_ann_table_select_privilege(ann_source: &LateInteractionAnnSource) {
    let session_user = session_user();
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT')",
        &[session_user.as_str().into(), ann_source.table_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check ANN token table privileges: {error}"),
        )
    })
    .unwrap_or(false);

    if !has_select {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for ANN token table: {}.{}",
                ann_source.schema_name, ann_source.table_name
            ),
        );
    }
}

fn qualified_table_name_from_sql(table_name: &str) -> QualifiedTableName {
    match QualifiedTableName::new(table_name) {
        Ok(table_name) => table_name,
        Err(error) => raise_core_error(error),
    }
}

fn sql_identifier_from_sql(identifier: &str) -> SqlIdentifier {
    match SqlIdentifier::new(identifier) {
        Ok(identifier) => identifier,
        Err(error) => raise_core_error(error),
    }
}
