//! Runtime hydration, canonicalization, and publication helpers.

use super::*;

mod catalog_helpers;
mod hydration;
mod output_helpers;
pub(super) use catalog_helpers::*;
pub(super) use hydration::*;
pub(super) use output_helpers::*;

pub(super) fn load_source_registration(
    collection_id: i64,
    source_name: &str,
) -> SourceRegistration {
    validate_bounded_name(source_name, MAX_SOURCE_NAME_BYTES, "document source");
    let prior_revision = Spi::get_one_with_args::<i64>(
        "SELECT registration_revision
           FROM pgcontext._visible_document_sources
          WHERE collection_id = $1 AND source_name = $2 AND status = 'ready'",
        &[collection_id.into(), source_name.into()],
    )
    .ok()
    .flatten();
    refresh_document_source(collection_id, source_name);
    let source = load_source_registration_row(collection_id, source_name);
    if prior_revision.is_some_and(|revision| revision != source.registration_revision) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk source registration changed during refresh",
        );
    }
    source
}

pub(super) fn load_source_registration_readonly(
    collection_id: i64,
    source_name: &str,
) -> SourceRegistration {
    validate_bounded_name(source_name, MAX_SOURCE_NAME_BYTES, "document source");
    load_source_registration_row(collection_id, source_name)
}

fn load_source_registration_row(collection_id: i64, source_name: &str) -> SourceRegistration {
    let source = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT sources.document_source_id,
                        rerank.source_table_oid, rerank.source_schema_name,
                        rerank.source_table_name, rerank.source_key_attnum,
                        rerank.source_key_type_oid, rerank.source_key_type_schema,
                        rerank.source_key_type_name, rerank.source_key_collation_oid,
                        rerank.text_column_name,
                        rerank.text_attnum, rerank.text_type_oid,
                        rerank.text_collation_oid, rerank.source_version_column_name,
                        rerank.source_version_attnum, rerank.source_version_type_oid,
                        sources.projection_table_oid, sources.projection_schema_name,
                        sources.projection_table_name, sources.registration_revision,
                        sources.projection_column_attnums,
                        sources.projection_column_type_oids,
                        sources.projection_column_collation_oids,
                        profiles.chunking_profile_id, profiles.max_document_bytes
                   FROM pgcontext._visible_document_sources AS sources
                   JOIN pgcontext._visible_semantic_rerank_sources AS rerank
                     ON rerank.rerank_source_id = sources.rerank_source_id
                   JOIN pgcontext._visible_chunking_profile_aliases AS aliases
                     ON aliases.chunking_profile_alias_id = sources.chunking_profile_alias_id
                    AND aliases.status = 'ready'
                   JOIN pgcontext._visible_chunking_profiles AS profiles
                     ON profiles.chunking_profile_id = aliases.chunking_profile_id
                  WHERE sources.collection_id = $1 AND sources.source_name = $2
                    AND sources.status = 'ready' AND rerank.status = 'ready'",
                Some(1),
                &[collection_id.into(), source_name.into()],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to load document source registration",
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                "document source does not exist or is not ready",
            );
        }
        let row = rows.into_iter().next().unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "document source registration row disappeared",
            )
        });
        SourceRegistration {
            document_source_id: required(&row, 1, "document source identity"),
            source_table_oid: required(&row, 2, "source table identity"),
            source_schema: required(&row, 3, "source schema"),
            source_table: required(&row, 4, "source table"),
            source_key_attnum: required(&row, 5, "source key attribute"),
            source_key_type_oid: required(&row, 6, "source key type"),
            source_key_type_schema: required(&row, 7, "source key type schema"),
            source_key_type_name: required(&row, 8, "source key type name"),
            source_key_collation_oid: required(&row, 9, "source key collation"),
            text_column: required(&row, 10, "source text column"),
            text_attnum: required(&row, 11, "source text attribute"),
            text_type_oid: required(&row, 12, "source text type"),
            text_collation_oid: required(&row, 13, "source text collation"),
            version_column: required(&row, 14, "source version column"),
            version_attnum: required(&row, 15, "source version attribute"),
            version_type_oid: required(&row, 16, "source version type"),
            projection_table_oid: required(&row, 17, "projection table identity"),
            projection_schema: required(&row, 18, "projection schema"),
            projection_table: required(&row, 19, "projection table"),
            registration_revision: required(&row, 20, "source registration revision"),
            projection_attnums: required(&row, 21, "projection attribute identities"),
            projection_type_oids: required(&row, 22, "projection type identities"),
            projection_collation_oids: required(&row, 23, "projection collation identities"),
            chunking_profile_id: required(&row, 24, "chunking profile identity"),
            max_document_bytes: required(&row, 25, "maximum document bytes"),
        }
    });
    let projection = quote_qualified(&source.projection_schema, &source.projection_table);
    validate_current_relation(source.projection_table_oid, &projection);
    let projection_identity = resolve_projection_contract(source.projection_table_oid);
    if projection_identity.attnums != source.projection_attnums
        || projection_identity.type_oids != source.projection_type_oids
        || projection_identity.collation_oids != source.projection_collation_oids
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk projection binding changed",
        );
    }
    source
}

fn refresh_document_source(collection_id: i64, source_name: &str) {
    Spi::run_with_args(
        "SELECT pgcontext._refresh_document_chunk_source($1, $2)",
        &[collection_id.into(), source_name.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to refresh document chunk source binding",
        )
    });
}

pub(super) fn lock_and_require_current_source(context: &JobContext) -> SourceRow {
    let registration = job_source_registration(context);
    let mut rows =
        hydrate_source_keys_locked(&registration, std::slice::from_ref(&context.source_key));
    if rows.len() != 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document source row is no longer visible",
        );
    }
    let source = rows.pop().unwrap_or_else(|| invalid_job_identity());
    require_source_matches(context, &source);
    source
}

pub(super) fn claimed_job_source_select_allowed(job_id: i64, lease_token: i64) -> bool {
    arm_document_chunk_permit(
        DocumentChunkPermitKind::LoadClaimSource,
        job_id,
        lease_token,
    );
    let identity = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT document_source_id, source_table_oid, source_key
                   FROM pgcontext._load_document_chunk_claim_source($1,$2)",
                Some(1),
                &[job_id.into(), lease_token.into()],
            )
            .ok()?;
        let row = rows.into_iter().next()?;
        Some((
            required::<i64>(&row, 1, "document source identity"),
            required::<pg_sys::Oid>(&row, 2, "source table identity"),
            required::<String>(&row, 3, "source key"),
        ))
    });
    identity.is_some_and(|(document_source_id, source_table_oid, source_key)| {
        source_select_allowed(source_table_oid)
            && source_key_visible(document_source_id, &source_key)
    })
}

pub(super) fn source_select_allowed(source_table_oid: pg_sys::Oid) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege(SESSION_USER, $1, 'SELECT')",
        &[source_table_oid.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(false)
}

pub(super) fn load_job_context(job_id: i64, lease_token: i64) -> JobContext {
    // The claim helper already fences the selected job. Refreshing catalog
    // registrations after that point would invert the global source -> job
    // lock order and can deadlock a concurrent registration cutover. Live
    // OID/attribute/source identity is revalidated below without writes.
    arm_document_chunk_permit(DocumentChunkPermitKind::LoadJob, job_id, lease_token);
    let lease_expires_micros = Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._load_document_chunk_lease_expiry($1,$2)",
        &[job_id.into(), lease_token.into()],
    )
    .ok()
    .flatten()
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "document chunk job lease is stale or unavailable",
        )
    });
    let context = Spi::connect_mut(|client| {
        let rows = client
            .update(
                "SELECT jobs.job_id, $2::bigint, $3::bigint,
                        generations.generation_id, generations.document_source_id,
                        generations.source_key, generations.source_version,
                        generations.source_sha256,
                        generations.source_registration_revision,
                        sources.registration_revision,
                        rerank.source_table_oid, rerank.source_schema_name,
                        rerank.source_table_name, rerank.source_key_attnum,
                        rerank.source_key_type_oid, rerank.source_key_type_schema,
                        rerank.source_key_type_name, rerank.source_key_collation_oid,
                        rerank.text_column_name,
                        rerank.text_attnum, rerank.text_type_oid,
                        rerank.text_collation_oid, rerank.source_version_column_name,
                        rerank.source_version_attnum, rerank.source_version_type_oid,
                        profiles.profile_revision, profiles.parser_revision,
                        profiles.target_tokens, profiles.max_tokens, profiles.min_tokens,
                        profiles.overlap_tokens, profiles.max_document_bytes,
                        profiles.include_structure_context,
                        sources.projection_table_oid, sources.projection_schema_name,
                        sources.projection_table_name,
                        sources.projection_column_attnums,
                        sources.projection_column_type_oids,
                        sources.projection_column_collation_oids,
                        sources.status, rerank.status, profiles.status, jobs.status
                   FROM pgcontext._visible_document_chunk_jobs AS jobs
                   JOIN pgcontext._visible_document_chunk_generations AS generations
                     ON generations.generation_id = jobs.generation_id
                   JOIN pgcontext._visible_document_sources AS sources
                     ON sources.document_source_id = generations.document_source_id
                   JOIN pgcontext._visible_semantic_rerank_sources AS rerank
                     ON rerank.rerank_source_id = sources.rerank_source_id
                   JOIN pgcontext._visible_chunking_profiles AS profiles
                     ON profiles.chunking_profile_id = generations.chunking_profile_id
                   JOIN pgcontext._visible_chunking_profile_aliases AS aliases
                     ON aliases.chunking_profile_alias_id = sources.chunking_profile_alias_id
                    AND (
                        generations.chunking_profile_id IN (
                            aliases.chunking_profile_id,
                            COALESCE(
                                aliases.shadow_chunking_profile_id,
                                aliases.chunking_profile_id
                            )
                        )
                        OR (
                            jobs.status = 'ready'
                            AND EXISTS (
                                SELECT 1
                                  FROM pgcontext._visible_chunking_profile_alias_retained AS retained
                                 WHERE retained.chunking_profile_alias_id =
                                           aliases.chunking_profile_alias_id
                                   AND retained.chunking_profile_id =
                                           generations.chunking_profile_id
                            )
                        )
                    )
                    AND aliases.status = 'ready'
                  WHERE jobs.job_id = $1",
                Some(1),
                &[
                    job_id.into(),
                    lease_token.into(),
                    lease_expires_micros.into(),
                ],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to load document chunk job",
                )
            });
        if rows.is_empty() {
            arm_document_chunk_permit(DocumentChunkPermitKind::LockJobAlias, job_id, lease_token);
            let alias_is_current = Spi::get_one_with_args::<bool>(
                "SELECT pgcontext._lock_document_chunk_job_alias($1,$2,false)",
                &[job_id.into(), lease_token.into()],
            )
            .ok()
            .flatten()
            .unwrap_or(false);
            if alias_is_current {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                    "document source row is no longer visible",
                );
            }
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
                "document chunk job lease is stale or unavailable",
            );
        }
        let row = rows.into_iter().next().unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "document chunk job row disappeared",
            )
        });
        let source_status: String = required(&row, 40, "document source status");
        let rerank_status: String = required(&row, 41, "semantic source status");
        let profile_status: String = required(&row, 42, "chunking profile status");
        if source_status != "ready" || rerank_status != "ready" || profile_status != "ready" {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "document chunk source or profile binding changed",
            );
        }
        JobContext {
            job_id: required(&row, 1, "job identity"),
            lease_token: required(&row, 2, "lease token"),
            job_status: required(&row, 43, "job status"),
            lease_expires_micros: required(&row, 3, "lease expiry"),
            generation_id: required(&row, 4, "generation identity"),
            document_source_id: required(&row, 5, "document source identity"),
            source_key: required(&row, 6, "source key"),
            source_version: required(&row, 7, "source version"),
            source_sha256: required(&row, 8, "source digest"),
            source_registration_revision: required(&row, 9, "stored source registration"),
            current_registration_revision: required(&row, 10, "current source registration"),
            source_table_oid: required(&row, 11, "source table identity"),
            source_schema: required(&row, 12, "source schema"),
            source_table: required(&row, 13, "source table"),
            source_key_attnum: required(&row, 14, "source key attribute"),
            source_key_type_oid: required(&row, 15, "source key type"),
            source_key_type_schema: required(&row, 16, "source key type schema"),
            source_key_type_name: required(&row, 17, "source key type name"),
            source_key_collation_oid: required(&row, 18, "source key collation"),
            text_column: required(&row, 19, "source text column"),
            text_attnum: required(&row, 20, "source text attribute"),
            text_type_oid: required(&row, 21, "source text type"),
            text_collation_oid: required(&row, 22, "source text collation"),
            version_column: required(&row, 23, "source version column"),
            version_attnum: required(&row, 24, "source version attribute"),
            version_type_oid: required(&row, 25, "source version type"),
            profile_revision: required(&row, 26, "profile revision"),
            parser: required(&row, 27, "parser revision"),
            target_tokens: required(&row, 28, "target tokens"),
            max_tokens: required(&row, 29, "maximum tokens"),
            min_tokens: required(&row, 30, "minimum tokens"),
            overlap_tokens: required(&row, 31, "overlap tokens"),
            max_document_bytes: required(&row, 32, "maximum document bytes"),
            include_structure_context: required(&row, 33, "structure context policy"),
            projection_table_oid: required(&row, 34, "projection table identity"),
            projection_schema: required(&row, 35, "projection schema"),
            projection_table: required(&row, 36, "projection table"),
            projection_attnums: required(&row, 37, "projection attribute identities"),
            projection_type_oids: required(&row, 38, "projection type identities"),
            projection_collation_oids: required(&row, 39, "projection collation identities"),
        }
    });
    validate_projection_identity(&context);
    context
}

pub(super) fn lock_and_validate_job_alias(context: &JobContext) {
    arm_document_chunk_permit(
        DocumentChunkPermitKind::LockJobAlias,
        context.job_id,
        context.lease_token,
    );
    let valid = Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._lock_document_chunk_job_alias($1,$2,$3)",
        &[
            context.job_id.into(),
            context.lease_token.into(),
            (context.job_status == "ready").into(),
        ],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !valid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "document chunk profile alias changed during worker lifecycle",
        );
    }
}

pub(super) fn hydrate_one_job_source(context: &JobContext) -> SourceRow {
    let source = job_source_registration(context);
    let mut rows = hydrate_source_keys_locked(&source, std::slice::from_ref(&context.source_key));
    if rows.len() != 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document source row is no longer visible",
        );
    }
    rows.pop().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "document source hydration returned no row",
        )
    })
}

pub(super) fn job_source_registration(context: &JobContext) -> SourceRegistration {
    SourceRegistration {
        document_source_id: context.document_source_id,
        registration_revision: context.current_registration_revision,
        chunking_profile_id: context.profile_revision,
        source_table_oid: context.source_table_oid,
        source_schema: context.source_schema.clone(),
        source_table: context.source_table.clone(),
        source_key_attnum: context.source_key_attnum,
        source_key_type_oid: context.source_key_type_oid,
        source_key_collation_oid: context.source_key_collation_oid,
        source_key_type_schema: context.source_key_type_schema.clone(),
        source_key_type_name: context.source_key_type_name.clone(),
        text_column: context.text_column.clone(),
        text_attnum: context.text_attnum,
        text_type_oid: context.text_type_oid,
        text_collation_oid: context.text_collation_oid,
        version_column: context.version_column.clone(),
        version_attnum: context.version_attnum,
        version_type_oid: context.version_type_oid,
        projection_table_oid: context.projection_table_oid,
        projection_schema: context.projection_schema.clone(),
        projection_table: context.projection_table.clone(),
        projection_attnums: context.projection_attnums.clone(),
        projection_type_oids: context.projection_type_oids.clone(),
        projection_collation_oids: context.projection_collation_oids.clone(),
        max_document_bytes: context.max_document_bytes,
    }
}

pub(super) fn require_source_matches(context: &JobContext, source: &SourceRow) {
    if !source_matches(context, source) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document source changed during chunk generation",
        );
    }
}

pub(super) fn source_matches(context: &JobContext, source: &SourceRow) -> bool {
    let digest = Sha256::digest(source.text.as_bytes());
    source.source_key == context.source_key
        && source.source_version == context.source_version
        && digest.as_slice() == context.source_sha256
        && context.source_registration_revision == context.current_registration_revision
}

pub(super) fn terminate_stale_claim(job_id: i64, lease_token: i64) {
    arm_document_chunk_permit(DocumentChunkPermitKind::SupersedeClaim, job_id, lease_token);
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._supersede_document_chunk_claim($1,$2)",
        &[job_id.into(), lease_token.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "failed to retire a stale document chunk claim",
        )
    });
}

pub(super) fn require_active_job(context: &JobContext) {
    if !matches!(
        context.job_status.as_str(),
        "leased" | "parsing" | "chunking" | "embedding" | "validating" | "publishing"
    ) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "document chunk job lease is stale or unavailable",
        );
    }
}

pub(super) fn job_cancellation_requested(job_id: i64) -> bool {
    Spi::get_one_with_args::<bool>(
        "SELECT status IN ('cancel_requested','cancelled')
           FROM pgcontext._visible_document_chunk_jobs
          WHERE job_id = $1",
        &[job_id.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(true)
}

pub(super) fn require_authorized_leased_job(job_id: i64, lease_token: i64) -> JobContext {
    let context = load_job_context(job_id, lease_token);
    let source = hydrate_one_job_source(&context);
    lock_and_validate_job_alias(&context);
    require_source_matches(&context, &source);
    context
}

pub(super) fn require_authorized_job(job_id: i64) {
    let identity = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT sources.collection_id, sources.source_name,
                        generations.source_key, generations.source_version,
                        generations.source_sha256,
                        generations.source_registration_revision
                   FROM pgcontext._visible_document_chunk_jobs AS jobs
                   JOIN pgcontext._visible_document_chunk_generations AS generations
                     USING (generation_id)
                   JOIN pgcontext._visible_document_sources AS sources
                     ON sources.document_source_id = generations.document_source_id
                  WHERE jobs.job_id = $1",
                Some(1),
                &[job_id.into()],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to validate document chunk job authority",
                )
            });
        let row = rows.into_iter().next().unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                "document chunk job is not visible",
            )
        });
        (
            required::<i64>(&row, 1, "collection identity"),
            required::<String>(&row, 2, "document source name"),
            required::<String>(&row, 3, "source key"),
            required::<i64>(&row, 4, "source version"),
            required::<Vec<u8>>(&row, 5, "source digest"),
            required::<i64>(&row, 6, "source registration revision"),
        )
    });
    let registration = load_source_registration_readonly(identity.0, &identity.1);
    let mut rows = hydrate_source_keys_locked(&registration, std::slice::from_ref(&identity.2));
    if rows.len() != 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "document source row is not visible",
        );
    }
    let row = rows.pop().unwrap_or_else(|| invalid_job_identity());
    let digest: Vec<u8> = Sha256::digest(row.text.as_bytes()).to_vec();
    if row.source_version != identity.3
        || digest != identity.4
        || registration.registration_revision != identity.5
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document source changed during chunk generation",
        );
    }
}

pub(super) fn require_buildable_job(job_id: i64) {
    arm_document_chunk_permit(DocumentChunkPermitKind::LockJobAlias, job_id, 0);
    let buildable = Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._lock_document_chunk_job_alias($1,0,false)",
        &[job_id.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !buildable {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk job profile is not a current build target",
        );
    }
}

pub(super) fn require_source_table_owner(source: &SourceRegistration) {
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(
                    SESSION_USER,
                    (SELECT class.relowner FROM pg_catalog.pg_class AS class WHERE class.oid = $1),
                    'USAGE'
                )",
        &[source.source_table_oid.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !allowed || !source_select_allowed(source.source_table_oid) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "document invalidation requires source-table ownership and SELECT",
        );
    }
}

pub(super) fn projected_worker_request_bytes(context: &JobContext, source: &SourceRow) -> usize {
    const VALUE_AND_CONTAINER_OVERHEAD: usize = 16 * 1024;
    let escaped_text = json_string_encoded_bytes(&source.text);
    source
        .text
        .len()
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(escaped_text))
        .and_then(|bytes| bytes.checked_add(context.source_key.len()))
        .and_then(|bytes| bytes.checked_add(context.parser.len()))
        .and_then(|bytes| bytes.checked_add(VALUE_AND_CONTAINER_OVERHEAD))
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "document chunk claim response exceeds aggregate byte limit",
            )
        })
}

fn json_string_encoded_bytes(value: &str) -> usize {
    value.bytes().fold(2usize, |total, byte| {
        let encoded = match byte {
            b'"' | b'\\' => 2,
            0..=0x1f => 6,
            _ => 1,
        };
        total.saturating_add(encoded)
    })
}

pub(super) fn worker_request(context: &JobContext, source: &SourceRow) -> Value {
    let document_id = document_identity(context.document_source_id, &context.source_key);
    json!({
        "version": "chunk_worker_request_v1",
        "request_id": context.job_id,
        "document_id": document_id,
        "source_version": context.source_version,
        "source_hash": hex(&context.source_sha256),
        "profile_revision": context.profile_revision,
        "parser": context.parser,
        "tokenizer_revision": "unicode_words_v1",
        "expires_at_micros": context.lease_expires_micros,
        "profile": {
            "target_tokens": context.target_tokens,
            "max_tokens": context.max_tokens,
            "min_tokens": context.min_tokens,
            "overlap_tokens": context.overlap_tokens,
            "max_document_bytes": context.max_document_bytes,
            "include_structure_context": context.include_structure_context
        },
        "source_text": source.text
    })
}

pub(super) fn canonical_response(context: &JobContext, source: &SourceRow) -> Value {
    let parser = parse_parser(&context.parser);
    let profile = token_profile(
        context.target_tokens,
        context.max_tokens,
        context.min_tokens,
        context.overlap_tokens,
        context.max_document_bytes,
    );
    let identity = ChunkIdentityContext::new(
        document_identity(context.document_source_id, &context.source_key),
        u64::try_from(context.source_version).unwrap_or_else(|_| invalid_job_identity()),
        u64::try_from(context.profile_revision).unwrap_or_else(|_| invalid_job_identity()),
    )
    .unwrap_or_else(|| invalid_job_identity());
    let lease_expires_micros = context.lease_expires_micros;
    let started = std::time::Instant::now();
    let chunks = chunk_document_tokens_with_identity_and_checkpoint(
        &source.text,
        parser,
        profile,
        identity,
        || {
            pgrx::pg_sys::check_for_interrupts!();
            started.elapsed().as_micros() < u128::from(MAX_DOCUMENT_CHUNK_ELAPSED_MICROS)
                && current_unix_micros().is_some_and(|now| now < lease_expires_micros)
                && !job_cancellation_requested(context.job_id)
        },
    )
    .unwrap_or_else(|error| match error {
        context_build::TokenChunkError::DeadlineExceeded => raise_sql_error(
            PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            "document chunking exceeded its elapsed limit",
        ),
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "document exceeds the registered chunking profile",
        ),
    });
    let chunks = chunks
        .into_iter()
        .map(|chunk| {
            let context_prefix =
                context_prefix(context.include_structure_context, chunk.structure_path());
            json!({
                "occurrence_id": chunk.occurrence_id().get(),
                "ordinal": chunk.ordinal(),
                "start_byte": chunk.start_byte(),
                "end_byte": chunk.end_byte(),
                "start_char": chunk.start_char(),
                "end_char": chunk.end_char(),
                "token_count": chunk.token_count(),
                "original_text": chunk.original_text(),
                "retrieval_text": chunk.retrieval_text(),
                "structure_kind": structure_kind(chunk.structure_kind()),
                "structure_path": chunk.structure_path(),
                "content_hash": chunk.content_hash(),
                "parent_occurrence_id": chunk.parent_occurrence_id().map(|value| value.get()),
                "previous_occurrence_id": chunk.previous_occurrence_id().map(|value| value.get()),
                "next_occurrence_id": chunk.next_occurrence_id().map(|value| value.get()),
                "context_prefix": context_prefix,
                "context_prefix_hash": context_prefix.as_deref().map(stable_hash)
            })
        })
        .collect::<Vec<_>>();
    json!({
        "version": "chunk_worker_response_v1",
        "request_id": context.job_id,
        "document_id": identity.document_id(),
        "source_version": context.source_version,
        "source_hash": hex(&context.source_sha256),
        "profile_revision": context.profile_revision,
        "complete": true,
        "chunks": chunks
    })
}

fn document_identity(document_source_id: i64, source_key: &str) -> u64 {
    if document_source_id <= 0 || source_key.is_empty() {
        invalid_job_identity();
    }
    let mut digest = Sha256::new();
    digest.update(b"pgcontext-document-row-v1");
    digest.update(document_source_id.to_le_bytes());
    digest.update(
        u64::try_from(source_key.len())
            .unwrap_or_else(|_| invalid_job_identity())
            .to_le_bytes(),
    );
    digest.update(source_key.as_bytes());
    let bytes: [u8; 8] = digest.finalize()[..8]
        .try_into()
        .unwrap_or_else(|_| invalid_job_identity());
    (u64::from_le_bytes(bytes) & i64::MAX as u64).max(1)
}

fn current_unix_micros() -> Option<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_micros()).ok())
}

pub(super) fn load_staged_response(job_id: i64, lease_token: i64) -> StagedResponse {
    arm_document_chunk_permit(DocumentChunkPermitKind::LoadStaging, job_id, lease_token);
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT response_json, response_sha256, chunk_count, token_count, staging_bytes
                   FROM pgcontext._load_document_chunk_staging($1, $2)",
                Some(1),
                &[job_id.into(), lease_token.into()],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to load staged document chunks",
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "document chunk response has not been staged",
            );
        }
        let row = rows.into_iter().next().unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "document chunk staging row disappeared",
            )
        });
        StagedResponse {
            response: required::<JsonB>(&row, 1, "staged response").0,
            response_sha256: required(&row, 2, "staged response digest"),
            chunk_count: required(&row, 3, "staged chunk count"),
            token_count: required(&row, 4, "staged token count"),
            staging_bytes: required(&row, 5, "staging bytes"),
        }
    })
}

pub(super) fn validate_projection_identity(context: &JobContext) {
    let qualified = quote_qualified(&context.projection_schema, &context.projection_table);
    validate_current_relation(context.projection_table_oid, &qualified);
    let current = resolve_projection_contract(context.projection_table_oid);
    if current.attnums != context.projection_attnums
        || current.type_oids != context.projection_type_oids
        || current.collation_oids != context.projection_collation_oids
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk projection binding changed",
        );
    }
}

pub(super) fn insert_projection_rows(context: &JobContext, response: &Value) {
    let projection = quote_qualified(&context.projection_schema, &context.projection_table);
    let sql = format!(
        "INSERT INTO {projection} (
            document_source_id, source_key, source_version, source_sha256,
            profile_revision, generation_id, occurrence_id, ordinal,
            original_text, retrieval_text, start_byte, end_byte, start_char, end_char,
            token_count, structure_kind, structure_path, page_number, region,
            parent_occurrence_id,
            previous_occurrence_id, next_occurrence_id, content_hash,
            context_prefix, context_prefix_hash, fake_embedding, ready, provenance
         )
         SELECT $2, $3, $4, $5, $6, $7,
                chunk.occurrence_id, chunk.ordinal, chunk.original_text,
                chunk.retrieval_text, chunk.start_byte, chunk.end_byte,
                chunk.start_char, chunk.end_char, chunk.token_count,
                chunk.structure_kind,
                ARRAY(SELECT value FROM pg_catalog.jsonb_array_elements_text(chunk.structure_path)),
                NULL::int4, NULL::jsonb,
                chunk.parent_occurrence_id, chunk.previous_occurrence_id,
                chunk.next_occurrence_id, chunk.content_hash,
                chunk.context_prefix, chunk.context_prefix_hash,
                pg_catalog.jsonb_build_array(
                    ((chunk.content_hash & 65535)::double precision / 65535.0),
                    (((chunk.content_hash >> 16) & 65535)::double precision / 65535.0),
                    (((chunk.content_hash >> 32) & 65535)::double precision / 65535.0)
                ), true,
                pg_catalog.jsonb_build_object(
                    'parser', $8::text, 'tokenizer', 'unicode_words_v1',
                    'profile_revision', $6::bigint, 'source_version', $4::bigint,
                    'complete', true, 'embedding', 'deterministic_fake_v1'
                )
           FROM pg_catalog.jsonb_to_recordset($1::jsonb->'chunks') AS chunk(
                occurrence_id bigint, ordinal int4, original_text text,
                retrieval_text text, start_byte bigint, end_byte bigint,
                start_char bigint, end_char bigint, token_count int4,
                structure_kind text, structure_path jsonb,
                parent_occurrence_id bigint, previous_occurrence_id bigint,
                next_occurrence_id bigint, content_hash bigint,
                context_prefix text, context_prefix_hash bigint
           )
         ON CONFLICT (generation_id, occurrence_id) DO UPDATE
           SET original_text = EXCLUDED.original_text,
               retrieval_text = EXCLUDED.retrieval_text,
               provenance = EXCLUDED.provenance
         WHERE {projection}.source_sha256 = EXCLUDED.source_sha256
           AND {projection}.profile_revision = EXCLUDED.profile_revision"
    );
    Spi::run_with_args(
        &sql,
        &[
            JsonB(response.clone()).into(),
            context.document_source_id.into(),
            context.source_key.as_str().into(),
            context.source_version.into(),
            context.source_sha256.clone().into(),
            context.profile_revision.into(),
            context.generation_id.into(),
            context.parser.as_str().into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to publish document chunk projection rows",
        )
    });
}

pub(super) fn verify_projection_rows(context: &JobContext, response: &Value, expected_count: i32) {
    let projection = quote_qualified(&context.projection_schema, &context.projection_table);
    let sql = format!(
        "WITH expected AS (
             SELECT chunk.occurrence_id, chunk.ordinal, chunk.original_text,
                    chunk.retrieval_text, chunk.start_byte, chunk.end_byte,
                    chunk.start_char, chunk.end_char, chunk.token_count,
                    chunk.structure_kind,
                    ARRAY(SELECT value FROM pg_catalog.jsonb_array_elements_text(chunk.structure_path)) AS structure_path,
                    NULL::int4 AS page_number, NULL::jsonb AS region,
                    chunk.parent_occurrence_id, chunk.previous_occurrence_id,
                    chunk.next_occurrence_id, chunk.content_hash,
                    chunk.context_prefix, chunk.context_prefix_hash,
                    pg_catalog.jsonb_build_array(
                        ((chunk.content_hash & 65535)::double precision / 65535.0),
                        (((chunk.content_hash >> 16) & 65535)::double precision / 65535.0),
                        (((chunk.content_hash >> 32) & 65535)::double precision / 65535.0)
                    ) AS fake_embedding,
                    pg_catalog.jsonb_build_object(
                        'parser', $9::text, 'tokenizer', 'unicode_words_v1',
                        'profile_revision', $8::bigint, 'source_version', $6::bigint,
                        'complete', true, 'embedding', 'deterministic_fake_v1'
                    ) AS provenance
               FROM pg_catalog.jsonb_to_recordset($1::jsonb->'chunks') AS chunk(
                    occurrence_id bigint, ordinal int4, original_text text,
                    retrieval_text text, start_byte bigint, end_byte bigint,
                    start_char bigint, end_char bigint, token_count int4,
                    structure_kind text, structure_path jsonb,
                    parent_occurrence_id bigint, previous_occurrence_id bigint,
                    next_occurrence_id bigint, content_hash bigint,
                    context_prefix text, context_prefix_hash bigint
               )
         ), actual AS (
             SELECT * FROM {projection} WHERE generation_id = $2
         )
         SELECT (SELECT pg_catalog.count(*) FROM expected) = $3
            AND (SELECT pg_catalog.count(*) FROM actual) = $3
            AND NOT EXISTS (
                SELECT 1 FROM expected
                FULL JOIN actual USING (occurrence_id)
                 WHERE expected.occurrence_id IS NULL OR actual.occurrence_id IS NULL
                    OR actual.document_source_id IS DISTINCT FROM $4
                    OR actual.source_key IS DISTINCT FROM $5
                    OR actual.source_version IS DISTINCT FROM $6
                    OR actual.source_sha256 IS DISTINCT FROM $7
                    OR actual.profile_revision IS DISTINCT FROM $8
                    OR actual.ordinal IS DISTINCT FROM expected.ordinal
                    OR actual.original_text IS DISTINCT FROM expected.original_text
                    OR actual.retrieval_text IS DISTINCT FROM expected.retrieval_text
                    OR actual.start_byte IS DISTINCT FROM expected.start_byte
                    OR actual.end_byte IS DISTINCT FROM expected.end_byte
                    OR actual.start_char IS DISTINCT FROM expected.start_char
                    OR actual.end_char IS DISTINCT FROM expected.end_char
                    OR actual.token_count IS DISTINCT FROM expected.token_count
                    OR actual.structure_kind IS DISTINCT FROM expected.structure_kind
                    OR actual.structure_path IS DISTINCT FROM expected.structure_path
                    OR actual.page_number IS DISTINCT FROM expected.page_number
                    OR actual.region IS DISTINCT FROM expected.region
                    OR actual.parent_occurrence_id IS DISTINCT FROM expected.parent_occurrence_id
                    OR actual.previous_occurrence_id IS DISTINCT FROM expected.previous_occurrence_id
                    OR actual.next_occurrence_id IS DISTINCT FROM expected.next_occurrence_id
                    OR actual.content_hash IS DISTINCT FROM expected.content_hash
                    OR actual.context_prefix IS DISTINCT FROM expected.context_prefix
                    OR actual.context_prefix_hash IS DISTINCT FROM expected.context_prefix_hash
                    OR actual.fake_embedding IS DISTINCT FROM expected.fake_embedding
                    OR actual.provenance IS DISTINCT FROM expected.provenance
                    OR actual.ready IS DISTINCT FROM true
            )"
    );
    let complete = Spi::get_one_with_args::<bool>(
        &sql,
        &[
            JsonB(response.clone()).into(),
            context.generation_id.into(),
            expected_count.into(),
            context.document_source_id.into(),
            context.source_key.as_str().into(),
            context.source_version.into(),
            context.source_sha256.clone().into(),
            context.profile_revision.into(),
            context.parser.as_str().into(),
        ],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !complete {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk projection publication is incomplete",
        );
    }
}
