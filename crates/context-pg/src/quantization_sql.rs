//! SQL-facing quantization helpers.

use context_core::DenseVector;
use context_index::{
    HnswError, ProductCodebook, ProductQuantizedVector, ProductQuantizer, ScalarQuantizedVector,
    ScalarQuantizer, binary_quantize as index_binary_quantize,
};
use pgrx::JsonB;
use pgrx::prelude::*;
use serde_json::Value;

use crate::error::{raise_context_error, raise_core_error, raise_sql_error};
use crate::vector::Vector;
use crate::vector_variants::BitVec;

/// Converts a dense vector to a binary sign-code bit vector.
#[pg_extern(schema = "pgcontext", immutable, parallel_safe)]
pub fn binary_quantize(vector: Vector) -> BitVec {
    let vector = vector_to_core(vector);
    match index_binary_quantize(&vector) {
        Ok(vector) => BitVec::from_bit(vector),
        Err(error) => raise_index_error(error),
    }
}

/// Quantizes a dense vector to scalar byte codes.
#[pg_extern(schema = "pgcontext", immutable, parallel_safe)]
pub fn scalar_quantize(vector: Vector, min: f32, max: f32, levels: i32) -> Vec<u8> {
    let vector = vector_to_core(vector);
    let quantizer = scalar_quantizer_from_sql(min, max, levels);

    match quantizer.quantize(&vector) {
        Ok(vector) => vector.codes().to_vec(),
        Err(error) => raise_index_error(error),
    }
}

/// Reconstructs a dense vector from scalar byte codes.
#[pg_extern(schema = "pgcontext", immutable, parallel_safe)]
pub fn scalar_reconstruct(codes: Vec<u8>, min: f32, max: f32, levels: i32) -> Vector {
    let quantizer = scalar_quantizer_from_sql(min, max, levels);
    let codes = match ScalarQuantizedVector::new(codes) {
        Ok(codes) => codes,
        Err(error) => raise_index_error(error),
    };

    match quantizer.reconstruct(&codes) {
        Ok(vector) => Vector::from_dense(vector),
        Err(error) => raise_index_error(error),
    }
}

/// Quantizes a dense vector using JSONB product-quantization codebooks.
#[pg_extern(schema = "pgcontext", immutable, parallel_safe)]
pub fn product_quantize(vector: Vector, subvector_dimensions: i32, codebooks: JsonB) -> Vec<u8> {
    let vector = vector_to_core(vector);
    let quantizer = product_quantizer_from_sql(subvector_dimensions, codebooks);

    match quantizer.quantize(&vector) {
        Ok(vector) => vector.codes().to_vec(),
        Err(error) => raise_index_error(error),
    }
}

/// Reconstructs a dense vector from product byte codes and JSONB codebooks.
#[pg_extern(schema = "pgcontext", immutable, parallel_safe)]
pub fn product_reconstruct(codes: Vec<u8>, subvector_dimensions: i32, codebooks: JsonB) -> Vector {
    let quantizer = product_quantizer_from_sql(subvector_dimensions, codebooks);
    let codes = match ProductQuantizedVector::new(codes) {
        Ok(codes) => codes,
        Err(error) => raise_index_error(error),
    };

    match quantizer.reconstruct(&codes) {
        Ok(vector) => Vector::from_dense(vector),
        Err(error) => raise_index_error(error),
    }
}

fn vector_to_core(vector: Vector) -> DenseVector {
    match vector.to_dense() {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    }
}

fn scalar_quantizer_from_sql(min: f32, max: f32, levels: i32) -> ScalarQuantizer {
    let levels = match u16::try_from(levels) {
        Ok(levels) => levels,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid scalar quantization levels: {levels}"),
        ),
    };

    match ScalarQuantizer::new(min, max, levels) {
        Ok(quantizer) => quantizer,
        Err(error) => raise_index_error(error),
    }
}

fn product_quantizer_from_sql(subvector_dimensions: i32, codebooks: JsonB) -> ProductQuantizer {
    let subvector_dimensions = match usize::try_from(subvector_dimensions) {
        Ok(dimensions) => dimensions,
        Err(_) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("invalid product quantization subvector dimensions: {subvector_dimensions}"),
        ),
    };
    let codebooks = product_codebooks_from_json(codebooks);

    match ProductQuantizer::new(subvector_dimensions, codebooks) {
        Ok(quantizer) => quantizer,
        Err(error) => raise_index_error(error),
    }
}

fn product_codebooks_from_json(codebooks: JsonB) -> Vec<ProductCodebook> {
    let Value::Array(codebooks) = codebooks.0 else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "product quantization codebooks must be a JSON array",
        );
    };

    codebooks
        .into_iter()
        .enumerate()
        .map(|(codebook_index, codebook)| product_codebook_from_json(codebook_index, codebook))
        .collect()
}

fn product_codebook_from_json(codebook_index: usize, codebook: Value) -> ProductCodebook {
    let Value::Array(centroids) = codebook else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("product quantization codebook {codebook_index} must be a JSON array"),
        );
    };
    let centroids = centroids
        .into_iter()
        .enumerate()
        .map(|(centroid_index, centroid)| {
            product_centroid_from_json(codebook_index, centroid_index, centroid)
        })
        .collect::<Vec<_>>();

    match ProductCodebook::new(centroids) {
        Ok(codebook) => codebook,
        Err(error) => raise_index_error(error),
    }
}

fn product_centroid_from_json(
    codebook_index: usize,
    centroid_index: usize,
    centroid: Value,
) -> DenseVector {
    let Value::Array(values) = centroid else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "product quantization centroid {codebook_index}.{centroid_index} must be a JSON array"
            ),
        );
    };
    let values = values
        .into_iter()
        .enumerate()
        .map(|(value_index, value)| {
            product_centroid_value_from_json(codebook_index, centroid_index, value_index, value)
        })
        .collect::<Vec<_>>();

    match DenseVector::new(values) {
        Ok(vector) => vector,
        Err(error) => raise_core_error(error),
    }
}

fn product_centroid_value_from_json(
    codebook_index: usize,
    centroid_index: usize,
    value_index: usize,
    value: Value,
) -> f32 {
    let Some(value) = value.as_f64() else {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "product quantization centroid {codebook_index}.{centroid_index}.{value_index} must be a number"
            ),
        );
    };
    if !value.is_finite() || value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_NUMERIC_VALUE_OUT_OF_RANGE,
            format!(
                "product quantization centroid {codebook_index}.{centroid_index}.{value_index} is outside f32 range"
            ),
        );
    }

    f64_to_checked_f32(value)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "caller validates the finite value is inside the f32 range before centroid storage"
)]
fn f64_to_checked_f32(value: f64) -> f32 {
    value as f32
}

fn raise_index_error(error: HnswError) -> ! {
    raise_context_error(error.context_error(), error.to_string())
}
