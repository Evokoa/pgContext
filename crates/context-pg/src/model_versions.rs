//! SQL-facing embedding model version catalog.

use context_core::{DistanceMetric, VectorDimensions};
use pgrx::prelude::*;

use crate::domain_types::{
    distance_metric_from_catalog, distance_metric_from_sql, distance_metric_label,
};
use crate::error::raise_sql_error;

const MAX_MODEL_LABEL_LENGTH: usize = 128;

#[derive(Debug, Clone, Copy)]
struct ModelCollection {
    collection_id: i64,
    owner_role: pg_sys::Oid,
}

struct ModelVersionRow {
    collection_name: String,
    model_name: String,
    model_version: String,
    dimensions: i32,
    metric: DistanceMetric,
    is_active: bool,
}

/// Registers an embedding model version for a collection.
///
/// # Errors
///
/// Raises `undefined_object` for missing collections, `insufficient_privilege`
/// for non-owner callers, `invalid_parameter_value` for invalid labels or
/// dimensions, `feature_not_supported` for unsupported metrics, and
/// `duplicate_object` when the model/version pair already exists.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(
    schema = "pgcontext",
    name = "register_model_version",
    security_definer
)]
#[search_path(pg_catalog, pgcontext)]
pub fn register_model_version(
    collection: String,
    model_name: String,
    model_version: String,
    dimensions: i32,
    metric: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(model_name, String),
        name!(model_version, String),
        name!(dimensions, i32),
        name!(metric, String),
        name!(is_active, bool),
    ),
> {
    validate_label("model_name", &model_name);
    validate_label("model_version", &model_version);
    let dimensions = validate_dimensions(dimensions);
    let metric = distance_metric_from_sql(&metric, "");
    let collection_row = resolve_collection(&collection);
    require_collection_owner(collection_row, &collection);
    insert_model_version(
        collection_row.collection_id,
        &model_name,
        &model_version,
        dimensions,
        metric,
    );

    TableIterator::once((
        collection,
        model_name,
        model_version,
        dimensions,
        distance_metric_label(metric).to_owned(),
        true,
    ))
}

/// Lists registered embedding model versions.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(schema = "pgcontext", name = "model_versions")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn model_versions() -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(model_name, String),
        name!(model_version, String),
        name!(dimensions, i32),
        name!(metric, String),
        name!(is_active, bool),
    ),
> {
    TableIterator::new(resolve_model_versions().into_iter().map(|row| {
        (
            row.collection_name,
            row.model_name,
            row.model_version,
            row.dimensions,
            distance_metric_label(row.metric).to_owned(),
            row.is_active,
        )
    }))
}

fn validate_label(argument_name: &'static str, value: &str) {
    if value.is_empty() || value.len() > MAX_MODEL_LABEL_LENGTH {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{argument_name} must be 1..={MAX_MODEL_LABEL_LENGTH} bytes"),
        );
    }
}

fn validate_dimensions(dimensions: i32) -> i32 {
    let Ok(dimensions_usize) = usize::try_from(dimensions) else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid vector dimensions: {dimensions}"),
        );
    };
    if let Err(error) = VectorDimensions::new(dimensions_usize) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        );
    }
    dimensions
}

fn resolve_collection(collection: &str) -> ModelCollection {
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
        Ok::<_, spi::Error>(ModelCollection {
            collection_id: required_column(row.get::<i64>(1)?, "collection_id"),
            owner_role: required_column(row.get::<pg_sys::Oid>(2)?, "owner_role"),
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("model version collection lookup failed: {error}"),
        )
    })
}

fn require_collection_owner(collection: ModelCollection, collection_name: &str) {
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

fn insert_model_version(
    collection_id: i64,
    model_name: &str,
    model_version: &str,
    dimensions: i32,
    metric: DistanceMetric,
) {
    reject_duplicate_model_version(collection_id, model_name, model_version);
    Spi::run_with_args(
        "INSERT INTO pgcontext._model_versions (
             collection_id,
             model_name,
             model_version,
             dimensions,
             metric
         )
         VALUES ($1, $2, $3, $4, $5)",
        &[
            collection_id.into(),
            model_name.into(),
            model_version.into(),
            dimensions.into(),
            distance_metric_label(metric).into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to register model version: {error}"),
        )
    });
}

fn reject_duplicate_model_version(collection_id: i64, model_name: &str, model_version: &str) {
    let exists = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pgcontext._model_versions
              WHERE collection_id = $1
                AND model_name = $2
                AND model_version = $3
         )",
        &[
            collection_id.into(),
            model_name.into(),
            model_version.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check model version duplicate: {error}"),
        )
    })
    .unwrap_or_default();
    if exists {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            format!("model version already registered: {model_name}@{model_version}"),
        );
    }
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
fn resolve_model_versions() -> Vec<ModelVersionRow> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT collections.collection_name,
                    versions.model_name,
                    versions.model_version,
                    versions.dimensions,
                    versions.metric,
                    versions.is_active
               FROM pgcontext._model_versions AS versions
               JOIN pgcontext._collections AS collections USING (collection_id)
              ORDER BY collections.collection_name,
                       versions.model_name,
                       versions.model_version",
            None,
            &[],
        )?;
        let mut output = Vec::new();
        for row in rows {
            output.push(ModelVersionRow {
                collection_name: required_column(row.get::<String>(1)?, "collection_name"),
                model_name: required_column(row.get::<String>(2)?, "model_name"),
                model_version: required_column(row.get::<String>(3)?, "model_version"),
                dimensions: required_column(row.get::<i32>(4)?, "dimensions"),
                metric: distance_metric_from_catalog(
                    required_column(row.get::<String>(5)?, "metric"),
                    "model",
                ),
                is_active: required_column(row.get::<bool>(6)?, "is_active"),
            });
        }
        Ok::<_, spi::Error>(output)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("model version lookup failed: {error}"),
        )
    })
}

fn required_column<T>(value: Option<T>, column_name: &'static str) -> T {
    match value {
        Some(value) => value,
        None => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("model version catalog column was unexpectedly null: {column_name}"),
        ),
    }
}
