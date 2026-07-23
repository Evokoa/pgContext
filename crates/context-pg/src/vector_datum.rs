//! Audited PostgreSQL varlena boundary for dense vectors.

use core::{mem::align_of, ptr, slice};

use pgrx::PgMemoryContexts;
use pgrx::datum::{FromDatum, IntoDatum, UnboxDatum};
use pgrx::prelude::*;

use crate::error::{raise_core_error, raise_sql_error};
use crate::vector::{
    VECTOR_BINARY_HEADER_BYTES, Vector, decode_vector_payload, decode_vector_payload_dimensions,
    encode_vector_payload,
};

static VECTOR_DISTANCE_FINFO: pg_sys::Pg_finfo_record = pg_sys::Pg_finfo_record { api_version: 1 };

/// Borrowed view over the float payload of an aligned, detoasted vector datum.
pub(crate) struct VectorPayloadView<'a> {
    values: &'a [f32],
}

impl<'a> VectorPayloadView<'a> {
    pub(crate) const fn values(&self) -> &'a [f32] {
        self.values
    }
}

/// Validates a dense-vector payload and borrows its values without allocation.
pub(crate) fn decode_vector_payload_view(
    payload: &[u8],
) -> Result<VectorPayloadView<'_>, context_core::Error> {
    let dimensions = decode_vector_payload_dimensions(payload)?;
    let value_bytes = &payload[VECTOR_BINARY_HEADER_BYTES..];
    if !cfg!(target_endian = "little")
        || !value_bytes
            .as_ptr()
            .addr()
            .is_multiple_of(align_of::<f32>())
    {
        return Err(context_core::Error::InvalidVector(
            "dense vector binary payload values are not natively aligned".to_owned(),
        ));
    }
    // SAFETY: Header validation proves the byte span contains exactly
    // `dimensions` f32 values; the explicit check proves f32 alignment. The
    // binary format is native on little-endian V1 targets.
    let values = unsafe { slice::from_raw_parts(value_bytes.as_ptr().cast::<f32>(), dimensions) };
    Ok(VectorPayloadView { values })
}

struct DetoastedVectorDatum {
    original: *mut pg_sys::varlena,
    detoasted: *mut pg_sys::varlena,
}

impl DetoastedVectorDatum {
    /// Detoasts a non-null vector datum for the duration of one function call.
    ///
    /// # Safety
    ///
    /// `datum` must be a non-null datum of the pgContext `vector` SQL type.
    unsafe fn from_datum(datum: pg_sys::Datum) -> Self {
        let original = datum.cast_mut_ptr::<pg_sys::varlena>();
        // SAFETY: The caller guarantees that `datum` is a live vector varlena.
        // Packed detoasting avoids rewriting ordinary inline datums. The
        // payload decoder below still rejects any representation whose float
        // region is not naturally aligned.
        let packed = unsafe { pg_sys::pg_detoast_datum_packed(original) };
        // SAFETY: `packed` is the live detoasted varlena returned above; the
        // decoder validates its complete payload before this address is read.
        let packed_values = unsafe {
            pgrx::varlena::vardata_any(packed)
                .cast::<u8>()
                .add(VECTOR_BINARY_HEADER_BYTES)
        };
        let detoasted = if packed_values.addr() % align_of::<f32>() == 0 {
            packed
        } else {
            if packed != original {
                // SAFETY: A distinct packed detoast result is caller-owned.
                unsafe { pg_sys::pfree(packed.cast()) };
            }
            // SAFETY: The original datum remains live and full detoasting
            // provides an aligned four-byte varlena representation.
            unsafe { pg_sys::pg_detoast_datum(original) }
        };
        Self {
            original,
            detoasted,
        }
    }

    fn values(&self) -> &[f32] {
        // SAFETY: `detoasted` remains live for `self`; PostgreSQL's varlena
        // helpers bound the payload span to the detoasted allocation.
        let payload = unsafe {
            slice::from_raw_parts(
                pgrx::varlena::vardata_any(self.detoasted).cast::<u8>(),
                pgrx::varlena::varsize_any_exhdr(self.detoasted),
            )
        };
        decode_vector_payload_view(payload)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    format!("invalid dense vector datum: {error}"),
                )
            })
            .values()
    }
}

impl Drop for DetoastedVectorDatum {
    fn drop(&mut self) {
        if self.detoasted != self.original {
            // SAFETY: A distinct pointer returned by `pg_detoast_datum` is a
            // caller-owned palloc allocation and is released exactly once.
            unsafe { pg_sys::pfree(self.detoasted.cast()) };
        }
    }
}

/// Computes a distance directly over detoasted vector payloads.
///
/// # Safety
///
/// PostgreSQL must supply a valid V1 `FunctionCallInfo` for a strict function
/// with two non-null pgContext vector arguments.
unsafe fn distance_from_fcinfo(
    fcinfo: pg_sys::FunctionCallInfo,
    metric: context_core::DistanceMetric,
) -> f32 {
    // SAFETY: The SQL declarations below are strict two-vector functions and
    // PostgreSQL supplies the matching V1 call frame.
    let left_datum = unsafe { pgrx::fcinfo::pg_getarg_datum_raw(fcinfo, 0) };
    // SAFETY: Same call-frame contract as the preceding argument.
    let right_datum = unsafe { pgrx::fcinfo::pg_getarg_datum_raw(fcinfo, 1) };
    // SAFETY: Both datums have the SQL vector type declared below.
    let left = unsafe { DetoastedVectorDatum::from_datum(left_datum) };
    // SAFETY: Both datums have the SQL vector type declared below.
    let right = unsafe { DetoastedVectorDatum::from_datum(right_datum) };
    metric
        .distance_slices(left.values(), right.values())
        .unwrap_or_else(|error| raise_core_error(error))
}

macro_rules! vector_distance_finfo {
    ($symbol:ident) => {
        #[unsafe(no_mangle)]
        pub extern "C-unwind" fn $symbol() -> *const pg_sys::Pg_finfo_record {
            &VECTOR_DISTANCE_FINFO
        }
    };
}

vector_distance_finfo!(pg_finfo_pgcontext_l2_distance_fast);
vector_distance_finfo!(pg_finfo_pgcontext_l2_distance_fast8);
vector_distance_finfo!(pg_finfo_pgcontext_negative_inner_product_fast);
vector_distance_finfo!(pg_finfo_pgcontext_cosine_distance_fast);
vector_distance_finfo!(pg_finfo_pgcontext_l1_distance_fast);

macro_rules! vector_distance_entrypoint {
    ($symbol:ident, $metric:expr, $transform:expr) => {
        /// PostgreSQL V1 zero-copy distance entrypoint.
        ///
        /// # Safety
        ///
        /// PostgreSQL must call this exported symbol through the matching
        /// strict two-vector SQL declaration below.
        #[pg_guard]
        #[unsafe(no_mangle)]
        pub unsafe extern "C-unwind" fn $symbol(fcinfo: pg_sys::FunctionCallInfo) -> pg_sys::Datum {
            // SAFETY: The exported symbol is reachable only through its strict
            // SQL declaration with two pgContext vector arguments.
            let score = unsafe { distance_from_fcinfo(fcinfo, $metric) };
            ($transform)(score)
                .into_datum()
                .expect("non-null floating-point distance datum")
        }
    };
}

vector_distance_entrypoint!(
    pgcontext_l2_distance_fast,
    context_core::DistanceMetric::L2,
    |score: f32| score
);
vector_distance_entrypoint!(
    pgcontext_l2_distance_fast8,
    context_core::DistanceMetric::L2,
    f64::from
);
vector_distance_entrypoint!(
    pgcontext_negative_inner_product_fast,
    context_core::DistanceMetric::InnerProduct,
    |score: f32| -score
);
vector_distance_entrypoint!(
    pgcontext_cosine_distance_fast,
    context_core::DistanceMetric::Cosine,
    |score: f32| score
);
vector_distance_entrypoint!(
    pgcontext_l1_distance_fast,
    context_core::DistanceMetric::L1,
    |score: f32| score
);

pgrx::extension_sql!(
    r#"
CREATE FUNCTION pgcontext._l2_distance_fast(pgcontext.vector, pgcontext.vector)
RETURNS real
AS 'MODULE_PATHNAME', 'pgcontext_l2_distance_fast'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE FUNCTION pgcontext._l2_distance_fast8(pgcontext.vector, pgcontext.vector)
RETURNS double precision
AS 'MODULE_PATHNAME', 'pgcontext_l2_distance_fast8'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE FUNCTION pgcontext._negative_inner_product_fast(pgcontext.vector, pgcontext.vector)
RETURNS real
AS 'MODULE_PATHNAME', 'pgcontext_negative_inner_product_fast'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE FUNCTION pgcontext._cosine_distance_fast(pgcontext.vector, pgcontext.vector)
RETURNS real
AS 'MODULE_PATHNAME', 'pgcontext_cosine_distance_fast'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;

CREATE FUNCTION pgcontext._l1_distance_fast(pgcontext.vector, pgcontext.vector)
RETURNS real
AS 'MODULE_PATHNAME', 'pgcontext_l1_distance_fast'
LANGUAGE C IMMUTABLE STRICT PARALLEL SAFE;
"#,
    name = "create_vector_fast_distance_functions",
    requires = ["pgcontext_bootstrap", Vector]
);

impl IntoDatum for Vector {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        let payload =
            encode_vector_payload(self.as_slice()).unwrap_or_else(|error| raise_core_error(error));
        let allocation_len = pg_sys::VARHDRSZ
            .checked_add(payload.len())
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "dense vector datum length overflows",
                )
            });
        let varlena_len = i32::try_from(allocation_len).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "dense vector datum exceeds PostgreSQL varlena storage",
            )
        });
        // SAFETY: PostgreSQL allocates `allocation_len` bytes in the current
        // memory context or raises an ERROR. The full allocation is initialized
        // before the pointer is returned as a Datum.
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

impl FromDatum for Vector {
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
        // SAFETY: the detoasted varlena remains valid while its bounded payload
        // slice is decoded into Rust-owned values.
        let payload = unsafe {
            slice::from_raw_parts(
                pgrx::varlena::vardata_any(varlena).cast::<u8>(),
                pgrx::varlena::varsize_any_exhdr(varlena),
            )
        };
        let values = decode_vector_payload(payload).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("invalid dense vector datum: {error}"),
            )
        });
        Some(Vector::from_validated_values(values))
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
                // SAFETY: PostgreSQL copies the source datum into the requested
                // memory context before the regular validated decoder consumes it.
                let copied = pg_sys::pg_detoast_datum_copy(datum.cast_mut_ptr());
                // SAFETY: `copied` is a non-null datum of this SQL type in the
                // active memory context.
                Self::from_polymorphic_datum(copied.into(), false, typoid)
            })
        }
    }
}

// SAFETY: `IntoDatum` returns a PostgreSQL-owned varlena pointer for this exact
// SQL type, or marks the function result null through `FcInfo`.
unsafe impl pgrx::callconv::BoxRet for Vector {
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
// returned `Vector` owns all values and does not borrow from the source datum.
unsafe impl UnboxDatum for Vector {
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
                "dense vector datum is null",
            )
        })
    }
}

// SAFETY: PostgreSQL supplies an argument with the Vector SQL type established
// by the generated function signature. Decoding owns the returned values.
unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for Vector {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        let index = arg.index();
        // SAFETY: `ArgAbi` guarantees this unchecked path receives a non-null
        // argument of the declared logical type.
        unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                format!("dense vector argument {index} is null"),
            )
        })
    }
}
