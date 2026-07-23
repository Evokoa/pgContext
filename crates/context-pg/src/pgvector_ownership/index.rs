use pgrx::prelude::*;

use super::execution::{required_shadow_attnum, validate_pre_cutover_state};
use super::persistence::ConversionState;
use super::validation::{
    IndexPlan, canonical_opclass, collect_fast_index_plans, ensure_index_build_privileges,
    resolve_conversion_target,
};
use super::{qualified_relation, quote_ident};
use crate::error::raise_sql_error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TargetIndexState {
    Missing,
    Invalid,
    Ready,
}

pub(super) fn create_index_command(state: &ConversionState) -> String {
    let target = validate_pre_cutover_state(state, false);
    let current_plans = collect_fast_index_plans(&target);
    ensure_index_build_privileges(&target, &current_plans);
    let opclass = canonical_opclass(&state.source_type_name, &state.metric).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "persisted conversion metric has no canonical opclass",
        )
    });
    let source_plan = online_source_index_plan(state);
    let options = source_plan
        .as_ref()
        .filter(|plan| !plan.options.is_empty())
        .map(|plan| format!(" WITH ({})", plan.options.join(", ")))
        .unwrap_or_default();
    let tablespace = source_plan
        .as_ref()
        .and_then(|plan| plan.tablespace.as_deref())
        .map(|name| format!(" TABLESPACE {}", quote_ident(name)))
        .unwrap_or_default();
    format!(
        "CREATE INDEX CONCURRENTLY {} ON {} USING pgcontext_hnsw ({} {}){options}{tablespace}",
        quote_ident(&state.index_name),
        qualified_relation(&state.source_schema_name, &state.source_table_name),
        quote_ident(&state.shadow_column_name),
        opclass,
    )
}

pub(super) fn drop_invalid_index_command(state: &ConversionState) -> String {
    format!(
        "DROP INDEX CONCURRENTLY {}.{}",
        quote_ident(&state.source_schema_name),
        quote_ident(&state.index_name)
    )
}

pub(super) fn target_index_state(state: &ConversionState) -> TargetIndexState {
    let shadow_attnum = required_shadow_attnum(state);
    let expected_opclass = canonical_opclass(&state.source_type_name, &state.metric)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "persisted conversion metric has no canonical opclass",
            )
        });
    let row = Spi::connect(|client| {
        let row = client
            .select(
                "SELECT index.indrelid,
                        index.indkey[0]::int4,
                        access_method.amname::text,
                        namespace.nspname::text,
                        opclass.opcname::text,
                        index.indisvalid,
                        index.indisready,
                        index.indislive,
                        index.indnkeyatts = 1
                            AND index.indnatts = 1
                            AND index.indexprs IS NULL
                            AND index.indpred IS NULL,
                        EXISTS (
                            SELECT 1
                              FROM pg_catalog.pg_depend AS dependency
                              JOIN pg_catalog.pg_extension AS extension
                                ON extension.oid = dependency.refobjid
                             WHERE dependency.classid = 'pg_catalog.pg_opclass'::pg_catalog.regclass
                               AND dependency.objid = opclass.oid
                               AND dependency.deptype = 'e'
                               AND extension.extname = 'pgcontext'
                        ),
                        COALESCE(index_relation.reloptions, '{}')::text[],
                        NULLIF(tablespace.spcname, 'pg_default')::text
                   FROM pg_catalog.pg_class AS index_relation
                   JOIN pg_catalog.pg_namespace AS index_namespace
                     ON index_namespace.oid = index_relation.relnamespace
                   JOIN pg_catalog.pg_index AS index
                     ON index.indexrelid = index_relation.oid
                   JOIN pg_catalog.pg_am AS access_method
                     ON access_method.oid = index_relation.relam
                   JOIN pg_catalog.pg_opclass AS opclass
                     ON opclass.oid = index.indclass[0]
                   JOIN pg_catalog.pg_namespace AS namespace
                     ON namespace.oid = opclass.opcnamespace
                   LEFT JOIN pg_catalog.pg_tablespace AS tablespace
                     ON tablespace.oid = index_relation.reltablespace
                  WHERE index_namespace.nspname = $1
                    AND index_relation.relname = $2",
                None,
                &[
                    state.source_schema_name.clone().into(),
                    state.index_name.clone().into(),
                ],
            )?
            .first();
        if row.is_empty() {
            return Ok(None);
        }
        Ok::<_, spi::Error>(Some((
            row.get::<pg_sys::Oid>(1)?.unwrap_or(pg_sys::InvalidOid),
            row.get::<i32>(2)?.unwrap_or_default(),
            row.get::<String>(3)?.unwrap_or_default(),
            row.get::<String>(4)?.unwrap_or_default(),
            row.get::<String>(5)?.unwrap_or_default(),
            row.get::<bool>(6)?.unwrap_or(false),
            row.get::<bool>(7)?.unwrap_or(false),
            row.get::<bool>(8)?.unwrap_or(false),
            row.get::<bool>(9)?.unwrap_or(false),
            row.get::<bool>(10)?.unwrap_or(false),
            row.get::<Vec<String>>(11)?.unwrap_or_default(),
            row.get::<String>(12)?,
        )))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate conversion index: {error}"),
        )
    });
    let Some((
        table_oid,
        attnum,
        access_method,
        namespace,
        opclass,
        valid,
        ready,
        live,
        simple_index,
        pgcontext_owned,
        options,
        tablespace,
    )) = row
    else {
        return TargetIndexState::Missing;
    };
    let source_plan = online_source_index_plan(state);
    let expected_options = source_plan
        .as_ref()
        .map(|plan| plan.options.as_slice())
        .unwrap_or_default();
    let expected_tablespace = source_plan
        .as_ref()
        .and_then(|plan| plan.tablespace.as_deref());
    if table_oid != state.source_table_oid
        || attnum != i32::from(shadow_attnum)
        || access_method != "pgcontext_hnsw"
        || namespace != "pgcontext"
        || format!("pgcontext.{opclass}") != expected_opclass
        || !simple_index
        || !pgcontext_owned
        || options != expected_options
        || tablespace.as_deref() != expected_tablespace
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_WRONG_OBJECT_TYPE,
            format!(
                "conversion index {} exists with an unexpected binding",
                state.index_name
            ),
        );
    }
    if valid && ready && live {
        TargetIndexState::Ready
    } else {
        TargetIndexState::Invalid
    }
}

fn online_source_index_plan(state: &ConversionState) -> Option<IndexPlan> {
    let target = resolve_conversion_target(state.source_table_oid, &state.source_column_name);
    let mut plans = collect_fast_index_plans(&target);
    if plans.len() > 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "restricted-online source index profile changed",
        );
    }
    plans.pop()
}
