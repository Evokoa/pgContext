//! Property coverage for scroll cursor tokens.

use context_core::{ScrollCursor, ScrollCursorError};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        .. ProptestConfig::default()
    })]

    #[test]
    fn scroll_cursor_tokens_round_trip(collection_id in 0_i64..i64::MAX, point_id in 0_i64..i64::MAX) {
        let cursor = ScrollCursor::new(collection_id, point_id);
        let encoded = cursor.encode();

        prop_assert_eq!(
            ScrollCursor::decode_for_collection(&encoded, collection_id),
            Ok(cursor)
        );
        prop_assert_eq!(encoded.split(':').count(), 4);
    }

    #[test]
    fn scroll_cursor_rejects_collection_mismatches(
        collection_id in 0_i64..i64::MAX - 1,
        point_id in 0_i64..i64::MAX,
    ) {
        let token = ScrollCursor::new(collection_id, point_id).encode();

        prop_assert_eq!(
            ScrollCursor::decode_for_collection(&token, collection_id + 1),
            Err(ScrollCursorError::CollectionMismatch {
                expected: collection_id + 1,
                actual: collection_id,
            })
        );
    }

    #[test]
    fn arbitrary_cursor_text_never_panics(token in "\\PC*") {
        let result = ScrollCursor::decode_for_collection(&token, 0);

        if let Ok(cursor) = result {
            prop_assert_eq!(cursor.collection_id(), 0);
            prop_assert!(cursor.after_point_id() >= 0);
        }
    }
}
