//! Adversarial count validation for portable HNSW graph payloads.

use context_core::DenseVector;
use context_storage::{
    HnswGraphArtifactRecord, HnswGraphPayloadError, HnswGraphQuantization,
    HnswGraphQuantizationCodebook, decode_hnsw_graph_payload, decode_hnsw_graph_payload_versioned,
    encode_hnsw_graph_payload, encode_hnsw_graph_payload_v2,
};

#[test]
fn rejects_adversarial_record_count_before_allocation() -> Result<(), Box<dyn std::error::Error>> {
    let record = HnswGraphArtifactRecord::new(0, 101, DenseVector::new(vec![0.0])?, Vec::new());
    let mut payload = encode_hnsw_graph_payload(&[record])?;
    payload[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    let declared = usize::try_from(u32::MAX)?;

    assert_eq!(
        decode_hnsw_graph_payload(&payload),
        Err(HnswGraphPayloadError::RecordCountLimit {
            declared,
            maximum: 1_000_000,
        })
    );
    Ok(())
}

#[test]
fn rejects_adversarial_product_codebook_count() -> Result<(), Box<dyn std::error::Error>> {
    let records = vec![HnswGraphArtifactRecord::new(
        0,
        101,
        DenseVector::new(vec![0.0])?,
        Vec::new(),
    )];
    let quantization = HnswGraphQuantization::new(
        HnswGraphQuantizationCodebook::Product {
            dimensions: 1,
            subvector_dimensions: 1,
            codebooks: vec![vec![DenseVector::new(vec![0.0])?]],
        },
        vec![vec![0]],
    );
    let mut payload = encode_hnsw_graph_payload_v2(&records, Some(&quantization))?;
    payload[48..52].copy_from_slice(&u32::MAX.to_le_bytes());

    assert!(matches!(
        decode_hnsw_graph_payload_versioned(&payload),
        Err(HnswGraphPayloadError::InvalidQuantization(message))
            if message.contains("codebook count")
    ));
    Ok(())
}
