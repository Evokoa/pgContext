// SQL-facing vector types and functions.

use core::{cmp::Ordering, ffi::CStr, mem::size_of};

use context_core::{
    DenseVector, DistanceMetric, Error as CoreError, ExactSearchItem, SearchLimit, exact_top_k,
};
use context_index::{HnswError, RerankCandidate, rerank_by_original_vectors};
use pgrx::InOutFuncs;
use pgrx::prelude::*;

use crate::error::{raise_context_error, raise_core_error, raise_sql_error};
use crate::late_interaction::{enforce_late_interaction_budget, late_interaction_score};

/// Dense vector varlena payload layout, byte-for-byte identical to
/// pgvector's `struct Vector`: `{ int16 dim; int16 unused; float4 x[dim] }`
/// after the varlena length word. This parity is the foundation of
/// pgvector coexist/replacement compatibility — pgContext functions and
/// opclasses can bind directly to pgvector's `vector` type (and vice
/// versa at restore time) with zero conversion. The `unused` word must be
/// zero on encode and is required zero on decode (pgvector reserves it;
/// treating nonzero as corruption fails closed). `MAX_VECTOR_DIMENSIONS`
/// (16,000) keeps `dim` comfortably inside `i16` range.
pub(crate) const VECTOR_BINARY_HEADER_BYTES: usize = 4;

pub(crate) fn encode_vector_payload(values: &[f32]) -> Result<Vec<u8>, CoreError> {
    if values.len() > context_core::policy::MAX_VECTOR_DIMENSIONS {
        return Err(CoreError::InvalidVector(
            "dense vector dimensions exceed binary storage".to_owned(),
        ));
    }
    let dimensions = u16::try_from(values.len()).map_err(|_| {
        CoreError::InvalidVector("dense vector dimensions exceed binary storage".to_owned())
    })?;
    let payload_len = VECTOR_BINARY_HEADER_BYTES
        .checked_add(values.len().saturating_mul(size_of::<f32>()))
        .ok_or_else(|| {
            CoreError::InvalidVector("dense vector payload length overflows".to_owned())
        })?;
    let mut payload = Vec::with_capacity(payload_len);
    payload.extend_from_slice(&dimensions.to_le_bytes());
    payload.extend_from_slice(&0_u16.to_le_bytes());
    for value in values {
        payload.extend_from_slice(&value.to_le_bytes());
    }
    Ok(payload)
}

pub(crate) fn decode_vector_payload(payload: &[u8]) -> Result<Vec<f32>, CoreError> {
    let dimensions = decode_vector_payload_dimensions(payload)?;
    let mut values = Vec::with_capacity(dimensions);
    for bytes in payload[VECTOR_BINARY_HEADER_BYTES..].chunks_exact(size_of::<f32>()) {
        values.push(f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]));
    }
    DenseVector::new(values).map(DenseVector::into_values)
}

pub(crate) fn decode_vector_payload_dimensions(payload: &[u8]) -> Result<usize, CoreError> {
    if payload.len() < VECTOR_BINARY_HEADER_BYTES {
        return Err(CoreError::InvalidVector(
            "dense vector binary payload is truncated".to_owned(),
        ));
    }
    let dimensions = u16::from_le_bytes([payload[0], payload[1]]);
    let unused = u16::from_le_bytes([payload[2], payload[3]]);
    if unused != 0 {
        return Err(CoreError::InvalidVector(
            "dense vector binary payload reserved word is nonzero".to_owned(),
        ));
    }
    let dimensions = usize::from(dimensions);
    if dimensions > context_core::policy::MAX_VECTOR_DIMENSIONS {
        return Err(CoreError::InvalidVector(
            "dense vector dimensions exceed the supported maximum".to_owned(),
        ));
    }
    let expected = VECTOR_BINARY_HEADER_BYTES
        .checked_add(dimensions.saturating_mul(size_of::<f32>()))
        .ok_or_else(|| {
            CoreError::InvalidVector("dense vector payload length overflows".to_owned())
        })?;
    if payload.len() != expected {
        return Err(CoreError::InvalidVector(
            "dense vector binary payload length does not match its dimensions".to_owned(),
        ));
    }
    Ok(dimensions)
}

/// PostgreSQL dense vector wrapper backed by the framework-free core vector.
#[derive(Debug, Clone, PartialEq, PostgresType)]
#[inoutfuncs]
#[bikeshed_postgres_type_manually_impl_from_into_datum]
pub struct Vector {
    values: Vec<f32>,
}

impl Vector {
    /// Creates a SQL vector wrapper from a validated core vector.
    #[must_use]
    pub fn from_dense(vector: DenseVector) -> Self {
        Self {
            values: vector.into_values(),
        }
    }

    pub(crate) fn from_validated_values(values: Vec<f32>) -> Self {
        Self { values }
    }

    /// Returns the stored vector dimension without copying its values.
    #[must_use]
    pub(crate) fn dimension(&self) -> usize {
        self.values.len()
    }

    /// Borrows the stored values without allocating.
    #[must_use]
    pub(crate) fn as_slice(&self) -> &[f32] {
        &self.values
    }

    /// Converts this SQL wrapper into the core vector type.
    ///
    /// # Errors
    ///
    /// Returns [`context_core::Error::InvalidVector`] if the stored values are invalid.
    pub fn to_dense(&self) -> Result<DenseVector, context_core::Error> {
        DenseVector::new(self.values.clone())
    }
}

impl InOutFuncs for Vector {
    fn input(input: &CStr) -> Self {
        let text = match input.to_str() {
            Ok(text) => text,
            Err(_) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION,
                "invalid vector input: expected UTF-8 text",
            ),
        };

        match text.parse::<DenseVector>() {
            Ok(vector) => Self::from_dense(vector),
            Err(error) => raise_core_error(error),
        }
    }

    fn output(&self, buffer: &mut pgrx::StringInfo) {
        match self.to_dense() {
            Ok(vector) => buffer.push_str(&vector.to_string()),
            Err(error) => raise_core_error(error),
        }
    }
}

pgrx::extension_sql!(
    r#"
CREATE CAST (real[] AS vector)
    WITH FUNCTION pgcontext.vector_from_real_array(real[])
    AS ASSIGNMENT;

CREATE CAST (integer[] AS vector)
    WITH FUNCTION pgcontext.vector_from_integer_array(integer[]);

CREATE CAST (double precision[] AS vector)
    WITH FUNCTION pgcontext.vector_from_double_array(double precision[]);

CREATE CAST (vector AS real[])
    WITH FUNCTION pgcontext.vector_to_real_array(vector)
    AS ASSIGNMENT;
"#,
    name = "create_real_array_vector_casts",
    requires = [
        Vector,
        vector_from_real_array,
        vector_from_integer_array,
        vector_from_double_array,
        vector_to_real_array
    ]
);

pgrx::extension_sql!(
    r#"
CREATE OPERATOR pgcontext.<#> (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext._negative_inner_product_fast,
    COMMUTATOR = OPERATOR(pgcontext.<#>)
);

CREATE OPERATOR pgcontext.<=> (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext._cosine_distance_fast,
    COMMUTATOR = OPERATOR(pgcontext.<=>)
);

CREATE OPERATOR pgcontext.<+> (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext._l1_distance_fast,
    COMMUTATOR = OPERATOR(pgcontext.<+>)
);
"#,
    name = "create_vector_distance_operators",
    requires = [Vector, "create_vector_fast_distance_functions"]
);

pgrx::extension_sql!(
    r#"
CREATE OPERATOR pgcontext.< (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext.vector_lt,
    COMMUTATOR = OPERATOR(pgcontext.>),
    NEGATOR = OPERATOR(pgcontext.>=)
);

CREATE OPERATOR pgcontext.<= (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext.vector_le,
    COMMUTATOR = OPERATOR(pgcontext.>=),
    NEGATOR = OPERATOR(pgcontext.>)
);

CREATE OPERATOR pgcontext.= (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext.vector_eq,
    COMMUTATOR = OPERATOR(pgcontext.=),
    NEGATOR = OPERATOR(pgcontext.<>)
);

CREATE OPERATOR pgcontext.<> (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext.vector_ne,
    COMMUTATOR = OPERATOR(pgcontext.<>),
    NEGATOR = OPERATOR(pgcontext.=)
);

CREATE OPERATOR pgcontext.>= (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext.vector_ge,
    COMMUTATOR = OPERATOR(pgcontext.<=),
    NEGATOR = OPERATOR(pgcontext.<)
);

CREATE OPERATOR pgcontext.> (
    LEFTARG = vector,
    RIGHTARG = vector,
    FUNCTION = pgcontext.vector_gt,
    COMMUTATOR = OPERATOR(pgcontext.<),
    NEGATOR = OPERATOR(pgcontext.<=)
);

CREATE OPERATOR CLASS pgcontext.vector_ops
    DEFAULT FOR TYPE vector USING btree AS
    OPERATOR 1 pgcontext.< (vector, vector),
    OPERATOR 2 pgcontext.<= (vector, vector),
    OPERATOR 3 pgcontext.= (vector, vector),
    OPERATOR 4 pgcontext.>= (vector, vector),
    OPERATOR 5 pgcontext.> (vector, vector),
    FUNCTION 1 pgcontext.vector_cmp(vector, vector);
"#,
    name = "create_vector_comparison_operators",
    requires = [
        Vector, vector_lt, vector_le, vector_eq, vector_ne, vector_ge, vector_gt, vector_cmp
    ]
);

pgrx::extension_sql!(
    r#"
CREATE AGGREGATE pgcontext.sum(vector) (
    SFUNC = pgcontext.vector_sum_transition,
    STYPE = real[],
    FINALFUNC = pgcontext.vector_sum_final
);

CREATE AGGREGATE pgcontext.avg(vector) (
    SFUNC = pgcontext.vector_sum_transition,
    STYPE = real[],
    FINALFUNC = pgcontext.vector_avg_final
);
"#,
    name = "create_vector_aggregates",
    requires = [
        Vector,
        vector_sum_transition,
        vector_sum_final,
        vector_avg_final
    ]
);

/// Converts a PostgreSQL `real[]` array into a dense vector.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_from_real_array(values: Vec<f32>) -> Vector {
    match DenseVector::new(values) {
        Ok(vector) => Vector::from_dense(vector),
        Err(error) => raise_core_error(error),
    }
}

/// Converts a PostgreSQL `integer[]` array into a dense vector.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_from_integer_array(values: Vec<i32>) -> Vector {
    let values = exact_f32_values_from_i32(values).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            "integer array value cannot be represented exactly as a dense vector element",
        )
    });
    vector_from_real_array(values)
}

/// Converts a PostgreSQL `double precision[]` array into a dense vector.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_from_double_array(values: Vec<f64>) -> Vector {
    let values = exact_f32_values_from_f64(values).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            "double precision array value cannot be represented exactly as a dense vector element",
        )
    });
    vector_from_real_array(values)
}

/// Converts a dense vector into a PostgreSQL `real[]` array.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_to_real_array(vector: Vector) -> Vec<f32> {
    match vector.to_dense() {
        Ok(vector) => vector.as_slice().to_vec(),
        Err(error) => raise_core_error(error),
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the result is accepted only when widening it back reproduces the original integer"
)]
fn exact_f32_values_from_i32(values: Vec<i32>) -> Option<Vec<f32>> {
    values
        .into_iter()
        .map(|value| {
            let narrowed = value as f32;
            (f64::from(narrowed) == f64::from(value)).then_some(narrowed)
        })
        .collect()
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the result is accepted only when widening it back reproduces the original float"
)]
fn exact_f32_values_from_f64(values: Vec<f64>) -> Option<Vec<f32>> {
    values
        .into_iter()
        .map(|value| {
            let narrowed = value as f32;
            (narrowed.is_finite() && f64::from(narrowed) == value).then_some(narrowed)
        })
        .collect()
}

/// Accumulates one dense vector into the aggregate state.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_sum_transition(state: Option<Vec<f32>>, value: Option<Vector>) -> Option<Vec<f32>> {
    let Some(value) = value else {
        return state;
    };
    let value = match value.to_dense() {
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
            "vector aggregate state is empty",
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

/// Finalizes dense vector sum aggregates.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_sum_final(state: Vec<f32>) -> Vector {
    vector_from_aggregate_state(state, AggregateFinal::Sum)
}

/// Finalizes dense vector average aggregates.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_avg_final(state: Vec<f32>) -> Vector {
    vector_from_aggregate_state(state, AggregateFinal::Average)
}

/// Compares dense vectors for btree ordering.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_cmp(left: Vector, right: Vector) -> i32 {
    match compare_vectors(left, right) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }
}

/// Returns whether the left vector is less than the right vector.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_lt(left: Vector, right: Vector) -> bool {
    compare_vectors(left, right).is_lt()
}

/// Returns whether the left vector is less than or equal to the right vector.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_le(left: Vector, right: Vector) -> bool {
    compare_vectors(left, right).is_le()
}

/// Returns whether two vectors have identical dense values.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_eq(left: Vector, right: Vector) -> bool {
    compare_vectors(left, right).is_eq()
}

/// Returns whether two vectors have different dense values.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_ne(left: Vector, right: Vector) -> bool {
    !compare_vectors(left, right).is_eq()
}

/// Returns whether the left vector is greater than or equal to the right vector.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_ge(left: Vector, right: Vector) -> bool {
    compare_vectors(left, right).is_ge()
}

/// Returns whether the left vector is greater than the right vector.
#[pg_extern(immutable, parallel_safe)]
pub fn vector_gt(left: Vector, right: Vector) -> bool {
    compare_vectors(left, right).is_gt()
}

fn compare_vectors(left: Vector, right: Vector) -> Ordering {
    let left = match left.to_dense() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };
    let right = match right.to_dense() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };

    for (left, right) in left.as_slice().iter().zip(right.as_slice()) {
        match left.partial_cmp(right) {
            Some(Ordering::Equal) => {}
            Some(ordering) => return ordering,
            None => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "cannot compare non-finite vector values",
            ),
        }
    }
    left.dimension().cmp(&right.dimension())
}

#[derive(Debug, Copy, Clone)]
enum AggregateFinal {
    Sum,
    Average,
}

fn vector_from_aggregate_state(mut state: Vec<f32>, aggregate: AggregateFinal) -> Vector {
    if state.len() < 2 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "vector aggregate state is missing dimensions",
        );
    }
    let count = state.remove(0);
    if count <= 0.0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            "vector aggregate state has no rows",
        );
    }
    if matches!(aggregate, AggregateFinal::Average) {
        for value in &mut state {
            *value /= count;
        }
    }

    match DenseVector::new(state) {
        Ok(vector) => Vector::from_dense(vector),
        Err(error) => raise_core_error(error),
    }
}

/// Returns the number of dimensions in a dense vector.
#[pg_extern(immutable, parallel_safe)]
#[must_use]
pub fn vector_dims(vector: Vector) -> i32 {
    match vector.to_dense() {
        Ok(vector) => match i32::try_from(vector.dimension()) {
            Ok(dimension) => dimension,
            Err(_) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                "vector dimension exceeds PostgreSQL integer range",
            ),
        },
        Err(error) => raise_core_error(error),
    }
}

/// Returns L2 distance between two dense vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn l2_distance(left: Vector, right: Vector) -> f32 {
    distance(left, right, DistanceMetric::L2)
}

/// Returns inner product between two dense vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn inner_product(left: Vector, right: Vector) -> f32 {
    distance(left, right, DistanceMetric::InnerProduct)
}

/// Returns negative inner product for pgvector-compatible ascending sort order.
#[pg_extern(immutable, parallel_safe)]
pub fn negative_inner_product(left: Vector, right: Vector) -> f32 {
    -distance(left, right, DistanceMetric::InnerProduct)
}

/// Returns cosine distance between two dense vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn cosine_distance(left: Vector, right: Vector) -> f32 {
    distance(left, right, DistanceMetric::Cosine)
}

/// Returns L1 distance between two dense vectors.
#[pg_extern(immutable, parallel_safe)]
pub fn l1_distance(left: Vector, right: Vector) -> f32 {
    distance(left, right, DistanceMetric::L1)
}

/// Returns exact top-k dense-vector search results.
///
/// The `point_ids` and `vectors` arrays describe the candidate set. Results are
/// sorted by ascending metric score and then by ascending point id.
#[pg_extern(immutable, parallel_safe)]
pub fn search(
    query: Vector,
    point_ids: Vec<i64>,
    vectors: Vec<Vector>,
    metric: String,
    limit: i32,
) -> TableIterator<'static, (name!(point_id, i64), name!(score, f32))> {
    let query = match query.to_dense() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };
    let metric = parse_metric(&metric);
    let limit = search_limit_from_sql(limit);
    let candidates = search_items_from_sql(point_ids, vectors);

    let results = exact_top_k(&query, &candidates, metric, limit)
        .map(|result| match result {
            Ok(point) => {
                let point_id = match i64::try_from(point.point_id()) {
                    Ok(point_id) => point_id,
                    Err(_) => raise_sql_error(
                        PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                        "point id exceeds PostgreSQL bigint range",
                    ),
                };
                (point_id, point.score())
            }
            Err(error) => raise_core_error(error),
        })
        .collect::<Vec<_>>();

    TableIterator::new(results)
}

/// Exact-reranks approximate or quantized candidate rows by original vectors.
///
/// Candidate order is treated only as ANN input. Results are sorted by exact
/// metric score against `original_vectors` and then by ascending point id.
#[pg_extern(immutable, parallel_safe)]
pub fn rerank_quantized_candidates(
    query: Vector,
    point_ids: Vec<i64>,
    original_vectors: Vec<Vector>,
    metric: String,
    limit: i32,
) -> TableIterator<'static, (name!(point_id, i64), name!(score, f32))> {
    let query = match query.to_dense() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    };
    let metric = parse_metric(&metric);
    let limit = search_limit_from_sql(limit);
    let candidates = rerank_candidates_from_sql(point_ids, original_vectors);

    let results = match rerank_by_original_vectors(&query, &candidates, metric, limit) {
        Ok(results) => results,
        Err(error) => raise_index_error(error),
    };
    let rows = results
        .into_iter()
        .map(|result| {
            let point_id = match i64::try_from(result.point_id()) {
                Ok(point_id) => point_id,
                Err(_) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                    "point id exceeds PostgreSQL bigint range",
                ),
            };
            (point_id, result.score())
        })
        .collect::<Vec<_>>();

    TableIterator::new(rows)
}

/// Exact-reranks multi-vector candidates with late-interaction MaxSim scoring.
///
/// `candidate_offsets` partitions `candidate_vectors` by point using zero-based
/// offsets. Its length must be `point_ids.len() + 1`, start at `0`, and end at
/// `candidate_vectors.len()`. Scores are the sum, over each query vector, of
/// the best inner product against that point's candidate vectors. Higher scores
/// rank first; ties use ascending point id.
#[pg_extern(immutable, parallel_safe)]
pub fn rerank_late_interaction(
    query_vectors: Vec<Vector>,
    point_ids: Vec<i64>,
    candidate_vectors: Vec<Vector>,
    candidate_offsets: Vec<i32>,
    limit: i32,
) -> TableIterator<'static, (name!(point_id, i64), name!(score, f32))> {
    let query_vectors = dense_vectors_from_sql("query_vectors", query_vectors);
    let candidate_vectors = dense_vectors_from_sql("candidate_vectors", candidate_vectors);
    let point_ids = point_ids_from_sql(point_ids);
    let ranges = candidate_ranges_from_sql(&point_ids, candidate_offsets, candidate_vectors.len());
    let limit = search_limit_from_sql(limit);

    enforce_late_interaction_budget(query_vectors.len(), candidate_vectors.len());
    let mut rows = point_ids
        .into_iter()
        .zip(ranges)
        .map(|(point_id, range)| {
            let score =
                late_interaction_score(&query_vectors, &candidate_vectors[range.start..range.end]);
            let point_id = match i64::try_from(point_id) {
                Ok(point_id) => point_id,
                Err(_) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
                    "point id exceeds PostgreSQL bigint range",
                ),
            };
            (point_id, score)
        })
        .collect::<Vec<_>>();

    rows.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    rows.truncate(limit.get());

    TableIterator::new(rows)
}

fn distance(left: Vector, right: Vector, metric: DistanceMetric) -> f32 {
    match metric.distance_slices(left.as_slice(), right.as_slice()) {
        Ok(distance) => distance,
        Err(error) => raise_core_error(error),
    }
}

fn parse_metric(metric: &str) -> DistanceMetric {
    crate::domain_types::distance_metric_from_query(metric, "")
}

fn search_limit_from_sql(limit: i32) -> SearchLimit {
    let limit = match usize::try_from(limit) {
        Ok(limit) => limit,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid search limit: {limit}"),
        ),
    };

    match SearchLimit::new(limit) {
        Ok(limit) => limit,
        Err(error) => raise_core_error(error),
    }
}

fn search_items_from_sql(point_ids: Vec<i64>, vectors: Vec<Vector>) -> Vec<ExactSearchItem> {
    if point_ids.len() != vectors.len() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "point_ids and vectors must have the same length: got {} ids and {} vectors",
                point_ids.len(),
                vectors.len()
            ),
        );
    }

    point_ids
        .into_iter()
        .zip(vectors)
        .map(|(point_id, vector)| {
            let point_id = match u64::try_from(point_id) {
                Ok(point_id) => point_id,
                Err(_) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("point id must be non-negative: {point_id}"),
                ),
            };
            let vector = match vector.to_dense() {
                Ok(vector) => vector,
                Err(error) => raise_core_error(error),
            };
            ExactSearchItem::new(point_id, vector)
        })
        .collect()
}

fn dense_vectors_from_sql(label: &'static str, vectors: Vec<Vector>) -> Vec<DenseVector> {
    if vectors.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{label} must not be empty"),
        );
    }
    vectors
        .into_iter()
        .map(|vector| match vector.to_dense() {
            Ok(vector) => vector,
            Err(error) => raise_core_error(error),
        })
        .collect()
}

fn point_ids_from_sql(point_ids: Vec<i64>) -> Vec<u64> {
    if point_ids.is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "late interaction requires at least one candidate point",
        );
    }
    point_ids
        .into_iter()
        .map(|point_id| match u64::try_from(point_id) {
            Ok(point_id) => point_id,
            Err(_) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("point id must be non-negative: {point_id}"),
            ),
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
struct CandidateVectorRange {
    start: usize,
    end: usize,
}

fn candidate_ranges_from_sql(
    point_ids: &[u64],
    offsets: Vec<i32>,
    candidate_vector_count: usize,
) -> Vec<CandidateVectorRange> {
    if offsets.len() != point_ids.len() + 1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "candidate_offsets must have one more entry than point_ids: got {} offsets and {} point ids",
                offsets.len(),
                point_ids.len()
            ),
        );
    }

    let offsets = offsets
        .into_iter()
        .map(|offset| match usize::try_from(offset) {
            Ok(offset) => offset,
            Err(_) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("candidate offset must be non-negative: {offset}"),
            ),
        })
        .collect::<Vec<_>>();

    if offsets.first().copied() != Some(0) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "candidate_offsets must start at 0",
        );
    }
    if offsets.last().copied() != Some(candidate_vector_count) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "candidate_offsets must end at candidate vector count {candidate_vector_count}"
            ),
        );
    }

    let mut ranges = Vec::with_capacity(point_ids.len());
    for pair in offsets.windows(2) {
        let [start, end] = pair else {
            unreachable!("windows(2) always yields two offsets")
        };
        if end < start {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "candidate_offsets must be non-decreasing",
            );
        }
        if end == start {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                "each late-interaction candidate point must have at least one vector",
            );
        }
        ranges.push(CandidateVectorRange {
            start: *start,
            end: *end,
        });
    }

    ranges
}

fn rerank_candidates_from_sql(
    point_ids: Vec<i64>,
    original_vectors: Vec<Vector>,
) -> Vec<RerankCandidate> {
    if point_ids.len() != original_vectors.len() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "quantized rerank requires one original vector per candidate point: got {} ids and {} vectors",
                point_ids.len(),
                original_vectors.len()
            ),
        );
    }

    point_ids
        .into_iter()
        .zip(original_vectors)
        .map(|(point_id, vector)| {
            let point_id = match u64::try_from(point_id) {
                Ok(point_id) => point_id,
                Err(_) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                    format!("point id must be non-negative: {point_id}"),
                ),
            };
            let vector = match vector.to_dense() {
                Ok(vector) => vector,
                Err(error) => raise_core_error(error),
            };
            RerankCandidate::with_original(point_id, vector)
        })
        .collect()
}

fn raise_index_error(error: HnswError) -> ! {
    raise_context_error(error.context_error(), error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        decode_vector_payload, encode_vector_payload, exact_f32_values_from_f64,
        exact_f32_values_from_i32, size_of,
    };
    use crate::vector_datum::decode_vector_payload_view;

    #[test]
    fn vector_binary_payload_is_compact_and_round_trips() -> Result<(), context_core::Error> {
        let values = [1.0, -2.5, 3.25];
        let payload = encode_vector_payload(&values)?;

        assert_eq!(payload.len(), 4 + values.len() * size_of::<f32>());
        assert_eq!(decode_vector_payload(&payload)?, values);
        assert!(decode_vector_payload(&payload[..payload.len() - 1]).is_err());

        Ok(())
    }

    /// Pins the payload layout to pgvector's `struct Vector` byte-for-byte:
    /// `{ int16 dim; int16 unused; float4 x[dim] }`, little-endian. This
    /// fixture is the coexist/replacement compatibility contract — if it
    /// ever fails, pgContext can no longer bind to pgvector's type and the
    /// change must be rejected, not accommodated.
    #[test]
    fn vector_binary_payload_matches_pgvector_layout_fixture() -> Result<(), context_core::Error> {
        let values = [1.0_f32, 2.0, 3.0];
        let mut fixture: Vec<u8> = vec![0x03, 0x00, 0x00, 0x00];
        fixture.extend_from_slice(&1.0_f32.to_le_bytes());
        fixture.extend_from_slice(&2.0_f32.to_le_bytes());
        fixture.extend_from_slice(&3.0_f32.to_le_bytes());

        assert_eq!(encode_vector_payload(&values)?, fixture);
        assert_eq!(decode_vector_payload(&fixture)?, values);

        // The reserved word is required zero: nonzero means "not a vector
        // we understand" and must fail closed.
        let mut reserved_set = fixture.clone();
        reserved_set[2] = 1;
        assert!(decode_vector_payload(&reserved_set).is_err());
        Ok(())
    }

    #[test]
    fn vector_binary_payload_view_borrows_aligned_values_without_copying()
    -> Result<(), context_core::Error> {
        let values = [1.0, -2.5, 3.25];
        let payload = encode_vector_payload(&values)?;
        let view = decode_vector_payload_view(&payload)?;

        assert_eq!(view.values(), values);
        assert_eq!(view.values().as_ptr().cast::<u8>(), payload[4..].as_ptr());

        Ok(())
    }

    #[test]
    fn vector_binary_payload_view_rejects_truncation_and_misalignment()
    -> Result<(), context_core::Error> {
        let payload = encode_vector_payload(&[1.0, 2.0])?;

        assert!(decode_vector_payload_view(&payload[..payload.len() - 1]).is_err());
        let mut misaligned = vec![0_u8];
        misaligned.extend_from_slice(&payload);
        assert!(decode_vector_payload_view(&misaligned[1..]).is_err());

        Ok(())
    }

    #[test]
    fn exact_integer_narrowing_rejects_precision_loss() {
        assert_eq!(
            exact_f32_values_from_i32(vec![16_777_216]),
            Some(vec![16_777_216.0])
        );
        assert_eq!(exact_f32_values_from_i32(vec![16_777_217]), None);
    }

    #[test]
    fn exact_double_narrowing_rejects_rounding_and_non_finite_values() {
        assert_eq!(exact_f32_values_from_f64(vec![1.5]), Some(vec![1.5]));
        assert_eq!(exact_f32_values_from_f64(vec![16_777_217.0]), None);
        assert_eq!(exact_f32_values_from_f64(vec![f64::INFINITY]), None);
        assert_eq!(exact_f32_values_from_f64(vec![f64::NAN]), None);
    }
}
