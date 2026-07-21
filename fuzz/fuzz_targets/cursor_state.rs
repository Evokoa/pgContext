#![no_main]

use context_core::{ScrollCursor, ScrollCursorError};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let collection_id = non_negative_i64(data.get(..8).unwrap_or(data));
    let after_point_id = non_negative_i64(data.get(8..16).unwrap_or_default());
    let cursor = ScrollCursor::new(collection_id, after_point_id);
    let token = cursor.encode();
    let decoded = ScrollCursor::decode_for_collection(&token, collection_id)
        .expect("newly encoded cursor must decode for its collection");
    assert_eq!(decoded, cursor);
    assert_eq!(decoded.encode(), token);

    let other_collection = if collection_id == i64::MAX {
        0
    } else {
        collection_id + 1
    };
    assert!(matches!(
        ScrollCursor::decode_for_collection(&token, other_collection),
        Err(ScrollCursorError::CollectionMismatch { .. })
    ));

    let arbitrary_token = data.get(16..).unwrap_or(data);
    if let Ok(arbitrary_token) = core::str::from_utf8(arbitrary_token) {
        let _ = ScrollCursor::decode_for_collection(arbitrary_token, collection_id);
    }
});

fn non_negative_i64(bytes: &[u8]) -> i64 {
    let mut encoded = [0_u8; 8];
    let count = bytes.len().min(encoded.len());
    encoded[..count].copy_from_slice(&bytes[..count]);
    i64::from_le_bytes(encoded) & i64::MAX
}
