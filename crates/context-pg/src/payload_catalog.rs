//! SQL-facing payload field registration functions.

use context_core::{CollectionName, SqlIdentifier};
use context_filter::{FieldRegistry, JsonbPath};
use pgrx::prelude::*;

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
    owner_role: pg_sys::Oid,
    source_table: Option<TableResolution>,
}

#[derive(Debug, Clone)]
struct JsonbColumnResolution {
    column_name: String,
    attnum: i16,
}

/// Registers a JSONB path as a filter and facet field.
///
/// # Errors
///
/// Raises `undefined_object` when the collection is missing,
/// `undefined_column` when the JSONB column is missing,
/// `datatype_mismatch` when the column is not `jsonb`,
/// `insufficient_privilege` when the caller does not own the collection or
/// lacks source-table `SELECT`, and `duplicate_object` when the filter key or
/// source column registration already exists.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn register_jsonb_path(
    collection_name: String,
    filter_key: String,
    column_name: String,
    path: Vec<String>,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(filter_key, String),
        name!(table_schema, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(jsonb_path, Vec<String>),
    ),
> {
    validate_jsonb_registration(&filter_key, &column_name, path.clone());
    let collection_name = collection_name_from_sql(collection_name);
    let column_name = sql_identifier_from_sql(column_name);
    let path = jsonb_path_from_sql(path);

    let collection = match find_collection(&collection_name) {
        Some(collection) => collection,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("collection does not exist: {}", collection_name.as_str()),
        ),
    };
    require_collection_owner(&collection, &collection_name);
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
    let column = resolve_jsonb_column(source_table, &column_name);
    let row = match insert_jsonb_path_registration(
        &collection,
        &collection_name,
        &filter_key,
        source_table,
        &column,
        path.segments(),
    ) {
        Some(row) => row,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            format!(
                "JSONB path already registered for collection {}: {}",
                collection_name.as_str(),
                filter_key
            ),
        ),
    };

    TableIterator::once(row)
}

fn validate_jsonb_registration(filter_key: &str, column_name: &str, path: Vec<String>) {
    if let Err(error) = FieldRegistry::builder().register_jsonb_path(filter_key, column_name, path)
    {
        raise_filter_error(error);
    }
}

fn raise_filter_error(error: context_filter::FilterError) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        error.to_string(),
    )
}

fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn sql_identifier_from_sql(identifier: String) -> SqlIdentifier {
    match SqlIdentifier::new(identifier) {
        Ok(identifier) => identifier,
        Err(error) => raise_core_error(error),
    }
}

fn jsonb_path_from_sql(path: Vec<String>) -> JsonbPath {
    match JsonbPath::new(path) {
        Ok(path) => path,
        Err(error) => raise_filter_error(error),
    }
}

fn find_collection(collection_name: &CollectionName) -> Option<CollectionRecord> {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT collection_id,
                    owner_role,
                    source_table_oid,
                    source_schema_name,
                    source_table_name
               FROM pgcontext._collections
              WHERE collection_name = $1",
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
            return None;
        }

        let row = rows.first();
        let source_table_oid = spi_optional_column::<pg_sys::Oid>(&row, 3, "source_table_oid");
        let source_schema_name = spi_optional_column::<String>(&row, 4, "source_schema_name");
        let source_table_name = spi_optional_column::<String>(&row, 5, "source_table_name");
        let source_table = match (source_table_oid, source_schema_name, source_table_name) {
            (Some(oid), Some(schema_name), Some(table_name)) => Some(TableResolution {
                oid,
                schema_name,
                table_name,
            }),
            _ => None,
        };

        Some(CollectionRecord {
            id: spi_required_column::<i64>(&row, 1, "collection_id"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 2, "owner_role"),
            source_table,
        })
    })
}

fn resolve_jsonb_column(
    table: &TableResolution,
    column_name: &SqlIdentifier,
) -> JsonbColumnResolution {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT attribute.attname::text,
                    attribute.attnum,
                    attribute.atttypid = 'jsonb'::regtype AS is_jsonb
               FROM pg_catalog.pg_attribute AS attribute
              WHERE attribute.attrelid = $1
                AND attribute.attname = $2
                AND attribute.attnum > 0
                AND NOT attribute.attisdropped",
            Some(1),
            &[table.oid.into(), column_name.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to resolve JSONB column: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "JSONB column does not exist on {}.{}: {}",
                    table.schema_name,
                    table.table_name,
                    column_name.as_str()
                ),
            );
        }

        let row = rows.first();
        let is_jsonb = spi_required_column::<bool>(&row, 3, "is_jsonb");
        if !is_jsonb {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                format!(
                    "column is not jsonb on {}.{}: {}",
                    table.schema_name,
                    table.table_name,
                    column_name.as_str()
                ),
            );
        }

        JsonbColumnResolution {
            column_name: spi_required_column::<String>(&row, 1, "column_name"),
            attnum: spi_required_column::<i16>(&row, 2, "attnum"),
        }
    })
}

fn insert_jsonb_path_registration(
    collection: &CollectionRecord,
    collection_name: &CollectionName,
    filter_key: &str,
    table: &TableResolution,
    column: &JsonbColumnResolution,
    path: &[String],
) -> Option<(String, String, String, String, String, Vec<String>)> {
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
                     column_attnum,
                     jsonb_path
                 )
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text[])
                 ON CONFLICT DO NOTHING
                 RETURNING filter_key,
                           source_schema_name,
                           source_table_name,
                           column_name,
                           jsonb_path
             )
             SELECT filter_key,
                    source_schema_name,
                    source_table_name,
                    column_name,
                    jsonb_path
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
                path.to_vec().into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to register JSONB path: {error}"),
            ),
        };

        if rows.is_empty() {
            return None;
        }

        let row = rows.first();
        Some((
            collection_name.as_str().to_owned(),
            spi_required_column::<String>(&row, 1, "filter_key"),
            spi_required_column::<String>(&row, 2, "source_schema_name"),
            spi_required_column::<String>(&row, 3, "source_table_name"),
            spi_required_column::<String>(&row, 4, "column_name"),
            spi_required_column::<Vec<String>>(&row, 5, "jsonb_path"),
        ))
    })
}

fn require_collection_owner(collection: &CollectionRecord, collection_name: &CollectionName) {
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
            format!("payload catalog column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read payload catalog column {column_name}: {error}"),
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
            format!("failed to read payload catalog column {column_name}: {error}"),
        ),
    }
}
