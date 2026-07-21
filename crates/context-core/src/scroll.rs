//! Stable cursor tokens for deterministic point-id scrolling.

const CURSOR_VERSION: &str = "v1";
const CHECKSUM_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const CHECKSUM_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Opaque cursor describing the last point returned by a scroll page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollCursor {
    collection_id: i64,
    after_point_id: i64,
}

impl ScrollCursor {
    /// Creates a cursor for a collection and last returned point id.
    #[must_use]
    pub const fn new(collection_id: i64, after_point_id: i64) -> Self {
        Self {
            collection_id,
            after_point_id,
        }
    }

    /// Encodes the cursor into a SQL-visible token.
    #[must_use]
    pub fn encode(self) -> String {
        let checksum = checksum(self.collection_id, self.after_point_id);
        format!(
            "{CURSOR_VERSION}:{}:{}:{checksum:016x}",
            self.collection_id, self.after_point_id
        )
    }

    /// Decodes a cursor token and verifies that it belongs to `collection_id`.
    ///
    /// # Errors
    ///
    /// Returns [`ScrollCursorError::Malformed`] when the token shape or numeric
    /// fields are invalid, [`ScrollCursorError::InvalidChecksum`] when the
    /// token contents do not match its checksum, and
    /// [`ScrollCursorError::CollectionMismatch`] when the token belongs to a
    /// different collection id.
    pub fn decode_for_collection(
        token: &str,
        collection_id: i64,
    ) -> Result<Self, ScrollCursorError> {
        let mut parts = token.split(':');
        let Some(version) = parts.next() else {
            return Err(ScrollCursorError::Malformed);
        };
        if version != CURSOR_VERSION {
            return Err(ScrollCursorError::Malformed);
        }

        let cursor_collection_id = parse_non_negative_i64(parts.next())?;
        let after_point_id = parse_non_negative_i64(parts.next())?;
        let supplied_checksum = parse_checksum(parts.next())?;
        if parts.next().is_some() {
            return Err(ScrollCursorError::Malformed);
        }

        let expected_checksum = checksum(cursor_collection_id, after_point_id);
        if supplied_checksum != expected_checksum {
            return Err(ScrollCursorError::InvalidChecksum);
        }
        if cursor_collection_id != collection_id {
            return Err(ScrollCursorError::CollectionMismatch {
                expected: collection_id,
                actual: cursor_collection_id,
            });
        }

        Ok(Self {
            collection_id: cursor_collection_id,
            after_point_id,
        })
    }

    /// Returns the collection id bound to this cursor.
    #[must_use]
    pub const fn collection_id(self) -> i64 {
        self.collection_id
    }

    /// Returns the point id after which the next scroll page starts.
    #[must_use]
    pub const fn after_point_id(self) -> i64 {
        self.after_point_id
    }
}

/// Error returned while decoding a scroll cursor token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScrollCursorError {
    /// The token does not match the expected cursor format.
    #[error("malformed scroll cursor")]
    Malformed,

    /// The token checksum does not match the cursor contents.
    #[error("invalid scroll cursor checksum")]
    InvalidChecksum,

    /// The token belongs to another collection id.
    #[error("scroll cursor belongs to collection {actual}, expected {expected}")]
    CollectionMismatch {
        /// Collection id expected by the current request.
        expected: i64,
        /// Collection id encoded in the cursor token.
        actual: i64,
    },
}

fn parse_non_negative_i64(value: Option<&str>) -> Result<i64, ScrollCursorError> {
    let Some(value) = value else {
        return Err(ScrollCursorError::Malformed);
    };
    let value = value
        .parse::<i64>()
        .map_err(|_| ScrollCursorError::Malformed)?;
    if value < 0 {
        return Err(ScrollCursorError::Malformed);
    }
    Ok(value)
}

fn parse_checksum(value: Option<&str>) -> Result<u64, ScrollCursorError> {
    let Some(value) = value else {
        return Err(ScrollCursorError::Malformed);
    };
    u64::from_str_radix(value, 16).map_err(|_| ScrollCursorError::Malformed)
}

fn checksum(collection_id: i64, after_point_id: i64) -> u64 {
    let mut state = CHECKSUM_OFFSET;
    state = update_checksum(state, CURSOR_VERSION.as_bytes());
    state = update_checksum(state, &collection_id.to_le_bytes());
    update_checksum(state, &after_point_id.to_le_bytes())
}

fn update_checksum(mut state: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(CHECKSUM_PRIME);
    }
    state
}
