//! Bounded PostgreSQL datum admission for chunk-worker responses and key arrays.

#![allow(
    unsafe_code,
    reason = "the PostgreSQL ABI requires an unsafe datum unboxing boundary"
)]

use super::*;
use pgrx::datum::AnyElement;

/// Returns a varlena's logical raw size without detoasting it into Rust.
#[pg_extern(name = "_document_chunk_raw_datum_bytes")]
#[search_path(pg_catalog, pgcontext)]
fn document_chunk_raw_datum_bytes(value: AnyElement) -> i64 {
    if value.oid() != pg_sys::JSONBOID {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
            "document chunk raw datum admission requires jsonb",
        );
    }
    // SAFETY: the OID check above proves this live non-null datum uses the
    // PostgreSQL jsonb varlena representation. `toast_raw_datum_size` reads
    // only its varlena metadata/TOAST size and does not detoast the value.
    let bytes = unsafe { pg_sys::toast_raw_datum_size(value.datum()) };
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

/// A chunk-worker response decoded only after raw bytes and JSON nodes pass.
#[derive(Debug)]
pub struct BoundedChunkResponse(pub(super) JsonB);

impl FromDatum for BoundedChunkResponse {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _typoid: pg_sys::Oid,
    ) -> Option<Self> {
        crate::semantic_rerank::decode_bounded_jsonb(
            datum,
            is_null,
            MAX_STAGING_BYTES,
            MAX_STAGING_JSON_NODES,
            MAX_STAGING_JSON_DEPTH,
            "document chunk response exceeds the JSON allocation budget",
        )
        .map(Self)
    }
}

impl IntoDatum for BoundedChunkResponse {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        self.0.into_datum()
    }

    fn type_oid() -> pg_sys::Oid {
        JsonB::type_oid()
    }
}

// SAFETY: this wrapper declares SQL jsonb and performs bounded decoding before
// constructing a serde_json value.
unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for BoundedChunkResponse {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        // SAFETY: ArgAbi guarantees the declared jsonb SQL type.
        unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                "document chunk response is null",
            )
        })
    }
}

impl_sql_translatable!(BoundedChunkResponse, "jsonb");

const MAX_SOURCE_KEYS_RAW_BYTES: usize =
    MAX_SOURCE_KEYS * (MAX_SOURCE_KEY_BYTES + pg_sys::VARHDRSZ + 8);

/// A source-key array whose raw datum is bounded before any Rust strings exist.
#[derive(Debug)]
pub struct BoundedSourceKeys(Vec<String>);

impl BoundedSourceKeys {
    pub(super) fn into_vec(mut self) -> Vec<String> {
        // Every row-locking consumer visits keys in one canonical order. This
        // avoids reversed caller arrays forming source-row lock cycles while
        // retaining the one-row-at-a-time aggregate allocation bound.
        self.0.sort_unstable();
        self.0
    }
}

impl FromDatum for BoundedSourceKeys {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _typoid: pg_sys::Oid,
    ) -> Option<Self> {
        if is_null {
            return None;
        }
        // SAFETY: PostgreSQL supplied a live array datum. Its raw TOAST size is
        // inspected before pgrx detoasts the array or converts any element.
        let raw_bytes = unsafe { pg_sys::toast_raw_datum_size(datum) };
        if raw_bytes > MAX_SOURCE_KEYS_RAW_BYTES {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "document chunk source key array exceeds the allocation budget",
            );
        }
        // SAFETY: the OID check proves this is text[]. Keep elements as raw
        // datums until each individual TOAST size passes its own byte bound.
        let array = unsafe {
            Array::<pg_sys::Datum>::from_polymorphic_datum(datum, false, pg_sys::TEXTARRAYOID)
        }
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "document chunk source key array is invalid",
            )
        });
        let mut keys = Vec::new();
        for (position, key_datum) in array.iter().enumerate() {
            if position >= MAX_SOURCE_KEYS {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "document chunk source key count is outside 1..=256",
                );
            }
            let key_datum = key_datum.unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "document chunk source keys cannot contain nulls",
                )
            });
            // SAFETY: this datum is a non-null text[] element. PostgreSQL's raw
            // size includes the uncompressed logical varlena length.
            let key_bytes = unsafe { pg_sys::toast_raw_datum_size(key_datum) };
            if key_bytes > MAX_SOURCE_KEY_BYTES.saturating_add(pg_sys::VARHDRSZ) {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    "document chunk source keys are invalid",
                );
            }
            // SAFETY: the parent array OID proves each element is SQL text and
            // the size bound was enforced before constructing the Rust String.
            let key = unsafe { String::from_polymorphic_datum(key_datum, false, pg_sys::TEXTOID) }
                .unwrap_or_else(|| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                        "document chunk source keys are invalid",
                    )
                });
            keys.push(key);
        }
        validate_source_keys(&keys);
        Some(Self(keys))
    }
}

impl IntoDatum for BoundedSourceKeys {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        self.0.into_datum()
    }

    fn type_oid() -> pg_sys::Oid {
        pg_sys::TEXTARRAYOID
    }
}

// SAFETY: this wrapper declares SQL text[] and bounds the raw array before
// delegating lazy element conversion to pgrx.
unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for BoundedSourceKeys {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        // SAFETY: ArgAbi guarantees the declared text[] SQL type.
        unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                "document chunk source key array is null",
            )
        })
    }
}

impl_sql_translatable!(BoundedSourceKeys, "text[]");
