//! Production page-native IVFFlat metapage contract tests.

use std::error::Error;

use context_storage::{
    IVF_PAGE_META_BYTES, IVF_PAGE_META_VERSION, IvfPageChunkKind, IvfPageError, IvfPageMeta,
    decode_ivf_page_chunk, encode_ivf_page_chunk,
};

fn populated_meta() -> IvfPageMeta {
    IvfPageMeta {
        metric_tag: 1,
        dimensions: 2,
        lists: 2,
        tuples: 3,
        centroid_bytes: 16,
        directory_bytes: 32,
        codec_bytes: 0,
        posting_bytes: 48,
        centroid_start: 1,
        centroid_end: 2,
        directory_start: 2,
        directory_end: 3,
        codec_start: 3,
        codec_end: 3,
        posting_start: 3,
        posting_end: 4,
        delta_start: 4,
        delta_end: 4,
        delta_count: 0,
        generation: 7,
        codec_revision: 0,
        codec_code_width: 0,
        codec_mode: 0,
        build_workers: 2,
    }
}

#[test]
fn section_chunks_share_the_production_checksum_contract() -> Result<(), Box<dyn Error>> {
    let encoded = encode_ivf_page_chunk(IvfPageChunkKind::Postings, 7, 1, 2, 10, b"ij")?;
    assert_eq!(
        decode_ivf_page_chunk(&encoded, IvfPageChunkKind::Postings, 7, 1, 2, 10, 8)?,
        b"ij"
    );
    let mut corrupt = encoded;
    let last = corrupt.len() - 1;
    corrupt[last] ^= 1;
    assert_eq!(
        decode_ivf_page_chunk(&corrupt, IvfPageChunkKind::Postings, 7, 1, 2, 10, 8,),
        Err(IvfPageError::ChecksumMismatch)
    );
    Ok(())
}

#[test]
fn page_meta_round_trip_validates_before_section_allocation() -> Result<(), Box<dyn Error>> {
    let encoded = populated_meta().encode();
    assert_eq!(encoded.len(), IVF_PAGE_META_BYTES);
    assert_eq!(
        u16::from_le_bytes([encoded[4], encoded[5]]),
        IVF_PAGE_META_VERSION
    );
    let decoded = IvfPageMeta::decode(&encoded)?;
    assert_eq!(decoded, populated_meta());
    decoded.validate_page_extents(128, 4)?;
    Ok(())
}

#[test]
fn page_meta_checksum_and_canonical_lengths_fail_closed() {
    let mut corrupt = populated_meta().encode();
    corrupt[16] ^= 1;
    assert_eq!(
        IvfPageMeta::decode(&corrupt),
        Err(IvfPageError::ChecksumMismatch)
    );

    let mut invalid = populated_meta();
    invalid.posting_bytes += 1;
    assert!(matches!(
        IvfPageMeta::decode(&invalid.encode()),
        Err(IvfPageError::Invalid(
            "section byte lengths are not canonical"
        ))
    ));
}

#[test]
fn page_meta_rejects_old_versions_and_impossible_extents() -> Result<(), Box<dyn Error>> {
    let mut old = populated_meta().encode();
    old[4..6].copy_from_slice(&(IVF_PAGE_META_VERSION - 1).to_le_bytes());
    assert_eq!(
        IvfPageMeta::decode(&old),
        Err(IvfPageError::RebuildRequired {
            version: IVF_PAGE_META_VERSION - 1
        })
    );

    let decoded = IvfPageMeta::decode(&populated_meta().encode())?;
    assert_eq!(
        decoded.validate_page_extents(128, 3),
        Err(IvfPageError::Invalid("page extent exceeds relation"))
    );
    Ok(())
}
