//! Portable structural format for one bounded segmented-HNSW publication.

use std::{collections::BTreeSet, fmt};

const MAGIC: [u8; 8] = *b"PGCTXHSD";
const HEADER_BYTES: usize = 72;
const SEGMENT_BYTES: usize = 88;
const MAX_SEGMENTS: usize = 64;

/// Current segmented-HNSW directory format.
pub const HNSW_SEGMENT_DIRECTORY_VERSION: u32 = 1;

/// One immutable graph segment extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswSegmentDescriptor {
    /// Directory-local stable identity.
    pub segment_id: u64,
    /// Artifact generation of the segment payload.
    pub generation: u64,
    /// First inclusive main-fork block.
    pub start_block: u64,
    /// First exclusive main-fork block.
    pub end_block: u64,
    /// Authoritative live rows represented by the segment.
    pub row_count: u64,
    /// Encoded payload bytes.
    pub payload_bytes: u64,
    /// Payload checksum.
    pub checksum: u64,
    /// Source revision captured by the segment.
    pub source_revision: u64,
    /// Configuration revision captured by the segment.
    pub config_revision: u64,
    /// First inclusive frozen mutation-log block, or `u64::MAX` when absent.
    pub mutation_start_block: u64,
    /// First exclusive frozen mutation-log block, or `u64::MAX` when absent.
    pub mutation_end_block: u64,
}

/// One exact active-delta extent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswDeltaDescriptor {
    /// First inclusive delta block, or `u64::MAX` when unopened.
    pub start_block: u64,
    /// First exclusive delta block, or `u64::MAX` when unopened.
    pub end_block: u64,
    /// Ordered records including tombstones.
    pub record_count: u64,
    /// Delta generation incremented on rotation.
    pub generation: u64,
}

/// One complete, immutable directory publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HnswSegmentDirectory {
    /// Atomic publication generation.
    pub generation: u64,
    /// Shared source revision required of every segment.
    pub source_revision: u64,
    /// Shared configuration revision required of every segment.
    pub config_revision: u64,
    /// Immutable graph segments in segment-id order.
    pub segments: Vec<HnswSegmentDescriptor>,
    /// The one exact active delta.
    pub delta: HnswDeltaDescriptor,
}

/// Directory corruption or construction error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HnswSegmentDirectoryError {
    /// Header or entry bytes are truncated.
    Truncated,
    /// Magic is not the segmented-HNSW directory magic.
    BadMagic,
    /// The format is not readable by this build.
    RebuildRequired(u32),
    /// The encoded entry count exceeds the hard format bound.
    TooManySegments,
    /// A generation or revision is zero.
    ZeroIdentity,
    /// Entries are not strictly ordered by unique segment identity.
    InvalidOrder,
    /// One immutable segment extent is empty or invalid.
    InvalidExtent,
    /// Two immutable segment extents overlap.
    OverlappingExtents,
    /// A segment carries a different source/config revision.
    MixedRevision,
    /// The active-delta extent is inconsistent.
    InvalidDelta,
    /// Encoded length arithmetic overflowed.
    LengthOverflow,
}

impl fmt::Display for HnswSegmentDirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Truncated => "segmented HNSW directory is truncated",
            Self::BadMagic => "segmented HNSW directory magic is invalid",
            Self::RebuildRequired(version) => {
                return write!(
                    formatter,
                    "segmented HNSW directory format {version} requires rebuild"
                );
            }
            Self::TooManySegments => "segmented HNSW directory has too many segments",
            Self::ZeroIdentity => "segmented HNSW directory has a zero identity",
            Self::InvalidOrder => {
                "segmented HNSW directory segment identities are not ordered and unique"
            }
            Self::InvalidExtent => "segmented HNSW directory has an invalid segment extent",
            Self::OverlappingExtents => "segmented HNSW directory segment extents overlap",
            Self::MixedRevision => "segmented HNSW directory contains mixed revisions",
            Self::InvalidDelta => "segmented HNSW directory has an invalid delta extent",
            Self::LengthOverflow => "segmented HNSW directory length overflow",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for HnswSegmentDirectoryError {}

/// Encodes a structurally valid segmented-HNSW directory.
pub fn encode_hnsw_segment_directory(
    directory: &HnswSegmentDirectory,
) -> Result<Vec<u8>, HnswSegmentDirectoryError> {
    validate(directory)?;
    let length = directory
        .segments
        .len()
        .checked_mul(SEGMENT_BYTES)
        .and_then(|entries| HEADER_BYTES.checked_add(entries))
        .ok_or(HnswSegmentDirectoryError::LengthOverflow)?;
    let mut output = Vec::with_capacity(length);
    output.extend_from_slice(&MAGIC);
    output.extend_from_slice(&HNSW_SEGMENT_DIRECTORY_VERSION.to_le_bytes());
    output.extend_from_slice(
        &u32::try_from(directory.segments.len())
            .map_err(|_| HnswSegmentDirectoryError::TooManySegments)?
            .to_le_bytes(),
    );
    output.extend_from_slice(&directory.generation.to_le_bytes());
    output.extend_from_slice(&directory.source_revision.to_le_bytes());
    output.extend_from_slice(&directory.config_revision.to_le_bytes());
    output.extend_from_slice(&directory.delta.start_block.to_le_bytes());
    output.extend_from_slice(&directory.delta.end_block.to_le_bytes());
    output.extend_from_slice(&directory.delta.record_count.to_le_bytes());
    output.extend_from_slice(&directory.delta.generation.to_le_bytes());
    for segment in &directory.segments {
        for value in [
            segment.segment_id,
            segment.generation,
            segment.start_block,
            segment.end_block,
            segment.row_count,
            segment.payload_bytes,
            segment.checksum,
            segment.source_revision,
            segment.config_revision,
            segment.mutation_start_block,
            segment.mutation_end_block,
        ] {
            output.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(output)
}

/// Decodes and fully validates a segmented-HNSW directory.
pub fn decode_hnsw_segment_directory(
    input: &[u8],
) -> Result<HnswSegmentDirectory, HnswSegmentDirectoryError> {
    if input.len() < HEADER_BYTES {
        return Err(HnswSegmentDirectoryError::Truncated);
    }
    if input[..8] != MAGIC {
        return Err(HnswSegmentDirectoryError::BadMagic);
    }
    let version = read_u32(input, 8)?;
    if version != HNSW_SEGMENT_DIRECTORY_VERSION {
        return Err(HnswSegmentDirectoryError::RebuildRequired(version));
    }
    let count = usize::try_from(read_u32(input, 12)?)
        .map_err(|_| HnswSegmentDirectoryError::TooManySegments)?;
    if count > MAX_SEGMENTS {
        return Err(HnswSegmentDirectoryError::TooManySegments);
    }
    let expected = count
        .checked_mul(SEGMENT_BYTES)
        .and_then(|entries| HEADER_BYTES.checked_add(entries))
        .ok_or(HnswSegmentDirectoryError::LengthOverflow)?;
    if input.len() != expected {
        return Err(HnswSegmentDirectoryError::Truncated);
    }
    let mut directory = HnswSegmentDirectory {
        generation: read_u64(input, 16)?,
        source_revision: read_u64(input, 24)?,
        config_revision: read_u64(input, 32)?,
        segments: Vec::with_capacity(count),
        delta: HnswDeltaDescriptor {
            start_block: read_u64(input, 40)?,
            end_block: read_u64(input, 48)?,
            record_count: read_u64(input, 56)?,
            generation: read_u64(input, 64)?,
        },
    };
    for index in 0..count {
        let offset = HEADER_BYTES + index * SEGMENT_BYTES;
        directory.segments.push(HnswSegmentDescriptor {
            segment_id: read_u64(input, offset)?,
            generation: read_u64(input, offset + 8)?,
            start_block: read_u64(input, offset + 16)?,
            end_block: read_u64(input, offset + 24)?,
            row_count: read_u64(input, offset + 32)?,
            payload_bytes: read_u64(input, offset + 40)?,
            checksum: read_u64(input, offset + 48)?,
            source_revision: read_u64(input, offset + 56)?,
            config_revision: read_u64(input, offset + 64)?,
            mutation_start_block: read_u64(input, offset + 72)?,
            mutation_end_block: read_u64(input, offset + 80)?,
        });
    }
    validate(&directory)?;
    Ok(directory)
}

fn validate(directory: &HnswSegmentDirectory) -> Result<(), HnswSegmentDirectoryError> {
    if directory.generation == 0
        || directory.source_revision == 0
        || directory.config_revision == 0
        || directory.delta.generation == 0
    {
        return Err(HnswSegmentDirectoryError::ZeroIdentity);
    }
    if directory.segments.len() > MAX_SEGMENTS {
        return Err(HnswSegmentDirectoryError::TooManySegments);
    }
    let mut ids = BTreeSet::new();
    let mut previous_id = None;
    for segment in &directory.segments {
        if segment.segment_id == 0 || segment.generation == 0 {
            return Err(HnswSegmentDirectoryError::ZeroIdentity);
        }
        if previous_id.is_some_and(|previous| previous >= segment.segment_id)
            || !ids.insert(segment.segment_id)
        {
            return Err(HnswSegmentDirectoryError::InvalidOrder);
        }
        previous_id = Some(segment.segment_id);
        let graph_present = segment.start_block < segment.end_block
            && segment.row_count > 0
            && segment.payload_bytes > 0;
        let graph_absent = segment.start_block == segment.end_block
            && segment.row_count == 0
            && segment.payload_bytes == 0
            && segment.checksum == 0;
        let mutation_present = segment.mutation_start_block < segment.mutation_end_block;
        let mutation_absent =
            segment.mutation_start_block == u64::MAX && segment.mutation_end_block == u64::MAX;
        if (!graph_present && !graph_absent)
            || (!mutation_present && !mutation_absent)
            || (graph_absent && mutation_absent)
        {
            return Err(HnswSegmentDirectoryError::InvalidExtent);
        }
        if segment.source_revision != directory.source_revision
            || segment.config_revision != directory.config_revision
        {
            return Err(HnswSegmentDirectoryError::MixedRevision);
        }
    }
    let mut extents: Vec<_> = directory
        .segments
        .iter()
        .filter(|segment| segment.start_block < segment.end_block)
        .map(|segment| (segment.start_block, segment.end_block))
        .collect();
    extents.sort_unstable();
    if extents.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(HnswSegmentDirectoryError::OverlappingExtents);
    }
    let unopened = directory.delta.start_block == u64::MAX
        && directory.delta.end_block == u64::MAX
        && directory.delta.record_count == 0;
    let opened = directory.delta.start_block < directory.delta.end_block;
    if !unopened && !opened {
        return Err(HnswSegmentDirectoryError::InvalidDelta);
    }
    Ok(())
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, HnswSegmentDirectoryError> {
    let bytes = input
        .get(offset..offset + 4)
        .ok_or(HnswSegmentDirectoryError::Truncated)?;
    Ok(u32::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| HnswSegmentDirectoryError::Truncated)?,
    ))
}

fn read_u64(input: &[u8], offset: usize) -> Result<u64, HnswSegmentDirectoryError> {
    let bytes = input
        .get(offset..offset + 8)
        .ok_or(HnswSegmentDirectoryError::Truncated)?;
    Ok(u64::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| HnswSegmentDirectoryError::Truncated)?,
    ))
}
