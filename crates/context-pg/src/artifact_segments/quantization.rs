//! Build-side quantization policy adaptation for mmap HNSW artifacts.

use context_codec::{
    QuantizedCodebook, TrainedQuantizer, train_product_quantizer, train_scalar_quantizer,
    validate_retrieval_combination,
};
use context_core::{DenseVector, DistanceMetric, IndexKind, VectorRepresentation};
use context_storage::{HnswGraphArtifactRecord, HnswGraphQuantization};
use serde_json::Value;

use crate::vector_metadata_validation::{QuantizationConfig, parse_quantization_options};

const DEFAULT_PQ_SUBVECTOR_DIMENSIONS: usize = 8;
const DEFAULT_PQ_CENTROIDS: usize = 16;
const DEFAULT_PQ_ITERATIONS: usize = 8;
const MAX_QUANTIZATION_TRAINING_SAMPLE: usize = 4_096;

pub(super) fn quantize_graph_records(
    records: &[HnswGraphArtifactRecord],
    options: &Value,
    metric: DistanceMetric,
) -> Result<Option<HnswGraphQuantization>, String> {
    let config = parse_quantization_options(options)?;
    if config == QuantizationConfig::Plain {
        return Ok(None);
    }
    let codec = config.codec_kind();
    validate_retrieval_combination(VectorRepresentation::Dense, metric, IndexKind::Hnsw, codec)
        .map_err(|error| error.to_string())?;
    let sample = deterministic_training_sample(records);
    let dimensions = sample
        .first()
        .map(DenseVector::dimension)
        .ok_or_else(|| "cannot train quantization for an empty graph".to_owned())?;
    let trained = match config {
        QuantizationConfig::Binary => TrainedQuantizer::binary(dimensions),
        QuantizationConfig::Scalar {
            levels,
            minimum,
            maximum,
        } => {
            let observed =
                train_scalar_quantizer(&sample, levels, None).map_err(|error| error.to_string())?;
            let observed = observed
                .scalar()
                .ok_or_else(|| "scalar training returned a non-scalar codebook".to_owned())?;
            let minimum = minimum.unwrap_or(observed.min());
            let maximum = maximum.unwrap_or(observed.max());
            train_scalar_quantizer(&sample, levels, Some((minimum, maximum)))
        }
        QuantizationConfig::Product {
            subvector_dimensions,
        } => {
            let subvector_dimensions =
                subvector_dimensions.unwrap_or_else(|| default_subvector_dimensions(dimensions));
            let centroid_count = DEFAULT_PQ_CENTROIDS.min(sample.len());
            train_product_quantizer(
                &sample,
                subvector_dimensions,
                centroid_count,
                DEFAULT_PQ_ITERATIONS,
            )
        }
        QuantizationConfig::Plain => unreachable!("plain mode returns before codec training"),
    }
    .map_err(|error| error.to_string())?;
    let codebook = persisted_codebook(&trained);
    let codes = records
        .iter()
        .map(|record| {
            trained
                .quantize(record.vector())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(HnswGraphQuantization::new(codebook, codes)))
}

pub(super) fn validate_graph_quantization_policy(
    records: &[HnswGraphArtifactRecord],
    actual: Option<&HnswGraphQuantization>,
    options: &Value,
    metric: DistanceMetric,
) -> Result<(), String> {
    let expected = quantize_graph_records(records, options, metric)?;
    if expected.as_ref() == actual {
        Ok(())
    } else {
        let expected_mode = quantization_mode(expected.as_ref());
        let actual_mode = quantization_mode(actual);
        Err(format!(
            "persisted codebook/codes do not match registered policy: expected {expected_mode}, got {actual_mode}"
        ))
    }
}

fn quantization_mode(quantization: Option<&HnswGraphQuantization>) -> &'static str {
    match quantization.map(HnswGraphQuantization::codebook) {
        None => "none",
        Some(QuantizedCodebook::Binary { .. }) => "binary",
        Some(QuantizedCodebook::Scalar { .. }) => "scalar",
        Some(QuantizedCodebook::Product { .. }) => "pq",
    }
}

fn deterministic_training_sample(records: &[HnswGraphArtifactRecord]) -> Vec<DenseVector> {
    let sample_count = records.len().min(MAX_QUANTIZATION_TRAINING_SAMPLE);
    (0..sample_count)
        .map(|sample_index| {
            let record_index = sample_index.saturating_mul(records.len()) / sample_count;
            records[record_index].vector().clone()
        })
        .collect()
}

fn persisted_codebook(trained: &TrainedQuantizer) -> QuantizedCodebook {
    match trained {
        TrainedQuantizer::Binary { dimensions } => QuantizedCodebook::Binary {
            dimensions: *dimensions,
        },
        TrainedQuantizer::Scalar {
            quantizer,
            dimensions,
        } => QuantizedCodebook::Scalar {
            dimensions: *dimensions,
            minimum: quantizer.min(),
            maximum: quantizer.max(),
            levels: quantizer.levels(),
        },
        TrainedQuantizer::Product(quantizer) => QuantizedCodebook::Product {
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
