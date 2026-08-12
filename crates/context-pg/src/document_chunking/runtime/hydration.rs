//! Bounded source-row preflight, locking, and hydration.

use super::*;

pub(in crate::document_chunking) fn hydrate_source_keys(
    source: &SourceRegistration,
    keys: &[String],
) -> Vec<SourceRow> {
    hydrate_source_keys_with_lock(source, keys, false)
}

pub(in crate::document_chunking) fn hydrate_source_keys_locked(
    source: &SourceRegistration,
    keys: &[String],
) -> Vec<SourceRow> {
    hydrate_source_keys_with_lock(source, keys, true)
}

/// Locks a bounded key set in canonical source-key order without detoasting text.
pub(in crate::document_chunking) fn lock_source_keys_bounded(
    source: &SourceRegistration,
    keys: &[String],
) -> usize {
    require_source_select_and_contract(source);
    let table = quote_qualified(&source.source_schema, &source.source_table);
    let key_type = quote_qualified(&source.source_key_type_schema, &source.source_key_type_name);
    let text = quote_identifier(&source.text_column);
    let version = quote_identifier(&source.version_column);
    let sql = format!(
        "SELECT source.id::text, source.{version},
                pg_catalog.octet_length(source.{text})::bigint
           FROM {table} AS source
           JOIN pg_catalog.unnest($1::text[]) AS requested(source_key)
             ON source.id = requested.source_key::text::{key_type}
          ORDER BY source.id::text"
    );
    let identities = Spi::connect(|client| {
        client
            .select(
                &sql,
                Some(i64::try_from(keys.len()).unwrap_or(i64::MAX)),
                &[keys.to_vec().into()],
            )
            .unwrap_or_else(|_| hydration_error("failed to lock authorized document rows"))
            .map(|row| {
                let source_key = required(&row, 1, "source key");
                let source_version = required(&row, 2, "source version");
                let source_bytes = required(&row, 3, "source text bytes");
                admit_text_bytes(source_bytes, source);
                (source_key, source_version, source_bytes)
            })
            .collect::<Vec<(String, i64, i64)>>()
    });
    for (source_key, source_version, source_bytes) in &identities {
        let _ = lock_source_identity(source, source_key, *source_version, *source_bytes);
    }
    identities.len()
}

fn hydrate_source_keys_with_lock(
    source: &SourceRegistration,
    keys: &[String],
    lock_rows: bool,
) -> Vec<SourceRow> {
    require_source_select_and_contract(source);
    let table = quote_qualified(&source.source_schema, &source.source_table);
    let key_type = quote_qualified(&source.source_key_type_schema, &source.source_key_type_name);
    let text = quote_identifier(&source.text_column);
    let version = quote_identifier(&source.version_column);
    let preflight_sql = format!(
        "SELECT source.id::text, source.{version},
                pg_catalog.octet_length(source.{text})::bigint
           FROM {table} AS source
           JOIN pg_catalog.unnest($1::text[]) WITH ORDINALITY AS requested(source_key, ordinal)
             ON source.id = requested.source_key::text::{key_type}
          ORDER BY requested.ordinal"
    );
    let expected = Spi::connect(|client| {
        client
            .select(
                &preflight_sql,
                Some(i64::try_from(keys.len()).unwrap_or(i64::MAX)),
                &[keys.to_vec().into()],
            )
            .unwrap_or_else(|_| hydration_error("failed to preflight authorized document rows"))
            .map(|row| {
                let source_key = required(&row, 1, "source key");
                let source_version = required(&row, 2, "source version");
                let source_bytes = required(&row, 3, "source text bytes");
                admit_text_bytes(source_bytes, source);
                (source_key, source_version, source_bytes)
            })
            .collect::<Vec<(String, i64, i64)>>()
    });
    let locked_digests = lock_rows.then(|| {
        expected
            .iter()
            .map(|(source_key, source_version, source_bytes)| {
                lock_source_identity(source, source_key, *source_version, *source_bytes)
            })
            .collect::<Vec<_>>()
    });
    let sql = format!(
        "SELECT source.id::text, source.{text}, source.{version}
           FROM {table} AS source
           JOIN pg_catalog.unnest($1::text[]) WITH ORDINALITY AS requested(source_key, ordinal)
             ON source.id = requested.source_key::text::{key_type}
          ORDER BY requested.ordinal"
    );
    let rows = Spi::connect(|client| {
        client
            .select(
                &sql,
                Some(i64::try_from(keys.len()).unwrap_or(i64::MAX)),
                &[keys.to_vec().into()],
            )
            .unwrap_or_else(|_| hydration_error("failed to hydrate authorized document rows"))
            .map(|row| SourceRow {
                source_key: required(&row, 1, "source key"),
                text: required(&row, 2, "source text"),
                source_version: required(&row, 3, "source version"),
            })
            .collect::<Vec<_>>()
    });
    if rows.len() != expected.len()
        || rows.iter().enumerate().any(|(index, row)| {
            let expected = &expected[index];
            row.source_key != expected.0
                || row.source_version != expected.1
                || locked_digests.as_ref().is_some_and(|digests| {
                    Sha256::digest(row.text.as_bytes()).as_slice() != digests[index]
                })
        })
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "document source changed during hydration",
        );
    }
    rows
}

fn lock_source_identity(
    source: &SourceRegistration,
    source_key: &str,
    source_version: i64,
    source_bytes: i64,
) -> Vec<u8> {
    arm_document_chunk_permit(
        DocumentChunkPermitKind::LockSource,
        source.document_source_id,
        source_version,
    );
    let locked_sha256 = Spi::get_one_with_args::<Vec<u8>>(
        "SELECT pgcontext._lock_document_chunk_source($1,$2,$3,$4,$5)",
        &[
            source.document_source_id.into(),
            source_key.into(),
            source_version.into(),
            source_bytes.into(),
            source.max_document_bytes.into(),
        ],
    )
    .ok()
    .flatten()
    .unwrap_or_default();
    if locked_sha256.len() != 32 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "document source changed while acquiring its publication lock",
        );
    }
    locked_sha256
}

fn require_source_select_and_contract(source: &SourceRegistration) {
    if !source_select_allowed(source.source_table_oid) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "document source SELECT privilege is required",
        );
    }
    let table = quote_qualified(&source.source_schema, &source.source_table);
    validate_current_relation(source.source_table_oid, &table);
    validate_source_contract(source);
}

fn admit_text_bytes(text_bytes: i64, source: &SourceRegistration) {
    if text_bytes < 0 || text_bytes > source.max_document_bytes {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "document source text exceeds the registered profile byte limit",
        );
    }
}

fn hydration_error(message: &'static str) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
        message,
    )
}
