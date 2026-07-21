//! Delta-segment page codec contract tests.
#![allow(
    clippy::expect_used,
    reason = "test fixtures expect on codec operations the test itself constructed"
)]

use context_storage::{
    DELTA_PAGE_HEADER_BYTES, DeltaRecord, DeltaRecordKind, DeltaSegmentError, decode_delta_page,
    encode_delta_page,
};
use proptest::prelude::*;

const PAGE_PAYLOAD: usize = 8_000;

fn fixture_records() -> Vec<DeltaRecord> {
    vec![
        DeltaRecord::live(42, vec![1.0, -2.5, 0.25]).expect("valid live record"),
        DeltaRecord::tombstone(7),
        DeltaRecord::live(u64::MAX, vec![0.0; 8]).expect("valid live record"),
    ]
}

#[test]
fn delta_page_round_trips_records_and_generation() {
    let records = fixture_records();
    let payload = encode_delta_page(9, &records, PAGE_PAYLOAD).expect("encode");
    assert_eq!(payload.len(), PAGE_PAYLOAD);
    let (generation, decoded) = decode_delta_page(&payload).expect("decode");
    assert_eq!(generation, 9);
    assert_eq!(decoded, records);
}

#[test]
fn delta_page_rejects_structural_corruption() {
    let payload = encode_delta_page(1, &fixture_records(), PAGE_PAYLOAD).expect("encode");

    // Truncated header.
    assert_eq!(
        decode_delta_page(&payload[..DELTA_PAGE_HEADER_BYTES - 1]),
        Err(DeltaSegmentError::Truncated)
    );
    // Bad magic.
    let mut corrupt = payload.clone();
    corrupt[0] ^= 0xff;
    assert_eq!(
        decode_delta_page(&corrupt),
        Err(DeltaSegmentError::BadMagic)
    );
    // Unsupported version.
    let mut corrupt = payload.clone();
    corrupt[8] = 0xee;
    assert!(matches!(
        decode_delta_page(&corrupt),
        Err(DeltaSegmentError::UnsupportedVersion { .. })
    ));
    // Flipped record byte -> checksum mismatch.
    let mut corrupt = payload.clone();
    corrupt[DELTA_PAGE_HEADER_BYTES] ^= 0x01;
    assert_eq!(
        decode_delta_page(&corrupt),
        Err(DeltaSegmentError::ChecksumMismatch)
    );
    // Record count claims more than the page holds (checksum refreshed so
    // the structural check is what fires).
    let mut corrupt = payload;
    corrupt[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
    corrupt[24..32].fill(0);
    let checksum = {
        // Recompute FNV-1a the same way the codec does.
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        for byte in &corrupt {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    };
    corrupt[24..32].copy_from_slice(&checksum.to_le_bytes());
    // The exact rejection depends on what the walker hits first in the
    // zero-filled tail (unknown flags vs truncation); failing closed is the
    // contract, not which structural violation wins.
    assert!(decode_delta_page(&corrupt).is_err());
}

#[test]
fn delta_page_rejects_oversized_and_invalid_records() {
    // Overflowing the page fails closed with the space accounting.
    let big = DeltaRecord::live(1, vec![1.0; 1_500]).expect("valid record");
    let result = encode_delta_page(1, &[big.clone(), big], PAGE_PAYLOAD);
    assert!(matches!(
        result,
        Err(DeltaSegmentError::PageOverflow { .. })
    ));
    // Live records validate their vectors at construction.
    assert!(DeltaRecord::live(1, vec![]).is_err());
    assert!(DeltaRecord::live(1, vec![f32::NAN]).is_err());
    assert!(DeltaRecord::live(1, vec![f32::INFINITY, 1.0]).is_err());
}

proptest! {
    #[test]
    fn delta_page_round_trips_generated_batches(
        generation in any::<u64>(),
        specs in proptest::collection::vec(
            (any::<u64>(), proptest::option::of(
                proptest::collection::vec(-1_000.0_f32..1_000.0, 1..24)
            )),
            0..40,
        ),
    ) {
        let records: Vec<DeltaRecord> = specs
            .into_iter()
            .map(|(tid, vector)| match vector {
                Some(values) => DeltaRecord::live(tid, values).expect("bounded finite vector"),
                None => DeltaRecord::tombstone(tid),
            })
            .collect();
        let required: usize = DELTA_PAGE_HEADER_BYTES
            + records.iter().map(DeltaRecord::encoded_len).sum::<usize>();
        let payload = encode_delta_page(generation, &records, required.max(PAGE_PAYLOAD))
            .expect("bounded batch encodes");
        let (decoded_generation, decoded) = decode_delta_page(&payload).expect("decode");
        prop_assert_eq!(decoded_generation, generation);
        prop_assert_eq!(decoded, records);
        // Tombstones stay vector-free by construction.
        prop_assert!(
            decode_delta_page(&payload)
                .expect("decode")
                .1
                .iter()
                .all(|record| record.kind != DeltaRecordKind::Tombstone
                    || record.vector.is_empty())
        );
    }
}

#[test]
fn delta_record_item_payload_round_trips_and_fails_closed() {
    use context_storage::{decode_delta_record, encode_delta_record};
    for record in fixture_records() {
        let payload = encode_delta_record(&record).expect("encode record");
        assert_eq!(
            decode_delta_record(&payload).expect("decode record"),
            record
        );
    }
    // Truncated, trailing-byte, unknown-flag, and non-finite payloads fail.
    let good = encode_delta_record(&fixture_records()[0]).expect("encode");
    assert!(decode_delta_record(&good[..8]).is_err());
    let mut trailing = good.clone();
    trailing.push(0);
    assert!(decode_delta_record(&trailing).is_err());
    let mut bad_flags = good.clone();
    bad_flags[8] = 0x7f;
    assert!(decode_delta_record(&bad_flags).is_err());
    let mut non_finite = good;
    non_finite[12..16].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(decode_delta_record(&non_finite).is_err());
}
