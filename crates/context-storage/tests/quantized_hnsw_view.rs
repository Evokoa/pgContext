//! Borrowed quantized graph view and encoded-distance integration tests.

use std::error::Error;

use context_core::{DenseVector, DistanceMetric};
use context_storage::{
    HnswGraphArtifactRecord, HnswGraphQuantization, HnswGraphQuantizationCodebook,
    QuantizedHnswGraphView, encode_hnsw_graph_payload_v2,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn vector(values: &[f32]) -> TestResult<DenseVector> {
    Ok(DenseVector::new(values.to_vec())?)
}

#[test]
fn quantized_view_borrows_codes_and_neighbors() -> TestResult {
    let records = vec![
        HnswGraphArtifactRecord::new(0, 10, vector(&[-1.0, 1.0])?, vec![1]),
        HnswGraphArtifactRecord::new(1, 20, vector(&[1.0, -1.0])?, vec![0]),
    ];
    let quantization = HnswGraphQuantization::new(
        HnswGraphQuantizationCodebook::Binary { dimensions: 2 },
        vec![vec![0b10], vec![0b01]],
    );
    let payload = encode_hnsw_graph_payload_v2(&records, Some(&quantization))?;
    let view = QuantizedHnswGraphView::attach(&payload)?
        .ok_or_else(|| std::io::Error::other("payload should be quantized"))?;

    assert_eq!(view.dimensions(), 2);
    assert_eq!(view.len(), 2);
    let node = view
        .node(0)
        .ok_or_else(|| std::io::Error::other("first node should exist"))?;
    assert_eq!(node.point_id(), 10);
    assert_eq!(node.neighbors().collect::<Vec<_>>(), vec![1]);
    assert_eq!(node.code(), &[0b10]);
    assert!(node.code().as_ptr() >= payload.as_ptr());
    assert!(node.code().as_ptr() < payload[payload.len()..].as_ptr());
    Ok(())
}

#[test]
fn encoded_distance_matches_reconstruction_without_allocating_a_node_vector() -> TestResult {
    let codebook = HnswGraphQuantizationCodebook::Scalar {
        dimensions: 3,
        minimum: -2.0,
        maximum: 2.0,
        levels: 5,
    };
    let code = [0, 2, 4];
    let query = vector(&[-1.0, 1.0, 2.0])?;
    let reconstructed = codebook.reconstruct(&code)?;

    for metric in [
        DistanceMetric::L2,
        DistanceMetric::L1,
        DistanceMetric::NegativeInnerProduct,
        DistanceMetric::Cosine,
    ] {
        let encoded = codebook.approximate_distance(&query, &code, metric)?;
        let expected = metric.distance(&query, &reconstructed)?;
        assert!((encoded - expected).abs() <= f32::EPSILON * 8.0);
    }
    Ok(())
}

#[test]
fn cosine_navigation_deprioritizes_a_quantized_zero_vector() -> TestResult {
    let codebook = HnswGraphQuantizationCodebook::Product {
        dimensions: 2,
        subvector_dimensions: 2,
        codebooks: vec![vec![vector(&[0.0, 0.0])?, vector(&[1.0, 1.0])?]],
    };
    let score =
        codebook.approximate_distance(&vector(&[1.0, 0.0])?, &[0], DistanceMetric::Cosine)?;
    assert_eq!(score, f32::INFINITY);
    Ok(())
}
