//! Audited compatibility boundary for pgvector's packed `sparsevec` varlena.

use core::{ptr, slice};

use context_core::{SparseEntry, SparseVector};
use pgrx::datum::{FromDatum, IntoDatum};
use pgrx::prelude::*;

use super::SparseVec;
use crate::error::{raise_core_error, raise_sql_error};

const PGVECTOR_SPARSE_HEADER_BYTES: usize = 3 * size_of::<i32>();
const PGVECTOR_SPARSE_ENTRY_BYTES: usize = size_of::<i32>() + size_of::<f32>();

static PGVECTOR_SPARSE_FINFO: pg_sys::Pg_finfo_record =
    pg_sys::Pg_finfo_record { api_version: 1 };

fn read_i32(payload: &[u8], offset: usize) -> i32 {
    let mut bytes = [0_u8; size_of::<i32>()];
    bytes.copy_from_slice(&payload[offset..offset + size_of::<i32>()]);
    i32::from_ne_bytes(bytes)
}

fn read_f32(payload: &[u8], offset: usize) -> f32 {
    let mut bytes = [0_u8; size_of::<f32>()];
    bytes.copy_from_slice(&payload[offset..offset + size_of::<f32>()]);
    f32::from_ne_bytes(bytes)
}

/// Decodes pgvector's payload (the bytes after PostgreSQL's varlena header).
pub(crate) fn decode_pgvector_sparsevec_payload(
    payload: &[u8],
) -> Result<SparseVector, context_core::Error> {
    if payload.len() < PGVECTOR_SPARSE_HEADER_BYTES {
        return Err(context_core::Error::InvalidVector(
            "pgvector sparsevec payload is truncated".to_owned(),
        ));
    }
    let dimensions = read_i32(payload, 0);
    let non_zero_count = read_i32(payload, size_of::<i32>());
    let reserved = read_i32(payload, 2 * size_of::<i32>());
    if dimensions <= 0 {
        return Err(context_core::Error::InvalidVector(format!(
            "invalid pgvector sparsevec dimensions: {dimensions}"
        )));
    }
    if non_zero_count < 0 {
        return Err(context_core::Error::InvalidVector(format!(
            "invalid pgvector sparsevec nonzero count: {non_zero_count}"
        )));
    }
    if reserved != 0 {
        return Err(context_core::Error::InvalidVector(
            "pgvector sparsevec reserved field is not zero".to_owned(),
        ));
    }
    let non_zero_count = usize::try_from(non_zero_count).map_err(|_| {
        context_core::Error::InvalidVector(
            "pgvector sparsevec nonzero count exceeds addressable memory".to_owned(),
        )
    })?;
    let expected_len = PGVECTOR_SPARSE_HEADER_BYTES
        .checked_add(
            non_zero_count
                .checked_mul(PGVECTOR_SPARSE_ENTRY_BYTES)
                .ok_or_else(|| {
                    context_core::Error::InvalidVector(
                        "pgvector sparsevec payload length overflows".to_owned(),
                    )
                })?,
        )
        .ok_or_else(|| {
            context_core::Error::InvalidVector(
                "pgvector sparsevec payload length overflows".to_owned(),
            )
        })?;
    if payload.len() != expected_len {
        return Err(context_core::Error::InvalidVector(format!(
            "pgvector sparsevec payload length is {}, expected {expected_len}",
            payload.len()
        )));
    }

    let indices_offset = PGVECTOR_SPARSE_HEADER_BYTES;
    let values_offset = indices_offset + non_zero_count * size_of::<i32>();
    let mut previous_index = None;
    let mut entries = Vec::with_capacity(non_zero_count);
    for position in 0..non_zero_count {
        let packed_index = read_i32(payload, indices_offset + position * size_of::<i32>());
        if packed_index < 0 || packed_index >= dimensions {
            return Err(context_core::Error::InvalidVector(format!(
                "pgvector sparsevec index {packed_index} is outside dimensions {dimensions}"
            )));
        }
        if previous_index.is_some_and(|previous| packed_index <= previous) {
            return Err(context_core::Error::InvalidVector(
                "pgvector sparsevec indices are not strictly increasing".to_owned(),
            ));
        }
        let value = read_f32(payload, values_offset + position * size_of::<f32>());
        if value == 0.0 {
            return Err(context_core::Error::InvalidVector(format!(
                "pgvector sparsevec value at index {packed_index} is zero"
            )));
        }
        let one_based_index = usize::try_from(packed_index)
            .ok()
            .and_then(|index| index.checked_add(1))
            .ok_or_else(|| {
                context_core::Error::InvalidVector(
                    "pgvector sparsevec index exceeds addressable memory".to_owned(),
                )
            })?;
        entries.push(SparseEntry::new(one_based_index, value)?);
        previous_index = Some(packed_index);
    }

    SparseVector::new(
        usize::try_from(dimensions).map_err(|_| {
            context_core::Error::InvalidVector(
                "pgvector sparsevec dimensions exceed addressable memory".to_owned(),
            )
        })?,
        entries,
    )
}

/// Encodes a validated pgContext sparse vector in pgvector's packed payload.
pub(crate) fn encode_pgvector_sparsevec_payload(
    vector: &SparseVector,
) -> Result<Vec<u8>, context_core::Error> {
    let dimensions = i32::try_from(vector.dimensions()).map_err(|_| {
        context_core::Error::InvalidVector(
            "sparsevec dimensions exceed pgvector integer range".to_owned(),
        )
    })?;
    let non_zero_count = i32::try_from(vector.non_zero_count()).map_err(|_| {
        context_core::Error::InvalidVector(
            "sparsevec nonzero count exceeds pgvector integer range".to_owned(),
        )
    })?;
    let payload_len = PGVECTOR_SPARSE_HEADER_BYTES
        .checked_add(
            vector
                .non_zero_count()
                .checked_mul(PGVECTOR_SPARSE_ENTRY_BYTES)
                .ok_or_else(|| {
                    context_core::Error::InvalidVector(
                        "pgvector sparsevec payload length overflows".to_owned(),
                    )
                })?,
        )
        .ok_or_else(|| {
            context_core::Error::InvalidVector(
                "pgvector sparsevec payload length overflows".to_owned(),
            )
        })?;
    let mut payload = vec![0_u8; payload_len];
    payload[0..4].copy_from_slice(&dimensions.to_ne_bytes());
    payload[4..8].copy_from_slice(&non_zero_count.to_ne_bytes());
    payload[8..12].copy_from_slice(&0_i32.to_ne_bytes());
    let values_offset =
        PGVECTOR_SPARSE_HEADER_BYTES + vector.non_zero_count() * size_of::<i32>();
    for (position, entry) in vector.entries().iter().enumerate() {
        let packed_index = i32::try_from(entry.index() - 1).map_err(|_| {
            context_core::Error::InvalidVector(
                "sparsevec index exceeds pgvector integer range".to_owned(),
            )
        })?;
        let index_offset = PGVECTOR_SPARSE_HEADER_BYTES + position * size_of::<i32>();
        payload[index_offset..index_offset + 4].copy_from_slice(&packed_index.to_ne_bytes());
        let value_offset = values_offset + position * size_of::<f32>();
        payload[value_offset..value_offset + 4].copy_from_slice(&entry.value().to_ne_bytes());
    }
    Ok(payload)
}

/// Decodes a non-null, certified pgvector `sparsevec` datum.
///
/// # Safety
///
/// `datum` must be a live non-null datum whose SQL type is the certified
/// pgvector-owned `public.sparsevec`.
pub(crate) unsafe fn sparse_from_pgvector_datum(datum: pg_sys::Datum) -> SparseVector {
    let original = datum.cast_mut_ptr::<pg_sys::varlena>();
    // SAFETY: The caller guarantees a live pgvector sparsevec varlena.
    let detoasted = unsafe { pg_sys::pg_detoast_datum_packed(original) };
    // SAFETY: PostgreSQL's varlena helpers bound the payload to the detoasted allocation.
    let payload = unsafe {
        slice::from_raw_parts(
            pgrx::varlena::vardata_any(detoasted).cast::<u8>(),
            pgrx::varlena::varsize_any_exhdr(detoasted),
        )
    };
    if payload.len() >= PGVECTOR_SPARSE_HEADER_BYTES {
        let dimensions = read_i32(payload, 0);
        if dimensions
            > i32::try_from(context_core::policy::MAX_VECTOR_DIMENSIONS)
                .unwrap_or(i32::MAX)
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                format!(
                    "pgvector sparsevec dimensions {dimensions} exceed pgContext's current limit {}; \
                     large-dimension sparse support is planned",
                    context_core::policy::MAX_VECTOR_DIMENSIONS
                ),
            );
        }
    }
    let decoded = decode_pgvector_sparsevec_payload(payload).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("invalid pgvector sparsevec datum: {error}"),
        )
    });
    if detoasted != original {
        // SAFETY: A distinct packed detoast result is caller-owned.
        unsafe { pg_sys::pfree(detoasted.cast()) };
    }
    decoded
}

pub(crate) fn sparse_into_pgvector_datum(vector: &SparseVector) -> pg_sys::Datum {
    let payload =
        encode_pgvector_sparsevec_payload(vector).unwrap_or_else(|error| raise_core_error(error));
    let allocation_len = pg_sys::VARHDRSZ
        .checked_add(payload.len())
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "pgvector sparsevec datum length overflows",
            )
        });
    let varlena_len = i32::try_from(allocation_len).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "pgvector sparsevec datum exceeds PostgreSQL varlena storage",
        )
    });
    // SAFETY: PostgreSQL allocates the complete datum in the current memory context.
    let varlena = unsafe { pg_sys::palloc(allocation_len).cast::<pg_sys::varlena>() };
    // SAFETY: The header and payload exactly initialize the allocation above.
    unsafe {
        pgrx::varlena::set_varsize_4b(varlena, varlena_len);
        ptr::copy_nonoverlapping(
            payload.as_ptr(),
            varlena.cast::<u8>().add(pg_sys::VARHDRSZ),
            payload.len(),
        );
    }
    varlena.into()
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_finfo_pgcontext_pgvector_sparsevec_to_pgcontext(
) -> *const pg_sys::Pg_finfo_record {
    &PGVECTOR_SPARSE_FINFO
}

#[unsafe(no_mangle)]
pub extern "C-unwind" fn pg_finfo_pgcontext_pgcontext_sparsevec_to_pgvector(
) -> *const pg_sys::Pg_finfo_record {
    &PGVECTOR_SPARSE_FINFO
}

/// Converts a certified pgvector sparsevec datum to pgContext ownership.
///
/// # Safety
///
/// PostgreSQL must call this through the matching strict one-argument SQL
/// declaration in the `pgcontext_pgvector` bridge.
#[pg_guard]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn pgcontext_pgvector_sparsevec_to_pgcontext(
    fcinfo: pg_sys::FunctionCallInfo,
) -> pg_sys::Datum {
    // SAFETY: The bridge SQL declaration is strict and accepts public.sparsevec.
    let datum = unsafe { pgrx::fcinfo::pg_getarg_datum_raw(fcinfo, 0) };
    // SAFETY: The bridge preflight certifies the argument type and extension owner.
    let sparse = unsafe { sparse_from_pgvector_datum(datum) };
    SparseVec::from_sparse(sparse)
        .into_datum()
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                "pgContext sparsevec conversion unexpectedly returned null",
            )
        })
}

/// Converts a pgContext sparsevec datum to pgvector's packed representation.
///
/// # Safety
///
/// PostgreSQL must call this through the matching strict one-argument SQL
/// declaration in the `pgcontext_pgvector` bridge.
#[pg_guard]
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn pgcontext_pgcontext_sparsevec_to_pgvector(
    fcinfo: pg_sys::FunctionCallInfo,
) -> pg_sys::Datum {
    // SAFETY: The bridge SQL declaration is strict and accepts pgcontext.sparsevec.
    let datum = unsafe { pgrx::fcinfo::pg_getarg_datum_raw(fcinfo, 0) };
    // SAFETY: PostgreSQL binds the argument to the canonical sparsevec SQL type.
    let sparse = unsafe { SparseVec::from_datum(datum, false) }.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
            "pgContext sparsevec conversion argument is null",
        )
    });
    let sparse = sparse
        .to_sparse()
        .unwrap_or_else(|error| raise_core_error(error));
    sparse_into_pgvector_datum(&sparse)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_payload() -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&5_i32.to_ne_bytes());
        payload.extend_from_slice(&2_i32.to_ne_bytes());
        payload.extend_from_slice(&0_i32.to_ne_bytes());
        payload.extend_from_slice(&0_i32.to_ne_bytes());
        payload.extend_from_slice(&3_i32.to_ne_bytes());
        payload.extend_from_slice(&1.5_f32.to_ne_bytes());
        payload.extend_from_slice(&(-2.25_f32).to_ne_bytes());
        payload
    }

    #[test]
    fn pgvector_sparsevec_payload_round_trips_exact_values(
    ) -> Result<(), context_core::Error> {
        let decoded = decode_pgvector_sparsevec_payload(&fixture_payload())?;
        assert_eq!(decoded.to_string(), "{1:1.5,4:-2.25}/5");
        assert_eq!(
            encode_pgvector_sparsevec_payload(&decoded)?,
            fixture_payload()
        );
        Ok(())
    }

    #[test]
    fn pgvector_sparsevec_payload_rejects_noncanonical_or_malformed_data() {
        assert!(decode_pgvector_sparsevec_payload(&[0; 8]).is_err());

        let mut reserved = fixture_payload();
        reserved[8..12].copy_from_slice(&1_i32.to_ne_bytes());
        assert!(decode_pgvector_sparsevec_payload(&reserved).is_err());

        let mut duplicate = fixture_payload();
        duplicate[16..20].copy_from_slice(&0_i32.to_ne_bytes());
        assert!(decode_pgvector_sparsevec_payload(&duplicate).is_err());

        let mut zero = fixture_payload();
        zero[20..24].copy_from_slice(&0_f32.to_ne_bytes());
        assert!(decode_pgvector_sparsevec_payload(&zero).is_err());

        let mut out_of_bounds = fixture_payload();
        out_of_bounds[16..20].copy_from_slice(&5_i32.to_ne_bytes());
        assert!(decode_pgvector_sparsevec_payload(&out_of_bounds).is_err());
    }
}
