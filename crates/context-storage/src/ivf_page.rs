//! Canonical checksummed metadata for page-native IVFFlat generations.

use core::fmt;

use context_core::policy::MAX_VECTOR_DIMENSIONS;

use crate::{FNV_OFFSET_BASIS, checksum_bytes};

const META_MAGIC: u32 = 0x4956_4646;
const CHECKSUM_OFFSET: usize = 128;
const CHUNK_MAGIC: u32 = 0x4956_5047;
const CHUNK_VERSION: u16 = 1;
const CHUNK_CHECKSUM_OFFSET: usize = 32;

/// Clean-break page-native IVFFlat format version.
pub const IVF_PAGE_META_VERSION: u16 = 4;
/// Encoded metapage payload length.
pub const IVF_PAGE_META_BYTES: usize = 136;
/// Maximum list count accepted by the SQL reloption and storage verifier.
pub const IVF_PAGE_MAX_LISTS: usize = 32_768;
/// Bytes preceding data in each checksummed section page item.
pub const IVF_PAGE_CHUNK_HEADER_BYTES: usize = 40;

/// Logical section stored by a page chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum IvfPageChunkKind {
    /// Full-precision list centroids.
    Centroids = 1,
    /// Contiguous list posting extents.
    Directory = 2,
    /// Optional shared quantization artifact.
    Codec = 3,
    /// Fixed-width base postings.
    Postings = 4,
    /// One chronological foreground mutation stream.
    Delta = 5,
}

/// A malformed or unsupported page-native IVFFlat generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IvfPageError {
    /// An older or newer format must be rebuilt from the source relation.
    RebuildRequired {
        /// Unsupported stored page format.
        version: u16,
    },
    /// A storage invariant was violated.
    Invalid(&'static str),
    /// The complete metapage checksum did not match.
    ChecksumMismatch,
}

impl fmt::Display for IvfPageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RebuildRequired { version } => {
                write!(formatter, "IVFFlat page format {version} requires REINDEX")
            }
            Self::Invalid(reason) => write!(formatter, "invalid IVFFlat metapage: {reason}"),
            Self::ChecksumMismatch => formatter.write_str("IVFFlat metapage checksum mismatch"),
        }
    }
}

impl std::error::Error for IvfPageError {}

/// Canonical publication record for one immutable base generation and delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IvfPageMeta {
    /// Opclass metric discriminator.
    pub metric_tag: u16,
    /// Source vector dimensions.
    pub dimensions: u32,
    /// Published centroid/list count.
    pub lists: u32,
    /// Published base posting count.
    pub tuples: u64,
    /// Encoded centroid section length.
    pub centroid_bytes: u64,
    /// Encoded posting-directory section length.
    pub directory_bytes: u64,
    /// Encoded optional codec section length.
    pub codec_bytes: u64,
    /// Encoded base posting section length.
    pub posting_bytes: u64,
    /// First centroid page, inclusive.
    pub centroid_start: u32,
    /// First page after centroids.
    pub centroid_end: u32,
    /// First directory page, inclusive.
    pub directory_start: u32,
    /// First page after the directory.
    pub directory_end: u32,
    /// First codec page, inclusive.
    pub codec_start: u32,
    /// First page after the codec.
    pub codec_end: u32,
    /// First base posting page, inclusive.
    pub posting_start: u32,
    /// First page after base postings.
    pub posting_end: u32,
    /// First delta page, inclusive.
    pub delta_start: u32,
    /// First page after delta records.
    pub delta_end: u32,
    /// Chronological delta record count.
    pub delta_count: u64,
    /// Immutable generation identity.
    pub generation: u64,
    /// Shared codec revision, or zero for full precision.
    pub codec_revision: u64,
    /// Bytes in each encoded posting code, or zero for full precision.
    pub codec_code_width: u32,
    /// Shared codec mode discriminator.
    pub codec_mode: u16,
    /// PostgreSQL parallel workers that produced the base generation.
    pub build_workers: u16,
}

impl IvfPageMeta {
    /// Returns a valid empty generation publication.
    #[must_use]
    pub const fn empty() -> Self {
        Self::empty_at(1, 1)
    }

    /// Returns an empty generation published after superseded relation pages.
    #[must_use]
    pub const fn empty_at(cursor: u32, generation: u64) -> Self {
        Self {
            metric_tag: 0,
            dimensions: 0,
            lists: 0,
            tuples: 0,
            centroid_bytes: 0,
            directory_bytes: 0,
            codec_bytes: 0,
            posting_bytes: 0,
            centroid_start: cursor,
            centroid_end: cursor,
            directory_start: cursor,
            directory_end: cursor,
            codec_start: cursor,
            codec_end: cursor,
            posting_start: cursor,
            posting_end: cursor,
            delta_start: cursor,
            delta_end: cursor,
            delta_count: 0,
            generation,
            codec_revision: 0,
            codec_code_width: 0,
            codec_mode: 0,
            build_workers: 0,
        }
    }

    /// Encodes this publication with a checksum over every field.
    #[must_use]
    pub fn encode(self) -> [u8; IVF_PAGE_META_BYTES] {
        let mut bytes = [0_u8; IVF_PAGE_META_BYTES];
        bytes[..4].copy_from_slice(&META_MAGIC.to_le_bytes());
        bytes[4..6].copy_from_slice(&IVF_PAGE_META_VERSION.to_le_bytes());
        bytes[6..8].copy_from_slice(&self.metric_tag.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.dimensions.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.lists.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.tuples.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.centroid_bytes.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.directory_bytes.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.codec_bytes.to_le_bytes());
        bytes[48..56].copy_from_slice(&self.posting_bytes.to_le_bytes());
        bytes[56..60].copy_from_slice(&self.centroid_start.to_le_bytes());
        bytes[60..64].copy_from_slice(&self.centroid_end.to_le_bytes());
        bytes[64..68].copy_from_slice(&self.directory_start.to_le_bytes());
        bytes[68..72].copy_from_slice(&self.directory_end.to_le_bytes());
        bytes[72..76].copy_from_slice(&self.codec_start.to_le_bytes());
        bytes[76..80].copy_from_slice(&self.codec_end.to_le_bytes());
        bytes[80..84].copy_from_slice(&self.posting_start.to_le_bytes());
        bytes[84..88].copy_from_slice(&self.posting_end.to_le_bytes());
        bytes[88..92].copy_from_slice(&self.delta_start.to_le_bytes());
        bytes[92..96].copy_from_slice(&self.delta_end.to_le_bytes());
        bytes[96..104].copy_from_slice(&self.delta_count.to_le_bytes());
        bytes[104..112].copy_from_slice(&self.generation.to_le_bytes());
        bytes[112..120].copy_from_slice(&self.codec_revision.to_le_bytes());
        bytes[120..124].copy_from_slice(&self.codec_code_width.to_le_bytes());
        bytes[124..126].copy_from_slice(&self.codec_mode.to_le_bytes());
        bytes[126..128].copy_from_slice(&self.build_workers.to_le_bytes());
        let checksum = checksum_bytes(FNV_OFFSET_BASIS, &bytes);
        bytes[CHECKSUM_OFFSET..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    /// Decodes and validates all non-page-count metadata before allocation.
    pub fn decode(bytes: &[u8]) -> Result<Self, IvfPageError> {
        if bytes.len() != IVF_PAGE_META_BYTES {
            return Err(IvfPageError::Invalid("item length is invalid"));
        }
        if read_u32(bytes, 0) != META_MAGIC {
            return Err(IvfPageError::Invalid("magic is invalid"));
        }
        let version = read_u16(bytes, 4);
        if version != IVF_PAGE_META_VERSION {
            return Err(IvfPageError::RebuildRequired { version });
        }
        let stored_checksum = read_u64(bytes, CHECKSUM_OFFSET);
        let mut canonical = bytes.to_vec();
        canonical[CHECKSUM_OFFSET..].fill(0);
        if checksum_bytes(FNV_OFFSET_BASIS, &canonical) != stored_checksum {
            return Err(IvfPageError::ChecksumMismatch);
        }
        let meta = Self {
            metric_tag: read_u16(bytes, 6),
            dimensions: read_u32(bytes, 8),
            lists: read_u32(bytes, 12),
            tuples: read_u64(bytes, 16),
            centroid_bytes: read_u64(bytes, 24),
            directory_bytes: read_u64(bytes, 32),
            codec_bytes: read_u64(bytes, 40),
            posting_bytes: read_u64(bytes, 48),
            centroid_start: read_u32(bytes, 56),
            centroid_end: read_u32(bytes, 60),
            directory_start: read_u32(bytes, 64),
            directory_end: read_u32(bytes, 68),
            codec_start: read_u32(bytes, 72),
            codec_end: read_u32(bytes, 76),
            posting_start: read_u32(bytes, 80),
            posting_end: read_u32(bytes, 84),
            delta_start: read_u32(bytes, 88),
            delta_end: read_u32(bytes, 92),
            delta_count: read_u64(bytes, 96),
            generation: read_u64(bytes, 104),
            codec_revision: read_u64(bytes, 112),
            codec_code_width: read_u32(bytes, 120),
            codec_mode: read_u16(bytes, 124),
            build_workers: read_u16(bytes, 126),
        };
        meta.validate_logical_layout()?;
        Ok(meta)
    }

    /// Validates section page counts and relation bounds before section reads.
    pub fn validate_page_extents(
        self,
        chunk_capacity: usize,
        relation_blocks: u32,
    ) -> Result<(), IvfPageError> {
        if chunk_capacity == 0 {
            return Err(IvfPageError::Invalid("chunk capacity is zero"));
        }
        validate_extent(
            self.centroid_start,
            self.centroid_end,
            self.centroid_bytes,
            chunk_capacity,
        )?;
        validate_extent(
            self.directory_start,
            self.directory_end,
            self.directory_bytes,
            chunk_capacity,
        )?;
        validate_extent(
            self.codec_start,
            self.codec_end,
            self.codec_bytes,
            chunk_capacity,
        )?;
        validate_extent(
            self.posting_start,
            self.posting_end,
            self.posting_bytes,
            chunk_capacity,
        )?;
        if self.delta_end > relation_blocks || self.posting_end > relation_blocks {
            return Err(IvfPageError::Invalid("page extent exceeds relation"));
        }
        if self.delta_count == 0 && self.delta_start != self.delta_end {
            return Err(IvfPageError::Invalid("empty delta has pages"));
        }
        if self.delta_count > 0 && self.delta_start == self.delta_end {
            return Err(IvfPageError::Invalid("non-empty delta has no pages"));
        }
        Ok(())
    }

    fn validate_logical_layout(self) -> Result<(), IvfPageError> {
        if self.centroid_start == 0
            || self.centroid_end < self.centroid_start
            || self.directory_start != self.centroid_end
            || self.directory_end < self.directory_start
            || self.codec_start != self.directory_end
            || self.codec_end < self.codec_start
            || self.posting_start != self.codec_end
            || self.posting_end < self.posting_start
            || self.delta_start != self.posting_end
            || self.delta_end < self.delta_start
            || self.generation == 0
        {
            return Err(IvfPageError::Invalid("section extent is invalid"));
        }
        if self.tuples == 0 && self.delta_count == 0 {
            if self.metric_tag != 0
                || self.dimensions != 0
                || self.lists != 0
                || self.centroid_bytes != 0
                || self.directory_bytes != 0
                || self.codec_bytes != 0
                || self.posting_bytes != 0
                || self.centroid_start != self.delta_end
                || self.codec_revision != 0
                || self.codec_code_width != 0
                || self.codec_mode != 0
            {
                return Err(IvfPageError::Invalid("empty generation is inconsistent"));
            }
            return Ok(());
        }
        let dimensions = usize::try_from(self.dimensions)
            .map_err(|_| IvfPageError::Invalid("dimensions exceed platform"))?;
        if dimensions == 0 || dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(IvfPageError::Invalid("dimensions exceed vector policy"));
        }
        if !(1..=6).contains(&self.metric_tag) {
            return Err(IvfPageError::Invalid("metric tag is invalid"));
        }
        if self.tuples == 0 {
            if self.lists != 0
                || self.centroid_bytes != 0
                || self.directory_bytes != 0
                || self.codec_bytes != 0
                || self.posting_bytes != 0
            {
                return Err(IvfPageError::Invalid(
                    "delta-only generation has base sections",
                ));
            }
        } else {
            let lists = usize::try_from(self.lists)
                .map_err(|_| IvfPageError::Invalid("lists exceed platform"))?;
            if lists == 0 || lists > IVF_PAGE_MAX_LISTS {
                return Err(IvfPageError::Invalid("lists exceed policy"));
            }
            let centroid_bytes = checked_product(lists, dimensions, 4)?;
            let directory_bytes = lists
                .checked_mul(16)
                .ok_or(IvfPageError::Invalid("directory length overflow"))?;
            let payload_width = if self.codec_code_width == 0 {
                dimensions
                    .checked_mul(4)
                    .ok_or(IvfPageError::Invalid("posting width overflow"))?
            } else {
                usize::try_from(self.codec_code_width)
                    .map_err(|_| IvfPageError::Invalid("codec width exceeds platform"))?
            };
            let posting_stride = 8usize
                .checked_add(payload_width)
                .ok_or(IvfPageError::Invalid("posting stride overflow"))?;
            let tuples = usize::try_from(self.tuples)
                .map_err(|_| IvfPageError::Invalid("tuple count exceeds platform"))?;
            let posting_bytes = tuples
                .checked_mul(posting_stride)
                .ok_or(IvfPageError::Invalid("posting length overflow"))?;
            if self.centroid_bytes != centroid_bytes as u64
                || self.directory_bytes != directory_bytes as u64
                || self.posting_bytes != posting_bytes as u64
            {
                return Err(IvfPageError::Invalid(
                    "section byte lengths are not canonical",
                ));
            }
        }
        let plain = self.codec_bytes == 0
            && self.codec_revision == 0
            && self.codec_code_width == 0
            && self.codec_mode == 0;
        let quantized = self.codec_bytes > 0
            && self.codec_revision > 0
            && self.codec_code_width > 0
            && matches!(self.codec_mode, 1..=3);
        if !plain && !quantized {
            return Err(IvfPageError::Invalid("codec binding is inconsistent"));
        }
        Ok(())
    }
}

/// Encodes one canonical checksummed section chunk.
pub fn encode_ivf_page_chunk(
    kind: IvfPageChunkKind,
    stream_id: u64,
    chunk_index: usize,
    chunk_count: usize,
    total_bytes: usize,
    chunk: &[u8],
) -> Result<Vec<u8>, IvfPageError> {
    if chunk_count == 0 || chunk_index >= chunk_count || chunk.len() > total_bytes {
        return Err(IvfPageError::Invalid("chunk identity is invalid"));
    }
    let chunk_index = u32::try_from(chunk_index)
        .map_err(|_| IvfPageError::Invalid("chunk index exceeds format"))?;
    let chunk_count = u32::try_from(chunk_count)
        .map_err(|_| IvfPageError::Invalid("chunk count exceeds format"))?;
    let total_bytes = u64::try_from(total_bytes)
        .map_err(|_| IvfPageError::Invalid("section length exceeds format"))?;
    let mut payload = vec![0_u8; IVF_PAGE_CHUNK_HEADER_BYTES + chunk.len()];
    payload[..4].copy_from_slice(&CHUNK_MAGIC.to_le_bytes());
    payload[4..6].copy_from_slice(&CHUNK_VERSION.to_le_bytes());
    payload[6..8].copy_from_slice(&(kind as u16).to_le_bytes());
    payload[8..16].copy_from_slice(&stream_id.to_le_bytes());
    payload[16..20].copy_from_slice(&chunk_index.to_le_bytes());
    payload[20..24].copy_from_slice(&chunk_count.to_le_bytes());
    payload[24..32].copy_from_slice(&total_bytes.to_le_bytes());
    payload[IVF_PAGE_CHUNK_HEADER_BYTES..].copy_from_slice(chunk);
    let checksum = checksum_bytes(FNV_OFFSET_BASIS, &payload);
    payload[CHUNK_CHECKSUM_OFFSET..IVF_PAGE_CHUNK_HEADER_BYTES]
        .copy_from_slice(&checksum.to_le_bytes());
    Ok(payload)
}

/// Validates one section chunk and returns only its data bytes.
pub fn decode_ivf_page_chunk(
    payload: &[u8],
    expected_kind: IvfPageChunkKind,
    expected_stream: u64,
    expected_index: usize,
    expected_count: usize,
    expected_total: usize,
    chunk_capacity: usize,
) -> Result<&[u8], IvfPageError> {
    let (kind, stream, index, count, total) = decode_chunk_header(payload)?;
    if kind != expected_kind
        || stream != expected_stream
        || index != expected_index
        || count != expected_count
        || total != expected_total
        || chunk_capacity == 0
        || count != expected_total.div_ceil(chunk_capacity)
    {
        return Err(IvfPageError::Invalid(
            "chunk identity disagrees with section",
        ));
    }
    let expected_data = if index + 1 == count {
        expected_total
            .checked_sub(index.saturating_mul(chunk_capacity))
            .ok_or(IvfPageError::Invalid("chunk offset exceeds section"))?
    } else {
        chunk_capacity
    };
    if payload.len() != IVF_PAGE_CHUNK_HEADER_BYTES + expected_data {
        return Err(IvfPageError::Invalid("chunk length is invalid"));
    }
    Ok(&payload[IVF_PAGE_CHUNK_HEADER_BYTES..])
}

/// Returns the validated identity of a delta stream's first page.
pub fn decode_ivf_delta_chunk_identity(
    payload: &[u8],
) -> Result<(u64, usize, usize, usize), IvfPageError> {
    let (kind, stream, index, count, total) = decode_chunk_header(payload)?;
    if kind != IvfPageChunkKind::Delta || index != 0 || count == 0 {
        return Err(IvfPageError::Invalid("delta chunk identity is invalid"));
    }
    Ok((stream, index, count, total))
}

fn decode_chunk_header(
    payload: &[u8],
) -> Result<(IvfPageChunkKind, u64, usize, usize, usize), IvfPageError> {
    if payload.len() < IVF_PAGE_CHUNK_HEADER_BYTES
        || read_u32(payload, 0) != CHUNK_MAGIC
        || read_u16(payload, 4) != CHUNK_VERSION
    {
        return Err(IvfPageError::Invalid("chunk header is invalid"));
    }
    let stored_checksum = read_u64(payload, CHUNK_CHECKSUM_OFFSET);
    let mut canonical = payload.to_vec();
    canonical[CHUNK_CHECKSUM_OFFSET..IVF_PAGE_CHUNK_HEADER_BYTES].fill(0);
    if checksum_bytes(FNV_OFFSET_BASIS, &canonical) != stored_checksum {
        return Err(IvfPageError::ChecksumMismatch);
    }
    let kind = match read_u16(payload, 6) {
        1 => IvfPageChunkKind::Centroids,
        2 => IvfPageChunkKind::Directory,
        3 => IvfPageChunkKind::Codec,
        4 => IvfPageChunkKind::Postings,
        5 => IvfPageChunkKind::Delta,
        _ => return Err(IvfPageError::Invalid("chunk kind is invalid")),
    };
    let index = usize::try_from(read_u32(payload, 16))
        .map_err(|_| IvfPageError::Invalid("chunk index exceeds platform"))?;
    let count = usize::try_from(read_u32(payload, 20))
        .map_err(|_| IvfPageError::Invalid("chunk count exceeds platform"))?;
    let total = usize::try_from(read_u64(payload, 24))
        .map_err(|_| IvfPageError::Invalid("section length exceeds platform"))?;
    Ok((kind, read_u64(payload, 8), index, count, total))
}

fn validate_extent(start: u32, end: u32, bytes: u64, capacity: usize) -> Result<(), IvfPageError> {
    let bytes = usize::try_from(bytes)
        .map_err(|_| IvfPageError::Invalid("section length exceeds platform"))?;
    let expected_pages = bytes.div_ceil(capacity);
    if usize::try_from(end.saturating_sub(start)).ok() != Some(expected_pages) {
        return Err(IvfPageError::Invalid(
            "section pages disagree with byte length",
        ));
    }
    Ok(())
}

fn checked_product(left: usize, middle: usize, right: usize) -> Result<usize, IvfPageError> {
    left.checked_mul(middle)
        .and_then(|value| value.checked_mul(right))
        .ok_or(IvfPageError::Invalid("section length overflow"))
}

fn read_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(input[offset..offset + 2].try_into().unwrap_or([0; 2]))
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap_or([0; 4]))
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(input[offset..offset + 8].try_into().unwrap_or([0; 8]))
}
