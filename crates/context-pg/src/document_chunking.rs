//! PostgreSQL authority, lifecycle, staging, and publication for P13 chunking.

use std::collections::BTreeSet;

use context_build::{
    ChunkIdentityContext, DocumentParser, MAX_CERTIFIED_PROFILE_OVERLAP_TOKENS, TokenChunkProfile,
    chunk_document_tokens_with_identity_and_checkpoint,
};
use pgrx::{JsonB, prelude::*};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{raise_query_error, raise_sql_error};

mod contract;
mod datum;
mod enqueue_api;
mod lifecycle_api;
mod permit;
mod profile_api;
mod query;
mod runtime;

use contract::*;
use datum::{BoundedChunkResponse, BoundedSourceKeys};
use permit::*;
use runtime::*;

const MAX_PROFILE_NAME_BYTES: usize = 128;
const MAX_SOURCE_NAME_BYTES: usize = 128;
const MAX_SOURCE_KEYS: usize = 256;
const MAX_SOURCE_KEY_BYTES: usize = 1024;
const MAX_STAGING_BYTES: usize = 32 * 1024 * 1024;
const MAX_WORKER_CLAIM_BYTES: usize = 40 * 1024 * 1024;
const MAX_STAGING_JSON_NODES: usize = 1_000_000;
const MAX_STAGING_JSON_DEPTH: usize = 64;
const MAX_DOCUMENT_CHUNK_ELAPSED_MICROS: u64 = 120_000_000;

/// Applies current source-table ACL/RLS to one registered document key.
///
/// This internal predicate intentionally executes as the invoker. Public
/// membership views use it so collection membership alone never reveals a
/// hidden source row or its derived lifecycle state.
#[pg_extern(name = "_document_chunk_source_key_visible")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn document_chunk_source_key_visible(document_source_id: i64, source_key: String) -> bool {
    if document_source_id <= 0 || source_key.is_empty() || source_key.len() > MAX_SOURCE_KEY_BYTES {
        return false;
    }
    source_key_visible(document_source_id, &source_key)
}

#[derive(Clone, Debug)]
struct JobContext {
    job_id: i64,
    lease_token: i64,
    job_status: String,
    lease_expires_micros: i64,
    generation_id: i64,
    document_source_id: i64,
    source_key: String,
    source_version: i64,
    source_sha256: Vec<u8>,
    source_registration_revision: i64,
    current_registration_revision: i64,
    source_table_oid: pg_sys::Oid,
    source_schema: String,
    source_table: String,
    source_key_attnum: i16,
    source_key_type_oid: pg_sys::Oid,
    source_key_collation_oid: pg_sys::Oid,
    source_key_type_schema: String,
    source_key_type_name: String,
    text_column: String,
    text_attnum: i16,
    text_type_oid: pg_sys::Oid,
    text_collation_oid: pg_sys::Oid,
    version_column: String,
    version_attnum: i16,
    version_type_oid: pg_sys::Oid,
    profile_revision: i64,
    parser: String,
    target_tokens: i32,
    max_tokens: i32,
    min_tokens: i32,
    overlap_tokens: i32,
    max_document_bytes: i64,
    include_structure_context: bool,
    projection_table_oid: pg_sys::Oid,
    projection_schema: String,
    projection_table: String,
    projection_attnums: Vec<i16>,
    projection_type_oids: Vec<pg_sys::Oid>,
    projection_collation_oids: Vec<pg_sys::Oid>,
}

/// Creates one user-owned table with the frozen P13 chunk projection contract.
///
/// The function runs as the caller. The target schema must already exist and
/// grant the caller `CREATE`; pgContext never owns the resulting projection.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn create_document_chunk_projection(table_name: String) -> String {
    let (schema, table) = qualified_name(&table_name);
    let qualified = quote_qualified(&schema, &table);
    let sql = format!(
        "CREATE TABLE {qualified} (
            document_source_id bigint NOT NULL,
            source_key text NOT NULL,
            source_version bigint NOT NULL CHECK (source_version > 0),
            source_sha256 bytea NOT NULL CHECK (pg_catalog.octet_length(source_sha256) = 32),
            profile_revision bigint NOT NULL CHECK (profile_revision > 0),
            generation_id bigint NOT NULL CHECK (generation_id > 0),
            occurrence_id bigint NOT NULL CHECK (occurrence_id > 0),
            ordinal int4 NOT NULL CHECK (ordinal >= 0),
            original_text text NOT NULL,
            retrieval_text text NOT NULL,
            start_byte bigint NOT NULL,
            end_byte bigint NOT NULL,
            start_char bigint NOT NULL,
            end_char bigint NOT NULL,
            token_count int4 NOT NULL CHECK (token_count BETWEEN 1 AND 512),
            structure_kind text NOT NULL,
            structure_path text[] NOT NULL,
            page_number int4,
            region jsonb,
            parent_occurrence_id bigint,
            previous_occurrence_id bigint,
            next_occurrence_id bigint,
            content_hash bigint NOT NULL,
            context_prefix text,
            context_prefix_hash bigint,
            fake_embedding jsonb NOT NULL CHECK (pg_catalog.jsonb_typeof(fake_embedding) = 'array'),
            ready boolean NOT NULL DEFAULT true,
            provenance jsonb NOT NULL,
            PRIMARY KEY (generation_id, occurrence_id),
            UNIQUE (generation_id, ordinal)
        )"
    );
    Spi::run(&sql).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_SCHEMA_NAME,
            "failed to create document chunk projection",
        )
    });
    table_name
}

/// Registers one immutable token-aware chunking profile.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::too_many_arguments,
    reason = "the SQL profile exposes each frozen bound"
)]
pub fn register_chunking_profile(
    profile_name: String,
    parser: String,
    target_tokens: i32,
    max_tokens: i32,
    min_tokens: i32,
    overlap_tokens: i32,
    max_document_bytes: i64,
    include_structure_context: bool,
) -> i64 {
    validate_bounded_name(&profile_name, MAX_PROFILE_NAME_BYTES, "chunking profile");
    if usize::try_from(overlap_tokens).unwrap_or(usize::MAX) > MAX_CERTIFIED_PROFILE_OVERLAP_TOKENS
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "chunking profile overlap exceeds the certified ceiling",
        );
    }
    let parser = parse_parser(&parser);
    let profile = token_profile(
        target_tokens,
        max_tokens,
        min_tokens,
        overlap_tokens,
        max_document_bytes,
    );
    let configuration = format!(
        "{}|unicode_words_v1|{}|{}|{}|{}|{}|{}",
        parser.as_str(),
        profile.target_tokens(),
        profile.max_tokens(),
        profile.min_tokens(),
        profile.overlap_tokens(),
        max_document_bytes,
        include_structure_context
    );
    let digest: Vec<u8> = Sha256::digest(configuration.as_bytes()).to_vec();
    arm_document_chunk_permit(DocumentChunkPermitKind::RegisterProfile, 0, 0);
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._register_chunking_profile($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        &[
            profile_name.as_str().into(),
            parser.as_str().into(),
            target_tokens.into(),
            max_tokens.into(),
            min_tokens.into(),
            overlap_tokens.into(),
            max_document_bytes.into(),
            include_structure_context.into(),
            digest.into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to register chunking profile",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "chunking profile registration returned no identity",
        )
    })
}

/// Registers a source text/version binding and user-owned projection table.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn register_document_source(
    collection: String,
    source_name: String,
    text_column: String,
    source_version_column: String,
    projection_table: String,
    profile_name: String,
) -> i64 {
    validate_bounded_name(&source_name, MAX_SOURCE_NAME_BYTES, "document source");
    validate_identifier(&text_column, "document text column");
    validate_identifier(&source_version_column, "document version column");
    validate_bounded_name(&profile_name, MAX_PROFILE_NAME_BYTES, "chunking profile");
    let collection_id = require_collection_owner(&collection);
    let rerank_source_id = Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._register_semantic_rerank_source($1,$2,$3,$4)",
        &[
            collection_id.into(),
            source_name.as_str().into(),
            text_column.as_str().into(),
            source_version_column.as_str().into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "failed to register document source binding",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "document source binding returned no identity",
        )
    });
    let (profile_alias_id, profile_id) = Spi::get_two_with_args::<i64, i64>(
        "SELECT CASE WHEN pg_catalog.count(*) = 1
                     THEN pg_catalog.min(chunking_profile_alias_id) END,
                CASE WHEN pg_catalog.count(*) = 1
                     THEN pg_catalog.min(chunking_profile_id) END
           FROM pgcontext._visible_chunking_profile_aliases
          WHERE alias_name = $1 AND status = 'ready'",
        &[profile_name.as_str().into()],
    )
    .ok()
    .and_then(|(alias_id, profile_id)| alias_id.zip(profile_id))
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "chunking profile alias is missing or ambiguous",
        )
    });
    let (projection_oid, projection_schema, projection_name) =
        resolve_projection(&projection_table);
    let projection_contract = resolve_projection_contract(projection_oid);
    let projection_collation_oids = projection_contract
        .collation_oids
        .iter()
        .map(|oid| i64::from(oid.to_u32()))
        .collect::<Vec<_>>();
    arm_document_chunk_permit(
        DocumentChunkPermitKind::RegisterSource,
        collection_id,
        rerank_source_id,
    );
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._register_document_source($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
        &[
            collection_id.into(),
            source_name.as_str().into(),
            rerank_source_id.into(),
            profile_id.into(),
            profile_alias_id.into(),
            projection_oid.into(),
            projection_schema.as_str().into(),
            projection_name.as_str().into(),
            projection_contract.attnums.into(),
            projection_contract.type_oids.into(),
            projection_collation_oids.into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to register document source",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "document source registration returned no identity",
        )
    })
}

/// Installs the transactional source-change outbox trigger for a registration.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn install_document_chunk_trigger(collection: String, source_name: String) -> String {
    let collection_id = require_collection_owner(&collection);
    let source = load_source_registration(collection_id, &source_name);
    require_source_table_owner(&source);
    arm_document_chunk_permit(
        DocumentChunkPermitKind::InstallTrigger,
        source.document_source_id,
        0,
    );
    Spi::get_one_with_args::<String>(
        "SELECT pgcontext._install_document_chunk_outbox_trigger($1)",
        &[source.document_source_id.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "failed to install document chunk outbox trigger",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "document chunk outbox trigger returned no identity",
        )
    })
}

/// Claims bounded jobs and returns source-authorized worker envelopes.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL row shape inline"
)]
pub fn claim_document_chunk_jobs(
    limit: i32,
    lease_millis: i32,
    worker_id: String,
) -> TableIterator<
    'static,
    (
        name!(job_id, i64),
        name!(lease_token, i64),
        name!(request, JsonB),
    ),
> {
    validate_claim_bounds(limit, lease_millis, &worker_id);
    let target = usize::try_from(limit).unwrap_or_else(|_| invalid_job_identity());
    let mut rows = Vec::with_capacity(target);
    let mut projected_bytes = 0usize;
    let mut scanned = 0usize;
    while rows.len() < target && scanned < MAX_SOURCE_KEYS {
        let remaining = i32::try_from(target - rows.len()).unwrap_or(1);
        let claimed_ids = claim_job_identities(remaining, lease_millis, &worker_id);
        if claimed_ids.is_empty() {
            break;
        }
        scanned = scanned
            .checked_add(claimed_ids.len())
            .unwrap_or(MAX_SOURCE_KEYS);
        for (job_id, lease_token) in claimed_ids {
            if !claimed_job_source_select_allowed(job_id, lease_token) {
                release_document_chunk_claim(job_id, lease_token);
                continue;
            }
            let context = load_job_context(job_id, lease_token);
            let registration = job_source_registration(&context);
            let mut authorized = if source_select_allowed(registration.source_table_oid) {
                hydrate_source_keys(&registration, std::slice::from_ref(&context.source_key))
            } else {
                Vec::new()
            };
            if authorized.len() != 1 {
                release_document_chunk_claim(job_id, lease_token);
                continue;
            }
            let source = authorized.pop().unwrap_or_else(|| invalid_job_identity());
            lock_and_validate_job_alias(&context);
            if !source_matches(&context, &source) {
                terminate_stale_claim(job_id, lease_token);
                continue;
            }
            projected_bytes = projected_bytes
                .checked_add(projected_worker_request_bytes(&context, &source))
                .filter(|bytes| *bytes <= MAX_WORKER_CLAIM_BYTES)
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                        "document chunk claim response exceeds aggregate byte limit",
                    )
                });
            rows.push((
                job_id,
                lease_token,
                JsonB(worker_request(&context, &source)),
            ));
        }
    }
    TableIterator::new(rows)
}

/// Records one monotonic worker progress checkpoint under the current lease.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn checkpoint_document_chunk_job(
    job_id: i64,
    lease_token: i64,
    status: String,
    processed_units: i64,
    total_units: i64,
) -> bool {
    if !matches!(status.as_str(), "parsing" | "chunking" | "embedding")
        || !(1..=1_000_000).contains(&total_units)
        || processed_units < 0
        || processed_units > total_units
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "invalid document chunk progress checkpoint",
        );
    }
    require_authorized_leased_job(job_id, lease_token);
    arm_document_chunk_permit(DocumentChunkPermitKind::Checkpoint, job_id, lease_token);
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._checkpoint_document_chunk_job($1,$2,$3,$4,$5)",
        &[
            job_id.into(),
            lease_token.into(),
            status.into(),
            processed_units.into(),
            total_units.into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "failed to checkpoint document chunk job",
        )
    })
    .unwrap_or(false)
}

/// Terminates a leased job with one bounded content-free worker failure code.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn fail_document_chunk_job(job_id: i64, lease_token: i64, error_code: String) -> bool {
    if error_code.is_empty()
        || error_code.len() > 64
        || !error_code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "invalid document chunk failure code",
        );
    }
    require_authorized_leased_job(job_id, lease_token);
    arm_document_chunk_permit(DocumentChunkPermitKind::Fail, job_id, lease_token);
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._fail_document_chunk_job($1,$2,$3)",
        &[job_id.into(), lease_token.into(), error_code.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "failed to terminate document chunk job",
        )
    })
    .unwrap_or(false)
}

/// Validates and stages one complete worker response under a fenced lease.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn stage_document_chunks(
    job_id: i64,
    lease_token: i64,
    response: BoundedChunkResponse,
) -> bool {
    let timeout =
        crate::retrieval::current_statement_timeout::Guard::arm(MAX_DOCUMENT_CHUNK_ELAPSED_MICROS)
            .unwrap_or_else(|error| raise_query_error(error));
    let response = response.0;
    let context = load_job_context(job_id, lease_token);
    require_active_job(&context);
    let source = hydrate_one_job_source(&context);
    lock_and_validate_job_alias(&context);
    require_source_matches(&context, &source);
    let canonical = canonical_response(&context, &source);
    if response.0 != canonical {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "document chunk worker response does not match canonical output",
        );
    }
    let encoded = serde_json::to_vec(&response.0).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "document chunk worker response is malformed",
        )
    });
    if encoded.len() > MAX_STAGING_BYTES {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "document chunk staging bytes exceed the configured limit",
        );
    }
    let chunks = response.0["chunks"].as_array().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "document chunk worker response is malformed",
        )
    });
    let token_count = chunks.iter().try_fold(0_i64, |total, chunk| {
        let count = chunk["token_count"].as_i64()?;
        total.checked_add(count)
    });
    let token_count = token_count.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "document chunk token total is invalid",
        )
    });
    let chunk_count = i32::try_from(chunks.len()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "document chunk count is outside the catalog domain",
        )
    });
    let response_digest: Vec<u8> = Sha256::digest(&encoded).to_vec();
    arm_document_chunk_permit(DocumentChunkPermitKind::Stage, job_id, lease_token);
    Spi::run_with_args(
        "SELECT pgcontext._stage_document_chunk_response($1,$2,$3,$4,$5,$6,$7)",
        &[
            job_id.into(),
            lease_token.into(),
            response.into(),
            response_digest.into(),
            chunk_count.into(),
            token_count.into(),
            i64::try_from(encoded.len())
                .unwrap_or_else(|_| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                        "document chunk staging size is outside the catalog domain",
                    )
                })
                .into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "failed to stage document chunks under the current lease",
        )
    });
    timeout.restore();
    true
}

/// Atomically publishes one validated staged generation.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn publish_document_chunk_generation(job_id: i64, lease_token: i64) -> i64 {
    let timeout =
        crate::retrieval::current_statement_timeout::Guard::arm(MAX_DOCUMENT_CHUNK_ELAPSED_MICROS)
            .unwrap_or_else(|error| raise_query_error(error));
    let context = load_job_context(job_id, lease_token);
    let source = hydrate_one_job_source(&context);
    lock_and_validate_job_alias(&context);
    require_source_matches(&context, &source);
    serialize_projection_publication(&context.projection_schema, &context.projection_table);
    if context.job_status == "ready" {
        let expected = Spi::get_one_with_args::<Vec<u8>>(
            "SELECT projection_sha256
               FROM pgcontext._visible_document_chunk_generations
              WHERE generation_id = $1 AND status = 'ready'",
            &[context.generation_id.into()],
        )
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "ready document chunk generation has no projection digest",
            )
        });
        require_projection_generation_digest(
            &context.projection_schema,
            &context.projection_table,
            context.generation_id,
            &expected,
        );
        timeout.restore();
        return context.generation_id;
    }
    let staged = load_staged_response(job_id, lease_token);
    let canonical = canonical_response(&context, &source);
    if staged.response != canonical {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "staged document chunks are no longer current",
        );
    }
    lock_and_require_current_source(&context);
    validate_projection_identity(&context);
    insert_projection_rows(&context, &staged.response);
    verify_projection_rows(&context, &staged.response, staged.chunk_count);
    let projection_digest = projection_generation_digest(
        &context.projection_schema,
        &context.projection_table,
        context.generation_id,
    );
    arm_document_chunk_permit(DocumentChunkPermitKind::Complete, job_id, lease_token);
    let generation_id = Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._complete_document_chunk_publication($1,$2,$3,$4,$5,$6,$7)",
        &[
            job_id.into(),
            lease_token.into(),
            staged.response_sha256.into(),
            projection_digest.into(),
            staged.chunk_count.into(),
            staged.token_count.into(),
            staged.staging_bytes.into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "document chunk publication lost its lease or source identity",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "document chunk publication returned no generation identity",
        )
    });
    timeout.restore();
    generation_id
}

/// Deterministic no-egress worker used to certify lifecycle/publication first.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn fake_process_document_chunk_job(job_id: i64, lease_token: i64) -> i64 {
    let context = load_job_context(job_id, lease_token);
    let source = hydrate_one_job_source(&context);
    lock_and_validate_job_alias(&context);
    require_source_matches(&context, &source);
    let response = BoundedChunkResponse(JsonB(canonical_response(&context, &source)));
    stage_document_chunks(job_id, lease_token, response);
    publish_document_chunk_generation(job_id, lease_token)
}

/// Renews a current fenced job lease inside the 60-second ceiling.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn heartbeat_document_chunk_job(job_id: i64, lease_token: i64, lease_millis: i32) -> bool {
    require_authorized_leased_job(job_id, lease_token);
    arm_document_chunk_permit(DocumentChunkPermitKind::Heartbeat, job_id, lease_token);
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._heartbeat_document_chunk_job($1,$2,$3)",
        &[job_id.into(), lease_token.into(), lease_millis.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "failed to renew document chunk job lease",
        )
    })
    .unwrap_or(false)
}

/// Requests cancellation while retaining the prior ready generation.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn cancel_document_chunk_job(job_id: i64) -> bool {
    require_authorized_job(job_id);
    arm_document_chunk_permit(DocumentChunkPermitKind::Cancel, job_id, 0);
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._cancel_document_chunk_job($1)",
        &[job_id.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to cancel document chunk job",
        )
    })
    .unwrap_or(false)
}

/// Requeues a cancelled or failed job within its three-attempt ceiling.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn retry_document_chunk_job(job_id: i64) -> bool {
    require_authorized_job(job_id);
    require_buildable_job(job_id);
    arm_document_chunk_permit(DocumentChunkPermitKind::Retry, job_id, 0);
    Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._retry_document_chunk_job($1)",
        &[job_id.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to retry document chunk job",
        )
    })
    .unwrap_or(false)
}

/// Invalidates current aliases for explicitly deleted/tombstoned source keys.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn invalidate_document_chunks(
    collection: String,
    source_name: String,
    source_keys: BoundedSourceKeys,
) -> i64 {
    let source_keys = source_keys.into_vec();
    let collection_id = require_collection_owner(&collection);
    let source = load_source_registration(collection_id, &source_name);
    require_source_table_owner(&source);
    arm_document_chunk_permit(
        DocumentChunkPermitKind::Invalidate,
        source.document_source_id,
        i64::try_from(source_keys.len()).unwrap_or_else(|_| invalid_job_identity()),
    );
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._invalidate_document_chunk_aliases($1,$2)",
        &[source.document_source_id.into(), source_keys.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to invalidate document chunk aliases",
        )
    })
    .unwrap_or(0)
}

/// Makes a previously published generation current again for one document.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn rollback_document_chunk_generation(
    collection: String,
    source_name: String,
    source_key: String,
    generation_id: i64,
) -> i64 {
    validate_source_keys(std::slice::from_ref(&source_key));
    if generation_id <= 0 {
        invalid_job_identity();
    }
    let collection_id = require_collection_owner(&collection);
    let source = load_source_registration(collection_id, &source_name);
    let mut current = hydrate_source_keys_locked(&source, std::slice::from_ref(&source_key));
    if current.len() != 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document source row is no longer visible",
        );
    }
    let current = current.pop().unwrap_or_else(|| invalid_job_identity());
    let current_digest: Vec<u8> = Sha256::digest(current.text.as_bytes()).to_vec();
    let target = Spi::get_two_with_args::<bool, Vec<u8>>(
        "SELECT pg_catalog.count(*) = 1,
                (pg_catalog.array_agg(projection_sha256 ORDER BY generation_id))[1]
           FROM pgcontext._visible_document_chunk_generations
          WHERE generation_id = $1 AND document_source_id = $2 AND source_key = $3
            AND source_version = $4 AND source_sha256 = $5
            AND source_registration_revision = $6 AND published_at IS NOT NULL",
        &[
            generation_id.into(),
            source.document_source_id.into(),
            source_key.as_str().into(),
            current.source_version.into(),
            current_digest.clone().into(),
            source.registration_revision.into(),
        ],
    )
    .ok()
    .and_then(|(matches, digest)| matches.zip(digest));
    let Some((target_is_current, projection_digest)) = target else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk rollback generation has no projection digest",
        );
    };
    if !target_is_current {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk rollback generation is not current with the source row",
        );
    }
    let locked = hydrate_source_keys_locked(&source, std::slice::from_ref(&source_key));
    if locked.len() != 1
        || locked[0].source_version != current.source_version
        || Sha256::digest(locked[0].text.as_bytes()).as_slice() != current_digest
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document source changed during chunk rollback",
        );
    }
    lock_projection_writes(&source.projection_schema, &source.projection_table);
    require_projection_generation_digest(
        &source.projection_schema,
        &source.projection_table,
        generation_id,
        &projection_digest,
    );
    arm_document_chunk_permit(
        DocumentChunkPermitKind::Rollback,
        source.document_source_id,
        generation_id,
    );
    Spi::get_one_with_args::<i64>(
        "SELECT pgcontext._rollback_document_chunk_generation($1,$2,$3)",
        &[
            source.document_source_id.into(),
            source_key.into(),
            generation_id.into(),
        ],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "failed to roll back the document chunk generation",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "document chunk rollback returned no generation identity",
        )
    })
}

#[derive(Clone, Debug)]
struct SourceRegistration {
    document_source_id: i64,
    registration_revision: i64,
    chunking_profile_id: i64,
    source_table_oid: pg_sys::Oid,
    source_schema: String,
    source_table: String,
    source_key_attnum: i16,
    source_key_type_oid: pg_sys::Oid,
    source_key_collation_oid: pg_sys::Oid,
    source_key_type_schema: String,
    source_key_type_name: String,
    text_column: String,
    text_attnum: i16,
    text_type_oid: pg_sys::Oid,
    text_collation_oid: pg_sys::Oid,
    version_column: String,
    version_attnum: i16,
    version_type_oid: pg_sys::Oid,
    projection_table_oid: pg_sys::Oid,
    projection_schema: String,
    projection_table: String,
    projection_attnums: Vec<i16>,
    projection_type_oids: Vec<pg_sys::Oid>,
    projection_collation_oids: Vec<pg_sys::Oid>,
    max_document_bytes: i64,
}

#[derive(Clone, Debug)]
struct SourceRow {
    source_key: String,
    text: String,
    source_version: i64,
}

#[derive(Clone, Debug)]
struct StagedResponse {
    response: Value,
    response_sha256: Vec<u8>,
    chunk_count: i32,
    token_count: i64,
    staging_bytes: i64,
}

type ProjectionRow = (
    String,
    i64,
    i64,
    i64,
    i64,
    i32,
    String,
    String,
    i64,
    i64,
    i64,
    i64,
    i32,
    String,
    Vec<String>,
    Option<i32>,
    Option<JsonB>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<String>,
    JsonB,
);
