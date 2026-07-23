use pgrx::prelude::*;
use std::cell::Cell;

use super::validation::{
    dependency_inventory, ensure_certified_bridge, ensure_index_build_privileges,
    ensure_no_blockers, resolve_conversion_target,
};
use crate::error::raise_sql_error;

#[derive(Debug, Clone)]
pub(super) struct ConversionState {
    pub(super) conversion_id: i64,
    pub(super) owner_role: pg_sys::Oid,
    pub(super) source_table_oid: pg_sys::Oid,
    pub(super) source_schema_name: String,
    pub(super) source_table_name: String,
    pub(super) source_column_name: String,
    pub(super) source_attnum: i16,
    pub(super) source_type_oid: pg_sys::Oid,
    pub(super) source_type_name: String,
    pub(super) source_typmod: i32,
    pub(super) shadow_column_name: String,
    pub(super) shadow_attnum: Option<i16>,
    pub(super) backup_column_name: String,
    pub(super) trigger_name: String,
    pub(super) index_name: String,
    pub(super) mode: String,
    pub(super) metric: String,
    pub(super) status: String,
    pub(super) dependency_manifest: Vec<String>,
    pub(super) validation_attestations: Vec<String>,
    pub(super) total_rows: i64,
    pub(super) processed_rows: i64,
    pub(super) mismatch_count: i64,
    pub(super) backfill_cursor: String,
    pub(super) source_checksum: Option<String>,
    pub(super) shadow_checksum: Option<String>,
    pub(super) error_message: Option<String>,
}

const STATE_COLUMNS: &str = r"
conversion_id,
owner_role,
source_table_oid,
source_schema_name::text,
source_table_name::text,
source_column_name::text,
source_attnum,
source_type_oid,
source_type_name,
source_typmod,
shadow_column_name::text,
shadow_attnum,
backup_column_name::text,
trigger_name::text,
index_name::text,
mode,
metric,
status,
dependency_manifest,
validation_attestations,
total_rows,
processed_rows,
mismatch_count,
backfill_cursor,
source_checksum,
shadow_checksum,
error_message";

pub(super) fn load_visible_conversion(conversion_id: i64) -> ConversionState {
    let sql = format!(
        "SELECT {STATE_COLUMNS}
           FROM pgcontext._visible_pgvector_ownership_conversions
          WHERE conversion_id = $1"
    );
    Spi::connect(|client| {
        let row = client.select(&sql, None, &[conversion_id.into()])?.first();
        if row.is_empty() {
            return Ok(None);
        }
        Ok::<_, spi::Error>(Some(state_from_row(&row)?))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to read pgvector conversion {conversion_id}: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("pgvector ownership conversion does not exist: {conversion_id}"),
        )
    })
}

fn state_from_row(row: &spi::SpiTupleTable<'_>) -> Result<ConversionState, spi::Error> {
    Ok(ConversionState {
        conversion_id: required(row.get(1)?, "conversion_id"),
        owner_role: required(row.get(2)?, "owner_role"),
        source_table_oid: required(row.get(3)?, "source_table_oid"),
        source_schema_name: required(row.get(4)?, "source_schema_name"),
        source_table_name: required(row.get(5)?, "source_table_name"),
        source_column_name: required(row.get(6)?, "source_column_name"),
        source_attnum: required(row.get(7)?, "source_attnum"),
        source_type_oid: required(row.get(8)?, "source_type_oid"),
        source_type_name: required(row.get(9)?, "source_type_name"),
        source_typmod: required(row.get(10)?, "source_typmod"),
        shadow_column_name: required(row.get(11)?, "shadow_column_name"),
        shadow_attnum: row.get(12)?,
        backup_column_name: required(row.get(13)?, "backup_column_name"),
        trigger_name: required(row.get(14)?, "trigger_name"),
        index_name: required(row.get(15)?, "index_name"),
        mode: required(row.get(16)?, "mode"),
        metric: required(row.get(17)?, "metric"),
        status: required(row.get(18)?, "status"),
        dependency_manifest: row.get(19)?.unwrap_or_default(),
        validation_attestations: row.get(20)?.unwrap_or_default(),
        total_rows: required(row.get(21)?, "total_rows"),
        processed_rows: required(row.get(22)?, "processed_rows"),
        mismatch_count: required(row.get(23)?, "mismatch_count"),
        backfill_cursor: required(row.get(24)?, "backfill_cursor"),
        source_checksum: row.get(25)?,
        shadow_checksum: row.get(26)?,
        error_message: row.get(27)?,
    })
}

fn required<T>(value: Option<T>, column: &'static str) -> T {
    value.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("pgvector conversion column is unexpectedly null: {column}"),
        )
    })
}

/// Inserts one private conversion row after re-deriving the caller-owned
/// pgvector target. This helper performs no DDL.
#[pg_extern(name = "_begin_pgvector_ownership_conversion", security_definer)]
#[search_path(pg_catalog, pgcontext)]
fn begin_conversion_catalog(
    source_table_oid: pg_sys::Oid,
    source_column_name: String,
    mode: String,
    metric: String,
    dependency_manifest: Vec<String>,
    validation_attestations: Vec<String>,
) -> i64 {
    require_catalog_write_capability();
    ensure_certified_bridge();
    let target = resolve_conversion_target(source_table_oid, &source_column_name);
    let inventory = dependency_inventory(&target, None, false);
    ensure_no_blockers(&inventory);
    let certified_source_indexes = super::validation::collect_fast_index_plans(&target);
    if mode == "restricted_online" || !certified_source_indexes.is_empty() {
        ensure_index_build_privileges(&target, &certified_source_indexes);
    }
    if mode == "restricted_online" {
        super::validation::ensure_online_index_profile(&target, &metric);
    }
    if inventory.manifest != dependency_manifest {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "conversion dependencies changed while the job was being created",
        );
    }
    if !matches!(mode.as_str(), "fast" | "restricted_online") {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported pgvector ownership conversion mode: {mode}"),
        );
    }
    if mode == "fast" && target.source_type_name == "sparsevec" {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "pgvector sparsevec ownership conversion requires restricted_online mode",
        );
    }
    if super::validation::canonical_opclass(&target.source_type_name, &metric).is_none() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "unsupported conversion metric {metric} for {}",
                target.source_type_name
            ),
        );
    }
    if mode == "restricted_online"
        && !validation_attestations
            .iter()
            .any(|attestation| attestation == "application_uses_column_lists")
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "restricted-online conversion requires application_uses_column_lists attestation",
        );
    }
    if !validation_attestations
        .iter()
        .any(|attestation| attestation == "application_dependencies_reviewed")
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "ownership conversion requires application_dependencies_reviewed attestation",
        );
    }
    let active = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pgcontext._pgvector_ownership_conversions
              WHERE source_table_oid = $1
                AND source_attnum = $2
                AND status IN ('planned', 'backfilling', 'index_pending', 'ready', 'cutover', 'failed')
         )",
        &[source_table_oid.into(), i32::from(target.attnum).into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if active {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_IN_USE,
            format!(
                "an active ownership conversion already targets {}.{}.{}",
                target.schema_name, target.table_name, target.column_name
            ),
        );
    }

    let conversion_id = Spi::get_one_with_args::<i64>(
        "INSERT INTO pgcontext._pgvector_ownership_conversions (
             owner_role,
             source_table_oid,
             source_schema_name,
             source_table_name,
             source_column_name,
             source_attnum,
             source_type_oid,
             source_type_name,
             source_typmod,
             shadow_column_name,
             backup_column_name,
             trigger_name,
             index_name,
             mode,
             metric,
             dependency_manifest,
             validation_attestations
         ) VALUES (
             $1, $2, $3, $4, $5, $6, $7, $8, $9,
             '__pending_shadow', '__pending_backup', '__pending_trigger', '__pending_index',
             $10, $11, $12, $13
         )
         RETURNING conversion_id",
        &[
            target.owner_role.into(),
            target.table_oid.into(),
            target.schema_name.into(),
            target.table_name.into(),
            target.column_name.into(),
            i32::from(target.attnum).into(),
            target.source_type_oid.into(),
            target.source_type_name.into(),
            target.source_typmod.into(),
            mode.into(),
            metric.into(),
            dependency_manifest.into(),
            validation_attestations.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to create pgvector ownership conversion: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "pgvector ownership conversion insert returned no identifier",
        )
    });
    Spi::run_with_args(
        "UPDATE pgcontext._pgvector_ownership_conversions
            SET shadow_column_name = ('__pgcontext_' || conversion_id || '_new')::name,
                backup_column_name = ('__pgcontext_' || conversion_id || '_old')::name,
                trigger_name = ('pgcontext_pgv_' || conversion_id || '_sync')::name,
                index_name = ('pgcontext_pgv_' || conversion_id || '_hnsw')::name,
                updated_at = pg_catalog.now()
          WHERE conversion_id = $1",
        &[conversion_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to bind pgvector conversion object names: {error}"),
        )
    });
    conversion_id
}

/// Advances a caller-owned private conversion row through its constrained
/// state machine. Public orchestration revalidates physical state before each
/// call; this helper owns only catalog writes.
#[allow(clippy::too_many_arguments)]
#[pg_extern(name = "_transition_pgvector_ownership_conversion", security_definer)]
#[search_path(pg_catalog, pgcontext)]
fn transition_conversion_catalog(
    conversion_id: i64,
    expected_status: String,
    new_status: String,
    shadow_attnum: Option<i16>,
    total_rows: i64,
    processed_rows: i64,
    mismatch_count: i64,
    backfill_cursor: Option<String>,
    source_checksum: Option<String>,
    shadow_checksum: Option<String>,
    attestation: Option<String>,
    error_message: Option<String>,
) {
    require_catalog_write_capability();
    let current = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT status::text, mode::text, owner_role
                   FROM pgcontext._pgvector_ownership_conversions
                  WHERE conversion_id = $1
                  FOR UPDATE",
                None,
                &[conversion_id.into()],
            )?
            .first();
        if row.is_empty() {
            return Ok(None);
        }
        Ok::<_, spi::Error>(Some((
            row.get::<String>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<pg_sys::Oid>(3)?.unwrap_or(pg_sys::InvalidOid),
        )))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to lock pgvector ownership conversion: {error}"),
        )
    });
    let Some((current_status, mode, owner_role)) = current else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            format!("pgvector ownership conversion does not exist: {conversion_id}"),
        );
    };
    let authorized = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, $1::oid, 'MEMBER')",
        &[owner_role.into()],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if !authorized {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for pgvector ownership conversion {conversion_id}"),
        );
    }
    if current_status != expected_status {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "conversion {conversion_id} status changed: expected {expected_status}, found {current_status}"
            ),
        );
    }
    if !allowed_transition(&mode, &current_status, &new_status) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "invalid {mode} ownership conversion transition: {current_status} -> {new_status}"
            ),
        );
    }
    if total_rows < 0 || processed_rows < 0 || processed_rows > total_rows || mismatch_count < 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "pgvector ownership conversion progress is invalid",
        );
    }
    Spi::run_with_args(
        "UPDATE pgcontext._pgvector_ownership_conversions
            SET status = $2,
                shadow_attnum = coalesce($3, shadow_attnum),
                total_rows = $4,
                processed_rows = $5,
                mismatch_count = $6,
                backfill_cursor = coalesce($7, backfill_cursor),
                source_checksum = $8,
                shadow_checksum = $9,
                validation_attestations = CASE
                    WHEN $10::text IS NULL OR $10 = ANY (validation_attestations)
                    THEN validation_attestations
                    ELSE pg_catalog.array_append(validation_attestations, $10)
                END,
                error_message = $11,
                started_at = coalesce(started_at, pg_catalog.now()),
                updated_at = pg_catalog.now(),
                completed_at = CASE
                    WHEN $2 IN ('completed', 'rolled_back') THEN pg_catalog.now()
                    ELSE NULL
                END
          WHERE conversion_id = $1",
        &[
            conversion_id.into(),
            new_status.into(),
            shadow_attnum.into(),
            total_rows.into(),
            processed_rows.into(),
            mismatch_count.into(),
            backfill_cursor.into(),
            source_checksum.into(),
            shadow_checksum.into(),
            attestation.into(),
            error_message.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to update pgvector ownership conversion: {error}"),
        )
    });
}

fn allowed_transition(mode: &str, current: &str, next: &str) -> bool {
    matches!(
        (mode, current, next),
        ("fast", "planned", "completed" | "failed" | "rolled_back")
            | ("fast", "failed", "planned" | "rolled_back")
            | (
                "restricted_online",
                "planned",
                "backfilling" | "rolled_back"
            )
            | (
                "restricted_online",
                "backfilling",
                "backfilling" | "index_pending" | "failed" | "rolled_back"
            )
            | (
                "restricted_online",
                "index_pending",
                "backfilling" | "ready" | "failed" | "rolled_back"
            )
            | (
                "restricted_online",
                "ready",
                "backfilling" | "cutover" | "failed" | "rolled_back"
            )
            | (
                "restricted_online",
                "cutover",
                "completed" | "failed" | "rolled_back"
            )
            | ("restricted_online", "failed", "backfilling" | "rolled_back")
    )
}

pub(super) fn begin_catalog(
    target: &super::validation::ConversionTarget,
    mode: &str,
    metric: &str,
    manifest: &[String],
    attestations: &[String],
) -> i64 {
    with_catalog_write_capability(|| {
        Spi::get_one_with_args::<i64>(
            "SELECT pgcontext._begin_pgvector_ownership_conversion($1, $2, $3, $4, $5, $6)",
            &[
                target.table_oid.into(),
                target.column_name.clone().into(),
                mode.into(),
                metric.into(),
                manifest.to_vec().into(),
                attestations.to_vec().into(),
            ],
        )
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to call pgvector conversion catalog helper: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "pgvector conversion catalog helper returned no identifier",
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn transition_catalog(
    state: &ConversionState,
    new_status: &str,
    shadow_attnum: Option<i16>,
    total_rows: i64,
    processed_rows: i64,
    mismatch_count: i64,
    backfill_cursor: Option<&str>,
    source_checksum: Option<String>,
    shadow_checksum: Option<String>,
    attestation: Option<&str>,
    error_message: Option<String>,
) {
    with_catalog_write_capability(|| {
        Spi::run_with_args(
            "SELECT pgcontext._transition_pgvector_ownership_conversion(
             $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12
         )",
            &[
                state.conversion_id.into(),
                state.status.clone().into(),
                new_status.into(),
                shadow_attnum.into(),
                total_rows.into(),
                processed_rows.into(),
                mismatch_count.into(),
                backfill_cursor.map(str::to_owned).into(),
                source_checksum.into(),
                shadow_checksum.into(),
                attestation.map(str::to_owned).into(),
                error_message.into(),
            ],
        )
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to persist pgvector conversion transition: {error}"),
        )
    });
}

thread_local! {
    static CATALOG_WRITE_CAPABILITY_DEPTH: Cell<u32> = const { Cell::new(0) };
}

struct CatalogWriteCapabilityGuard;

impl Drop for CatalogWriteCapabilityGuard {
    fn drop(&mut self) {
        CATALOG_WRITE_CAPABILITY_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

fn with_catalog_write_capability<T>(operation: impl FnOnce() -> T) -> T {
    CATALOG_WRITE_CAPABILITY_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
    let guard = CatalogWriteCapabilityGuard;
    let result = operation();
    drop(guard);
    result
}

fn require_catalog_write_capability() {
    let authorized = CATALOG_WRITE_CAPABILITY_DEPTH.with(|depth| depth.get() > 0);
    if !authorized {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "pgvector ownership catalog helpers are internal",
        );
    }
}
