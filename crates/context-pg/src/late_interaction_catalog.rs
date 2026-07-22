//! Registration and materialization for pgContext-owned late-interaction tokens.

use context_core::{CollectionName, QualifiedTableName, SqlIdentifier};
use pgrx::prelude::*;

use crate::{
    error::{raise_core_error, raise_sql_error},
    vector::Vector,
};

const REGISTRATION_BATCH_SIZE: i64 = 512;
const MAX_TOKENS_PER_POINT: usize = 16_384;

#[derive(Debug, Clone)]
struct LateInteractionRegistrationSource {
    collection_id: i64,
    owner_role: pg_sys::Oid,
    table_oid: pg_sys::Oid,
    schema_name: String,
    table_name: String,
    token_column_name: String,
    token_attnum: i16,
}

#[derive(Debug)]
struct SourceTokenRow {
    source_key: String,
    token_vectors: Vec<Vector>,
}

/// Registers and materializes an internally maintained late-interaction source.
///
/// The collection must already be bound to `source_table`. `token_source` must
/// name a non-null `vector[]` column on that table. Source rows are read by this
/// SECURITY INVOKER function, so ordinary table ACLs and RLS apply to the
/// initial materialization. Catalog writes and trigger installation are routed
/// through narrowly validated SECURITY DEFINER helpers.
#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn register_late_interaction(
    collection: String,
    source_table: String,
    token_source: String,
) -> TableIterator<
    'static,
    (
        name!(collection, String),
        name!(source_table, String),
        name!(token_source, String),
        name!(dimensions, Option<i32>),
        name!(point_count, i64),
        name!(token_count, i64),
        name!(status, String),
    ),
> {
    let collection = collection_name_from_sql(collection);
    let source_table = qualified_table_name_from_sql(&source_table);
    let token_source = sql_identifier_from_sql(&token_source);
    let source = resolve_registration_source(&collection, &source_table, &token_source);
    require_registration_owner(&source, &collection);
    require_registration_source_select(&source);
    reject_existing_registration(source.collection_id, &collection);

    begin_registration(&source);
    let (dimensions, point_count, token_count) = materialize_source_tokens(&source);
    let status = match dimensions {
        Some(dimensions) => {
            finish_registration(source.collection_id, dimensions);
            "ready"
        }
        None => "building",
    };

    TableIterator::once((
        collection.as_str().to_owned(),
        format!("{}.{}", source.schema_name, source.table_name),
        source.token_column_name,
        dimensions,
        point_count,
        token_count,
        status.to_owned(),
    ))
}

fn resolve_registration_source(
    collection: &CollectionName,
    source_table: &QualifiedTableName,
    token_source: &SqlIdentifier,
) -> LateInteractionRegistrationSource {
    let schema_name = source_table.schema().as_str();
    let table_name = source_table.table().as_str();
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT collections.collection_id,
                        collections.owner_role,
                        source_class.oid,
                        source_namespace.nspname::text,
                        source_class.relname::text,
                        token_attribute.attnum,
                        token_attribute.atttypid = 'public.vector[]'::regtype AS token_type_valid,
                        token_attribute.attnotnull,
                        id_attribute.attname IS NOT NULL AS id_exists
                   FROM pgcontext._collection_acl AS collections
                   JOIN pgcontext._visible_collections AS visible_collections
                     USING (collection_id)
                   JOIN pg_catalog.pg_class AS source_class
                     ON source_class.oid = visible_collections.source_table_oid
                   JOIN pg_catalog.pg_namespace AS source_namespace
                     ON source_namespace.oid = source_class.relnamespace
                   LEFT JOIN pg_catalog.pg_attribute AS token_attribute
                     ON token_attribute.attrelid = source_class.oid
                    AND token_attribute.attname = $4
                    AND token_attribute.attnum > 0
                    AND NOT token_attribute.attisdropped
                   LEFT JOIN pg_catalog.pg_attribute AS id_attribute
                     ON id_attribute.attrelid = source_class.oid
                    AND id_attribute.attname = 'id'
                    AND id_attribute.attnum > 0
                    AND NOT id_attribute.attisdropped
                  WHERE collections.collection_name = $1
                    AND source_namespace.nspname = $2
                    AND source_class.relname = $3
                    AND source_class.relkind IN ('r', 'p')",
                Some(1),
                &[
                    collection.as_str().into(),
                    schema_name.into(),
                    table_name.into(),
                    token_source.as_str().into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to resolve late-interaction source: {error}"),
                )
            });

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!(
                    "collection source table does not match registration: {} -> {}.{}",
                    collection.as_str(),
                    schema_name,
                    table_name
                ),
            );
        }
        let row = rows.first();
        let token_attnum = spi_optional_column::<i16>(&row, 6).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction token source column does not exist: {}.{}.{}",
                    schema_name,
                    table_name,
                    token_source.as_str()
                ),
            )
        });
        if spi_optional_column::<bool>(&row, 7) != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                format!(
                    "late-interaction token source must have type vector[]: {}.{}.{}",
                    schema_name,
                    table_name,
                    token_source.as_str()
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 8) != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction token source must be NOT NULL: {}.{}.{}",
                    schema_name,
                    table_name,
                    token_source.as_str()
                ),
            );
        }
        if !spi_required_column::<bool>(&row, 9, "source_id_exists") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction source key column does not exist: {schema_name}.{table_name}.id"
                ),
            );
        }

        LateInteractionRegistrationSource {
            collection_id: spi_required_column(&row, 1, "collection_id"),
            owner_role: spi_required_column(&row, 2, "owner_role"),
            table_oid: spi_required_column(&row, 3, "source_table_oid"),
            schema_name: spi_required_column(&row, 4, "source_schema_name"),
            table_name: spi_required_column(&row, 5, "source_table_name"),
            token_column_name: token_source.as_str().to_owned(),
            token_attnum,
        }
    })
}

fn require_registration_owner(
    source: &LateInteractionRegistrationSource,
    collection: &CollectionName,
) {
    let is_member = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, $1, 'MEMBER')",
        &[source.owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check late-interaction collection ownership: {error}"),
        )
    })
    .unwrap_or(false);
    if !is_member {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for late-interaction collection: {}",
                collection.as_str()
            ),
        );
    }
}

fn require_registration_source_select(source: &LateInteractionRegistrationSource) {
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege(SESSION_USER, $1, 'SELECT')",
        &[source.table_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check late-interaction source privileges: {error}"),
        )
    })
    .unwrap_or(false);
    if !has_select {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for late-interaction source table: {}.{}",
                source.schema_name, source.table_name
            ),
        );
    }
}

fn reject_existing_registration(collection_id: i64, collection: &CollectionName) {
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pgcontext._visible_collection_late_interaction
              WHERE collection_id = $1
         )",
        &[collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check late-interaction registration: {error}"),
        )
    })
    .unwrap_or(false);
    if exists {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            format!(
                "late-interaction registration already exists: {}",
                collection.as_str()
            ),
        );
    }
}

fn begin_registration(source: &LateInteractionRegistrationSource) {
    Spi::run_with_args(
        "SELECT pgcontext._begin_late_interaction_registration($1, $2, $3, $4, $5, $6)",
        &[
            source.collection_id.into(),
            source.table_oid.into(),
            source.schema_name.as_str().into(),
            source.table_name.as_str().into(),
            source.token_column_name.as_str().into(),
            source.token_attnum.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to begin late-interaction registration: {error}"),
        )
    });
}

fn materialize_source_tokens(
    source: &LateInteractionRegistrationSource,
) -> (Option<i32>, i64, i64) {
    let table_name = quote_qualified_identifier(&source.schema_name, &source.table_name);
    let token_column = quote_identifier(&source.token_column_name);
    let sql = format!(
        "SELECT id::text, {token_column}
           FROM {table_name}
          ORDER BY id::text
          LIMIT $1
         OFFSET $2"
    );
    let mut offset = 0_i64;
    let mut dimensions = None;
    let mut point_count = 0_i64;
    let mut token_count = 0_i64;

    loop {
        let batch = load_source_token_batch(&sql, offset);
        if batch.is_empty() {
            break;
        }
        for row in batch {
            let row_dimensions = validate_source_token_row(&row);
            match dimensions {
                Some(expected) if expected != row_dimensions => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!(
                        "late-interaction token dimension mismatch: expected {expected}, found {row_dimensions} for source key {}",
                        row.source_key
                    ),
                ),
                None => dimensions = Some(row_dimensions),
                Some(_) => {}
            }
            store_source_token_row(source.collection_id, &row);
            point_count = point_count.checked_add(1).unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "late-interaction point count overflow",
                )
            });
            let row_tokens = i64::try_from(row.token_vectors.len()).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "late-interaction token count exceeds bigint range",
                )
            });
            token_count = token_count.checked_add(row_tokens).unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "late-interaction token count overflow",
                )
            });
        }
        offset = point_count;
    }

    (dimensions, point_count, token_count)
}

fn load_source_token_batch(sql: &str, offset: i64) -> Vec<SourceTokenRow> {
    Spi::connect(|client| {
        let rows = client
            .select(sql, None, &[REGISTRATION_BATCH_SIZE.into(), offset.into()])
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to scan late-interaction source rows: {error}"),
                )
            });
        rows.into_iter()
            .map(|row| SourceTokenRow {
                source_key: spi_iter_required_column(&row, 1, "source_key"),
                token_vectors: spi_iter_required_column(&row, 2, "token_vectors"),
            })
            .collect()
    })
}

fn validate_source_token_row(row: &SourceTokenRow) -> i32 {
    if row.token_vectors.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "late-interaction token source must contain at least one vector for source key {}",
                row.source_key
            ),
        );
    }
    if row.token_vectors.len() > MAX_TOKENS_PER_POINT {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "late-interaction token count exceeds per-point limit {MAX_TOKENS_PER_POINT} for source key {}",
                row.source_key
            ),
        );
    }
    let first = row.token_vectors[0]
        .to_dense()
        .unwrap_or_else(|error| raise_core_error(error));
    let dimensions = first.dimension();
    for token in &row.token_vectors[1..] {
        let token = token
            .to_dense()
            .unwrap_or_else(|error| raise_core_error(error));
        if token.dimension() != dimensions {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "late-interaction token dimensions must be uniform for source key {}",
                    row.source_key
                ),
            );
        }
    }
    i32::try_from(dimensions).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "late-interaction token dimensions exceed integer range for source key {}",
                row.source_key
            ),
        )
    })
}

fn store_source_token_row(collection_id: i64, row: &SourceTokenRow) {
    Spi::run_with_args(
        "SELECT pgcontext._store_late_interaction_tokens($1, $2, $3)",
        &[
            collection_id.into(),
            row.source_key.as_str().into(),
            row.token_vectors.clone().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!(
                "failed to store late-interaction tokens for source key {}: {error}",
                row.source_key
            ),
        )
    });
}

fn finish_registration(collection_id: i64, dimensions: i32) {
    Spi::run_with_args(
        "SELECT pgcontext._finish_late_interaction_registration($1, $2)",
        &[collection_id.into(), dimensions.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to finalize late-interaction registration: {error}"),
        )
    });
}

fn collection_name_from_sql(value: String) -> CollectionName {
    CollectionName::new(value).unwrap_or_else(|error| raise_core_error(error))
}

fn qualified_table_name_from_sql(value: &str) -> QualifiedTableName {
    QualifiedTableName::new(value).unwrap_or_else(|error| raise_core_error(error))
}

fn sql_identifier_from_sql(value: &str) -> SqlIdentifier {
    SqlIdentifier::new(value).unwrap_or_else(|error| raise_core_error(error))
}

fn quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn quote_qualified_identifier(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_identifier(schema), quote_identifier(table))
}

fn spi_optional_column<T>(row: &spi::SpiTupleTable<'_>, index: usize) -> Option<T>
where
    T: FromDatum + IntoDatum,
{
    row.get::<T>(index).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read late-interaction catalog column {index}: {error}"),
        )
    })
}

fn spi_required_column<T>(row: &spi::SpiTupleTable<'_>, index: usize, label: &'static str) -> T
where
    T: FromDatum + IntoDatum,
{
    spi_optional_column(row, index).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("late-interaction catalog column is null: {label}"),
        )
    })
}

fn spi_iter_required_column<T>(
    row: &spi::SpiHeapTupleData<'_>,
    index: usize,
    label: &'static str,
) -> T
where
    T: FromDatum + IntoDatum,
{
    match row.get::<T>(index) {
        Ok(Some(value)) => value,
        Ok(None) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("late-interaction source column is null: {label}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read late-interaction source column {label}: {error}"),
        ),
    }
}
