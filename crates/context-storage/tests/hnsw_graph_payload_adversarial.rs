//! Adversarial count validation for portable HNSW graph payloads.

use context_codec::{CodecRevision, ContiguousCodes, QuantizedCodebook, ReconstructionPolicy};
use context_core::DenseVector;
use context_storage::{
    HnswGraphArtifactRecord, HnswGraphPayloadError, HnswGraphQuantization,
    decode_hnsw_graph_payload, decode_hnsw_graph_payload_versioned, encode_hnsw_graph_payload,
    encode_hnsw_graph_payload_current,
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
fn rejects_adversarial_product_codebook_corruption_before_decode()
-> Result<(), Box<dyn std::error::Error>> {
    let records = vec![HnswGraphArtifactRecord::new(
        0,
        101,
        DenseVector::new(vec![0.0])?,
        Vec::new(),
    )];
    let codebook = QuantizedCodebook::Product {
        dimensions: 1,
        subvector_dimensions: 1,
        codebooks: vec![vec![DenseVector::new(vec![0.0])?]],
    };
    let quantization = HnswGraphQuantization::new(
        CodecRevision::new(1)
            .ok_or_else(|| std::io::Error::other("test codec revision is invalid"))?,
        ReconstructionPolicy::ExactSourceRerank,
        codebook.clone(),
        ContiguousCodes::from_rows(codebook.code_len(), &[vec![0]])?,
    )?;
    let mut payload = encode_hnsw_graph_payload_current(&records, Some(&quantization))?;
    payload[156..160].copy_from_slice(&u32::MAX.to_le_bytes());

    assert!(matches!(
        decode_hnsw_graph_payload_versioned(&payload),
        Err(HnswGraphPayloadError::InvalidQuantization(message))
            if message.contains("checksum")
    ));
    Ok(())
}
