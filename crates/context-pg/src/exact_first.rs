//! Exact-first source introspection, idempotent registration, and readiness.

#![allow(
    unsafe_code,
    reason = "bounded JSONB admission inspects raw PostgreSQL datum sizes before pgrx conversion"
)]

use std::collections::BTreeSet;

use context_core::{
    CollectionName, EXACT_FIRST_MAX_COLUMNS as MAX_COLUMNS, EXACT_FIRST_MAX_INDEXES as MAX_INDEXES,
    EXACT_FIRST_MAX_JSON_DEPTH as MAX_JSON_DEPTH, EXACT_FIRST_MAX_JSON_NODES as MAX_JSON_NODES,
    EXACT_FIRST_MAX_NAME_BYTES as MAX_NAME_BYTES,
    EXACT_FIRST_MAX_OBJECTIVES_BYTES as MAX_OBJECTIVES_BYTES,
    EXACT_FIRST_MAX_SPEC_BYTES as MAX_SPEC_BYTES, ExactFirstApplyPolicy, ExactFirstReadinessFacts,
    ExactFirstReason, ExactFirstState, QualifiedTableName, derive_exact_first_readiness,
};
use pgrx::{datum::DatumWithOid, prelude::*};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::raise_sql_error;

mod advisor;
mod apply;
mod controller;
mod datum;
mod introspection;
mod registration;
pub(crate) mod search;

use datum::{BoundedExactFirstObjectives, BoundedExactFirstSpecification};
use introspection::{
    ColumnBinding, InspectedColumn, ResolvedSource, classify_column, inspect_columns,
    resolve_source, resolve_source_key,
};
use registration::{
    CollectionRegistration, ExistingRegistration, ensure_collection, insert_registration,
    load_existing_registration, load_registration_readiness,
};

const REGISTRATION_VERSION: &str = "exact_first_registration_v1";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExactFirstSpecification {
    version: String,
    key_column: String,
    bindings: Vec<ExactFirstBindingSpecification>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExactFirstBindingSpecification {
    name: String,
    column: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dimensions: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metric: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text_configuration: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    normalization: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectionOptions {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    supported_only: bool,
}

/// Inspects one ordinary PostgreSQL relation without changing registration.
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL table shape inline"
)]
#[pg_extern(name = "inspect_exact_first_source", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn inspect_exact_first_source(
    source_table: String,
    options: default!(BoundedExactFirstObjectives, "'{}'::jsonb"),
) -> TableIterator<
    'static,
    (
        name!(ordinal_position, i16),
        name!(column_name, String),
        name!(type_schema, String),
        name!(type_name, String),
        name!(formatted_type, String),
        name!(nullable, bool),
        name!(support_family, Option<String>),
        name!(supported, bool),
        name!(reason, String),
    ),
> {
    let options = parse_inspection_options(options);
    let source = resolve_source(&source_table);
    require_source_select(&source);
    let rows = inspect_columns(source.oid)
        .into_iter()
        .filter_map(|column| {
            let classification = classify_column(&column);
            if options.supported_only && classification.family.is_none() {
                return None;
            }
            Some((
                column.attnum,
                column.name,
                column.type_schema,
                column.type_name,
                column.formatted_type,
                !column.not_null,
                classification.family.map(str::to_owned),
                classification.family.is_some(),
                classification.reason.to_owned(),
            ))
        })
        .collect::<Vec<_>>();
    TableIterator::new(rows)
}

/// Registers one complete exact-first source contract idempotently.
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL table shape inline"
)]
#[pg_extern(name = "register_exact_first", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn register_exact_first(
    collection: String,
    source_table: String,
    specification: BoundedExactFirstSpecification,
    apply_policy: default!(String, "'recommend_only'"),
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(registration_revision, i64),
        name!(specification_sha256, String),
        name!(readiness_state, String),
        name!(readiness_reason, String),
        name!(plan_revision, Option<i64>),
    ),
> {
    let collection_name = CollectionName::new(collection.clone()).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        )
    });
    let apply_policy = ExactFirstApplyPolicy::from_catalog(&apply_policy).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "unsupported exact-first apply policy",
        )
    });
    if matches!(
        apply_policy,
        ExactFirstApplyPolicy::Enqueue | ExactFirstApplyPolicy::ApplyForeground
    ) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "exact-first optimization application is not enabled before the P14 supervised-build gate",
        );
    }

    let source = resolve_source(&source_table);
    require_source_select(&source);
    let collection = ensure_collection(&collection_name, &source);
    require_collection_owner(&collection);

    let mut specification = parse_specification(specification);
    normalize_specification(&mut specification);
    let columns = inspect_columns(source.oid);
    let source_key = resolve_source_key(&source, &columns, &specification.key_column);
    let bindings = resolve_bindings(&columns, &specification.bindings);
    if !bindings.iter().any(|binding| binding.kind == "dense") {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "exact-first registration requires at least one dense vector binding with a complete public exact adapter",
        );
    }
    if bindings
        .iter()
        .any(|binding| !matches!(binding.kind.as_str(), "dense" | "filter" | "payload"))
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "exact-first registration currently supports dense, filter, and payload bindings",
        );
    }
    let specification_bytes = serde_json::to_vec(&specification).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to normalize exact-first specification: {error}"),
        )
    });
    let digest = Sha256::digest(&specification_bytes).to_vec();

    let registration = match load_existing_registration(collection.collection_id) {
        Some(existing) => converge_existing_registration(&source, &source_key, &digest, existing),
        None => insert_registration(&collection, &source, &source_key, &bindings, &digest),
    };

    TableIterator::once((
        collection_name.as_str().to_owned(),
        registration.registration_revision,
        hex_digest(&digest),
        registration.readiness_state.as_catalog().to_owned(),
        registration.readiness_reason.as_catalog().to_owned(),
        registration.current_plan_revision,
    ))
}

/// Returns current exact-first readiness after catalog identity revalidation.
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL table shape inline"
)]
#[pg_extern(name = "exact_first_readiness", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn exact_first_readiness(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(collection_name, String),
        name!(registration_revision, i64),
        name!(readiness_state, String),
        name!(readiness_reason, String),
        name!(plan_revision, Option<i64>),
        name!(build_job_id, Option<i64>),
        name!(repair_advice, String),
    ),
> {
    let collection_name = CollectionName::new(collection.clone()).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        )
    });
    let collection = registration::load_collection(&collection_name);
    require_collection_owner(&collection);
    let status = load_registration_readiness(&collection);
    TableIterator::once((
        collection_name.as_str().to_owned(),
        status.registration_revision,
        status.readiness.state.as_catalog().to_owned(),
        status.readiness.reason.as_catalog().to_owned(),
        status.plan_revision,
        status.build_job_id,
        repair_advice(status.readiness.state, status.readiness.reason).to_owned(),
    ))
}

fn parse_inspection_options(options: BoundedExactFirstObjectives) -> InspectionOptions {
    let parsed = serde_json::from_value::<InspectionOptions>(options.into_json().0).unwrap_or_else(
        |error| invalid_specification(format!("invalid inspection options: {error}")),
    );
    if parsed
        .version
        .as_deref()
        .is_some_and(|version| version != "exact_first_inspection_v1")
    {
        invalid_specification("unsupported exact-first inspection version");
    }
    parsed
}

fn parse_specification(specification: BoundedExactFirstSpecification) -> ExactFirstSpecification {
    let parsed = serde_json::from_value::<ExactFirstSpecification>(specification.into_json().0)
        .unwrap_or_else(|error| {
            invalid_specification(format!("invalid exact-first specification: {error}"))
        });
    if parsed.version != REGISTRATION_VERSION {
        invalid_specification("unsupported exact-first registration version");
    }
    if parsed.bindings.is_empty() || parsed.bindings.len() > MAX_COLUMNS {
        invalid_specification("exact-first bindings exceed the registered column bound");
    }
    parsed
}

fn normalize_specification(specification: &mut ExactFirstSpecification) {
    validate_name(&specification.key_column, "source key column");
    specification
        .bindings
        .sort_by(|left, right| left.name.cmp(&right.name));
    let mut names = BTreeSet::new();
    let mut columns = BTreeSet::new();
    for binding in &specification.bindings {
        validate_name(&binding.name, "binding name");
        validate_name(&binding.column, "binding column");
        if !names.insert(binding.name.as_str()) {
            invalid_specification("exact-first binding names must be unique");
        }
        if !columns.insert(binding.column.as_str()) {
            invalid_specification("exact-first binding columns must be unique");
        }
    }
}

fn resolve_bindings(
    columns: &[InspectedColumn],
    specifications: &[ExactFirstBindingSpecification],
) -> Vec<ColumnBinding> {
    specifications
        .iter()
        .enumerate()
        .map(|(ordinal, specification)| {
            let column = columns
                .iter()
                .find(|column| column.name == specification.column)
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                        format!(
                            "exact-first source column does not exist: {}",
                            specification.column
                        ),
                    )
                });
            introspection::resolve_binding(ordinal, column, specification)
        })
        .collect()
}

fn converge_existing_registration(
    source: &ResolvedSource,
    source_key: &introspection::SourceKeyBinding,
    digest: &[u8],
    existing: ExistingRegistration,
) -> registration::StoredRegistration {
    if existing.source_schema_name != source.schema_name
        || existing.source_table_name != source.table_name
        || existing.source_table_oid != source.oid
        || existing.source_key_column_name != source_key.column.name
        || existing.source_key_attnum != source_key.column.attnum
        || existing.source_key_type_oid != source_key.column.type_oid
        || existing.source_key_typmod != source_key.column.typmod
        || existing.source_key_collation_oid != i64::from(source_key.column.collation_oid.to_u32())
        || existing.source_key_index_oid != source_key.unique_index_oid
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "exact-first source registration identity changed",
        );
    }
    if existing.specification_sha256 != digest {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DUPLICATE_OBJECT,
            "exact-first registration already exists with a different specification",
        );
    }
    registration::StoredRegistration {
        registration_revision: existing.registration_revision,
        readiness_state: ExactFirstState::from_catalog(&existing.readiness_state)
            .unwrap_or(ExactFirstState::Stale),
        readiness_reason: ExactFirstReason::from_catalog(&existing.readiness_reason)
            .unwrap_or(ExactFirstReason::ConfigurationChanged),
        current_plan_revision: existing.current_plan_revision,
    }
}

fn require_source_select(source: &ResolvedSource) {
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege(SESSION_USER, $1, 'SELECT')",
        &[source.oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check exact-first source SELECT privilege: {error}"),
        )
    })
    .unwrap_or(false);
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "permission denied for exact-first source table",
        );
    }
}

fn require_collection_owner(collection: &CollectionRegistration) {
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, $1, 'MEMBER')",
        &[collection.owner_role.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check exact-first collection ownership: {error}"),
        )
    })
    .unwrap_or(false);
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "permission denied for exact-first collection",
        );
    }
}

fn validate_name(value: &str, subject: &str) {
    if value.is_empty() || value.len() > MAX_NAME_BYTES || value.chars().any(char::is_control) {
        invalid_specification(format!(
            "{subject} is empty, oversized, or contains control characters"
        ));
    }
}

fn invalid_specification(message: impl Into<String>) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE, message)
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

const fn repair_advice(state: ExactFirstState, reason: ExactFirstReason) -> &'static str {
    match (state, reason) {
        (ExactFirstState::ExactOnly, _) => "review or enqueue a current exact-first plan",
        (ExactFirstState::Building, _) => {
            "monitor exact_first_progress; exact serving remains available"
        }
        (ExactFirstState::Indexed, _) => "no repair required",
        (ExactFirstState::Stale, ExactFirstReason::SourceRelationChanged) => {
            "restore the registered source relation identity or register a new revision"
        }
        (ExactFirstState::Stale, ExactFirstReason::SourceKeyChanged) => {
            "restore the unique non-null source-key contract or register a new revision"
        }
        (ExactFirstState::Stale, _) => {
            "restore the registered source/configuration binding or register a new revision"
        }
        (ExactFirstState::Degraded, _) => {
            "inspect the current plan failure and retry or retain exact-only serving"
        }
    }
}

fn required<T>(value: Option<T>, subject: &'static str) -> T {
    value.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("exact-first catalog {subject} is null"),
        )
    })
}
