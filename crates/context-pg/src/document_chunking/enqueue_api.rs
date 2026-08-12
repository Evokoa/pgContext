//! Bounded current-profile and shadow-profile enqueue APIs.

use super::*;

/// Enqueues visible current source rows under an idempotent version/profile key.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn enqueue_document_chunking(
    collection: String,
    source_name: String,
    source_keys: BoundedSourceKeys,
) -> i64 {
    let collection_id = require_collection_owner(&collection);
    let source = load_source_registration(collection_id, &source_name);
    enqueue_with_profile(source, source_keys.into_vec())
}

/// Enqueues a bounded source-key set for the alias's prepared shadow profile.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn enqueue_document_chunking_profile(
    collection: String,
    source_name: String,
    profile_name: String,
    source_keys: BoundedSourceKeys,
) -> i64 {
    validate_bounded_name(&profile_name, MAX_PROFILE_NAME_BYTES, "chunking profile");
    let collection_id = require_collection_owner(&collection);
    let mut source = load_source_registration(collection_id, &source_name);
    let target = Spi::get_two_with_args::<i64, i64>(
        "SELECT profiles.chunking_profile_id, profiles.max_document_bytes
           FROM pgcontext._visible_document_sources AS sources
           JOIN pgcontext._visible_chunking_profile_aliases AS aliases
             USING (chunking_profile_alias_id)
           JOIN pgcontext._visible_chunking_profiles AS profiles
             ON profiles.chunking_profile_id = aliases.shadow_chunking_profile_id
          WHERE sources.document_source_id = $1
            AND profiles.profile_name = $2
            AND aliases.status = 'ready' AND profiles.status = 'ready'",
        &[
            source.document_source_id.into(),
            profile_name.as_str().into(),
        ],
    )
    .ok()
    .and_then(|(profile_id, max_bytes)| profile_id.zip(max_bytes))
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "prepared chunking profile shadow is unavailable",
        )
    });
    source.chunking_profile_id = target.0;
    source.max_document_bytes = target.1;
    enqueue_with_profile(source, source_keys.into_vec())
}

fn enqueue_with_profile(source: SourceRegistration, source_keys: Vec<String>) -> i64 {
    if lock_source_keys_bounded(&source, &source_keys) != source_keys.len() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "one or more document rows are unavailable",
        );
    }
    let mut enqueued = 0_i64;
    for source_key in source_keys {
        let mut rows = hydrate_source_keys(&source, std::slice::from_ref(&source_key));
        if rows.len() != 1 {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
                "one or more document rows are unavailable",
            );
        }
        let row = rows.pop().unwrap_or_else(|| invalid_job_identity());
        let digest: Vec<u8> = Sha256::digest(row.text.as_bytes()).to_vec();
        arm_document_chunk_permit(
            DocumentChunkPermitKind::Enqueue,
            source.document_source_id,
            row.source_version,
        );
        Spi::get_one_with_args::<i64>(
            "SELECT pgcontext._enqueue_document_chunk_job($1,$2,$3,$4,$5)",
            &[
                source.document_source_id.into(),
                row.source_key.as_str().into(),
                row.source_version.into(),
                digest.into(),
                source.chunking_profile_id.into(),
            ],
        )
        .unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "failed to enqueue document chunk job",
            )
        });
        enqueued += 1;
    }
    enqueued
}
