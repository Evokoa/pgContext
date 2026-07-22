use pgrx::prelude::*;

use super::index::{TargetIndexState, target_index_state};
use super::persistence::{ConversionState, transition_catalog};
use super::validation::{
    ConversionTarget, collect_fast_index_plans, dependency_inventory, ensure_no_blockers,
    resolve_conversion_target, target_type_sql,
};
use super::{qualified_relation, quote_ident};
use crate::error::raise_sql_error;

#[derive(Debug, Clone)]
pub(super) struct ConversionStats {
    pub(super) total_rows: i64,
    pub(super) processed_rows: i64,
    pub(super) mismatch_count: i64,
    pub(super) source_checksum: Option<String>,
    pub(super) shadow_checksum: Option<String>,
}

pub(super) fn advisory_lock(conversion_id: i64) {
    Spi::run_with_args(
        "SELECT pg_catalog.pg_advisory_xact_lock($1)",
        &[conversion_id.wrapping_add(0x5047_4354_0000_0000).into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to lock pgvector conversion {conversion_id}: {error}"),
        )
    });
}

pub(super) fn lock_conversion_target(table_oid: pg_sys::Oid) {
    // SAFETY: PostgreSQL validates the catalog OID and owns the transaction
    // lock lifecycle. Binding the lock to the OID avoids name-reuse races.
    unsafe {
        pg_sys::LockRelationOid(table_oid, pg_sys::AccessExclusiveLock.cast_signed());
    }
}

pub(super) fn lock_source_table(state: &ConversionState) {
    lock_relation_oid(state, pg_sys::AccessExclusiveLock.cast_signed());
}

fn lock_source_table_for_dml(state: &ConversionState) {
    lock_relation_oid(state, pg_sys::RowExclusiveLock.cast_signed());
}

fn lock_source_table_for_read(state: &ConversionState) {
    lock_relation_oid(state, pg_sys::AccessShareLock.cast_signed());
}

fn lock_relation_oid(state: &ConversionState, lock_mode: pg_sys::LOCKMODE) {
    // SAFETY: The persisted OID is treated only as an input to PostgreSQL's
    // lock manager, which errors if it no longer identifies a live relation.
    unsafe {
        pg_sys::LockRelationOid(state.source_table_oid, lock_mode);
    }
    let binding_matches = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_class AS relation
               JOIN pg_catalog.pg_namespace AS namespace
                 ON namespace.oid = relation.relnamespace
              WHERE relation.oid = $1
                AND namespace.nspname = $2
                AND relation.relname = $3
                AND relation.relowner = $4
                AND relation.relkind = 'r'
                AND relation.relpersistence = 'p'
         )",
        &[
            state.source_table_oid.into(),
            state.source_schema_name.clone().into(),
            state.source_table_name.clone().into(),
            state.owner_role.into(),
        ],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if !binding_matches {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "conversion source binding changed: {}.{}",
                state.source_schema_name, state.source_table_name
            ),
        );
    }
}

pub(super) fn initialize_online_conversion(state: &ConversionState, target: &ConversionTarget) {
    let relation = qualified_relation(&target.schema_name, &target.table_name);
    let shadow = quote_ident(&state.shadow_column_name);
    let target_type = target_type_sql(target);
    Spi::run(&format!(
        "ALTER TABLE {relation} ADD COLUMN {shadow} {target_type}"
    ))
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to add pgvector conversion shadow column: {error}"),
        )
    });
    let shadow_attnum = resolve_attnum(state.source_table_oid, &state.shadow_column_name);
    create_sync_trigger(state, state.source_attnum, shadow_attnum);
    ensure_sync_trigger(state, state.source_attnum, shadow_attnum);
    transition_catalog(
        state,
        "backfilling",
        Some(shadow_attnum),
        0,
        0,
        0,
        Some("(0,0)"),
        None,
        None,
        None,
        None,
    );
}

pub(super) fn validate_pre_cutover_state(
    state: &ConversionState,
    check_prepared_statements: bool,
) -> ConversionTarget {
    let target = resolve_conversion_target(state.source_table_oid, &state.source_column_name);
    if target.schema_name != state.source_schema_name
        || target.table_name != state.source_table_name
        || target.attnum != state.source_attnum
        || target.owner_role != state.owner_role
        || target.source_type_oid != state.source_type_oid
        || target.source_typmod != state.source_typmod
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "pgvector conversion {} source binding drifted",
                state.conversion_id
            ),
        );
    }
    let ignored_trigger =
        (state.mode == "restricted_online").then_some(state.trigger_name.as_str());
    let inventory = dependency_inventory(&target, ignored_trigger, check_prepared_statements);
    ensure_no_blockers(&inventory);
    if inventory.manifest != state.dependency_manifest {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "pgvector conversion {} dependency manifest changed",
                state.conversion_id
            ),
        );
    }
    target
}

pub(super) fn run_fast_conversion(state: &ConversionState) {
    lock_source_table(state);
    let target = validate_pre_cutover_state(state, true);
    let index_plans = collect_fast_index_plans(&target);
    let relation = qualified_relation(&state.source_schema_name, &state.source_table_name);
    let relfilenode_before = relation_filenode(state.source_table_oid);
    for plan in &index_plans {
        Spi::run(&format!(
            "DROP INDEX {}.{}",
            quote_ident(&state.source_schema_name),
            quote_ident(&plan.index_name)
        ))
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to drop source index {}: {error}", plan.index_name),
            )
        });
    }
    Spi::run(&format!(
        "ALTER TABLE {relation} ALTER COLUMN {} TYPE pgcontext.{}",
        quote_ident(&state.source_column_name),
        state.source_type_name,
    ))
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to convert pgvector column ownership: {error}"),
        )
    });
    if state.source_typmod > 0 {
        let dimension_function = match state.source_type_name.as_str() {
            "vector" => "vector_dims",
            "halfvec" => "halfvec_dims",
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "certified fast conversion has an unexpected source type",
            ),
        };
        let dimension_constraint = format!("pgcontext_pgv_{}_dims", state.conversion_id);
        Spi::run(&format!(
            "ALTER TABLE {relation}
             ADD CONSTRAINT {} CHECK (
                 {} IS NULL OR pgcontext.{dimension_function}({}) = {}
             )",
            quote_ident(&dimension_constraint),
            quote_ident(&state.source_column_name),
            quote_ident(&state.source_column_name),
            state.source_typmod,
        ))
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to preserve converted vector dimensions: {error}"),
            )
        });
    }
    for plan in &index_plans {
        let options = if plan.options.is_empty() {
            String::new()
        } else {
            format!(" WITH ({})", plan.options.join(", "))
        };
        let tablespace = plan
            .tablespace
            .as_deref()
            .map(|name| format!(" TABLESPACE {}", quote_ident(name)))
            .unwrap_or_default();
        Spi::run(&format!(
            "CREATE INDEX {} ON {relation} USING pgcontext_hnsw ({} {}){options}{tablespace}",
            quote_ident(&plan.index_name),
            quote_ident(&state.source_column_name),
            plan.canonical_opclass,
        ))
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!(
                    "failed to rebuild converted index {}: {error}",
                    plan.index_name
                ),
            )
        });
    }
    if relation_filenode(state.source_table_oid) != relfilenode_before {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_EXCEPTION,
            "certified fast conversion unexpectedly rewrote the source heap",
        );
    }
    ensure_column_binding(
        state,
        &state.source_column_name,
        state.source_attnum,
        -1,
        "pgcontext",
    );
    let (total_rows, checksum) = single_column_stats(state, &state.source_column_name);
    transition_catalog(
        state,
        "completed",
        None,
        total_rows,
        total_rows,
        0,
        None,
        checksum.clone(),
        checksum,
        Some("sessions_drained"),
        None,
    );
}

pub(super) fn backfill_online_batch(state: &ConversionState, batch_size: i32) {
    lock_source_table_for_dml(state);
    let target = validate_pre_cutover_state(state, false);
    let shadow_attnum = required_shadow_attnum(state);
    ensure_column_binding(
        state,
        &state.shadow_column_name,
        shadow_attnum,
        state.source_typmod,
        "pgcontext",
    );
    ensure_sync_trigger(state, state.source_attnum, shadow_attnum);
    let relation = qualified_relation(&state.source_schema_name, &state.source_table_name);
    let source = quote_ident(&state.source_column_name);
    let shadow = quote_ident(&state.shadow_column_name);
    let target_type = target_type_sql(&target);
    let sql = format!(
        "WITH batch AS MATERIALIZED (
             SELECT ctid
               FROM {relation}
              WHERE ctid > $1::tid
              ORDER BY ctid
              LIMIT $2
              FOR UPDATE SKIP LOCKED
         ), updated AS (
             UPDATE {relation} AS source_table
                SET {shadow} = source_table.{source}::{target_type}
               FROM batch
              WHERE source_table.ctid = batch.ctid
                AND source_table.{source}::text IS DISTINCT FROM source_table.{shadow}::text
             RETURNING batch.ctid
         )
         SELECT (SELECT count(*)::bigint FROM batch),
                (SELECT count(*)::bigint FROM updated),
                (SELECT ctid::text FROM batch ORDER BY ctid DESC LIMIT 1)"
    );
    let (selected_rows, updated_rows, next_cursor) = Spi::connect(|client| {
        let row = client
            .select(
                &sql,
                None,
                &[state.backfill_cursor.clone().into(), batch_size.into()],
            )?
            .first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<i64>(2)?.unwrap_or_default(),
            row.get::<String>(3)?,
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to backfill pgvector ownership batch: {error}"),
        )
    });
    let next_cursor = next_cursor.unwrap_or_else(|| state.backfill_cursor.clone());
    if selected_rows < i64::from(batch_size) {
        let stats = column_stats(state, &state.source_column_name, &state.shadow_column_name);
        let (new_status, persisted_cursor) = if stats.mismatch_count == 0 {
            ("index_pending", next_cursor.as_str())
        } else {
            ("backfilling", "(0,0)")
        };
        transition_catalog(
            state,
            new_status,
            Some(shadow_attnum),
            stats.total_rows,
            stats.processed_rows,
            stats.mismatch_count,
            Some(persisted_cursor),
            stats.source_checksum,
            stats.shadow_checksum,
            None,
            None,
        );
    } else {
        let total_rows = state
            .total_rows
            .max(state.processed_rows.saturating_add(selected_rows));
        let processed_rows = state
            .processed_rows
            .saturating_add(selected_rows)
            .min(total_rows);
        transition_catalog(
            state,
            "backfilling",
            Some(shadow_attnum),
            total_rows,
            processed_rows,
            state.mismatch_count.saturating_sub(updated_rows).max(0),
            Some(&next_cursor),
            state.source_checksum.clone(),
            state.shadow_checksum.clone(),
            None,
            None,
        );
    }
}

pub(super) fn acknowledge_ready_index(state: &ConversionState) {
    lock_source_table_for_dml(state);
    validate_pre_cutover_state(state, false);
    ensure_sync_trigger(state, state.source_attnum, required_shadow_attnum(state));
    if target_index_state(state) != TargetIndexState::Ready {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "conversion index is not valid, ready, and live",
        );
    }
    let stats = column_stats(state, &state.source_column_name, &state.shadow_column_name);
    if stats.mismatch_count != 0 {
        transition_catalog(
            state,
            "backfilling",
            state.shadow_attnum,
            stats.total_rows,
            stats.processed_rows,
            stats.mismatch_count,
            Some("(0,0)"),
            stats.source_checksum,
            stats.shadow_checksum,
            Some("index_recheck_requested_backfill"),
            None,
        );
        return;
    }
    ensure_sync_trigger(state, state.source_attnum, required_shadow_attnum(state));
    if state.status == "ready" {
        return;
    }
    transition_catalog(
        state,
        "ready",
        state.shadow_attnum,
        stats.total_rows,
        stats.processed_rows,
        0,
        None,
        stats.source_checksum,
        stats.shadow_checksum,
        None,
        None,
    );
}

pub(super) fn cutover_online_conversion(state: &ConversionState) {
    // Validate the complete data set while ordinary DML is still permitted.
    // The certified BEFORE trigger preserves equality for commits racing this
    // snapshot; the subsequent lock upgrade freezes both columns before DDL.
    lock_source_table_for_read(state);
    let target = validate_pre_cutover_state(state, true);
    ensure_sync_trigger(state, state.source_attnum, required_shadow_attnum(state));
    if target_index_state(state) != TargetIndexState::Ready {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "conversion index is not ready for cutover",
        );
    }
    let stats = column_stats(state, &state.source_column_name, &state.shadow_column_name);
    if stats.mismatch_count != 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("conversion has {} mismatched rows", stats.mismatch_count),
        );
    }
    lock_source_table(state);
    validate_pre_cutover_state(state, true);
    ensure_sync_trigger(state, state.source_attnum, required_shadow_attnum(state));
    if target_index_state(state) != TargetIndexState::Ready {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "conversion index is not ready for cutover",
        );
    }
    if target.not_null {
        Spi::run(&format!(
            "ALTER TABLE {} ALTER COLUMN {} SET NOT NULL",
            qualified_relation(&state.source_schema_name, &state.source_table_name),
            quote_ident(&state.shadow_column_name),
        ))
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NOT_NULL_VIOLATION,
                format!("failed to preserve NOT NULL during cutover: {error}"),
            )
        });
    }
    drop_sync_trigger(state);
    let relation = qualified_relation(&state.source_schema_name, &state.source_table_name);
    Spi::run(&format!(
        "ALTER TABLE {relation} RENAME COLUMN {} TO {}",
        quote_ident(&state.source_column_name),
        quote_ident(&state.backup_column_name)
    ))
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to preserve legacy pgvector column: {error}"),
        )
    });
    Spi::run(&format!(
        "ALTER TABLE {relation} RENAME COLUMN {} TO {}",
        quote_ident(&state.shadow_column_name),
        quote_ident(&state.source_column_name)
    ))
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to publish canonical pgContext column: {error}"),
        )
    });
    create_sync_trigger(state, required_shadow_attnum(state), state.source_attnum);
    ensure_sync_trigger(state, required_shadow_attnum(state), state.source_attnum);
    ensure_column_binding(
        state,
        &state.source_column_name,
        required_shadow_attnum(state),
        state.source_typmod,
        "pgcontext",
    );
    ensure_column_binding(
        state,
        &state.backup_column_name,
        state.source_attnum,
        state.source_typmod,
        "vector",
    );
    transition_catalog(
        state,
        "cutover",
        state.shadow_attnum,
        stats.total_rows,
        stats.processed_rows,
        0,
        None,
        stats.source_checksum,
        stats.shadow_checksum,
        Some("sessions_drained_at_cutover"),
        None,
    );
}

pub(super) fn finalize_online_conversion(state: &ConversionState) {
    // Keep the authoritative checksum scan readable and writable. The reverse
    // trigger maintains equality until the subsequent short lock upgrade.
    lock_source_table_for_read(state);
    ensure_column_binding(
        state,
        &state.source_column_name,
        required_shadow_attnum(state),
        state.source_typmod,
        "pgcontext",
    );
    ensure_column_binding(
        state,
        &state.backup_column_name,
        state.source_attnum,
        state.source_typmod,
        "vector",
    );
    ensure_sync_trigger(state, required_shadow_attnum(state), state.source_attnum);
    let stats = column_stats(state, &state.source_column_name, &state.backup_column_name);
    if stats.mismatch_count != 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "cutover rollback column has {} mismatched rows",
                stats.mismatch_count
            ),
        );
    }
    lock_source_table(state);
    ensure_column_binding(
        state,
        &state.source_column_name,
        required_shadow_attnum(state),
        state.source_typmod,
        "pgcontext",
    );
    ensure_column_binding(
        state,
        &state.backup_column_name,
        state.source_attnum,
        state.source_typmod,
        "vector",
    );
    ensure_sync_trigger(state, required_shadow_attnum(state), state.source_attnum);
    drop_sync_trigger(state);
    Spi::run(&format!(
        "ALTER TABLE {} DROP COLUMN {}",
        qualified_relation(&state.source_schema_name, &state.source_table_name),
        quote_ident(&state.backup_column_name)
    ))
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DEPENDENT_OBJECTS_STILL_EXIST,
            format!("failed to remove legacy pgvector column: {error}"),
        )
    });
    transition_catalog(
        state,
        "completed",
        state.shadow_attnum,
        stats.total_rows,
        stats.total_rows,
        0,
        None,
        stats.source_checksum,
        stats.shadow_checksum,
        Some("legacy_column_dropped"),
        None,
    );
}

pub(super) fn rollback_online_conversion(state: &ConversionState) {
    lock_source_table(state);
    let relation = qualified_relation(&state.source_schema_name, &state.source_table_name);
    match state.status.as_str() {
        "planned" => {}
        "backfilling" | "index_pending" | "ready" | "failed" => {
            ensure_column_binding(
                state,
                &state.source_column_name,
                state.source_attnum,
                state.source_typmod,
                "vector",
            );
            ensure_column_binding(
                state,
                &state.shadow_column_name,
                required_shadow_attnum(state),
                state.source_typmod,
                "pgcontext",
            );
            drop_sync_trigger(state);
            Spi::run(&format!(
                "ALTER TABLE {relation} DROP COLUMN IF EXISTS {}",
                quote_ident(&state.shadow_column_name)
            ))
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to remove conversion shadow column: {error}"),
                )
            });
        }
        "cutover" => {
            ensure_column_binding(
                state,
                &state.source_column_name,
                required_shadow_attnum(state),
                state.source_typmod,
                "pgcontext",
            );
            ensure_column_binding(
                state,
                &state.backup_column_name,
                state.source_attnum,
                state.source_typmod,
                "vector",
            );
            let stats = column_stats(state, &state.source_column_name, &state.backup_column_name);
            if stats.mismatch_count != 0 {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                    format!("cannot roll back {} mismatched rows", stats.mismatch_count),
                );
            }
            drop_sync_trigger(state);
            Spi::run(&format!(
                "ALTER TABLE {relation} RENAME COLUMN {} TO {}",
                quote_ident(&state.source_column_name),
                quote_ident(&state.shadow_column_name)
            ))
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to unpublish canonical column: {error}"),
                )
            });
            Spi::run(&format!(
                "ALTER TABLE {relation} RENAME COLUMN {} TO {}",
                quote_ident(&state.backup_column_name),
                quote_ident(&state.source_column_name)
            ))
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to restore legacy pgvector column: {error}"),
                )
            });
            Spi::run(&format!(
                "ALTER TABLE {relation} DROP COLUMN {}",
                quote_ident(&state.shadow_column_name)
            ))
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to discard canonical rollback column: {error}"),
                )
            });
        }
        other => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("cannot roll back conversion in status {other}"),
        ),
    }
    let (total_rows, checksum) = single_column_stats(state, &state.source_column_name);
    transition_catalog(
        state,
        "rolled_back",
        state.shadow_attnum,
        total_rows,
        total_rows,
        0,
        None,
        checksum.clone(),
        checksum,
        Some("legacy_pgvector_restored"),
        None,
    );
}

pub(super) fn column_stats(
    state: &ConversionState,
    source_column: &str,
    shadow_column: &str,
) -> ConversionStats {
    let relation = qualified_relation(&state.source_schema_name, &state.source_table_name);
    let source = quote_ident(source_column);
    let shadow = quote_ident(shadow_column);
    let sql = format!(
        "SELECT count(*)::bigint,
                count(*) FILTER (
                    WHERE {source}::text IS NOT DISTINCT FROM {shadow}::text
                )::bigint,
                count(*) FILTER (
                    WHERE {source}::text IS DISTINCT FROM {shadow}::text
                )::bigint,
                pg_catalog.md5(coalesce(
                    pg_catalog.sum(pg_catalog.hashtextextended(coalesce({source}::text, '<NULL>'), 0)::numeric)::text,
                    '0'
                )),
                pg_catalog.md5(coalesce(
                    pg_catalog.sum(pg_catalog.hashtextextended(coalesce({shadow}::text, '<NULL>'), 0)::numeric)::text,
                    '0'
                ))
           FROM {relation}"
    );
    Spi::connect(|client| {
        let row = client.select(&sql, None, &[])?.first();
        Ok::<_, spi::Error>(ConversionStats {
            total_rows: row.get::<i64>(1)?.unwrap_or_default(),
            processed_rows: row.get::<i64>(2)?.unwrap_or_default(),
            mismatch_count: row.get::<i64>(3)?.unwrap_or_default(),
            source_checksum: row.get::<String>(4)?,
            shadow_checksum: row.get::<String>(5)?,
        })
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate pgvector conversion rows: {error}"),
        )
    })
}

fn single_column_stats(state: &ConversionState, column_name: &str) -> (i64, Option<String>) {
    let relation = qualified_relation(&state.source_schema_name, &state.source_table_name);
    let column = quote_ident(column_name);
    let sql = format!(
        "SELECT count(*)::bigint,
                pg_catalog.md5(coalesce(
                    pg_catalog.sum(pg_catalog.hashtextextended(coalesce({column}::text, '<NULL>'), 0)::numeric)::text,
                    '0'
                ))
           FROM {relation}"
    );
    Spi::connect(|client| {
        let row = client.select(&sql, None, &[])?.first();
        Ok::<_, spi::Error>((
            row.get::<i64>(1)?.unwrap_or_default(),
            row.get::<String>(2)?,
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to checksum converted column: {error}"),
        )
    })
}

fn create_sync_trigger(state: &ConversionState, source_attnum: i16, target_attnum: i16) {
    Spi::run(&format!(
        "CREATE TRIGGER {}
         BEFORE INSERT OR UPDATE ON {}
         FOR EACH ROW
         EXECUTE FUNCTION pgcontext._sync_pgvector_ownership_columns('{}', '{}')",
        quote_ident(&state.trigger_name),
        qualified_relation(&state.source_schema_name, &state.source_table_name),
        source_attnum,
        target_attnum,
    ))
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to create pgvector conversion synchronization trigger: {error}"),
        )
    });
}

fn drop_sync_trigger(state: &ConversionState) {
    Spi::run(&format!(
        "DROP TRIGGER IF EXISTS {} ON {}",
        quote_ident(&state.trigger_name),
        qualified_relation(&state.source_schema_name, &state.source_table_name)
    ))
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to drop pgvector conversion synchronization trigger: {error}"),
        )
    });
}

fn ensure_sync_trigger(state: &ConversionState, source_attnum: i16, target_attnum: i16) {
    let binding = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT trigger.tgtype::int4,
                        trigger.tgenabled::text,
                        trigger.tgnargs::int4,
                        trigger.tgfoid = 'pgcontext._sync_pgvector_ownership_columns()'::pg_catalog.regprocedure,
                        trigger.tgargs
                   FROM pg_catalog.pg_trigger AS trigger
                  WHERE trigger.tgrelid = $1
                    AND trigger.tgname = $2
                    AND NOT trigger.tgisinternal",
                None,
                &[
                    state.source_table_oid.into(),
                    state.trigger_name.clone().into(),
                ],
            )?
            .first();
        if row.is_empty() {
            return Ok(None);
        }
        Ok::<_, spi::Error>(Some((
            row.get::<i32>(1)?.unwrap_or_default(),
            row.get::<String>(2)?.unwrap_or_default(),
            row.get::<i32>(3)?.unwrap_or_default(),
            row.get::<bool>(4)?.unwrap_or(false),
            row.get::<Vec<u8>>(5)?.unwrap_or_default(),
        )))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate ownership synchronization trigger: {error}"),
        )
    });
    let expected_arguments = format!("{source_attnum}\0{target_attnum}\0").into_bytes();
    let expected_type = (pg_sys::TRIGGER_TYPE_ROW
        | pg_sys::TRIGGER_TYPE_BEFORE
        | pg_sys::TRIGGER_TYPE_INSERT
        | pg_sys::TRIGGER_TYPE_UPDATE)
        .cast_signed();
    let valid = matches!(
        binding,
        Some((trigger_type, enabled, argument_count, correct_function, arguments))
            if trigger_type == expected_type
                && enabled == "O"
                && argument_count == 2
                && correct_function
                && arguments == expected_arguments
    );
    if !valid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "ownership synchronization trigger binding changed for conversion {}",
                state.conversion_id
            ),
        );
    }
}

fn resolve_attnum(table_oid: pg_sys::Oid, column_name: &str) -> i16 {
    Spi::get_one_with_args::<i16>(
        "SELECT attnum
           FROM pg_catalog.pg_attribute
          WHERE attrelid = $1
            AND attname = $2
            AND attnum > 0
            AND NOT attisdropped",
        &[table_oid.into(), column_name.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to resolve conversion column attribute: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
            format!("conversion column does not exist: {column_name}"),
        )
    })
}

pub(super) fn required_shadow_attnum(state: &ConversionState) -> i16 {
    state.shadow_attnum.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("conversion {} has no shadow attribute", state.conversion_id),
        )
    })
}

fn relation_filenode(table_oid: pg_sys::Oid) -> pg_sys::Oid {
    Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT relfilenode FROM pg_catalog.pg_class WHERE oid = $1",
        &[table_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to resolve source relfilenode: {error}"),
        )
    })
    .unwrap_or(pg_sys::InvalidOid)
}

fn ensure_column_binding(
    state: &ConversionState,
    column_name: &str,
    attnum: i16,
    typmod: i32,
    extension_name: &str,
) {
    let valid = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_catalog.pg_attribute AS attribute
               JOIN pg_catalog.pg_type AS type ON type.oid = attribute.atttypid
               JOIN pg_catalog.pg_depend AS dependency
                 ON dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
                AND dependency.objid = type.oid
                AND dependency.deptype = 'e'
               JOIN pg_catalog.pg_extension AS extension
                 ON extension.oid = dependency.refobjid
              WHERE attribute.attrelid = $1
                AND attribute.attname = $2
                AND attribute.attnum = $3
                AND attribute.atttypmod = $4
                AND type.typname = $5
                AND extension.extname = $6
                AND NOT attribute.attisdropped
         )",
        &[
            state.source_table_oid.into(),
            column_name.into(),
            i32::from(attnum).into(),
            typmod.into(),
            state.source_type_name.clone().into(),
            extension_name.into(),
        ],
    )
    .unwrap_or(Some(false))
    .unwrap_or(false);
    if !valid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "conversion column binding is stale: {}.{}.{}",
                state.source_schema_name, state.source_table_name, column_name
            ),
        );
    }
}
