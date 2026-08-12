//! Small SPI decoding and source-identity helpers shared by the runtime paths.

use super::*;

pub(in crate::document_chunking) fn required<T: FromDatum + IntoDatum>(
    row: &spi::SpiHeapTupleData<'_>,
    ordinal: usize,
    label: &'static str,
) -> T {
    row.get::<T>(ordinal).ok().flatten().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("document chunk catalog is missing {label}"),
        )
    })
}

pub(in crate::document_chunking) fn optional<T: FromDatum + IntoDatum>(
    row: &spi::SpiHeapTupleData<'_>,
    ordinal: usize,
) -> Option<T> {
    row.get::<T>(ordinal).ok().flatten()
}

pub(in crate::document_chunking) fn validate_current_relation(
    expected_oid: pg_sys::Oid,
    qualified: &str,
) {
    Spi::run(&format!("LOCK TABLE {qualified} IN ACCESS SHARE MODE")).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk relation could not be locked",
        )
    });
    let current = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT class.oid
           FROM pg_catalog.pg_class AS class
          WHERE class.oid = pg_catalog.to_regclass($1)
            AND class.relkind IN ('r','p')",
        &[qualified.into()],
    )
    .ok()
    .flatten();
    if current != Some(expected_oid) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk relation identity changed",
        );
    }
}

pub(in crate::document_chunking) fn validate_source_contract(source: &SourceRegistration) {
    let valid = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.count(*) = 3
           FROM pg_catalog.pg_attribute AS attributes
          WHERE attributes.attrelid = $1::bigint::oid
            AND NOT attributes.attisdropped
            AND (
                (attributes.attnum = $2 AND attributes.attname = 'id'
                 AND attributes.atttypid = $3::bigint::oid
                 AND attributes.attcollation = $4::bigint::oid)
                OR (attributes.attnum = $5 AND attributes.attname = $6
                    AND attributes.atttypid = $7::bigint::oid
                    AND attributes.attcollation = $8::bigint::oid)
                OR (attributes.attnum = $9 AND attributes.attname = $10
                    AND attributes.atttypid = $11::bigint::oid)
            )",
        &[
            i64::from(source.source_table_oid.to_u32()).into(),
            source.source_key_attnum.into(),
            i64::from(source.source_key_type_oid.to_u32()).into(),
            i64::from(source.source_key_collation_oid.to_u32()).into(),
            source.text_attnum.into(),
            source.text_column.as_str().into(),
            i64::from(source.text_type_oid.to_u32()).into(),
            i64::from(source.text_collation_oid.to_u32()).into(),
            source.version_attnum.into(),
            source.version_column.as_str().into(),
            i64::from(source.version_type_oid.to_u32()).into(),
        ],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !valid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "document chunk source binding changed",
        );
    }
}

pub(in crate::document_chunking) fn source_key_visible(
    document_source_id: i64,
    source_key: &str,
) -> bool {
    let identity = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT rerank.source_table_oid, rerank.source_schema_name,
                        rerank.source_table_name, rerank.source_key_attnum,
                        rerank.source_key_type_oid, rerank.source_key_type_schema,
                        rerank.source_key_type_name, rerank.source_key_collation_oid
                   FROM pgcontext._visible_document_sources AS sources
                   JOIN pgcontext._visible_semantic_rerank_sources AS rerank
                     USING (rerank_source_id)
                  WHERE sources.document_source_id = $1
                    AND sources.status = 'ready' AND rerank.status = 'ready'",
                Some(1),
                &[document_source_id.into()],
            )
            .ok()?;
        let row = rows.into_iter().next()?;
        Some((
            required::<pg_sys::Oid>(&row, 1, "source table identity"),
            required::<String>(&row, 2, "source schema"),
            required::<String>(&row, 3, "source table"),
            required::<i16>(&row, 4, "source key attribute"),
            required::<pg_sys::Oid>(&row, 5, "source key type"),
            required::<String>(&row, 6, "source key type schema"),
            required::<String>(&row, 7, "source key type name"),
            required::<pg_sys::Oid>(&row, 8, "source key collation"),
        ))
    });
    let Some((
        table_oid,
        schema,
        table_name,
        key_attnum,
        key_type_oid,
        key_type_schema,
        key_type_name,
        key_collation_oid,
    )) = identity
    else {
        return false;
    };
    if !source_select_allowed(table_oid) {
        return false;
    }
    let table = quote_qualified(&schema, &table_name);
    validate_current_relation(table_oid, &table);
    let binding_valid = Spi::get_one_with_args::<bool>(
        "SELECT EXISTS (
             SELECT 1 FROM pg_catalog.pg_attribute AS attribute
              WHERE attribute.attrelid = $1::bigint::oid AND attribute.attnum = $2
                AND attribute.atttypid = $3::bigint::oid
                AND attribute.attcollation = $4::bigint::oid
                AND attribute.attname = 'id' AND NOT attribute.attisdropped
         )",
        &[
            i64::from(table_oid.to_u32()).into(),
            key_attnum.into(),
            i64::from(key_type_oid.to_u32()).into(),
            i64::from(key_collation_oid.to_u32()).into(),
        ],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !binding_valid {
        return false;
    }
    let key_type = quote_qualified(&key_type_schema, &key_type_name);
    let sql = format!(
        "SELECT pg_catalog.count(*) = 1 FROM {table} AS source
          WHERE source.id = $1::text::{key_type}"
    );
    Spi::get_one_with_args::<bool>(&sql, &[source_key.into()])
        .ok()
        .flatten()
        .unwrap_or(false)
}

pub(in crate::document_chunking) fn claim_job_identities(
    limit: i32,
    lease_millis: i32,
    worker_id: &str,
) -> Vec<(i64, i64)> {
    validate_claim_bounds(limit, lease_millis, worker_id);
    arm_document_chunk_permit(
        DocumentChunkPermitKind::Claim,
        i64::from(limit),
        i64::from(lease_millis),
    );
    Spi::connect(|client| {
        client
            .select(
                "SELECT job_id, lease_token
                   FROM pgcontext._claim_document_chunk_jobs($1,$2,$3)",
                Some(i64::from(limit)),
                &[limit.into(), lease_millis.into(), worker_id.into()],
            )
            .unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    "failed to claim document chunk jobs",
                )
            })
            .map(|row| {
                (
                    required(&row, 1, "job identity"),
                    required(&row, 2, "lease token"),
                )
            })
            .collect()
    })
}

pub(in crate::document_chunking) fn validate_claim_bounds(
    limit: i32,
    lease_millis: i32,
    worker_id: &str,
) {
    if !(1..=256).contains(&limit)
        || !(1..=60_000).contains(&lease_millis)
        || worker_id.is_empty()
        || worker_id.len() > 128
        || worker_id.chars().any(char::is_control)
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "invalid document chunk claim bounds",
        );
    }
}

pub(in crate::document_chunking) fn release_document_chunk_claim(job_id: i64, lease_token: i64) {
    arm_document_chunk_permit(DocumentChunkPermitKind::ReleaseClaim, job_id, lease_token);
    let released = Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._release_document_chunk_claim($1,$2)",
        &[job_id.into(), lease_token.into()],
    )
    .ok()
    .flatten()
    .unwrap_or(false);
    if !released {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "failed to release unauthorized document chunk claim",
        );
    }
}
