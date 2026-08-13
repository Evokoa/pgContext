use pgrx::{IntoDatum, JsonB, datum::FromDatum, prelude::*};

#[derive(Debug)]
pub(crate) struct BoundedExactFirstSpecification(JsonB);

impl BoundedExactFirstSpecification {
    pub(super) fn into_json(self) -> JsonB {
        self.0
    }
}

impl FromDatum for BoundedExactFirstSpecification {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _typoid: pg_sys::Oid,
    ) -> Option<Self> {
        crate::semantic_rerank::decode_bounded_jsonb(
            datum,
            is_null,
            super::MAX_SPEC_BYTES,
            super::MAX_JSON_NODES,
            super::MAX_JSON_DEPTH,
            "exact-first specification exceeds the JSON allocation budget",
        )
        .map(Self)
    }
}

impl IntoDatum for BoundedExactFirstSpecification {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        self.0.into_datum()
    }

    fn type_oid() -> pg_sys::Oid {
        JsonB::type_oid()
    }
}

// SAFETY: the generated wrapper declares jsonb and bounded decoding happens
// before any serde-owned tree is returned to the entrypoint.
unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for BoundedExactFirstSpecification {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        // SAFETY: ArgAbi guarantees the declared jsonb SQL type.
        unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
            crate::error::raise_sql_error(
                PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                "exact-first specification is null",
            )
        })
    }
}

impl_sql_translatable!(BoundedExactFirstSpecification, "jsonb");

#[derive(Debug)]
pub(crate) struct BoundedExactFirstObjectives(JsonB);

impl BoundedExactFirstObjectives {
    pub(super) fn into_json(self) -> JsonB {
        self.0
    }
}

impl FromDatum for BoundedExactFirstObjectives {
    unsafe fn from_polymorphic_datum(
        datum: pg_sys::Datum,
        is_null: bool,
        _typoid: pg_sys::Oid,
    ) -> Option<Self> {
        crate::semantic_rerank::decode_bounded_jsonb(
            datum,
            is_null,
            super::MAX_OBJECTIVES_BYTES,
            super::MAX_JSON_NODES,
            super::MAX_JSON_DEPTH,
            "exact-first objectives exceed the JSON allocation budget",
        )
        .map(Self)
    }
}

impl IntoDatum for BoundedExactFirstObjectives {
    fn into_datum(self) -> Option<pg_sys::Datum> {
        self.0.into_datum()
    }

    fn type_oid() -> pg_sys::Oid {
        JsonB::type_oid()
    }
}

// SAFETY: the generated wrapper declares jsonb and bounded decoding happens
// before any serde-owned tree is returned to the entrypoint.
unsafe impl<'fcx> pgrx::callconv::ArgAbi<'fcx> for BoundedExactFirstObjectives {
    unsafe fn unbox_arg_unchecked(arg: pgrx::callconv::Arg<'_, 'fcx>) -> Self {
        // SAFETY: ArgAbi guarantees the declared jsonb SQL type.
        unsafe { arg.unbox_arg_using_from_datum() }.unwrap_or_else(|| {
            crate::error::raise_sql_error(
                PgSqlErrorCode::ERRCODE_NULL_VALUE_NOT_ALLOWED,
                "exact-first objectives are null",
            )
        })
    }
}

impl_sql_translatable!(BoundedExactFirstObjectives, "jsonb");
