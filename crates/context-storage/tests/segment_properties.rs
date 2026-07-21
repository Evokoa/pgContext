//! Property coverage for segment encode/decode boundaries.

use context_storage::{
    SegmentError, SegmentHeader, SegmentKind, decode_segment, encode_segment, validate_mmap_segment,
};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        .. ProptestConfig::default()
    })]

    #[test]
    fn encoded_segments_decode_and_validate_as_mmap_views(payload in prop::collection::vec(any::<u8>(), 0..4096)) {
        let encoded = encode_segment(SegmentKind::HnswGraph, &payload)?;
        let decoded = decode_segment(&encoded)?;
        let view = validate_mmap_segment(&encoded)?;

        prop_assert_eq!(decoded.payload(), payload.as_slice());
        prop_assert_eq!(view.payload(), payload.as_slice());
        prop_assert_eq!(decoded.header(), view.header());
    }

    #[test]
    fn corrupted_encoded_segments_fail_checksum(payload in prop::collection::vec(any::<u8>(), 1..4096)) {
        let mut encoded = encode_segment(SegmentKind::HnswGraph, &payload)?;
        let last = encoded.len() - 1;
        encoded[last] ^= 0x55;

        prop_assert_eq!(decode_segment(&encoded), Err(SegmentError::ChecksumMismatch));
        prop_assert_eq!(
            validate_mmap_segment(&encoded),
            Err(SegmentError::ChecksumMismatch)
        );
    }

    #[test]
    fn truncated_encoded_segments_report_short_payload(payload in prop::collection::vec(any::<u8>(), 1..4096)) {
        let mut encoded = encode_segment(SegmentKind::HnswGraph, &payload)?;
        encoded.pop();

        let is_expected_error = match decode_segment(&encoded) {
            Err(SegmentError::TruncatedPayload { expected, actual }) => {
                expected == payload.len() && actual + 1 == payload.len()
            }
            _ => false,
        };
        prop_assert!(is_expected_error);
    }

    #[test]
    fn short_inputs_report_truncated_header(len in 0_usize..SegmentHeader::ENCODED_LEN) {
        let input = vec![0_u8; len];

        prop_assert_eq!(
            decode_segment(&input),
            Err(SegmentError::TruncatedHeader {
                actual: len,
                minimum: SegmentHeader::ENCODED_LEN,
            })
        );
    }
}
