//! Safe owning mapping for immutable packed HNSW graph images.

#![allow(
    unsafe_code,
    reason = "the owner binds an erased borrowed view to its immutable mmap allocation"
)]

use std::path::Path;

use crate::{
    MappedSegment, PackedGraphImageError, PackedGraphImageView, SegmentFileError, SegmentKind,
    map_segment_file,
};

const IDENTITY_MAGIC: [u8; 8] = *b"PGCTXMAP";
const IDENTITY_VERSION: u32 = 1;
const IDENTITY_HEADER_LEN: usize = 56;

/// Physical PostgreSQL index generation bound to a mapped packed image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedGraphIdentity {
    /// Database containing the index.
    pub database_oid: u32,
    /// Stable logical index OID.
    pub index_oid: u32,
    /// Physical relfilenode, changed by REINDEX.
    pub rel_file_number: u32,
    /// Published HNSW directory epoch.
    pub directory_epoch: u64,
    /// Metapage LSN identifying the graph publication.
    pub meta_lsn: u64,
}

/// Failure to open an immutable mapped packed-graph generation.
#[derive(Debug)]
pub enum MappedPackedGraphError {
    /// The outer segment file failed validation or mapping.
    Segment(SegmentFileError),
    /// The segment kind is not an HNSW graph.
    WrongSegmentKind,
    /// The index-generation binding header is truncated or invalid.
    InvalidIdentity(&'static str),
    /// The file belongs to a different physical index generation.
    IdentityMismatch,
    /// The packed graph image failed its own structural validation.
    Graph(PackedGraphImageError),
}

impl core::fmt::Display for MappedPackedGraphError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Segment(error) => write!(formatter, "mapped segment validation failed: {error}"),
            Self::WrongSegmentKind => formatter.write_str("mapped segment is not an HNSW graph"),
            Self::InvalidIdentity(message) => {
                write!(formatter, "invalid mapped graph identity: {message}")
            }
            Self::IdentityMismatch => formatter
                .write_str("mapped graph identity does not match the live index generation"),
            Self::Graph(error) => write!(formatter, "packed graph validation failed: {error}"),
        }
    }
}

impl std::error::Error for MappedPackedGraphError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Segment(error) => Some(error),
            Self::Graph(error) => Some(error),
            Self::WrongSegmentKind | Self::InvalidIdentity(_) | Self::IdentityMismatch => None,
        }
    }
}

impl From<SegmentFileError> for MappedPackedGraphError {
    fn from(error: SegmentFileError) -> Self {
        Self::Segment(error)
    }
}

/// Immutable OS mapping coupled to its validated packed graph view.
///
/// The view's internal lifetime is erased only inside this owning type. The
/// mapping allocation is stable when this value moves, no mutable payload
/// access exists, and the view is exposed only through a borrow of `self`, so
/// safe callers cannot retain graph slices after the map is unmapped.
pub struct MappedPackedGraphImage {
    view: PackedGraphImageView<'static>,
    segment: MappedSegment,
}

impl MappedPackedGraphImage {
    /// Maps and validates a packed HNSW graph segment.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that no process modifies or truncates the
    /// generation file until this owner is dropped. Atomic replacement or
    /// unlinking of the pathname is allowed because it does not alter the
    /// already-open file description.
    ///
    /// # Errors
    ///
    /// Returns [`MappedPackedGraphError`] for filesystem errors, outer segment
    /// corruption, a wrong segment kind, or packed-image corruption.
    pub unsafe fn open(
        path: impl AsRef<Path>,
        expected: MappedGraphIdentity,
    ) -> Result<Self, MappedPackedGraphError> {
        // SAFETY: upheld by this constructor's caller for the full lifetime of
        // the returned owner.
        let segment = unsafe { map_segment_file(path)? };
        if segment.header().kind() != SegmentKind::HnswGraph {
            return Err(MappedPackedGraphError::WrongSegmentKind);
        }
        let payload = segment.payload();
        let (identity, packed) = decode_identity(payload)?;
        if identity != expected {
            return Err(MappedPackedGraphError::IdentityMismatch);
        }
        let view =
            PackedGraphImageView::attach(packed, true).map_err(MappedPackedGraphError::Graph)?;
        // SAFETY: `view` borrows the stable OS allocation owned by `segment`,
        // not the movable Rust field. Both are moved together into this value;
        // `view` is declared before `segment` so it is dropped first, and the
        // only accessor below ties every exposed borrow to `&self`.
        let view = unsafe {
            core::mem::transmute::<PackedGraphImageView<'_>, PackedGraphImageView<'static>>(view)
        };
        Ok(Self { view, segment })
    }

    /// Returns the validated packed graph view for the lifetime of this owner.
    #[must_use]
    pub const fn view(&self) -> &PackedGraphImageView<'_> {
        &self.view
    }

    /// Returns the mapped encoded byte length, including the segment header.
    #[must_use]
    pub const fn mapped_len(&self) -> usize {
        self.segment.mapped_len()
    }

    /// Returns the immutable generation file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.segment.path()
    }
}

/// Encodes a full-layer packed image with its physical index identity.
#[must_use]
pub fn encode_mapped_packed_graph(identity: MappedGraphIdentity, packed_image: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(IDENTITY_HEADER_LEN.saturating_add(packed_image.len()));
    output.extend_from_slice(&IDENTITY_MAGIC);
    output.extend_from_slice(&IDENTITY_VERSION.to_le_bytes());
    output.extend_from_slice(&identity.database_oid.to_le_bytes());
    output.extend_from_slice(&identity.index_oid.to_le_bytes());
    output.extend_from_slice(&identity.rel_file_number.to_le_bytes());
    output.extend_from_slice(&identity.directory_epoch.to_le_bytes());
    output.extend_from_slice(&identity.meta_lsn.to_le_bytes());
    output.extend_from_slice(
        &u64::try_from(packed_image.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    output.extend_from_slice(&0_u64.to_le_bytes());
    output.extend_from_slice(packed_image);
    output
}

fn decode_identity(payload: &[u8]) -> Result<(MappedGraphIdentity, &[u8]), MappedPackedGraphError> {
    if payload.len() < IDENTITY_HEADER_LEN {
        return Err(MappedPackedGraphError::InvalidIdentity("truncated header"));
    }
    if payload[0..8] != IDENTITY_MAGIC {
        return Err(MappedPackedGraphError::InvalidIdentity("bad magic"));
    }
    if read_u32(payload, 8) != IDENTITY_VERSION {
        return Err(MappedPackedGraphError::InvalidIdentity(
            "unsupported version",
        ));
    }
    if read_u64(payload, 48) != 0 {
        return Err(MappedPackedGraphError::InvalidIdentity(
            "non-zero reserved field",
        ));
    }
    let packed_len = usize::try_from(read_u64(payload, 40))
        .map_err(|_| MappedPackedGraphError::InvalidIdentity("packed length overflow"))?;
    let packed_end = IDENTITY_HEADER_LEN.checked_add(packed_len).ok_or(
        MappedPackedGraphError::InvalidIdentity("packed range overflow"),
    )?;
    if packed_end != payload.len() {
        return Err(MappedPackedGraphError::InvalidIdentity(
            "packed length mismatch",
        ));
    }
    Ok((
        MappedGraphIdentity {
            database_oid: read_u32(payload, 12),
            index_oid: read_u32(payload, 16),
            rel_file_number: read_u32(payload, 20),
            directory_epoch: read_u64(payload, 24),
            meta_lsn: read_u64(payload, 32),
        },
        &payload[IDENTITY_HEADER_LEN..packed_end],
    ))
}

fn read_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(input[offset..offset + 4].try_into().unwrap_or([0; 4]))
}

fn read_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(input[offset..offset + 8].try_into().unwrap_or([0; 8]))
}
