//! Typmod support for SQL-facing vector variant wrappers.

use core::ffi::CStr;

use context_core::policy::MAX_VECTOR_DIMENSIONS;
use pgrx::ffi::CString;
use pgrx::prelude::*;
use pgrx::{Array, PgSqlErrorCode};

use crate::error::{raise_core_error, raise_sql_error};
use crate::vector::Vector;
use crate::vector_variants::{BitVec, HalfVec, SparseVec};

pgrx::extension_sql!(
    r#"
ALTER TYPE vector SET (
    TYPMOD_IN = pgcontext.vector_typmod_in,
    TYPMOD_OUT = pgcontext.vector_typmod_out
);

ALTER TYPE halfvec SET (
    TYPMOD_IN = pgcontext.halfvec_typmod_in,
    TYPMOD_OUT = pgcontext.halfvec_typmod_out
);

ALTER TYPE sparsevec SET (
    TYPMOD_IN = pgcontext.sparsevec_typmod_in,
    TYPMOD_OUT = pgcontext.sparsevec_typmod_out
);

ALTER TYPE bitvec SET (
    TYPMOD_IN = pgcontext.bitvec_typmod_in,
    TYPMOD_OUT = pgcontext.bitvec_typmod_out
);

CREATE CAST (halfvec AS halfvec)
    WITH FUNCTION pgcontext.halfvec_enforce_typmod(halfvec, integer, boolean)
    AS IMPLICIT;

CREATE CAST (sparsevec AS sparsevec)
    WITH FUNCTION pgcontext.sparsevec_enforce_typmod(sparsevec, integer, boolean)
    AS IMPLICIT;

CREATE CAST (bitvec AS bitvec)
    WITH FUNCTION pgcontext.bitvec_enforce_typmod(bitvec, integer, boolean)
    AS IMPLICIT;

CREATE CAST (vector AS vector)
    WITH FUNCTION pgcontext.vector_enforce_typmod(vector, integer, boolean)
    AS IMPLICIT;
"#,
    name = "create_vector_variant_typmods",
    requires = [
        Vector,
        HalfVec,
        SparseVec,
        BitVec,
        vector_typmod_in,
        vector_typmod_out,
        vector_enforce_typmod,
        halfvec_typmod_in,
        halfvec_typmod_out,
        sparsevec_typmod_in,
        sparsevec_typmod_out,
        bitvec_typmod_in,
        bitvec_typmod_out,
        halfvec_enforce_typmod,
        sparsevec_enforce_typmod,
        bitvec_enforce_typmod
    ]
);

/// Parses a `vector(n)` type modifier.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn vector_typmod_in(modifiers: Array<'_, &CStr>) -> i32 {
    parse_vector_typmod(modifiers, "vector")
}

/// Formats a `vector(n)` type modifier.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn vector_typmod_out(typmod: i32) -> CString {
    format_vector_typmod(typmod)
}

/// Enforces a declared `vector(n)` typmod during assignment.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn vector_enforce_typmod(vector: Vector, typmod: i32, _explicit: bool) -> Vector {
    if typmod >= 0 {
        let required = typmod_to_dimension(typmod, "vector");
        ensure_typmod_dimension("vector", required, vector.dimension());
    }
    vector
}

/// Parses a `halfvec(n)` type modifier.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn halfvec_typmod_in(modifiers: Array<'_, &CStr>) -> i32 {
    parse_vector_typmod(modifiers, "halfvec")
}

/// Formats a `halfvec(n)` type modifier.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn halfvec_typmod_out(typmod: i32) -> CString {
    format_vector_typmod(typmod)
}

/// Parses a `sparsevec(n)` type modifier.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn sparsevec_typmod_in(modifiers: Array<'_, &CStr>) -> i32 {
    parse_vector_typmod(modifiers, "sparsevec")
}

/// Formats a `sparsevec(n)` type modifier.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn sparsevec_typmod_out(typmod: i32) -> CString {
    format_vector_typmod(typmod)
}

/// Parses a `bitvec(n)` type modifier.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn bitvec_typmod_in(modifiers: Array<'_, &CStr>) -> i32 {
    parse_vector_typmod(modifiers, "bitvec")
}

/// Formats a `bitvec(n)` type modifier.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn bitvec_typmod_out(typmod: i32) -> CString {
    format_vector_typmod(typmod)
}

/// Enforces a declared `halfvec(n)` typmod during assignment.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn halfvec_enforce_typmod(vector: HalfVec, typmod: i32, _explicit: bool) -> HalfVec {
    if typmod >= 0 {
        let required = typmod_to_dimension(typmod, "halfvec");
        let actual = match vector.to_half() {
            Ok(vector) => vector.dimension(),
            Err(error) => raise_core_error(error),
        };
        ensure_typmod_dimension("halfvec", required, actual);
    }
    vector
}

/// Enforces a declared `sparsevec(n)` typmod during assignment.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn sparsevec_enforce_typmod(vector: SparseVec, typmod: i32, _explicit: bool) -> SparseVec {
    if typmod >= 0 {
        let required = typmod_to_dimension(typmod, "sparsevec");
        let actual = match vector.to_sparse() {
            Ok(vector) => vector.dimensions(),
            Err(error) => raise_core_error(error),
        };
        ensure_typmod_dimension("sparsevec", required, actual);
    }
    vector
}

/// Enforces a declared `bitvec(n)` typmod during assignment.
#[pg_extern(immutable, parallel_safe, strict)]
#[must_use]
pub fn bitvec_enforce_typmod(vector: BitVec, typmod: i32, _explicit: bool) -> BitVec {
    if typmod >= 0 {
        let required = typmod_to_dimension(typmod, "bitvec");
        let actual = match vector.to_bit() {
            Ok(vector) => vector.len(),
            Err(error) => raise_core_error(error),
        };
        ensure_typmod_dimension("bitvec", required, actual);
    }
    vector
}

fn parse_vector_typmod(modifiers: Array<'_, &CStr>, label: &str) -> i32 {
    if modifiers.len() != 1 || modifiers.contains_nulls() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} typmod requires exactly one dimension"),
        );
    }

    let Some(modifier) = modifiers.iter_deny_null().next() else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} typmod requires exactly one dimension"),
        );
    };
    let text = match modifier.to_str() {
        Ok(text) => text,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} typmod dimension must be valid UTF-8"),
        ),
    };
    let dimensions = match text.parse::<i32>() {
        Ok(dimensions) => dimensions,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} typmod dimension must be an integer: {text}"),
        ),
    };

    validate_typmod_dimension(label, dimensions)
}

fn format_vector_typmod(typmod: i32) -> CString {
    if typmod < 0 {
        return CString::default();
    }
    match CString::new(format!("({typmod})")) {
        Ok(output) => output,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "typmod output unexpectedly contained a NUL byte",
        ),
    }
}

fn typmod_to_dimension(typmod: i32, label: &str) -> usize {
    let dimensions = validate_typmod_dimension(label, typmod);
    match usize::try_from(dimensions) {
        Ok(dimensions) => dimensions,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            format!("{label} typmod dimensions exceed usize range: {dimensions}"),
        ),
    }
}

fn validate_typmod_dimension(label: &str, dimensions: i32) -> i32 {
    let max = match i32::try_from(MAX_VECTOR_DIMENSIONS) {
        Ok(max) => max,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "vector dimension policy exceeds PostgreSQL integer range",
        ),
    };
    if !(1..=max).contains(&dimensions) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} typmod dimensions must be between 1 and {max}: {dimensions}"),
        );
    }
    dimensions
}

fn ensure_typmod_dimension(label: &str, required: usize, actual: usize) {
    if required != actual {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "dimension mismatch: {label} typmod requires {required} dimensions, value has {actual}"
            ),
        );
    }
}
