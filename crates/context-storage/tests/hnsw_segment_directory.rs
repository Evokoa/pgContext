//! Structural and adversarial tests for segmented-HNSW directories.

use context_storage::{
    HNSW_SEGMENT_DIRECTORY_VERSION, HnswDeltaDescriptor, HnswSegmentDescriptor,
    HnswSegmentDirectory, HnswSegmentDirectoryError, decode_hnsw_segment_directory,
    encode_hnsw_segment_directory,
};
use proptest::prelude::*;

fn segment(id: u64, start: u64, rows: u64) -> HnswSegmentDescriptor {
    HnswSegmentDescriptor {
        segment_id: id,
        generation: id + 10,
        start_block: start,
        end_block: start + 3,
        row_count: rows,
        payload_bytes: rows * 16,
        checksum: id * 17,
        source_revision: 7,
        config_revision: 9,
        mutation_start_block: u64::MAX,
        mutation_end_block: u64::MAX,
    }
}

fn directory() -> HnswSegmentDirectory {
    HnswSegmentDirectory {
        generation: 4,
        source_revision: 7,
        config_revision: 9,
        segments: vec![segment(1, 10, 100), segment(2, 20, 200)],
        delta: HnswDeltaDescriptor {
            start_block: 30,
            end_block: 31,
            record_count: 4,
            generation: 3,
        },
    }
}

#[test]
fn directory_round_trips_and_old_format_requires_rebuild() -> Result<(), HnswSegmentDirectoryError>
{
    let encoded = encode_hnsw_segment_directory(&directory())?;
    assert_eq!(decode_hnsw_segment_directory(&encoded)?, directory());
    let mut old = encoded;
    old[8..12].copy_from_slice(&(HNSW_SEGMENT_DIRECTORY_VERSION - 1).to_le_bytes());
    assert_eq!(
        decode_hnsw_segment_directory(&old),
        Err(HnswSegmentDirectoryError::RebuildRequired(0))
    );
    Ok(())
}

#[test]
fn directory_rejects_mixed_revision_and_overlapping_extents() {
    let mut mixed = directory();
    mixed.segments[1].source_revision += 1;
    assert_eq!(
        encode_hnsw_segment_directory(&mixed),
        Err(HnswSegmentDirectoryError::MixedRevision)
    );
    let mut overlap = directory();
    overlap.segments[1].start_block = 12;
    assert_eq!(
        encode_hnsw_segment_directory(&overlap),
        Err(HnswSegmentDirectoryError::OverlappingExtents)
    );
}

#[test]
fn directory_accepts_a_tombstone_only_immutable_segment() {
    let mut expected = directory();
    expected.segments[1] = HnswSegmentDescriptor {
        start_block: 30,
        end_block: 30,
        row_count: 0,
        payload_bytes: 0,
        checksum: 0,
        mutation_start_block: 20,
        mutation_end_block: 30,
        ..expected.segments[1]
    };
    expected.delta.start_block = 31;
    expected.delta.end_block = 32;
    let encoded = encode_hnsw_segment_directory(&expected);
    assert_eq!(
        encoded.and_then(|bytes| decode_hnsw_segment_directory(&bytes)),
        Ok(expected)
    );
}

proptest! {
    #[test]
    fn arbitrary_single_byte_corruption_never_decodes_as_the_same_directory(
        index in 0_usize..248,
        bit in 0_u8..8,
    ) {
        let expected = directory();
        let mut encoded = encode_hnsw_segment_directory(&expected)?;
        encoded[index] ^= 1 << bit;
        let decoded = decode_hnsw_segment_directory(&encoded);
        prop_assert!(decoded.as_ref().is_err() || decoded.as_ref().ok() != Some(&expected));
    }
}
