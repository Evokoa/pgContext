//! Scroll cursor token regression coverage.

use context_core::{ScrollCursor, ScrollCursorError};

#[test]
fn scroll_cursor_round_trips_collection_and_point_id() {
    let cursor = ScrollCursor::new(42, 9001);
    let encoded = cursor.encode();

    let decoded = ScrollCursor::decode_for_collection(&encoded, 42);

    assert_eq!(decoded, Ok(ScrollCursor::new(42, 9001)));
}

#[test]
fn scroll_cursor_rejects_tampered_tokens() {
    let token = ScrollCursor::new(42, 9001).encode();
    let tampered = token.replace("9001", "9002");

    let result = ScrollCursor::decode_for_collection(&tampered, 42);

    assert_eq!(result, Err(ScrollCursorError::InvalidChecksum));
}

#[test]
fn scroll_cursor_rejects_stale_collection_tokens() {
    let token = ScrollCursor::new(42, 9001).encode();

    assert_eq!(
        ScrollCursor::decode_for_collection(&token, 43),
        Err(ScrollCursorError::CollectionMismatch {
            expected: 43,
            actual: 42
        })
    );
}

#[test]
fn scroll_cursor_rejects_malformed_tokens() {
    let result = ScrollCursor::decode_for_collection("v1:42:nope:1234", 42);

    assert_eq!(result, Err(ScrollCursorError::Malformed));
}
