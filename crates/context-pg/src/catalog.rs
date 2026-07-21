//! SQL-facing collection catalog functions.

use context_core::{
    CollectionName, DistanceMetric, QualifiedTableName, SqlIdentifier, VectorDimensions, VectorName,
};
use context_filter::FieldRegistry;
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;

use crate::domain_types::{
    distance_metric_from_catalog, distance_metric_from_sql, distance_metric_label,
};
use crate::error::{raise_core_error, raise_sql_error};
#[derive(Debug, Clone)]
struct TableResolution {
    oid: pg_sys::Oid,
    schema_name: String,
    table_name: String,
}

#[derive(Debug, Clone)]
struct CollectionRecord {
    id: i64,
    name: String,
    owner_role: pg_sys::Oid,
    owner_name: String,
    source_table: Option<TableResolution>,
}

#[derive(Debug, Clone)]
struct VectorColumnResolution {
    attnum: i16,
    column_name: String,
}

#[derive(Debug, Clone)]
struct FilterColumnResolution {
    attnum: i16,
    column_name: String,
}

#[derive(Debug, Clone)]
struct RegisteredVector {
    collection_name: String,
    vector_name: String,
    table_schema: String,
    table_name: String,
    vector_column: String,
    dimensions: i32,
    metric: DistanceMetric,
}

/// Creates a named pgContext collection catalog entry.
///
/// # Errors
///
/// Raises `invalid_parameter_value` when `collection_name` is not a valid pgContext
/// collection name and `duplicate_object` when the collection already exists.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn create_collection(
    collection_name: String,
) -> TableIterator<
    'static,
    (
        name!(collection_id, i64),
        name!(collection_name, String),
        name!(owner_name, String),
        name!(table_schema, Option<String>),
        name!(table_name, Option<String>),
    ),
> {
    create_collection_inner(collection_name, None)
}

/// Creates a collection associated with an existing ordinary table.
///
/// `table_name` must use `schema.table` form. pgContext records table metadata
/// only; it does not copy or own the source table.
///
/// # Errors
///
/// Raises `invalid_parameter_value` when identifiers are not valid, `undefined_table` when
/// the source table cannot be resolved, and `duplicate_object` when the
/// collection already exists.
#[pg_extern(schema = "pgcontext", name = "create_collection", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn create_collection_for_table(
    collection_name: String,
    table_name: String,
) -> TableIterator<
    'static,
    (
        name!(collection_id, i64),
        name!(collection_name, String),
        name!(owner_name, String),
        name!(table_schema, Option<String>),
        name!(table_name, Option<String>),
    ),
> {
    let table_name = qualified_table_name_from_sql(&table_name);
    let table = resolve_table(&table_name);
    require_table_select_privilege(&table);
    create_collection_inner(collection_name, Some(table))
}

/// Returns metadata for one pgContext collection.
///
/// # Errors
///
/// Raises `invalid_parameter_value` when `collection_name` is not valid and
/// `undefined_object` when no matching collection exists.
#[pg_extern(schema = "pgcontext", stable, security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn collection_info(
    collection_name: String,
) -> TableIterator<
    'static,
    (
        name!(collection_id, i64),
        name!(collection_name, String),
        name!(owner_name, String),
        name!(table_schema, Option<String>),
        name!(table_name, Option<String>),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let row = match find_collection(&collection_name) {
        Some(row) => collection_row(row),
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("collection does not exist: {}", collection_name.as_str()),
        ),
    };

    TableIterator::once(row)
}

/// Registers a dense vector column for a table-backed pgContext collection.
///
/// The collection must have been created with a source table. Registration
/// validates that the named column currently exists on that table and has the
/// pgContext `vector` type.
///
/// # Errors
///
/// Raises `invalid_parameter_value` for malformed names, `undefined_object` for unknown
/// collections, `undefined_column` for missing columns, `datatype_mismatch` for
/// non-vector columns, `invalid_parameter_value` for invalid dimensions, and
/// `duplicate_object` when a vector name or column is already registered.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn register_vector(
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
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let vector_name = vector_name_from_sql(vector_name);
    let vector_column = sql_identifier_from_sql(vector_column);
    let dimensions = dimensions_from_sql(dimensions);
    let metric = distance_metric_from_sql(&metric, "");

    let collection = match find_collection(&collection_name) {
        Some(collection) => collection,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("collection does not exist: {}", collection_name.as_str()),
        ),
    };
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
    let vector_column = resolve_vector_column(source_table, &vector_column);
    crate::collection_limits::enforce_vector_registration_limits(
        collection.id,
        &collection_name,
        dimensions.get(),
    );
    let row = match insert_vector_registration(
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
                "vector registration already exists for collection {}: {}",
                collection_name.as_str(),
                vector_name.as_str()
            ),
        ),
    };

    TableIterator::once(vector_registration_row(row))
}

/// Registers a source-table column as a filter and facet field.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn register_filter_column(
    collection_name: String,
    filter_key: String,
    column_name: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(filter_key, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(column_name, String),
    ),
> {
    validate_filter_column_registration(&filter_key, &column_name);
    let collection_name = collection_name_from_sql(collection_name);
    let column_name = sql_identifier_from_sql(column_name);

    let collection = match find_collection(&collection_name) {
        Some(collection) => collection,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("collection does not exist: {}", collection_name.as_str()),
        ),
    };
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
    let column = resolve_filter_column(source_table, &column_name);
    let row =
        match insert_filter_column_registration(&collection, &filter_key, source_table, &column) {
            Some(row) => row,
            None => raise_sql_error(
                PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
                format!(
                    "filter column already registered for collection {}: {}",
                    collection_name.as_str(),
                    filter_key
                ),
            ),
        };

    TableIterator::once(row)
}

/// Drops a pgContext collection catalog entry.
///
/// Returns `true` when an entry was deleted and `false` when the collection did
/// not exist. Dependent vector registrations are deleted with the collection.
///
/// # Errors
///
/// Raises `invalid_parameter_value` when `collection_name` is not valid.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn drop_collection(collection_name: String) -> bool {
    let collection_name = collection_name_from_sql(collection_name);
    let Some(collection) = find_collection(&collection_name) else {
        return false;
    };
    require_collection_owner(&collection);

    match Spi::get_one_with_args::<bool>(
        "WITH deleted AS (
             DELETE FROM pgcontext._collections
              WHERE collection_id = $1
              RETURNING 1
         )
         SELECT EXISTS (SELECT 1 FROM deleted)",
        &[collection.id.into()],
    ) {
        Ok(Some(dropped)) => dropped,
        Ok(None) => false,
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to drop collection: {error}"),
        ),
    }
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx name!() table shape is shared with exported collection functions"
)]
fn create_collection_inner(
    collection_name: String,
    table: Option<TableResolution>,
) -> TableIterator<
    'static,
    (
        name!(collection_id, i64),
        name!(collection_name, String),
        name!(owner_name, String),
        name!(table_schema, Option<String>),
        name!(table_name, Option<String>),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    crate::collection_aliases::reject_existing_alias_name(&collection_name);
    let row = match insert_collection(&collection_name, table.as_ref()) {
        Some(row) => collection_row(row),
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            format!("collection already exists: {}", collection_name.as_str()),
        ),
    };

    TableIterator::once(row)
}

fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn vector_name_from_sql(vector_name: String) -> VectorName {
    match VectorName::new(vector_name) {
        Ok(vector_name) => vector_name,
        Err(error) => raise_core_error(error),
    }
}

fn sql_identifier_from_sql(identifier: String) -> SqlIdentifier {
    match SqlIdentifier::new(identifier) {
        Ok(identifier) => identifier,
        Err(error) => raise_core_error(error),
    }
}

fn qualified_table_name_from_sql(table_name: &str) -> QualifiedTableName {
    match QualifiedTableName::new(table_name) {
        Ok(table_name) => table_name,
        Err(error) => raise_core_error(error),
    }
}

fn dimensions_from_sql(dimensions: i32) -> VectorDimensions {
    let dimensions = match usize::try_from(dimensions) {
        Ok(dimensions) => dimensions,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid vector dimensions: {dimensions}"),
        ),
    };
    match VectorDimensions::new(dimensions) {
        Ok(dimensions) => dimensions,
        Err(error) => raise_core_error(error),
    }
}

fn resolve_table(table_name: &QualifiedTableName) -> TableResolution {
    let qualified_name = table_name.as_qualified_name();
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT class.oid,
                    namespace.nspname::text,
                    class.relname::text
               FROM pg_catalog.pg_class AS class
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
              WHERE class.oid = pg_catalog.to_regclass($1)
                AND class.relkind IN ('r', 'p')",
            Some(1),
            &[qualified_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                format!("failed to resolve source table {qualified_name}: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                format!("source table does not exist: {qualified_name}"),
            );
        }

        let row = rows.first();
        TableResolution {
            oid: spi_required_column::<pg_sys::Oid>(&row, 1, "source_table_oid"),
            schema_name: spi_required_column::<String>(&row, 2, "source_schema_name"),
            table_name: spi_required_column::<String>(&row, 3, "source_table_name"),
        }
    })
}

fn resolve_vector_column(
    table: &TableResolution,
    column: &SqlIdentifier,
) -> VectorColumnResolution {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT attribute.attnum,
                    attribute.attname::text,
                    attribute.atttypid = 'public.vector'::regtype AS is_vector,
                    pg_catalog.format_type(attribute.atttypid, attribute.atttypmod)
               FROM pg_catalog.pg_attribute AS attribute
              WHERE attribute.attrelid = $1
                AND attribute.attname = $2
                AND attribute.attnum > 0
                AND NOT attribute.attisdropped",
            Some(1),
            &[table.oid.into(), column.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to inspect vector column: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "vector column does not exist on {}.{}: {}",
                    table.schema_name,
                    table.table_name,
                    column.as_str()
                ),
            );
        }

        let row = rows.first();
        let is_vector = spi_required_column::<bool>(&row, 3, "is_vector");
        if !is_vector {
            let data_type = spi_required_column::<String>(&row, 4, "data_type");
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                format!(
                    "vector column must have type vector: {}.{} is {data_type}",
                    table.table_name,
                    column.as_str()
                ),
            );
        }

        VectorColumnResolution {
            attnum: spi_required_column::<i16>(&row, 1, "vector_attnum"),
            column_name: spi_required_column::<String>(&row, 2, "vector_column_name"),
        }
    })
}

fn validate_filter_column_registration(filter_key: &str, column_name: &str) {
    if let Err(error) = FieldRegistry::builder().register_column(filter_key, column_name) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        );
    }
}

fn resolve_filter_column(
    table: &TableResolution,
    column: &SqlIdentifier,
) -> FilterColumnResolution {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT attribute.attnum,
                    attribute.attname::text
               FROM pg_catalog.pg_attribute AS attribute
              WHERE attribute.attrelid = $1
                AND attribute.attname = $2
                AND attribute.attnum > 0
                AND NOT attribute.attisdropped",
            Some(1),
            &[table.oid.into(), column.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to inspect filter column: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "filter column does not exist on {}.{}: {}",
                    table.schema_name,
                    table.table_name,
                    column.as_str()
                ),
            );
        }

        let row = rows.first();
        FilterColumnResolution {
            attnum: spi_required_column::<i16>(&row, 1, "filter_attnum"),
            column_name: spi_required_column::<String>(&row, 2, "filter_column_name"),
        }
    })
}

fn insert_collection(
    collection_name: &CollectionName,
    table: Option<&TableResolution>,
) -> Option<CollectionRecord> {
    let source_table_oid = table.map(|table| table.oid);
    let source_schema_name = table.map(|table| table.schema_name.as_str());
    let source_table_name = table.map(|table| table.table_name.as_str());

    select_collection_row(
        "WITH inserted AS (
             INSERT INTO pgcontext._collections (
                 collection_name,
                 owner_role,
                 source_table_oid,
                 source_schema_name,
                 source_table_name
             )
             SELECT $1, role.oid, $2, $3, $4
               FROM pg_catalog.pg_roles AS role
              WHERE role.rolname = SESSION_USER
             ON CONFLICT (collection_name) DO NOTHING
             RETURNING collection_id,
                       collection_name,
                       owner_role,
                       source_table_oid,
                       source_schema_name,
                       source_table_name
         )
         SELECT inserted.collection_id,
                inserted.collection_name,
                inserted.owner_role,
                role.rolname::text,
                inserted.source_table_oid,
                inserted.source_schema_name,
                inserted.source_table_name
           FROM inserted
           JOIN pg_catalog.pg_roles AS role ON role.oid = inserted.owner_role",
        &[
            collection_name.as_str().into(),
            nullable_oid(source_table_oid),
            nullable_text(source_schema_name),
            nullable_text(source_table_name),
        ],
        true,
    )
}

fn find_collection(collection_name: &CollectionName) -> Option<CollectionRecord> {
    select_collection_row(
        "SELECT collection.collection_id,
                collection.collection_name,
                collection.owner_role,
                role.rolname::text,
                collection.source_table_oid,
                collection.source_schema_name,
                collection.source_table_name
           FROM pgcontext._collections AS collection
           JOIN pg_catalog.pg_roles AS role ON role.oid = collection.owner_role
          WHERE collection.collection_name = $1",
        &[collection_name.as_str().into()],
        false,
    )
}

fn select_collection_row(
    sql: &str,
    args: &[DatumWithOid<'_>],
    mutating: bool,
) -> Option<CollectionRecord> {
    Spi::connect_mut(|client| {
        let rows = if mutating {
            client.update(sql, Some(1), args)
        } else {
            client.select(sql, Some(1), args)
        };

        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query collection catalog: {error}"),
            ),
        };

        if rows.is_empty() {
            return None;
        }

        let row = rows.first();
        let source_table_oid = spi_optional_column::<pg_sys::Oid>(&row, 5, "source_table_oid");
        let source_schema_name = spi_optional_column::<String>(&row, 6, "source_schema_name");
        let source_table_name = spi_optional_column::<String>(&row, 7, "source_table_name");
        let source_table = match (source_table_oid, source_schema_name, source_table_name) {
            (Some(oid), Some(schema_name), Some(table_name)) => Some(TableResolution {
                oid,
                schema_name,
                table_name,
            }),
            (None, None, None) => None,
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "collection catalog has partial source table metadata",
            ),
        };

        Some(CollectionRecord {
            id: spi_required_column::<i64>(&row, 1, "collection_id"),
            name: spi_required_column::<String>(&row, 2, "collection_name"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 3, "owner_role"),
            owner_name: spi_required_column::<String>(&row, 4, "owner_name"),
            source_table,
        })
    })
}

fn insert_filter_column_registration(
    collection: &CollectionRecord,
    filter_key: &str,
    table: &TableResolution,
    column: &FilterColumnResolution,
) -> Option<(String, String, String, String, String)> {
    Spi::connect_mut(|client| {
        let rows = match client.update(
            "WITH inserted AS (
                 INSERT INTO pgcontext._collection_payload_columns (
                     collection_id,
                     filter_key,
                     source_table_oid,
                     source_schema_name,
                     source_table_name,
                     column_name,
                     column_attnum
                 )
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT DO NOTHING
                 RETURNING filter_key,
                           source_schema_name,
                           source_table_name,
                           column_name
             )
             SELECT filter_key,
                    source_schema_name,
                    source_table_name,
                    column_name
               FROM inserted",
            Some(1),
            &[
                collection.id.into(),
                filter_key.into(),
                table.oid.into(),
                table.schema_name.as_str().into(),
                table.table_name.as_str().into(),
                column.column_name.as_str().into(),
                column.attnum.into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to register filter column: {error}"),
            ),
        };

        if rows.is_empty() {
            return None;
        }

        let row = rows.first();
        Some((
            collection.name.clone(),
            spi_required_column::<String>(&row, 1, "filter_key"),
            spi_required_column::<String>(&row, 2, "source_schema_name"),
            spi_required_column::<String>(&row, 3, "source_table_name"),
            spi_required_column::<String>(&row, 4, "column_name"),
        ))
    })
}

fn require_collection_owner(collection: &CollectionRecord) {
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
                "permission denied for collection {}: owner is {}",
                collection.name, collection.owner_name
            ),
        );
    }
}

fn require_table_select_privilege(table: &TableResolution) {
    let session_user = session_user();
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT')",
        &[session_user.as_str().into(), table.oid.into()],
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
                table.schema_name, table.table_name
            ),
        );
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

fn insert_vector_registration(
    collection: &CollectionRecord,
    vector_name: &VectorName,
    table: &TableResolution,
    column: &VectorColumnResolution,
    dimensions: VectorDimensions,
    metric: DistanceMetric,
) -> Option<RegisteredVector> {
    Spi::connect_mut(|client| {
        let rows = match client.update(
            "WITH inserted AS (
                 INSERT INTO pgcontext._collection_vectors (
                     collection_id,
                     vector_name,
                     source_table_oid,
                     source_schema_name,
                     source_table_name,
                     vector_column_name,
                     vector_attnum,
                     dimensions,
                     metric
                 )
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                 ON CONFLICT DO NOTHING
                 RETURNING vector_name,
                           source_schema_name,
                           source_table_name,
                           vector_column_name,
                           dimensions,
                           metric
             )
             SELECT vector_name,
                    source_schema_name,
                    source_table_name,
                    vector_column_name,
                    dimensions,
                    metric
               FROM inserted",
            Some(1),
            &[
                collection.id.into(),
                vector_name.as_str().into(),
                table.oid.into(),
                table.schema_name.as_str().into(),
                table.table_name.as_str().into(),
                column.column_name.as_str().into(),
                column.attnum.into(),
                i32::try_from(dimensions.get()).unwrap_or(i32::MAX).into(),
                distance_metric_label(metric).into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to register vector: {error}"),
            ),
        };

        if rows.is_empty() {
            return None;
        }

        let row = rows.first();
        Some(RegisteredVector {
            collection_name: collection.name.clone(),
            vector_name: spi_required_column::<String>(&row, 1, "vector_name"),
            table_schema: spi_required_column::<String>(&row, 2, "source_schema_name"),
            table_name: spi_required_column::<String>(&row, 3, "source_table_name"),
            vector_column: spi_required_column::<String>(&row, 4, "vector_column_name"),
            dimensions: spi_required_column::<i32>(&row, 5, "dimensions"),
            metric: distance_metric_from_catalog(
                spi_required_column::<String>(&row, 6, "metric"),
                "vector",
            ),
        })
    })
}

fn collection_row(row: CollectionRecord) -> (i64, String, String, Option<String>, Option<String>) {
    let (table_schema, table_name) = match row.source_table {
        Some(table) => (Some(table.schema_name), Some(table.table_name)),
        None => (None, None),
    };
    (row.id, row.name, row.owner_name, table_schema, table_name)
}

fn vector_registration_row(
    row: RegisteredVector,
) -> (String, String, String, String, String, i32, String) {
    (
        row.collection_name,
        row.vector_name,
        row.table_schema,
        row.table_name,
        row.vector_column,
        row.dimensions,
        distance_metric_label(row.metric).to_owned(),
    )
}

fn nullable_oid(value: Option<pg_sys::Oid>) -> DatumWithOid<'static> {
    match value {
        Some(value) => value.into(),
        None => DatumWithOid::null_oid(pg_sys::OIDOID),
    }
}

fn nullable_text(value: Option<&str>) -> DatumWithOid<'_> {
    match value {
        Some(value) => value.into(),
        None => DatumWithOid::null_oid(pg_sys::TEXTOID),
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
            format!("collection catalog column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read collection catalog column {column_name}: {error}"),
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
            format!("failed to read collection catalog column {column_name}: {error}"),
        ),
    }
}
