//! Public immutable-profile alias promotion and rollback operations.

use super::*;

fn resolve_alias_profile(alias_name: &str, profile_name: &str) -> (i64, i64) {
    Spi::get_two_with_args::<i64, i64>(
        "WITH matches AS (
             SELECT aliases.chunking_profile_alias_id, profiles.chunking_profile_id
               FROM pgcontext._visible_chunking_profile_aliases AS aliases
               JOIN pgcontext._visible_chunking_profiles AS profiles
                 ON profiles.owner_role = aliases.owner_role
              WHERE aliases.alias_name = $1 AND aliases.status = 'ready'
                AND profiles.profile_name = $2 AND profiles.status = 'ready'
         )
         SELECT CASE WHEN pg_catalog.count(*) = 1
                     THEN pg_catalog.min(chunking_profile_alias_id) END,
                CASE WHEN pg_catalog.count(*) = 1
                     THEN pg_catalog.min(chunking_profile_id) END
           FROM matches",
        &[alias_name.into(), profile_name.into()],
    )
    .ok()
    .and_then(|(alias_id, profile_id)| alias_id.zip(profile_id))
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "chunking profile alias or target is missing or ambiguous",
        )
    })
}

/// Selects one immutable profile as the bounded shadow build target for an alias.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn prepare_chunking_profile_alias(alias_name: String, profile_name: String) -> i64 {
    validate_bounded_name(
        &alias_name,
        MAX_PROFILE_NAME_BYTES,
        "chunking profile alias",
    );
    validate_bounded_name(&profile_name, MAX_PROFILE_NAME_BYTES, "chunking profile");
    let identity = resolve_alias_profile(&alias_name, &profile_name);
    arm_document_chunk_permit(
        DocumentChunkPermitKind::PrepareProfileAlias,
        identity.0,
        identity.1,
    );
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._prepare_chunking_profile_alias($1,$2)",
        &[identity.0.into(), identity.1.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "failed to prepare chunking profile shadow",
        )
    })
    .unwrap_or_else(|| invalid_job_identity())
}

/// Atomically points a named chunking-profile alias at another immutable profile.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn promote_chunking_profile_alias(alias_name: String, profile_name: String) -> i64 {
    validate_bounded_name(
        &alias_name,
        MAX_PROFILE_NAME_BYTES,
        "chunking profile alias",
    );
    validate_bounded_name(&profile_name, MAX_PROFILE_NAME_BYTES, "chunking profile");
    let identity = resolve_alias_profile(&alias_name, &profile_name);
    arm_document_chunk_permit(
        DocumentChunkPermitKind::PromoteProfileAlias,
        identity.0,
        identity.1,
    );
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._promote_chunking_profile_alias($1,$2)",
        &[identity.0.into(), identity.1.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "failed to promote chunking profile alias",
        )
    })
    .unwrap_or_else(|| invalid_job_identity())
}

/// Rolls a named profile alias back to its previous distinct immutable target.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn rollback_chunking_profile_alias(alias_name: String) -> i64 {
    validate_bounded_name(
        &alias_name,
        MAX_PROFILE_NAME_BYTES,
        "chunking profile alias",
    );
    let alias_id = Spi::get_one_with_args::<i64>(
        "SELECT CASE WHEN pg_catalog.count(*) = 1
                     THEN pg_catalog.min(chunking_profile_alias_id) END
           FROM pgcontext._visible_chunking_profile_aliases
          WHERE alias_name = $1 AND status = 'ready'",
        &[alias_name.as_str().into()],
    )
    .ok()
    .flatten()
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "chunking profile alias is missing or ambiguous",
        )
    });
    arm_document_chunk_permit(DocumentChunkPermitKind::RollbackProfileAlias, alias_id, 0);
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._rollback_chunking_profile_alias($1)",
        &[alias_id.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "chunking profile alias has no rollback target",
        )
    })
    .unwrap_or_else(|| invalid_job_identity())
}

/// Stops serving one retained predecessor profile for a named alias.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn drain_chunking_profile_alias(alias_name: String, profile_name: String) -> bool {
    validate_bounded_name(
        &alias_name,
        MAX_PROFILE_NAME_BYTES,
        "chunking profile alias",
    );
    validate_bounded_name(&profile_name, MAX_PROFILE_NAME_BYTES, "chunking profile");
    let identity = resolve_alias_profile(&alias_name, &profile_name);
    arm_document_chunk_permit(
        DocumentChunkPermitKind::DrainProfileAlias,
        identity.0,
        identity.1,
    );
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._drain_chunking_profile_alias($1,$2)",
        &[identity.0.into(), identity.1.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "failed to drain retained chunking profile",
        )
    })
    .unwrap_or_else(|| invalid_job_identity())
}
