//! Projection-row decoding and deterministic output metadata helpers.

use super::*;

pub(in crate::document_chunking) fn projection_row(
    row: spi::SpiHeapTupleData<'_>,
) -> ProjectionRow {
    (
        required(&row, 1, "source key"),
        required(&row, 2, "source version"),
        required(&row, 3, "profile revision"),
        required(&row, 4, "generation identity"),
        required(&row, 5, "occurrence identity"),
        required(&row, 6, "chunk ordinal"),
        required(&row, 7, "original text"),
        required(&row, 8, "retrieval text"),
        required(&row, 9, "start byte"),
        required(&row, 10, "end byte"),
        required(&row, 11, "start character"),
        required(&row, 12, "end character"),
        required(&row, 13, "token count"),
        required(&row, 14, "structure kind"),
        required(&row, 15, "structure path"),
        optional(&row, 16),
        optional(&row, 17),
        optional(&row, 18),
        optional(&row, 19),
        optional(&row, 20),
        required(&row, 21, "content hash"),
        optional(&row, 22),
        required(&row, 23, "fake embedding"),
    )
}

pub(in crate::document_chunking) fn context_prefix(
    enabled: bool,
    path: &[String],
) -> Option<String> {
    if !enabled || path.is_empty() {
        return None;
    }
    context_build::bounded_unicode_word_context_prefix(path)
}

pub(in crate::document_chunking) fn structure_kind(
    kind: context_build::StructureKind,
) -> &'static str {
    match kind {
        context_build::StructureKind::Document => "document",
        context_build::StructureKind::Heading => "heading",
        context_build::StructureKind::Paragraph => "paragraph",
        context_build::StructureKind::List => "list",
        context_build::StructureKind::Table => "table",
        context_build::StructureKind::Code => "code",
        context_build::StructureKind::Sentence => "sentence",
        context_build::StructureKind::TokenWindow => "token_window",
    }
}

pub(in crate::document_chunking) fn stable_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf2_9ce4_8422_2325, |mut hash, byte| {
        hash ^= u64::from(byte);
        hash.wrapping_mul(0x0000_0100_0000_01b3)
    }) & i64::MAX as u64
}

pub(in crate::document_chunking) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

pub(in crate::document_chunking) fn invalid_job_identity() -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
        "document chunk job identity is invalid",
    )
}

pub(in crate::document_chunking) fn projection_generation_digest(
    projection_schema: &str,
    projection_table: &str,
    generation_id: i64,
) -> Vec<u8> {
    let projection = quote_qualified(projection_schema, projection_table);
    let size_preflight_sql = format!(
        "SELECT pg_catalog.count(*) <= 16384
                AND COALESCE(pg_catalog.sum(
                    pg_catalog.octet_length(source_key)
                  + pg_catalog.octet_length(source_sha256)
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
                ), 0) <= $2
           FROM {projection} WHERE generation_id = $1"
    );
    let bounded = Spi::get_one_with_args::<bool>(
        &size_preflight_sql,
        &[
            generation_id.into(),
            i64::try_from(MAX_STAGING_BYTES)
                .unwrap_or_else(|_| invalid_job_identity())
                .into(),
        ],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !bounded {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "document chunk projection exceeds the bounded digest contract",
        );
    }
    let shape_preflight_sql = format!(
        "SELECT COALESCE(pg_catalog.bool_and(
                    page_number IS NULL
                    AND region IS NULL
                    AND fake_embedding = pg_catalog.jsonb_build_array(
                        ((content_hash & 65535)::double precision / 65535.0),
                        (((content_hash >> 16) & 65535)::double precision / 65535.0),
                        (((content_hash >> 32) & 65535)::double precision / 65535.0)
                    )
                    AND pg_catalog.jsonb_typeof(provenance) = 'object'
                    AND pg_catalog.octet_length(provenance->>'parser') <= 64
                    AND provenance = pg_catalog.jsonb_build_object(
                        'parser', provenance->>'parser',
                        'tokenizer', 'unicode_words_v1',
                        'profile_revision', profile_revision,
                        'source_version', source_version,
                        'complete', true,
                        'embedding', 'deterministic_fake_v1'
                    )
                ), true)
           FROM {projection} WHERE generation_id = $1"
    );
    let canonical_shape =
        Spi::get_one_with_args::<bool>(&shape_preflight_sql, &[generation_id.into()])
            .ok()
            .flatten()
            .unwrap_or(false);
    if !canonical_shape {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk projection contains noncanonical derived values",
        );
    }
    let sql = format!(
        "SELECT pg_catalog.sha256(COALESCE(
             pg_catalog.string_agg(
                 pg_catalog.sha256(pg_catalog.convert_to(pg_catalog.to_jsonb(row_value)::text, 'UTF8')),
                 ''::bytea ORDER BY row_value.ordinal
             ), ''::bytea
         ))
           FROM (SELECT * FROM {projection} WHERE generation_id = $1) AS row_value"
    );
    Spi::get_one_with_args::<Vec<u8>>(&sql, &[generation_id.into()])
        .ok()
        .flatten()
        .filter(|digest| digest.len() == 32)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "document chunk projection digest is unavailable",
            )
        })
}

pub(in crate::document_chunking) fn lock_projection_writes(
    projection_schema: &str,
    projection_table: &str,
) {
    let projection = quote_qualified(projection_schema, projection_table);
    Spi::run(&format!("LOCK TABLE {projection} IN SHARE MODE")).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk projection could not be locked for alias publication",
        )
    });
}

pub(in crate::document_chunking) fn serialize_projection_publication(
    projection_schema: &str,
    projection_table: &str,
) {
    let projection = quote_qualified(projection_schema, projection_table);
    Spi::run(&format!(
        "LOCK TABLE {projection} IN SHARE ROW EXCLUSIVE MODE"
    ))
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk projection could not be locked for publication",
        )
    });
}

pub(in crate::document_chunking) fn require_projection_generation_digest(
    projection_schema: &str,
    projection_table: &str,
    generation_id: i64,
    expected: &[u8],
) {
    if projection_generation_digest(projection_schema, projection_table, generation_id) != expected
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk projection changed after publication",
        );
    }
}
