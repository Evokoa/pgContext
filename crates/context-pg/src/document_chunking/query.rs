//! Source-authoritative current chunk reads and content-free progress reports.

use std::collections::BTreeMap;

use super::*;

/// Returns bounded, content-free job and publication diagnostics for a source.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn document_chunking_progress(collection: String, source_name: String) -> JsonB {
    let collection_id = require_collection_owner(&collection);
    let source = load_source_registration_readonly(collection_id, &source_name);
    if !source_select_allowed(source.source_table_oid) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "document source SELECT privilege is required",
        );
    }
    let table = quote_qualified(&source.source_schema, &source.source_table);
    validate_current_relation(source.source_table_oid, &table);
    validate_source_contract(&source);
    let key_type = quote_qualified(&source.source_key_type_schema, &source.source_key_type_name);
    let text_column = quote_identifier(&source.text_column);
    let version_column = quote_identifier(&source.version_column);
    let sql = format!(
        "SELECT pg_catalog.jsonb_build_object(
             'document_source_id', $1::bigint,
             'jobs_total', pg_catalog.count(jobs.job_id),
             'queued', pg_catalog.count(*) FILTER (WHERE jobs.status = 'queued'),
             'active', pg_catalog.count(*) FILTER (
                 WHERE jobs.status IN ('leased','parsing','chunking','embedding',
                                       'validating','publishing','cancel_requested')
             ),
             'ready', pg_catalog.count(*) FILTER (WHERE jobs.status = 'ready'),
             'failed', pg_catalog.count(*) FILTER (WHERE jobs.status = 'failed'),
             'cancelled', pg_catalog.count(*) FILTER (WHERE jobs.status = 'cancelled'),
             'superseded', pg_catalog.count(*) FILTER (WHERE jobs.status = 'superseded'),
             'retired', pg_catalog.count(*) FILTER (WHERE jobs.status = 'retired'),
             'processed_units', COALESCE(pg_catalog.sum(jobs.processed_units), 0::numeric),
             'total_units', COALESCE(pg_catalog.sum(jobs.total_units), 0::numeric),
             'active_staging_bytes', COALESCE((
                 SELECT pg_catalog.sum(staging.staging_bytes)
                   FROM pgcontext._visible_document_chunk_staging AS staging
                   JOIN pgcontext._visible_document_chunk_jobs AS staged_jobs USING (job_id)
                  WHERE staged_jobs.document_source_id = $1
             ), 0::numeric),
             'published_bytes', COALESCE(pg_catalog.sum(generations.staging_bytes), 0::numeric),
             'current_documents', (
                 SELECT pg_catalog.count(DISTINCT aliases.source_key) FILTER (
                     WHERE authoritative.{version_column} = current.source_version
                       AND pg_catalog.sha256(pg_catalog.convert_to(authoritative.{text_column}, 'UTF8'))
                           = current.source_sha256
                       AND current.source_registration_revision = $2
                 )
                   FROM pgcontext._visible_current_document_chunk_generations AS aliases
                   JOIN pgcontext._visible_document_chunk_generations AS current
                     USING (generation_id)
                   JOIN {table} AS authoritative
                     ON authoritative.id = aliases.source_key::text::{key_type}
                  WHERE aliases.document_source_id = $1
             ),
             'stale_documents', (
                 SELECT pg_catalog.count(DISTINCT aliases.source_key) FILTER (
                     WHERE authoritative.{version_column} IS DISTINCT FROM current.source_version
                        OR pg_catalog.sha256(pg_catalog.convert_to(authoritative.{text_column}, 'UTF8'))
                           IS DISTINCT FROM current.source_sha256
                        OR current.source_registration_revision IS DISTINCT FROM $2
                 )
                   FROM pgcontext._visible_current_document_chunk_generations AS aliases
                   JOIN pgcontext._visible_document_chunk_generations AS current
                     USING (generation_id)
                   JOIN {table} AS authoritative
                     ON authoritative.id = aliases.source_key::text::{key_type}
                  WHERE aliases.document_source_id = $1
             )
         )
           FROM pgcontext._visible_document_chunk_jobs AS jobs
           JOIN pgcontext._visible_document_chunk_generations AS generations
             USING (generation_id)
           JOIN {table} AS source
             ON source.id = generations.source_key::text::{key_type}
          WHERE jobs.document_source_id = $1"
    );
    Spi::get_one_with_args::<JsonB>(
        &sql,
        &[
            source.document_source_id.into(),
            source.registration_revision.into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to read document chunking progress",
        )
    })
    .unwrap_or_else(|| JsonB(json!({})))
}

/// Returns only ready chunks that still match an invoker-visible source row.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "the stable chunk projection row is intentionally explicit"
)]
pub fn current_document_chunks(
    collection: String,
    source_name: String,
    source_keys: BoundedSourceKeys,
) -> TableIterator<
    'static,
    (
        name!(source_key, String),
        name!(source_version, i64),
        name!(profile_revision, i64),
        name!(generation_id, i64),
        name!(occurrence_id, i64),
        name!(ordinal, i32),
        name!(original_text, String),
        name!(retrieval_text, String),
        name!(start_byte, i64),
        name!(end_byte, i64),
        name!(start_char, i64),
        name!(end_char, i64),
        name!(token_count, i32),
        name!(structure_kind, String),
        name!(structure_path, Vec<String>),
        name!(page_number, Option<i32>),
        name!(region, Option<JsonB>),
        name!(parent_occurrence_id, Option<i64>),
        name!(previous_occurrence_id, Option<i64>),
        name!(next_occurrence_id, Option<i64>),
        name!(content_hash, i64),
        name!(context_prefix, Option<String>),
        name!(fake_embedding, JsonB),
    ),
> {
    let source_keys = source_keys.into_vec();
    let collection_id = require_collection_owner(&collection);
    let mut registration = load_source_registration_readonly(collection_id, &source_name);
    // Current reads may legitimately fall back to a retained predecessor whose
    // immutable profile admitted a larger source document than the newly
    // promoted profile. Admission therefore uses the global certified source
    // ceiling; enqueue and worker execution retain their exact profile bounds.
    registration.max_document_bytes =
        i64::try_from(context_build::MAX_TOKEN_CHUNK_DOCUMENT_BYTES).unwrap_or(i64::MAX);
    lock_source_keys_bounded(&registration, &source_keys);
    let mut authoritative = BTreeMap::new();
    for source_key in &source_keys {
        let mut rows = hydrate_source_keys(&registration, std::slice::from_ref(source_key));
        if let Some(row) = rows.pop() {
            let digest: Vec<u8> = Sha256::digest(row.text.as_bytes()).to_vec();
            authoritative.insert(row.source_key, (row.source_version, digest));
        }
    }
    let visible_keys = authoritative.keys().cloned().collect::<Vec<_>>();
    if visible_keys.is_empty() {
        return TableIterator::new(Vec::new());
    }
    arm_document_chunk_permit(
        DocumentChunkPermitKind::LockReadAlias,
        registration.document_source_id,
        0,
    );
    let selected_profile_id = Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._lock_document_chunk_read_alias($1)",
        &[registration.document_source_id.into()],
    )
    .ok()
    .flatten()
    .unwrap_or_else(|| invalid_job_identity());
    let authoritative_versions = visible_keys
        .iter()
        .map(|key| authoritative.get(key).map_or(0, |identity| identity.0))
        .collect::<Vec<_>>();
    let authoritative_digests = visible_keys
        .iter()
        .map(|key| {
            authoritative
                .get(key)
                .map_or_else(Vec::new, |identity| identity.1.clone())
        })
        .collect::<Vec<_>>();
    let aliases = Spi::connect(|client| {
        client
            .select(
                "SELECT DISTINCT ON (aliases.source_key)
                        aliases.generation_id, aliases.source_key,
                        generations.source_version, generations.source_sha256,
                        generations.source_registration_revision,
                        generations.chunk_count, profiles.profile_revision,
                        generations.projection_sha256
                   FROM pgcontext._visible_current_document_chunk_generations AS aliases
                   JOIN pgcontext._visible_document_chunk_generations AS generations
                     ON generations.generation_id = aliases.generation_id
                    AND generations.chunking_profile_id = aliases.chunking_profile_id
                   JOIN pgcontext._visible_chunking_profiles AS profiles
                     ON profiles.chunking_profile_id = aliases.chunking_profile_id
                   JOIN pgcontext._visible_document_sources AS configured_source
                     ON configured_source.document_source_id = aliases.document_source_id
                   LEFT JOIN pgcontext._visible_chunking_profile_alias_retained AS retained
                     ON retained.chunking_profile_alias_id =
                            configured_source.chunking_profile_alias_id
                    AND retained.chunking_profile_id = aliases.chunking_profile_id
                   JOIN ROWS FROM (
                        pg_catalog.unnest($2::text[]),
                        pg_catalog.unnest($4::bigint[]),
                        pg_catalog.unnest($5::bytea[])
                   ) AS authoritative(source_key, source_version, source_sha256)
                     ON authoritative.source_key = aliases.source_key
                    AND authoritative.source_version = generations.source_version
                    AND authoritative.source_sha256 = generations.source_sha256
                  WHERE aliases.document_source_id = $1
                    AND generations.status = 'ready'
                    AND (
                        aliases.chunking_profile_id = $3
                        OR retained.chunking_profile_id IS NOT NULL
                    )
                  ORDER BY aliases.source_key,
                           (aliases.chunking_profile_id = $3) DESC,
                           retained.retained_revision DESC NULLS LAST,
                           aliases.updated_at DESC,
                           aliases.publication_revision DESC",
                Some(i64::try_from(MAX_SOURCE_KEYS).unwrap_or(i64::MAX)),
                &[
                    registration.document_source_id.into(),
                    visible_keys.into(),
                    selected_profile_id.into(),
                    authoritative_versions.into(),
                    authoritative_digests.into(),
                ],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to resolve current document chunk generations",
                )
            })
            .map(|row| {
                (
                    required::<i64>(&row, 1, "generation identity"),
                    required::<String>(&row, 2, "source key"),
                    required::<i64>(&row, 3, "source version"),
                    required::<Vec<u8>>(&row, 4, "source digest"),
                    required::<i64>(&row, 5, "source registration revision"),
                    required::<i32>(&row, 6, "published chunk count"),
                    required::<i64>(&row, 7, "profile revision"),
                    required::<Vec<u8>>(&row, 8, "projection digest"),
                )
            })
            .collect::<Vec<_>>()
    });
    let projection = quote_qualified(
        &registration.projection_schema,
        &registration.projection_table,
    );
    validate_current_relation(registration.projection_table_oid, &projection);
    // The digest and returned rows must describe the same immutable snapshot.
    // SHARE conflicts with projection writers and is held until this statement's
    // transaction ends, so no UPDATE/DELETE can race between verification and
    // materialization.
    lock_projection_writes(
        &registration.projection_schema,
        &registration.projection_table,
    );
    let mut output = Vec::new();
    let mut projected_output_bytes = 0usize;
    let mut admitted_aliases = Vec::with_capacity(aliases.len());
    for (
        generation_id,
        source_key,
        source_version,
        source_sha256,
        registration_revision,
        declared_count,
        profile_revision,
        projection_digest,
    ) in aliases
    {
        let source_matches = authoritative
            .get(&source_key)
            .is_some_and(|(version, digest)| {
                *version == source_version && *digest == source_sha256
            });
        if !source_matches || registration_revision != registration.registration_revision {
            continue;
        }
        let projection_stats_sql = format!(
            "SELECT pg_catalog.count(*)::bigint,
                    COALESCE(pg_catalog.sum(
                        pg_catalog.octet_length(source_key)
                      + pg_catalog.octet_length(original_text)
                      + pg_catalog.octet_length(retrieval_text)
                      + pg_catalog.octet_length(structure_kind)
                      + COALESCE((
                            SELECT pg_catalog.sum(pg_catalog.octet_length(segment))
                              FROM pg_catalog.unnest(structure_path) AS segment
                        ), 0)
                      + COALESCE(pg_catalog.octet_length(context_prefix), 0)
                      + COALESCE(pgcontext._document_chunk_raw_datum_bytes(region), 0)
                      + pgcontext._document_chunk_raw_datum_bytes(fake_embedding)
                      + pgcontext._document_chunk_raw_datum_bytes(provenance)
                      + 512
                    ), 0)::bigint,
                    CASE WHEN pg_catalog.count(*) = 0 THEN $7 = 0 ELSE COALESCE(pg_catalog.bool_and(
                        document_source_id = $3 AND source_key = $2
                        AND source_version = $4 AND source_sha256 = $5
                        AND profile_revision = $6 AND generation_id = $1 AND ready
                    ), false) END
               FROM {projection}
              WHERE generation_id = $1"
        );
        let (actual_count, generation_bytes, identities_match) = Spi::connect(|client| {
            let rows = client
                .select(
                    &projection_stats_sql,
                    Some(1),
                    &[
                        generation_id.into(),
                        source_key.as_str().into(),
                        registration.document_source_id.into(),
                        source_version.into(),
                        source_sha256.clone().into(),
                        profile_revision.into(),
                        declared_count.into(),
                    ],
                )
                .unwrap_or_else(|_| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        "failed to preflight current document chunks",
                    )
                });
            let row = rows
                .into_iter()
                .next()
                .unwrap_or_else(|| invalid_job_identity());
            (
                required::<i64>(&row, 1, "projection row count"),
                required::<i64>(&row, 2, "projection output bytes"),
                required::<bool>(&row, 3, "projection identity"),
            )
        });
        if actual_count != i64::from(declared_count) || !identities_match {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "current document chunk projection is incomplete or changed",
            );
        }
        let generation_bytes = usize::try_from(generation_bytes).unwrap_or(usize::MAX);
        projected_output_bytes = projected_output_bytes
            .checked_add(generation_bytes)
            .filter(|bytes| *bytes <= MAX_STAGING_BYTES)
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "current document chunk output exceeds aggregate byte limit",
                )
            });
        admitted_aliases.push((generation_id, source_key, projection_digest, declared_count));
    }
    for (generation_id, source_key, projection_digest, declared_count) in admitted_aliases {
        require_projection_generation_digest(
            &registration.projection_schema,
            &registration.projection_table,
            generation_id,
            &projection_digest,
        );
        let sql = format!(
            "SELECT source_key, source_version, profile_revision, generation_id,
                    occurrence_id, ordinal, original_text, retrieval_text,
                    start_byte, end_byte, start_char, end_char, token_count,
                    structure_kind, structure_path, page_number, region,
                    parent_occurrence_id,
                    previous_occurrence_id, next_occurrence_id, content_hash,
                    context_prefix, fake_embedding
               FROM {projection}
              WHERE generation_id = $1 AND source_key = $2 AND ready
              ORDER BY ordinal"
        );
        let generation_rows = Spi::connect(|client| {
            client
                .select(
                    &sql,
                    Some(i64::from(declared_count) + 1),
                    &[generation_id.into(), source_key.as_str().into()],
                )
                .unwrap_or_else(|_| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        "failed to read current document chunks",
                    )
                })
                .map(projection_row)
                .collect::<Vec<_>>()
        });
        if generation_rows.len() != usize::try_from(declared_count).unwrap_or(usize::MAX) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "current document chunk projection is incomplete or changed",
            );
        }
        output.extend(generation_rows);
    }
    TableIterator::new(output)
}
