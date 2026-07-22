//! SQL-facing vector registration metadata functions.
use context_core::{CollectionName, DistanceMetric, SqlIdentifier, VectorDimensions, VectorName};
use pgrx::JsonB;
use pgrx::prelude::*;
use serde_json::Value;

use crate::domain_types::{
    VectorStatus, distance_metric_from_catalog, distance_metric_from_sql, distance_metric_label,
    vector_status_from_catalog, vector_status_from_sql,
};
use crate::error::{raise_core_error, raise_sql_error};
use crate::vector_metadata_validation::validate_quantization_options;

#[derive(Debug, Clone)]
struct CollectionAcl {
    id: i64,
    name: String,
    owner_role: pg_sys::Oid,
    owner_name: String,
    source_table: Option<TableResolution>,
}
#[derive(Debug, Clone)]
struct TableResolution {
    oid: pg_sys::Oid,
    schema_name: String,
    table_name: String,
}
#[derive(Debug, Clone)]
struct SparseVectorColumnResolution {
    attnum: i16,
    column_name: String,
}
#[derive(Debug, Clone)]
struct VectorMetadata {
    collection_name: String,
    vector_name: String,
    table_schema: String,
    table_name: String,
    vector_column: String,
    dimensions: i32,
    metric: DistanceMetric,
    hnsw_options: Value,
    quantization_options: Value,
    status: VectorStatus,
}
#[derive(Debug, Clone)]
struct SparseVectorMetadata {
    collection_name: String,
    vector_name: String,
    table_schema: String,
    table_name: String,
    vector_column: String,
    dimensions: i32,
    metric: DistanceMetric,
    storage_options: Value,
    index_options: Value,
    status: VectorStatus,
}
/// Lists registered dense-vector metadata.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn collection_vectors(
    collection_name: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(vector_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(vector_column, String),
        name!(dimensions, i32),
        name!(metric, String),
        name!(hnsw_options, JsonB),
        name!(quantization_options, JsonB),
        name!(status, String),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = require_collection(&collection_name);
    require_collection_owner(&collection);

    TableIterator::new(
        select_vector_metadata(collection.id, &collection.name)
            .into_iter()
            .map(vector_metadata_row),
    )
}

/// Updates dense-vector configuration and invalidates incompatible artifacts.
///
/// Each successful update advances the collection configuration revision and marks
/// file-materialized artifacts for the collection rebuild-required in the same
/// transaction. A later artifact can become serving-ready only when its build
/// job captured the current revision.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn configure_vector(
    collection_name: String,
    vector_name: String,
    hnsw_options: JsonB,
    quantization_options: JsonB,
    status: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(vector_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(vector_column, String),
        name!(dimensions, i32),
        name!(metric, String),
        name!(hnsw_options, JsonB),
        name!(quantization_options, JsonB),
        name!(status, String),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let vector_name = vector_name_from_sql(vector_name);
    let hnsw_options = json_object_from_sql("hnsw_options", hnsw_options);
    let quantization_options = json_object_from_sql("quantization_options", quantization_options);
    if let Err(error) = validate_quantization_options(&quantization_options) {
        raise_sql_error(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, error);
    }
    let status = vector_status_from_sql(&status);
    let collection = require_collection(&collection_name);
    require_collection_owner(&collection);

    let row = match update_vector_metadata(
        collection.id,
        &collection.name,
        &vector_name,
        &hnsw_options,
        &quantization_options,
        status,
    ) {
        Some(row) => row,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!(
                "vector registration does not exist for collection {}: {}",
                collection_name.as_str(),
                vector_name.as_str()
            ),
        ),
    };

    TableIterator::once(vector_metadata_row(row))
}

/// Binds a registered dense vector to its validated PostgreSQL HNSW index.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn attach_hnsw_index(collection_name: String, vector_name: String, index_name: String) {
    let collection_name = collection_name_from_sql(collection_name);
    let vector_name = vector_name_from_sql(vector_name);
    let collection = require_collection(&collection_name);
    require_collection_owner(&collection);
    let updated = Spi::get_one_with_args::<i64>(
        "UPDATE pgcontext._collection_vectors AS vectors
            SET hnsw_index_oid = index_class.oid, updated_at = pg_catalog.now()
           FROM pg_catalog.pg_class AS index_class
           JOIN pg_catalog.pg_index AS index_def ON index_def.indexrelid = index_class.oid
           JOIN pg_catalog.pg_am AS access_method ON access_method.oid = index_class.relam
          WHERE vectors.collection_id = $1
            AND vectors.vector_name = $2
            AND index_class.oid = pg_catalog.to_regclass($3)
            AND index_def.indrelid = vectors.source_table_oid
            AND access_method.amname = 'pgcontext_hnsw'
            AND vectors.vector_attnum = ANY(index_def.indkey)
        RETURNING vectors.vector_id",
        &[
            collection.id.into(),
            vector_name.as_str().into(),
            index_name.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to attach HNSW index: {error}"),
        )
    });
    if updated.is_none() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "HNSW index does not match the registered collection vector",
        );
    }
}

/// Binds a registered sparse vector to its validated PostgreSQL HNSW index.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn attach_sparse_hnsw_index(collection_name: String, vector_name: String, index_name: String) {
    let collection_name = collection_name_from_sql(collection_name);
    let vector_name = vector_name_from_sql(vector_name);
    let collection = require_collection(&collection_name);
    require_collection_owner(&collection);
    let updated = Spi::connect_mut(|client| {
        client.update(
        "UPDATE pgcontext._collection_sparse_vectors AS vectors
            SET index_options = pg_catalog.jsonb_set(
                    vectors.index_options,
                    '{hnsw_index}',
                    pg_catalog.to_jsonb(pg_catalog.format('%I.%I', index_namespace.nspname, index_class.relname)),
                    true
                ),
                updated_at = pg_catalog.now()
           FROM pg_catalog.pg_class AS index_class
           JOIN pg_catalog.pg_namespace AS index_namespace
             ON index_namespace.oid = index_class.relnamespace
           JOIN pg_catalog.pg_index AS index_def ON index_def.indexrelid = index_class.oid
           JOIN pg_catalog.pg_am AS access_method ON access_method.oid = index_class.relam
           JOIN pg_catalog.pg_opclass AS operator_class ON operator_class.oid = index_def.indclass[0]
           JOIN pg_catalog.pg_namespace AS operator_namespace
             ON operator_namespace.oid = operator_class.opcnamespace
          WHERE vectors.collection_id = $1
            AND vectors.vector_name = $2
            AND index_class.oid = pg_catalog.to_regclass($3)
            AND index_class.relkind = 'i'
            AND index_def.indrelid = vectors.source_table_oid
            AND index_def.indisvalid
            AND index_def.indisready
            AND index_def.indislive
            AND index_def.indpred IS NULL
            AND index_def.indexprs IS NULL
            AND index_def.indnkeyatts = 1
            AND access_method.amname = 'pgcontext_hnsw'
            AND index_def.indkey[0] = vectors.vector_attnum
            AND operator_namespace.nspname = 'pgcontext'
            AND operator_class.opcintype = 'public.sparsevec'::pg_catalog.regtype
            AND operator_class.opcname = CASE vectors.metric
                    WHEN 'l2' THEN 'sparsevec_hnsw_ops'
                    WHEN 'inner_product' THEN 'sparsevec_hnsw_ip_ops'
                    WHEN 'cosine' THEN 'sparsevec_hnsw_cosine_ops'
                    WHEN 'l1' THEN 'sparsevec_hnsw_l1_ops'
                END
        RETURNING vectors.sparse_vector_id",
        Some(1),
        &[
            collection.id.into(),
            vector_name.as_str().into(),
            index_name.into(),
        ],
        )
        .map(|rows| !rows.is_empty())
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to attach sparse HNSW index: {error}"),
        )
    });
    if !updated {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "HNSW index does not match the registered sparse collection vector",
        );
    }
}

/// Registers a table-backed sparse vector column.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn register_sparse_vector(
    collection_name: String,
    vector_name: String,
    vector_column: String,
    dimensions: i32,
    metric: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(vector_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(vector_column, String),
        name!(dimensions, i32),
        name!(metric, String),
        name!(storage_options, JsonB),
        name!(index_options, JsonB),
        name!(status, String),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let vector_name = vector_name_from_sql(vector_name);
    let vector_column = sql_identifier_from_sql(vector_column);
    let dimensions = dimensions_from_sql(dimensions);
    let metric = distance_metric_from_sql(&metric, "");
    let collection = require_collection(&collection_name);
    require_collection_owner(&collection);
    let Some(source_table) = collection.source_table.as_ref() else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "collection has no source table: {}",
                collection_name.as_str()
            ),
        );
    };
    require_table_select_privilege(source_table);
    let vector_column = resolve_sparse_vector_column(source_table, &vector_column);

    let row = match insert_sparse_vector_registration(
        &collection,
        &vector_name,
        source_table,
        &vector_column,
        dimensions,
        metric,
    ) {
        Some(row) => row,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            format!(
                "sparse vector registration already exists for collection {}: {}",
                collection_name.as_str(),
                vector_name.as_str()
            ),
        ),
    };

    TableIterator::once(sparse_vector_metadata_row(row))
}

/// Lists registered sparse-vector metadata.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn collection_sparse_vectors(
    collection_name: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(vector_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(vector_column, String),
        name!(dimensions, i32),
        name!(metric, String),
        name!(storage_options, JsonB),
        name!(index_options, JsonB),
        name!(status, String),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = require_collection(&collection_name);
    require_collection_owner(&collection);

    TableIterator::new(
        select_sparse_vector_metadata(collection.id, &collection.name)
            .into_iter()
            .map(sparse_vector_metadata_row),
    )
}

/// Updates sparse-vector storage, index, and lifecycle metadata.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn configure_sparse_vector(
    collection_name: String,
    vector_name: String,
    storage_options: JsonB,
    index_options: JsonB,
    status: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(vector_name, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(vector_column, String),
        name!(dimensions, i32),
        name!(metric, String),
        name!(storage_options, JsonB),
        name!(index_options, JsonB),
        name!(status, String),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let vector_name = vector_name_from_sql(vector_name);
    let storage_options = json_object_from_sql("storage_options", storage_options);
    let index_options = json_object_from_sql("index_options", index_options);
    let status = vector_status_from_sql(&status);
    let collection = require_collection(&collection_name);
    require_collection_owner(&collection);

    let row = match update_sparse_vector_metadata(
        collection.id,
        &collection.name,
        &vector_name,
        &storage_options,
        &index_options,
        status,
    ) {
        Some(row) => row,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!(
                "sparse vector registration does not exist for collection {}: {}",
                collection_name.as_str(),
                vector_name.as_str()
            ),
        ),
    };

    TableIterator::once(sparse_vector_metadata_row(row))
}

include!("vector_catalog/persistence.rs");
