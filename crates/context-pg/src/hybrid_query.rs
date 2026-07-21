//! SQL-facing hybrid retrieval over registered table-backed collections.

use context_core::{CollectionName, DistanceMetric, SearchLimit};
use context_hybrid::{CandidateBranch, RrfK, reciprocal_rank_fusion_batches};
use pgrx::prelude::*;

use crate::domain_types::distance_metric_label;
use crate::error::{raise_core_error, raise_sql_error};
use crate::pgcontext::QueryExplainStatus;
use crate::vector::Vector;
use crate::vector_variants::SparseVec;

mod candidate_hydration;
mod catalog;
mod late_interaction;
mod late_interaction_ann;
use candidate_hydration::{
    HydratedBranch as QueryBranch, HydratedCandidate, hydrate_dense_exact_candidates,
    hydrate_full_text_candidates, hydrate_sparse_planned_candidates,
};
use catalog::{
    point_id_to_sql, require_collection_owner, require_sparse_table_select_privilege,
    require_table_select_privilege, resolve_collection, resolve_registered_sparse_vector,
    resolve_registered_vector, validate_query_drift, validate_query_vector_drift,
    validate_sparse_query_drift,
};

#[derive(Debug, Clone)]
struct QueryCollection {
    collection_id: i64,
    owner_role: pg_sys::Oid,
    active_points: i64,
}

#[derive(Debug, Clone)]
struct QueryVector {
    schema_name: String,
    table_name: String,
    table_oid: pg_sys::Oid,
    vector_column_name: String,
    vector_attnum: i16,
    metric: DistanceMetric,
}

#[derive(Debug, Clone)]
struct SparseQueryVector {
    schema_name: String,
    table_name: String,
    table_oid: pg_sys::Oid,
    vector_name: String,
    vector_column_name: String,
    vector_attnum: i16,
    metric: DistanceMetric,
}

/// Queries a table-backed collection with dense vector and full-text branches.
///
/// The dense branch uses the collection's registered vector. The full-text
/// branch uses PostgreSQL `simple` text search over the requested source-table
/// column. Branches are fused with reciprocal rank fusion and returned in
/// deterministic fused-score order.
///
/// # Errors
///
/// Raises `undefined_object` when the collection or vector registration is
/// missing, `undefined_column` when the vector or text column has drifted,
/// `insufficient_privilege` when the caller does not own the collection or
/// lacks source-table `SELECT`, and `invalid_parameter_value` when `limit` is
/// invalid.
#[pg_extern(schema = "pgcontext", name = "query")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_collection(
    collection: String,
    vector: Vector,
    text_query: String,
    text_column: String,
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
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_query_drift(
        collection.collection_id,
        &mut registered_vector,
        &text_column,
    );
    require_table_select_privilege(&registered_vector);

    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let dense = dense_branch(collection.collection_id, &registered_vector, vector, limit);
    let full_text = full_text_branch(
        collection.collection_id,
        &registered_vector,
        &text_column,
        &text_query,
        limit,
    );
    let rows = fuse_branches(&dense, &full_text, limit);
    TableIterator::new(rows)
}

/// Queries a table-backed collection with dense and named sparse branches.
///
/// The dense branch uses the collection's registered dense vector. The sparse
/// branch uses a registered sparse vector and exact sparse scoring. Branches
/// are fused with reciprocal rank fusion and returned in deterministic fused
/// score order.
#[pg_extern(schema = "pgcontext", name = "query")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_collection_dense_sparse(
    collection: String,
    vector: Vector,
    sparse_vector_name: String,
    sparse_query: SparseVec,
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
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_query_vector_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let mut registered_sparse_vector = resolve_registered_sparse_vector(
        &collection_name,
        collection.collection_id,
        &sparse_vector_name,
    );
    validate_sparse_query_drift(collection.collection_id, &mut registered_sparse_vector);
    require_sparse_table_select_privilege(&registered_sparse_vector);

    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let dense = dense_branch(collection.collection_id, &registered_vector, vector, limit);
    let sparse = sparse_branch(
        collection.collection_id,
        &registered_sparse_vector,
        sparse_query,
        limit,
    );
    let rows = fuse_branches(&dense, &sparse, limit);
    TableIterator::new(rows)
}

/// Explains the current dense plus full-text query plan for a collection.
///
/// # Errors
///
/// Raises the same catalog, drift, ownership, and source-table privilege errors
/// as [`query_collection`].
#[pg_extern(schema = "pgcontext", name = "explain")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
pub fn explain_collection_query(
    collection: String,
    text_column: String,
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
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_query_drift(
        collection.collection_id,
        &mut registered_vector,
        &text_column,
    );
    require_table_select_privilege(&registered_vector);

    TableIterator::new(vec![
        (
            "collection".to_owned(),
            format!(
                "source_table={}.{}",
                registered_vector.schema_name, registered_vector.table_name
            ),
            None,
            "source_table".to_owned(),
            QueryExplainStatus::Ready,
            Some(collection.active_points),
            None,
        ),
        (
            "dense".to_owned(),
            format!(
                "vector_column={} metric={}",
                registered_vector.vector_column_name,
                distance_metric_label(registered_vector.metric)
            ),
            Some("dense".to_owned()),
            "exact_table_scan".to_owned(),
            QueryExplainStatus::Fallback,
            Some(collection.active_points),
            Some(policy_to_i64(
                context_core::policy::MAX_SEARCH_LIMIT,
                "max_search_limit",
            )),
        ),
        (
            "full_text".to_owned(),
            format!("text_column={text_column} config=simple"),
            Some("full_text".to_owned()),
            "postgres_full_text".to_owned(),
            QueryExplainStatus::Ready,
            Some(collection.active_points),
            Some(policy_to_i64(
                context_core::policy::MAX_SEARCH_LIMIT,
                "max_search_limit",
            )),
        ),
        (
            "fusion".to_owned(),
            format!(
                "algorithm=rrf k={} tie_break=point_id",
                RrfK::STANDARD.get()
            ),
            Some("hybrid".to_owned()),
            "reciprocal_rank_fusion".to_owned(),
            QueryExplainStatus::Ready,
            None,
            Some(policy_to_i64(
                context_core::policy::MAX_SEARCH_LIMIT,
                "max_search_limit",
            )),
        ),
        (
            "recall_budget".to_owned(),
            format!(
                "max_recall_check_point_ids={} hnsw_candidate_budget={} hnsw_iterative_expansion_limit={} hnsw_recall_threshold={}",
                context_core::policy::MAX_RECALL_CHECK_POINT_IDS,
                crate::settings::hnsw_candidate_budget_from_guc(),
                crate::settings::hnsw_iterative_expansion_limit_from_guc(),
                crate::settings::hnsw_recall_threshold_from_guc()
            ),
            None,
            "policy".to_owned(),
            QueryExplainStatus::Policy,
            None,
            Some(policy_to_i64(
                crate::settings::hnsw_candidate_budget_from_guc(),
                "hnsw_candidate_budget",
            )),
        ),
    ])
}

fn fuse_branches(
    dense: &QueryBranch,
    full_text: &QueryBranch,
    limit: SearchLimit,
) -> Vec<(i64, String, f64)> {
    let fused = reciprocal_rank_fusion_batches(
        &[dense.candidates.clone(), full_text.candidates.clone()],
        RrfK::STANDARD,
        limit.get(),
    );
    let mut source_keys = dense.source_keys.clone();
    source_keys.extend(full_text.source_keys.clone());

    fused
        .into_iter()
        .map(|point| {
            let point_id = point_id_to_sql(point.point_id());
            let source_key = source_keys
                .get(&point.point_id())
                .cloned()
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("missing fused source key for point_id {point_id}"),
                    )
                });
            (point_id, source_key, point.score())
        })
        .collect()
}

fn dense_branch(
    collection_id: i64,
    registered_vector: &QueryVector,
    query: Vector,
    limit: SearchLimit,
) -> QueryBranch {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = distance_function(registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1)::double precision AS branch_score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
          ORDER BY branch_score ASC,
                   points.point_id ASC
          LIMIT $3"
    );
    let limit = limit_to_sql(limit);

    Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            Some(limit),
            &[query.into(), collection_id.into(), limit.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load dense query branch: {error}"),
            ),
        };
        branch_from_rows(rows, CandidateBranch::DenseExact, "dense query")
    })
}

fn full_text_branch(
    collection_id: i64,
    registered_vector: &QueryVector,
    text_column: &str,
    text_query: &str,
    limit: SearchLimit,
) -> QueryBranch {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let text_column = quote_identifier(text_column);
    let sql = format!(
        "WITH query AS (
             SELECT pg_catalog.plainto_tsquery('simple', $2) AS tsquery
         )
         SELECT points.point_id,
                points.source_key,
                pg_catalog.ts_rank_cd(
                    pg_catalog.to_tsvector('simple', coalesce(source.{text_column}::text, '')),
                    query.tsquery
                )::double precision AS branch_score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
           CROSS JOIN query
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL
            AND pg_catalog.to_tsvector('simple', coalesce(source.{text_column}::text, '')) @@ query.tsquery
          ORDER BY branch_score DESC,
                   points.point_id ASC
          LIMIT $3"
    );
    let limit = limit_to_sql(limit);

    Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            Some(limit),
            &[collection_id.into(), text_query.into(), limit.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load full-text query branch: {error}"),
            ),
        };
        branch_from_rows(rows, CandidateBranch::FullText, "full-text query")
    })
}

fn sparse_branch(
    collection_id: i64,
    registered_vector: &SparseQueryVector,
    query: SparseVec,
    limit: SearchLimit,
) -> QueryBranch {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = sparse_distance_function(registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1)::double precision AS branch_score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
          ORDER BY branch_score ASC,
                   points.point_id ASC
          LIMIT $3"
    );
    let limit = limit_to_sql(limit);

    Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            Some(limit),
            &[query.into(), collection_id.into(), limit.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load sparse query branch: {error}"),
            ),
        };
        branch_from_rows(rows, CandidateBranch::SparsePlanned, "sparse query")
    })
}

fn branch_from_rows(
    rows: spi::SpiTupleTable<'_>,
    branch: CandidateBranch,
    context: &'static str,
) -> QueryBranch {
    let mut candidates = Vec::new();
    for row in rows {
        let point_id = row
            .get::<i64>(1)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to read {context} point_id: {error}"),
                )
            })
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("{context} point_id is null"),
                )
            });
        let source_key = row
            .get::<String>(2)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to read {context} source_key: {error}"),
                )
            })
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("{context} source_key is null"),
                )
            });
        let branch_score = row
            .get::<f64>(3)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to read {context} branch_score: {error}"),
                )
            })
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("{context} branch_score is null"),
                )
            });
        candidates.push(HydratedCandidate::with_score(
            point_id,
            source_key,
            branch_score,
        ));
    }

    let hydrated = match branch {
        CandidateBranch::DenseExact => hydrate_dense_exact_candidates(candidates, context),
        CandidateBranch::FullText => hydrate_full_text_candidates(candidates, context),
        CandidateBranch::SparsePlanned => hydrate_sparse_planned_candidates(candidates, context),
        CandidateBranch::DenseAnn | CandidateBranch::UserProvided => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("unsupported SPI branch hydration path: {branch:?}"),
        ),
    };
    match hydrated {
        Ok(branch) => branch,
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to hydrate {context} candidates: {error}"),
        ),
    }
}

fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn search_limit_from_sql(limit: i32) -> SearchLimit {
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

const fn sparse_distance_function(metric: DistanceMetric) -> &'static str {
    match metric {
        DistanceMetric::L2 => "sparsevec_l2_distance",
        DistanceMetric::InnerProduct | DistanceMetric::NegativeInnerProduct => {
            "sparsevec_negative_inner_product"
        }
        DistanceMetric::Cosine => "sparsevec_cosine_distance",
        DistanceMetric::L1 => "sparsevec_l1_distance",
    }
}

fn limit_to_sql(limit: SearchLimit) -> i64 {
    i64::try_from(limit.get()).unwrap_or(i64::MAX)
}

pub(super) fn policy_to_i64(value: usize, label: &'static str) -> i64 {
    match i64::try_from(value) {
        Ok(value) => value,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{label} exceeds bigint range: {value}"),
        ),
    }
}

const fn distance_function(metric: DistanceMetric) -> &'static str {
    match metric {
        DistanceMetric::L2 => "l2_distance",
        DistanceMetric::InnerProduct | DistanceMetric::NegativeInnerProduct => {
            "negative_inner_product"
        }
        DistanceMetric::Cosine => "cosine_distance",
        DistanceMetric::L1 => "l1_distance",
    }
}

fn quote_qualified_identifier(schema_name: &str, table_name: &str) -> String {
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

fn quote_identifier(identifier: &str) -> String {
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
            format!("hybrid query column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read hybrid query column {column_name}: {error}"),
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
            format!("failed to read hybrid query column {column_name}: {error}"),
        ),
    }
}

fn spi_iter_required_column<T>(
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
            format!("hybrid query column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read hybrid query column {column_name}: {error}"),
        ),
    }
}
