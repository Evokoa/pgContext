//! SQL-facing Qdrant-style payload mutation functions.

use context_core::{CollectionName, SourceKey};
use pgrx::JsonB;
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use serde_json::{Map, Value};

use crate::error::{raise_core_error, raise_sql_error};

#[derive(Debug, Clone)]
struct PayloadCollection {
    id: i64,
    owner_role: pg_sys::Oid,
    table_oid: pg_sys::Oid,
    schema_name: String,
    table_name: String,
}

#[derive(Debug, Clone)]
struct PayloadField {
    filter_key: String,
    column_name: String,
    jsonb_path: Option<Vec<String>>,
    data_type: PayloadColumnType,
    not_null: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PayloadColumnType {
    Text,
    Bool,
    I16,
    I32,
    I64,
    F32,
    F64,
    Numeric,
    Jsonb,
}

/// Merges registered payload keys into source-table payload columns.
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn set_payload(
    collection_name: String,
    source_keys: Vec<String>,
    payload: JsonB,
) -> TableIterator<'static, (name!(source_key, String), name!(updated, bool))> {
    let collection_name = collection_name_from_sql(collection_name);
    let source_keys = source_keys_from_sql(source_keys);
    let payload = payload_object(payload);
    let collection = resolve_payload_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    require_table_privilege(&collection, "UPDATE");
    let fields = resolve_payload_fields(&collection);
    let selected_fields = resolve_set_fields(&collection_name, &fields, &payload);
    validate_source_keys(&collection_name, &collection, &source_keys);

    for source_key in &source_keys {
        for field in &selected_fields {
            let value = payload.get(&field.filter_key).unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!(
                        "selected payload field is missing from validated payload: {}",
                        field.filter_key
                    ),
                )
            });
            set_payload_field(&collection, source_key, field, value);
        }
    }

    TableIterator::new(mutation_rows(source_keys))
}

/// Deletes registered payload keys from source-table payload columns.
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn delete_payload(
    collection_name: String,
    source_keys: Vec<String>,
    payload_keys: Vec<String>,
) -> TableIterator<'static, (name!(source_key, String), name!(updated, bool))> {
    let collection_name = collection_name_from_sql(collection_name);
    let source_keys = source_keys_from_sql(source_keys);
    let collection = resolve_payload_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    require_table_privilege(&collection, "UPDATE");
    let fields = resolve_payload_fields(&collection);
    let selected_fields = resolve_delete_fields(&collection_name, &fields, payload_keys);
    validate_source_keys(&collection_name, &collection, &source_keys);

    for source_key in &source_keys {
        for field in &selected_fields {
            delete_payload_field(&collection, source_key, field);
        }
    }

    TableIterator::new(mutation_rows(source_keys))
}

/// Clears all registered payload fields from source-table payload columns.
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn clear_payload(
    collection_name: String,
    source_keys: Vec<String>,
) -> TableIterator<'static, (name!(source_key, String), name!(updated, bool))> {
    let collection_name = collection_name_from_sql(collection_name);
    let source_keys = source_keys_from_sql(source_keys);
    let collection = resolve_payload_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    require_table_privilege(&collection, "UPDATE");
    let fields = resolve_payload_fields(&collection);
    let selected_fields = fields.iter().collect::<Vec<_>>();
    validate_delete_fields(&collection_name, &selected_fields);
    validate_source_keys(&collection_name, &collection, &source_keys);

    for source_key in &source_keys {
        for field in &selected_fields {
            delete_payload_field(&collection, source_key, field);
        }
    }

    TableIterator::new(mutation_rows(source_keys))
}

fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn source_keys_from_sql(source_keys: Vec<String>) -> Vec<SourceKey> {
    source_keys
        .into_iter()
        .map(|source_key| match SourceKey::new(source_key) {
            Ok(source_key) => source_key,
            Err(error) => raise_core_error(error),
        })
        .collect()
}

fn payload_object(payload: JsonB) -> Map<String, Value> {
    match payload.0 {
        Value::Object(object) => object,
        other => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("payload must be a JSON object: {other}"),
        ),
    }
}

fn resolve_payload_collection(collection_name: &CollectionName) -> PayloadCollection {
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
                format!("failed to query payload collection: {error}"),
            ),
        };
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {}", collection_name.as_str()),
            );
        }
        let row = rows.first();
        let table_oid = spi_optional_column::<pg_sys::Oid>(&row, 3, "source_table_oid");
        let schema_name = spi_optional_column::<String>(&row, 4, "source_schema_name");
        let table_name = spi_optional_column::<String>(&row, 5, "source_table_name");
        let (Some(table_oid), Some(schema_name), Some(table_name)) =
            (table_oid, schema_name, table_name)
        else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has no source table: {}",
                    collection_name.as_str()
                ),
            );
        };
        PayloadCollection {
            id: spi_required_column::<i64>(&row, 1, "collection_id"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 2, "owner_role"),
            table_oid,
            schema_name,
            table_name,
        }
    })
}

fn resolve_payload_fields(collection: &PayloadCollection) -> Vec<PayloadField> {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT payload.filter_key,
                    payload.column_name,
                    payload.jsonb_path,
                    attribute.atttypid,
                    attribute.attnotnull
               FROM pgcontext._collection_payload_columns AS payload
               JOIN pg_catalog.pg_attribute AS attribute
                 ON attribute.attrelid = payload.source_table_oid
                AND attribute.attnum = payload.column_attnum
              WHERE payload.collection_id = $1
                AND attribute.attnum > 0
                AND NOT attribute.attisdropped
              ORDER BY payload.filter_key",
            Some(i64::MAX),
            &[collection.id.into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query payload fields: {error}"),
            ),
        };
        let mut fields = Vec::new();
        for row in rows {
            let type_oid = row
                .get::<pg_sys::Oid>(4)
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read payload type oid: {error}"),
                    )
                })
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        "payload type is null",
                    )
                });
            fields.push(PayloadField {
                filter_key: row
                    .get::<String>(1)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read payload key: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "payload key is null",
                        )
                    }),
                column_name: row
                    .get::<String>(2)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read payload column: {error}"),
                        )
                    })
                    .unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "payload column is null",
                        )
                    }),
                jsonb_path: row.get::<Vec<String>>(3).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read payload JSONB path: {error}"),
                    )
                }),
                data_type: payload_column_type(type_oid),
                not_null: row
                    .get::<bool>(5)
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to read payload not-null flag: {error}"),
                        )
                    })
                    .unwrap_or(false),
            });
        }
        fields
    })
}

fn payload_column_type(type_oid: pg_sys::Oid) -> PayloadColumnType {
    match type_oid {
        pg_sys::TEXTOID | pg_sys::VARCHAROID | pg_sys::BPCHAROID => PayloadColumnType::Text,
        pg_sys::BOOLOID => PayloadColumnType::Bool,
        pg_sys::INT2OID => PayloadColumnType::I16,
        pg_sys::INT4OID => PayloadColumnType::I32,
        pg_sys::INT8OID => PayloadColumnType::I64,
        pg_sys::FLOAT4OID => PayloadColumnType::F32,
        pg_sys::FLOAT8OID => PayloadColumnType::F64,
        pg_sys::NUMERICOID => PayloadColumnType::Numeric,
        pg_sys::JSONBOID => PayloadColumnType::Jsonb,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            format!("registered payload column type cannot be mutated: {type_oid}"),
        ),
    }
}

fn resolve_set_fields<'a>(
    collection_name: &CollectionName,
    fields: &'a [PayloadField],
    payload: &Map<String, Value>,
) -> Vec<&'a PayloadField> {
    let mut selected = Vec::new();
    for key in payload.keys() {
        let Some(field) = fields.iter().find(|field| &field.filter_key == key) else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "unknown payload field for collection {}: {key}",
                    collection_name.as_str()
                ),
            );
        };
        selected.push(field);
    }
    selected
}

fn resolve_delete_fields<'a>(
    collection_name: &CollectionName,
    fields: &'a [PayloadField],
    payload_keys: Vec<String>,
) -> Vec<&'a PayloadField> {
    let mut selected = Vec::new();
    for key in payload_keys {
        let Some(field) = fields.iter().find(|field| field.filter_key == key) else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "unknown payload field for collection {}: {key}",
                    collection_name.as_str()
                ),
            );
        };
        selected.push(field);
    }
    validate_delete_fields(collection_name, &selected);
    selected
}

fn validate_delete_fields(collection_name: &CollectionName, fields: &[&PayloadField]) {
    for field in fields {
        if field.jsonb_path.is_none() && field.not_null {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NOT_NULL_VIOLATION,
                format!(
                    "payload field is not nullable for collection {}: {}",
                    collection_name.as_str(),
                    field.filter_key
                ),
            );
        }
    }
}

fn validate_source_keys(
    collection_name: &CollectionName,
    collection: &PayloadCollection,
    source_keys: &[SourceKey],
) {
    for source_key in source_keys {
        if !active_point_exists(collection, source_key)
            || !source_row_exists(collection, source_key)
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!(
                    "source key does not exist in collection {}: {}",
                    collection_name.as_str(),
                    source_key.as_str()
                ),
            );
        }
    }
}

fn active_point_exists(collection: &PayloadCollection, source_key: &SourceKey) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pgcontext._collection_points
              WHERE collection_id = $1
                AND source_key = $2
                AND deleted_at IS NULL
         )",
        &[collection.id.into(), source_key.as_str().into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check payload point mapping: {error}"),
        )
    })
    .unwrap_or(false)
}

fn source_row_exists(collection: &PayloadCollection, source_key: &SourceKey) -> bool {
    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let sql = format!("SELECT EXISTS (SELECT 1 FROM {table_name} WHERE id::text = $1)");
    Spi::get_one_with_args::<bool>(&sql, &[source_key.as_str().into()])
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to check payload source row: {error}"),
            )
        })
        .unwrap_or(false)
}

fn set_payload_field(
    collection: &PayloadCollection,
    source_key: &SourceKey,
    field: &PayloadField,
    value: &Value,
) {
    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let column_name = quote_identifier(&field.column_name);
    let sql = if field.jsonb_path.is_some() {
        format!(
            "UPDATE {table_name}
                SET {column_name} = pg_catalog.jsonb_set({column_name}, $1::text[], $2::jsonb, true)
              WHERE id::text = $3"
        )
    } else {
        format!(
            "UPDATE {table_name}
                SET {column_name} = {}
              WHERE id::text = $2",
            ordinary_set_expression(field.data_type)
        )
    };
    let json_value = JsonB(value.clone());
    let path = field.jsonb_path.as_ref();
    let args = match path {
        Some(path) => vec![
            path.clone().into(),
            json_value.into(),
            source_key.as_str().into(),
        ],
        None => vec![json_value.into(), source_key.as_str().into()],
    };
    run_payload_update(&sql, args, "set payload");
}

fn delete_payload_field(
    collection: &PayloadCollection,
    source_key: &SourceKey,
    field: &PayloadField,
) {
    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let column_name = quote_identifier(&field.column_name);
    let sql = if field.jsonb_path.is_some() {
        format!(
            "UPDATE {table_name}
                SET {column_name} = {column_name} #- $1::text[]
              WHERE id::text = $2"
        )
    } else {
        format!(
            "UPDATE {table_name}
                SET {column_name} = NULL
              WHERE id::text = $1"
        )
    };
    let args = match field.jsonb_path.as_ref() {
        Some(path) => vec![path.clone().into(), source_key.as_str().into()],
        None => vec![source_key.as_str().into()],
    };
    run_payload_update(&sql, args, "delete payload");
}

fn ordinary_set_expression(data_type: PayloadColumnType) -> &'static str {
    match data_type {
        PayloadColumnType::Text => "($1 #>> '{}')::text",
        PayloadColumnType::Bool => "($1 #>> '{}')::boolean",
        PayloadColumnType::I16 => "($1 #>> '{}')::smallint",
        PayloadColumnType::I32 => "($1 #>> '{}')::integer",
        PayloadColumnType::I64 => "($1 #>> '{}')::bigint",
        PayloadColumnType::F32 => "($1 #>> '{}')::real",
        PayloadColumnType::F64 => "($1 #>> '{}')::double precision",
        PayloadColumnType::Numeric => "($1 #>> '{}')::numeric",
        PayloadColumnType::Jsonb => "$1::jsonb",
    }
}

fn run_payload_update(sql: &str, args: Vec<DatumWithOid<'_>>, context: &'static str) {
    Spi::run_with_args(sql, args.as_slice()).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to {context}: {error}"),
        )
    });
}

fn mutation_rows(source_keys: Vec<SourceKey>) -> Vec<(String, bool)> {
    source_keys
        .into_iter()
        .map(|source_key| (source_key.into_string(), true))
        .collect()
}

fn require_collection_owner(collection: &PayloadCollection, collection_name: &CollectionName) {
    let session_user = session_user();
    let is_owner = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role($1, $2, 'MEMBER')",
        &[session_user.as_str().into(), collection.owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check payload collection owner: {error}"),
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

fn require_table_privilege(collection: &PayloadCollection, privilege: &'static str) {
    let has_privilege = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege(SESSION_USER, $1, $2)",
        &[collection.table_oid.into(), privilege.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check source-table {privilege}: {error}"),
        )
    })
    .unwrap_or(false);

    if !has_privilege {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for source table {}.{}",
                collection.schema_name, collection.table_name
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

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn quote_qualified_identifier(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_identifier(schema), quote_identifier(table))
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
            format!("payload mutation column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read payload mutation column {column_name}: {error}"),
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
            format!("failed to read payload mutation column {column_name}: {error}"),
        ),
    }
}
