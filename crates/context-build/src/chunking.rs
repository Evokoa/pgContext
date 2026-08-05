//! Deterministic, structure-first, token-safe text chunking.
//!
//! The original document row stays authoritative. A chunk is a *citation span*
//! into that row — a byte range plus the text it covers — so a chunk can always
//! be traced back to the exact region it came from, and re-chunking the same
//! source under the same profile always produces the same spans.
//!
//! Chunking prefers structural boundaries (blank lines, then line breaks, then
//! sentence ends, then whitespace) and falls back to a hard cut only when a
//! single run of text exceeds the budget. Every cut lands on a UTF-8 character
//! boundary, so a chunk is never a partial code point.

use std::fmt;

/// Maximum characters one chunk may span.
pub const MAX_CHUNK_CHARS: usize = 64 * 1024;
/// Maximum characters a chunking profile may accept as input.
pub const MAX_CHUNK_SOURCE_CHARS: usize = 8 * 1024 * 1024;

/// Why a chunking profile or input was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChunkError {
    /// A profile bound was outside its accepted range.
    InvalidProfile {
        /// Bound that failed validation.
        field: &'static str,
        /// Stable reason.
        reason: String,
    },
    /// The supplied document exceeded the accepted input size.
    SourceTooLarge {
        /// Observed character count.
        actual: usize,
        /// Maximum accepted character count.
        maximum: usize,
    },
}

impl fmt::Display for ChunkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProfile { field, reason } => {
                write!(formatter, "invalid chunk profile {field}: {reason}")
            }
            Self::SourceTooLarge { actual, maximum } => write!(
                formatter,
                "document has {actual} characters, exceeding the maximum {maximum}"
            ),
        }
    }
}

impl std::error::Error for ChunkError {}

/// Result type for chunking.
pub type Result<T> = core::result::Result<T, ChunkError>;

/// Immutable chunking contract.
///
/// `overlap_chars` is carried from the end of one chunk into the start of the
/// next so a span that straddles a boundary is still retrievable. It must stay
/// strictly below `max_chars`, otherwise a chunk could never advance past its
/// own overlap and chunking would not terminate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkProfile {
    max_chars: usize,
    overlap_chars: usize,
}

impl ChunkProfile {
    /// Creates a validated chunking profile.
    ///
    /// # Errors
    ///
    /// Returns [`ChunkError::InvalidProfile`] when `max_chars` is zero or above
    /// [`MAX_CHUNK_CHARS`], or when `overlap_chars` is not strictly below
    /// `max_chars`.
    pub fn new(max_chars: usize, overlap_chars: usize) -> Result<Self> {
        if max_chars == 0 || max_chars > MAX_CHUNK_CHARS {
            return Err(ChunkError::InvalidProfile {
                field: "max_chars",
                reason: format!("must be within 1..={MAX_CHUNK_CHARS}"),
            });
        }
        if overlap_chars >= max_chars {
            return Err(ChunkError::InvalidProfile {
                field: "overlap_chars",
                reason: "must be strictly below max_chars so chunking terminates".to_owned(),
            });
        }
        Ok(Self {
            max_chars,
            overlap_chars,
        })
    }

    /// Returns the maximum characters per chunk.
    #[must_use]
    pub const fn max_chars(self) -> usize {
        self.max_chars
    }

    /// Returns the characters carried between adjacent chunks.
    #[must_use]
    pub const fn overlap_chars(self) -> usize {
        self.overlap_chars
    }
}

/// One chunk and the exact source span it cites.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chunk {
    ordinal: usize,
    start_char: usize,
    end_char: usize,
    text: String,
    content_hash: u64,
}

impl Chunk {
    /// Returns the zero-based position of this chunk in the document.
    #[must_use]
    pub const fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// Returns the inclusive start character offset into the source.
    #[must_use]
    pub const fn start_char(&self) -> usize {
        self.start_char
    }

    /// Returns the exclusive end character offset into the source.
    #[must_use]
    pub const fn end_char(&self) -> usize {
        self.end_char
    }

    /// Returns the chunk text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns a stable content hash over this chunk's text.
    ///
    /// The hash is FNV-1a and deliberately fixed: it identifies unchanged
    /// content across processes and runs, so re-chunking an unedited document
    /// can skip re-embedding.
    #[must_use]
    pub const fn content_hash(&self) -> u64 {
        self.content_hash
    }
}

/// Splits a document into deterministic, ordered, token-safe chunks.
///
/// Guarantees, each covered by a property test:
///
/// - **Ordered coverage.** Chunks are emitted in source order and their spans
///   cover every character of the document exactly once, ignoring overlap.
/// - **Boundary safety.** Every span boundary is a UTF-8 character boundary, so
///   no chunk contains a partial code point.
/// - **Bounded size.** No chunk exceeds the profile's `max_chars`.
/// - **Determinism.** The same document and profile always produce the same
///   chunks.
/// - **Progress.** Every chunk advances past the previous chunk's start, so
///   chunking always terminates.
///
/// # Errors
///
/// Returns [`ChunkError::SourceTooLarge`] when the document exceeds
/// [`MAX_CHUNK_SOURCE_CHARS`]. Oversized input is refused rather than silently
/// truncated: a truncated document would produce chunks that cite spans the
/// caller never asked to publish.
pub fn chunk_document(source: &str, profile: ChunkProfile) -> Result<Vec<Chunk>> {
    let characters = source.chars().collect::<Vec<_>>();
    if characters.len() > MAX_CHUNK_SOURCE_CHARS {
        return Err(ChunkError::SourceTooLarge {
            actual: characters.len(),
            maximum: MAX_CHUNK_SOURCE_CHARS,
        });
    }
    if characters.is_empty() {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    let mut start = 0_usize;
    while start < characters.len() {
        let hard_end = characters.len().min(start + profile.max_chars());
        let end = if hard_end == characters.len() {
            hard_end
        } else {
            structural_break(&characters, start, hard_end)
        };
        // `structural_break` never returns `start`, so each iteration consumes
        // at least one character and the loop terminates.
        let text = characters[start..end].iter().collect::<String>();
        let content_hash = fnv1a(text.as_bytes());
        chunks.push(Chunk {
            ordinal: chunks.len(),
            start_char: start,
            end_char: end,
            text,
            content_hash,
        });
        if end >= characters.len() {
            break;
        }
        // Overlap rewinds the next start, but never far enough to revisit the
        // current chunk's own start, so progress is strictly monotonic.
        let rewind = profile.overlap_chars().min(end.saturating_sub(start + 1));
        start = end - rewind;
    }
    Ok(chunks)
}

/// Finds the best structural boundary within `start..hard_end`.
///
/// Preference order is paragraph break, line break, sentence end, then any
/// whitespace. A run of text with no boundary at all is cut at `hard_end`,
/// which is why a single very long token still respects the size bound.
fn structural_break(characters: &[char], start: usize, hard_end: usize) -> usize {
    debug_assert!(hard_end > start);
    let window = &characters[start..hard_end];

    // A paragraph break: the split lands after the second newline.
    for index in (1..window.len()).rev() {
        if window[index] == '\n' && window[index - 1] == '\n' {
            return start + index + 1;
        }
    }
    for index in (0..window.len()).rev() {
        if window[index] == '\n' {
            return start + index + 1;
        }
    }
    for index in (0..window.len()).rev() {
        if matches!(window[index], '.' | '!' | '?')
            && window
                .get(index + 1)
                .is_none_or(|next| next.is_whitespace())
        {
            return start + index + 1;
        }
    }
    for index in (0..window.len()).rev() {
        if window[index].is_whitespace() {
            return start + index + 1;
        }
    }
    hard_end
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use proptest::prelude::*;

    fn profile(max: usize, overlap: usize) -> ChunkProfile {
        ChunkProfile::new(max, overlap).expect("valid profile")
    }

    #[test]
    fn profiles_reject_bounds_that_would_not_terminate() {
        assert!(ChunkProfile::new(0, 0).is_err());
        assert!(ChunkProfile::new(MAX_CHUNK_CHARS + 1, 0).is_err());
        assert!(
            ChunkProfile::new(100, 100).is_err(),
            "overlap equal to the chunk size would never advance"
        );
        assert!(ChunkProfile::new(100, 101).is_err());
        assert!(ChunkProfile::new(100, 99).is_ok());
    }

    #[test]
    fn an_empty_document_produces_no_chunks() {
        assert!(
            chunk_document("", profile(50, 0))
                .expect("chunks")
                .is_empty()
        );
    }

    #[test]
    fn an_oversized_document_is_refused_rather_than_truncated() {
        let oversized = "a".repeat(MAX_CHUNK_SOURCE_CHARS + 1);
        assert!(matches!(
            chunk_document(&oversized, profile(50, 0)),
            Err(ChunkError::SourceTooLarge { .. })
        ));
    }

    #[test]
    fn chunking_prefers_paragraph_then_line_then_sentence_boundaries() {
        let paragraphs =
            chunk_document("alpha\n\nbeta gamma delta", profile(12, 0)).expect("chunks");
        assert_eq!(paragraphs[0].text(), "alpha\n\n");

        let lines = chunk_document("alpha\nbeta gamma delta", profile(12, 0)).expect("chunks");
        assert_eq!(lines[0].text(), "alpha\n");

        let sentences = chunk_document("One two. Three four five", profile(12, 0)).expect("chunks");
        assert_eq!(sentences[0].text(), "One two.");
    }

    #[test]
    fn a_single_oversized_run_is_hard_cut_at_the_size_bound() {
        let chunks = chunk_document(&"x".repeat(25), profile(10, 0)).expect("chunks");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].text().chars().count(), 10);
        assert_eq!(chunks[2].text().chars().count(), 5);
    }

    #[test]
    fn overlap_repeats_the_tail_of_the_previous_chunk() {
        let chunks = chunk_document(&"x".repeat(20), profile(10, 3)).expect("chunks");
        assert!(chunks.len() >= 2);
        assert_eq!(chunks[1].start_char(), chunks[0].end_char() - 3);
    }

    #[test]
    fn identical_text_hashes_identically_across_positions() {
        let chunks = chunk_document("abcde\nabcde\n", profile(6, 0)).expect("chunks");
        assert_eq!(chunks[0].text(), chunks[1].text());
        assert_eq!(chunks[0].content_hash(), chunks[1].content_hash());
        assert_ne!(chunks[0].start_char(), chunks[1].start_char());
    }

    #[test]
    fn multi_byte_characters_are_never_split() {
        // Each emoji is 4 UTF-8 bytes but one char; a char-indexed cut keeps
        // them intact where a byte-indexed one would not.
        let chunks = chunk_document(&"😀".repeat(7), profile(3, 0)).expect("chunks");
        assert_eq!(chunks.len(), 3);
        for chunk in &chunks {
            assert!(chunk.text().chars().all(|character| character == '😀'));
        }
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.text().chars().count())
                .sum::<usize>(),
            7
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn chunks_cover_the_source_in_order_without_gaps(
            source in ".{0,400}",
            max in 1_usize..64,
            overlap in 0_usize..64,
        ) {
            prop_assume!(overlap < max);
            let profile = ChunkProfile::new(max, overlap).expect("profile");
            let characters = source.chars().collect::<Vec<_>>();
            let chunks = chunk_document(&source, profile).expect("chunks");

            if characters.is_empty() {
                prop_assert!(chunks.is_empty());
                return Ok(());
            }

            prop_assert_eq!(chunks[0].start_char(), 0);
            prop_assert_eq!(
                chunks.last().expect("last chunk").end_char(),
                characters.len()
            );
            for pair in chunks.windows(2) {
                // Ordered, strictly advancing, and gapless once overlap is
                // accounted for: the next chunk starts no later than where the
                // previous one ended.
                prop_assert!(pair[1].start_char() > pair[0].start_char());
                prop_assert!(pair[1].start_char() <= pair[0].end_char());
            }
            for (ordinal, chunk) in chunks.iter().enumerate() {
                prop_assert_eq!(chunk.ordinal(), ordinal);
                prop_assert!(chunk.end_char() > chunk.start_char());
                prop_assert!(chunk.text().chars().count() <= max);
                let expected = characters[chunk.start_char()..chunk.end_char()]
                    .iter()
                    .collect::<String>();
                prop_assert_eq!(chunk.text(), &expected);
            }
        }

        #[test]
        fn chunking_is_deterministic(
            source in ".{0,300}",
            max in 1_usize..48,
            overlap in 0_usize..48,
        ) {
            prop_assume!(overlap < max);
            let profile = ChunkProfile::new(max, overlap).expect("profile");
            prop_assert_eq!(
                chunk_document(&source, profile).expect("chunks"),
                chunk_document(&source, profile).expect("chunks")
            );
        }

        #[test]
        fn concatenating_non_overlapping_chunks_reproduces_the_source(
            source in ".{0,400}",
            max in 1_usize..64,
        ) {
            let profile = ChunkProfile::new(max, 0).expect("profile");
            let rebuilt = chunk_document(&source, profile)
                .expect("chunks")
                .iter()
                .map(|chunk| chunk.text().to_owned())
                .collect::<String>();
            prop_assert_eq!(rebuilt, source);
        }
    }
}
