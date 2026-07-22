//! Allocation-bounded mapped graph view tests.

use std::error::Error;

use context_core::DenseVector;
use context_storage::{
    HnswGraphArtifactRecord, HnswGraphPayloadError, MappedGraphView, encode_hnsw_graph_payload,
    encode_hnsw_graph_payload_v2,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn vector(values: &[f32]) -> TestResult<DenseVector> {
    Ok(DenseVector::new(values.to_vec())?)
}

fn records() -> TestResult<Vec<HnswGraphArtifactRecord>> {
    Ok(vec![
        HnswGraphArtifactRecord::new(0, 10, vector(&[0.25, -1.0])?, vec![1]),
        HnswGraphArtifactRecord::new(1, 20, vector(&[2.0, 0.5])?, vec![0]),
    ])
}

#[test]
fn mapped_view_borrows_v1_nodes_and_decodes_one_vector_into_scratch() -> TestResult {
    let payload = encode_hnsw_graph_payload(&records()?)?;
    let view = MappedGraphView::attach(&payload)?;
    let mut scratch = Vec::new();

    assert_eq!(view.version(), 1);
    assert_eq!(view.dimensions(), 2);
    assert_eq!(view.len(), 2);
    assert!(view.codebook().is_none());
    let node = view
        .node(0)
        .ok_or_else(|| std::io::Error::other("first node should exist"))?;
    assert_eq!(node.point_id(), 10);
    assert_eq!(node.neighbors().collect::<Vec<_>>(), vec![1]);
    assert_eq!(node.decode_vector_into(&mut scratch), &[0.25, -1.0]);
    assert!(node.code().is_none());
    Ok(())
}

#[test]
fn mapped_view_borrows_unquantized_v2_nodes() -> TestResult {
    let payload = encode_hnsw_graph_payload_v2(&records()?, None)?;
    let view = MappedGraphView::attach(&payload)?;
    let mut scratch = vec![99.0; 64];
    let node = view
        .node(1)
        .ok_or_else(|| std::io::Error::other("second node should exist"))?;

    assert_eq!(view.version(), 2);
    assert_eq!(node.point_id(), 20);
    assert_eq!(node.decode_vector_into(&mut scratch), &[2.0, 0.5]);
    assert_eq!(scratch.capacity(), 64);
    assert_eq!(node.neighbors().collect::<Vec<_>>(), vec![0]);
    Ok(())
}

#[test]
fn mapped_view_fails_closed_on_truncated_node_bytes() -> TestResult {
    let mut payload = encode_hnsw_graph_payload(&records()?)?;
    payload.pop();

    assert!(matches!(
        MappedGraphView::attach(&payload),
        Err(HnswGraphPayloadError::TruncatedRecord { .. })
    ));
    Ok(())
}
