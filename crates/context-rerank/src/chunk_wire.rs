//! Bounded wire contract for deterministic document parsing and chunking.

use std::{collections::BTreeSet, fmt};

use context_build::{
    ChunkIdentityContext, DocumentParser, MAX_CERTIFIED_PROFILE_OVERLAP_TOKENS,
    MAX_CHUNKS_PER_DOCUMENT, MAX_STRUCTURE_DEPTH, MAX_STRUCTURE_SEGMENT_BYTES,
    MAX_TOKEN_CHUNK_DOCUMENT_BYTES, MAX_TOKENS_PER_CHUNK, StructureKind, TokenChunkProfile,
    bounded_unicode_word_context_prefix, chunk_document_tokens_with_identity_and_checkpoint,
};
use serde::{Deserialize, Deserializer, Serialize, de::SeqAccess, de::Visitor};

/// Frozen request discriminator for the automatic-chunking worker.
pub const CHUNK_WORKER_REQUEST_VERSION: &str = "chunk_worker_request_v1";
/// Frozen response discriminator for the automatic-chunking worker.
pub const CHUNK_WORKER_RESPONSE_VERSION: &str = "chunk_worker_response_v1";
/// Frozen operational-failure frame discriminator.
pub const CHUNK_WORKER_FAILURE_VERSION: &str = "chunk_worker_failure_v1";
/// Maximum encoded request or response frame.
pub const MAX_CHUNK_WORKER_FRAME_BYTES: usize = 40 * 1024 * 1024;
/// Maximum complete parsing/chunking duration for one worker request.
pub const MAX_CHUNK_WORKER_ELAPSED_MICROS: u64 = 120_000_000;
const MAX_SOURCE_HASH_BYTES: usize = 128;
const MAX_CHUNK_OUTPUT_TEXT_BYTES: usize = 32 * 1024 * 1024;

/// Serialized immutable token-chunk profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireChunkProfileV1 {
    /// Preferred chunk token count.
    pub target_tokens: usize,
    /// Hard chunk token ceiling.
    pub max_tokens: usize,
    /// Preferred final-window minimum.
    pub min_tokens: usize,
    /// Token overlap between adjacent windows.
    pub overlap_tokens: usize,
    /// Hard raw document-byte ceiling.
    pub max_document_bytes: usize,
    /// Whether deterministic structure context is stored separately.
    #[serde(default)]
    pub include_structure_context: bool,
}

/// One bounded chunk-worker request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkWorkerRequestV1 {
    /// Exact wire discriminator.
    pub version: String,
    /// Non-zero caller correlation identity.
    pub request_id: u64,
    /// Non-zero stable document identity.
    pub document_id: u64,
    /// Non-zero authoritative source version.
    pub source_version: u64,
    /// Bounded source digest supplied by PostgreSQL.
    pub source_hash: String,
    /// Non-zero immutable chunk-profile revision.
    pub profile_revision: u64,
    /// Parser revision name.
    pub parser: String,
    /// Tokenizer revision name.
    pub tokenizer_revision: String,
    /// Inclusive request-expiry instant in Unix microseconds.
    pub expires_at_micros: u64,
    /// Immutable token and source bounds.
    pub profile: WireChunkProfileV1,
    /// Authoritative document text admitted by PostgreSQL.
    pub source_text: String,
}

impl ChunkWorkerRequestV1 {
    /// Parses and validates one bounded JSON request.
    ///
    /// # Errors
    ///
    /// Returns a content-free typed error for malformed, unsupported, or
    /// oversized input.
    pub fn from_json(payload: &str) -> Result<Self, ChunkWorkerError> {
        if payload.len() > MAX_CHUNK_WORKER_FRAME_BYTES {
            return Err(ChunkWorkerError::FrameTooLarge);
        }
        let request = serde_json::from_str::<Self>(payload)
            .map_err(|_| ChunkWorkerError::MalformedRequest)?;
        request.validate()?;
        Ok(request)
    }

    /// Serializes one validated bounded request.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the request is invalid or its encoded frame
    /// exceeds the certified ceiling.
    pub fn to_json(&self) -> Result<String, ChunkWorkerError> {
        self.validate()?;
        let payload =
            serde_json::to_string(self).map_err(|_| ChunkWorkerError::MalformedRequest)?;
        if payload.len() > MAX_CHUNK_WORKER_FRAME_BYTES {
            return Err(ChunkWorkerError::FrameTooLarge);
        }
        Ok(payload)
    }

    fn validate(&self) -> Result<(), ChunkWorkerError> {
        if self.version != CHUNK_WORKER_REQUEST_VERSION
            || self.request_id == 0
            || self.document_id == 0
            || self.source_version == 0
            || self.profile_revision == 0
            || self.expires_at_micros == 0
        {
            return Err(ChunkWorkerError::MalformedRequest);
        }
        if self.source_hash.is_empty()
            || self.source_hash.len() > MAX_SOURCE_HASH_BYTES
            || !self
                .source_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ChunkWorkerError::MalformedRequest);
        }
        if self.tokenizer_revision != "unicode_words_v1" {
            return Err(ChunkWorkerError::UnsupportedTokenizer);
        }
        if self.source_text.len() > MAX_TOKEN_CHUNK_DOCUMENT_BYTES
            || self.source_text.len() > self.profile.max_document_bytes
        {
            return Err(ChunkWorkerError::DocumentTooLarge);
        }
        if self.profile.max_tokens > MAX_TOKENS_PER_CHUNK
            || self.profile.overlap_tokens > MAX_CERTIFIED_PROFILE_OVERLAP_TOKENS
        {
            return Err(ChunkWorkerError::InvalidProfile);
        }
        TokenChunkProfile::new(
            self.profile.target_tokens,
            self.profile.max_tokens,
            self.profile.min_tokens,
            self.profile.overlap_tokens,
            self.profile.max_document_bytes,
        )
        .map_err(|_| ChunkWorkerError::InvalidProfile)?;
        parser(&self.parser)?;
        Ok(())
    }
}

/// One source-linked chunk returned by the worker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WireTokenChunkV1 {
    /// Globally stable occurrence identity.
    pub occurrence_id: u64,
    /// Zero-based deterministic document ordinal.
    pub ordinal: usize,
    /// Inclusive source byte offset.
    pub start_byte: usize,
    /// Exclusive source byte offset.
    pub end_byte: usize,
    /// Inclusive source character offset.
    pub start_char: usize,
    /// Exclusive source character offset.
    pub end_char: usize,
    /// Certified tokenizer token count.
    pub token_count: usize,
    /// Exact source citation text.
    pub original_text: String,
    /// Parser-normalized retrieval text.
    pub retrieval_text: String,
    /// Structural kind name.
    pub structure_kind: String,
    /// Bounded heading/section lineage.
    pub structure_path: Vec<String>,
    /// Stable normalized-content hash.
    pub content_hash: u64,
    /// Nearest owning heading occurrence.
    pub parent_occurrence_id: Option<u64>,
    /// Previous occurrence in document order.
    pub previous_occurrence_id: Option<u64>,
    /// Next occurrence in document order.
    pub next_occurrence_id: Option<u64>,
    /// Optional deterministic context kept separate from citation text.
    pub context_prefix: Option<String>,
    /// Hash of the optional context prefix.
    pub context_prefix_hash: Option<u64>,
}

/// One complete bounded chunk-worker response.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkWorkerResponseV1 {
    /// Exact wire discriminator.
    pub version: String,
    /// Caller correlation identity.
    pub request_id: u64,
    /// Stable document identity.
    pub document_id: u64,
    /// Authoritative source version.
    pub source_version: u64,
    /// Authoritative source digest.
    pub source_hash: String,
    /// Immutable profile revision.
    pub profile_revision: u64,
    /// True only for a complete validated chunk set.
    pub complete: bool,
    /// Deterministically ordered chunks.
    #[serde(deserialize_with = "deserialize_chunks")]
    pub chunks: Vec<WireTokenChunkV1>,
}

/// Bounded content-free operational failure emitted by the chunk worker.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChunkWorkerFailureV1 {
    /// Exact failure-frame discriminator.
    pub version: String,
    /// Validated request identity, absent only before request parsing succeeds.
    pub request_id: Option<u64>,
    /// Stable machine-readable failure category.
    pub error_code: String,
}

impl ChunkWorkerFailureV1 {
    /// Creates a content-free failure frame.
    #[must_use]
    pub fn new(request_id: Option<u64>, error: ChunkWorkerError) -> Self {
        Self {
            version: CHUNK_WORKER_FAILURE_VERSION.to_owned(),
            request_id: request_id.filter(|value| *value > 0),
            error_code: error.code().to_owned(),
        }
    }

    /// Serializes one bounded failure frame.
    ///
    /// # Errors
    ///
    /// Returns a malformed-response error only on unexpected serialization
    /// failure.
    pub fn to_json(&self) -> Result<String, ChunkWorkerError> {
        serde_json::to_string(self).map_err(|_| ChunkWorkerError::MalformedResponse)
    }
}

impl ChunkWorkerResponseV1 {
    /// Parses and validates one bounded JSON response.
    ///
    /// # Errors
    ///
    /// Returns a content-free typed error for malformed or incomplete output.
    pub fn from_json(payload: &str) -> Result<Self, ChunkWorkerError> {
        if payload.len() > MAX_CHUNK_WORKER_FRAME_BYTES {
            return Err(ChunkWorkerError::FrameTooLarge);
        }
        let response = serde_json::from_str::<Self>(payload)
            .map_err(|_| ChunkWorkerError::MalformedResponse)?;
        response.validate()?;
        Ok(response)
    }

    /// Serializes one complete validated response.
    ///
    /// # Errors
    ///
    /// Returns a typed error when output is incomplete, invalid, or too large.
    pub fn to_json(&self) -> Result<String, ChunkWorkerError> {
        self.validate()?;
        let payload =
            serde_json::to_string(self).map_err(|_| ChunkWorkerError::MalformedResponse)?;
        if payload.len() > MAX_CHUNK_WORKER_FRAME_BYTES {
            return Err(ChunkWorkerError::FrameTooLarge);
        }
        Ok(payload)
    }

    fn validate(&self) -> Result<(), ChunkWorkerError> {
        if self.version != CHUNK_WORKER_RESPONSE_VERSION
            || self.request_id == 0
            || self.document_id == 0
            || self.source_version == 0
            || self.profile_revision == 0
            || !self.complete
            || self.source_hash.is_empty()
            || self.source_hash.len() > MAX_SOURCE_HASH_BYTES
        {
            return Err(ChunkWorkerError::MalformedResponse);
        }
        let mut identities = BTreeSet::new();
        let mut output_bytes = 0usize;
        for (ordinal, chunk) in self.chunks.iter().enumerate() {
            if chunk.ordinal != ordinal
                || chunk.occurrence_id == 0
                || chunk.start_byte >= chunk.end_byte
                || chunk.start_char >= chunk.end_char
                || chunk.token_count == 0
                || chunk.token_count > MAX_TOKENS_PER_CHUNK
                || chunk.structure_path.len() > MAX_STRUCTURE_DEPTH
                || chunk
                    .structure_path
                    .iter()
                    .any(|part| part.is_empty() || part.len() > MAX_STRUCTURE_SEGMENT_BYTES)
                || !identities.insert(chunk.occurrence_id)
            {
                return Err(ChunkWorkerError::MalformedResponse);
            }
            let expected_previous = ordinal
                .checked_sub(1)
                .map(|index| self.chunks[index].occurrence_id);
            let expected_next = self.chunks.get(ordinal + 1).map(|next| next.occurrence_id);
            if chunk.previous_occurrence_id != expected_previous
                || chunk.next_occurrence_id != expected_next
            {
                return Err(ChunkWorkerError::MalformedResponse);
            }
            output_bytes = output_bytes
                .checked_add(chunk.original_text.len())
                .and_then(|value| value.checked_add(chunk.retrieval_text.len()))
                .and_then(|value| {
                    value.checked_add(chunk.context_prefix.as_ref().map_or(0, String::len))
                })
                .ok_or(ChunkWorkerError::OutputTooLarge)?;
        }
        if output_bytes > MAX_CHUNK_OUTPUT_TEXT_BYTES {
            return Err(ChunkWorkerError::OutputTooLarge);
        }
        Ok(())
    }
}

/// Parses and chunks one already-admitted document request.
///
/// `now_micros` uses the same inclusive expiry rule as PostgreSQL cleanup.
///
/// # Errors
///
/// Returns a content-free typed failure for expiry, unsupported contracts,
/// invalid profiles, or any certified chunking bound.
pub fn process_chunk_request(
    request: ChunkWorkerRequestV1,
    now_micros: u64,
) -> Result<ChunkWorkerResponseV1, ChunkWorkerError> {
    request.validate()?;
    if now_micros >= request.expires_at_micros {
        return Err(ChunkWorkerError::Expired);
    }
    let allowed_micros = request
        .expires_at_micros
        .saturating_sub(now_micros)
        .min(MAX_CHUNK_WORKER_ELAPSED_MICROS);
    let started = std::time::Instant::now();
    let parser = parser(&request.parser)?;
    let profile = TokenChunkProfile::new(
        request.profile.target_tokens,
        request.profile.max_tokens,
        request.profile.min_tokens,
        request.profile.overlap_tokens,
        request.profile.max_document_bytes,
    )
    .map_err(|_| ChunkWorkerError::InvalidProfile)?;
    let identity = ChunkIdentityContext::new(
        request.document_id,
        request.source_version,
        request.profile_revision,
    )
    .ok_or(ChunkWorkerError::MalformedRequest)?;
    let chunks = chunk_document_tokens_with_identity_and_checkpoint(
        &request.source_text,
        parser,
        profile,
        identity,
        || started.elapsed().as_micros() < u128::from(allowed_micros),
    )
    .map_err(|error| match error {
        context_build::TokenChunkError::DocumentTooLarge { .. }
        | context_build::TokenChunkError::TooManyTokens { .. }
        | context_build::TokenChunkError::TooManyChunks { .. } => {
            ChunkWorkerError::DocumentTooLarge
        }
        context_build::TokenChunkError::InvalidProfile { .. } => ChunkWorkerError::InvalidProfile,
        context_build::TokenChunkError::StructureLimitExceeded => ChunkWorkerError::OutputTooLarge,
        context_build::TokenChunkError::DeadlineExceeded => ChunkWorkerError::Expired,
        context_build::TokenChunkError::ArithmeticOverflow => ChunkWorkerError::OutputTooLarge,
    })?;
    let chunks = chunks
        .into_iter()
        .map(|chunk| {
            let context_prefix = if request.profile.include_structure_context {
                bounded_unicode_word_context_prefix(chunk.structure_path())
            } else {
                None
            };
            let context_prefix_hash = context_prefix.as_deref().map(stable_hash);
            WireTokenChunkV1 {
                occurrence_id: chunk.occurrence_id().get(),
                ordinal: chunk.ordinal(),
                start_byte: chunk.start_byte(),
                end_byte: chunk.end_byte(),
                start_char: chunk.start_char(),
                end_char: chunk.end_char(),
                token_count: chunk.token_count(),
                original_text: chunk.original_text().to_owned(),
                retrieval_text: chunk.retrieval_text().to_owned(),
                structure_kind: structure_kind(chunk.structure_kind()).to_owned(),
                structure_path: chunk.structure_path().to_vec(),
                content_hash: chunk.content_hash(),
                parent_occurrence_id: chunk.parent_occurrence_id().map(|value| value.get()),
                previous_occurrence_id: chunk.previous_occurrence_id().map(|value| value.get()),
                next_occurrence_id: chunk.next_occurrence_id().map(|value| value.get()),
                context_prefix,
                context_prefix_hash,
            }
        })
        .collect();
    let response = ChunkWorkerResponseV1 {
        version: CHUNK_WORKER_RESPONSE_VERSION.to_owned(),
        request_id: request.request_id,
        document_id: request.document_id,
        source_version: request.source_version,
        source_hash: request.source_hash,
        profile_revision: request.profile_revision,
        complete: true,
        chunks,
    };
    response.validate()?;
    Ok(response)
}

/// Content-free chunk-worker failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChunkWorkerError {
    /// Encoded input exceeded the frame ceiling.
    FrameTooLarge,
    /// Request JSON or identity was invalid.
    MalformedRequest,
    /// Response JSON or invariants were invalid.
    MalformedResponse,
    /// Parser revision is not certified.
    UnsupportedParser,
    /// Tokenizer revision is not certified.
    UnsupportedTokenizer,
    /// Profile bounds are inconsistent.
    InvalidProfile,
    /// Source exceeds a document/token/chunk ceiling.
    DocumentTooLarge,
    /// Response projection exceeds a staging/frame ceiling.
    OutputTooLarge,
    /// Request reached its inclusive expiry instant.
    Expired,
}

impl ChunkWorkerError {
    /// Returns the stable machine-readable failure code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::FrameTooLarge => "frame_too_large",
            Self::MalformedRequest => "malformed_request",
            Self::MalformedResponse => "malformed_response",
            Self::UnsupportedParser => "unsupported_parser",
            Self::UnsupportedTokenizer => "unsupported_tokenizer",
            Self::InvalidProfile => "invalid_profile",
            Self::DocumentTooLarge => "document_too_large",
            Self::OutputTooLarge => "output_too_large",
            Self::Expired => "expired",
        }
    }
}

impl fmt::Display for ChunkWorkerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::FrameTooLarge => "chunk worker frame exceeds the byte limit",
            Self::MalformedRequest => "malformed chunk worker request",
            Self::MalformedResponse => "malformed chunk worker response",
            Self::UnsupportedParser => "unsupported chunk worker parser",
            Self::UnsupportedTokenizer => "unsupported chunk worker tokenizer",
            Self::InvalidProfile => "invalid chunk worker profile",
            Self::DocumentTooLarge => "chunk worker document exceeds a certified limit",
            Self::OutputTooLarge => "chunk worker output exceeds a certified limit",
            Self::Expired => "chunk worker request expired",
        })
    }
}

impl std::error::Error for ChunkWorkerError {}

fn parser(value: &str) -> Result<DocumentParser, ChunkWorkerError> {
    match value {
        "plain_text_v1" => Ok(DocumentParser::PlainTextV1),
        "markdown_v1" => Ok(DocumentParser::MarkdownV1),
        "html_v1" => Ok(DocumentParser::HtmlV1),
        _ => Err(ChunkWorkerError::UnsupportedParser),
    }
}

fn structure_kind(value: StructureKind) -> &'static str {
    match value {
        StructureKind::Document => "document",
        StructureKind::Heading => "heading",
        StructureKind::Paragraph => "paragraph",
        StructureKind::List => "list",
        StructureKind::Table => "table",
        StructureKind::Code => "code",
        StructureKind::Sentence => "sentence",
        StructureKind::TokenWindow => "token_window",
    }
}

fn stable_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf2_9ce4_8422_2325, |mut hash, byte| {
        hash ^= u64::from(byte);
        hash.wrapping_mul(0x0000_0100_0000_01b3)
    }) & i64::MAX as u64
}

fn deserialize_chunks<'de, D>(deserializer: D) -> Result<Vec<WireTokenChunkV1>, D::Error>
where
    D: Deserializer<'de>,
{
    struct BoundedChunks;

    impl<'de> Visitor<'de> for BoundedChunks {
        type Value = Vec<WireTokenChunkV1>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a bounded chunk sequence")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut chunks = Vec::new();
            while let Some(chunk) = sequence.next_element()? {
                if chunks.len() >= MAX_CHUNKS_PER_DOCUMENT {
                    return Err(serde::de::Error::custom("chunk sequence exceeds limit"));
                }
                chunks.push(chunk);
            }
            Ok(chunks)
        }
    }

    deserializer.deserialize_seq(BoundedChunks)
}
