//! Named dense-vector search overloads.

use context_core::{CollectionName, VectorName};
use pgrx::prelude::*;

use crate::Vector;
use crate::error::{raise_core_error, raise_sql_error};

use super::{
    SearchVector, collection_name_from_sql, load_filter_fields, require_collection_owner,
    require_table_select_privilege, resolve_collection, resolve_filter_plan, search_limit_from_sql,
    search_registered_table, search_registered_table_filtered, validate_search_drift,
};

#[pg_extern(schema = "pgcontext", name = "search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_collection_named_vector(
    collection: String,
    vector_name: String,
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
    let vector_name = vector_name_from_sql(vector_name);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector_by_name(&collection_name, collection.collection_id, &vector_name);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let rows = search_registered_table(collection.collection_id, &registered_vector, vector, limit);
    TableIterator::new(rows)
}

#[pg_extern(schema = "pgcontext", name = "search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_collection_named_vector_filtered(
    collection: String,
    vector_name: String,
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
    let vector_name = vector_name_from_sql(vector_name);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector_by_name(&collection_name, collection.collection_id, &vector_name);
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

pub(super) fn resolve_registered_vector_by_name(
    collection_name: &CollectionName,
    collection_id: i64,
    vector_name: &VectorName,
) -> SearchVector {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT source_schema_name,
                    source_table_name,
                    source_table_oid,
                    vector_column_name,
                    vector_attnum,
                    hnsw_index_oid,
                    metric
               FROM pgcontext._visible_collection_vectors
              WHERE collection_id = $1
                AND vector_name = $2",
            Some(1),
            &[collection_id.into(), vector_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query vector registration: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!(
                    "registered vector does not exist for collection {}: {}",
                    collection_name.as_str(),
                    vector_name.as_str()
                ),
            );
        }

        let row = rows.first();
        SearchVector {
            schema_name: super::spi_required_column::<String>(&row, 1, "source_schema_name"),
            table_name: super::spi_required_column::<String>(&row, 2, "source_table_name"),
            table_oid: super::spi_required_column::<pg_sys::Oid>(&row, 3, "source_table_oid"),
            vector_column_name: super::spi_required_column::<String>(&row, 4, "vector_column_name"),
            vector_attnum: super::spi_required_column::<i16>(&row, 5, "vector_attnum"),
            hnsw_index_oid: super::spi_optional_column::<pg_sys::Oid>(&row, 6, "hnsw_index_oid"),
            metric: crate::domain_types::distance_metric_from_catalog(
                super::spi_required_column::<String>(&row, 7, "metric"),
                "vector",
            ),
        }
    })
}

pub(super) fn vector_name_from_sql(vector_name: String) -> VectorName {
    match VectorName::new(vector_name) {
        Ok(vector_name) => vector_name,
        Err(error) => raise_core_error(error),
    }
}
