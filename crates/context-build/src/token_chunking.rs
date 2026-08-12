//! Deterministic token-aware parsing and source-linked chunk construction.
//!
//! This module keeps original citation spans separate from parser-normalized
//! retrieval text. It performs no I/O and trusts no parser-provided offsets.

use std::{collections::BTreeMap, fmt, mem::size_of};

mod parsers;

use parsers::{html_blocks, markdown_blocks, plain_blocks};

/// Maximum raw document bytes accepted by the certified token chunker.
pub const MAX_TOKEN_CHUNK_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum retained parser output allocation before token window construction.
pub const MAX_TOKEN_CHUNK_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum tokens admitted for one document.
pub const MAX_TOKENS_PER_DOCUMENT: usize = 2_000_000;
/// Maximum token count in one chunk.
pub const MAX_TOKENS_PER_CHUNK: usize = 512;
/// Maximum overlap admitted by the certified PostgreSQL and worker profiles.
///
/// The pure kernel accepts larger overlaps for adversarial allocation tests,
/// while product-facing profile admission freezes this tighter cost bound.
pub const MAX_CERTIFIED_PROFILE_OVERLAP_TOKENS: usize = 64;
/// Maximum structural heading/path depth emitted by a certified parser.
pub const MAX_STRUCTURE_DEPTH: usize = 32;
/// Maximum UTF-8 bytes in one structural path segment.
pub const MAX_STRUCTURE_SEGMENT_BYTES: usize = 512;
/// Maximum tokens retained in a derived structure-context prefix.
pub const MAX_CONTEXT_PREFIX_TOKENS: usize = 96;

/// Certified document parser.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentParser {
    /// Plain UTF-8 text split at paragraph boundaries.
    PlainTextV1,
    /// Markdown headings and paragraph-like blocks.
    MarkdownV1,
    /// Bounded HTML heading and block extraction.
    HtmlV1,
}

impl DocumentParser {
    /// Returns the immutable parser revision.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PlainTextV1 => "plain_text_v1",
            Self::MarkdownV1 => "markdown_v1",
            Self::HtmlV1 => "html_v1",
        }
    }
}

/// Certified tokenizer revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TokenizerRevision {
    /// Contiguous Unicode alphanumeric scalars with apostrophes inside words.
    UnicodeWordsV1,
}

impl TokenizerRevision {
    /// Returns the immutable tokenizer revision.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnicodeWordsV1 => "unicode_words_v1",
        }
    }
}

/// Structural source unit owning a chunk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructureKind {
    /// Whole-document structural root.
    Document,
    /// Heading or section title.
    Heading,
    /// Ordinary paragraph.
    Paragraph,
    /// List item.
    List,
    /// Table-like block.
    Table,
    /// Code or preformatted block.
    Code,
    /// Sentence-sized structural unit.
    Sentence,
    /// Token window split from a larger source unit.
    TokenWindow,
}

/// One exact tokenizer span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WordToken {
    start_byte: usize,
    end_byte: usize,
    text: String,
}

impl WordToken {
    /// Returns the token text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the inclusive start byte in the tokenizer input.
    #[must_use]
    pub const fn start_byte(&self) -> usize {
        self.start_byte
    }

    /// Returns the exclusive end byte in the tokenizer input.
    #[must_use]
    pub const fn end_byte(&self) -> usize {
        self.end_byte
    }
}

/// Token-aware immutable chunking profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TokenChunkProfile {
    target_tokens: usize,
    max_tokens: usize,
    min_tokens: usize,
    overlap_tokens: usize,
    max_document_bytes: usize,
}

impl TokenChunkProfile {
    /// Creates a bounded token chunking profile.
    ///
    /// # Errors
    ///
    /// Returns [`TokenChunkError::InvalidProfile`] when the token or document
    /// bounds are zero, inconsistent, or above the certified maxima.
    pub fn new(
        target_tokens: usize,
        max_tokens: usize,
        min_tokens: usize,
        overlap_tokens: usize,
        max_document_bytes: usize,
    ) -> Result<Self> {
        if target_tokens == 0 || target_tokens > max_tokens {
            return Err(invalid_profile(
                "target_tokens",
                "must be within 1..=max_tokens",
            ));
        }
        if max_tokens == 0 || max_tokens > MAX_TOKENS_PER_CHUNK {
            return Err(invalid_profile(
                "max_tokens",
                "must be inside the certified token ceiling",
            ));
        }
        if min_tokens == 0 || min_tokens > target_tokens {
            return Err(invalid_profile(
                "min_tokens",
                "must be within 1..=target_tokens",
            ));
        }
        if overlap_tokens >= target_tokens {
            return Err(invalid_profile(
                "overlap_tokens",
                "must be strictly below target_tokens",
            ));
        }
        if max_document_bytes == 0 || max_document_bytes > MAX_TOKEN_CHUNK_DOCUMENT_BYTES {
            return Err(invalid_profile(
                "max_document_bytes",
                "must be inside the certified document ceiling",
            ));
        }
        Ok(Self {
            target_tokens,
            max_tokens,
            min_tokens,
            overlap_tokens,
            max_document_bytes,
        })
    }

    /// Returns the target token count.
    #[must_use]
    pub const fn target_tokens(self) -> usize {
        self.target_tokens
    }

    /// Returns the maximum token count.
    #[must_use]
    pub const fn max_tokens(self) -> usize {
        self.max_tokens
    }

    /// Returns the preferred minimum token count for a trailing window.
    #[must_use]
    pub const fn min_tokens(self) -> usize {
        self.min_tokens
    }

    /// Returns the overlap token count.
    #[must_use]
    pub const fn overlap_tokens(self) -> usize {
        self.overlap_tokens
    }
}

/// Stable chunk occurrence identity within a deterministic generation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ChunkOccurrenceId(u64);

impl ChunkOccurrenceId {
    /// Returns the stable numeric representation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Immutable source lineage used to derive globally stable chunk occurrences.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChunkIdentityContext {
    document_id: u64,
    source_version: u64,
    profile_revision: u64,
}

impl ChunkIdentityContext {
    /// Creates non-zero source lineage.
    #[must_use]
    pub const fn new(document_id: u64, source_version: u64, profile_revision: u64) -> Option<Self> {
        if document_id == 0 || source_version == 0 || profile_revision == 0 {
            None
        } else {
            Some(Self {
                document_id,
                source_version,
                profile_revision,
            })
        }
    }

    /// Returns the stable document identity.
    #[must_use]
    pub const fn document_id(self) -> u64 {
        self.document_id
    }

    /// Returns the source version bound to this generation.
    #[must_use]
    pub const fn source_version(self) -> u64 {
        self.source_version
    }

    /// Returns the immutable profile revision.
    #[must_use]
    pub const fn profile_revision(self) -> u64 {
        self.profile_revision
    }
}

/// One source-linked token chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenChunk {
    occurrence_id: ChunkOccurrenceId,
    ordinal: usize,
    start_byte: usize,
    end_byte: usize,
    start_char: usize,
    end_char: usize,
    token_count: usize,
    original_text: String,
    retrieval_text: String,
    structure_kind: StructureKind,
    structure_path: Vec<String>,
    content_hash: u64,
    parent_occurrence_id: Option<ChunkOccurrenceId>,
    previous_occurrence_id: Option<ChunkOccurrenceId>,
    next_occurrence_id: Option<ChunkOccurrenceId>,
}

impl TokenChunk {
    /// Returns the stable occurrence identity.
    #[must_use]
    pub const fn occurrence_id(&self) -> ChunkOccurrenceId {
        self.occurrence_id
    }

    /// Returns the zero-based document ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// Returns the inclusive original-source byte offset.
    #[must_use]
    pub const fn start_byte(&self) -> usize {
        self.start_byte
    }

    /// Returns the exclusive original-source byte offset.
    #[must_use]
    pub const fn end_byte(&self) -> usize {
        self.end_byte
    }

    /// Returns the inclusive original-source Unicode scalar offset.
    #[must_use]
    pub const fn start_char(&self) -> usize {
        self.start_char
    }

    /// Returns the exclusive original-source Unicode scalar offset.
    #[must_use]
    pub const fn end_char(&self) -> usize {
        self.end_char
    }

    /// Returns the original source substring used for citation.
    #[must_use]
    pub fn original_text(&self) -> &str {
        &self.original_text
    }

    /// Returns parser-normalized retrieval text.
    #[must_use]
    pub fn retrieval_text(&self) -> &str {
        &self.retrieval_text
    }

    /// Returns the number of certified tokenizer tokens.
    #[must_use]
    pub const fn token_count(&self) -> usize {
        self.token_count
    }

    /// Returns the owning structural kind.
    #[must_use]
    pub const fn structure_kind(&self) -> StructureKind {
        self.structure_kind
    }

    /// Returns the heading/section path.
    #[must_use]
    pub fn structure_path(&self) -> &[String] {
        &self.structure_path
    }

    /// Returns the stable hash of normalized retrieval text.
    #[must_use]
    pub const fn content_hash(&self) -> u64 {
        self.content_hash
    }

    /// Returns the nearest owning heading occurrence, when one exists.
    #[must_use]
    pub const fn parent_occurrence_id(&self) -> Option<ChunkOccurrenceId> {
        self.parent_occurrence_id
    }

    /// Returns the preceding occurrence identity.
    #[must_use]
    pub const fn previous_occurrence_id(&self) -> Option<ChunkOccurrenceId> {
        self.previous_occurrence_id
    }

    /// Returns the following occurrence identity.
    #[must_use]
    pub const fn next_occurrence_id(&self) -> Option<ChunkOccurrenceId> {
        self.next_occurrence_id
    }
}

/// Token-aware parser/chunker failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenChunkError {
    /// A profile bound is invalid.
    InvalidProfile {
        /// Invalid field.
        field: &'static str,
        /// Stable reason.
        reason: &'static str,
    },
    /// The raw document is too large.
    DocumentTooLarge {
        /// Observed bytes.
        actual: usize,
        /// Maximum bytes.
        maximum: usize,
    },
    /// The token stream is too large.
    TooManyTokens {
        /// Maximum admitted tokens.
        maximum: usize,
    },
    /// The output would contain too many chunks.
    TooManyChunks {
        /// Maximum admitted chunks.
        maximum: usize,
    },
    /// Checked size arithmetic overflowed.
    ArithmeticOverflow,
    /// Structural parser output exceeded a frozen path or retained-byte bound.
    StructureLimitExceeded,
    /// Cooperative elapsed/cancellation admission stopped the operation.
    DeadlineExceeded,
}

impl fmt::Display for TokenChunkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProfile { field, reason } => {
                write!(formatter, "invalid token chunk profile {field}: {reason}")
            }
            Self::DocumentTooLarge { actual, maximum } => write!(
                formatter,
                "document bytes {actual} exceed the maximum {maximum}"
            ),
            Self::TooManyTokens { maximum } => {
                write!(formatter, "document exceeds the maximum {maximum} tokens")
            }
            Self::TooManyChunks { maximum } => {
                write!(formatter, "document exceeds the maximum {maximum} chunks")
            }
            Self::ArithmeticOverflow => formatter.write_str("chunk size arithmetic overflowed"),
            Self::StructureLimitExceeded => {
                formatter.write_str("document structure output exceeds a certified limit")
            }
            Self::DeadlineExceeded => formatter.write_str("document chunking deadline exceeded"),
        }
    }
}

impl std::error::Error for TokenChunkError {}

/// Result type for token-aware chunking.
pub type Result<T> = core::result::Result<T, TokenChunkError>;

/// Tokenizes contiguous Unicode words with exact UTF-8 byte offsets.
///
/// # Errors
///
/// Returns [`TokenChunkError::TooManyTokens`] before retaining more than the
/// certified document-token ceiling.
pub fn unicode_word_tokens(source: &str) -> Result<Vec<WordToken>> {
    unicode_word_tokens_with_checkpoint(source, &mut || true)
}

/// Counts `unicode_words_v1` tokens up to an explicit admission limit.
///
/// Returns `None` as soon as the input contains more than `maximum` tokens, so
/// callers can enforce token admission without allocating token strings.
#[must_use]
pub fn unicode_word_token_count_up_to(source: &str, maximum: usize) -> Option<usize> {
    let mut count = 0usize;
    let complete = visit_unicode_words(source, &mut || true, &mut |_, _| {
        if count >= maximum {
            return Ok(false);
        }
        count += 1;
        Ok(true)
    })
    .ok()?;
    complete.then_some(count)
}

/// Builds a structure-context prefix under the certified path and token limits.
///
/// Token admission uses the same `unicode_words_v1` boundaries as chunking.
/// Invalid paths fail closed; a segment that would cross the token budget and
/// every following segment are omitted.
#[must_use]
pub fn bounded_unicode_word_context_prefix(path: &[String]) -> Option<String> {
    validate_context_path(path)?;
    let mut parts = Vec::new();
    let mut tokens = 0usize;
    for part in path {
        let remaining = MAX_CONTEXT_PREFIX_TOKENS.checked_sub(tokens)?;
        let Some(part_tokens) = unicode_word_token_count_up_to(part, remaining) else {
            break;
        };
        parts.push(part.as_str());
        tokens += part_tokens;
    }
    (!parts.is_empty()).then(|| parts.join(" > "))
}

fn validate_context_path(path: &[String]) -> Option<()> {
    (path.len() <= MAX_STRUCTURE_DEPTH
        && path
            .iter()
            .all(|segment| !segment.is_empty() && segment.len() <= MAX_STRUCTURE_SEGMENT_BYTES))
    .then_some(())
}

/// Parses and chunks one bounded document deterministically.
///
/// # Errors
///
/// Returns a typed error when the source/profile exceeds a certified bound or
/// when checked output arithmetic overflows. Input is never silently truncated.
pub fn chunk_document_tokens(
    source: &str,
    parser: DocumentParser,
    profile: TokenChunkProfile,
) -> Result<Vec<TokenChunk>> {
    let identity = ChunkIdentityContext {
        document_id: 1,
        source_version: 1,
        profile_revision: 1,
    };
    chunk_document_tokens_with_identity(source, parser, profile, identity)
}

/// Parses and chunks a document with explicit immutable source lineage.
///
/// # Errors
///
/// Returns a typed error when the source/profile exceeds a certified bound or
/// when checked output arithmetic overflows. Input is never silently truncated.
pub fn chunk_document_tokens_with_identity(
    source: &str,
    parser: DocumentParser,
    profile: TokenChunkProfile,
    identity: ChunkIdentityContext,
) -> Result<Vec<TokenChunk>> {
    chunk_document_tokens_with_identity_and_checkpoint(source, parser, profile, identity, || true)
}

/// Parses and chunks a document while consulting a cooperative checkpoint.
///
/// The callback is invoked before and throughout parsing, tokenization, and
/// output construction. Returning `false` fails closed before more work is
/// performed. This keeps the deterministic parser independent of any runtime
/// clock while allowing PostgreSQL and worker adapters to enforce deadlines.
///
/// # Errors
///
/// Returns [`TokenChunkError::DeadlineExceeded`] when `checkpoint` rejects the
/// operation, or the same bounded parser errors as
/// [`chunk_document_tokens_with_identity`].
pub fn chunk_document_tokens_with_identity_and_checkpoint<F>(
    source: &str,
    parser: DocumentParser,
    profile: TokenChunkProfile,
    identity: ChunkIdentityContext,
    mut checkpoint: F,
) -> Result<Vec<TokenChunk>>
where
    F: FnMut() -> bool,
{
    require_checkpoint(&mut checkpoint)?;
    if source.len() > profile.max_document_bytes {
        return Err(TokenChunkError::DocumentTooLarge {
            actual: source.len(),
            maximum: profile.max_document_bytes,
        });
    }
    let blocks = match parser {
        DocumentParser::PlainTextV1 => plain_blocks(source, &mut checkpoint)?,
        DocumentParser::MarkdownV1 => markdown_blocks(source, &mut checkpoint)?,
        DocumentParser::HtmlV1 => html_blocks(source, &mut checkpoint)?,
    };
    let mut drafts = Vec::new();
    let mut draft_budget = RetainedOutputBudget::default();
    let mut document_tokens = 0usize;
    for block in blocks {
        require_checkpoint(&mut checkpoint)?;
        append_block_chunks(
            source,
            &block,
            profile,
            &mut drafts,
            &mut draft_budget,
            &mut document_tokens,
            &mut checkpoint,
        )?;
    }
    if drafts.len() > super::chunking::MAX_CHUNKS_PER_DOCUMENT {
        return Err(TokenChunkError::TooManyChunks {
            maximum: super::chunking::MAX_CHUNKS_PER_DOCUMENT,
        });
    }
    let identities = drafts
        .iter()
        .enumerate()
        .map(|(ordinal, draft)| occurrence_id(parser, identity, ordinal, draft))
        .collect::<Vec<_>>();
    let parents = parent_occurrences(&drafts, &identities);
    let character_offsets = character_offsets(source, &drafts, &mut checkpoint)?;
    let mut chunks = Vec::new();
    let mut output_budget = RetainedOutputBudget::default();
    for (ordinal, draft) in drafts.into_iter().enumerate() {
        if ordinal % 1_024 == 0 {
            require_checkpoint(&mut checkpoint)?;
        }
        let original_text = &source[draft.start_byte..draft.end_byte];
        let text_bytes = original_text
            .len()
            .checked_add(draft.retrieval_text.len())
            .ok_or(TokenChunkError::ArithmeticOverflow)?;
        output_budget.admit_record::<TokenChunk>(text_bytes, &draft.path)?;
        chunks.push(TokenChunk {
            occurrence_id: identities[ordinal],
            ordinal,
            start_byte: draft.start_byte,
            end_byte: draft.end_byte,
            start_char: character_offsets[ordinal].0,
            end_char: character_offsets[ordinal].1,
            token_count: draft.token_count,
            original_text: original_text.to_owned(),
            retrieval_text: draft.retrieval_text,
            structure_kind: draft.kind,
            structure_path: draft.path,
            content_hash: draft.content_hash,
            parent_occurrence_id: parents[ordinal],
            previous_occurrence_id: ordinal.checked_sub(1).map(|index| identities[index]),
            next_occurrence_id: identities.get(ordinal + 1).copied(),
        });
    }
    Ok(chunks)
}

#[derive(Clone, Debug)]
struct ParsedBlock {
    start_byte: usize,
    end_byte: usize,
    retrieval_text: String,
    kind: StructureKind,
    path: Vec<String>,
    retrieval_maps_source: bool,
}

#[derive(Debug, Default)]
struct RetainedOutputBudget {
    bytes: usize,
}

impl RetainedOutputBudget {
    fn admit_record<T>(&mut self, text_bytes: usize, path: &[String]) -> Result<()> {
        let path_bytes = path.iter().try_fold(
            path.len()
                .checked_mul(size_of::<String>())
                .ok_or(TokenChunkError::ArithmeticOverflow)?,
            |total, segment| {
                total
                    .checked_add(segment.len())
                    .ok_or(TokenChunkError::ArithmeticOverflow)
            },
        )?;
        let projected = self
            .bytes
            .checked_add(size_of::<T>())
            .and_then(|total| total.checked_add(text_bytes))
            .and_then(|total| total.checked_add(path_bytes))
            .ok_or(TokenChunkError::ArithmeticOverflow)?;
        if projected > MAX_TOKEN_CHUNK_OUTPUT_BYTES {
            return Err(TokenChunkError::StructureLimitExceeded);
        }
        self.bytes = projected;
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct ChunkDraft {
    start_byte: usize,
    end_byte: usize,
    token_count: usize,
    retrieval_text: String,
    kind: StructureKind,
    path: Vec<String>,
    content_hash: u64,
}

fn invalid_profile(field: &'static str, reason: &'static str) -> TokenChunkError {
    TokenChunkError::InvalidProfile { field, reason }
}

fn push_token(tokens: &mut Vec<WordToken>, source: &str, start: usize, end: usize) -> Result<()> {
    if tokens.len() >= MAX_TOKENS_PER_DOCUMENT {
        return Err(TokenChunkError::TooManyTokens {
            maximum: MAX_TOKENS_PER_DOCUMENT,
        });
    }
    tokens.push(WordToken {
        start_byte: start,
        end_byte: end,
        text: source[start..end].to_owned(),
    });
    Ok(())
}

fn append_block_chunks(
    source: &str,
    block: &ParsedBlock,
    profile: TokenChunkProfile,
    drafts: &mut Vec<ChunkDraft>,
    retained: &mut RetainedOutputBudget,
    document_tokens: &mut usize,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<()> {
    require_checkpoint(checkpoint)?;
    let tokens = unicode_word_tokens_with_checkpoint(&block.retrieval_text, checkpoint)?;
    add_document_tokens(document_tokens, tokens.len())?;
    if tokens.is_empty() {
        return Ok(());
    }
    let mut start_index = 0;
    while start_index < tokens.len() {
        require_checkpoint(checkpoint)?;
        if drafts.len() >= super::chunking::MAX_CHUNKS_PER_DOCUMENT {
            return Err(TokenChunkError::TooManyChunks {
                maximum: super::chunking::MAX_CHUNKS_PER_DOCUMENT,
            });
        }
        let remaining = tokens.len() - start_index;
        let mut window_tokens = if remaining <= profile.max_tokens {
            remaining
        } else {
            profile.target_tokens
        };
        if remaining > window_tokens {
            let next_start = start_index
                .checked_add(window_tokens)
                .and_then(|end| end.checked_sub(profile.overlap_tokens))
                .ok_or(TokenChunkError::ArithmeticOverflow)?;
            let next_remaining = tokens.len() - next_start;
            if next_remaining < profile.min_tokens {
                window_tokens = remaining.min(profile.max_tokens);
            }
        }
        let end_index = start_index
            .checked_add(window_tokens)
            .ok_or(TokenChunkError::ArithmeticOverflow)?
            .min(tokens.len());
        let retrieval_start = tokens[start_index].start_byte;
        let retrieval_end = tokens[end_index - 1].end_byte;
        let is_complete_block = start_index == 0 && end_index == tokens.len();
        let retrieval_text = if is_complete_block {
            block.retrieval_text.trim()
        } else {
            &block.retrieval_text[retrieval_start..retrieval_end]
        };
        let (start_byte, end_byte) = if block.retrieval_maps_source && is_complete_block {
            (block.start_byte, block.end_byte)
        } else if block.retrieval_maps_source {
            (
                block
                    .start_byte
                    .checked_add(retrieval_start)
                    .ok_or(TokenChunkError::ArithmeticOverflow)?,
                block
                    .start_byte
                    .checked_add(retrieval_end)
                    .ok_or(TokenChunkError::ArithmeticOverflow)?,
            )
        } else {
            (block.start_byte, block.end_byte)
        };
        if end_byte > source.len() || start_byte >= end_byte {
            return Err(TokenChunkError::ArithmeticOverflow);
        }
        retained.admit_record::<ChunkDraft>(retrieval_text.len(), &block.path)?;
        drafts.push(ChunkDraft {
            start_byte,
            end_byte,
            token_count: end_index - start_index,
            content_hash: stable_hash(retrieval_text.as_bytes()),
            retrieval_text: retrieval_text.to_owned(),
            kind: block.kind,
            path: block.path.clone(),
        });
        if end_index == tokens.len() {
            break;
        }
        start_index = end_index.saturating_sub(profile.overlap_tokens);
    }
    Ok(())
}

fn add_document_tokens(document_tokens: &mut usize, block_tokens: usize) -> Result<()> {
    let next = document_tokens
        .checked_add(block_tokens)
        .ok_or(TokenChunkError::ArithmeticOverflow)?;
    if next > MAX_TOKENS_PER_DOCUMENT {
        return Err(TokenChunkError::TooManyTokens {
            maximum: MAX_TOKENS_PER_DOCUMENT,
        });
    }
    *document_tokens = next;
    Ok(())
}

fn unicode_word_tokens_with_checkpoint(
    source: &str,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<WordToken>> {
    let mut tokens = Vec::new();
    visit_unicode_words(source, checkpoint, &mut |start, end| {
        push_token(&mut tokens, source, start, end)?;
        Ok(true)
    })?;
    Ok(tokens)
}

fn visit_unicode_words(
    source: &str,
    checkpoint: &mut impl FnMut() -> bool,
    visitor: &mut impl FnMut(usize, usize) -> Result<bool>,
) -> Result<bool> {
    let mut start = None;
    for (index, (offset, character)) in source.char_indices().enumerate() {
        if index % 4_096 == 0 {
            require_checkpoint(checkpoint)?;
        }
        let word = character.is_alphanumeric()
            || (character == '\''
                && start.is_some()
                && source[offset + 1..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric));
        match (start, word) {
            (None, true) => start = Some(offset),
            (Some(token_start), false) => {
                if !visitor(token_start, offset)? {
                    return Ok(false);
                }
                start = None;
            }
            _ => {}
        }
    }
    if let Some(token_start) = start
        && !visitor(token_start, source.len())?
    {
        return Ok(false);
    }
    Ok(true)
}

fn parent_occurrences(
    drafts: &[ChunkDraft],
    identities: &[ChunkOccurrenceId],
) -> Vec<Option<ChunkOccurrenceId>> {
    let mut headings = BTreeMap::<Vec<String>, ChunkOccurrenceId>::new();
    let mut parents = Vec::with_capacity(drafts.len());
    for (ordinal, draft) in drafts.iter().enumerate() {
        let maximum_depth = if draft.kind == StructureKind::Heading {
            draft.path.len().saturating_sub(1)
        } else {
            draft.path.len()
        };
        let parent = (1..=maximum_depth)
            .rev()
            .find_map(|depth| headings.get(&draft.path[..depth]).copied());
        parents.push(parent);
        if draft.kind == StructureKind::Heading {
            headings.retain(|path, _| !path.starts_with(&draft.path));
            headings.insert(draft.path.clone(), identities[ordinal]);
        }
    }
    parents
}

fn character_offsets(
    source: &str,
    drafts: &[ChunkDraft],
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<(usize, usize)>> {
    let mut boundaries = Vec::with_capacity(drafts.len().saturating_mul(2));
    for (ordinal, draft) in drafts.iter().enumerate() {
        boundaries.push((draft.start_byte, ordinal, false));
        boundaries.push((draft.end_byte, ordinal, true));
    }
    boundaries.sort_unstable_by_key(|(offset, _, _)| *offset);
    let mut result = vec![(0usize, 0usize); drafts.len()];
    let mut characters = source.char_indices().peekable();
    let mut character_count = 0usize;
    for (index, (boundary, ordinal, is_end)) in boundaries.into_iter().enumerate() {
        if index % 1_024 == 0 {
            require_checkpoint(checkpoint)?;
        }
        while characters
            .peek()
            .is_some_and(|(offset, _)| *offset < boundary)
        {
            characters.next();
            character_count = character_count
                .checked_add(1)
                .ok_or(TokenChunkError::ArithmeticOverflow)?;
        }
        if !source.is_char_boundary(boundary) {
            return Err(TokenChunkError::ArithmeticOverflow);
        }
        if is_end {
            result[ordinal].1 = character_count;
        } else {
            result[ordinal].0 = character_count;
        }
    }
    Ok(result)
}

fn require_checkpoint(checkpoint: &mut impl FnMut() -> bool) -> Result<()> {
    if checkpoint() {
        Ok(())
    } else {
        Err(TokenChunkError::DeadlineExceeded)
    }
}

fn occurrence_id(
    parser: DocumentParser,
    identity: ChunkIdentityContext,
    ordinal: usize,
    draft: &ChunkDraft,
) -> ChunkOccurrenceId {
    let mut hash = stable_hash(parser.as_str().as_bytes());
    for bytes in [
        identity.document_id.to_le_bytes(),
        identity.source_version.to_le_bytes(),
        identity.profile_revision.to_le_bytes(),
        ordinal.to_le_bytes(),
        draft.start_byte.to_le_bytes(),
        draft.end_byte.to_le_bytes(),
        draft.content_hash.to_le_bytes(),
    ] {
        hash = fnv1a(hash, &bytes);
    }
    for segment in &draft.path {
        hash = fnv1a(hash, segment.as_bytes());
    }
    ChunkOccurrenceId((hash & i64::MAX as u64).max(1))
}

fn stable_hash(bytes: &[u8]) -> u64 {
    fnv1a(0xcbf2_9ce4_8422_2325, bytes) & i64::MAX as u64
}

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_token_limit_accumulates_across_blocks() {
        let mut total = MAX_TOKENS_PER_DOCUMENT - 2;
        assert_eq!(add_document_tokens(&mut total, 2), Ok(()));
        assert_eq!(total, MAX_TOKENS_PER_DOCUMENT);
        assert_eq!(
            add_document_tokens(&mut total, 1),
            Err(TokenChunkError::TooManyTokens {
                maximum: MAX_TOKENS_PER_DOCUMENT,
            })
        );
        assert_eq!(total, MAX_TOKENS_PER_DOCUMENT);
    }
}
