//! Owned read-only operating-system mappings for immutable segment files.

#![allow(
    unsafe_code,
    reason = "the mmap owner isolates the OS pointer lifetime and unmaps it exactly once"
)]

use std::{
    fs::File,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    ptr::NonNull,
    slice,
};

use crate::{
    MAX_SEGMENT_FILE_BYTES, SegmentFileError, SegmentHeader, io_error, validate_mmap_segment,
};

/// An immutable segment file mapped into this process.
///
/// The mapping owns the file descriptor for at least as long as the mapped
/// address and validates the full segment header, payload bounds, and checksum
/// before exposing any borrowed bytes.
#[derive(Debug)]
pub struct MappedSegment {
    _file: File,
    path: PathBuf,
    address: NonNull<u8>,
    length: usize,
    header: SegmentHeader,
}

impl MappedSegment {
    /// Returns the validated segment header.
    #[must_use]
    pub const fn header(&self) -> SegmentHeader {
        self.header
    }

    /// Returns the immutable payload directly from the operating-system map.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        let payload_length = usize::try_from(self.header.payload_len())
            .unwrap_or(self.length.saturating_sub(SegmentHeader::ENCODED_LEN));
        // SAFETY: construction mapped `length` readable bytes, validation
        // proved that the fixed header and declared payload are in bounds, and
        // `&self` keeps the mapping alive and immutable for the returned borrow.
        let bytes = unsafe { slice::from_raw_parts(self.address.as_ptr(), self.length) };
        &bytes[SegmentHeader::ENCODED_LEN..SegmentHeader::ENCODED_LEN + payload_length]
    }

    /// Returns the encoded mapped byte length, including the segment header.
    #[must_use]
    pub const fn mapped_len(&self) -> usize {
        self.length
    }

    /// Returns the mapped file path for diagnostics.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MappedSegment {
    fn drop(&mut self) {
        // SAFETY: `address` and `length` are the unchanged successful mmap
        // result owned exclusively by this value and Drop runs exactly once.
        let result = unsafe { libc::munmap(self.address.as_ptr().cast(), self.length) };
        debug_assert_eq!(result, 0, "validated segment munmap should succeed");
    }
}

/// Maps and validates an immutable segment file without copying its payload.
///
/// # Errors
///
/// Returns [`SegmentFileError`] when the file cannot be opened, is outside the
/// segment size policy, cannot be mapped, or fails format/checksum validation.
pub fn map_segment_file(path: impl AsRef<Path>) -> Result<MappedSegment, SegmentFileError> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|source| io_error("open", path, source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("metadata", path, source))?;
    let length = usize::try_from(metadata.len()).map_err(|_| SegmentFileError::FileTooLarge {
        path: path.to_path_buf(),
        length: metadata.len(),
        maximum: MAX_SEGMENT_FILE_BYTES,
    })?;
    if length > MAX_SEGMENT_FILE_BYTES {
        return Err(SegmentFileError::FileTooLarge {
            path: path.to_path_buf(),
            length: metadata.len(),
            maximum: MAX_SEGMENT_FILE_BYTES,
        });
    }
    if length == 0 {
        return Err(crate::SegmentError::TruncatedHeader {
            actual: 0,
            minimum: SegmentHeader::ENCODED_LEN,
        }
        .into());
    }

    // SAFETY: the file is open, `length` is its nonzero validated size, the
    // mapping is read-only/private, and the returned pointer is checked before
    // being stored in the sole mapping owner.
    let raw_address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };
    if raw_address == libc::MAP_FAILED {
        return Err(io_error("mmap", path, std::io::Error::last_os_error()));
    }
    let Some(address) = NonNull::new(raw_address.cast::<u8>()) else {
        // SAFETY: a null address is unusual but may still name the successful
        // mapping returned above; release it before rejecting the unusable
        // pointer representation.
        unsafe { libc::munmap(raw_address, length) };
        return Err(io_error(
            "mmap",
            path,
            std::io::Error::other("mmap returned a null address"),
        ));
    };
    // SAFETY: mmap returned a non-null readable range of exactly `length`
    // bytes, borrowed only until either validation fails or ownership moves
    // into `MappedSegment`.
    let bytes = unsafe { slice::from_raw_parts(address.as_ptr(), length) };
    let view = match validate_mmap_segment(bytes) {
        Ok(view) => view,
        Err(error) => {
            // SAFETY: validation failure still leaves the successful mapping
            // owned here; it must be unmapped before returning the error.
            unsafe { libc::munmap(address.as_ptr().cast(), length) };
            return Err(error.into());
        }
    };

    Ok(MappedSegment {
        _file: file,
        path: path.to_path_buf(),
        address,
        length,
        header: view.header(),
    })
}
