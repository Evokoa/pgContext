//! Late-interaction SQL search over table-backed vector arrays.

use core::cmp::Ordering;

use context_core::{CollectionName, DenseVector, SearchLimit};
use context_query::{
    MultiVectorAnnReason, MultiVectorAnnStrategy, MultiVectorAnnStrategyInput,
    MultiVectorAnnStrategyKind, select_multi_vector_ann_strategy,
};
use pgrx::{pg_sys, prelude::*};

use crate::error::{raise_core_error, raise_query_error, raise_sql_error};
use crate::late_interaction::{
    MAX_LATE_INTERACTION_COMPARISONS, enforce_late_interaction_budget,
    late_interaction_comparison_count, late_interaction_score,
};
use crate::vector::Vector;

use super::{
    QueryExplainStatus, collection_name_from_sql, policy_to_i64, quote_identifier,
    quote_qualified_identifier, search_limit_from_sql, session_user, spi_iter_required_column,
    spi_optional_column, spi_required_column,
};

pub(super) struct LateInteractionCollection {
    pub(super) collection_id: i64,
    pub(super) owner_role: pg_sys::Oid,
    pub(super) table_oid: pg_sys::Oid,
    pub(super) schema_name: String,
    pub(super) table_name: String,
}

/// Exact late-interaction search over a table-backed `vector[]` column.
///
/// Each source-table row stores all candidate vectors for one point in
/// `vector_column`. Scores use MaxSim: for every query vector, take the maximum
/// inner product against that row's candidate vectors, then sum those maxima.
/// Results order by descending score and ascending point ID.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_late_interaction(
    collection: String,
    query_vectors: Vec<Vector>,
    vector_column: String,
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

    let query_vectors = dense_vectors_from_sql("late interaction query_vectors", query_vectors);
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );

    let rows = search_late_interaction_table(&collection, &query_vectors, &vector_column, limit);
    TableIterator::new(rows)
}

/// Explains the exact late-interaction plan for a table-backed `vector[]` column.
///
/// The diagnostic validates the same collection ownership, table drift,
/// privileges, and query-vector shape as [`search_late_interaction`]. It then
/// reports the active point count, active candidate vector count, projected
/// MaxSim comparisons, and the comparison budget before the query loads any
/// candidate vectors.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
pub fn explain_late_interaction(
    collection: String,
    query_vectors: Vec<Vector>,
    vector_column: String,
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

    let query_vectors = dense_vectors_from_sql("late interaction query_vectors", query_vectors);
    let candidate_stats = late_interaction_candidate_stats(&collection, &vector_column);
    let projected_comparisons =
        late_interaction_comparison_count(query_vectors.len(), candidate_stats.vector_count);
    let ann_strategy = late_interaction_ann_strategy(&query_vectors, candidate_stats);

    TableIterator::new(vec![
        (
            "collection".to_owned(),
            format!(
                "source_table={}.{}",
                collection.schema_name, collection.table_name
            ),
            None,
            "source_table".to_owned(),
            QueryExplainStatus::Ready,
            Some(policy_to_i64(
                candidate_stats.point_count,
                "late_interaction_point_count",
            )),
            None,
        ),
        (
            "late_interaction".to_owned(),
            format!(
                "vector_column={} active_points={} candidate_vectors={}",
                vector_column, candidate_stats.point_count, candidate_stats.vector_count
            ),
            Some("multi_vector".to_owned()),
            "exact_table_scan".to_owned(),
            QueryExplainStatus::Fallback,
            Some(policy_to_i64(
                candidate_stats.vector_count,
                "late_interaction_candidate_vectors",
            )),
            Some(policy_to_i64(
                MAX_LATE_INTERACTION_COMPARISONS,
                "max_late_interaction_comparisons",
            )),
        ),
        (
            "maxsim".to_owned(),
            format!(
                "query_vectors={} projected_comparisons={} tie_break=point_id",
                query_vectors.len(),
                projected_comparisons
            ),
            Some("multi_vector".to_owned()),
            "exact_maxsim".to_owned(),
            QueryExplainStatus::Ready,
            Some(policy_to_i64(
                projected_comparisons,
                "late_interaction_projected_comparisons",
            )),
            Some(policy_to_i64(
                MAX_LATE_INTERACTION_COMPARISONS,
                "max_late_interaction_comparisons",
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
                MAX_LATE_INTERACTION_COMPARISONS,
                "max_late_interaction_comparisons",
            )),
        ),
    ])
}

fn late_interaction_ann_strategy(
    query_vectors: &[DenseVector],
    candidate_stats: LateInteractionCandidateStats,
) -> MultiVectorAnnStrategy {
    let input = MultiVectorAnnStrategyInput::new(
        candidate_stats.point_count,
        candidate_stats.vector_count,
        query_vectors.len(),
        false,
        false,
        usize::MAX,
        MAX_LATE_INTERACTION_COMPARISONS,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    select_multi_vector_ann_strategy(input)
}

pub(super) fn late_interaction_ann_candidate_strategy(
    query_vectors: &[DenseVector],
    candidate_stats: LateInteractionCandidateStats,
    candidates_per_query: SearchLimit,
) -> MultiVectorAnnStrategy {
    let projected_candidate_vectors = query_vectors
        .len()
        .saturating_mul(candidates_per_query.get())
        .min(candidate_stats.vector_count);
    let input = MultiVectorAnnStrategyInput::new(
        candidate_stats.point_count,
        projected_candidate_vectors,
        query_vectors.len(),
        true,
        true,
        usize::MAX,
        MAX_LATE_INTERACTION_COMPARISONS,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    select_multi_vector_ann_strategy(input)
}

pub(super) fn late_interaction_ann_strategy_name(kind: MultiVectorAnnStrategyKind) -> &'static str {
    match kind {
        MultiVectorAnnStrategyKind::ExactNoOp => "exact_noop",
        MultiVectorAnnStrategyKind::ExactTableScan => "exact_table_scan",
        MultiVectorAnnStrategyKind::Rejected => "rejected",
        MultiVectorAnnStrategyKind::PlannedNotServingReady => "planned_not_serving_ready",
        MultiVectorAnnStrategyKind::AnnCandidateServing => "ann_candidate_serving",
    }
}

pub(super) fn late_interaction_ann_status(kind: MultiVectorAnnStrategyKind) -> QueryExplainStatus {
    match kind {
        MultiVectorAnnStrategyKind::ExactNoOp => QueryExplainStatus::Ready,
        MultiVectorAnnStrategyKind::AnnCandidateServing => QueryExplainStatus::Ready,
        MultiVectorAnnStrategyKind::ExactTableScan => QueryExplainStatus::Fallback,
        MultiVectorAnnStrategyKind::Rejected
        | MultiVectorAnnStrategyKind::PlannedNotServingReady => QueryExplainStatus::Policy,
    }
}

pub(super) fn late_interaction_ann_detail(strategy: &MultiVectorAnnStrategy) -> String {
    let reasons = strategy
        .reasons()
        .iter()
        .map(|reason| match reason {
            MultiVectorAnnReason::EmptyCollection => "EmptyCollection",
            MultiVectorAnnReason::NoAnnServingPath => "NoAnnServingPath",
            MultiVectorAnnReason::ComparisonBudgetExceeded => "ComparisonBudgetExceeded",
            MultiVectorAnnReason::AnnMetadataNotServingReady => "AnnMetadataNotServingReady",
            MultiVectorAnnReason::AnnCandidateServingReady => "AnnCandidateServingReady",
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "kind={} reasons={} projected_comparisons={} comparison_budget={}",
        late_interaction_ann_strategy_name(strategy.kind()),
        reasons,
        strategy.projected_comparisons(),
        MAX_LATE_INTERACTION_COMPARISONS
    )
}

pub(super) fn resolve_late_interaction_collection(
    collection_name: &CollectionName,
) -> LateInteractionCollection {
    // Phase 1: identity + owner from the PUBLIC ACL view (which also resolves
    // aliases). This is readable by any caller, so both members and non-members
    // reach the ownership gate below and get a consistent error.
    let (collection_id, owner_role) = Spi::connect(|client| {
        let rows = match client.select(
            "SELECT collection_id, owner_role
               FROM pgcontext._collection_acl
              WHERE collection_name = $1",
            Some(1),
            &[collection_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query late-interaction collection catalog: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {}", collection_name.as_str()),
            );
        }

        let row = rows.first();
        (
            spi_required_column::<i64>(&row, 1, "collection_id"),
            spi_required_column::<pg_sys::Oid>(&row, 2, "owner_role"),
        )
    });

    // Ownership gate BEFORE reading source-table metadata. Source metadata lives
    // behind the membership-filtered `_visible_collections` view, so the check
    // must precede phase 2 to keep the "permission denied for collection" error
    // for non-members rather than surfacing an empty-row "no source table".
    require_late_interaction_owner_role(owner_role, collection_name);

    // Phase 2: source-table metadata from the membership-filtered view. The
    // caller is a confirmed member, so their row is visible.
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT source_table_oid, source_schema_name, source_table_name
               FROM pgcontext._visible_collections
              WHERE collection_id = $1",
            Some(1),
            &[collection_id.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query late-interaction collection catalog: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {}", collection_name.as_str()),
            );
        }

        let row = rows.first();
        let Some(table_oid) = spi_optional_column::<pg_sys::Oid>(&row, 1, "source_table_oid")
        else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has no source table: {}",
                    collection_name.as_str()
                ),
            );
        };
        let Some(schema_name) = spi_optional_column::<String>(&row, 2, "source_schema_name") else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has no source table: {}",
                    collection_name.as_str()
                ),
            );
        };
        let Some(table_name) = spi_optional_column::<String>(&row, 3, "source_table_name") else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has no source table: {}",
                    collection_name.as_str()
                ),
            );
        };

        LateInteractionCollection {
            collection_id,
            owner_role,
            table_oid,
            schema_name,
            table_name,
        }
    })
}

pub(super) fn validate_late_interaction_drift(
    collection: &mut LateInteractionCollection,
    vector_column: &str,
) {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT class.oid,
                    vector_attribute.attnum,
                    vector_attribute.attname::text,
                    vector_attribute.atttypid = 'pgcontext.vector[]'::regtype AS vector_is_valid,
                    id_attribute.attname IS NOT NULL AS id_exists
               FROM pg_catalog.pg_class AS class
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
               LEFT JOIN pg_catalog.pg_attribute AS vector_attribute
                 ON vector_attribute.attrelid = class.oid
                AND vector_attribute.attname = $3
                AND vector_attribute.attnum > 0
                AND NOT vector_attribute.attisdropped
               LEFT JOIN pg_catalog.pg_attribute AS id_attribute
                 ON id_attribute.attrelid = class.oid
                AND id_attribute.attname = 'id'
                AND id_attribute.attnum > 0
                AND NOT id_attribute.attisdropped
              WHERE namespace.nspname = $1
                AND class.relname = $2
                AND class.relkind IN ('r', 'p')",
            Some(1),
            &[
                collection.schema_name.as_str().into(),
                collection.table_name.as_str().into(),
                vector_column.into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to validate late-interaction catalog drift: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                format!(
                    "registered source table drifted: {}.{}",
                    collection.schema_name, collection.table_name
                ),
            );
        }

        let row = rows.first();
        let current_table_oid = spi_required_column::<pg_sys::Oid>(&row, 1, "source_table_oid");
        refresh_restored_late_interaction_metadata(collection, current_table_oid);

        let vector_column_name = spi_optional_column::<String>(&row, 3, "vector_column_name");
        if vector_column_name.is_none() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction vector column does not exist on {}.{}: {vector_column}",
                    collection.schema_name, collection.table_name
                ),
            );
        }

        let vector_is_valid = spi_optional_column::<bool>(&row, 4, "vector_is_valid");
        if vector_is_valid != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                format!(
                    "late-interaction vector column must have type vector[]: {}.{}.{vector_column}",
                    collection.schema_name, collection.table_name
                ),
            );
        }

        let id_exists = spi_required_column::<bool>(&row, 5, "id_exists");
        if !id_exists {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "source key column does not exist on {}.{}: id",
                    collection.schema_name, collection.table_name
                ),
            );
        }
    });
}

fn refresh_restored_late_interaction_metadata(
    collection: &mut LateInteractionCollection,
    current_table_oid: pg_sys::Oid,
) {
    if collection.table_oid == current_table_oid {
        return;
    }

    Spi::run_with_args(
        "SELECT pgcontext._refresh_collection_source_table($1)",
        &[collection.collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to refresh restored collection metadata: {error}"),
        )
    });

    collection.table_oid = current_table_oid;
}

fn search_late_interaction_table(
    collection: &LateInteractionCollection,
    query_vectors: &[DenseVector],
    vector_column: &str,
    limit: SearchLimit,
) -> Vec<(i64, String, f64)> {
    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let vector_column = quote_identifier(vector_column);
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                source.{vector_column}
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL"
    );

    let mut scored_rows = Spi::connect(|client| {
        let rows = match client.select(&sql, None, &[collection.collection_id.into()]) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load late-interaction candidates: {error}"),
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

#[derive(Debug, Clone, Copy)]
pub(super) struct LateInteractionCandidateStats {
    pub(super) point_count: usize,
    pub(super) vector_count: usize,
}

pub(super) fn late_interaction_candidate_stats(
    collection: &LateInteractionCollection,
    vector_column: &str,
) -> LateInteractionCandidateStats {
    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let vector_column = quote_identifier(vector_column);
    let sql = format!(
        "SELECT pg_catalog.count(*)::bigint AS active_points,
                coalesce(pg_catalog.sum(pg_catalog.cardinality(source.{vector_column})), 0)::bigint
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL"
    );

    Spi::connect(|client| {
        let rows = match client.select(&sql, None, &[collection.collection_id.into()]) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to explain late-interaction candidates: {error}"),
            ),
        };
        let row = rows.first();
        LateInteractionCandidateStats {
            point_count: usize_from_nonnegative_i64(
                spi_required_column::<i64>(&row, 1, "late_interaction_active_points"),
                "late_interaction_active_points",
            ),
            vector_count: usize_from_nonnegative_i64(
                spi_required_column::<i64>(&row, 2, "late_interaction_candidate_vectors"),
                "late_interaction_candidate_vectors",
            ),
        }
    })
}

fn usize_from_nonnegative_i64(value: i64, label: &'static str) -> usize {
    match usize::try_from(value) {
        Ok(value) => value,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{label} is negative or exceeds usize range: {value}"),
        ),
    }
}

pub(super) fn late_interaction_rows_from_spi(
    rows: spi::SpiTupleTable<'_>,
    query_vectors: &[DenseVector],
) -> Vec<(i64, String, f64)> {
    let mut output = Vec::new();
    let mut candidate_vector_count = 0usize;
    for row in rows {
        let point_id = spi_iter_required_column::<i64>(&row, 1, "late_interaction_point_id");
        let source_key = spi_iter_required_column::<String>(&row, 2, "late_interaction_source_key");
        let candidate_vectors = row
            .get::<Vec<Vector>>(3)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to read late-interaction vector array: {error}"),
                )
            })
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("late-interaction vector column is null for point_id {point_id}"),
                )
            });
        let candidate_vectors = late_interaction_candidate_vectors_from_sql(candidate_vectors);
        candidate_vector_count = candidate_vector_count.saturating_add(candidate_vectors.len());
        enforce_late_interaction_budget(query_vectors.len(), candidate_vector_count);
        let score = f64::from(late_interaction_score(query_vectors, &candidate_vectors));
        output.push((point_id, source_key, score));
    }
    output
}

pub(super) fn require_late_interaction_collection_owner(
    collection: &LateInteractionCollection,
    collection_name: &CollectionName,
) {
    require_late_interaction_owner_role(collection.owner_role, collection_name);
}

fn require_late_interaction_owner_role(owner_role: pg_sys::Oid, collection_name: &CollectionName) {
    let session_user = session_user();
    let is_owner = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role($1, $2, 'MEMBER')",
        &[session_user.as_str().into(), owner_role.into()],
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

pub(super) fn require_late_interaction_table_select_privilege(
    collection: &LateInteractionCollection,
) {
    let session_user = session_user();
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT')",
        &[session_user.as_str().into(), collection.table_oid.into()],
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
                collection.schema_name, collection.table_name
            ),
        );
    }
}

pub(super) fn dense_vectors_from_sql(
    label: &'static str,
    vectors: Vec<Vector>,
) -> Vec<DenseVector> {
    if vectors.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} must not be empty"),
        );
    }
    vectors
        .into_iter()
        .map(|vector| match vector.to_dense() {
            Ok(vector) => vector,
            Err(error) => raise_core_error(error),
        })
        .collect()
}

fn late_interaction_candidate_vectors_from_sql(vectors: Vec<Vector>) -> Vec<DenseVector> {
    if vectors.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "each late-interaction candidate point must have at least one vector",
        );
    }
    dense_vectors_from_sql("late interaction candidate_vectors", vectors)
}
