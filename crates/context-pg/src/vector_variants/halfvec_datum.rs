//! pgvector-layout binary codec and manual datum bindings for `HalfVec`.
//!
//! Mirrors `vector_datum.rs`'s treatment of `Vector`: the varlena payload is
//! byte-for-byte pgvector's `struct HalfVector`
//! (`{ int16 dim; int16 unused; uint16 x[dim] }`, elements as IEEE 754
//! binary16 bits), making coexist-mode binding to pgvector's `halfvec` type
//! lossless in both directions. The reserved word is written zero and
//! required zero on decode (fail closed on corruption).

use core::ptr;
use core::slice;

use context_core::{Error as CoreError, f32_to_half_bits, half_bits_to_f32};
use pgrx::datum::UnboxDatum;
use pgrx::prelude::*;
use pgrx::{FromDatum, IntoDatum, PgMemoryContexts};

use super::HalfVec;
use crate::error::{raise_core_error, raise_sql_error};

pub(crate) const HALFVEC_BINARY_HEADER_BYTES: usize = 4;

pub(crate) fn encode_halfvec_payload(values: &[f32]) -> Result<Vec<u8>, CoreError> {
    if values.len() > context_core::policy::MAX_VECTOR_DIMENSIONS {
        return Err(CoreError::InvalidVector(
            "halfvec dimensions exceed binary storage".to_owned(),
        ));
    }
    let dimensions = u16::try_from(values.len()).map_err(|_| {
        CoreError::InvalidVector("halfvec dimensions exceed binary storage".to_owned())
    })?;
    let payload_len = HALFVEC_BINARY_HEADER_BYTES
        .checked_add(values.len().saturating_mul(size_of::<u16>()))
        .ok_or_else(|| CoreError::InvalidVector("halfvec payload length overflows".to_owned()))?;
    let mut payload = Vec::with_capacity(payload_len);
    payload.extend_from_slice(&dimensions.to_le_bytes());
    payload.extend_from_slice(&0_u16.to_le_bytes());
    for value in values {
        payload.extend_from_slice(&f32_to_half_bits(*value).to_le_bytes());
    }
    Ok(payload)
}

pub(crate) fn decode_halfvec_payload(payload: &[u8]) -> Result<Vec<f32>, CoreError> {
    if payload.len() < HALFVEC_BINARY_HEADER_BYTES {
        return Err(CoreError::InvalidVector(
            "halfvec binary payload is truncated".to_owned(),
        ));
    }
    let dimensions = usize::from(u16::from_le_bytes([payload[0], payload[1]]));
    let unused = u16::from_le_bytes([payload[2], payload[3]]);
    if unused != 0 {
        return Err(CoreError::InvalidVector(
            "halfvec binary payload reserved word is nonzero".to_owned(),
        ));
    }
    if dimensions > context_core::policy::MAX_VECTOR_DIMENSIONS {
        return Err(CoreError::InvalidVector(
            "halfvec dimensions exceed the supported maximum".to_owned(),
        ));
    }
    let expected = HALFVEC_BINARY_HEADER_BYTES
        .checked_add(dimensions.saturating_mul(size_of::<u16>()))
        .ok_or_else(|| CoreError::InvalidVector("halfvec payload length overflows".to_owned()))?;
    if payload.len() != expected {
        return Err(CoreError::InvalidVector(
            "halfvec binary payload length does not match its dimensions".to_owned(),
        ));
    }
    let mut values = Vec::with_capacity(dimensions);
    for bytes in payload[HALFVEC_BINARY_HEADER_BYTES..].chunks_exact(size_of::<u16>()) {
        let widened = half_bits_to_f32(u16::from_le_bytes([bytes[0], bytes[1]]));
        if !widened.is_finite() {
            return Err(CoreError::InvalidVector(
                "halfvec binary payload contains a non-finite element".to_owned(),
            ));
        }
        values.push(widened);
    }
    if values.is_empty() {
        return Err(CoreError::InvalidVector(
            "halfvec values must contain at least one value".to_owned(),
        ));
    }
    Ok(values)
}

impl IntoDatum for HalfVec {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        let payload =
            encode_halfvec_payload(self.as_slice()).unwrap_or_else(|error| raise_core_error(error));
        let allocation_len = pg_sys::VARHDRSZ
            .checked_add(payload.len())
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "halfvec datum length overflows",
                )
            });
        let varlena_len = i32::try_from(allocation_len).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "halfvec datum exceeds PostgreSQL varlena storage",
            )
        });
        // SAFETY: PostgreSQL allocates `allocation_len` bytes in the current
        // memory context or raises an ERROR. The full allocation is
        // initialized before the pointer is returned as a Datum.
        let varlena = unsafe { pg_sys::palloc(allocation_len).cast::<pg_sys::varlena>() };
        // SAFETY: `varlena` points to the allocation above. The 4-byte header
        // and payload exactly cover `allocation_len` non-overlapping bytes.
        unsafe {
            pgrx::varlena::set_varsize_4b(varlena, varlena_len);
            ptr::copy_nonoverlapping(
                payload.as_ptr(),
                varlena.cast::<u8>().add(pg_sys::VARHDRSZ),
                payload.len(),
            );
        }
        Some(varlena.into())
    }

    fn type_oid() -> pg_sys::Oid {
        pgrx::wrappers::rust_regtypein::<Self>()
    }
}

impl FromDatum for HalfVec {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _typoid: pg_sys::Oid,
    ) -> Option<Self> {
        if is_null {
            return None;
        }
        // SAFETY: pgrx calls this implementation only for a non-null datum of
        // this SQL type. PostgreSQL detoasts it into memory valid for the
        // current context and the resulting payload is copied before return.
        let varlena = unsafe { pg_sys::pg_detoast_datum_packed(datum.cast_mut_ptr()) };
        // SAFETY: the detoasted varlena remains valid while its bounded
        // payload slice is decoded into Rust-owned values.
        let payload = unsafe {
            slice::from_raw_parts(
                pgrx::varlena::vardata_any(varlena).cast::<u8>(),
                pgrx::varlena::varsize_any_exhdr(varlena),
            )
        };
        let values = decode_halfvec_payload(payload).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("invalid halfvec datum: {error}"),
            )
        });
        Some(HalfVec::from_validated_values(values))
    }

    unsafe fn from_datum_in_memory_context(
        mut memory_context: PgMemoryContexts,
        datum: pg_sys::Datum,
        is_null: bool,
        typoid: pg_sys::Oid,
    ) -> Option<Self> {
        if is_null {
            return None;
        }
        // SAFETY: pgrx supplied this live memory context specifically for the
        // decoded argument copy and restores the prior context after closure.
        unsafe {
            memory_context.switch_to(|_| {
                // SAFETY: PostgreSQL copies the source datum into the
                // requested memory context before the validated decoder
                // consumes it.
                let copied = pg_sys::pg_detoast_datum_copy(datum.cast_mut_ptr());
                // SAFETY: `copied` is a non-null datum of this SQL type in
                // the active memory context.
                Self::from_polymorphic_datum(copied.into(), false, typoid)
            })
        }
    }
}

// SAFETY: `IntoDatum` returns a PostgreSQL-owned varlena pointer for this
// exact SQL type, or marks the function result null through `FcInfo`.
unsafe impl pgrx::callconv::BoxRet for HalfVec {
    unsafe fn box_into<'fcx>(
        self,
        fcinfo: &mut pgrx::callconv::FcInfo<'fcx>,
    ) -> pgrx::datum::Datum<'fcx> {
        match self.into_datum() {
            None => fcinfo.return_null(),
            // SAFETY: the datum was allocated in PostgreSQL's current result
            // memory context and is returned immediately to the caller.
            Some(datum) => unsafe { fcinfo.return_raw_datum(datum) },
        }
    }
}

// SAFETY: unboxing delegates to the validated owning `FromDatum` decoder; the
// returned `HalfVec` owns all values and does not borrow from the source
// datum.
unsafe impl UnboxDatum for HalfVec {
    type As<'src> = Self;

    unsafe fn unbox<'src>(datum: pgrx::datum::Datum<'src>) -> Self::As<'src>
    where
        Self: 'src,
    {
        // SAFETY: callers of `UnboxDatum` guarantee a non-null datum of this
        // logical SQL type.
        unsafe { Self::from_datum(datum.sans_lifetime(), false) }.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                "halfvec datum is null",
            )
        })
    }
}

// SAFETY: PostgreSQL supplies an argument with the HalfVec SQL type
// established by the generated function signature. Decoding owns the returned
// values.
unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for HalfVec {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        let index = arg.index();
        // SAFETY: `ArgAbi` guarantees this unchecked path receives a non-null
        // argument of the declared logical type.
        unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                format!("halfvec argument {index} is null"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_halfvec_payload, encode_halfvec_payload};

    /// Pins the payload layout to pgvector's `struct HalfVector`
    /// byte-for-byte: `{ int16 dim; int16 unused; uint16 x[dim] }` with
    /// little-endian binary16 elements (1.0 = 0x3C00, -2.5 = 0xC100).
    #[test]
    fn halfvec_binary_payload_matches_pgvector_layout_fixture() -> Result<(), context_core::Error> {
        let payload = encode_halfvec_payload(&[1.0, -2.5])?;
        assert_eq!(
            payload,
            vec![0x02, 0x00, 0x00, 0x00, 0x00, 0x3C, 0x00, 0xC1]
        );
        assert_eq!(decode_halfvec_payload(&payload)?, vec![1.0, -2.5]);
        Ok(())
    }

    #[test]
    fn halfvec_payload_decode_fails_closed() {
        // Truncated header.
        assert!(decode_halfvec_payload(&[0x01, 0x00]).is_err());
        // Nonzero reserved word.
        assert!(decode_halfvec_payload(&[0x01, 0x00, 0x01, 0x00, 0x00, 0x3C]).is_err());
        // Length/dimension mismatch.
        assert!(decode_halfvec_payload(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x3C]).is_err());
        // Non-finite element (binary16 infinity 0x7C00).
        assert!(decode_halfvec_payload(&[0x01, 0x00, 0x00, 0x00, 0x00, 0x7C]).is_err());
        // Zero dimensions.
        assert!(decode_halfvec_payload(&[0x00, 0x00, 0x00, 0x00]).is_err());
    }
}
