//! Portable checksummed IVFFlat generation artifacts.

use core::fmt;
use std::collections::BTreeSet;

use context_core::{DistanceMetric, policy::MAX_VECTOR_DIMENSIONS};

use crate::{FNV_OFFSET_BASIS, checksum_bytes};

const MAGIC: [u8; 8] = *b"PGCTXIVF";
const HEADER_LEN: usize = 112;
const CHECKSUM_OFFSET: usize = 96;
const ENDIAN_MARKER: u32 = 0x0102_0304;
const CODEC_PRESENT: u8 = 1;
const SECTION_ALIGNMENT: usize = 8;

/// Current portable IVFFlat generation format.
pub const CURRENT_IVF_ARTIFACT_VERSION: u16 = 1;

/// Failure while constructing, encoding, or attaching to an IVF artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IvfArtifactError {
    /// The stored generation format is not readable and must be rebuilt.
    RebuildRequired {
        /// Unsupported stored version.
        version: u16,
    },
    /// A bounded format invariant is invalid.
    Invalid(String),
    /// The stored checksum does not match the complete artifact.
    ChecksumMismatch,
}

impl fmt::Display for IvfArtifactError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RebuildRequired { version } => {
                write!(
                    formatter,
                    "IVFFlat artifact version {version} requires rebuild"
                )
            }
            Self::Invalid(reason) => write!(formatter, "invalid IVFFlat artifact: {reason}"),
            Self::ChecksumMismatch => formatter.write_str("IVFFlat artifact checksum mismatch"),
        }
    }
}

impl std::error::Error for IvfArtifactError {}

/// One full-precision posting in an owned IVF generation.
#[derive(Debug, Clone, PartialEq)]
pub struct IvfArtifactPosting {
    point_id: u64,
    vector: Vec<f32>,
}

impl IvfArtifactPosting {
    /// Creates a locally valid non-zero posting.
    ///
    /// # Errors
    ///
    /// Rejects a zero point identity or an empty, non-finite, or over-policy
    /// vector.
    pub fn new(point_id: u64, vector: Vec<f32>) -> Result<Self, IvfArtifactError> {
        if point_id == 0 {
            return Err(invalid("point id is zero"));
        }
        validate_vector(&vector)?;
        Ok(Self { point_id, vector })
    }

    /// Returns the stable source point identity.
    #[must_use]
    pub const fn point_id(&self) -> u64 {
        self.point_id
    }
    /// Returns full-precision coordinates.
    #[must_use]
    pub fn vector(&self) -> &[f32] {
        &self.vector
    }
}

/// Owned IVF generation ready for portable encoding.
#[derive(Debug, Clone, PartialEq)]
pub struct IvfGenerationArtifact {
    metric: DistanceMetric,
    dimensions: usize,
    centroids: Vec<Vec<f32>>,
    lists: Vec<Vec<IvfArtifactPosting>>,
    codec_revision: Option<u64>,
}

impl IvfGenerationArtifact {
    /// Creates a generation after validating every cross-section invariant.
    ///
    /// # Errors
    ///
    /// Rejects empty or ragged centroids, over-policy dimensions, mismatched
    /// posting lists, duplicate point identities, and invalid codec revisions.
    pub fn new(
        metric: DistanceMetric,
        centroids: Vec<Vec<f32>>,
        lists: Vec<Vec<IvfArtifactPosting>>,
        codec_revision: Option<u64>,
    ) -> Result<Self, IvfArtifactError> {
        let dimensions = centroids
            .first()
            .ok_or_else(|| invalid("centroids are empty"))?
            .len();
        if dimensions == 0 {
            return Err(invalid("dimensions are zero"));
        }
        if centroids.len() != lists.len() {
            return Err(invalid("centroid and posting-list counts differ"));
        }
        for centroid in &centroids {
            validate_vector(centroid)?;
            if centroid.len() != dimensions {
                return Err(invalid("centroids are ragged"));
            }
        }
        let mut ids = BTreeSet::new();
        for posting in lists.iter().flatten() {
            if posting.vector().len() != dimensions {
                return Err(invalid("posting dimensions differ from centroids"));
            }
            if !ids.insert(posting.point_id()) {
                return Err(invalid("duplicate point id"));
            }
        }
        if codec_revision == Some(0) {
            return Err(invalid("codec revision is zero"));
        }
        Ok(Self {
            metric,
            dimensions,
            centroids,
            lists,
            codec_revision,
        })
    }

    /// Returns the canonical distance metric.
    #[must_use]
    pub const fn metric(&self) -> DistanceMetric {
        self.metric
    }
    /// Returns vector dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }
    /// Returns centroids in stable list order.
    #[must_use]
    pub fn centroids(&self) -> &[Vec<f32>] {
        &self.centroids
    }
    /// Returns posting lists in stable list order.
    #[must_use]
    pub fn lists(&self) -> &[Vec<IvfArtifactPosting>] {
        &self.lists
    }
    /// Returns the optional shared codec revision.
    #[must_use]
    pub const fn codec_revision(&self) -> Option<u64> {
        self.codec_revision
    }
}

/// Encodes a complete IVF generation with canonical section offsets.
///
/// # Errors
///
/// Returns [`IvfArtifactError::Invalid`] when checked byte accounting exceeds
/// the portable field or segment-size bounds.
pub fn encode_ivf_generation(
    artifact: &IvfGenerationArtifact,
) -> Result<Vec<u8>, IvfArtifactError> {
    let list_count = artifact.centroids.len();
    let posting_count = artifact.lists.iter().try_fold(0usize, |count, list| {
        count
            .checked_add(list.len())
            .ok_or_else(|| invalid("posting count overflow"))
    })?;
    let centroid_len = checked_product(list_count, artifact.dimensions, 4, "centroid bytes")?;
    let directory_entries = list_count
        .checked_add(1)
        .ok_or_else(|| invalid("directory overflow"))?;
    let directory_len = directory_entries
        .checked_mul(8)
        .ok_or_else(|| invalid("directory overflow"))?;
    let posting_stride = aligned(
        8usize
            .checked_add(
                artifact
                    .dimensions
                    .checked_mul(4)
                    .ok_or_else(|| invalid("posting stride overflow"))?,
            )
            .ok_or_else(|| invalid("posting stride overflow"))?,
    )?;
    let posting_len = posting_count
        .checked_mul(posting_stride)
        .ok_or_else(|| invalid("posting bytes overflow"))?;
    let centroid_offset = HEADER_LEN;
    let directory_offset = aligned(
        centroid_offset
            .checked_add(centroid_len)
            .ok_or_else(|| invalid("centroid end overflow"))?,
    )?;
    let posting_offset = aligned(
        directory_offset
            .checked_add(directory_len)
            .ok_or_else(|| invalid("directory end overflow"))?,
    )?;
    let total_len = posting_offset
        .checked_add(posting_len)
        .ok_or_else(|| invalid("artifact length overflow"))?;
    if total_len > crate::MAX_SEGMENT_PAYLOAD_BYTES {
        return Err(invalid("artifact exceeds segment payload policy"));
    }
    let mut output = vec![0_u8; total_len];
    output[..8].copy_from_slice(&MAGIC);
    output[8..10].copy_from_slice(&CURRENT_IVF_ARTIFACT_VERSION.to_le_bytes());
    let header_len = u16::try_from(HEADER_LEN).map_err(|_| invalid("header length overflow"))?;
    output[10..12].copy_from_slice(&header_len.to_le_bytes());
    output[12..16].copy_from_slice(&ENDIAN_MARKER.to_le_bytes());
    output[16] = metric_code(artifact.metric);
    output[17] = u8::from(artifact.codec_revision.is_some()) * CODEC_PRESENT;
    put_u32(&mut output, 24, artifact.dimensions)?;
    put_u32(&mut output, 28, list_count)?;
    put_u64(&mut output, 32, posting_count)?;
    put_u64(&mut output, 40, centroid_offset)?;
    put_u64(&mut output, 48, centroid_len)?;
    put_u64(&mut output, 56, directory_offset)?;
    put_u64(&mut output, 64, directory_len)?;
    put_u64(&mut output, 72, posting_offset)?;
    put_u64(&mut output, 80, posting_len)?;
    output[88..96].copy_from_slice(&artifact.codec_revision.unwrap_or(0).to_le_bytes());

    let mut cursor = centroid_offset;
    for value in artifact.centroids.iter().flatten() {
        output[cursor..cursor + 4].copy_from_slice(&value.to_le_bytes());
        cursor += 4;
    }
    let mut cumulative = 0usize;
    for (list_index, list) in artifact.lists.iter().enumerate() {
        put_u64(&mut output, directory_offset + list_index * 8, cumulative)?;
        cumulative = cumulative
            .checked_add(list.len())
            .ok_or_else(|| invalid("directory overflow"))?;
    }
    put_u64(&mut output, directory_offset + list_count * 8, cumulative)?;
    let mut record_offset = posting_offset;
    for posting in artifact.lists.iter().flatten() {
        output[record_offset..record_offset + 8].copy_from_slice(&posting.point_id().to_le_bytes());
        let mut value_offset = record_offset + 8;
        for value in posting.vector() {
            output[value_offset..value_offset + 4].copy_from_slice(&value.to_le_bytes());
            value_offset += 4;
        }
        record_offset += posting_stride;
    }
    let checksum = artifact_checksum(&output);
    output[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 8].copy_from_slice(&checksum.to_le_bytes());
    Ok(output)
}

/// Validated borrowed IVF generation view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IvfGenerationView<'a> {
    input: &'a [u8],
    metric: DistanceMetric,
    dimensions: usize,
    list_count: usize,
    posting_count: usize,
    centroid_offset: usize,
    directory_offset: usize,
    posting_offset: usize,
    posting_stride: usize,
    codec_revision: Option<u64>,
}

impl<'a> IvfGenerationView<'a> {
    /// Validates and attaches to one complete artifact without copying sections.
    ///
    /// # Errors
    ///
    /// Rejects unsupported versions, checksum mismatches, non-canonical
    /// bounds, dimensions above policy, and invalid centroid/posting content.
    pub fn attach(input: &'a [u8]) -> Result<Self, IvfArtifactError> {
        if input.len() < HEADER_LEN {
            return Err(invalid("header is truncated"));
        }
        if input[..8] != MAGIC {
            return Err(invalid("magic is invalid"));
        }
        let version = read_u16(input, 8)?;
        if version != CURRENT_IVF_ARTIFACT_VERSION {
            return Err(IvfArtifactError::RebuildRequired { version });
        }
        if usize::from(read_u16(input, 10)?) != HEADER_LEN
            || read_u32(input, 12)? != ENDIAN_MARKER
            || input[18..24].iter().any(|byte| *byte != 0)
            || input[104..112].iter().any(|byte| *byte != 0)
        {
            return Err(invalid("header fields are invalid"));
        }
        let metric = decode_metric(input[16])?;
        let flags = input[17];
        if flags & !CODEC_PRESENT != 0 {
            return Err(invalid("unknown flags"));
        }
        let dimensions = read_u32(input, 24)? as usize;
        let list_count = read_u32(input, 28)? as usize;
        let posting_count = u64_to_usize(read_u64(input, 32)?, "posting count")?;
        if dimensions == 0 || list_count == 0 {
            return Err(invalid("dimensions or lists are zero"));
        }
        if dimensions > MAX_VECTOR_DIMENSIONS {
            return Err(invalid("dimensions exceed vector policy"));
        }
        let centroid_offset = u64_to_usize(read_u64(input, 40)?, "centroid offset")?;
        let centroid_len = u64_to_usize(read_u64(input, 48)?, "centroid length")?;
        let directory_offset = u64_to_usize(read_u64(input, 56)?, "directory offset")?;
        let directory_len = u64_to_usize(read_u64(input, 64)?, "directory length")?;
        let posting_offset = u64_to_usize(read_u64(input, 72)?, "posting offset")?;
        let posting_len = u64_to_usize(read_u64(input, 80)?, "posting length")?;
        let raw_revision = read_u64(input, 88)?;
        let codec_revision = match (flags & CODEC_PRESENT != 0, raw_revision) {
            (false, 0) => None,
            (true, revision @ 1..) => Some(revision),
            _ => return Err(invalid("codec flag and revision disagree")),
        };
        let expected_centroid_len = checked_product(list_count, dimensions, 4, "centroid bytes")?;
        let expected_directory_len = list_count
            .checked_add(1)
            .and_then(|value| value.checked_mul(8))
            .ok_or_else(|| invalid("directory overflow"))?;
        let posting_stride = aligned(
            8usize
                .checked_add(
                    dimensions
                        .checked_mul(4)
                        .ok_or_else(|| invalid("posting stride overflow"))?,
                )
                .ok_or_else(|| invalid("posting stride overflow"))?,
        )?;
        let expected_posting_len = posting_count
            .checked_mul(posting_stride)
            .ok_or_else(|| invalid("posting bytes overflow"))?;
        let expected_directory_offset = aligned(
            HEADER_LEN
                .checked_add(expected_centroid_len)
                .ok_or_else(|| invalid("centroid end overflow"))?,
        )?;
        let expected_posting_offset = aligned(
            expected_directory_offset
                .checked_add(expected_directory_len)
                .ok_or_else(|| invalid("directory end overflow"))?,
        )?;
        let expected_total = expected_posting_offset
            .checked_add(expected_posting_len)
            .ok_or_else(|| invalid("artifact end overflow"))?;
        if centroid_offset != HEADER_LEN
            || centroid_len != expected_centroid_len
            || directory_offset != expected_directory_offset
            || directory_len != expected_directory_len
            || posting_offset != expected_posting_offset
            || posting_len != expected_posting_len
            || input.len() != expected_total
        {
            return Err(invalid("section bounds are not canonical"));
        }
        if input[centroid_offset + centroid_len..directory_offset]
            .iter()
            .any(|byte| *byte != 0)
            || input[directory_offset + directory_len..posting_offset]
                .iter()
                .any(|byte| *byte != 0)
        {
            return Err(invalid("section padding is non-zero"));
        }
        verify_checksum(input)?;
        let view = Self {
            input,
            metric,
            dimensions,
            list_count,
            posting_count,
            centroid_offset,
            directory_offset,
            posting_offset,
            posting_stride,
            codec_revision,
        };
        view.validate_contents()?;
        Ok(view)
    }

    /// Returns the format version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        CURRENT_IVF_ARTIFACT_VERSION
    }
    /// Returns the metric.
    #[must_use]
    pub const fn metric(&self) -> DistanceMetric {
        self.metric
    }
    /// Returns dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }
    /// Returns list count.
    #[must_use]
    pub const fn list_count(&self) -> usize {
        self.list_count
    }
    /// Returns posting count.
    #[must_use]
    pub const fn posting_count(&self) -> usize {
        self.posting_count
    }
    /// Returns optional codec revision.
    #[must_use]
    pub const fn codec_revision(&self) -> Option<u64> {
        self.codec_revision
    }
    /// Returns one borrowed-decoding centroid iterator.
    #[must_use]
    pub fn centroid(&self, list: usize) -> Option<IvfFloatIter<'a>> {
        if list >= self.list_count {
            return None;
        }
        let start = self.centroid_offset + list * self.dimensions * 4;
        Some(IvfFloatIter {
            bytes: &self.input[start..start + self.dimensions * 4],
            cursor: 0,
        })
    }
    /// Returns list length.
    ///
    /// # Errors
    ///
    /// Returns [`IvfArtifactError::Invalid`] when `list` is outside the
    /// validated directory.
    pub fn list_len(&self, list: usize) -> Result<usize, IvfArtifactError> {
        let (start, end) = self.list_bounds(list)?;
        Ok(end - start)
    }
    /// Returns one borrowed-decoding posting.
    ///
    /// # Errors
    ///
    /// Returns [`IvfArtifactError::Invalid`] when the list or posting offset
    /// is outside the validated generation.
    pub fn posting(
        &self,
        list: usize,
        offset: usize,
    ) -> Result<IvfPostingView<'a>, IvfArtifactError> {
        let (start, end) = self.list_bounds(list)?;
        let global = start
            .checked_add(offset)
            .ok_or_else(|| invalid("posting offset overflow"))?;
        if global >= end {
            return Err(invalid("posting offset is outside the list"));
        }
        let record = self.posting_offset + global * self.posting_stride;
        Ok(IvfPostingView {
            point_id: read_u64(self.input, record)?,
            vector_bytes: &self.input[record + 8..record + 8 + self.dimensions * 4],
        })
    }

    fn list_bounds(&self, list: usize) -> Result<(usize, usize), IvfArtifactError> {
        if list >= self.list_count {
            return Err(invalid("list id is outside the directory"));
        }
        Ok((
            u64_to_usize(
                read_u64(self.input, self.directory_offset + list * 8)?,
                "list start",
            )?,
            u64_to_usize(
                read_u64(self.input, self.directory_offset + (list + 1) * 8)?,
                "list end",
            )?,
        ))
    }

    fn validate_contents(&self) -> Result<(), IvfArtifactError> {
        for list in 0..self.list_count {
            let mut centroid = self
                .centroid(list)
                .ok_or_else(|| invalid("centroid is missing"))?;
            if centroid.any(|value| !value.is_finite()) {
                return Err(invalid("centroid contains a non-finite value"));
            }
        }
        let mut prior = 0usize;
        for list in 0..self.list_count {
            let (start, end) = self.list_bounds(list)?;
            if start != prior || end < start || end > self.posting_count {
                return Err(invalid("posting directory is not monotonic"));
            }
            prior = end;
        }
        if prior != self.posting_count {
            return Err(invalid("posting directory does not end at posting count"));
        }
        let mut ids = BTreeSet::new();
        for list in 0..self.list_count {
            for offset in 0..self.list_len(list)? {
                let posting = self.posting(list, offset)?;
                if posting.point_id == 0 || !ids.insert(posting.point_id) {
                    return Err(invalid("posting point id is zero or duplicated"));
                }
                if posting.vector().any(|value| !value.is_finite()) {
                    return Err(invalid("posting contains a non-finite value"));
                }
                let record = self.posting_offset
                    + (self.list_bounds(list)?.0 + offset) * self.posting_stride;
                let value_end = record + 8 + self.dimensions * 4;
                if self.input[value_end..record + self.posting_stride]
                    .iter()
                    .any(|byte| *byte != 0)
                {
                    return Err(invalid("posting padding is non-zero"));
                }
            }
        }
        Ok(())
    }
}

/// Borrowed-decoding posting view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IvfPostingView<'a> {
    point_id: u64,
    vector_bytes: &'a [u8],
}

impl<'a> IvfPostingView<'a> {
    /// Returns the point identity.
    #[must_use]
    pub const fn point_id(self) -> u64 {
        self.point_id
    }
    /// Returns a no-allocation coordinate iterator.
    #[must_use]
    pub const fn vector(self) -> IvfFloatIter<'a> {
        IvfFloatIter {
            bytes: self.vector_bytes,
            cursor: 0,
        }
    }
}

/// Iterator decoding little-endian `f32` values from a borrowed section.
#[derive(Debug, Clone)]
pub struct IvfFloatIter<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl Iterator for IvfFloatIter<'_> {
    type Item = f32;
    fn next(&mut self) -> Option<Self::Item> {
        let bytes: [u8; 4] = self
            .bytes
            .get(self.cursor..self.cursor + 4)?
            .try_into()
            .ok()?;
        self.cursor += 4;
        Some(f32::from_le_bytes(bytes))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = (self.bytes.len() - self.cursor) / 4;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for IvfFloatIter<'_> {}

fn validate_vector(vector: &[f32]) -> Result<(), IvfArtifactError> {
    if vector.is_empty() {
        return Err(invalid("vector is empty"));
    }
    if vector.iter().any(|value| !value.is_finite()) {
        return Err(invalid("vector contains a non-finite value"));
    }
    if vector.len() > MAX_VECTOR_DIMENSIONS {
        return Err(invalid("vector dimensions exceed policy"));
    }
    Ok(())
}

fn checked_product(a: usize, b: usize, c: usize, label: &str) -> Result<usize, IvfArtifactError> {
    a.checked_mul(b)
        .and_then(|value| value.checked_mul(c))
        .ok_or_else(|| invalid(format!("{label} overflow")))
}

fn aligned(value: usize) -> Result<usize, IvfArtifactError> {
    value
        .checked_add(SECTION_ALIGNMENT - 1)
        .map(|value| value & !(SECTION_ALIGNMENT - 1))
        .ok_or_else(|| invalid("alignment overflow"))
}

fn metric_code(metric: DistanceMetric) -> u8 {
    match metric {
        DistanceMetric::L2 => 1,
        DistanceMetric::InnerProduct => 2,
        DistanceMetric::NegativeInnerProduct => 3,
        DistanceMetric::Cosine => 4,
        DistanceMetric::L1 => 5,
        DistanceMetric::Hamming => 6,
        DistanceMetric::Jaccard => 7,
    }
}

fn decode_metric(code: u8) -> Result<DistanceMetric, IvfArtifactError> {
    match code {
        1 => Ok(DistanceMetric::L2),
        2 => Ok(DistanceMetric::InnerProduct),
        3 => Ok(DistanceMetric::NegativeInnerProduct),
        4 => Ok(DistanceMetric::Cosine),
        5 => Ok(DistanceMetric::L1),
        6 => Ok(DistanceMetric::Hamming),
        7 => Ok(DistanceMetric::Jaccard),
        _ => Err(invalid("metric code is unknown")),
    }
}

fn put_u32(output: &mut [u8], offset: usize, value: usize) -> Result<(), IvfArtifactError> {
    let value = u32::try_from(value).map_err(|_| invalid("u32 field overflow"))?;
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn put_u64(output: &mut [u8], offset: usize, value: usize) -> Result<(), IvfArtifactError> {
    let value = u64::try_from(value).map_err(|_| invalid("u64 field overflow"))?;
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_u16(input: &[u8], offset: usize) -> Result<u16, IvfArtifactError> {
    Ok(u16::from_le_bytes(
        input
            .get(offset..offset + 2)
            .ok_or_else(|| invalid("u16 field is truncated"))?
            .try_into()
            .map_err(|_| invalid("u16 field is truncated"))?,
    ))
}
fn read_u32(input: &[u8], offset: usize) -> Result<u32, IvfArtifactError> {
    Ok(u32::from_le_bytes(
        input
            .get(offset..offset + 4)
            .ok_or_else(|| invalid("u32 field is truncated"))?
            .try_into()
            .map_err(|_| invalid("u32 field is truncated"))?,
    ))
}
fn read_u64(input: &[u8], offset: usize) -> Result<u64, IvfArtifactError> {
    Ok(u64::from_le_bytes(
        input
            .get(offset..offset + 8)
            .ok_or_else(|| invalid("u64 field is truncated"))?
            .try_into()
            .map_err(|_| invalid("u64 field is truncated"))?,
    ))
}
fn u64_to_usize(value: u64, label: &str) -> Result<usize, IvfArtifactError> {
    usize::try_from(value).map_err(|_| invalid(format!("{label} exceeds platform bounds")))
}

fn artifact_checksum(input: &[u8]) -> u64 {
    let mut checksum = checksum_bytes(FNV_OFFSET_BASIS, &input[..CHECKSUM_OFFSET]);
    checksum = checksum_bytes(checksum, &[0; 8]);
    checksum_bytes(checksum, &input[CHECKSUM_OFFSET + 8..])
}

fn verify_checksum(input: &[u8]) -> Result<(), IvfArtifactError> {
    let expected = read_u64(input, CHECKSUM_OFFSET)?;
    if artifact_checksum(input) == expected {
        Ok(())
    } else {
        Err(IvfArtifactError::ChecksumMismatch)
    }
}

fn invalid(reason: impl Into<String>) -> IvfArtifactError {
    IvfArtifactError::Invalid(reason.into())
}
