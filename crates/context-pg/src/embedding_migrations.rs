//! SQL-facing immutable-profile migration and backfill catalog.

use pgrx::prelude::*;

use crate::error::raise_sql_error;
use crate::pgcontext::EmbeddingMigrationStatus;

#[derive(Clone, Copy, Debug)]
struct MigrationCollection {
    collection_id: i64,
    owner_role: pg_sys::Oid,
}

#[derive(Clone, Copy, Debug)]
struct ProfileRef {
    embedding_profile_id: i64,
}

#[derive(Clone, Debug)]
struct MigrationRow {
    migration_id: i64,
    collection_name: String,
    source_profile: String,
    target_profile: String,
    status: EmbeddingMigrationStatus,
    total_points: i64,
    processed_points: i64,
}

type MigrationTuple = (
    i64,
    String,
    String,
    String,
    EmbeddingMigrationStatus,
    i64,
    i64,
);

/// Creates a backfill migration between two immutable embedding profiles.
///
/// # Errors
///
/// Raises `undefined_object` for a missing collection/profile,
/// `insufficient_privilege` for a non-owner caller, and
/// `invalid_parameter_value` for a negative count or identical profiles.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "create_embedding_migration", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn create_embedding_migration(
    collection: String,
    source_profile: String,
    target_profile: String,
    total_points: i64,
) -> TableIterator<
    'static,
    (
        name!(migration_id, i64),
        name!(collection_name, String),
        name!(source_profile, String),
        name!(target_profile, String),
        name!(status, EmbeddingMigrationStatus),
        name!(total_points, i64),
        name!(processed_points, i64),
    ),
> {
    validate_non_negative(total_points, "total_points");
    let collection_row = resolve_collection(&collection);
    require_collection_owner(collection_row, &collection);
    let source = resolve_profile(collection_row.collection_id, &source_profile);
    let target = resolve_profile(collection_row.collection_id, &target_profile);
    if source.embedding_profile_id == target.embedding_profile_id {
        invalid_parameter("source and target embedding profiles must differ");
    }
    let migration_id = insert_embedding_migration(
        collection_row.collection_id,
        source.embedding_profile_id,
        target.embedding_profile_id,
        total_points,
    );
    TableIterator::once((
        migration_id,
        collection,
        source_profile,
        target_profile,
        EmbeddingMigrationStatus::Planned,
        total_points,
        0,
    ))
}

/// Updates embedding-profile migration backfill progress.
///
/// # Errors
///
/// Raises `undefined_object` for a missing migration,
/// `insufficient_privilege` for a non-owner caller, and
/// `invalid_parameter_value` for invalid counts or statuses.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "update_embedding_migration", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn update_embedding_migration(
    migration_id: i64,
    processed_points: i64,
    status: String,
) -> TableIterator<
    'static,
    (
        name!(migration_id, i64),
        name!(collection_name, String),
        name!(source_profile, String),
        name!(target_profile, String),
        name!(status, EmbeddingMigrationStatus),
        name!(total_points, i64),
        name!(processed_points, i64),
    ),
> {
    validate_non_negative(migration_id, "migration_id");
    validate_non_negative(processed_points, "processed_points");
    let status = parse_migration_status(&status);
    require_migration_owner(migration_id);
    update_migration_row(migration_id, processed_points, status);
    TableIterator::once(resolve_migration_row(migration_id).into_tuple())
}

/// Lists embedding-profile migrations visible through collection membership.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "embedding_migrations", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn embedding_migrations() -> TableIterator<
    'static,
    (
        name!(migration_id, i64),
        name!(collection_name, String),
        name!(source_profile, String),
        name!(target_profile, String),
        name!(status, EmbeddingMigrationStatus),
        name!(total_points, i64),
        name!(processed_points, i64),
    ),
> {
    TableIterator::new(
        resolve_migration_rows(None)
            .into_iter()
            .map(MigrationRow::into_tuple),
    )
}

impl MigrationRow {
    fn into_tuple(self) -> MigrationTuple {
        (
            self.migration_id,
            self.collection_name,
            self.source_profile,
            self.target_profile,
            self.status,
            self.total_points,
            self.processed_points,
        )
    }
}

fn validate_non_negative(value: i64, argument_name: &'static str) {
    if value < 0 {
        invalid_parameter(format!("{argument_name} must not be negative: {value}"));
    }
}

fn parse_migration_status(status: &str) -> EmbeddingMigrationStatus {
    match status {
        "planned" => EmbeddingMigrationStatus::Planned,
        "running" => EmbeddingMigrationStatus::Running,
        "completed" => EmbeddingMigrationStatus::Completed,
        "failed" => EmbeddingMigrationStatus::Failed,
        _ => invalid_parameter(format!("unsupported embedding migration status: {status}")),
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
            undefined_object(format!("collection does not exist: {collection}"));
        }
        let row = rows.first();
        Ok::<_, spi::Error>(MigrationCollection {
            collection_id: required(row.get::<i64>(1)?, "collection_id"),
            owner_role: required(row.get::<pg_sys::Oid>(2)?, "owner_role"),
        })
    })
    .unwrap_or_else(|error| {
        internal(format!(
            "embedding migration collection lookup failed: {error}"
        ))
    })
}

fn require_collection_owner(collection: MigrationCollection, collection_name: &str) {
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, $1::oid, 'MEMBER')",
        &[collection.owner_role.into()],
    )
    .unwrap_or_else(|error| internal(format!("failed to check collection ownership: {error}")))
    .unwrap_or_default();
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for collection {collection_name}"),
        );
    }
}

fn require_migration_owner(migration_id: i64) {
    let owner_role = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT collections.owner_role
           FROM pgcontext._embedding_migrations AS migrations
           JOIN pgcontext._collections AS collections USING (collection_id)
          WHERE migrations.migration_id = $1",
        &[migration_id.into()],
    )
    .unwrap_or_else(|error| internal(format!("failed to resolve migration owner: {error}")))
    .unwrap_or_else(|| {
        undefined_object(format!(
            "embedding migration does not exist: {migration_id}"
        ))
    });
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, $1::oid, 'MEMBER')",
        &[owner_role.into()],
    )
    .unwrap_or_else(|error| internal(format!("failed to check migration ownership: {error}")))
    .unwrap_or_default();
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for embedding migration {migration_id}"),
        );
    }
}

fn resolve_profile(collection_id: i64, profile_name: &str) -> ProfileRef {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT embedding_profile_id
               FROM pgcontext._embedding_profiles
              WHERE collection_id = $1 AND profile_name = $2",
            Some(1),
            &[collection_id.into(), profile_name.into()],
        )?;
        if rows.is_empty() {
            undefined_object(format!("embedding profile does not exist: {profile_name}"));
        }
        Ok::<_, spi::Error>(ProfileRef {
            embedding_profile_id: required(rows.first().get::<i64>(1)?, "embedding_profile_id"),
        })
    })
    .unwrap_or_else(|error| internal(format!("embedding profile lookup failed: {error}")))
}

fn insert_embedding_migration(
    collection_id: i64,
    source_embedding_profile_id: i64,
    target_embedding_profile_id: i64,
    total_points: i64,
) -> i64 {
    Spi::get_one_with_args::<i64>(
        "INSERT INTO pgcontext._embedding_migrations (
             collection_id, source_embedding_profile_id,
             target_embedding_profile_id, status, total_points
         ) VALUES ($1, $2, $3, 'planned', $4)
         RETURNING migration_id",
        &[
            collection_id.into(),
            source_embedding_profile_id.into(),
            target_embedding_profile_id.into(),
            total_points.into(),
        ],
    )
    .unwrap_or_else(|error| internal(format!("failed to create embedding migration: {error}")))
    .unwrap_or_else(|| internal("embedding migration insert returned no id"))
}

fn update_migration_row(
    migration_id: i64,
    processed_points: i64,
    status: EmbeddingMigrationStatus,
) {
    let total_points = migration_total_points(migration_id);
    if processed_points > total_points {
        invalid_parameter(format!(
            "embedding migration progress exceeds total: {migration_id}"
        ));
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
    .unwrap_or_else(|error| internal(format!("failed to update embedding migration: {error}")));
}

fn migration_total_points(migration_id: i64) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT total_points
           FROM pgcontext._embedding_migrations
          WHERE migration_id = $1",
        &[migration_id.into()],
    )
    .unwrap_or_else(|error| internal(format!("failed to inspect embedding migration: {error}")))
    .unwrap_or_else(|| {
        undefined_object(format!(
            "embedding migration does not exist: {migration_id}"
        ))
    })
}

fn resolve_migration_row(migration_id: i64) -> MigrationRow {
    resolve_migration_rows(Some(migration_id))
        .into_iter()
        .next()
        .unwrap_or_else(|| {
            undefined_object(format!(
                "embedding migration does not exist: {migration_id}"
            ))
        })
}

fn resolve_migration_rows(migration_id: Option<i64>) -> Vec<MigrationRow> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT migrations.migration_id,
                    collections.collection_name,
                    source_profile.profile_name,
                    target_profile.profile_name,
                    migrations.status,
                    migrations.total_points,
                    migrations.processed_points
               FROM pgcontext._embedding_migrations AS migrations
               JOIN pgcontext._collections AS collections USING (collection_id)
               JOIN pgcontext._embedding_profiles AS source_profile
                 ON source_profile.embedding_profile_id = migrations.source_embedding_profile_id
               JOIN pgcontext._embedding_profiles AS target_profile
                 ON target_profile.embedding_profile_id = migrations.target_embedding_profile_id
              WHERE pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')
                AND ($1::bigint IS NULL OR migrations.migration_id = $1)
              ORDER BY migrations.migration_id",
            None,
            &[migration_id.into()],
        )?;
        rows.into_iter()
            .map(|row| {
                Ok(MigrationRow {
                    migration_id: required(row.get::<i64>(1)?, "migration_id"),
                    collection_name: required(row.get::<String>(2)?, "collection_name"),
                    source_profile: required(row.get::<String>(3)?, "source_profile"),
                    target_profile: required(row.get::<String>(4)?, "target_profile"),
                    status: parse_migration_status(&required(row.get::<String>(5)?, "status")),
                    total_points: required(row.get::<i64>(6)?, "total_points"),
                    processed_points: required(row.get::<i64>(7)?, "processed_points"),
                })
            })
            .collect::<Result<Vec<_>, spi::Error>>()
    })
    .unwrap_or_else(|error| internal(format!("embedding migration lookup failed: {error}")))
}

fn required<T>(value: Option<T>, column_name: &'static str) -> T {
    value.unwrap_or_else(|| {
        internal(format!(
            "embedding migration catalog column was unexpectedly null: {column_name}"
        ))
    })
}

fn invalid_parameter(message: impl Into<String>) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        message.into(),
    )
}

fn undefined_object(message: impl Into<String>) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT, message.into())
}

fn internal(message: impl Into<String>) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_INTERNAL_ERROR, message.into())
}
