//! SQL-facing hybrid retrieval over registered table-backed collections.

use context_core::{CollectionName, DistanceMetric, SearchLimit};
use context_hybrid::RrfK;
use context_query::{
    Fusion, LexicalQuery, LexicalSourceName, LexicalText, QueryIr, QueryKind, ScoreOrder,
};
use pgrx::prelude::*;

use crate::domain_types::distance_metric_label;
use crate::error::{raise_core_error, raise_sql_error};
use crate::pgcontext::QueryExplainStatus;
use crate::vector::Vector;
use crate::vector_variants::SparseVec;

mod catalog;
mod late_interaction;
pub(crate) mod late_interaction_ann;
use catalog::{
    require_collection_owner, require_table_select_privilege, resolve_collection,
    resolve_registered_vector, validate_query_drift,
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
#[allow(
    dead_code,
    reason = "retained only for the isolated sparse catalog validator pending its deletion"
)]
struct SparseQueryVector {
    schema_name: String,
    table_name: String,
    table_oid: pg_sys::Oid,
    vector_name: String,
    vector_column_name: String,
    vector_attnum: i16,
}

/// Queries a table-backed collection with dense vector and lexical branches.
///
/// The dense branch uses the collection's registered vector. The lexical
/// branch evaluates a plain PostgreSQL text-search query against a registered
/// lexical source, so its configuration, ranker, weights, and any attached
/// GIN/GiST index are the registered ones. Branches are fused with reciprocal
/// rank fusion and returned in deterministic fused-score order.
///
/// # Errors
///
/// Raises `undefined_object` when the collection, vector registration, or
/// lexical source is missing, `insufficient_privilege` when the caller does not
/// own the collection or lacks source-table `SELECT`, and
/// `invalid_parameter_value` when `text_query`, `lexical_source`, or `limit`
/// is invalid.
#[pg_extern(name = "query")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_collection(
    collection: String,
    vector: Vector,
    text_query: String,
    lexical_source: String,
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
    let limit = search_limit_from_sql(limit);
    let vector = vector
        .to_dense()
        .unwrap_or_else(|error| raise_core_error(error));
    let dense = QueryIr::nearest(
        None,
        vector.as_slice().to_vec(),
        ScoreOrder::LowerIsBetter,
        None,
        limit.get(),
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    let lexical = QueryIr::lexical(
        LexicalSourceName::new(lexical_source)
            .unwrap_or_else(|error| crate::error::raise_query_error(error)),
        LexicalQuery::Plain(
            LexicalText::new(text_query)
                .unwrap_or_else(|error| crate::error::raise_query_error(error)),
        ),
        None,
        limit.get(),
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    let plan = QueryIr::new(
        QueryKind::Prefetch {
            branches: vec![dense, lexical],
            fusion: Fusion::STANDARD_RRF,
        },
        ScoreOrder::HigherIsBetter,
        None,
        limit.get(),
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    TableIterator::new(
        crate::retrieval::run_query(
            &collection_name,
            plan,
            crate::retrieval::CandidateAdapter::Exact,
        )
        .into_iter()
        .map(|(point_id, source_key, score)| (point_id, source_key, f64::from(score)))
        .collect::<Vec<_>>(),
    )
}

/// Queries a table-backed collection with dense and named sparse branches.
///
/// The dense branch uses the collection's registered dense vector. The sparse
/// branch uses a registered sparse vector and exact sparse scoring. Branches
/// are fused with reciprocal rank fusion and returned in deterministic fused
/// score order.
#[pg_extern(name = "query")]
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
    let limit = search_limit_from_sql(limit);
    let vector = vector
        .to_dense()
        .unwrap_or_else(|error| raise_core_error(error));
    let sparse_query = sparse_query
        .to_sparse()
        .unwrap_or_else(|error| raise_core_error(error));
    let dense = QueryIr::nearest(
        None,
        vector.as_slice().to_vec(),
        ScoreOrder::LowerIsBetter,
        None,
        limit.get(),
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    let sparse = QueryIr::sparse_nearest(
        sparse_vector_name,
        sparse_query,
        ScoreOrder::LowerIsBetter,
        None,
        limit.get(),
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    let plan = QueryIr::new(
        QueryKind::Prefetch {
            branches: vec![dense, sparse],
            fusion: Fusion::STANDARD_RRF,
        },
        ScoreOrder::HigherIsBetter,
        None,
        limit.get(),
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    TableIterator::new(
        crate::retrieval::run_query(
            &collection_name,
            plan,
            crate::retrieval::CandidateAdapter::Exact,
        )
        .into_iter()
        .map(|(point_id, source_key, score)| (point_id, source_key, f64::from(score)))
        .collect::<Vec<_>>(),
    )
}

/// Explains the current dense plus lexical query plan for a collection.
///
/// # Errors
///
/// Raises the same catalog, drift, ownership, and source-table privilege errors
/// as [`query_collection`].
#[pg_extern(name = "explain")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
pub fn explain_collection_query(
    collection: String,
    lexical_source: String,
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
    validate_query_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);
    let lexical = crate::lexical_catalog::prepare_lexical_source(
        collection.collection_id,
        LexicalSourceName::new(lexical_source)
            .unwrap_or_else(|error| crate::error::raise_query_error(error))
            .as_str(),
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));

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
            "lexical".to_owned(),
            format!(
                "source={} config={}.{} ranker={}",
                lexical.source_name,
                lexical.configuration_schema_name,
                lexical.configuration_name,
                lexical.ranker.stable_name()
            ),
            Some("lexical".to_owned()),
            crate::retrieval::LexicalStrategy::attached(lexical.index.as_ref())
                .lexical_label()
                .to_owned(),
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

pub(super) fn policy_to_i64(value: usize, label: &'static str) -> i64 {
    match i64::try_from(value) {
        Ok(value) => value,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{label} exceeds bigint range: {value}"),
        ),
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
