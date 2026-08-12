//! Registration, identifier, and projection contract validation.

use super::*;

pub(super) fn validate_bounded_name(value: &str, maximum: usize, label: &'static str) {
    if value.is_empty()
        || value.len() > maximum
        || value.trim().is_empty()
        || value.chars().any(char::is_control)
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} name is invalid"),
        );
    }
}

pub(super) fn validate_identifier(value: &str, label: &'static str) {
    if value.is_empty()
        || value.len() > 63
        || value.as_bytes().contains(&0)
        || !value
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} is invalid"),
        );
    }
}

pub(super) fn qualified_name(value: &str) -> (String, String) {
    if value.len() > 127 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "qualified table name is too long",
        );
    }
    let Some((schema, table)) = value.split_once('.') else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "table name must use schema.table form",
        );
    };
    if table.contains('.') {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "table name must use schema.table form",
        );
    }
    validate_identifier(schema, "table schema");
    validate_identifier(table, "table name");
    (schema.to_owned(), table.to_owned())
}

pub(super) fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(super) fn quote_qualified(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_identifier(schema), quote_identifier(table))
}

pub(super) fn parse_parser(value: &str) -> DocumentParser {
    match value {
        "plain_text_v1" => DocumentParser::PlainTextV1,
        "markdown_v1" => DocumentParser::MarkdownV1,
        "html_v1" => DocumentParser::HtmlV1,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "document parser is not supported",
        ),
    }
}

pub(super) fn token_profile(
    target_tokens: i32,
    max_tokens: i32,
    min_tokens: i32,
    overlap_tokens: i32,
    max_document_bytes: i64,
) -> TokenChunkProfile {
    let values = [target_tokens, max_tokens, min_tokens, overlap_tokens];
    let [target_tokens, max_tokens, min_tokens, overlap_tokens] = values.map(|value| {
        usize::try_from(value).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "chunking profile token bounds must be nonnegative",
            )
        })
    });
    let max_document_bytes = usize::try_from(max_document_bytes).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "chunking profile document bytes must be positive",
        )
    });
    TokenChunkProfile::new(
        target_tokens,
        max_tokens,
        min_tokens,
        overlap_tokens,
        max_document_bytes,
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "chunking profile bounds are invalid",
        )
    })
}

pub(super) fn require_collection_owner(collection: &str) -> i64 {
    validate_bounded_name(collection, 128, "collection");
    let collection_id = Spi::get_one_with_args::<i64>(
        "SELECT collection_id FROM pgcontext._visible_collections WHERE collection_name = $1",
        &[collection.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to resolve document collection",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
            "document collection does not exist",
        )
    });
    Spi::run_with_args(
        "SELECT pgcontext._require_collection_owner($1)",
        &[collection_id.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "document collection owner privilege is required",
        )
    });
    collection_id
}

pub(super) fn require_collection_owner_id(collection_id: i64) {
    Spi::run_with_args(
        "SELECT pgcontext._require_collection_owner($1)",
        &[collection_id.into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "document collection owner privilege is required",
        )
    });
}

pub(super) fn resolve_projection(value: &str) -> (pg_sys::Oid, String, String) {
    let (schema, table) = qualified_name(value);
    let qualified = quote_qualified(&schema, &table);
    let oid = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT class.oid
           FROM pg_catalog.pg_class AS class
          WHERE class.oid = pg_catalog.to_regclass($1)
            AND class.relkind IN ('r','p')",
        &[qualified.as_str().into()],
    )
    .unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "failed to resolve document chunk projection",
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
            "document chunk projection does not exist",
        )
    });
    let privileges = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege(SESSION_USER,$1,'SELECT,INSERT,UPDATE,DELETE')",
        &[oid.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !privileges {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "document chunk projection requires SELECT, INSERT, UPDATE, and DELETE",
        );
    }
    (oid, schema, table)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProjectionContractIdentity {
    pub(super) attnums: Vec<i16>,
    pub(super) type_oids: Vec<pg_sys::Oid>,
    pub(super) collation_oids: Vec<pg_sys::Oid>,
}

pub(super) fn resolve_projection_contract(oid: pg_sys::Oid) -> ProjectionContractIdentity {
    let required = [
        ("document_source_id", pg_sys::INT8OID),
        ("source_key", pg_sys::TEXTOID),
        ("source_version", pg_sys::INT8OID),
        ("source_sha256", pg_sys::BYTEAOID),
        ("profile_revision", pg_sys::INT8OID),
        ("generation_id", pg_sys::INT8OID),
        ("occurrence_id", pg_sys::INT8OID),
        ("ordinal", pg_sys::INT4OID),
        ("original_text", pg_sys::TEXTOID),
        ("retrieval_text", pg_sys::TEXTOID),
        ("start_byte", pg_sys::INT8OID),
        ("end_byte", pg_sys::INT8OID),
        ("start_char", pg_sys::INT8OID),
        ("end_char", pg_sys::INT8OID),
        ("token_count", pg_sys::INT4OID),
        ("structure_kind", pg_sys::TEXTOID),
        ("structure_path", pg_sys::TEXTARRAYOID),
        ("page_number", pg_sys::INT4OID),
        ("region", pg_sys::JSONBOID),
        ("parent_occurrence_id", pg_sys::INT8OID),
        ("previous_occurrence_id", pg_sys::INT8OID),
        ("next_occurrence_id", pg_sys::INT8OID),
        ("content_hash", pg_sys::INT8OID),
        ("context_prefix", pg_sys::TEXTOID),
        ("context_prefix_hash", pg_sys::INT8OID),
        ("fake_embedding", pg_sys::JSONBOID),
        ("ready", pg_sys::BOOLOID),
        ("provenance", pg_sys::JSONBOID),
    ];
    let live_column_count = Spi::get_one_with_args::<i64>(
        "SELECT pg_catalog.count(*)
           FROM pg_catalog.pg_attribute AS attribute
          WHERE attribute.attrelid = $1
            AND attribute.attnum > 0 AND NOT attribute.attisdropped",
        &[oid.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(-1);
    if live_column_count != i64::try_from(required.len()).unwrap_or(-1) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            "document chunk projection must contain exactly the required columns",
        );
    }
    let mut identity = ProjectionContractIdentity {
        attnums: Vec::with_capacity(required.len()),
        type_oids: Vec::with_capacity(required.len()),
        collation_oids: Vec::with_capacity(required.len()),
    };
    for (name, expected_type_oid) in required {
        let observed = Spi::get_three_with_args::<i16, pg_sys::Oid, pg_sys::Oid>(
            "SELECT attribute.attnum, attribute.atttypid, attribute.attcollation
               FROM pg_catalog.pg_attribute AS attribute
              WHERE attribute.attrelid = $1 AND attribute.attname = $2
                AND attribute.attnum > 0 AND NOT attribute.attisdropped",
            &[oid.into(), name.into()],
        )
        .ok()
        .and_then(|(attnum, type_oid, collation_oid)| attnum.zip(type_oid).zip(collation_oid));
        let Some(((attnum, type_oid), collation_oid)) = observed else {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                "document chunk projection does not match the required column contract",
            );
        };
        if type_oid != expected_type_oid {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                "document chunk projection does not match the required column contract",
            );
        }
        identity.attnums.push(attnum);
        identity.type_oids.push(type_oid);
        identity.collation_oids.push(collation_oid);
    }
    let required_unique_indexes = Spi::get_one_with_args::<bool>(
        "WITH unique_indexes AS (
             SELECT pg_catalog.array_agg(key_attribute.attname ORDER BY key.ordinality) AS columns
               FROM pg_catalog.pg_index AS indexes
               CROSS JOIN LATERAL pg_catalog.unnest(indexes.indkey::int2[])
                   WITH ORDINALITY AS key(attnum, ordinality)
               JOIN pg_catalog.pg_attribute AS key_attribute
                 ON key_attribute.attrelid = indexes.indrelid
                AND key_attribute.attnum = key.attnum
              WHERE indexes.indrelid = $1 AND indexes.indisunique AND indexes.indislive
                AND indexes.indisvalid AND indexes.indisready AND indexes.indimmediate
                AND indexes.indpred IS NULL AND indexes.indexprs IS NULL
                AND indexes.indnkeyatts = 2 AND indexes.indnatts = 2
              GROUP BY indexes.indexrelid
         )
         SELECT EXISTS (
                    SELECT 1 FROM unique_indexes
                     WHERE columns = ARRAY['generation_id','occurrence_id']::name[]
                ) AND EXISTS (
                    SELECT 1 FROM unique_indexes
                     WHERE columns = ARRAY['generation_id','ordinal']::name[]
                )",
        &[oid.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !required_unique_indexes {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk projection is missing required unique indexes",
        );
    }
    identity
}

pub(super) fn validate_source_keys(keys: &[String]) {
    if keys.is_empty() || keys.len() > MAX_SOURCE_KEYS {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "document chunk source key count is outside 1..=256",
        );
    }
    let mut unique = BTreeSet::new();
    for key in keys {
        if key.is_empty()
            || key.len() > MAX_SOURCE_KEY_BYTES
            || key.as_bytes().contains(&0)
            || !unique.insert(key.as_str())
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "document chunk source keys are invalid",
            );
        }
    }
}
