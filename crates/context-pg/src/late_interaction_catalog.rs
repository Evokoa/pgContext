//! Registration and materialization for pgContext-owned late-interaction tokens.

use core::mem::size_of;

use context_core::{CollectionName, QualifiedTableName, SqlIdentifier};
use pgrx::prelude::*;

use crate::{
    error::{raise_core_error, raise_sql_error},
    vector::Vector,
};

const REGISTRATION_BATCH_SIZE: i64 = 512;
const MAX_TOKENS_PER_POINT: usize = 16_384;
const MAX_MATERIALIZATION_BATCH_BYTES: usize = 16 * 1024 * 1024;
const ESTIMATED_VECTOR_OVERHEAD_BYTES: usize = 32;

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

#[derive(Debug)]
struct SourceTokenMetadata {
    source_key: String,
    token_count: Option<i32>,
    dimensions: Option<i32>,
}

#[derive(Debug, Clone, Copy)]
struct MaterializationSummary {
    dimensions: Option<i32>,
    batch_count: i64,
    point_count: i64,
    token_count: i64,
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
    let summary = materialize_source_tokens(&source, REGISTRATION_BATCH_SIZE);
    let status = match summary.dimensions {
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
        summary.dimensions,
        summary.point_count,
        summary.token_count,
        status.to_owned(),
    ))
}

/// Rebuilds owned late-interaction tokens and their collection-scoped HNSW index.
///
/// The repair holds the source-table trigger lock for the surrounding
/// transaction, clears the derived rows, rescans the source under invoker ACLs
/// and RLS in bounded batches, then publishes a ready index atomically.
#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn repair_late_interaction(
    collection: String,
    batch_size: i32,
) -> TableIterator<
    'static,
    (
        name!(collection, String),
        name!(batch_count, i64),
        name!(point_count, i64),
        name!(token_count, i64),
        name!(dimensions, Option<i32>),
        name!(status, String),
    ),
> {
    let collection = collection_name_from_sql(collection);
    let batch_size = repair_batch_size_from_sql(batch_size);
    let source = resolve_repair_source(&collection);
    require_registration_owner(&source, &collection);
    require_registration_source_select(&source);
    refresh_repair_source_binding(source.collection_id);
    prepare_repair(&source);
    let summary = materialize_source_tokens(&source, batch_size);
    let status = match summary.dimensions {
        Some(dimensions) => {
            finish_registration(source.collection_id, dimensions);
            "ready"
        }
        None => "building",
    };

    TableIterator::once((
        collection.as_str().to_owned(),
        summary.batch_count,
        summary.point_count,
        summary.token_count,
        summary.dimensions,
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
                        id_attribute.attname IS NOT NULL AS id_exists,
                        id_attribute.attnotnull AND EXISTS (
                            SELECT 1
                              FROM pg_catalog.pg_index AS identity_index
                             WHERE identity_index.indrelid = source_class.oid
                               AND identity_index.indisunique
                               AND identity_index.indisvalid
                               AND identity_index.indisready
                               AND identity_index.indimmediate
                               AND identity_index.indpred IS NULL
                               AND identity_index.indexprs IS NULL
                               AND identity_index.indnkeyatts = 1
                               AND identity_index.indnatts = 1
                               AND identity_index.indkey[0] = id_attribute.attnum
                        ) AS id_is_valid_identity,
                        source_class.relkind::text
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
                    AND source_class.relname = $3",
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
        if spi_optional_column::<bool>(&row, 10) != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction source key must be a NOT NULL single-column immediate unique key: {schema_name}.{table_name}.id"
                ),
            );
        }
        if spi_required_column::<String>(&row, 11, "source_relation_kind") != "r" {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                "late-interaction registration requires an ordinary table source; partitioned tables are not supported",
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

fn resolve_repair_source(collection: &CollectionName) -> LateInteractionRegistrationSource {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT acl.collection_id,
                        acl.owner_role,
                        source_class.oid,
                        registrations.source_schema_name,
                        registrations.source_table_name,
                        registrations.token_column_name,
                        token_attribute.attnum,
                        token_attribute.atttypid = 'public.vector[]'::regtype AS token_type_valid,
                        token_attribute.attnotnull,
                        id_attribute.attname IS NOT NULL AS id_exists,
                        id_attribute.attnotnull AND EXISTS (
                            SELECT 1
                              FROM pg_catalog.pg_index AS identity_index
                             WHERE identity_index.indrelid = source_class.oid
                               AND identity_index.indisunique
                               AND identity_index.indisvalid
                               AND identity_index.indisready
                               AND identity_index.indimmediate
                               AND identity_index.indpred IS NULL
                               AND identity_index.indexprs IS NULL
                               AND identity_index.indnkeyatts = 1
                               AND identity_index.indnatts = 1
                               AND identity_index.indkey[0] = id_attribute.attnum
                        ) AS id_is_valid_identity,
                        source_class.relkind::text
                   FROM pgcontext._collection_acl AS acl
                   JOIN pgcontext._visible_collection_late_interaction AS registrations
                     USING (collection_id)
                   JOIN pg_catalog.pg_namespace AS source_namespace
                     ON source_namespace.nspname = registrations.source_schema_name
                   JOIN pg_catalog.pg_class AS source_class
                     ON source_class.relnamespace = source_namespace.oid
                    AND source_class.relname = registrations.source_table_name
                   LEFT JOIN pg_catalog.pg_attribute AS token_attribute
                     ON token_attribute.attrelid = source_class.oid
                    AND token_attribute.attname = registrations.token_column_name
                    AND token_attribute.attnum > 0
                    AND NOT token_attribute.attisdropped
                   LEFT JOIN pg_catalog.pg_attribute AS id_attribute
                     ON id_attribute.attrelid = source_class.oid
                    AND id_attribute.attname = 'id'
                    AND id_attribute.attnum > 0
                    AND NOT id_attribute.attisdropped
                  WHERE acl.collection_name = $1",
                Some(1),
                &[collection.as_str().into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to resolve late-interaction repair source: {error}"),
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!(
                    "late-interaction registration does not exist or its source table drifted: {}",
                    collection.as_str()
                ),
            );
        }
        let row = rows.first();
        let schema_name = spi_required_column::<String>(&row, 4, "source_schema_name");
        let table_name = spi_required_column::<String>(&row, 5, "source_table_name");
        let token_column_name = spi_required_column::<String>(&row, 6, "token_column_name");
        let token_attnum = spi_optional_column::<i16>(&row, 7).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction token source column does not exist: {schema_name}.{table_name}.{token_column_name}"
                ),
            )
        });
        if spi_optional_column::<bool>(&row, 8) != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                format!(
                    "late-interaction token source must have type vector[]: {schema_name}.{table_name}.{token_column_name}"
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 9) != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction token source must be NOT NULL: {schema_name}.{table_name}.{token_column_name}"
                ),
            );
        }
        if !spi_required_column::<bool>(&row, 10, "source_id_exists") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction source key column does not exist: {schema_name}.{table_name}.id"
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 11) != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction source key must be a NOT NULL single-column immediate unique key: {schema_name}.{table_name}.id"
                ),
            );
        }
        if spi_required_column::<String>(&row, 12, "source_relation_kind") != "r" {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                "late-interaction repair requires an ordinary table source; partitioned tables are not supported",
            );
        }

        LateInteractionRegistrationSource {
            collection_id: spi_required_column(&row, 1, "collection_id"),
            owner_role: spi_required_column(&row, 2, "owner_role"),
            table_oid: spi_required_column(&row, 3, "source_table_oid"),
            schema_name,
            table_name,
            token_column_name,
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
    batch_size: i64,
) -> MaterializationSummary {
    let table_name = quote_qualified_identifier(&source.schema_name, &source.table_name);
    let token_column = quote_identifier(&source.token_column_name);
    let metadata_sql = format!(
        "SELECT id::text,
                pg_catalog.cardinality({token_column}),
                CASE
                    WHEN pg_catalog.cardinality({token_column}) > 0
                    THEN pgcontext.vector_dims({token_column}[1])
                    ELSE NULL
                END
           FROM {table_name}
          WHERE $1 = '' OR id::text > $1
          ORDER BY id::text
          LIMIT $2"
    );
    let row_sql = format!(
        "SELECT id::text, {token_column}
           FROM {table_name}
          WHERE id::text = $1"
    );
    let mut last_source_key = String::new();
    let mut dimensions = None;
    let mut point_count = 0_i64;
    let mut token_count = 0_i64;
    let mut batch_count = 0_i64;

    loop {
        let metadata =
            load_source_token_metadata_batch(&metadata_sql, &last_source_key, batch_size);
        if metadata.is_empty() {
            break;
        }
        let mut selected_keys = Vec::new();
        let mut estimated_batch_bytes = 0_usize;
        for row in metadata {
            let estimated_row_bytes = estimate_source_token_row_bytes(&row);
            if estimated_row_bytes > MAX_MATERIALIZATION_BATCH_BYTES {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    format!(
                        "late-interaction source row exceeds materialization byte budget {MAX_MATERIALIZATION_BATCH_BYTES} for source key {}",
                        row.source_key
                    ),
                );
            }
            if !selected_keys.is_empty()
                && estimated_batch_bytes.saturating_add(estimated_row_bytes)
                    > MAX_MATERIALIZATION_BATCH_BYTES
            {
                break;
            }
            estimated_batch_bytes = estimated_batch_bytes.saturating_add(estimated_row_bytes);
            selected_keys.push(row.source_key);
        }
        let Some(next_last_source_key) = selected_keys.last().cloned() else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "late-interaction materialization made no keyset progress",
            );
        };
        let batch = load_source_token_batch(&row_sql, &selected_keys);
        batch_count = batch_count.checked_add(1).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "late-interaction batch count overflow",
            )
        });
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
        last_source_key = next_last_source_key;
    }

    MaterializationSummary {
        dimensions,
        batch_count,
        point_count,
        token_count,
    }
}

fn load_source_token_metadata_batch(
    sql: &str,
    last_source_key: &str,
    batch_size: i64,
) -> Vec<SourceTokenMetadata> {
    Spi::connect(|client| {
        let rows = client
            .select(sql, None, &[last_source_key.into(), batch_size.into()])
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to scan late-interaction source metadata: {error}"),
                )
            });
        rows.into_iter()
            .map(|row| SourceTokenMetadata {
                source_key: spi_iter_required_column(&row, 1, "source_key"),
                token_count: row.get::<i32>(2).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read late-interaction token cardinality: {error}"),
                    )
                }),
                dimensions: row.get::<i32>(3).unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to read late-interaction token dimensions: {error}"),
                    )
                }),
            })
            .collect()
    })
}

fn estimate_source_token_row_bytes(row: &SourceTokenMetadata) -> usize {
    let token_count = row
        .token_count
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1);
    let dimensions = row
        .dimensions
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1);
    token_count.saturating_mul(
        dimensions
            .saturating_mul(size_of::<f32>())
            .saturating_add(ESTIMATED_VECTOR_OVERHEAD_BYTES),
    )
}

fn load_source_token_batch(sql: &str, source_keys: &[String]) -> Vec<SourceTokenRow> {
    source_keys
        .iter()
        .map(|source_key| {
            Spi::connect(|client| {
                let rows = client
                    .select(sql, Some(1), &[source_key.as_str().into()])
                    .unwrap_or_else(|error| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            format!("failed to load late-interaction source row: {error}"),
                        )
                    });
                if rows.is_empty() {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                        format!(
                            "late-interaction source row changed during materialization: {source_key}"
                        ),
                    );
                }
                let row = rows.first();
                SourceTokenRow {
                    source_key: spi_required_column(&row, 1, "source_key"),
                    token_vectors: spi_required_column(&row, 2, "token_vectors"),
                }
            })
        })
        .collect()
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
        "SELECT pgcontext._store_late_interaction_tokens($1, $2)",
        &[collection_id.into(), row.source_key.as_str().into()],
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

fn refresh_repair_source_binding(collection_id: i64) {
    Spi::run_with_args(
        "SELECT pgcontext._refresh_collection_source_table($1)",
        &[collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to refresh late-interaction source binding: {error}"),
        )
    });
}

fn prepare_repair(source: &LateInteractionRegistrationSource) {
    Spi::run_with_args(
        "SELECT pgcontext._prepare_late_interaction_repair($1, $2, $3)",
        &[
            source.collection_id.into(),
            source.table_oid.into(),
            source.token_attnum.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to prepare late-interaction repair: {error}"),
        )
    });
}

fn repair_batch_size_from_sql(batch_size: i32) -> i64 {
    if !(1..=10_000).contains(&batch_size) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("late-interaction repair batch size must be between 1 and 10000: {batch_size}"),
        );
    }
    i64::from(batch_size)
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
