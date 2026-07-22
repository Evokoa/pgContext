//! Build-side quantization policy adaptation for mmap HNSW artifacts.

use context_core::DenseVector;
use context_index::{TrainedQuantizer, train_product_quantizer, train_scalar_quantizer};
use context_storage::{
    HnswGraphArtifactRecord, HnswGraphQuantization, HnswGraphQuantizationCodebook,
};
use serde_json::Value;

const DEFAULT_SCALAR_LEVELS: u16 = 256;
const DEFAULT_PQ_SUBVECTOR_DIMENSIONS: usize = 8;
const DEFAULT_PQ_CENTROIDS: usize = 16;
const DEFAULT_PQ_ITERATIONS: usize = 8;

pub(super) fn quantize_graph_records(
    records: &[HnswGraphArtifactRecord],
    options: &Value,
) -> Result<Option<HnswGraphQuantization>, String> {
    let mode = options
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("none");
    if mode == "none" {
        return Ok(None);
    }
    let sample = records
        .iter()
        .map(|record| record.vector().clone())
        .collect::<Vec<_>>();
    let dimensions = sample
        .first()
        .map(DenseVector::dimension)
        .ok_or_else(|| "cannot train quantization for an empty graph".to_owned())?;
    let trained = match mode {
        "binary" => TrainedQuantizer::binary(dimensions),
        "scalar" | "sq8" => {
            let levels = option_u16(options, "levels")?.unwrap_or(DEFAULT_SCALAR_LEVELS);
            let observed =
                train_scalar_quantizer(&sample, levels, None).map_err(|error| error.to_string())?;
            let observed = observed
                .scalar()
                .ok_or_else(|| "scalar training returned a non-scalar codebook".to_owned())?;
            let minimum = option_f32(options, "min")?.unwrap_or(observed.min());
            let maximum = option_f32(options, "max")?.unwrap_or(observed.max());
            train_scalar_quantizer(&sample, levels, Some((minimum, maximum)))
        }
        "pq" => {
            let subvector_dimensions = option_usize(options, "subvector_dimensions")?
                .unwrap_or_else(|| default_subvector_dimensions(dimensions));
            let centroid_count = DEFAULT_PQ_CENTROIDS.min(sample.len());
            train_product_quantizer(
                &sample,
                subvector_dimensions,
                centroid_count,
                DEFAULT_PQ_ITERATIONS,
            )
        }
        unsupported => return Err(format!("unsupported quantization mode: {unsupported}")),
    }
    .map_err(|error| error.to_string())?;
    let codebook = persisted_codebook(&trained);
    let codes = sample
        .iter()
        .map(|vector| trained.quantize(vector).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(HnswGraphQuantization::new(codebook, codes)))
}

fn persisted_codebook(trained: &TrainedQuantizer) -> HnswGraphQuantizationCodebook {
    match trained {
        TrainedQuantizer::Binary { dimensions } => HnswGraphQuantizationCodebook::Binary {
            dimensions: *dimensions,
        },
        TrainedQuantizer::Scalar {
            quantizer,
            dimensions,
        } => HnswGraphQuantizationCodebook::Scalar {
            dimensions: *dimensions,
            minimum: quantizer.min(),
            maximum: quantizer.max(),
            levels: quantizer.levels(),
        },
        TrainedQuantizer::Product(quantizer) => HnswGraphQuantizationCodebook::Product {
            dimensions: trained.dimensions(),
            subvector_dimensions: quantizer.subvector_dimensions(),
            codebooks: quantizer
                .codebooks()
                .iter()
                .map(|codebook| codebook.centroids().to_vec())
                .collect(),
        },
    }
}

fn default_subvector_dimensions(dimensions: usize) -> usize {
    (1..=DEFAULT_PQ_SUBVECTOR_DIMENSIONS.min(dimensions))
        .rev()
        .find(|candidate| dimensions.is_multiple_of(*candidate))
        .unwrap_or(1)
}

fn option_u16(options: &Value, key: &'static str) -> Result<Option<u16>, String> {
    options
        .get(key)
        .map(|value| {
            let value = value
                .as_u64()
                .ok_or_else(|| format!("quantization option {key} must be an integer"))?;
            u16::try_from(value)
                .map_err(|_| format!("quantization option {key} exceeds u16: {value}"))
        })
        .transpose()
}

fn option_usize(options: &Value, key: &'static str) -> Result<Option<usize>, String> {
    options
        .get(key)
        .map(|value| {
            let value = value
                .as_u64()
                .ok_or_else(|| format!("quantization option {key} must be an integer"))?;
            usize::try_from(value)
                .map_err(|_| format!("quantization option {key} exceeds usize: {value}"))
        })
        .transpose()
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "metadata validation bounds finite JSON numbers to f32-compatible quantizer policy"
)]
fn option_f32(options: &Value, key: &'static str) -> Result<Option<f32>, String> {
    options
        .get(key)
        .map(|value| {
            let value = value
                .as_f64()
                .ok_or_else(|| format!("quantization option {key} must be numeric"))?;
            let converted = value as f32;
            if converted.is_finite() {
                Ok(converted)
            } else {
                Err(format!("quantization option {key} exceeds f32"))
            }
        })
        .transpose()
}
