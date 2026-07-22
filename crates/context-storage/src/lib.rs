//! Rebuildable artifact storage for pgContext.
//!
//! Segment files and memory-mapped readers are cache material, not primary
//! user data. The crate may opt into tightly reviewed unsafe code when mmap
//! validation is implemented.
//!
//! # Segment Format Version 1
//!
//! All integer fields are little-endian. The fixed header is 40 bytes:
//!
//! | Offset | Size | Field |
//! |---:|---:|---|
//! | 0 | 8 | magic bytes `PGCTXSEG` |
//! | 8 | 4 | segment format version, currently `1` |
//! | 12 | 4 | endian marker `0x01020304` |
//! | 16 | 4 | segment kind |
//! | 20 | 4 | reserved, currently zero |
//! | 24 | 8 | payload byte length |
//! | 32 | 8 | checksum |
//!
//! The checksum is a deterministic FNV-1a 64-bit checksum over the header with
//! the checksum field set to zero followed by the payload bytes. The checksum
//! is an artifact-corruption guard, not a cryptographic authenticator.

use core::fmt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process,
    sync::atomic::{AtomicU64, Ordering},
};

mod delta_segment;
mod hnsw_graph_payload;
mod mmap_file;
mod packed_graph_image;

pub use delta_segment::{
    DELTA_MAX_DIMENSIONS, DELTA_PAGE_HEADER_BYTES, DeltaRecord, DeltaRecordKind, DeltaSegmentError,
    decode_delta_page, decode_delta_record, encode_delta_page, encode_delta_record,
};
pub use hnsw_graph_payload::{
    CURRENT_HNSW_GRAPH_PAYLOAD_VERSION, HnswGraphArtifactRecord, HnswGraphPayload,
    HnswGraphPayloadError, HnswGraphQuantization, HnswGraphQuantizationCodebook,
    MIN_READABLE_HNSW_GRAPH_PAYLOAD_VERSION, PreparedQuantizedQuery, QuantizedHnswGraphNodeView,
    QuantizedHnswGraphView, QuantizedNeighborIter, decode_hnsw_graph_payload,
    decode_hnsw_graph_payload_versioned, encode_hnsw_graph_payload, encode_hnsw_graph_payload_v2,
};
pub use mmap_file::{MappedSegment, map_segment_file};
pub use packed_graph_image::{
    AlignedImageBuf, CURRENT_PACKED_GRAPH_IMAGE_VERSION, MIN_READABLE_PACKED_GRAPH_IMAGE_VERSION,
    PackedGraphImageError, PackedGraphImageLayer, PackedGraphImageNode, PackedGraphImageView,
    encode_packed_graph_image, encode_packed_graph_image_v2, packed_graph_image_len,
};

/// Maximum segment payload accepted by this loader.
pub const MAX_SEGMENT_PAYLOAD_BYTES: usize = 1 << 30;

/// Required alignment for segment sections addressed inside mmap-backed files.
pub const SEGMENT_SECTION_ALIGNMENT_BYTES: usize = 8;

/// Maximum encoded segment file size accepted by file loaders.
pub const MAX_SEGMENT_FILE_BYTES: usize = SegmentHeader::ENCODED_LEN + MAX_SEGMENT_PAYLOAD_BYTES;

/// Current production segment format version written by pgContext.
pub const CURRENT_SEGMENT_FORMAT_VERSION: u32 = 1;

/// Oldest segment format version this loader can read.
pub const MIN_READABLE_SEGMENT_FORMAT_VERSION: u32 = 1;

/// Newest segment format version this loader can read.
pub const MAX_READABLE_SEGMENT_FORMAT_VERSION: u32 = CURRENT_SEGMENT_FORMAT_VERSION;

const MAGIC: [u8; 8] = *b"PGCTXSEG";
const ENDIAN_MARKER: u32 = 0x0102_0304;
const RESERVED: u32 = 0;
const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const PAYLOAD_SECTION: &str = "payload";
const TEMP_WRITE_ATTEMPTS: u32 = 64;
static TEMP_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Segment format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentVersion {
    /// Initial rebuildable segment format.
    V1,
}

impl SegmentVersion {
    /// Returns the on-disk integer representation.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::V1 => CURRENT_SEGMENT_FORMAT_VERSION,
        }
    }
}

/// Returns whether this build can read the given segment format version.
#[must_use]
pub const fn is_supported_segment_format_version(version: u32) -> bool {
    version >= MIN_READABLE_SEGMENT_FORMAT_VERSION && version <= MAX_READABLE_SEGMENT_FORMAT_VERSION
}

/// Rebuildable segment payload kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    /// HNSW graph segment payload.
    HnswGraph,
}

impl SegmentKind {
    fn as_u32(self) -> u32 {
        match self {
            Self::HnswGraph => 1,
        }
    }

    fn from_u32(kind: u32) -> Result<Self, SegmentError> {
        match kind {
            1 => Ok(Self::HnswGraph),
            _ => Err(SegmentError::UnknownKind { kind }),
        }
    }
}

/// Fixed segment header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentHeader {
    version: SegmentVersion,
    kind: SegmentKind,
    payload_len: u64,
    checksum: u64,
}

impl SegmentHeader {
    /// Encoded header length in bytes.
    pub const ENCODED_LEN: usize = 40;

    /// Returns the segment format version.
    #[must_use]
    pub const fn version(self) -> SegmentVersion {
        self.version
    }

    /// Returns the segment payload kind.
    #[must_use]
    pub const fn kind(self) -> SegmentKind {
        self.kind
    }

    /// Returns the payload byte length.
    #[must_use]
    pub const fn payload_len(self) -> u64 {
        self.payload_len
    }

    /// Returns the stored checksum.
    #[must_use]
    pub const fn checksum(self) -> u64 {
        self.checksum
    }

    fn new(kind: SegmentKind, payload_len: usize, checksum: u64) -> Result<Self, SegmentError> {
        let payload_len =
            u64::try_from(payload_len).map_err(|_| SegmentError::PayloadTooLarge {
                length: payload_len,
                maximum: MAX_SEGMENT_PAYLOAD_BYTES,
            })?;
        Ok(Self {
            version: SegmentVersion::V1,
            kind,
            payload_len,
            checksum,
        })
    }

    fn encode_with_checksum(self, checksum: u64) -> [u8; Self::ENCODED_LEN] {
        let mut output = [0; Self::ENCODED_LEN];
        output[0..8].copy_from_slice(&MAGIC);
        output[8..12].copy_from_slice(&self.version.as_u32().to_le_bytes());
        output[12..16].copy_from_slice(&ENDIAN_MARKER.to_le_bytes());
        output[16..20].copy_from_slice(&self.kind.as_u32().to_le_bytes());
        output[20..24].copy_from_slice(&RESERVED.to_le_bytes());
        output[24..32].copy_from_slice(&self.payload_len.to_le_bytes());
        output[32..40].copy_from_slice(&checksum.to_le_bytes());
        output
    }

    fn checksum_header(self) -> [u8; Self::ENCODED_LEN] {
        self.encode_with_checksum(0)
    }

    fn encode(self) -> [u8; Self::ENCODED_LEN] {
        self.encode_with_checksum(self.checksum)
    }
}

/// Owned encoded segment bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentBytes {
    header: SegmentHeader,
    payload: Vec<u8>,
}

impl SegmentBytes {
    /// Creates a segment payload after validating its length.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentError::PayloadTooLarge`] when `payload` exceeds
    /// [`MAX_SEGMENT_PAYLOAD_BYTES`].
    pub fn new(kind: SegmentKind, payload: Vec<u8>) -> Result<Self, SegmentError> {
        validate_payload_len(payload.len())?;
        let header = SegmentHeader::new(kind, payload.len(), 0)?;
        let checksum = checksum_segment(header, &payload);
        let header = SegmentHeader::new(kind, payload.len(), checksum)?;
        Ok(Self { header, payload })
    }

    /// Returns the decoded segment header.
    #[must_use]
    pub const fn header(&self) -> SegmentHeader {
        self.header
    }

    /// Returns the segment payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Encodes this segment as header bytes followed by payload bytes.
    #[must_use]
    pub fn into_encoded(self) -> Vec<u8> {
        let mut output = Vec::with_capacity(SegmentHeader::ENCODED_LEN + self.payload.len());
        output.extend_from_slice(&self.header.encode());
        output.extend_from_slice(&self.payload);
        output
    }
}

/// Borrowed validated segment view for mmap-backed bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentView<'a> {
    header: SegmentHeader,
    payload: &'a [u8],
}

impl<'a> SegmentView<'a> {
    /// Returns the decoded segment header.
    #[must_use]
    pub const fn header(self) -> SegmentHeader {
        self.header
    }

    /// Returns the borrowed segment payload.
    #[must_use]
    pub const fn payload(self) -> &'a [u8] {
        self.payload
    }
}

/// Segment format load or encode error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentError {
    /// Input is shorter than the fixed header.
    TruncatedHeader {
        /// Actual input byte length.
        actual: usize,
        /// Minimum required header length.
        minimum: usize,
    },
    /// Header magic bytes do not match pgContext segment files.
    BadMagic,
    /// Segment version is not supported by this loader.
    UnknownVersion {
        /// Raw version value from the header.
        version: u32,
    },
    /// Endian marker does not match the expected little-endian marker.
    WrongEndianMarker {
        /// Raw marker value from the header.
        marker: u32,
    },
    /// Segment kind is not supported by this loader.
    UnknownKind {
        /// Raw kind value from the header.
        kind: u32,
    },
    /// Reserved header bytes are non-zero.
    NonZeroReserved {
        /// Raw reserved value from the header.
        value: u32,
    },
    /// Payload length cannot fit in this process.
    PayloadLengthOverflow {
        /// Raw payload length from the header.
        length: u64,
    },
    /// Payload length exceeds the loader policy.
    PayloadTooLarge {
        /// Requested payload length.
        length: usize,
        /// Maximum accepted payload length.
        maximum: usize,
    },
    /// Input ended before the declared payload length.
    TruncatedPayload {
        /// Expected payload length.
        expected: usize,
        /// Actual available payload length.
        actual: usize,
    },
    /// A section offset is not aligned for mmap-backed access.
    MisalignedSection {
        /// Section name.
        section: &'static str,
        /// Section byte offset from the start of the file.
        offset: usize,
        /// Required byte alignment.
        alignment: usize,
    },
    /// A section range overflows addressable memory.
    SectionOffsetOverflow {
        /// Section name.
        section: &'static str,
        /// Section byte offset from the start of the file.
        offset: usize,
        /// Section byte length.
        length: usize,
    },
    /// Payload checksum does not match the header.
    ChecksumMismatch,
}

impl fmt::Display for SegmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader { actual, minimum } => {
                write!(formatter, "truncated segment header: {actual} < {minimum}")
            }
            Self::BadMagic => formatter.write_str("invalid segment magic"),
            Self::UnknownVersion { version } => {
                write!(formatter, "unknown segment version: {version}")
            }
            Self::WrongEndianMarker { marker } => {
                write!(formatter, "wrong segment endian marker: {marker:#010x}")
            }
            Self::UnknownKind { kind } => write!(formatter, "unknown segment kind: {kind}"),
            Self::NonZeroReserved { value } => {
                write!(formatter, "segment reserved field is non-zero: {value}")
            }
            Self::PayloadLengthOverflow { length } => {
                write!(
                    formatter,
                    "segment payload length overflows usize: {length}"
                )
            }
            Self::PayloadTooLarge { length, maximum } => {
                write!(formatter, "segment payload too large: {length} > {maximum}")
            }
            Self::TruncatedPayload { expected, actual } => {
                write!(
                    formatter,
                    "truncated segment payload: {actual} < {expected}"
                )
            }
            Self::MisalignedSection {
                section,
                offset,
                alignment,
            } => write!(
                formatter,
                "segment section {section} has misaligned offset {offset}; expected {alignment}-byte alignment"
            ),
            Self::SectionOffsetOverflow {
                section,
                offset,
                length,
            } => write!(
                formatter,
                "segment section {section} range overflows: offset {offset}, length {length}"
            ),
            Self::ChecksumMismatch => formatter.write_str("segment checksum mismatch"),
        }
    }
}

impl std::error::Error for SegmentError {}

/// Segment file load or atomic write error.
#[derive(Debug)]
pub enum SegmentFileError {
    /// Segment bytes are malformed.
    Format(SegmentError),
    /// Path cannot be used as a segment file target.
    InvalidPath {
        /// Segment path.
        path: PathBuf,
    },
    /// Could not allocate a unique temporary file name.
    TempNameExhausted {
        /// Segment path.
        path: PathBuf,
    },
    /// Segment file exceeds the loader's maximum encoded size.
    FileTooLarge {
        /// Segment path.
        path: PathBuf,
        /// Actual file length in bytes.
        length: u64,
        /// Maximum accepted encoded segment length.
        maximum: usize,
    },
    /// File system operation failed.
    Io {
        /// Operation being performed.
        operation: &'static str,
        /// Path involved in the operation.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
}

/// Atomic segment-write operation boundary used by fault-injection callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentWriteStage {
    /// Immediately before writing the temporary file bytes.
    BeforeWrite,
    /// Immediately before synchronizing the temporary file.
    BeforeFileSync,
    /// Immediately before atomically renaming the temporary file.
    BeforeRename,
    /// Immediately before synchronizing the containing directory.
    BeforeDirectorySync,
}

impl fmt::Display for SegmentFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(error) => write!(formatter, "invalid segment file: {error}"),
            Self::InvalidPath { path } => {
                write!(formatter, "invalid segment path: {}", path.display())
            }
            Self::TempNameExhausted { path } => write!(
                formatter,
                "could not allocate a temporary segment path for {}",
                path.display()
            ),
            Self::FileTooLarge {
                path,
                length,
                maximum,
            } => write!(
                formatter,
                "segment file {} is too large: {length} > {maximum}",
                path.display()
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "segment file {operation} failed for {}: {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SegmentFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Format(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::InvalidPath { .. }
            | Self::TempNameExhausted { .. }
            | Self::FileTooLarge { .. } => None,
        }
    }
}

impl From<SegmentError> for SegmentFileError {
    fn from(error: SegmentError) -> Self {
        Self::Format(error)
    }
}

/// Encodes a rebuildable segment with a versioned header and checksum.
///
/// # Errors
///
/// Returns [`SegmentError::PayloadTooLarge`] when `payload` exceeds
/// [`MAX_SEGMENT_PAYLOAD_BYTES`].
pub fn encode_segment(kind: SegmentKind, payload: &[u8]) -> Result<Vec<u8>, SegmentError> {
    SegmentBytes::new(kind, payload.to_vec()).map(SegmentBytes::into_encoded)
}

/// Decodes and validates a rebuildable segment.
///
/// # Errors
///
/// Returns [`SegmentError`] when the header is malformed, the payload length is
/// invalid, the payload is truncated, or the checksum does not match.
pub fn decode_segment(input: &[u8]) -> Result<SegmentBytes, SegmentError> {
    let view = validate_mmap_segment(input)?;
    Ok(SegmentBytes {
        header: view.header,
        payload: view.payload.to_vec(),
    })
}

/// Validates mmap-backed segment bytes and returns a borrowed segment view.
///
/// This function does not create an operating-system memory map. It validates
/// bytes supplied by a mmap owner without copying payload data, including
/// section bounds, section alignment, header fields, and checksum.
///
/// # Errors
///
/// Returns [`SegmentError`] when the mmap bytes do not contain a valid segment.
pub fn validate_mmap_segment(input: &[u8]) -> Result<SegmentView<'_>, SegmentError> {
    let (header, payload) = decode_header_and_payload(input)?;
    let computed = checksum_segment(header, payload);
    if computed != header.checksum {
        return Err(SegmentError::ChecksumMismatch);
    }
    Ok(SegmentView { header, payload })
}

/// Atomically writes a segment file and reloads it through the validator.
///
/// The write happens in a same-directory temporary file, followed by file
/// synchronization, atomic rename, parent-directory synchronization, and a
/// reload through [`load_segment_file`].
///
/// # Errors
///
/// Returns [`SegmentFileError`] when encoding, writing, renaming, syncing, or
/// reloading fails.
pub fn write_segment_atomic(
    path: impl AsRef<Path>,
    kind: SegmentKind,
    payload: &[u8],
) -> Result<SegmentBytes, SegmentFileError> {
    write_segment_atomic_with_hook(path, kind, payload, |_| Ok(()))
}

/// Atomically writes a segment while reporting each durable filesystem boundary.
///
/// # Errors
///
/// Returns an error from `hook`, encoding, an individual filesystem operation,
/// or the final validation reload. A hook error before rename removes its
/// temporary file; after rename, the new file remains fully encoded and valid.
pub fn write_segment_atomic_with_hook(
    path: impl AsRef<Path>,
    kind: SegmentKind,
    payload: &[u8],
    mut hook: impl FnMut(SegmentWriteStage) -> Result<(), SegmentFileError>,
) -> Result<SegmentBytes, SegmentFileError> {
    let path = path.as_ref();
    let encoded = encode_segment(kind, payload)?;
    let temp_path = write_temp_segment(path, &encoded, &mut hook)?;
    rename_temp_segment(&temp_path, path, &mut hook)?;
    sync_parent_directory(path, &mut hook)?;
    load_segment_file(path)
}

/// Loads a segment file through the same validator used for mmap bytes.
///
/// # Errors
///
/// Returns [`SegmentFileError`] when reading fails or the encoded segment is
/// malformed.
pub fn load_segment_file(path: impl AsRef<Path>) -> Result<SegmentBytes, SegmentFileError> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|source| io_error("open", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("metadata", path, source))?;
    ensure_file_size(path, metadata.len())?;

    let mut bytes = Vec::with_capacity(file_capacity(path, metadata.len())?);
    let read_limit =
        u64::try_from(MAX_SEGMENT_FILE_BYTES).map_err(|_| SegmentFileError::FileTooLarge {
            path: path.to_path_buf(),
            length: metadata.len(),
            maximum: MAX_SEGMENT_FILE_BYTES,
        })? + 1;
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read", path, source))?;
    if bytes.len() > MAX_SEGMENT_FILE_BYTES {
        return Err(SegmentFileError::FileTooLarge {
            path: path.to_path_buf(),
            length: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            maximum: MAX_SEGMENT_FILE_BYTES,
        });
    }
    decode_segment(&bytes).map_err(SegmentFileError::from)
}

/// Reloads a segment file through the validator.
///
/// # Errors
///
/// Returns [`SegmentFileError`] when reading fails or the encoded segment is
/// malformed.
pub fn reload_segment_file(path: impl AsRef<Path>) -> Result<SegmentBytes, SegmentFileError> {
    load_segment_file(path)
}

/// Exports a validated rebuildable segment artifact to `destination`.
///
/// The source is loaded through the validator before the destination is written
/// atomically. The returned segment is the reloaded destination artifact.
///
/// # Errors
///
/// Returns [`SegmentFileError`] when source validation, destination writing, or
/// destination reloading fails.
pub fn export_segment_file(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<SegmentBytes, SegmentFileError> {
    copy_validated_segment(source.as_ref(), destination.as_ref())
}

/// Imports a rebuildable segment artifact into `destination`.
///
/// The imported artifact is validated before it replaces the destination, and
/// the returned segment is reloaded from the installed destination path.
///
/// # Errors
///
/// Returns [`SegmentFileError`] when source validation, destination writing, or
/// destination reloading fails.
pub fn import_segment_file(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<SegmentBytes, SegmentFileError> {
    copy_validated_segment(source.as_ref(), destination.as_ref())
}

fn decode_header_and_payload(input: &[u8]) -> Result<(SegmentHeader, &[u8]), SegmentError> {
    if input.len() < SegmentHeader::ENCODED_LEN {
        return Err(SegmentError::TruncatedHeader {
            actual: input.len(),
            minimum: SegmentHeader::ENCODED_LEN,
        });
    }

    if input[0..8] != MAGIC {
        return Err(SegmentError::BadMagic);
    }

    let version = read_u32(input, 8);
    if !is_supported_segment_format_version(version) {
        return Err(SegmentError::UnknownVersion { version });
    }

    let marker = read_u32(input, 12);
    if marker != ENDIAN_MARKER {
        return Err(SegmentError::WrongEndianMarker { marker });
    }

    let kind = SegmentKind::from_u32(read_u32(input, 16))?;
    let reserved = read_u32(input, 20);
    if reserved != RESERVED {
        return Err(SegmentError::NonZeroReserved { value: reserved });
    }

    let payload_len = read_u64(input, 24);
    let payload_len_usize =
        usize::try_from(payload_len).map_err(|_| SegmentError::PayloadLengthOverflow {
            length: payload_len,
        })?;
    validate_payload_len(payload_len_usize)?;
    let payload_start = SegmentHeader::ENCODED_LEN;
    let payload_end = validate_section_bounds_and_alignment(
        PAYLOAD_SECTION,
        payload_start,
        payload_len_usize,
        input.len(),
        SEGMENT_SECTION_ALIGNMENT_BYTES,
    )?;

    let header = SegmentHeader {
        version: SegmentVersion::V1,
        kind,
        payload_len,
        checksum: read_u64(input, 32),
    };
    Ok((header, &input[payload_start..payload_end]))
}

fn copy_validated_segment(
    source: &Path,
    destination: &Path,
) -> Result<SegmentBytes, SegmentFileError> {
    let segment = load_segment_file(source)?;
    write_segment_atomic(destination, segment.header.kind(), segment.payload())
}

fn write_temp_segment(
    path: &Path,
    encoded: &[u8],
    hook: &mut impl FnMut(SegmentWriteStage) -> Result<(), SegmentFileError>,
) -> Result<PathBuf, SegmentFileError> {
    for attempt in 0..TEMP_WRITE_ATTEMPTS {
        let temp_path = temp_segment_path(path, attempt)?;
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(mut file) => {
                if let Err(error) = hook(SegmentWriteStage::BeforeWrite) {
                    cleanup_temp_segment(&temp_path);
                    return Err(error);
                }
                if let Err(source) = file.write_all(encoded) {
                    cleanup_temp_segment(&temp_path);
                    return Err(io_error("write temporary file", &temp_path, source));
                }
                if let Err(error) = hook(SegmentWriteStage::BeforeFileSync) {
                    cleanup_temp_segment(&temp_path);
                    return Err(error);
                }
                if let Err(source) = file.sync_all() {
                    cleanup_temp_segment(&temp_path);
                    return Err(io_error("sync temporary file", &temp_path, source));
                }
                return Ok(temp_path);
            }
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                continue;
            }
            Err(source) => return Err(io_error("create temporary file", &temp_path, source)),
        }
    }
    Err(SegmentFileError::TempNameExhausted {
        path: path.to_path_buf(),
    })
}

fn rename_temp_segment(
    temp_path: &Path,
    path: &Path,
    hook: &mut impl FnMut(SegmentWriteStage) -> Result<(), SegmentFileError>,
) -> Result<(), SegmentFileError> {
    if let Err(error) = hook(SegmentWriteStage::BeforeRename) {
        cleanup_temp_segment(temp_path);
        return Err(error);
    }
    if let Err(source) = fs::rename(temp_path, path) {
        cleanup_temp_segment(temp_path);
        return Err(io_error("rename", path, source));
    }
    Ok(())
}

fn sync_parent_directory(
    path: &Path,
    hook: &mut impl FnMut(SegmentWriteStage) -> Result<(), SegmentFileError>,
) -> Result<(), SegmentFileError> {
    let parent = segment_parent(path);
    let directory =
        File::open(parent).map_err(|source| io_error("open parent directory", parent, source))?;
    hook(SegmentWriteStage::BeforeDirectorySync)?;
    directory
        .sync_all()
        .map_err(|source| io_error("sync parent directory", parent, source))
}

fn temp_segment_path(path: &Path, attempt: u32) -> Result<PathBuf, SegmentFileError> {
    let parent = segment_parent(path);
    let file_name = path
        .file_name()
        .ok_or_else(|| SegmentFileError::InvalidPath {
            path: path.to_path_buf(),
        })?;
    let sequence = TEMP_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp_file_name = file_name.to_os_string();
    temp_file_name.push(format!(".tmp.{}.{}.{}", process::id(), sequence, attempt));
    Ok(parent.join(temp_file_name))
}

fn segment_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn cleanup_temp_segment(temp_path: &Path) {
    if fs::remove_file(temp_path).is_err() {}
}

fn ensure_file_size(path: &Path, length: u64) -> Result<(), SegmentFileError> {
    let length_usize = usize::try_from(length).map_err(|_| SegmentFileError::FileTooLarge {
        path: path.to_path_buf(),
        length,
        maximum: MAX_SEGMENT_FILE_BYTES,
    })?;
    if length_usize > MAX_SEGMENT_FILE_BYTES {
        return Err(SegmentFileError::FileTooLarge {
            path: path.to_path_buf(),
            length,
            maximum: MAX_SEGMENT_FILE_BYTES,
        });
    }
    Ok(())
}

fn file_capacity(path: &Path, length: u64) -> Result<usize, SegmentFileError> {
    usize::try_from(length).map_err(|_| SegmentFileError::FileTooLarge {
        path: path.to_path_buf(),
        length,
        maximum: MAX_SEGMENT_FILE_BYTES,
    })
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> SegmentFileError {
    SegmentFileError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

fn validate_section_bounds_and_alignment(
    section: &'static str,
    offset: usize,
    length: usize,
    input_len: usize,
    alignment: usize,
) -> Result<usize, SegmentError> {
    debug_assert!(alignment.is_power_of_two());
    if !offset.is_multiple_of(alignment) {
        return Err(SegmentError::MisalignedSection {
            section,
            offset,
            alignment,
        });
    }
    let end = offset
        .checked_add(length)
        .ok_or(SegmentError::SectionOffsetOverflow {
            section,
            offset,
            length,
        })?;
    if input_len < end {
        return Err(SegmentError::TruncatedPayload {
            expected: length,
            actual: input_len.saturating_sub(offset),
        });
    }
    Ok(end)
}

fn validate_payload_len(payload_len: usize) -> Result<(), SegmentError> {
    if payload_len > MAX_SEGMENT_PAYLOAD_BYTES {
        return Err(SegmentError::PayloadTooLarge {
            length: payload_len,
            maximum: MAX_SEGMENT_PAYLOAD_BYTES,
        });
    }
    Ok(())
}

fn checksum_segment(header: SegmentHeader, payload: &[u8]) -> u64 {
    let checksum = checksum_bytes(FNV_OFFSET_BASIS, &header.checksum_header());
    checksum_bytes(checksum, payload)
}

fn checksum_bytes(mut checksum: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        checksum ^= u64::from(*byte);
        checksum = checksum.wrapping_mul(FNV_PRIME);
    }
    checksum
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    let mut bytes = [0; 4];
    bytes.copy_from_slice(&input[offset..offset + 4]);
    u32::from_le_bytes(bytes)
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&input[offset..offset + 8]);
    u64::from_le_bytes(bytes)
}

/// Returns the package version compiled into this crate.
#[must_use]
pub const fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::{
        SEGMENT_SECTION_ALIGNMENT_BYTES, SegmentError, validate_section_bounds_and_alignment,
    };

    #[test]
    fn mmap_section_validation_rejects_misaligned_offsets() {
        assert_eq!(
            validate_section_bounds_and_alignment(
                "test",
                SEGMENT_SECTION_ALIGNMENT_BYTES - 1,
                8,
                64,
                SEGMENT_SECTION_ALIGNMENT_BYTES,
            ),
            Err(SegmentError::MisalignedSection {
                section: "test",
                offset: SEGMENT_SECTION_ALIGNMENT_BYTES - 1,
                alignment: SEGMENT_SECTION_ALIGNMENT_BYTES,
            })
        );
    }

    #[test]
    fn mmap_section_validation_rejects_overflowing_ranges() {
        assert_eq!(
            validate_section_bounds_and_alignment(
                "test",
                usize::MAX - (SEGMENT_SECTION_ALIGNMENT_BYTES - 1),
                8,
                usize::MAX,
                SEGMENT_SECTION_ALIGNMENT_BYTES,
            ),
            Err(SegmentError::SectionOffsetOverflow {
                section: "test",
                offset: usize::MAX - (SEGMENT_SECTION_ALIGNMENT_BYTES - 1),
                length: 8,
            })
        );
    }
}
