//! Build-side quantization policy adaptation for mmap HNSW artifacts.

use context_codec::{
    CodecKind, QuantizedCodebook, TrainedCodecArtifact, validate_retrieval_combination,
};
use context_core::{DenseVector, DistanceMetric, IndexKind, VectorRepresentation};
use context_storage::{HnswGraphArtifactRecord, HnswGraphQuantization};
use serde_json::Value;

use crate::vector_metadata_validation::parse_quantization_options;

const MAX_QUANTIZATION_TRAINING_SAMPLE: usize = 4_096;

pub(super) fn quantize_graph_records(
    records: &[HnswGraphArtifactRecord],
    options: &Value,
    metric: DistanceMetric,
) -> Result<Option<HnswGraphQuantization>, String> {
    let config = parse_quantization_options(options)?;
    if config.kind() == CodecKind::Plain {
        return Ok(None);
    }
    let codec = config.kind();
    validate_retrieval_combination(VectorRepresentation::Dense, metric, IndexKind::Hnsw, codec)
        .map_err(|error| error.to_string())?;
    let sample = deterministic_training_sample(records);
    let trained =
        TrainedCodecArtifact::train(config, &sample).map_err(|error| error.to_string())?;
    let codebook = trained
        .codebook()
        .cloned()
        .ok_or_else(|| "quantized training returned no codebook".to_owned())?;
    let contiguous = trained
        .encode_refs(records.iter().map(HnswGraphArtifactRecord::vector))
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "quantized training returned no codes".to_owned())?;
    HnswGraphQuantization::new(
        trained.revision(),
        trained.reconstruction_policy(),
        codebook,
        contiguous,
    )
    .map(Some)
    .map_err(|error| error.to_string())
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
