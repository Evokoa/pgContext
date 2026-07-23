//! Persisted fast and restricted-online pgvector ownership conversion.

#![allow(
    clippy::type_complexity,
    reason = "pgrx requires named table-return columns inline in pg_extern signatures"
)]

mod execution;
mod index;
mod persistence;
mod trigger;
mod validation;

use pgrx::PgRelation;
use pgrx::prelude::*;

use execution::{
    acknowledge_ready_index, advisory_lock, backfill_online_batch, cutover_online_conversion,
    finalize_online_conversion, initialize_online_conversion, lock_conversion_target,
    rollback_online_conversion, run_fast_conversion, validate_pre_cutover_state,
};
use index::{
    TargetIndexState, create_index_command, drop_invalid_index_command, target_index_state,
};
use persistence::{ConversionState, begin_catalog, load_visible_conversion};
use validation::{
    canonical_opclass, collect_fast_index_plans, dependency_inventory, ensure_certified_bridge,
    ensure_index_build_privileges, ensure_no_blockers, ensure_online_index_profile,
    resolve_conversion_target,
};

type ConversionResultRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Vec<String>,
    Option<String>,
    Option<String>,
);

/// Starts an atomic fast or persisted restricted-online pgvector ownership
/// conversion. Restricted-online mode immediately installs a certified binary
/// dual-write shadow and requires the caller to attest that application INSERTs
/// use column lists because the table row shape changes during migration. All
/// modes require an explicit attestation for dependencies hidden in application
/// SQL or string-bodied stored functions, which PostgreSQL cannot inventory.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn start_pgvector_ownership_conversion(
    target: PgRelation,
    column_name: String,
    mode: default!(String, "'fast'"),
    metric: default!(String, "'cosine'"),
    application_uses_column_lists: default!(bool, false),
    application_dependencies_reviewed: default!(bool, false),
) -> TableIterator<
    'static,
    (
        name!(conversion_id, i64),
        name!(mode, String),
        name!(status, String),
        name!(schema_name, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(target_type, String),
        name!(total_rows, i64),
        name!(processed_rows, i64),
        name!(mismatch_count, i64),
        name!(validation_attestations, Vec<String>),
        name!(next_command, Option<String>),
        name!(error_message, Option<String>),
    ),
> {
    ensure_certified_bridge();
    let mode = match mode.as_str() {
        "fast" => "fast",
        "online" | "restricted_online" => "restricted_online",
        _ => {
            crate::error::raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("unsupported pgvector ownership conversion mode: {mode}"),
            );
        }
    };
    let table_oid = target.oid();
    drop(target);
    let mut conversion_target = resolve_conversion_target(table_oid, &column_name);
    if mode == "fast" && conversion_target.source_type_name == "sparsevec" {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "pgvector sparsevec has a different physical layout from pgContext sparsevec; \
             use mode => 'restricted_online' for a validated, resumable conversion",
        );
    }
    if canonical_opclass(&conversion_target.source_type_name, &metric).is_none() {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "unsupported conversion metric {metric} for {}",
                conversion_target.source_type_name
            ),
        );
    }
    if mode == "restricted_online" && !application_uses_column_lists {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "restricted-online conversion changes the table row shape and requires \
             application_uses_column_lists => true",
        );
    }
    if !application_dependencies_reviewed {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "ownership conversion requires application_dependencies_reviewed => true because \
             PostgreSQL cannot inventory dependencies in application SQL or string-bodied \
             stored functions",
        );
    }
    if mode == "restricted_online" {
        lock_conversion_target(table_oid);
        conversion_target = resolve_conversion_target(table_oid, &column_name);
    }
    let inventory = dependency_inventory(&conversion_target, None, false);
    ensure_no_blockers(&inventory);
    let certified_source_indexes = collect_fast_index_plans(&conversion_target);
    if mode == "restricted_online" || !certified_source_indexes.is_empty() {
        ensure_index_build_privileges(&conversion_target, &certified_source_indexes);
    }
    if mode == "restricted_online" {
        ensure_online_index_profile(&conversion_target, &metric);
    }
    let mut attestations = vec!["application_dependencies_reviewed".to_owned()];
    if application_uses_column_lists {
        attestations.push("application_uses_column_lists".to_owned());
    }
    let conversion_id = begin_catalog(
        &conversion_target,
        mode,
        &metric,
        &inventory.manifest,
        &attestations,
    );
    let mut state = load_visible_conversion(conversion_id);
    if mode == "restricted_online" {
        validate_pre_cutover_state(&state, false);
        initialize_online_conversion(&state, &conversion_target);
        state = load_visible_conversion(conversion_id);
    }
    TableIterator::once(conversion_result(&state))
}

/// Runs one bounded conversion step. Fast mode completes atomically under an
/// ACCESS EXCLUSIVE lock after explicit session-drain attestation. Online mode
/// commits at most one backfill batch or acknowledges a top-level concurrent
/// index build that the caller ran from `next_command`.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn run_pgvector_ownership_conversion(
    conversion_id: i64,
    batch_size: default!(i32, 1_000),
    sessions_drained: default!(bool, false),
) -> TableIterator<
    'static,
    (
        name!(conversion_id, i64),
        name!(mode, String),
        name!(status, String),
        name!(schema_name, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(target_type, String),
        name!(total_rows, i64),
        name!(processed_rows, i64),
        name!(mismatch_count, i64),
        name!(validation_attestations, Vec<String>),
        name!(next_command, Option<String>),
        name!(error_message, Option<String>),
    ),
> {
    if !(1..=100_000).contains(&batch_size) {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "pgvector conversion batch_size must be between 1 and 100000",
        );
    }
    ensure_certified_bridge();
    advisory_lock(conversion_id);
    let state = load_visible_conversion(conversion_id);
    match (state.mode.as_str(), state.status.as_str()) {
        ("fast", "planned") => {
            if !sessions_drained {
                crate::error::raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "fast conversion requires sessions_drained => true",
                );
            }
            run_fast_conversion(&state);
        }
        ("restricted_online", "backfilling") => {
            backfill_online_batch(&state, batch_size);
        }
        ("restricted_online", "index_pending") => {
            if target_index_state(&state) == TargetIndexState::Ready {
                acknowledge_ready_index(&state);
            }
        }
        ("restricted_online", "ready") => acknowledge_ready_index(&state),
        (_, "failed") => crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("conversion {conversion_id} is failed and must be rolled back before retry"),
        ),
        (_, "planned" | "cutover" | "completed" | "rolled_back") => {}
        (mode, status) => crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("unexpected {mode} conversion status: {status}"),
        ),
    }
    let updated = load_visible_conversion(conversion_id);
    TableIterator::once(conversion_result(&updated))
}

/// Performs the short restricted-online name swap after the concurrent index
/// is ready. All application sessions must be drained and later reprepare SQL
/// against the pgContext-owned column type.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn cutover_pgvector_ownership_conversion(
    conversion_id: i64,
    sessions_drained: default!(bool, false),
) -> TableIterator<
    'static,
    (
        name!(conversion_id, i64),
        name!(mode, String),
        name!(status, String),
        name!(schema_name, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(target_type, String),
        name!(total_rows, i64),
        name!(processed_rows, i64),
        name!(mismatch_count, i64),
        name!(validation_attestations, Vec<String>),
        name!(next_command, Option<String>),
        name!(error_message, Option<String>),
    ),
> {
    if !sessions_drained {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "ownership conversion cutover requires sessions_drained => true",
        );
    }
    ensure_certified_bridge();
    advisory_lock(conversion_id);
    let state = load_visible_conversion(conversion_id);
    if state.mode != "restricted_online" || state.status != "ready" {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("conversion {conversion_id} is not ready for online cutover"),
        );
    }
    cutover_online_conversion(&state);
    let updated = load_visible_conversion(conversion_id);
    TableIterator::once(conversion_result(&updated))
}

/// Irreversibly removes the synchronized legacy pgvector column after cutover.
/// This is the boundary after which the conversion can no longer roll back.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn finalize_pgvector_ownership_conversion(
    conversion_id: i64,
) -> TableIterator<
    'static,
    (
        name!(conversion_id, i64),
        name!(mode, String),
        name!(status, String),
        name!(schema_name, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(target_type, String),
        name!(total_rows, i64),
        name!(processed_rows, i64),
        name!(mismatch_count, i64),
        name!(validation_attestations, Vec<String>),
        name!(next_command, Option<String>),
        name!(error_message, Option<String>),
    ),
> {
    advisory_lock(conversion_id);
    let state = load_visible_conversion(conversion_id);
    if state.mode != "restricted_online" || state.status != "cutover" {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("conversion {conversion_id} has not reached cutover"),
        );
    }
    finalize_online_conversion(&state);
    let updated = load_visible_conversion(conversion_id);
    TableIterator::once(conversion_result(&updated))
}

/// Restores the original pgvector column and indexes before finalization.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
fn rollback_pgvector_ownership_conversion(
    conversion_id: i64,
) -> TableIterator<
    'static,
    (
        name!(conversion_id, i64),
        name!(mode, String),
        name!(status, String),
        name!(schema_name, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(target_type, String),
        name!(total_rows, i64),
        name!(processed_rows, i64),
        name!(mismatch_count, i64),
        name!(validation_attestations, Vec<String>),
        name!(next_command, Option<String>),
        name!(error_message, Option<String>),
    ),
> {
    advisory_lock(conversion_id);
    let state = load_visible_conversion(conversion_id);
    if matches!(state.status.as_str(), "completed" | "rolled_back") {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "conversion {conversion_id} cannot roll back from {}",
                state.status
            ),
        );
    }
    rollback_online_conversion(&state);
    let updated = load_visible_conversion(conversion_id);
    TableIterator::once(conversion_result(&updated))
}

/// Lists persisted conversions visible to the current table owner role.
#[pg_extern]
#[search_path(pg_catalog, pgcontext)]
fn pgvector_ownership_conversions() -> TableIterator<
    'static,
    (
        name!(conversion_id, i64),
        name!(mode, String),
        name!(status, String),
        name!(schema_name, String),
        name!(table_name, String),
        name!(column_name, String),
        name!(target_type, String),
        name!(total_rows, i64),
        name!(processed_rows, i64),
        name!(mismatch_count, i64),
        name!(validation_attestations, Vec<String>),
        name!(next_command, Option<String>),
        name!(error_message, Option<String>),
    ),
> {
    let conversion_ids = Spi::connect(|client| {
        let rows = client.select(
            "SELECT conversion_id
               FROM pgcontext._visible_pgvector_ownership_conversions
              ORDER BY conversion_id",
            None,
            &[],
        )?;
        rows.map(|row| Ok::<_, spi::Error>(row.get::<i64>(1)?.unwrap_or_default()))
            .collect::<Result<Vec<_>, _>>()
    })
    .unwrap_or_else(|error| {
        crate::error::raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to list pgvector ownership conversions: {error}"),
        )
    });
    TableIterator::new(
        conversion_ids
            .into_iter()
            .map(load_visible_conversion)
            .map(|state| conversion_result(&state)),
    )
}

fn conversion_result(state: &ConversionState) -> ConversionResultRow {
    let next_command = if state.mode == "restricted_online" && state.status == "index_pending" {
        match target_index_state(state) {
            TargetIndexState::Missing => Some(create_index_command(state)),
            TargetIndexState::Invalid => Some(drop_invalid_index_command(state)),
            TargetIndexState::Ready => None,
        }
    } else {
        None
    };
    (
        state.conversion_id,
        state.mode.clone(),
        state.status.clone(),
        state.source_schema_name.clone(),
        state.source_table_name.clone(),
        state.source_column_name.clone(),
        format!("pgcontext.{}", state.source_type_name),
        state.total_rows,
        state.processed_rows,
        state.mismatch_count,
        state.validation_attestations.clone(),
        next_command,
        state.error_message.clone(),
    )
}

pub(super) fn quote_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(super) fn qualified_relation(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(table))
}
