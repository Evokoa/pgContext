//! Allocation-bounded mapped graph view tests.

use std::error::Error;

use context_codec::{CodecRevision, ContiguousCodes, QuantizedCodebook, ReconstructionPolicy};
use context_core::DenseVector;
use context_storage::{
    CURRENT_HNSW_GRAPH_PAYLOAD_VERSION, HnswGraphArtifactRecord, HnswGraphPayloadError,
    HnswGraphQuantization, MappedGraphView, encode_hnsw_graph_payload,
    encode_hnsw_graph_payload_current,
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
fn mapped_view_borrows_unquantized_current_nodes() -> TestResult {
    let payload = encode_hnsw_graph_payload_current(&records()?, None)?;
    let view = MappedGraphView::attach(&payload)?;
    let mut scratch = vec![99.0; 64];
    let node = view
        .node(1)
        .ok_or_else(|| std::io::Error::other("second node should exist"))?;

    assert_eq!(view.version(), CURRENT_HNSW_GRAPH_PAYLOAD_VERSION);
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

#[test]
fn budgeted_attach_rejects_declared_node_storage_before_allocation() -> TestResult {
    let mut payload = encode_hnsw_graph_payload(&records()?)?;
    let declared = 1_000_000_u32;
    payload[12..16].copy_from_slice(&declared.to_le_bytes());

    assert!(matches!(
        MappedGraphView::attach_with_memory_budget(&payload, 1024),
        Err(HnswGraphPayloadError::MemoryBudgetExceeded { maximum: 1024, .. })
    ));
    Ok(())
}

#[test]
fn budgeted_attach_projects_product_codebook_before_decoding_it() -> TestResult {
    let records = vec![HnswGraphArtifactRecord::new(
        0,
        10,
        vector(&[0.0, 0.0])?,
        Vec::new(),
    )];
    let codebook = QuantizedCodebook::Product {
        dimensions: 2,
        subvector_dimensions: 2,
        codebooks: vec![vec![vector(&[0.0, 0.0])?, vector(&[1.0, 1.0])?]],
    };
    let quantization = HnswGraphQuantization::new(
        CodecRevision::new(1)
            .ok_or_else(|| std::io::Error::other("test codec revision is invalid"))?,
        ReconstructionPolicy::ExactSourceRerank,
        codebook.clone(),
        ContiguousCodes::from_rows(codebook.code_len(), &[vec![0]])?,
    )?;
    let payload = encode_hnsw_graph_payload_current(&records, Some(&quantization))?;

    assert!(matches!(
        MappedGraphView::attach_with_memory_budget(&payload, 32),
        Err(HnswGraphPayloadError::MemoryBudgetExceeded { maximum: 32, .. })
    ));
    Ok(())
}
