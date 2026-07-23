//! Btree ordering support for SQL vector variant wrappers.

use core::cmp::Ordering;

use context_core::{BitVector, HalfVector, SparseVector};
use pgrx::prelude::*;

use crate::error::{raise_core_error, raise_sql_error};
use crate::vector_variants::{BitVec, HalfVec, SparseVec};

pgrx::extension_sql!(
    r#"
CREATE OPERATOR pgcontext.< (
    LEFTARG = halfvec,
    RIGHTARG = halfvec,
    FUNCTION = pgcontext.halfvec_lt,
    COMMUTATOR = OPERATOR(pgcontext.>),
    NEGATOR = OPERATOR(pgcontext.>=)
);
CREATE OPERATOR pgcontext.<= (
    LEFTARG = halfvec,
    RIGHTARG = halfvec,
    FUNCTION = pgcontext.halfvec_le,
    COMMUTATOR = OPERATOR(pgcontext.>=),
    NEGATOR = OPERATOR(pgcontext.>)
);
CREATE OPERATOR pgcontext.= (
    LEFTARG = halfvec,
    RIGHTARG = halfvec,
    FUNCTION = pgcontext.halfvec_eq,
    COMMUTATOR = OPERATOR(pgcontext.=),
    NEGATOR = OPERATOR(pgcontext.<>)
);
CREATE OPERATOR pgcontext.<> (
    LEFTARG = halfvec,
    RIGHTARG = halfvec,
    FUNCTION = pgcontext.halfvec_ne,
    COMMUTATOR = OPERATOR(pgcontext.<>),
    NEGATOR = OPERATOR(pgcontext.=)
);
CREATE OPERATOR pgcontext.>= (
    LEFTARG = halfvec,
    RIGHTARG = halfvec,
    FUNCTION = pgcontext.halfvec_ge,
    COMMUTATOR = OPERATOR(pgcontext.<=),
    NEGATOR = OPERATOR(pgcontext.<)
);
CREATE OPERATOR pgcontext.> (
    LEFTARG = halfvec,
    RIGHTARG = halfvec,
    FUNCTION = pgcontext.halfvec_gt,
    COMMUTATOR = OPERATOR(pgcontext.<),
    NEGATOR = OPERATOR(pgcontext.<=)
);
CREATE OPERATOR CLASS pgcontext.halfvec_ops
    DEFAULT FOR TYPE halfvec USING btree AS
    OPERATOR 1 pgcontext.< (halfvec, halfvec),
    OPERATOR 2 pgcontext.<= (halfvec, halfvec),
    OPERATOR 3 pgcontext.= (halfvec, halfvec),
    OPERATOR 4 pgcontext.>= (halfvec, halfvec),
    OPERATOR 5 pgcontext.> (halfvec, halfvec),
    FUNCTION 1 pgcontext.halfvec_cmp(halfvec, halfvec);

CREATE OPERATOR pgcontext.< (
    LEFTARG = sparsevec,
    RIGHTARG = sparsevec,
    FUNCTION = pgcontext.sparsevec_lt,
    COMMUTATOR = OPERATOR(pgcontext.>),
    NEGATOR = OPERATOR(pgcontext.>=)
);
CREATE OPERATOR pgcontext.<= (
    LEFTARG = sparsevec,
    RIGHTARG = sparsevec,
    FUNCTION = pgcontext.sparsevec_le,
    COMMUTATOR = OPERATOR(pgcontext.>=),
    NEGATOR = OPERATOR(pgcontext.>)
);
CREATE OPERATOR pgcontext.= (
    LEFTARG = sparsevec,
    RIGHTARG = sparsevec,
    FUNCTION = pgcontext.sparsevec_eq,
    COMMUTATOR = OPERATOR(pgcontext.=),
    NEGATOR = OPERATOR(pgcontext.<>)
);
CREATE OPERATOR pgcontext.<> (
    LEFTARG = sparsevec,
    RIGHTARG = sparsevec,
    FUNCTION = pgcontext.sparsevec_ne,
    COMMUTATOR = OPERATOR(pgcontext.<>),
    NEGATOR = OPERATOR(pgcontext.=)
);
CREATE OPERATOR pgcontext.>= (
    LEFTARG = sparsevec,
    RIGHTARG = sparsevec,
    FUNCTION = pgcontext.sparsevec_ge,
    COMMUTATOR = OPERATOR(pgcontext.<=),
    NEGATOR = OPERATOR(pgcontext.<)
);
CREATE OPERATOR pgcontext.> (
    LEFTARG = sparsevec,
    RIGHTARG = sparsevec,
    FUNCTION = pgcontext.sparsevec_gt,
    COMMUTATOR = OPERATOR(pgcontext.<),
    NEGATOR = OPERATOR(pgcontext.<=)
);
CREATE OPERATOR CLASS pgcontext.sparsevec_ops
    DEFAULT FOR TYPE sparsevec USING btree AS
    OPERATOR 1 pgcontext.< (sparsevec, sparsevec),
    OPERATOR 2 pgcontext.<= (sparsevec, sparsevec),
    OPERATOR 3 pgcontext.= (sparsevec, sparsevec),
    OPERATOR 4 pgcontext.>= (sparsevec, sparsevec),
    OPERATOR 5 pgcontext.> (sparsevec, sparsevec),
    FUNCTION 1 pgcontext.sparsevec_cmp(sparsevec, sparsevec);

CREATE OPERATOR pgcontext.< (
    LEFTARG = bitvec,
    RIGHTARG = bitvec,
    FUNCTION = pgcontext.bitvec_lt,
    COMMUTATOR = OPERATOR(pgcontext.>),
    NEGATOR = OPERATOR(pgcontext.>=)
);
CREATE OPERATOR pgcontext.<= (
    LEFTARG = bitvec,
    RIGHTARG = bitvec,
    FUNCTION = pgcontext.bitvec_le,
    COMMUTATOR = OPERATOR(pgcontext.>=),
    NEGATOR = OPERATOR(pgcontext.>)
);
CREATE OPERATOR pgcontext.= (
    LEFTARG = bitvec,
    RIGHTARG = bitvec,
    FUNCTION = pgcontext.bitvec_eq,
    COMMUTATOR = OPERATOR(pgcontext.=),
    NEGATOR = OPERATOR(pgcontext.<>)
);
CREATE OPERATOR pgcontext.<> (
    LEFTARG = bitvec,
    RIGHTARG = bitvec,
    FUNCTION = pgcontext.bitvec_ne,
    COMMUTATOR = OPERATOR(pgcontext.<>),
    NEGATOR = OPERATOR(pgcontext.=)
);
CREATE OPERATOR pgcontext.>= (
    LEFTARG = bitvec,
    RIGHTARG = bitvec,
    FUNCTION = pgcontext.bitvec_ge,
    COMMUTATOR = OPERATOR(pgcontext.<=),
    NEGATOR = OPERATOR(pgcontext.<)
);
CREATE OPERATOR pgcontext.> (
    LEFTARG = bitvec,
    RIGHTARG = bitvec,
    FUNCTION = pgcontext.bitvec_gt,
    COMMUTATOR = OPERATOR(pgcontext.<),
    NEGATOR = OPERATOR(pgcontext.<=)
);
CREATE OPERATOR CLASS pgcontext.bitvec_ops
    DEFAULT FOR TYPE bitvec USING btree AS
    OPERATOR 1 pgcontext.< (bitvec, bitvec),
    OPERATOR 2 pgcontext.<= (bitvec, bitvec),
    OPERATOR 3 pgcontext.= (bitvec, bitvec),
    OPERATOR 4 pgcontext.>= (bitvec, bitvec),
    OPERATOR 5 pgcontext.> (bitvec, bitvec),
    FUNCTION 1 pgcontext.bitvec_cmp(bitvec, bitvec);
"#,
    name = "create_vector_variant_comparison_operators",
    requires = [
        HalfVec,
        SparseVec,
        BitVec,
        halfvec_lt,
        halfvec_le,
        halfvec_eq,
        halfvec_ne,
        halfvec_ge,
        halfvec_gt,
        halfvec_cmp,
        sparsevec_lt,
        sparsevec_le,
        sparsevec_eq,
        sparsevec_ne,
        sparsevec_ge,
        sparsevec_gt,
        sparsevec_cmp,
        bitvec_lt,
        bitvec_le,
        bitvec_eq,
        bitvec_ne,
        bitvec_ge,
        bitvec_gt,
        bitvec_cmp
    ]
);

/// Compares half vectors for btree ordering.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_cmp(left: HalfVec, right: HalfVec) -> i32 {
    ordering_to_i32(compare_halfvecs(left, right))
}

#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_lt(left: HalfVec, right: HalfVec) -> bool {
    compare_halfvecs(left, right).is_lt()
}

#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_le(left: HalfVec, right: HalfVec) -> bool {
    compare_halfvecs(left, right).is_le()
}

#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_eq(left: HalfVec, right: HalfVec) -> bool {
    compare_halfvecs(left, right).is_eq()
}

#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_ne(left: HalfVec, right: HalfVec) -> bool {
    !compare_halfvecs(left, right).is_eq()
}

#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_ge(left: HalfVec, right: HalfVec) -> bool {
    compare_halfvecs(left, right).is_ge()
}

#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_gt(left: HalfVec, right: HalfVec) -> bool {
    compare_halfvecs(left, right).is_gt()
}

/// Compares sparse vectors for btree ordering.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_cmp(left: SparseVec, right: SparseVec) -> i32 {
    ordering_to_i32(compare_sparsevecs(left, right))
}

#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_lt(left: SparseVec, right: SparseVec) -> bool {
    compare_sparsevecs(left, right).is_lt()
}

#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_le(left: SparseVec, right: SparseVec) -> bool {
    compare_sparsevecs(left, right).is_le()
}

#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_eq(left: SparseVec, right: SparseVec) -> bool {
    compare_sparsevecs(left, right).is_eq()
}

#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_ne(left: SparseVec, right: SparseVec) -> bool {
    !compare_sparsevecs(left, right).is_eq()
}

#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_ge(left: SparseVec, right: SparseVec) -> bool {
    compare_sparsevecs(left, right).is_ge()
}

#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_gt(left: SparseVec, right: SparseVec) -> bool {
    compare_sparsevecs(left, right).is_gt()
}

/// Compares bit vectors for btree ordering.
#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_cmp(left: BitVec, right: BitVec) -> i32 {
    ordering_to_i32(compare_bitvecs(left, right))
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_lt(left: BitVec, right: BitVec) -> bool {
    compare_bitvecs(left, right).is_lt()
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_le(left: BitVec, right: BitVec) -> bool {
    compare_bitvecs(left, right).is_le()
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_eq(left: BitVec, right: BitVec) -> bool {
    compare_bitvecs(left, right).is_eq()
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_ne(left: BitVec, right: BitVec) -> bool {
    !compare_bitvecs(left, right).is_eq()
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_ge(left: BitVec, right: BitVec) -> bool {
    compare_bitvecs(left, right).is_ge()
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_gt(left: BitVec, right: BitVec) -> bool {
    compare_bitvecs(left, right).is_gt()
}

fn ordering_to_i32(ordering: Ordering) -> i32 {
    match ordering {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

fn compare_halfvecs(left: HalfVec, right: HalfVec) -> Ordering {
    let left = halfvec_to_core(left);
    let right = halfvec_to_core(right);

    for (left, right) in left.as_slice().iter().zip(right.as_slice()) {
        match left.partial_cmp(right) {
            Some(Ordering::Equal) => {}
            Some(ordering) => return ordering,
            None => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "cannot compare non-finite halfvec values",
            ),
        }
    }
    left.dimension().cmp(&right.dimension())
}

fn compare_sparsevecs(left: SparseVec, right: SparseVec) -> Ordering {
    let left = sparsevec_to_core(left);
    let right = sparsevec_to_core(right);

    for (left, right) in left.entries().iter().zip(right.entries()) {
        match left.index().cmp(&right.index()) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
        match left.value().partial_cmp(&right.value()) {
            Some(Ordering::Equal) => {}
            Some(ordering) => return ordering,
            None => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "cannot compare non-finite sparsevec values",
            ),
        }
    }
    match left.entries().len().cmp(&right.entries().len()) {
        Ordering::Equal => left.dimensions().cmp(&right.dimensions()),
        ordering => ordering,
    }
}

fn compare_bitvecs(left: BitVec, right: BitVec) -> Ordering {
    let left = bitvec_to_core(left);
    let right = bitvec_to_core(right);

    for (left, right) in left.as_slice().iter().zip(right.as_slice()) {
        match left.cmp(right) {
            Ordering::Equal => {}
            ordering => return ordering,
        }
    }
    left.len().cmp(&right.len())
}

fn halfvec_to_core(vector: HalfVec) -> HalfVector {
    match vector.to_half() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    }
}

fn sparsevec_to_core(vector: SparseVec) -> SparseVector {
    match vector.to_sparse() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    }
}

fn bitvec_to_core(vector: BitVec) -> BitVector {
    match vector.to_bit() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    }
}
