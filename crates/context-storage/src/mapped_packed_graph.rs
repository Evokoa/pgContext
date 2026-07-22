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

/// Failure to open an immutable mapped packed-graph generation.
#[derive(Debug)]
pub enum MappedPackedGraphError {
    /// The outer segment file failed validation or mapping.
    Segment(SegmentFileError),
    /// The segment kind is not an HNSW graph.
    WrongSegmentKind,
    /// The packed graph image failed its own structural validation.
    Graph(PackedGraphImageError),
}

impl core::fmt::Display for MappedPackedGraphError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Segment(error) => write!(formatter, "mapped segment validation failed: {error}"),
            Self::WrongSegmentKind => formatter.write_str("mapped segment is not an HNSW graph"),
            Self::Graph(error) => write!(formatter, "packed graph validation failed: {error}"),
        }
    }
}

impl std::error::Error for MappedPackedGraphError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Segment(error) => Some(error),
            Self::Graph(error) => Some(error),
            Self::WrongSegmentKind => None,
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
    /// # Errors
    ///
    /// Returns [`MappedPackedGraphError`] for filesystem errors, outer segment
    /// corruption, a wrong segment kind, or packed-image corruption.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MappedPackedGraphError> {
        let segment = map_segment_file(path)?;
        if segment.header().kind() != SegmentKind::HnswGraph {
            return Err(MappedPackedGraphError::WrongSegmentKind);
        }
        let view = PackedGraphImageView::attach(segment.payload(), true)
            .map_err(MappedPackedGraphError::Graph)?;
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
