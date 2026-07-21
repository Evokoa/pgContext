//! Catalog resolution and drift checks for hybrid SQL query paths.

use context_core::CollectionName;
use pgrx::{pg_sys, prelude::*};

use crate::error::raise_sql_error;

use super::{
    QueryCollection, QueryVector, SparseQueryVector, session_user, spi_optional_column,
    spi_required_column,
};

pub(super) fn resolve_collection(collection_name: &CollectionName) -> QueryCollection {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT acl.collection_id,
                    acl.owner_role,
                    acl.has_source_table,
                    (
                        SELECT count(*)::bigint
                          FROM pgcontext._visible_collection_points AS points
                         WHERE points.collection_id = acl.collection_id
                           AND points.deleted_at IS NULL
                    )
               FROM pgcontext._collection_acl AS acl
              WHERE acl.collection_name = $1",
            Some(1),
            &[collection_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query collection catalog: {error}"),
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

        QueryCollection {
            collection_id: spi_required_column::<i64>(&row, 1, "collection_id"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 2, "owner_role"),
            active_points: spi_required_column::<i64>(&row, 4, "active_points"),
        }
    })
}

pub(super) fn resolve_registered_vector(
    collection_name: &CollectionName,
    collection_id: i64,
) -> QueryVector {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT source_schema_name,
                    source_table_name,
                    source_table_oid,
                    vector_column_name,
                    vector_attnum,
                    metric
               FROM pgcontext._visible_collection_vectors
              WHERE collection_id = $1
              ORDER BY vector_id",
            Some(2),
            &[collection_id.into()],
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
                    "collection has no registered vector: {}",
                    collection_name.as_str()
                ),
            );
        }
        if rows.len() > 1 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has multiple registered vectors; specify a vector name: {}",
                    collection_name.as_str()
                ),
            );
        }

        let row = rows.first();
        QueryVector {
            schema_name: spi_required_column::<String>(&row, 1, "source_schema_name"),
            table_name: spi_required_column::<String>(&row, 2, "source_table_name"),
            table_oid: spi_required_column::<pg_sys::Oid>(&row, 3, "source_table_oid"),
            vector_column_name: spi_required_column::<String>(&row, 4, "vector_column_name"),
            vector_attnum: spi_required_column::<i16>(&row, 5, "vector_attnum"),
            metric: crate::domain_types::distance_metric_from_catalog(
                spi_required_column::<String>(&row, 6, "metric"),
                "vector",
            ),
        }
    })
}

pub(super) fn resolve_registered_sparse_vector(
    collection_name: &CollectionName,
    collection_id: i64,
    vector_name: &str,
) -> SparseQueryVector {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT source_schema_name,
                    source_table_name,
                    source_table_oid,
                    vector_name,
                    vector_column_name,
                    vector_attnum,
                    metric
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
        SparseQueryVector {
            schema_name: spi_required_column::<String>(&row, 1, "source_schema_name"),
            table_name: spi_required_column::<String>(&row, 2, "source_table_name"),
            table_oid: spi_required_column::<pg_sys::Oid>(&row, 3, "source_table_oid"),
            vector_name: spi_required_column::<String>(&row, 4, "vector_name"),
            vector_column_name: spi_required_column::<String>(&row, 5, "vector_column_name"),
            vector_attnum: spi_required_column::<i16>(&row, 6, "vector_attnum"),
            metric: crate::domain_types::distance_metric_from_catalog(
                spi_required_column::<String>(&row, 7, "metric"),
                "sparse vector",
            ),
        }
    })
}

pub(super) fn validate_query_vector_drift(collection_id: i64, registered_vector: &mut QueryVector) {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT class.oid,
                    vector_attribute.attnum,
                    vector_attribute.attname::text,
                    vector_attribute.atttypid = 'public.vector'::regtype AS vector_is_valid,
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
                format!("failed to validate dense+sparse query catalog drift: {error}"),
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
                    "registered vector column drifted: {}.{}",
                    registered_vector.table_name, registered_vector.vector_column_name
                ),
            );
        }

        let Some(current_vector_attnum) = current_vector_attnum else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "validated vector column had no attnum",
            );
        };
        refresh_restored_query_metadata(
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

pub(super) fn validate_query_drift(
    collection_id: i64,
    registered_vector: &mut QueryVector,
    text_column: &str,
) {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT class.oid,
                    vector_attribute.attnum,
                    vector_attribute.attname::text,
                    vector_attribute.atttypid = 'public.vector'::regtype AS vector_is_valid,
                    id_attribute.attname IS NOT NULL AS id_exists,
                    text_attribute.attname IS NOT NULL AS text_exists
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
               LEFT JOIN pg_catalog.pg_attribute AS text_attribute
                 ON text_attribute.attrelid = class.oid
                AND text_attribute.attname = $4
                AND text_attribute.attnum > 0
                AND NOT text_attribute.attisdropped
              WHERE namespace.nspname = $1
                AND class.relname = $2
                AND class.relkind IN ('r', 'p')",
            Some(1),
            &[
                registered_vector.schema_name.as_str().into(),
                registered_vector.table_name.as_str().into(),
                registered_vector.vector_column_name.as_str().into(),
                text_column.into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to validate hybrid query catalog drift: {error}"),
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
                    "registered vector column drifted: {}.{}",
                    registered_vector.table_name, registered_vector.vector_column_name
                ),
            );
        }

        let Some(current_vector_attnum) = current_vector_attnum else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "validated vector column had no attnum",
            );
        };
        refresh_restored_query_metadata(
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

        let text_exists = spi_required_column::<bool>(&row, 6, "text_exists");
        if !text_exists {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "query text column does not exist on {}.{}: {}",
                    registered_vector.schema_name, registered_vector.table_name, text_column
                ),
            );
        }
    });
}

pub(super) fn validate_sparse_query_drift(
    collection_id: i64,
    registered_vector: &mut SparseQueryVector,
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
                format!("failed to validate dense+sparse sparse-vector catalog drift: {error}"),
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
        refresh_restored_sparse_query_metadata(
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

fn refresh_restored_query_metadata(
    collection_id: i64,
    registered_vector: &mut QueryVector,
    current_table_oid: pg_sys::Oid,
    current_vector_attnum: i16,
) {
    if registered_vector.table_oid == current_table_oid
        && registered_vector.vector_attnum == current_vector_attnum
    {
        return;
    }

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
        "SELECT pgcontext._refresh_vector_source_binding($1, $2)",
        &[
            collection_id.into(),
            registered_vector.vector_column_name.as_str().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to refresh restored vector metadata: {error}"),
        )
    });

    registered_vector.table_oid = current_table_oid;
    registered_vector.vector_attnum = current_vector_attnum;
}

fn refresh_restored_sparse_query_metadata(
    collection_id: i64,
    registered_vector: &mut SparseQueryVector,
    current_table_oid: pg_sys::Oid,
    current_vector_attnum: i16,
) {
    if registered_vector.table_oid == current_table_oid
        && registered_vector.vector_attnum == current_vector_attnum
    {
        return;
    }

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

pub(super) fn require_collection_owner(
    collection: &QueryCollection,
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

pub(super) fn require_table_select_privilege(registered_vector: &QueryVector) {
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

pub(super) fn require_sparse_table_select_privilege(registered_vector: &SparseQueryVector) {
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

pub(super) fn point_id_to_sql(point_id: u64) -> i64 {
    match i64::try_from(point_id) {
        Ok(point_id) => point_id,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("fused point_id exceeds SQL bigint range: {point_id}"),
        ),
    }
}
