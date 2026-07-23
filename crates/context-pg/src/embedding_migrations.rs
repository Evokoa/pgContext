//! SQL-facing embedding migration and backfill catalog.

use pgrx::prelude::*;

use crate::error::raise_sql_error;
use crate::pgcontext::EmbeddingMigrationStatus;

#[derive(Debug, Clone, Copy)]
struct MigrationCollection {
    collection_id: i64,
    owner_role: pg_sys::Oid,
}

#[derive(Debug, Clone, Copy)]
struct ModelVersionRef {
    model_version_id: i64,
}

#[derive(Debug, Clone)]
struct MigrationRow {
    migration_id: i64,
    collection_name: String,
    source_model: String,
    source_version: String,
    target_model: String,
    target_version: String,
    status: EmbeddingMigrationStatus,
    total_points: i64,
    processed_points: i64,
}

/// Creates an embedding migration between two registered model versions.
///
/// # Errors
///
/// Raises `undefined_object` for missing collections or model versions,
/// `insufficient_privilege` for non-owner callers, and
/// `invalid_parameter_value` for invalid counts or identical source/target
/// model versions.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "create_embedding_migration", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn create_embedding_migration(
    collection: String,
    source_model_name: String,
    source_model_version: String,
    target_model_name: String,
    target_model_version: String,
    total_points: i64,
) -> TableIterator<
    'static,
    (
        name!(migration_id, i64),
        name!(collection_name, String),
        name!(source_model, String),
        name!(source_version, String),
        name!(target_model, String),
        name!(target_version, String),
        name!(status, EmbeddingMigrationStatus),
        name!(total_points, i64),
        name!(processed_points, i64),
    ),
> {
    validate_non_negative(total_points, "total_points");
    let collection_row = resolve_collection(&collection);
    require_collection_owner(collection_row, &collection);
    let source = resolve_model_version(
        collection_row.collection_id,
        &source_model_name,
        &source_model_version,
    );
    let target = resolve_model_version(
        collection_row.collection_id,
        &target_model_name,
        &target_model_version,
    );
    if source.model_version_id == target.model_version_id {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "source and target model versions must differ",
        );
    }
    let migration_id = insert_embedding_migration(
        collection_row.collection_id,
        source.model_version_id,
        target.model_version_id,
        total_points,
    );

    TableIterator::once((
        migration_id,
        collection,
        source_model_name,
        source_model_version,
        target_model_name,
        target_model_version,
        EmbeddingMigrationStatus::Planned,
        total_points,
        0,
    ))
}

/// Updates embedding migration backfill progress.
///
/// # Errors
///
/// Raises `undefined_object` for missing migrations and
/// `invalid_parameter_value` for invalid counts or statuses.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "update_embedding_migration")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn update_embedding_migration(
    migration_id: i64,
    processed_points: i64,
    status: String,
) -> TableIterator<
    'static,
    (
        name!(migration_id, i64),
        name!(collection_name, String),
        name!(source_model, String),
        name!(source_version, String),
        name!(target_model, String),
        name!(target_version, String),
        name!(status, EmbeddingMigrationStatus),
        name!(total_points, i64),
        name!(processed_points, i64),
    ),
> {
    validate_non_negative(migration_id, "migration_id");
    validate_non_negative(processed_points, "processed_points");
    let status = parse_migration_status(&status);
    update_migration_row(migration_id, processed_points, status);
    TableIterator::once(resolve_migration_row(migration_id).into_tuple())
}

/// Lists embedding migrations.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "embedding_migrations")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn embedding_migrations() -> TableIterator<
    'static,
    (
        name!(migration_id, i64),
        name!(collection_name, String),
        name!(source_model, String),
        name!(source_version, String),
        name!(target_model, String),
        name!(target_version, String),
        name!(status, EmbeddingMigrationStatus),
        name!(total_points, i64),
        name!(processed_points, i64),
    ),
> {
    TableIterator::new(
        resolve_migration_rows()
            .into_iter()
            .map(MigrationRow::into_tuple)
            .collect::<Vec<_>>(),
    )
}

impl MigrationRow {
    fn into_tuple(
        self,
    ) -> (
        i64,
        String,
        String,
        String,
        String,
        String,
        EmbeddingMigrationStatus,
        i64,
        i64,
    ) {
        (
            self.migration_id,
            self.collection_name,
            self.source_model,
            self.source_version,
            self.target_model,
            self.target_version,
            self.status,
            self.total_points,
            self.processed_points,
        )
    }
}

fn validate_non_negative(value: i64, argument_name: &'static str) {
    if value < 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{argument_name} must not be negative: {value}"),
        );
    }
}

fn parse_migration_status(status: &str) -> EmbeddingMigrationStatus {
    match status {
        "planned" => EmbeddingMigrationStatus::Planned,
        "running" => EmbeddingMigrationStatus::Running,
        "completed" => EmbeddingMigrationStatus::Completed,
        "failed" => EmbeddingMigrationStatus::Failed,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported embedding migration status: {status}"),
        ),
    }
}

fn status_to_sql(status: EmbeddingMigrationStatus) -> &'static str {
    match status {
        EmbeddingMigrationStatus::Planned => "planned",
        EmbeddingMigrationStatus::Running => "running",
        EmbeddingMigrationStatus::Completed => "completed",
        EmbeddingMigrationStatus::Failed => "failed",
    }
}

fn status_from_sql(status: String) -> EmbeddingMigrationStatus {
    parse_migration_status(&status)
}

fn resolve_collection(collection: &str) -> MigrationCollection {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collection_id, owner_role
               FROM pgcontext._collections
              WHERE collection_name = $1",
            Some(1),
            &[collection.into()],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("collection does not exist: {collection}"),
            );
        }
        let row = rows.first();
        Ok::<_, spi::Error>(MigrationCollection {
            collection_id: required_column(row.get::<i64>(1)?, "collection_id"),
            owner_role: required_column(row.get::<pg_sys::Oid>(2)?, "owner_role"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("embedding migration collection lookup failed: {error}"),
        )
    })
}

fn require_collection_owner(collection: MigrationCollection, collection_name: &str) {
    let is_owner = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, $1::oid, 'MEMBER')",
        &[collection.owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check collection ownership: {error}"),
        )
    })
    .unwrap_or_default();
    if !is_owner {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for collection {collection_name}"),
        );
    }
}

fn resolve_model_version(
    collection_id: i64,
    model_name: &str,
    model_version: &str,
) -> ModelVersionRef {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT model_version_id
               FROM pgcontext._model_versions
              WHERE collection_id = $1
                AND model_name = $2
                AND model_version = $3",
            Some(1),
            &[
                collection_id.into(),
                model_name.into(),
                model_version.into(),
            ],
        )?;
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                format!("model version does not exist: {model_name}@{model_version}"),
            );
        }
        let row = rows.first();
        Ok::<_, spi::Error>(ModelVersionRef {
            model_version_id: required_column(row.get::<i64>(1)?, "model_version_id"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("model version lookup failed: {error}"),
        )
    })
}

fn insert_embedding_migration(
    collection_id: i64,
    source_model_version_id: i64,
    target_model_version_id: i64,
    total_points: i64,
) -> i64 {
    Spi::get_one_with_args::<i64>(
        "INSERT INTO pgcontext._embedding_migrations (
             collection_id,
             source_model_version_id,
             target_model_version_id,
             status,
             total_points
         )
         VALUES ($1, $2, $3, 'planned', $4)
         RETURNING migration_id",
        &[
            collection_id.into(),
            source_model_version_id.into(),
            target_model_version_id.into(),
            total_points.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to create embedding migration: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "embedding migration insert returned no id",
        )
    })
}

fn update_migration_row(
    migration_id: i64,
    processed_points: i64,
    status: EmbeddingMigrationStatus,
) {
    let total_points = migration_total_points(migration_id);
    if processed_points > total_points {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("embedding migration progress exceeds total: {migration_id}"),
        );
    }
    Spi::run_with_args(
        "UPDATE pgcontext._embedding_migrations
            SET processed_points = $2,
                status = $3,
                updated_at = pg_catalog.now()
          WHERE migration_id = $1",
        &[
            migration_id.into(),
            processed_points.into(),
            status_to_sql(status).into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to update embedding migration: {error}"),
        )
    });
}

fn migration_total_points(migration_id: i64) -> i64 {
    let total_points = Spi::get_one_with_args::<i64>(
        "SELECT total_points
           FROM pgcontext._embedding_migrations
          WHERE migration_id = $1",
        &[migration_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to inspect embedding migration: {error}"),
        )
    });
    total_points.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("embedding migration does not exist: {migration_id}"),
        )
    })
}

fn resolve_migration_row(migration_id: i64) -> MigrationRow {
    let mut rows = resolve_migration_rows()
        .into_iter()
        .filter(|row| row.migration_id == migration_id)
        .collect::<Vec<_>>();
    rows.pop().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("embedding migration does not exist: {migration_id}"),
        )
    })
}

fn resolve_migration_rows() -> Vec<MigrationRow> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT migrations.migration_id,
                    collections.collection_name,
                    source_model.model_name,
                    source_model.model_version,
                    target_model.model_name,
                    target_model.model_version,
                    migrations.status,
                    migrations.total_points,
                    migrations.processed_points
               FROM pgcontext._embedding_migrations AS migrations
               JOIN pgcontext._collections AS collections USING (collection_id)
               JOIN pgcontext._model_versions AS source_model
                 ON source_model.model_version_id = migrations.source_model_version_id
               JOIN pgcontext._model_versions AS target_model
                 ON target_model.model_version_id = migrations.target_model_version_id
              ORDER BY migrations.migration_id",
            None,
            &[],
        )?;
        let mut output = Vec::new();
        for row in rows {
            output.push(MigrationRow {
                migration_id: required_column(row.get::<i64>(1)?, "migration_id"),
                collection_name: required_column(row.get::<String>(2)?, "collection_name"),
                source_model: required_column(row.get::<String>(3)?, "source_model"),
                source_version: required_column(row.get::<String>(4)?, "source_version"),
                target_model: required_column(row.get::<String>(5)?, "target_model"),
                target_version: required_column(row.get::<String>(6)?, "target_version"),
                status: status_from_sql(required_column(row.get::<String>(7)?, "status")),
                total_points: required_column(row.get::<i64>(8)?, "total_points"),
                processed_points: required_column(row.get::<i64>(9)?, "processed_points"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("embedding migration lookup failed: {error}"),
        )
    })
}

fn required_column<T>(value: Option<T>, column_name: &'static str) -> T {
    match value {
        Some(value) => value,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("embedding migration catalog column was unexpectedly null: {column_name}"),
        ),
    }
}
