//! Experimental sparse-vector exact search SQL surface.

use core::cmp::Ordering;

use context_core::{DistanceMetric, SearchLimit, SparseVector};
use pgrx::JsonB;
use pgrx::prelude::*;

use crate::domain_types::{distance_metric_from_catalog, distance_metric_from_query};
use crate::error::{raise_core_error, raise_sql_error};
use crate::vector_variants::SparseVec;

/// Returns exact top-k sparse-vector search results.
///
/// The `point_ids` and `vectors` arrays describe the candidate set. Results are
/// sorted by ascending metric score and then by ascending point id. For
/// `inner_product`, scores are negative inner product values so larger dot
/// products rank first under PostgreSQL's ascending distance convention.
#[pg_extern(schema = "pgcontext", immutable, parallel_safe)]
pub fn search_sparse(
    query: SparseVec,
    point_ids: Vec<i64>,
    vectors: Vec<SparseVec>,
    metric: String,
    limit: i32,
) -> TableIterator<'static, (name!(point_id, i64), name!(score, f32))> {
    let query = sparsevec_to_core(query);
    let metric = distance_metric_from_query(&metric, "sparse");
    let limit = search_limit_from_sql(limit);
    let candidates = sparse_search_items_from_sql(point_ids, vectors);

    let mut rows = candidates
        .iter()
        .map(|candidate| {
            let score = metric
                .distance_sparse(&query, &candidate.vector)
                .unwrap_or_else(|error| raise_core_error(error));
            (candidate.point_id, score)
        })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    rows.truncate(limit.get());

    TableIterator::new(rows)
}

/// Explains actual named sparse candidate and exact-recheck work.
#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn explain_sparse(
    collection: String,
    vector_name: String,
    query: SparseVec,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(strategy, String),
        name!(active_points, i64),
        name!(scored_count, i64),
        name!(candidate_count, i64),
        name!(recheck_count, i64),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let collection = resolve_sparse_collection(&collection_name);
    require_sparse_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_sparse_vector(&collection_name, collection.collection_id, &vector_name);
    validate_sparse_vector_drift(collection.collection_id, &mut registered_vector);
    require_sparse_table_select_privilege(&registered_vector);
    let query = sparsevec_to_core(query);
    require_sparse_query_dimensions(&registered_vector, &query);
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let execution = crate::retrieval::run_sparse_query(
        &collection_name,
        collection.collection_id,
        &registered_vector,
        query,
        None,
        limit.get(),
    );
    let scored_count = execution
        .outcome
        .diagnostics()
        .iter()
        .find(|diagnostic| diagnostic.stage() == context_query::StageKind::Candidates)
        .map_or(0, context_query::StageDiagnostic::input_count);
    let usage = execution.outcome.usage();
    let strategy = match execution.strategy {
        crate::retrieval::SparseCandidateStrategy::Exact => "exact",
        crate::retrieval::SparseCandidateStrategy::Hnsw(_) => "hnsw",
    };
    TableIterator::once((
        strategy.to_owned(),
        active_sparse_points(collection.collection_id, &registered_vector),
        count_to_i64(scored_count, "scored_count"),
        count_to_i64(usage.candidates(), "candidate_count"),
        count_to_i64(usage.rechecks(), "recheck_count"),
    ))
}

/// Returns ANN candidates with exact top-k reranking for a registered sparse vector.
#[pg_extern(schema = "pgcontext", name = "search_sparse")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_sparse_collection(
    collection: String,
    vector_name: String,
    query: SparseVec,
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
    let collection = resolve_sparse_collection(&collection_name);
    require_sparse_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_sparse_vector(&collection_name, collection.collection_id, &vector_name);
    validate_sparse_vector_drift(collection.collection_id, &mut registered_vector);
    require_sparse_table_select_privilege(&registered_vector);

    let query = sparsevec_to_core(query);
    require_sparse_query_dimensions(&registered_vector, &query);
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let rows = crate::retrieval::run_sparse_query(
        &collection_name,
        collection.collection_id,
        &registered_vector,
        query,
        None,
        limit.get(),
    )
    .rows;
    TableIterator::new(rows)
}

/// Returns top-k named sparse results restricted by a registered filter.
#[pg_extern(schema = "pgcontext", name = "search_sparse")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_sparse_collection_filtered(
    collection: String,
    vector_name: String,
    query: SparseVec,
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
    let collection = resolve_sparse_collection(&collection_name);
    require_sparse_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_sparse_vector(&collection_name, collection.collection_id, &vector_name);
    validate_sparse_vector_drift(collection.collection_id, &mut registered_vector);
    require_sparse_table_select_privilege(&registered_vector);

    let query = sparsevec_to_core(query);
    require_sparse_query_dimensions(&registered_vector, &query);
    let filter = filter.map(|filter| {
        serde_json::from_str(&filter).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("invalid sparse search filter JSON: {error}"),
            )
        })
    });
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let rows = crate::retrieval::run_sparse_query(
        &collection_name,
        collection.collection_id,
        &registered_vector,
        query,
        filter,
        limit.get(),
    )
    .rows;
    TableIterator::new(rows)
}

#[derive(Debug, Clone)]
struct SparseCollection {
    collection_id: i64,
    owner_role: pg_sys::Oid,
}

#[derive(Debug, Clone)]
pub(crate) struct RegisteredSparseVector {
    pub(crate) schema_name: String,
    pub(crate) table_name: String,
    pub(crate) table_oid: pg_sys::Oid,
    pub(crate) vector_name: String,
    pub(crate) vector_column_name: String,
    pub(crate) vector_attnum: i16,
    pub(crate) dimensions: usize,
    pub(crate) metric: DistanceMetric,
    pub(crate) hnsw_index_name: Option<String>,
}

#[derive(Debug)]
struct SparseSearchItem {
    point_id: i64,
    vector: SparseVector,
}

fn collection_name_from_sql(collection_name: String) -> context_core::CollectionName {
    match context_core::CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn resolve_sparse_collection(collection_name: &context_core::CollectionName) -> SparseCollection {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT collection_id, owner_role, has_source_table
               FROM pgcontext._collection_acl
              WHERE collection_name = $1",
            Some(1),
            &[collection_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query sparse collection catalog: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {}", collection_name.as_str()),
            );
        }

        let row = rows.first();
        let has_source_table = spi_required_column::<bool>(&row, 3, "has_source_table");
        if !has_source_table {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has no source table: {}",
                    collection_name.as_str()
                ),
            );
        }

        SparseCollection {
            collection_id: spi_required_column::<i64>(&row, 1, "collection_id"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 2, "owner_role"),
        }
    })
}

fn resolve_registered_sparse_vector(
    collection_name: &context_core::CollectionName,
    collection_id: i64,
    vector_name: &str,
) -> RegisteredSparseVector {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT source_schema_name,
                    source_table_name,
                    source_table_oid,
                    vector_name,
                    vector_column_name,
                    vector_attnum,
                    dimensions,
                    metric,
                    index_options
               FROM pgcontext._visible_collection_sparse_vectors
              WHERE collection_id = $1
                AND vector_name = $2",
            Some(1),
            &[collection_id.into(), vector_name.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query sparse vector registration: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!(
                    "sparse vector registration does not exist for collection {}: {vector_name}",
                    collection_name.as_str()
                ),
            );
        }

        let row = rows.first();
        RegisteredSparseVector {
            schema_name: spi_required_column::<String>(&row, 1, "source_schema_name"),
            table_name: spi_required_column::<String>(&row, 2, "source_table_name"),
            table_oid: spi_required_column::<pg_sys::Oid>(&row, 3, "source_table_oid"),
            vector_name: spi_required_column::<String>(&row, 4, "vector_name"),
            vector_column_name: spi_required_column::<String>(&row, 5, "vector_column_name"),
            vector_attnum: spi_required_column::<i16>(&row, 6, "vector_attnum"),
            dimensions: usize::try_from(spi_required_column::<i32>(&row, 7, "dimensions"))
                .unwrap_or_else(|_| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        "registered sparse vector dimensions are invalid",
                    )
                }),
            metric: distance_metric_from_catalog(
                spi_required_column::<String>(&row, 8, "metric"),
                "sparse vector",
            ),
            hnsw_index_name: spi_required_column::<JsonB>(&row, 9, "index_options")
                .0
                .get("hnsw_index")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        }
    })
}

fn validate_sparse_vector_drift(
    collection_id: i64,
    registered_vector: &mut RegisteredSparseVector,
) {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT class.oid,
                    vector_attribute.attnum,
                    vector_attribute.attname::text,
                    vector_attribute.atttypid = 'public.sparsevec'::regtype AS vector_is_valid,
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
                registered_vector.schema_name.as_str().into(),
                registered_vector.table_name.as_str().into(),
                registered_vector.vector_column_name.as_str().into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to validate sparse vector catalog drift: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                format!(
                    "registered source table drifted: {}.{}",
                    registered_vector.schema_name, registered_vector.table_name
                ),
            );
        }

        let row = rows.first();
        let current_table_oid = spi_required_column::<pg_sys::Oid>(&row, 1, "source_table_oid");
        let current_vector_attnum = spi_optional_column::<i16>(&row, 2, "vector_attnum");
        let vector_column = spi_optional_column::<String>(&row, 3, "vector_column_name");
        let vector_is_valid = spi_optional_column::<bool>(&row, 4, "vector_is_valid");
        if vector_column.is_none() || vector_is_valid != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "registered sparse vector column drifted: {}.{}",
                    registered_vector.table_name, registered_vector.vector_column_name
                ),
            );
        }
        let Some(current_vector_attnum) = current_vector_attnum else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "validated sparse vector column had no attnum",
            );
        };
        refresh_restored_sparse_metadata(
            collection_id,
            registered_vector,
            current_table_oid,
            current_vector_attnum,
        );

        let id_exists = spi_required_column::<bool>(&row, 5, "id_exists");
        if !id_exists {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "source key column does not exist on {}.{}: id",
                    registered_vector.schema_name, registered_vector.table_name
                ),
            );
        }
    });
}

pub(crate) fn resolve_sparse_hnsw_index(
    registered_vector: &RegisteredSparseVector,
) -> Option<pg_sys::Oid> {
    let index_name = registered_vector.hnsw_index_name.as_deref()?;
    let expected_opclass = match registered_vector.metric {
        DistanceMetric::L2 => "sparsevec_hnsw_ops",
        DistanceMetric::InnerProduct | DistanceMetric::NegativeInnerProduct => {
            "sparsevec_hnsw_ip_ops"
        }
        DistanceMetric::Cosine => "sparsevec_hnsw_cosine_ops",
        DistanceMetric::L1 => "sparsevec_hnsw_l1_ops",
        DistanceMetric::Hamming | DistanceMetric::Jaccard => return None,
    };
    Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT index_class.oid
           FROM pg_catalog.pg_class AS index_class
           JOIN pg_catalog.pg_index AS index_def ON index_def.indexrelid = index_class.oid
           JOIN pg_catalog.pg_am AS access_method ON access_method.oid = index_class.relam
           JOIN pg_catalog.pg_opclass AS operator_class ON operator_class.oid = index_def.indclass[0]
           JOIN pg_catalog.pg_namespace AS operator_namespace
             ON operator_namespace.oid = operator_class.opcnamespace
          WHERE index_class.oid = pg_catalog.to_regclass($1)
            AND index_class.relkind = 'i'
            AND index_def.indrelid = $2
            AND index_def.indisvalid
            AND index_def.indisready
            AND index_def.indislive
            AND index_def.indpred IS NULL
            AND index_def.indexprs IS NULL
            AND index_def.indnkeyatts = 1
            AND index_def.indkey[0] = $3
            AND access_method.amname = 'pgcontext_hnsw'
            AND operator_namespace.nspname = 'pgcontext'
            AND operator_class.opcintype = 'public.sparsevec'::pg_catalog.regtype
            AND operator_class.opcname = $4",
        &[
            index_name.into(),
            registered_vector.table_oid.into(),
            registered_vector.vector_attnum.into(),
            expected_opclass.into(),
        ],
    )
    .ok()
    .flatten()
}

fn require_sparse_query_dimensions(
    registered_vector: &RegisteredSparseVector,
    query: &SparseVector,
) {
    if query.dimensions() != registered_vector.dimensions {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "dimension mismatch: left has {} dimensions, right has {}",
                registered_vector.dimensions,
                query.dimensions()
            ),
        );
    }
}

fn active_sparse_points(collection_id: i64, registered_vector: &RegisteredSparseVector) -> i64 {
    let table_name = crate::table_search::quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    Spi::get_one_with_args::<i64>(
        &format!(
            "SELECT count(*)::bigint
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
              WHERE points.collection_id = $1
                AND points.deleted_at IS NULL"
        ),
        &[collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to count active sparse points: {error}"),
        )
    })
    .unwrap_or_default()
}

fn count_to_i64(value: usize, label: &'static str) -> i64 {
    i64::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            format!("sparse explain {label} exceeds bigint"),
        )
    })
}

fn refresh_restored_sparse_metadata(
    collection_id: i64,
    registered_vector: &mut RegisteredSparseVector,
    current_table_oid: pg_sys::Oid,
    current_vector_attnum: i16,
) {
    if registered_vector.table_oid == current_table_oid
        && registered_vector.vector_attnum == current_vector_attnum
    {
        return;
    }

    // Route metadata writes through SECURITY DEFINER helpers: `search_sparse`
    // runs SECURITY INVOKER, so a non-superuser collection member holds no
    // direct write privilege on the private catalog tables. The helpers
    // re-derive the authoritative oid/attnum from the stored source identity.
    Spi::run_with_args(
        "SELECT pgcontext._refresh_collection_source_table($1)",
        &[collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to refresh restored collection metadata: {error}"),
        )
    });

    Spi::run_with_args(
        "SELECT pgcontext._refresh_sparse_vector_source_binding($1, $2)",
        &[
            collection_id.into(),
            registered_vector.vector_name.as_str().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to refresh restored sparse vector metadata: {error}"),
        )
    });

    registered_vector.table_oid = current_table_oid;
    registered_vector.vector_attnum = current_vector_attnum;
}

pub(crate) fn sparse_distance_function(metric: DistanceMetric) -> &'static str {
    match metric {
        DistanceMetric::L2 => "sparsevec_l2_distance",
        DistanceMetric::InnerProduct | DistanceMetric::NegativeInnerProduct => {
            "sparsevec_negative_inner_product"
        }
        DistanceMetric::Cosine => "sparsevec_cosine_distance",
        DistanceMetric::L1 => "sparsevec_l1_distance",
        DistanceMetric::Hamming | DistanceMetric::Jaccard => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            "bit distance metrics cannot score sparsevec collections",
        ),
    }
}

fn require_sparse_collection_owner(
    collection: &SparseCollection,
    collection_name: &context_core::CollectionName,
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

fn require_sparse_table_select_privilege(registered_vector: &RegisteredSparseVector) {
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

fn sparse_search_items_from_sql(
    point_ids: Vec<i64>,
    vectors: Vec<SparseVec>,
) -> Vec<SparseSearchItem> {
    if point_ids.len() != vectors.len() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "point_ids and sparse vectors must have the same length: got {} ids and {} vectors",
                point_ids.len(),
                vectors.len()
            ),
        );
    }

    point_ids
        .into_iter()
        .zip(vectors)
        .map(|(point_id, vector)| {
            if point_id < 0 {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("point id must be non-negative: {point_id}"),
                );
            }
            SparseSearchItem {
                point_id,
                vector: sparsevec_to_core(vector),
            }
        })
        .collect()
}

fn sparsevec_to_core(vector: SparseVec) -> SparseVector {
    match vector.to_sparse() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    }
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
            format!("sparse search column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read sparse search column {column_name}: {error}"),
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
            format!("failed to read sparse search column {column_name}: {error}"),
        ),
    }
}
