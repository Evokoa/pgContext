//! SQL-facing point mapping functions.

use context_core::{CollectionName, SourceKey};
use pgrx::prelude::*;

use crate::error::{raise_core_error, raise_sql_error};

#[derive(Debug, Clone)]
struct PointCollection {
    id: i64,
    owner_role: pg_sys::Oid,
    table_oid: pg_sys::Oid,
    schema_name: String,
    table_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PointUpsert {
    point_id: i64,
    source_key: String,
    inserted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PointDelete {
    point_id: i64,
    source_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BulkUpsertProgress {
    batch_number: i64,
    processed_count: i64,
    inserted_count: i64,
    reactivated_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BulkDeleteProgress {
    batch_number: i64,
    processed_count: i64,
    deleted_count: i64,
    missing_count: i64,
}

/// Upserts source-row keys into stable pgContext point IDs.
///
/// The source rows remain owned by the user's table. pgContext stores only the
/// source key to point ID mapping needed by registered collection search.
///
/// # Errors
///
/// Raises `invalid_parameter_value` when the collection name is invalid,
/// `undefined_object` when the collection does not exist,
/// `invalid_parameter_value` when the collection has no source table or a
/// source key is empty/too large, and `internal_error` for unexpected catalog
/// failures.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn upsert_points(
    collection_name: String,
    source_keys: Vec<String>,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(inserted, bool),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = resolve_point_collection(&collection_name);
    let source_keys = source_keys
        .into_iter()
        .map(source_key_from_sql)
        .collect::<Vec<_>>();
    crate::collection_limits::enforce_point_upsert_limit(
        collection.id,
        &collection_name,
        &source_keys,
    );
    let rows = source_keys
        .iter()
        .map(|source_key| upsert_point(&collection, source_key))
        .map(|row| (row.point_id, row.source_key, row.inserted))
        .collect::<Vec<_>>();

    TableIterator::new(rows)
}

/// Marks source-row keys as deleted in the pgContext point mapping.
///
/// Deletion is logical: existing point IDs are retained so a later upsert of the
/// same source key can reactivate the same point ID.
///
/// # Errors
///
/// Raises `invalid_parameter_value` when the collection name is invalid,
/// `undefined_object` when the collection does not exist,
/// `invalid_parameter_value` when a source key is empty/too large, and
/// `internal_error` for unexpected catalog failures.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn delete_points(
    collection_name: String,
    source_keys: Vec<String>,
) -> TableIterator<'static, (name!(point_id, i64), name!(source_key, String))> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = resolve_point_collection(&collection_name);
    let rows = source_keys
        .into_iter()
        .map(source_key_from_sql)
        .filter_map(|source_key| delete_point(&collection, &source_key))
        .map(|row| (row.point_id, row.source_key))
        .collect::<Vec<_>>();

    TableIterator::new(rows)
}

/// Upserts source-row keys in bounded batches and reports progress per batch.
///
/// The function validates every supplied source key before mutating catalog
/// state, so malformed arrays fail without a partial point insert.
///
/// # Errors
///
/// Raises the same collection and source-key errors as [`upsert_points`], and
/// `invalid_parameter_value` when `batch_size` is not positive.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn bulk_upsert_points(
    collection_name: String,
    source_keys: Vec<String>,
    batch_size: i32,
) -> TableIterator<
    'static,
    (
        name!(batch_number, i64),
        name!(processed_count, i64),
        name!(inserted_count, i64),
        name!(reactivated_count, i64),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = resolve_point_collection(&collection_name);
    let source_keys = source_keys_from_sql(source_keys);
    crate::collection_limits::enforce_point_upsert_limit(
        collection.id,
        &collection_name,
        &source_keys,
    );
    let batch_size = batch_size_from_sql(batch_size);
    let rows = bulk_upsert_source_keys(&collection, &source_keys, batch_size)
        .into_iter()
        .map(|row| {
            (
                row.batch_number,
                row.processed_count,
                row.inserted_count,
                row.reactivated_count,
            )
        })
        .collect::<Vec<_>>();

    TableIterator::new(rows)
}

/// Deletes source-row keys in bounded batches and reports progress per batch.
///
/// Missing source keys are counted in progress rows and otherwise ignored, like
/// [`delete_points`].
///
/// # Errors
///
/// Raises the same collection and source-key errors as [`delete_points`], and
/// `invalid_parameter_value` when `batch_size` is not positive.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn bulk_delete_points(
    collection_name: String,
    source_keys: Vec<String>,
    batch_size: i32,
) -> TableIterator<
    'static,
    (
        name!(batch_number, i64),
        name!(processed_count, i64),
        name!(deleted_count, i64),
        name!(missing_count, i64),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = resolve_point_collection(&collection_name);
    let source_keys = source_keys_from_sql(source_keys);
    let batch_size = batch_size_from_sql(batch_size);
    let rows = bulk_delete_source_keys(&collection, &source_keys, batch_size)
        .into_iter()
        .map(|row| {
            (
                row.batch_number,
                row.processed_count,
                row.deleted_count,
                row.missing_count,
            )
        })
        .collect::<Vec<_>>();

    TableIterator::new(rows)
}

/// Backfills point mappings from the registered source table's `id` column.
///
/// The source table remains authoritative. This function scans source `id`
/// values in bounded batches and upserts their `id::text` values into the
/// pgContext point mapping.
///
/// # Errors
///
/// Raises the same collection errors as [`upsert_points`], `insufficient_privilege`
/// when the session user cannot `SELECT` from the source table, and
/// `invalid_parameter_value` when `batch_size` is not positive.
#[pg_extern(schema = "pgcontext", security_definer)]
#[search_path(pg_catalog, pgcontext)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires inline name!() table shapes for SQL generation"
)]
pub fn backfill_points(
    collection_name: String,
    batch_size: i32,
) -> TableIterator<
    'static,
    (
        name!(batch_number, i64),
        name!(processed_count, i64),
        name!(inserted_count, i64),
        name!(reactivated_count, i64),
    ),
> {
    let collection_name = collection_name_from_sql(collection_name);
    let collection = resolve_point_collection(&collection_name);
    require_table_select_privilege(&collection);
    let batch_size = batch_size_from_sql(batch_size);
    let rows = backfill_source_table(&collection, &collection_name, batch_size)
        .into_iter()
        .map(|row| {
            (
                row.batch_number,
                row.processed_count,
                row.inserted_count,
                row.reactivated_count,
            )
        })
        .collect::<Vec<_>>();

    TableIterator::new(rows)
}

fn collection_name_from_sql(collection_name: String) -> CollectionName {
    match CollectionName::new(collection_name) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    }
}

fn source_key_from_sql(source_key: String) -> SourceKey {
    match SourceKey::new(source_key) {
        Ok(source_key) => source_key,
        Err(error) => raise_core_error(error),
    }
}

fn source_keys_from_sql(source_keys: Vec<String>) -> Vec<SourceKey> {
    source_keys
        .into_iter()
        .map(source_key_from_sql)
        .collect::<Vec<_>>()
}

fn batch_size_from_sql(batch_size: i32) -> usize {
    match usize::try_from(batch_size) {
        Ok(batch_size) if batch_size > 0 => batch_size,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid point batch size: {batch_size}"),
        ),
    }
}

fn resolve_point_collection(collection_name: &CollectionName) -> PointCollection {
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
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {}", collection_name.as_str()),
            );
        }

        let row = rows.first();
        let source_table_oid = spi_optional_column::<pg_sys::Oid>(&row, 3, "source_table_oid");
        let source_schema_name = spi_optional_column::<String>(&row, 4, "source_schema_name");
        let source_table_name = spi_optional_column::<String>(&row, 5, "source_table_name");
        let (Some(table_oid), Some(schema_name), Some(table_name)) =
            (source_table_oid, source_schema_name, source_table_name)
        else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "collection has no source table: {}",
                    collection_name.as_str()
                ),
            );
        };

        let collection = PointCollection {
            id: spi_required_column::<i64>(&row, 1, "collection_id"),
            owner_role: spi_required_column::<pg_sys::Oid>(&row, 2, "owner_role"),
            table_oid,
            schema_name,
            table_name,
        };
        require_collection_owner(&collection, collection_name);
        collection
    })
}

fn require_collection_owner(collection: &PointCollection, collection_name: &CollectionName) {
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

fn require_table_select_privilege(collection: &PointCollection) {
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

fn bulk_upsert_source_keys(
    collection: &PointCollection,
    source_keys: &[SourceKey],
    batch_size: usize,
) -> Vec<BulkUpsertProgress> {
    source_keys
        .chunks(batch_size)
        .enumerate()
        .map(|(batch_index, batch)| {
            let mut inserted_count = 0_i64;
            let mut reactivated_count = 0_i64;
            for source_key in batch {
                let row = upsert_point(collection, source_key);
                if row.inserted {
                    inserted_count += 1;
                } else {
                    reactivated_count += 1;
                }
            }
            BulkUpsertProgress {
                batch_number: usize_to_sql_i64(batch_index + 1, "bulk upsert batch number"),
                processed_count: usize_to_sql_i64(batch.len(), "bulk upsert processed count"),
                inserted_count,
                reactivated_count,
            }
        })
        .collect()
}

fn bulk_delete_source_keys(
    collection: &PointCollection,
    source_keys: &[SourceKey],
    batch_size: usize,
) -> Vec<BulkDeleteProgress> {
    source_keys
        .chunks(batch_size)
        .enumerate()
        .map(|(batch_index, batch)| {
            let mut deleted_count = 0_i64;
            for source_key in batch {
                if delete_point(collection, source_key).is_some() {
                    deleted_count += 1;
                }
            }
            let processed_count = usize_to_sql_i64(batch.len(), "bulk delete processed count");
            BulkDeleteProgress {
                batch_number: usize_to_sql_i64(batch_index + 1, "bulk delete batch number"),
                processed_count,
                deleted_count,
                missing_count: processed_count - deleted_count,
            }
        })
        .collect()
}

fn backfill_source_table(
    collection: &PointCollection,
    collection_name: &CollectionName,
    batch_size: usize,
) -> Vec<BulkUpsertProgress> {
    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let mut offset = 0_i64;
    let mut output = Vec::new();
    loop {
        let source_keys = select_source_keys(&table_name, batch_size, offset);
        if source_keys.is_empty() {
            break;
        }
        crate::collection_limits::enforce_point_upsert_limit(
            collection.id,
            collection_name,
            &source_keys,
        );
        let mut batch = bulk_upsert_source_keys(collection, &source_keys, batch_size);
        let batch_number = usize_to_sql_i64(output.len() + 1, "backfill batch number");
        if let Some(batch) = batch.first_mut() {
            batch.batch_number = batch_number;
        }
        output.extend(batch);
        let selected = usize_to_sql_i64(source_keys.len(), "backfill selected row count");
        offset = offset.checked_add(selected).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "point backfill offset exceeds PostgreSQL bigint range",
            )
        });
    }
    output
}

fn select_source_keys(table_name: &str, batch_size: usize, offset: i64) -> Vec<SourceKey> {
    let sql = format!(
        "SELECT id::text
           FROM {table_name}
          ORDER BY id::text
          LIMIT $1
         OFFSET $2"
    );
    let limit = usize_to_sql_i64(batch_size, "point backfill batch size");
    Spi::connect(|client| {
        let rows = match client.select(&sql, Some(limit), &[limit.into(), offset.into()]) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to scan source table for point backfill: {error}"),
            ),
        };
        let mut source_keys = Vec::new();
        for row in rows {
            let source_key = spi_iter_required_column::<String>(&row, 1, "source_key");
            source_keys.push(source_key_from_sql(source_key));
        }
        source_keys
    })
}

fn usize_to_sql_i64(value: usize, label: &'static str) -> i64 {
    i64::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{label} exceeds PostgreSQL bigint range: {value}"),
        )
    })
}

fn upsert_point(collection: &PointCollection, source_key: &SourceKey) -> PointUpsert {
    if let Some(point_id) = find_point(collection, source_key) {
        reactivate_point(collection, source_key);
        return PointUpsert {
            point_id,
            source_key: source_key.to_string(),
            inserted: false,
        };
    }

    let point_id = Spi::get_one_with_args::<i64>(
        "INSERT INTO pgcontext._collection_points (collection_id, source_key)
         VALUES ($1, $2)
         RETURNING point_id",
        &[collection.id.into(), source_key.as_str().into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to insert point mapping: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "point insert returned no row",
        )
    });

    PointUpsert {
        point_id,
        source_key: source_key.to_string(),
        inserted: true,
    }
}

fn find_point(collection: &PointCollection, source_key: &SourceKey) -> Option<i64> {
    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT point_id
               FROM pgcontext._collection_points
              WHERE collection_id = $1
                AND source_key = $2",
            Some(1),
            &[collection.id.into(), source_key.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to query point mapping: {error}"),
            ),
        };

        if rows.is_empty() {
            return None;
        }

        let row = rows.first();
        Some(spi_required_column::<i64>(&row, 1, "point_id"))
    })
}

fn reactivate_point(collection: &PointCollection, source_key: &SourceKey) {
    Spi::run_with_args(
        "UPDATE pgcontext._collection_points
            SET deleted_at = NULL,
                updated_at = pg_catalog.now()
          WHERE collection_id = $1
            AND source_key = $2",
        &[collection.id.into(), source_key.as_str().into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to reactivate point mapping: {error}"),
        )
    });
}

fn delete_point(collection: &PointCollection, source_key: &SourceKey) -> Option<PointDelete> {
    Spi::connect_mut(|client| {
        let rows = match client.update(
            "UPDATE pgcontext._collection_points
                SET deleted_at = COALESCE(deleted_at, pg_catalog.now()),
                    updated_at = pg_catalog.now()
              WHERE collection_id = $1
                AND source_key = $2
              RETURNING point_id, source_key",
            Some(1),
            &[collection.id.into(), source_key.as_str().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to delete point mapping: {error}"),
            ),
        };

        if rows.is_empty() {
            return None;
        }

        let row = rows.first();
        Some(PointDelete {
            point_id: spi_required_column::<i64>(&row, 1, "point_id"),
            source_key: spi_required_column::<String>(&row, 2, "source_key"),
        })
    })
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
            format!("point catalog column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read point catalog column {column_name}: {error}"),
        ),
    }
}

fn spi_iter_required_column<T>(
    row: &spi::SpiHeapTupleData<'_>,
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
            format!("point catalog column is null: {column_name}"),
        ),
        Err(error) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read point catalog column {column_name}: {error}"),
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
            format!("failed to read point catalog column {column_name}: {error}"),
        ),
    }
}

fn quote_qualified_identifier(schema_name: &str, table_name: &str) -> String {
    Spi::get_one_with_args::<String>(
        "SELECT pg_catalog.format('%I.%I', $1, $2)",
        &[schema_name.into(), table_name.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to quote table identifier: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "quoted table identifier returned null",
        )
    })
}
