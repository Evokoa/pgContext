// SQL-facing experimental vector variant wrappers.

#[allow(
    unsafe_code,
    reason = "the pgvector-layout halfvec varlena codec is an audited PostgreSQL datum boundary"
)]
#[path = "vector_variants/halfvec_datum.rs"]
mod halfvec_datum;
#[allow(
    unsafe_code,
    reason = "the pgvector sparsevec packed-varlena codec is an audited PostgreSQL datum boundary"
)]
#[path = "vector_variants/pgvector_sparsevec_datum.rs"]
pub(crate) mod pgvector_sparsevec_datum;
#[path = "vector_variants/vector_variant_distance.rs"]
mod vector_variant_distance;

use core::ffi::CStr;

use context_core::{
    BitVector, DenseVector, DistanceMetric, Error as CoreError, HalfVector, SparseEntry,
    SparseVector,
};
use pgrx::InOutFuncs;
use pgrx::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{raise_core_error, raise_sql_error};
use crate::vector::Vector;
use vector_variant_distance::{
    SparseDistanceMetric, bitvec_to_core, dimension_to_i32, halfvec_distance, sparsevec_distance,
    sparsevec_to_core,
};

/// PostgreSQL half-precision vector wrapper backed by checked core values.
///
/// Storage is byte-for-byte pgvector's `struct HalfVector`:
/// `{ int16 dim; int16 unused; uint16 x[dim] }` after the varlena length
/// word, where each element is an IEEE 754 binary16 bit pattern. Values are
/// canonicalized to half precision at construction so in-memory, printed,
/// and stored values are always identical to what pgvector would hold.
#[derive(Debug, Clone, PartialEq, PostgresType)]
#[inoutfuncs]
#[bikeshed_postgres_type_manually_impl_from_into_datum]
pub struct HalfVec {
    values: Vec<f32>,
}

/// PostgreSQL sparse vector wrapper backed by canonical core entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, PostgresType)]
#[inoutfuncs]
pub struct SparseVec {
    dimensions: i32,
    entries: Vec<SparseVecEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct SparseVecEntry {
    index: i32,
    value: f32,
}

/// PostgreSQL bit-vector wrapper backed by checked core bit values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, PostgresType)]
#[inoutfuncs]
pub struct BitVec {
    bits: Vec<bool>,
}

impl HalfVec {
    /// Creates a SQL half vector wrapper from a validated core vector.
    ///
    /// Values are canonicalized through binary16 (round-to-nearest-even and
    /// widen back), matching pgvector's input semantics: `0.1::halfvec`
    /// holds exactly the half-precision value `0.099975586` everywhere.
    #[must_use]
    pub fn from_half(vector: HalfVector) -> Self {
        Self {
            values: vector
                .as_slice()
                .iter()
                .map(|value| context_core::half_bits_to_f32(context_core::f32_to_half_bits(*value)))
                .collect(),
        }
    }

    pub(crate) fn from_validated_values(values: Vec<f32>) -> Self {
        Self { values }
    }

    pub(crate) fn as_slice(&self) -> &[f32] {
        &self.values
    }

    /// Converts this SQL wrapper into the core half vector type.
    ///
    /// # Errors
    ///
    /// Returns [`context_core::Error::InvalidVector`] if the stored values are invalid.
    pub fn to_half(&self) -> Result<HalfVector, context_core::Error> {
        HalfVector::new(self.values.clone())
    }
}

impl SparseVec {
    /// Creates a SQL sparse vector wrapper from a validated core vector.
    #[must_use]
    pub fn from_sparse(vector: SparseVector) -> Self {
        let dimensions = match i32::try_from(vector.dimensions()) {
            Ok(dimensions) => dimensions,
            Err(_) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                "sparsevec dimensions exceed PostgreSQL integer range",
            ),
        };
        let entries = vector
            .entries()
            .iter()
            .map(|entry| SparseVecEntry {
                index: match i32::try_from(entry.index()) {
                    Ok(index) => index,
                    Err(_) => raise_sql_error(
                        PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                        "sparsevec index exceeds PostgreSQL integer range",
                    ),
                },
                value: entry.value(),
            })
            .collect();

        Self {
            dimensions,
            entries,
        }
    }

    /// Converts this SQL wrapper into the core sparse vector type.
    ///
    /// # Errors
    ///
    /// Returns [`context_core::Error::InvalidVector`] if stored dimensions or entries are invalid.
    pub fn to_sparse(&self) -> Result<SparseVector, context_core::Error> {
        let dimensions = usize::try_from(self.dimensions).map_err(|_| {
            CoreError::InvalidVector(format!("invalid sparsevec dimensions: {}", self.dimensions))
        })?;
        let entries = self
            .entries
            .iter()
            .map(|entry| {
                let index = usize::try_from(entry.index).map_err(|_| {
                    CoreError::InvalidVector(format!("invalid sparsevec index: {}", entry.index))
                })?;
                SparseEntry::new(index, entry.value)
            })
            .collect::<Result<Vec<_>, _>>()?;

        SparseVector::new(dimensions, entries)
    }
}

impl BitVec {
    /// Creates a SQL bit-vector wrapper from a validated core vector.
    #[must_use]
    pub fn from_bit(vector: BitVector) -> Self {
        Self {
            bits: vector.as_slice().to_vec(),
        }
    }

    /// Converts this SQL wrapper into the core bit vector type.
    ///
    /// # Errors
    ///
    /// Returns [`context_core::Error::InvalidVector`] if the stored bits are invalid.
    pub fn to_bit(&self) -> Result<BitVector, context_core::Error> {
        BitVector::new(self.bits.clone())
    }
}

impl InOutFuncs for HalfVec {
    fn input(input: &CStr) -> Self {
        let text = input_text(input, "halfvec");

        match text.parse::<HalfVector>() {
            Ok(vector) => Self::from_half(vector),
            Err(error) => raise_core_error(error),
        }
    }

    fn output(&self, buffer: &mut pgrx::StringInfo) {
        match self.to_half() {
            Ok(vector) => buffer.push_str(&vector.to_string()),
            Err(error) => raise_core_error(error),
        }
    }
}

impl InOutFuncs for SparseVec {
    fn input(input: &CStr) -> Self {
        let text = input_text(input, "sparsevec");

        match text.parse::<SparseVector>() {
            Ok(vector) => Self::from_sparse(vector),
            Err(error) => raise_core_error(error),
        }
    }

    fn output(&self, buffer: &mut pgrx::StringInfo) {
        match self.to_sparse() {
            Ok(vector) => buffer.push_str(&vector.to_string()),
            Err(error) => raise_core_error(error),
        }
    }
}

impl InOutFuncs for BitVec {
    fn input(input: &CStr) -> Self {
        let text = input_text(input, "bitvec");

        match text.parse::<BitVector>() {
            Ok(vector) => Self::from_bit(vector),
            Err(error) => raise_core_error(error),
        }
    }

    fn output(&self, buffer: &mut pgrx::StringInfo) {
        match self.to_bit() {
            Ok(vector) => buffer.push_str(&vector.to_string()),
            Err(error) => raise_core_error(error),
        }
    }
}

pgrx::extension_sql!(
    r#"
CREATE CAST (real[] AS halfvec)
    WITH FUNCTION pgcontext.halfvec_from_real_array(real[]);

CREATE CAST (integer[] AS halfvec)
    WITH FUNCTION pgcontext.halfvec_from_integer_array(integer[]);

CREATE CAST (double precision[] AS halfvec)
    WITH FUNCTION pgcontext.halfvec_from_double_array(double precision[]);

CREATE CAST (halfvec AS real[])
    WITH FUNCTION pgcontext.halfvec_to_real_array(halfvec)
    AS ASSIGNMENT;

CREATE CAST (halfvec AS vector)
    WITH FUNCTION pgcontext.halfvec_to_vector(halfvec)
    AS ASSIGNMENT;
"#,
    name = "create_halfvec_array_casts",
    requires = [
        Vector,
        HalfVec,
        halfvec_from_real_array,
        halfvec_from_integer_array,
        halfvec_from_double_array,
        halfvec_to_real_array,
        halfvec_to_vector
    ]
);

pgrx::extension_sql!(
    r#"
CREATE CAST (bit AS bitvec) WITH INOUT AS ASSIGNMENT;
CREATE CAST (bit varying AS bitvec) WITH INOUT AS ASSIGNMENT;
CREATE CAST (bitvec AS bit varying) WITH INOUT AS ASSIGNMENT;
CREATE CAST (bitvec AS bit) WITH INOUT;
CREATE CAST (boolean[] AS bitvec) WITH FUNCTION pgcontext.bitvec_from_bool_array(boolean[]) AS ASSIGNMENT;
CREATE CAST (bitvec AS boolean[]) WITH FUNCTION pgcontext.bitvec_to_bool_array(bitvec) AS ASSIGNMENT;
CREATE FUNCTION pgcontext.hamming_distance("left" bit, "right" bit) RETURNS double precision LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT AS $$
    SELECT pgcontext.bitvec_hamming_distance($1::bitvec, $2::bitvec)::double precision
$$;
CREATE FUNCTION pgcontext.hamming_distance("left" bit varying, "right" bit varying) RETURNS double precision LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT AS $$
    SELECT pgcontext.bitvec_hamming_distance($1::bitvec, $2::bitvec)::double precision
$$;
CREATE FUNCTION pgcontext.jaccard_distance("left" bit, "right" bit) RETURNS double precision LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT AS $$
    SELECT pgcontext.bitvec_jaccard_distance($1::bitvec, $2::bitvec)
$$;
CREATE FUNCTION pgcontext.jaccard_distance("left" bit varying, "right" bit varying) RETURNS double precision LANGUAGE sql IMMUTABLE PARALLEL SAFE STRICT AS $$
    SELECT pgcontext.bitvec_jaccard_distance($1::bitvec, $2::bitvec)
$$;
CREATE OPERATOR pgcontext.<~> (LEFTARG = bit, RIGHTARG = bit, FUNCTION = pgcontext.hamming_distance, COMMUTATOR = OPERATOR(pgcontext.<~>));
CREATE OPERATOR pgcontext.<~> (LEFTARG = bit varying, RIGHTARG = bit varying, FUNCTION = pgcontext.hamming_distance, COMMUTATOR = OPERATOR(pgcontext.<~>));
CREATE OPERATOR pgcontext.<%> (LEFTARG = bit, RIGHTARG = bit, FUNCTION = pgcontext.jaccard_distance, COMMUTATOR = OPERATOR(pgcontext.<%>));
CREATE OPERATOR pgcontext.<%> (LEFTARG = bit varying, RIGHTARG = bit varying, FUNCTION = pgcontext.jaccard_distance, COMMUTATOR = OPERATOR(pgcontext.<%>));
"#,
    name = "create_bitvec_bool_array_casts",
    requires = [
        BitVec,
        bitvec_from_bool_array,
        bitvec_to_bool_array,
        bitvec_hamming_distance,
        bitvec_jaccard_distance
    ]
);

pgrx::extension_sql!(
    r#"
CREATE CAST (real[] AS sparsevec)
    WITH FUNCTION pgcontext.sparsevec_from_real_array(real[])
    AS ASSIGNMENT;

CREATE CAST (sparsevec AS real[])
    WITH FUNCTION pgcontext.sparsevec_to_real_array(sparsevec)
    AS ASSIGNMENT;

CREATE CAST (vector AS sparsevec)
    WITH FUNCTION pgcontext.sparsevec_from_vector(vector)
    AS ASSIGNMENT;

CREATE CAST (sparsevec AS vector)
    WITH FUNCTION pgcontext.sparsevec_to_vector(sparsevec)
    AS ASSIGNMENT;
"#,
    name = "create_sparsevec_real_array_casts",
    requires = [
        Vector,
        SparseVec,
        sparsevec_from_real_array,
        sparsevec_to_real_array,
        sparsevec_from_vector,
        sparsevec_to_vector
    ]
);

pgrx::extension_sql!(
    r#"
CREATE OPERATOR pgcontext.<#> (LEFTARG = halfvec, RIGHTARG = halfvec, FUNCTION = pgcontext.halfvec_negative_inner_product, COMMUTATOR = OPERATOR(pgcontext.<#>));
CREATE OPERATOR pgcontext.<=> (LEFTARG = halfvec, RIGHTARG = halfvec, FUNCTION = pgcontext.halfvec_cosine_distance, COMMUTATOR = OPERATOR(pgcontext.<=>));
CREATE OPERATOR pgcontext.<+> (LEFTARG = halfvec, RIGHTARG = halfvec, FUNCTION = pgcontext.halfvec_l1_distance, COMMUTATOR = OPERATOR(pgcontext.<+>));
CREATE OPERATOR pgcontext.<#> (LEFTARG = sparsevec, RIGHTARG = sparsevec, FUNCTION = pgcontext.sparsevec_negative_inner_product, COMMUTATOR = OPERATOR(pgcontext.<#>));
CREATE OPERATOR pgcontext.<=> (LEFTARG = sparsevec, RIGHTARG = sparsevec, FUNCTION = pgcontext.sparsevec_cosine_distance, COMMUTATOR = OPERATOR(pgcontext.<=>));
CREATE OPERATOR pgcontext.<+> (LEFTARG = sparsevec, RIGHTARG = sparsevec, FUNCTION = pgcontext.sparsevec_l1_distance, COMMUTATOR = OPERATOR(pgcontext.<+>));
CREATE OPERATOR pgcontext.<%> (LEFTARG = bitvec, RIGHTARG = bitvec, FUNCTION = pgcontext.bitvec_jaccard_distance, COMMUTATOR = OPERATOR(pgcontext.<%>));

"#,
    name = "create_vector_variant_distance_operators",
    requires = [
        Vector,
        HalfVec,
        SparseVec,
        BitVec,
        halfvec_l2_distance,
        halfvec_negative_inner_product,
        halfvec_cosine_distance,
        halfvec_l1_distance,
        sparsevec_l2_distance,
        sparsevec_negative_inner_product,
        sparsevec_cosine_distance,
        sparsevec_l1_distance,
        bitvec_hamming_distance,
        bitvec_jaccard_distance
    ]
);

pgrx::extension_sql!(
    r#"
CREATE AGGREGATE pgcontext.sum(halfvec) (
    SFUNC = pgcontext.halfvec_sum_transition,
    STYPE = real[],
    FINALFUNC = pgcontext.halfvec_sum_final
);

CREATE AGGREGATE pgcontext.avg(halfvec) (
    SFUNC = pgcontext.halfvec_sum_transition,
    STYPE = real[],
    FINALFUNC = pgcontext.halfvec_avg_final
);

CREATE AGGREGATE pgcontext.sum(sparsevec) (
    SFUNC = pgcontext.sparsevec_sum_transition,
    STYPE = real[],
    FINALFUNC = pgcontext.sparsevec_sum_final
);

CREATE AGGREGATE pgcontext.avg(sparsevec) (
    SFUNC = pgcontext.sparsevec_sum_transition,
    STYPE = real[],
    FINALFUNC = pgcontext.sparsevec_avg_final
);

CREATE AGGREGATE pgcontext.bit_or(bitvec) (
    SFUNC = pgcontext.bitvec_or_transition,
    STYPE = boolean[],
    FINALFUNC = pgcontext.bitvec_bits_final
);

CREATE AGGREGATE pgcontext.bit_and(bitvec) (
    SFUNC = pgcontext.bitvec_and_transition,
    STYPE = boolean[],
    FINALFUNC = pgcontext.bitvec_bits_final
);
"#,
    name = "create_vector_variant_aggregates",
    requires = [
        HalfVec,
        SparseVec,
        BitVec,
        halfvec_sum_transition,
        halfvec_sum_final,
        halfvec_avg_final,
        sparsevec_sum_transition,
        sparsevec_sum_final,
        sparsevec_avg_final,
        bitvec_or_transition,
        bitvec_and_transition,
        bitvec_bits_final
    ]
);

fn input_text<'a>(input: &'a CStr, label: &str) -> &'a str {
    match input.to_str() {
        Ok(text) => text,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION,
            format!("invalid {label} input: expected UTF-8 text"),
        ),
    }
}

/// Explicitly converts a PostgreSQL `real[]` array into a half vector.
///
/// Values are rounded to half precision.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_from_real_array(values: Vec<f32>) -> HalfVec {
    halfvec_from_values(values)
}

/// Explicitly converts a PostgreSQL `integer[]` array into a half vector.
///
/// Values are rounded to half precision.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_from_integer_array(values: Vec<i32>) -> HalfVec {
    halfvec_from_numeric_text_values(values)
}

/// Explicitly converts a PostgreSQL `double precision[]` array into a half vector.
///
/// Values are rounded to half precision.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_from_double_array(values: Vec<f64>) -> HalfVec {
    halfvec_from_numeric_text_values(values)
}

/// Converts a half vector into a PostgreSQL `real[]` array.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_to_real_array(vector: HalfVec) -> Vec<f32> {
    match vector.to_half() {
        Ok(vector) => vector.as_slice().to_vec(),
        Err(error) => raise_core_error(error),
    }
}

/// Converts a half vector into a dense SQL vector for ANN storage.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_to_vector(vector: HalfVec) -> Vector {
    let values = halfvec_to_real_array(vector);
    let vector = DenseVector::new(values).unwrap_or_else(|error| raise_core_error(error));
    Vector::from_dense(vector)
}

/// Converts a PostgreSQL `boolean[]` array into a bit vector.
#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_from_bool_array(bits: Vec<bool>) -> BitVec {
    match BitVector::new(bits) {
        Ok(vector) => BitVec::from_bit(vector),
        Err(error) => raise_core_error(error),
    }
}

/// Converts a bit vector into a PostgreSQL `boolean[]` array.
#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_to_bool_array(vector: BitVec) -> Vec<bool> {
    match vector.to_bit() {
        Ok(vector) => vector.as_slice().to_vec(),
        Err(error) => raise_core_error(error),
    }
}

/// Converts a PostgreSQL `real[]` array into a sparse vector.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_from_real_array(values: Vec<f32>) -> SparseVec {
    let dimensions = values.len();
    let entries = values
        .into_iter()
        .enumerate()
        .filter(|(_, value)| *value != 0.0)
        .map(|(offset, value)| {
            SparseEntry::new(offset + 1, value).unwrap_or_else(|error| raise_core_error(error))
        })
        .collect::<Vec<_>>();

    SparseVec::from_sparse(
        SparseVector::new(dimensions, entries).unwrap_or_else(|error| raise_core_error(error)),
    )
}

/// Converts a sparse vector into a PostgreSQL `real[]` array.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_to_real_array(vector: SparseVec) -> Vec<f32> {
    let vector = sparsevec_to_core(vector);
    let mut values = vec![0.0; vector.dimensions()];
    for entry in vector.entries() {
        values[entry.index() - 1] = entry.value();
    }
    values
}

/// Converts a dense SQL vector into a sparse vector.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_from_vector(vector: Vector) -> SparseVec {
    let vector = match vector.to_dense() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };
    sparsevec_from_dense(vector)
}

/// Converts a sparse vector into a dense SQL vector.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_to_vector(vector: SparseVec) -> Vector {
    let values = sparsevec_to_real_array(vector);
    let vector = DenseVector::new(values).unwrap_or_else(|error| raise_core_error(error));
    Vector::from_dense(vector)
}

/// Parses a SQL `halfvec` value from text.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec(input: &str) -> HalfVec {
    match input.parse::<HalfVector>() {
        Ok(vector) => HalfVec::from_half(vector),
        Err(error) => raise_core_error(error),
    }
}

/// Parses a SQL `sparsevec` value from text.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec(input: &str) -> SparseVec {
    match input.parse::<SparseVector>() {
        Ok(vector) => SparseVec::from_sparse(vector),
        Err(error) => raise_core_error(error),
    }
}

/// Builds a SQL `sparsevec` value from aligned index and value arrays.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_from_arrays(indices: Vec<i32>, values: Vec<f32>, dimensions: i32) -> SparseVec {
    if indices.len() != values.len() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "sparsevec indices and values must have the same length: got {} indices and {} values",
                indices.len(),
                values.len()
            ),
        );
    }

    let dimensions = match usize::try_from(dimensions) {
        Ok(dimensions) => dimensions,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid sparsevec dimensions: {dimensions}"),
        ),
    };

    let entries = indices
        .into_iter()
        .zip(values)
        .map(|(index, value)| {
            let index = match usize::try_from(index) {
                Ok(index) => index,
                Err(_) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("invalid sparsevec index: {index}"),
                ),
            };
            match SparseEntry::new(index, value) {
                Ok(entry) => entry,
                Err(error) => raise_core_error(error),
            }
        })
        .collect::<Vec<_>>();

    match SparseVector::new(dimensions, entries) {
        Ok(vector) => SparseVec::from_sparse(vector),
        Err(error) => raise_core_error(error),
    }
}

/// Parses a SQL `bitvec` value from text.
#[pg_extern(immutable, parallel_safe)]
pub fn bitvec(input: &str) -> BitVec {
    match input.parse::<BitVector>() {
        Ok(vector) => BitVec::from_bit(vector),
        Err(error) => raise_core_error(error),
    }
}

fn halfvec_from_numeric_text_values<T: ToString>(values: Vec<T>) -> HalfVec {
    let values = values
        .into_iter()
        .map(|value| match value.to_string().parse::<f32>() {
            Ok(value) => value,
            Err(_) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                "array value cannot be represented as a halfvec element",
            ),
        })
        .collect::<Vec<_>>();
    halfvec_from_values(values)
}

fn halfvec_from_values(values: Vec<f32>) -> HalfVec {
    match HalfVector::new(values) {
        Ok(vector) => HalfVec::from_half(vector),
        Err(error) => raise_core_error(error),
    }
}

fn sparsevec_from_dense(vector: DenseVector) -> SparseVec {
    let dimensions = vector.dimension();
    let entries = vector
        .as_slice()
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, value)| *value != 0.0)
        .map(|(offset, value)| {
            SparseEntry::new(offset + 1, value).unwrap_or_else(|error| raise_core_error(error))
        })
        .collect::<Vec<_>>();

    SparseVec::from_sparse(
        SparseVector::new(dimensions, entries).unwrap_or_else(|error| raise_core_error(error)),
    )
}

/// Returns the number of dimensions in a half vector.
#[pg_extern(immutable, parallel_safe)]
#[must_use]
pub fn halfvec_dims(vector: HalfVec) -> i32 {
    match vector.to_half() {
        Ok(vector) => dimension_to_i32(vector.dimension(), "halfvec dimension"),
        Err(error) => raise_core_error(error),
    }
}

/// Returns the number of dimensions in a sparse vector.
#[pg_extern(immutable, parallel_safe)]
#[must_use]
pub fn sparsevec_dims(vector: SparseVec) -> i32 {
    match vector.to_sparse() {
        Ok(vector) => dimension_to_i32(vector.dimensions(), "sparsevec dimensions"),
        Err(error) => raise_core_error(error),
    }
}

/// Returns canonical sparse-vector indexes as a PostgreSQL `integer[]` array.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_indices(vector: SparseVec) -> Vec<i32> {
    sparsevec_to_core(vector)
        .entries()
        .iter()
        .map(|entry| dimension_to_i32(entry.index(), "sparsevec index"))
        .collect()
}

/// Returns canonical sparse-vector values as a PostgreSQL `real[]` array.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_values(vector: SparseVec) -> Vec<f32> {
    sparsevec_to_core(vector)
        .entries()
        .iter()
        .map(|entry| entry.value())
        .collect()
}

/// Returns the number of bits in a bit vector.
#[pg_extern(immutable, parallel_safe)]
#[must_use]
pub fn bitvec_dims(vector: BitVec) -> i32 {
    match vector.to_bit() {
        Ok(vector) => dimension_to_i32(vector.len(), "bitvec dimensions"),
        Err(error) => raise_core_error(error),
    }
}

/// Returns L2 distance between two half vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_l2_distance(left: HalfVec, right: HalfVec) -> f32 {
    halfvec_distance(left, right, DistanceMetric::L2)
}

/// Returns inner product between two half vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_inner_product(left: HalfVec, right: HalfVec) -> f32 {
    halfvec_distance(left, right, DistanceMetric::InnerProduct)
}

/// Returns negative inner product between two half vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_negative_inner_product(left: HalfVec, right: HalfVec) -> f32 {
    -halfvec_distance(left, right, DistanceMetric::InnerProduct)
}

/// Returns cosine distance between two half vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_cosine_distance(left: HalfVec, right: HalfVec) -> f32 {
    halfvec_distance(left, right, DistanceMetric::Cosine)
}

/// Returns L1 distance between two half vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_l1_distance(left: HalfVec, right: HalfVec) -> f32 {
    halfvec_distance(left, right, DistanceMetric::L1)
}

/// Returns L2 distance between two sparse vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_l2_distance(left: SparseVec, right: SparseVec) -> f32 {
    sparsevec_distance(left, right, SparseDistanceMetric::L2)
}

/// Returns inner product between two sparse vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_inner_product(left: SparseVec, right: SparseVec) -> f32 {
    sparsevec_distance(left, right, SparseDistanceMetric::InnerProduct)
}

/// Returns negative inner product between two sparse vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_negative_inner_product(left: SparseVec, right: SparseVec) -> f32 {
    -sparsevec_distance(left, right, SparseDistanceMetric::InnerProduct)
}

/// Returns cosine distance between two sparse vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_cosine_distance(left: SparseVec, right: SparseVec) -> f32 {
    sparsevec_distance(left, right, SparseDistanceMetric::Cosine)
}

/// Returns L1 distance between two sparse vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_l1_distance(left: SparseVec, right: SparseVec) -> f32 {
    sparsevec_distance(left, right, SparseDistanceMetric::L1)
}

/// Returns Hamming distance between two bit vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_hamming_distance(left: BitVec, right: BitVec) -> i32 {
    let left = bitvec_to_core(left);
    let right = bitvec_to_core(right);

    match left.hamming_distance(&right) {
        Ok(distance) => dimension_to_i32(distance, "bitvec hamming distance"),
        Err(error) => raise_core_error(error),
    }
}

/// Returns Jaccard distance between two bit vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_jaccard_distance(left: BitVec, right: BitVec) -> f64 {
    let left = bitvec_to_core(left);
    let right = bitvec_to_core(right);

    match left.jaccard_distance(&right) {
        Ok(distance) => distance,
        Err(error) => raise_core_error(error),
    }
}

/// Accumulates one half vector into the aggregate state.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_sum_transition(state: Option<Vec<f32>>, value: Option<HalfVec>) -> Option<Vec<f32>> {
    let Some(value) = value else {
        return state;
    };
    let value = match value.to_half() {
        Ok(value) => value,
        Err(error) => raise_core_error(error),
    };

    let mut state = match state {
        Some(state) => state,
        None => {
            let mut state = Vec::with_capacity(value.dimension() + 1);
            state.push(0.0);
            state.resize(value.dimension() + 1, 0.0);
            state
        }
    };
    if state.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "halfvec aggregate state is empty",
        );
    }
    let dimensions = state.len() - 1;
    if dimensions != value.dimension() {
        raise_core_error(CoreError::DimensionMismatch {
            left: dimensions,
            right: value.dimension(),
        });
    }

    state[0] += 1.0;
    for (sum, value) in state[1..].iter_mut().zip(value.as_slice()) {
        *sum += value;
    }
    Some(state)
}

/// Finalizes half vector sum aggregates.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_sum_final(state: Vec<f32>) -> HalfVec {
    halfvec_from_aggregate_state(state, HalfVecAggregateFinal::Sum)
}

/// Finalizes half vector average aggregates.
#[pg_extern(immutable, parallel_safe)]
pub fn halfvec_avg_final(state: Vec<f32>) -> HalfVec {
    halfvec_from_aggregate_state(state, HalfVecAggregateFinal::Average)
}

/// Accumulates one sparse vector into the aggregate state.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_sum_transition(
    state: Option<Vec<f32>>,
    value: Option<SparseVec>,
) -> Option<Vec<f32>> {
    let Some(value) = value else {
        return state;
    };
    let value = sparsevec_to_core(value);

    let mut state = match state {
        Some(state) => state,
        None => {
            let mut state = Vec::with_capacity(value.dimensions() + 1);
            state.push(0.0);
            state.resize(value.dimensions() + 1, 0.0);
            state
        }
    };
    if state.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "sparsevec aggregate state is empty",
        );
    }
    let dimensions = state.len() - 1;
    if dimensions != value.dimensions() {
        raise_core_error(CoreError::DimensionMismatch {
            left: dimensions,
            right: value.dimensions(),
        });
    }

    state[0] += 1.0;
    for entry in value.entries() {
        state[entry.index()] += entry.value();
    }
    Some(state)
}

/// Finalizes sparse vector sum aggregates.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_sum_final(state: Vec<f32>) -> SparseVec {
    sparsevec_from_aggregate_state(state, SparseVecAggregateFinal::Sum)
}

/// Finalizes sparse vector average aggregates.
#[pg_extern(immutable, parallel_safe)]
pub fn sparsevec_avg_final(state: Vec<f32>) -> SparseVec {
    sparsevec_from_aggregate_state(state, SparseVecAggregateFinal::Average)
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_or_transition(state: Option<Vec<bool>>, value: Option<BitVec>) -> Option<Vec<bool>> {
    bitvec_bool_transition(state, value, BitVecAggregateOp::Or)
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_and_transition(state: Option<Vec<bool>>, value: Option<BitVec>) -> Option<Vec<bool>> {
    bitvec_bool_transition(state, value, BitVecAggregateOp::And)
}

#[pg_extern(immutable, parallel_safe)]
pub fn bitvec_bits_final(state: Vec<bool>) -> BitVec {
    match BitVector::new(state) {
        Ok(vector) => BitVec::from_bit(vector),
        Err(error) => raise_core_error(error),
    }
}

include!("vector_variants/aggregate_helpers.rs");
